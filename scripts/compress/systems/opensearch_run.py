"""OpenSearch comparator (single-node throwaway container, security plugin disabled, heap 1g).

Two indices per corpus, both index.codec=best_compression, 1 shard, 0 replicas, _source on:
  idx  : text mapped {"type": "text"}                 (indexed + _source)
  noidx: text mapped {"type": "text", "index": false} (only in _source)
Other fields: seq long, session integer, turn integer, kind keyword, valid_at long (default
index + doc_values). _id = seq. Bulk load (~8 MiB NDJSON requests, refresh_interval -1), then
refresh, _forcemerge?max_num_segments=1, flush. Size = _cat/indices?bytes=b pri.store.size.
Verify via _mget of _source.text; point reads = one _mget of 20 ids (keep-alive HTTP).
Phase 1 (size-only, no bench lock): build + size + verify all; phase 2: locked read loops.
Run: python3 opensearch_run.py [corpus ...]
"""
import http.client, json, os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
import common as C

IMAGE = "opensearchproject/opensearch:2.19.6"
NAME = "oojuro-compress-opensearch"
PORT = 19200
DIR = os.path.join(C.SYS, "opensearch")
TEXT_MAP = {"idx": {"type": "text"}, "noidx": {"type": "text", "index": False}}


class OS:
    def __init__(self):
        self.h = http.client.HTTPConnection("127.0.0.1", PORT, timeout=3600)

    def call(self, method, path, body=None, ndjson=False):
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode()
        hdr = {"Content-Type": "application/x-ndjson" if ndjson else "application/json"}
        self.h.request(method, path, body=body, headers=hdr)
        r = self.h.getresponse()
        out = r.read()
        if r.status >= 300:
            raise RuntimeError(f"{method} {path}: {r.status} {out[:400]!r}")
        return json.loads(out) if out else None


def start():
    C.rm_container(NAME)
    shutil.rmtree(DIR, ignore_errors=True)
    os.makedirs(os.path.join(DIR, "data"))
    C.docker("run", "-d", "--name", NAME, "--memory", "2g", "--cpus", "2", "-p", f"127.0.0.1:{PORT}:9200",
             "-e", "discovery.type=single-node", "-e", "DISABLE_SECURITY_PLUGIN=true", "-e", "DISABLE_INSTALL_DEMO_CONFIG=true",
             "-e", "OPENSEARCH_JAVA_OPTS=-Xms1g -Xmx1g", "-e", "http.max_content_length=200mb",
             "-v", f"{os.path.join(DIR, 'data')}:/usr/share/opensearch/data", IMAGE)
    C.wait_http(f"http://127.0.0.1:{PORT}/_cluster/health", timeout=300,
                ok=lambda code, body: code == 200 and json.loads(body)["status"] in ("green", "yellow"))


def build(es, ver, c, rows):
    recs = C.load(c)
    n, raw = len(recs), C.raw_bytes(c)
    exp = C.digests(recs)
    for v, tm in TEXT_MAP.items():
        ix = f"{c}-{v}"
        try:
            es.call("DELETE", f"/{ix}")
        except RuntimeError:
            pass
        es.call("PUT", f"/{ix}", {
            "settings": {"index": {"number_of_shards": 1, "number_of_replicas": 0, "codec": "best_compression",
                                   "refresh_interval": "-1"}},
            "mappings": {"dynamic": "strict", "properties": {
                "seq": {"type": "long"}, "session": {"type": "integer"}, "turn": {"type": "integer"},
                "kind": {"type": "keyword"}, "valid_at": {"type": "long"}, "text": tm}}})
        buf, size = [], 0

        def send():
            res = es.call("POST", f"/{ix}/_bulk", b"".join(buf), ndjson=True)
            if res.get("errors"):
                errs = [it for it in res["items"] if it["index"].get("error")]
                raise RuntimeError(f"bulk errors: {len(errs)} first={json.dumps(errs[0])[:400]}")
        for r in recs:
            a = json.dumps({"index": {"_id": str(r["seq"])}}).encode() + b"\n"
            d = json.dumps(r, ensure_ascii=False).encode("utf-8") + b"\n"
            buf += [a, d]; size += len(a) + len(d)
            if size > 8 << 20:
                send(); buf, size = [], 0
        if buf:
            send()
        es.call("POST", f"/{ix}/_refresh")
        es.call("POST", f"/{ix}/_forcemerge?max_num_segments=1")
        es.call("POST", f"/{ix}/_flush")
        es.call("POST", f"/{ix}/_refresh")
        # store stats are cached for index.store.stats_refresh_interval (10s): wait until the
        # reported size matches the Lucene directory on disk and is stable across two refreshes
        prev = None
        for _ in range(30):
            time.sleep(11)
            cat = es.call("GET", f"/_cat/indices/{ix}?bytes=b&format=json&h=index,uuid,docs.count,pri.store.size,store.size")[0]
            lucene = C.du_sb(os.path.join(DIR, "data", "nodes", "0", "indices", cat["uuid"], "0", "index"))
            cur = int(cat["pri.store.size"])
            if cur == prev and abs(cur - lucene) <= 8192:
                break
            prev = cur
        else:
            raise RuntimeError(f"store size did not settle for {ix}: cat={cur} du={lucene}")
        segs = es.call("GET", f"/_cat/segments/{ix}?format=json&bytes=b")
        uuid = cat["uuid"]
        dirb = C.du_sb(os.path.join(DIR, "data", "nodes", "0", "indices", uuid))
        vs = C.verify_set(c, n)
        got = {}
        for i in range(0, len(vs), 1000):
            res = es.call("POST", f"/{ix}/_mget?_source_includes=text", {"ids": [str(s) for s in vs[i:i + 1000]]})
            for d in res["docs"]:
                if d.get("found"):
                    got[int(d["_id"])] = d["_source"]["text"]
        ok, bad = C.compare({s: exp[s] for s in vs}, got)
        notes = (f"text mapping {json.dumps(tm)}; best_compression, 1 shard 0 replicas, forcemerge to 1 segment "
                 f"(segments={len(segs)}), docs.count={cat['docs.count']}; du -sb shard lucene dir={lucene}, index dir incl translog={dirb}; "
                 f"build untimed (size-only step outside bench lock); read = one _mget of 20 ids (full _source), "
                 f"keep-alive HTTP, warm, container --cpus 2 heap 1g")
        if bad:
            notes += f"; mismatches={len(bad)} first={bad[:3]}"
        disk = int(cat["pri.store.size"])
        rows.append(dict(system="opensearch", version=f"{ver} ({IMAGE})", variant=f"best_compression text-{v}", corpus=c,
                         raw=raw, disk_bytes=disk, how_measured="_cat/indices?bytes=b pri.store.size after forcemerge(1)+flush, read once it matched du of the shard index dir and was stable over 2 stats refreshes",
                         exact=not bad and int(cat["docs.count"]) == n, verified_records=ok, load_s=None, notes=notes,
                         _ctx=dict(index=ix, n=n)))
        print(c, v, disk, disk / raw, ok, flush=True)


def reader_for(es):
    def reader(r):
        ix, n = r["_ctx"]["index"], r["_ctx"]["n"]
        cli = {}

        def prep():
            cli["c"] = OS()
            cli["c"].call("GET", "/")

        def read(b):
            res = cli["c"].call("POST", f"/{ix}/_mget", {"ids": [str(s) for s in b]})
            return {int(d["_id"]): d["_source"]["text"] for d in res["docs"] if d.get("found")}
        return C.timed_reads(read, C.read_batches(r["corpus"], n), prep)
    return reader


if __name__ == "__main__":
    corp, resume, build_only = C.flags(sys.argv)
    if not resume:
        start()
    elif not C.docker("ps", "-q", "--filter", f"name=^{NAME}$").stdout.strip():
        C.docker("start", NAME)  # stopped between phases to free RAM; data dir is kept
        C.wait_http(f"http://127.0.0.1:{PORT}/_cluster/health?wait_for_status=green&timeout=60s", timeout=300,
                    ok=lambda code, body: code == 200 and json.loads(body)["status"] == "green")
    done = False
    try:
        es = OS()
        info = es.call("GET", "/")
        ver = f"OpenSearch {info['version']['number']} (Lucene {info['version']['lucene_version']})"
        rows = C.load_pending("opensearch-state") if resume else []
        have = {r["corpus"] for r in rows}
        for c in corp:
            if c not in have and not (resume and rows):  # emitted corpora are not rebuilt on --resume
                build(es, ver, c, rows)
                C.save_pending("opensearch-state", rows)
        if build_only:
            sys.exit(0)
        C.finish("opensearch-state", rows, reader_for(es), lambda r: OS().call("DELETE", f"/{r['_ctx']['index']}"))
        done = True
    finally:
        if done or not (resume or build_only):
            C.rm_container(NAME)
            shutil.rmtree(DIR, ignore_errors=True)
        elif not done and not build_only:
            print("resume failed: container and staged rows kept for retry", file=sys.stderr)
