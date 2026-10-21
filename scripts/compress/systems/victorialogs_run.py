"""VictoriaLogs comparator (one throwaway single-node container + fresh data dir per corpus).

Ingest: POST /insert/jsonline?_msg_field=text&_time_field=ts&_stream_fields=session
  (STREAM=none env: no _stream_fields, i.e. a single stream -- contrast variant)
  each record keeps seq, session, turn, kind, valid_at, text and gets
  ts = 2026-01-01T00:00:00Z + seq milliseconds (valid_at is synthetic -- up to 5.8e9 -- and
  would fall outside the ingest window if used as seconds; all records land in one day partition).
Then /internal/force_flush, /internal/force_merge?partition_prefix=20260101, wait for merges to
reach 0 and du to settle; size = du -sb of -storageDataPath.
Verify: LogsQL `* | fields seq, _msg` over the whole time range, every record, sha256 of _msg vs text.
No point-read latency (no primary-key get in VictoriaLogs; not natural).
Run: python3 victorialogs_run.py [corpus ...]
"""
import datetime, http.client, json, os, shutil, subprocess, sys, time, urllib.parse
sys.path.insert(0, os.path.dirname(__file__))
import common as C

IMAGE = "victoriametrics/victoria-logs:v1.53.0"
NAME = "oojuro-compress-victorialogs"
PORT = 19428
DIR = os.path.join(C.SYS, "victorialogs")
STREAM = "" if os.environ.get("STREAM") == "none" else "session"
BASE = datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc)


def conn():
    return http.client.HTTPConnection("127.0.0.1", PORT, timeout=3600)


def req(method, path, body=None):
    h = conn()
    h.request(method, path, body=body)
    r = h.getresponse()
    out = r.read()
    if r.status not in (200, 202, 204):
        raise RuntimeError(f"{path}: {r.status} {out[:300]!r}")
    return out


def metric(name):
    tot = 0.0
    for line in req("GET", "/metrics").decode().splitlines():
        if line.startswith(name + "{") or line.startswith(name + " "):
            tot += float(line.rsplit(" ", 1)[1])
    return tot


HELPER = "ghcr.io/y-scope/clp/clp-core-x86-ubuntu-jammy:main"  # has a shell; VictoriaLogs image has none


def du_root(path):
    # data files are root-owned (container runs as root); measure through a helper container
    out = subprocess.check_output(["docker", "run", "--rm", "--name", NAME + "-du", "-v", f"{path}:/d:ro",
                                   "--entrypoint", "du", HELPER, "-sb", "/d/data"]).split()[0]
    return int(out)


def wipe():
    subprocess.run(["docker", "run", "--rm", "--name", NAME + "-rm", "-v", f"{C.SYS}:/s", "--entrypoint", "sh", HELPER,
                    "-c", "rm -rf /s/victorialogs"], capture_output=True)


def run(c):
    C.rm_container(NAME)
    wipe()
    os.makedirs(os.path.join(DIR, "data"))
    C.docker("run", "-d", "--name", NAME, "--memory", "2g", "--cpus", "2", "-p", f"127.0.0.1:{PORT}:9428",
             "-v", f"{os.path.join(DIR, 'data')}:/data", IMAGE, "-storageDataPath=/data", "-retentionPeriod=100y",
             "-insert.maxLineSizeBytes=4MiB", "-search.maxQueryDuration=10m", "-httpListenAddr=:9428")
    C.wait_http(f"http://127.0.0.1:{PORT}/health")
    ver = req("GET", "/metrics").decode()
    ver = next((l for l in ver.splitlines() if l.startswith("vm_app_version")), "?")
    recs = C.load(c)
    n, raw = len(recs), C.raw_bytes(c)
    exp = C.digests(recs)
    qp = {"_msg_field": "text", "_time_field": "ts"}
    if STREAM:
        qp["_stream_fields"] = STREAM
    q = urllib.parse.urlencode(qp)
    buf, size = [], 0
    for r in recs:
        o = dict(r)
        o["ts"] = (BASE + datetime.timedelta(milliseconds=r["seq"])).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
        line = json.dumps(o, ensure_ascii=False).encode("utf-8") + b"\n"
        buf.append(line); size += len(line)
        if size > 8 << 20:
            req("POST", "/insert/jsonline?" + q, b"".join(buf)); buf, size = [], 0
    if buf:
        req("POST", "/insert/jsonline?" + q, b"".join(buf))
    del recs
    req("POST", "/internal/force_flush")
    time.sleep(2)
    req("POST", "/internal/force_merge?partition_prefix=20260101")
    last, stable = None, 0
    for _ in range(600):
        time.sleep(2)
        d = du_root(DIR)
        active = metric("vl_active_merges")
        stable = stable + 1 if (d == last and active == 0) else 0
        last = d
        if stable >= 5:
            break
    rows_ingested = metric("vl_rows_ingested_total")
    parts = {k: metric(k) for k in ("vl_storage_parts", "vl_data_size_bytes", "vl_compressed_data_size_bytes", "vl_uncompressed_data_size_bytes")}
    disk = du_root(DIR)
    # read back everything
    body = urllib.parse.urlencode({"query": "* | fields seq, _msg", "start": "2025-12-31T00:00:00Z", "end": "2026-01-03T00:00:00Z"})
    h = conn()
    h.request("POST", "/select/logsql/query", body=body, headers={"Content-Type": "application/x-www-form-urlencoded"})
    resp = h.getresponse()
    got, extra = {}, 0
    for line in resp:
        if not line.strip():
            continue
        o = json.loads(line)
        s = int(o["seq"])
        if s in got:
            extra += 1
        got[s] = o.get("_msg")
    ok, bad = C.compare(exp, got)
    detail = []
    if bad:
        recs = {r["seq"]: r["text"] for r in C.load(c)}
        for s, why in bad[:5]:
            detail.append({"seq": s, "why": why, **(C.describe_diff(recs[s], got[s]) if got.get(s) is not None else {})})
    notes = (f"_time = 2026-01-01T00:00:00Z + seq ms; _stream_fields={STREAM or '(none: single stream)'}; seq/turn/kind/valid_at as regular fields; "
             f"force_flush + force_merge(20260101), waited for vl_active_merges=0 and stable du; metrics={json.dumps(parts)}; "
             f"rows_ingested={rows_ingested:.0f} returned={len(got)} dup_returned={extra}; build untimed (size-only, outside bench lock)")
    if bad:
        notes += f"; mismatches={len(bad)} detail={json.dumps(detail)}"
    C.emit(system="victorialogs", version=f"{ver} ({IMAGE})", variant=f"jsonline _msg=text stream={STREAM or 'none'}", corpus=c, raw=raw,
           disk_bytes=disk, how_measured="du -sb -storageDataPath after force_flush + force_merge", exact=not bad,
           verified_records=ok, read20_us_p50=None, read20_us_p99=None, load_s=None, notes=notes)
    C.rm_container(NAME)


if __name__ == "__main__":
    try:
        for c in C.corpora(sys.argv):
            run(c)
    finally:
        C.rm_container(NAME)
        wipe()
        shutil.rmtree(DIR, ignore_errors=True)
