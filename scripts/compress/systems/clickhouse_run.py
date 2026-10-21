"""ClickHouse comparator (local throwaway container).

One MergeTree table per (corpus, variant), ORDER BY seq; columns
seq UInt64, session UInt32, turn UInt32, kind LowCardinality(String), valid_at UInt64,
text String CODEC(<variant>) (other columns: server default codec, LZ4).
Variants: lz4, zstd3, zstd19 (all default index_granularity=8192 / index_granularity_bytes=10MiB),
plus zstd19-g256 (index_granularity=256) as a point-read granularity contrast.
Insert = the corpus .jsonl posted once via HTTP as FORMAT JSONEachRow, then OPTIMIZE TABLE FINAL.
Size = sum(bytes_on_disk) of active parts in system.parts; text column data_compressed_bytes
from system.columns is recorded in notes.
Run: python3 clickhouse_run.py [corpus ...]
"""
import base64, http.client, json, os, shutil, subprocess, sys, time, urllib.parse
sys.path.insert(0, os.path.dirname(__file__))
import common as C

IMAGE = "clickhouse/clickhouse-server:26.9.23.15"
NAME = "oojuro-compress-clickhouse"
PORT = 18123
DIR = os.path.join(C.SYS, "clickhouse")
AUTH = "Basic " + base64.b64encode(b"bench:bench").decode()  # throwaway local container login
VARIANTS = {
    "lz4": ("LZ4", ""),
    "zstd3": ("ZSTD(3)", ""),
    "zstd19": ("ZSTD(19)", ""),
    "zstd19-g256": ("ZSTD(19)", " SETTINGS index_granularity = 256"),
}


class CH:
    def __init__(self):
        self.conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=3600)

    def q(self, sql, body=None, params=None):
        p = {"output_format_json_quote_64bit_integers": "0"}
        p.update(params or {})
        if body is None:
            path, data = "/?" + urllib.parse.urlencode(p), sql.encode()
        else:
            p["query"] = sql
            path, data = "/?" + urllib.parse.urlencode(p), body
        self.conn.request("POST", path, body=data, headers={"Authorization": AUTH})
        r = self.conn.getresponse()
        out = r.read()
        if r.status != 200:
            raise RuntimeError(f"{r.status}: {out[:500]!r}")
        return out

    def rows(self, sql):
        return [json.loads(l) for l in self.q(sql + " FORMAT JSONEachRow").splitlines() if l]


def start():
    C.rm_container(NAME)
    shutil.rmtree(DIR, ignore_errors=True)
    os.makedirs(os.path.join(DIR, "data"))
    C.docker("run", "-d", "--name", NAME, "--memory", "2g", "--cpus", "2", "--ulimit", "nofile=262144:262144",
             "-p", f"127.0.0.1:{PORT}:8123", "-e", "CLICKHOUSE_USER=bench", "-e", "CLICKHOUSE_PASSWORD=bench",
             "-v", f"{os.path.join(DIR, 'data')}:/var/lib/clickhouse", IMAGE)
    t0 = time.time()
    while True:
        try:
            CH().q("SELECT 1")
            return
        except Exception:
            if time.time() - t0 > 120:
                raise
            time.sleep(1)


def build(ch, ver, c, rows):
    recs = C.load(c)
    n, raw = len(recs), C.raw_bytes(c)
    exp = C.digests(recs)
    del recs
    with open(C.jsonl_path(c), "rb") as f:
        body = f.read()
    for v, (codec, extra) in VARIANTS.items():
        t = f"t_{c}_{v.replace('-', '_')}"
        ch.q(f"DROP TABLE IF EXISTS {t} SYNC")
        ch.q(f"CREATE TABLE {t} (seq UInt64, session UInt32, turn UInt32, kind LowCardinality(String), "
             f"valid_at UInt64, text String CODEC({codec})) ENGINE = MergeTree ORDER BY seq{extra}")
        ch.q(f"INSERT INTO {t} FORMAT JSONEachRow", body=body)
        ch.q(f"OPTIMIZE TABLE {t} FINAL")
        p = ch.rows(f"SELECT count() parts, sum(rows) rows, sum(bytes_on_disk) b, sum(data_compressed_bytes) dc, "
                    f"sum(data_uncompressed_bytes) du, sum(marks) marks FROM system.parts WHERE database = currentDatabase() "
                    f"AND table = '{t}' AND active")[0]
        col = ch.rows(f"SELECT name, data_compressed_bytes dc, data_uncompressed_bytes du FROM system.columns "
                      f"WHERE database = currentDatabase() AND table = '{t}'")
        colmap = {r["name"]: (r["dc"], r["du"]) for r in col}
        vs = C.verify_set(c, n)
        got = {}
        for i in range(0, len(vs), 5000):
            chunk = vs[i:i + 5000]
            for r in ch.rows(f"SELECT seq, text FROM {t} WHERE seq IN ({','.join(map(str, chunk))})"):
                got[r["seq"]] = r["text"]
        ok, bad = C.compare({s: exp[s] for s in vs}, got)
        notes = (f"text CODEC({codec}){extra}; parts={p['parts']} rows={p['rows']} marks={p['marks']} "
                 f"text_col_compressed={colmap['text'][0]} text_col_uncompressed={colmap['text'][1]} all_cols_compressed={p['dc']}; "
                 f"other cols compressed: {json.dumps({k: v[0] for k, v in colmap.items() if k != 'text'})}; "
                 f"build = HTTP JSONEachRow insert + OPTIMIZE FINAL (untimed, size-only step outside bench lock); "
                 f"read = one HTTP SELECT seq,text WHERE seq IN (20 seqs), keep-alive, JSONEachRow output, warm, container --cpus 2")
        if bad:
            notes += f"; mismatches={len(bad)} first={bad[:3]}"
        rows.append(dict(system="clickhouse", version=f"{ver} ({IMAGE})", variant=v, corpus=c, raw=raw, disk_bytes=p["b"],
                         how_measured="sum(bytes_on_disk) FROM system.parts WHERE active, after OPTIMIZE FINAL",
                         exact=not bad and p["rows"] == n, verified_records=ok, load_s=None, notes=notes,
                         text_col_compressed=colmap["text"][0], _ctx=dict(table=t, n=n)))
        print(c, v, p["b"], p["b"] / raw, ok, flush=True)


def reader_for(ch):
    def reader(r):
        t, n = r["_ctx"]["table"], r["_ctx"]["n"]
        cli = {}

        def prep():
            cli["c"] = CH()
            cli["c"].q("SELECT 1")

        def read(b):
            return {x["seq"]: x["text"] for x in cli["c"].rows(f"SELECT seq, text FROM {t} WHERE seq IN ({','.join(map(str, b))})")}
        return C.timed_reads(read, C.read_batches(r["corpus"], n), prep)
    return reader


def running():
    return C.docker("ps", "-q", "--filter", f"name=^{NAME}$").stdout.strip() != ""


if __name__ == "__main__":
    corp, resume, build_only = C.flags(sys.argv)
    rebuild = C.rebuild_list(sys.argv)
    if not resume:
        start()
    elif not running():
        if C.docker("ps", "-aq", "--filter", f"name=^{NAME}$").stdout.strip():
            C.docker("start", NAME)  # stopped between phases to free RAM; data dir is kept
            t0 = time.time()
            while True:
                try:
                    CH().q("SELECT count() FROM system.tables"); break
                except Exception:
                    if time.time() - t0 > 180:
                        raise
                    time.sleep(1)
        else:
            start()
    done = False
    try:
        ch = CH()
        ver = ch.q("SELECT version()").decode().strip()
        rows = C.load_pending("clickhouse-state") if resume else []
        rows = [r for r in rows if r["corpus"] not in rebuild]
        have = {r["corpus"] for r in rows}
        for c in corp:
            if c not in have and (not resume or c in rebuild or not rows):
                build(ch, ver, c, rows)
                C.save_pending("clickhouse-state", rows)
        if build_only:
            sys.exit(0)
        # zstd19 at default granularity takes ~30 s per loop: its own hold; the other three share one
        C.finish("clickhouse-state", rows, reader_for(ch),
                 lambda r: CH().q(f"DROP TABLE IF EXISTS {r['_ctx']['table']} SYNC"),
                 group=lambda r: r["variant"] == "zstd19")
        done = True
    finally:
        if done or not (resume or build_only):
            C.rm_container(NAME)
            subprocess.run(["docker", "run", "--rm", "--name", NAME + "-rm", "-v", f"{DIR}:/d", "--entrypoint", "sh", IMAGE,
                           "-c", "rm -rf /d/data"], capture_output=True)
            shutil.rmtree(DIR, ignore_errors=True)
        elif not done and not build_only:
            print("resume failed: container and staged rows kept for retry", file=sys.stderr)
