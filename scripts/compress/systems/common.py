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


_HELD = [False]


@contextlib.contextmanager
def bench_lock():
    """Same lock as `flock $OOJURO_BENCH_LOCK <cmd>` (flock(2), exclusive).
    Re-entrant within this process (a group hold wraps several timed loops)."""
    if _HELD[0]:
        yield
        return
    os.makedirs(os.path.dirname(LOCK), exist_ok=True)
    fd = os.open(LOCK, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        _HELD[0] = True
        yield
    finally:
        _HELD[0] = False
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def timed_reads(fn, batches, prep=None):
    """fn(list_of_seqs) -> dict seq->text. One lock around the whole loop. prep() runs inside
    the lock before timing (untimed): e.g. reopen an HTTP connection that idled while queued."""
    lat = []
    with bench_lock():
        if prep:
            prep()
        for b in batches:
            t0 = time.perf_counter_ns()
            got = fn(b)
            lat.append((time.perf_counter_ns() - t0) / 1000.0)
            if len(got) != len(b):
                raise RuntimeError(f"read returned {len(got)} of {len(b)}")
    return pct(lat, 50), pct(lat, 99)


def save_pending(system, rows):
    """Size/verify rows waiting for their locked read-latency loop (crash-safe staging)."""
    d = os.path.join(SYS, system); os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "pending.json"), "w") as f:
        json.dump(rows, f)


def load_pending(system):
    p = os.path.join(SYS, system, "pending.json")
    return json.load(open(p)) if os.path.exists(p) else []


def flags(argv):
    """argv -> (corpora, resume, build_only). --resume: reuse staged rows + existing stores;
    --build-only: stage rows and keep stores, skip the locked read phase."""
    fl = {a.split("=")[0] for a in argv[1:] if a.startswith("--")}
    cs = [a for a in argv[1:] if not a.startswith("--")] or CORPORA
    return cs, "--resume" in fl, "--build-only" in fl


GAP_S = 15  # idle time between two bench-lock holds so other sessions' waiters get the lock


# corpora whose stores get the timed 200-query loop (one bench-lock hold each); the lock
# queue on the shared box made one hold per (variant, corpus) infeasible
READ_CORPORA = set(os.environ.get("READ_CORPORA", "swe").split(","))


def finish(system, rows, reader, cleanup=None, group=lambda r: 0):
    """Measure the staged rows of READ_CORPORA, then emit every row.
    reader(row) -> (p50, p99) runs one timed 200-query loop and must not load/merge/clean up.
    Rows with the same group(row) share one bench-lock hold (a few seconds of back-to-back
    loops; each loop is timed separately); holds are separated by GAP_S outside the lock.
    cleanup(row) (drop table / delete index / rm store) always runs outside the lock.
    Builds ran outside the bench lock, so load_s stays null."""
    save_pending(system, rows)
    skip = os.environ.get("SKIP_READS")  # reason string: emit staged rows without timed reads
    todo = [] if skip else [r for r in rows if r["corpus"] in READ_CORPORA]
    groups = {}
    for r in todo:
        groups.setdefault(group(r), []).append(r)
    for gi, (g, rs) in enumerate(groups.items()):
        if gi:
            time.sleep(GAP_S)
        with bench_lock():
            for r in rs:
                res = reader(r)
                if res is not None:
                    r["read20_us_p50"], r["read20_us_p99"] = res
        for r in rs:
            r["notes"] += f"; bench-lock hold shared by {len(rs)} back-to-back 200-query loop(s) of this system"
    for r in rows:
        if skip:
            r["notes"] += f"; point-read latency not measured: {skip}"
        elif r["corpus"] not in READ_CORPORA:
            r["notes"] += f"; point-read latency measured only for corpus {'/'.join(sorted(READ_CORPORA))} (bench-lock queue budget)"
    for i, r in enumerate(rows):
        if cleanup:
            cleanup(r)
        r.pop("_ctx", None)
        emit(**r)
        save_pending(system, rows[i + 1:])


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


def rebuild_list(argv):
    """--rebuild=c1,c2 : corpora whose staged rows are dropped and rebuilt on --resume."""
    for a in argv[1:]:
        if a.startswith("--rebuild="):
            return a.split("=", 1)[1].split(",")
    return []
