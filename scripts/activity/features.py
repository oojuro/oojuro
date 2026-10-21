"""Per-record activity features from an `oojuro export` stream (one pass).

    oojuro export --from 30d | nice -n 19 python3 -I features.py - OUT.tsv.zst
    nice -n 19 python3 -I features.py export.jsonl.zst OUT.tsv.zst

For every record, in time order, writes one TSV row of numbers (no text):

    id ts_us writer session kind fam err bytes zs zd cdc_new ng ntok
    new_tool new_files repeat echo

- `err`: 0 none, 1 error output, 2 failed call (common.error_level).
- Novelty estimators, each the information a record adds given the
  writer's own earlier records (the record text only; activity records only):
  - `zs`  bits: zstd level 3 streaming per writer (8 MiB window), one block
          flushed per record: the code length given the recent history.
  - `zd`  bits: the record compressed alone with a zstd dictionary trained
          (110 KiB) on the writer's records before the current UTC day
          (ring of the last 16 MiB); before the first training, no dictionary.
  - `cdc_new` bytes: bytes of content-defined chunks (~1 KiB) the writer
          never stored before.
  - `ng`  bits: adaptive token model (hashed unigram + bigram with Dirichlet
          backoff, digits folded to one class), first 20k tokens of a record.
- `new_tool`: first call of this tool name by the writer; `new_files`:
  paths first named by the writer; `repeat`: a tool call whose exact input
  occurred among the session's last 32 calls.

Timings per estimator go to stderr (and OUT.timing.json).
"""
import json
import re
import sys
import time
from array import array
from collections import OrderedDict, deque
from math import log2

from compression import zstd
from compression.zstd import CompressionParameter, ZstdCompressor

sys.path.insert(0, __import__("os").path.dirname(__file__))
from common import FAM, KIND, Families, call_paths, cdc_chunks, error_level, is_echo, open_records  # noqa: E402

DAY = 86_400_000_000
LIVE_STREAMS = 6
RING_BYTES = 16 << 20
DICT_BYTES = 112_640
SAMPLE_CAP = 64 << 10
NG_TOKENS = 20_000
UNI_BITS, BI_BITS = 23, 24
TOK = re.compile(r"[A-Za-z_][A-Za-z0-9_]*|[0-9]+|[^\sA-Za-z0-9_]")


class Timer:
    def __init__(self):
        self.t = {}
        self.bytes = {}

    def add(self, k, dt, n):
        self.t[k] = self.t.get(k, 0.0) + dt
        self.bytes[k] = self.bytes.get(k, 0) + n


class Streams:
    """zstd streaming compressors per writer (LRU: at most LIVE_STREAMS live)."""

    def __init__(self):
        self.live = OrderedDict()
        self.evicted = 0

    def bits(self, w, data):
        c = self.live.get(w)
        if c is None:
            c = ZstdCompressor(options={CompressionParameter.compression_level: 3, CompressionParameter.window_log: 23})
            self.live[w] = c
            if len(self.live) > LIVE_STREAMS:
                self.live.popitem(last=False)
                self.evicted += 1
        else:
            self.live.move_to_end(w)
        return 8 * len(c.compress(data, ZstdCompressor.FLUSH_BLOCK))


class Dicts:
    """Per writer: a ring of recent record samples and a dictionary trained
    on it at the first record of each new UTC day."""

    def __init__(self):
        self.ring = {}
        self.ring_bytes = {}
        self.comp = {}
        self.day = {}
        self.trainings = 0
        self.train_s = 0.0

    def bits(self, w, data, ts):
        d = ts // DAY
        if self.day.get(w) != d:
            self.day[w] = d
            samples = self.ring.get(w)
            if samples and self.ring_bytes[w] >= 1 << 20:
                t0 = time.perf_counter()
                try:
                    zd = zstd.train_dict(list(samples), DICT_BYTES)
                    self.comp[w] = ZstdCompressor(level=3, zstd_dict=zd)
                except zstd.ZstdError:
                    pass
                self.train_s += time.perf_counter() - t0
                self.trainings += 1
        c = self.comp.get(w)
        if c is None:
            c = self.comp[w] = ZstdCompressor(level=3)
        n = 8 * len(c.compress(data, ZstdCompressor.FLUSH_FRAME))
        s = data[:SAMPLE_CAP]
        ring = self.ring.setdefault(w, deque())
        ring.append(s)
        self.ring_bytes[w] = self.ring_bytes.get(w, 0) + len(s)
        while self.ring_bytes[w] > RING_BYTES:
            self.ring_bytes[w] -= len(ring.popleft())
        return n


class Ngram:
    """Adaptive hashed unigram + bigram token model shared by all writers
    (keys salted with the writer). Code length in bits per record."""

    def __init__(self):
        self.uni = array("I", bytes(4 << UNI_BITS))
        self.bi = array("I", bytes(4 << BI_BITS))
        self.n = {}
        self.um = (1 << UNI_BITS) - 1
        self.bm = (1 << BI_BITS) - 1

    def bits(self, w, text):
        uni, bi, um, bm = self.uni, self.bi, self.um, self.bm
        n = self.n.get(w, 0)
        total = 0.0
        prev = hash((w, "\x02"))
        k = 0
        mu = 4.0
        for m in TOK.finditer(text):
            tok = m.group()
            if tok[0].isdigit():
                tok = "0"
            h = hash((w, tok))
            hu = h & um
            cu = uni[hu]
            pu = (cu + 0.5) / (n + 524_288.0)
            hb = hash((prev, tok)) & bm
            cb = bi[hb]
            ctx = uni[prev & um]
            p = (cb + mu * pu) / (ctx + mu)
            total -= log2(p)
            if cu < 0xFFFFFFFF:
                uni[hu] = cu + 1
            if cb < 0xFFFFFFFF:
                bi[hb] = cb + 1
            n += 1
            prev = h
            k += 1
            if k >= NG_TOKENS:
                break
        self.n[w] = n
        return total, k


def main():
    src, out = sys.argv[1], sys.argv[2]
    timer = Timer()
    streams, dicts, ngram = Streams(), Dicts(), Ngram()
    fams = Families()
    chunks = {}
    tools_seen = {}
    files_seen = {}
    recent_calls = {}
    writers = {}
    rows = 0
    t_start = time.perf_counter()
    with zstd.open(out, "wt", encoding="utf-8") as f:
        f.write("id\tts_us\twriter\tsession\tkind\tfam\terr\tbytes\tzs\tzd\tcdc_new\tng\tntok\t"
                "new_tool\tnew_files\trepeat\techo\n")
        for r in open_records(src):
            w = writers.setdefault(r["writer"], len(writers))
            kind = r["kind"]
            text = r["text"]
            echo = is_echo(r)
            t0 = time.perf_counter()
            fam = fams.of(r) if kind != "system" else "other"
            err = error_level(r)
            new_tool = new_files = repeat = 0
            if kind == "tool_call" and not echo:
                tool = r.get("tool", "")
                ts = tools_seen.setdefault(w, set())
                if tool not in ts:
                    ts.add(tool)
                    new_tool = 1
                fs = files_seen.setdefault(w, set())
                for p in call_paths(tool, text):
                    if p not in fs:
                        fs.add(p)
                        new_files += 1
                key = hash((tool, text))
                q = recent_calls.setdefault(r["session"], deque(maxlen=32))
                repeat = int(key in q)
                q.append(key)
            timer.add("rules", time.perf_counter() - t0, len(text))
            zs = zd = cdc_new = 0
            ng = 0.0
            ntok = 0
            if kind != "system" and not echo and text:
                data = text.encode("utf-8", "surrogatepass")
                t0 = time.perf_counter()
                zs = streams.bits(w, data)
                t1 = time.perf_counter()
                zd = dicts.bits(w, data, r["ts_us"])
                t2 = time.perf_counter()
                seen = chunks.setdefault(w, set())
                for a, b in cdc_chunks(data):
                    h = hash(data[a:b])
                    if h not in seen:
                        seen.add(h)
                        cdc_new += b - a
                t3 = time.perf_counter()
                ng, ntok = ngram.bits(w, text)
                t4 = time.perf_counter()
                n = len(data)
                timer.add("zs", t1 - t0, n)
                timer.add("zd", t2 - t1, n)
                timer.add("cdc", t3 - t2, n)
                timer.add("ng", t4 - t3, n)
            f.write(f"{r['id']}\t{r['ts_us']}\t{w}\t{r['session']}\t{KIND[kind]}\t{FAM[fam]}\t{err}\t"
                    f"{len(text.encode('utf-8', 'surrogatepass'))}\t{zs}\t{zd}\t{cdc_new}\t{ng:.1f}\t{ntok}\t"
                    f"{new_tool}\t{new_files}\t{repeat}\t{int(echo)}\n")
            rows += 1
            if rows % 50_000 == 0:
                print(f"{rows} rows, {time.perf_counter() - t_start:.0f} s", file=sys.stderr)
    timing = {
        k: {"seconds": round(v, 2), "mb": round(timer.bytes[k] / 1e6, 1),
            "mb_per_s": round(timer.bytes[k] / 1e6 / v, 1) if v else None}
        for k, v in timer.t.items()
    }
    timing["zd_training"] = {"trainings": dicts.trainings, "seconds": round(dicts.train_s, 2)}
    timing["zs_evictions"] = streams.evicted
    timing["rows"] = rows
    timing["writers"] = len(writers)
    timing["wall_s"] = round(time.perf_counter() - t_start, 1)
    with open(out.rsplit(".tsv", 1)[0] + ".timing.json", "w") as tf:
        json.dump(timing, tf, indent=1)
    with open(out.rsplit(".tsv", 1)[0] + ".writers.json", "w") as wf:
        json.dump(writers, wf)
    print(json.dumps(timing, indent=1), file=sys.stderr)


if __name__ == "__main__":
    main()
