"""Shared helpers for the system comparator runs (no dataset text lives here).

Inputs:  .scratch/data/s64/<corpus>.jsonl  (+ sibling .txt whose size is `raw`)
Output:  .scratch/out/sys-comparators.jsonl (one JSON object per measurement)
"""
import contextlib, fcntl, hashlib, json, os, random, subprocess, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
DATA = os.path.join(ROOT, ".scratch", "data", "s64")
SYS = os.path.join(ROOT, ".scratch", "sys")
OUT = os.path.join(ROOT, ".scratch", "out", "sys-comparators.jsonl")
LOCK = os.path.expanduser(os.environ.get("OOJURO_BENCH_LOCK", "~/.cache/oojuro-bench.lock"))
CORPORA = ["swe", "ohjson", "othink", "hermes", "ohsft", "fiqa"]
FULL_VERIFY_CORPUS = "ohsft"  # every record read back for this corpus; 1000-sample for the rest
SAMPLE_N = 1000
READ_QUERIES, READ_BATCH = 200, 20


def corpora(argv):
    return argv[1:] if len(argv) > 1 else CORPORA


def jsonl_path(c):
    return os.path.join(DATA, c + ".jsonl")


def raw_bytes(c):
    return os.path.getsize(os.path.join(DATA, c + ".txt"))


def load(c):
    with open(jsonl_path(c), "rb") as f:
        return [json.loads(line) for line in f]


def sha(s):
    return hashlib.sha256(s.encode("utf-8") if isinstance(s, str) else s).hexdigest()


def digests(recs):
    return {r["seq"]: sha(r["text"]) for r in recs}


def verify_set(c, n):
    """seqs to read back: all for FULL_VERIFY_CORPUS, else a fixed 1000 sample."""
    if c == FULL_VERIFY_CORPUS:
        return list(range(n))
    return sorted(random.Random(f"verify-{c}").sample(range(n), min(SAMPLE_N, n)))


def read_batches(c, n):
    rng = random.Random(f"read-{c}")
    return [rng.sample(range(n), READ_BATCH) for _ in range(READ_QUERIES)]


def compare(expect, got):
    """expect: seq->sha; got: seq->text|None. Returns (ok_count, problems list)."""
    bad = []
    for s, h in expect.items():
        t = got.get(s)
        if t is None:
            bad.append((s, "missing"))
        elif sha(t) != h:
            bad.append((s, "sha-mismatch"))
    return len(expect) - len(bad), bad


def describe_diff(orig, got):
    """Content-free description of how a returned text differs (lengths, positions only)."""
    o, g = orig.encode("utf-8"), got.encode("utf-8") if isinstance(got, str) else got
    i = 0
    while i < min(len(o), len(g)) and o[i] == g[i]:
        i += 1
    return {"orig_len": len(o), "got_len": len(g), "first_diff_at": i,
            "orig_byte": o[i] if i < len(o) else None, "got_byte": g[i] if i < len(g) else None}


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return None
    k = (len(xs) - 1) * p / 100.0
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


@contextlib.contextmanager
def bench_lock():
    """Same lock as `flock $OOJURO_BENCH_LOCK <cmd>` (flock(2), exclusive)."""
    os.makedirs(os.path.dirname(LOCK), exist_ok=True)
    fd = os.open(LOCK, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def timed_reads(fn, batches):
    """fn(list_of_seqs) -> dict seq->text. One lock around the whole loop."""
    lat = []
    with bench_lock():
        for b in batches:
            t0 = time.perf_counter_ns()
            got = fn(b)
            lat.append((time.perf_counter_ns() - t0) / 1000.0)
            if len(got) != len(b):
                raise RuntimeError(f"read returned {len(got)} of {len(b)}")
    return pct(lat, 50), pct(lat, 99)


def du_sb(path):
    return int(subprocess.check_output(["du", "-sb", path]).split()[0])


def emit(**kw):
    keys = ["system", "version", "variant", "corpus", "raw", "disk_bytes", "ratio", "how_measured",
            "exact", "verified_records", "read20_us_p50", "read20_us_p99", "load_s", "notes"]
    rec = {k: kw.get(k) for k in keys}
    if rec["ratio"] is None and rec["disk_bytes"] is not None:
        rec["ratio"] = rec["disk_bytes"] / rec["raw"]
    for k, v in kw.items():
        if k not in rec:
            rec.setdefault("extra", {})[k] = v
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(json.dumps(rec), flush=True)


def docker(*args, check=True):
    return subprocess.run(["docker", *args], check=check, capture_output=True, text=True)


def rm_container(name):
    docker("rm", "-f", name, check=False)


def wait_http(url, timeout=180, ok=lambda code, body: code == 200):
    import urllib.request, urllib.error
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            with urllib.request.urlopen(url, timeout=5) as r:
                if ok(r.status, r.read()):
                    return
        except Exception:
            pass
        time.sleep(1)
    raise RuntimeError(f"timeout waiting for {url}")
