"""Grafana Loki comparator (optional; single-binary throwaway container, filesystem storage).

One stream per corpus: labels {corpus="<c>"}; log line = text; seq/session/turn/kind/valid_at
as structured metadata (schema v13, tsdb index). Timestamp = 2026-01-01T00:00:00Z + seq ms.
(session as a label, as in the VictoriaLogs run, is not used: Loki's per-tenant stream limits
and index are not built for 57,600 one-entry streams.)
Variants: ingester.chunk_encoding snappy (default) and zstd; chunk_target_size / chunk_block_size
left at defaults (1.5 MiB / 256 KiB).
Push via /loki/api/v1/push (JSON, ~4 MiB requests), POST /flush, graceful docker stop, then
size = du -sb of the chunks directory (chunk objects + shipped tsdb index); WAL size in notes.
Restart and read every record back with query_range (forward, 5000 per page).
Run: python3 loki_run.py [corpus ...]
"""
import http.client, json, os, shutil, subprocess, sys, time, urllib.parse
sys.path.insert(0, os.path.dirname(__file__))
import common as C

IMAGE = "grafana/loki:3.7.8"
DIGEST = "sha256:1107dd5274e0ada47e42472b7a7e71f3b2a2fe878878108f3e2f9e51528f0193"
NAME = "oojuro-compress-loki"
PORT = 13100
DIR = os.path.join(C.SYS, "loki")
BASE_NS = 1767225600 * 10**9  # 2026-01-01T00:00:00Z
ENCODINGS = ["snappy", "zstd"]

CONFIG = """auth_enabled: false
server:
  http_listen_port: 3100
  grpc_listen_port: 9096
  grpc_server_max_recv_msg_size: 268435456
  grpc_server_max_send_msg_size: 268435456
  log_level: warn
common:
  instance_addr: 127.0.0.1
  path_prefix: /loki
  storage:
    filesystem:
      chunks_directory: /loki/chunks
      rules_directory: /loki/rules
  replication_factor: 1
  ring:
    kvstore:
      store: inmemory
schema_config:
  configs:
    - from: 2025-01-01
      store: tsdb
      object_store: filesystem
      schema: v13
      index:
        prefix: index_
        period: 24h
ingester:
  chunk_encoding: {enc}
  chunk_idle_period: 2h
  max_chunk_age: 2h
  wal:
    enabled: true
    dir: /loki/wal
limits_config:
  reject_old_samples: false
  ingestion_rate_mb: 10000
  ingestion_burst_size_mb: 10000
  per_stream_rate_limit: 10000MB
  per_stream_rate_limit_burst: 10000MB
  max_line_size: 0
  allow_structured_metadata: true
  max_entries_limit_per_query: 100000
  max_query_length: 0
  max_query_series: 100000
query_range:
  cache_results: false
pattern_ingester:
  enabled: false
analytics:
  reporting_enabled: false
"""


def req(method, path, body=None, hdr=None):
    h = http.client.HTTPConnection("127.0.0.1", PORT, timeout=600)
    h.request(method, path, body=body, headers=hdr or {})
    r = h.getresponse()
    out = r.read()
    if r.status >= 300:
        raise RuntimeError(f"{method} {path}: {r.status} {out[:400]!r}")
    return out


def up():
    C.docker("run", "-d", "--name", NAME, "--memory", "2g", "--cpus", "2", "--user", "1000:1000",
             "-p", f"127.0.0.1:{PORT}:3100", "-v", f"{DIR}/loki.yaml:/etc/loki/loki.yaml:ro", "-v", f"{DIR}/data:/loki",
             f"grafana/loki@{DIGEST}", "-config.file=/etc/loki/loki.yaml")
    C.wait_http(f"http://127.0.0.1:{PORT}/ready", timeout=180)


def run(c):
    recs = C.load(c)
    n, raw = len(recs), C.raw_bytes(c)
    exp = C.digests(recs)
    for enc in ENCODINGS:
        C.rm_container(NAME)
        shutil.rmtree(DIR, ignore_errors=True)
        os.makedirs(os.path.join(DIR, "data"))
        with open(os.path.join(DIR, "loki.yaml"), "w") as f:
            f.write(CONFIG.replace("{enc}", enc))
        up()
        ver = json.loads(req("GET", "/loki/api/v1/status/buildinfo"))["version"]
        vals, size = [], 0

        def push():
            body = json.dumps({"streams": [{"stream": {"corpus": c}, "values": vals}]}, ensure_ascii=False).encode("utf-8")
            req("POST", "/loki/api/v1/push", body, {"Content-Type": "application/json"})
        for r in recs:
            md = {k: str(r[k]) for k in ("seq", "session", "turn", "kind", "valid_at")}
            vals.append([str(BASE_NS + r["seq"] * 10**6), r["text"], md]); size += len(r["text"]) + 80
            if size > 4 << 20:
                push(); vals, size = [], 0
        if vals:
            push()
        req("POST", "/flush")
        time.sleep(5)
        subprocess.run(["docker", "stop", "-t", "120", NAME], capture_output=True)
        chunks = C.du_sb(os.path.join(DIR, "data", "chunks"))
        wal = C.du_sb(os.path.join(DIR, "data", "wal")) if os.path.isdir(os.path.join(DIR, "data", "wal")) else 0
        whole = C.du_sb(os.path.join(DIR, "data"))
        idx = C.du_sb(os.path.join(DIR, "data", "chunks", "index")) if os.path.isdir(os.path.join(DIR, "data", "chunks", "index")) else 0
        nchunks = sum(len(fs) for _, _, fs in os.walk(os.path.join(DIR, "data", "chunks")))
        C.rm_container(NAME)
        up()
        got, start = {}, BASE_NS
        end = BASE_NS + (n + 1) * 10**6
        while True:
            q = urllib.parse.urlencode({"query": f'{{corpus="{c}"}}', "start": str(start), "end": str(end),
                                        "limit": "5000", "direction": "forward"})
            res = json.loads(req("GET", "/loki/api/v1/query_range?" + q))["data"]["result"]
            page = [v for s in res for v in s["values"]]
            if not page:
                break
            last = 0
            for v in page:
                ts = int(v[0]); s = (ts - BASE_NS) // 10**6
                got[s] = v[1]; last = max(last, ts)
            start = last + 1
            if len(page) < 5000:
                break
        ok, bad = C.compare(exp, got)
        detail = []
        if bad:
            src = {r["seq"]: r["text"] for r in recs}
            detail = [{"seq": s, "why": w, **(C.describe_diff(src[s], got[s]) if got.get(s) is not None else {})} for s, w in bad[:5]]
        notes = (f"labels {{corpus}} (1 stream), 5 fields as structured metadata, ts = 2026-01-01Z + seq ms, chunk_encoding={enc}, "
                 f"default chunk_target_size/block_size; after /flush + graceful stop: chunks_dir={chunks} (tsdb index part={idx}, "
                 f"files={nchunks}), wal={wal}, whole data dir={whole}; verify = query_range all records, returned={len(got)}; "
                 f"build untimed (size-only, outside bench lock)")
        if bad:
            notes += f"; mismatches={len(bad)} detail={json.dumps(detail)}"
        C.emit(system="loki", version=f"Loki {ver} ({IMAGE} {DIGEST[:19]}...)", variant=f"chunk_encoding={enc}", corpus=c, raw=raw,
               disk_bytes=chunks, how_measured="du -sb chunks dir (chunks + shipped tsdb index) after /flush + graceful stop",
               exact=not bad, verified_records=ok, read20_us_p50=None, read20_us_p99=None, load_s=None, notes=notes)
        C.rm_container(NAME)
        shutil.rmtree(DIR, ignore_errors=True)


if __name__ == "__main__":
    try:
        for c in C.corpora(sys.argv):
            run(c)
    finally:
        C.rm_container(NAME)
        shutil.rmtree(DIR, ignore_errors=True)
