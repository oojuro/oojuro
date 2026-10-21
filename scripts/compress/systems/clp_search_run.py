"""clp-s search latency over level-19 archives (swe, ohjson) for identifier-token queries.

Archives: clp-s c --compression-level 19 (same image/flags as clp_run.py), re-verified by a
full `clp-s x --ordered` decompress. Queries: .scratch/out/identifier-queries.json
(50 lowercase tokens per corpus). Each query = `clp-s s -i --count <arch> 'text: *<token>*'`
(case-insensitive substring on the text field) via `docker exec` into ONE long-running
container, so container startup is excluded; wall time is client-side around docker exec.
Overheads: `docker exec <c> true` and a trivial query `seq: -1` (no match, int column only).
Untimed warm pass first (also checks counts against a Python case-insensitive substring
count). Timed: 3 passes x (trivial x10 + 50 queries) per corpus under the bench lock -- one
hold for everything when the warm pass predicts < 60 s, else one hold per pass.
Run: python3 clp_search_run.py            (SKIP_TIMING="<reason>" runs only the untimed
     archive/verify/count part and records latency as not measured)
"""
import json, os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
import common as C

IMAGE = "ghcr.io/y-scope/clp/clp-core-x86-ubuntu-jammy"
DIGEST = "sha256:b2d550bf9ff60e431118658cc4916455bd7ce8da3adc27b164f388e3f92d48de"
NAME = "oojuro-compress-clp-search"
DIR = os.path.join(C.SYS, "clp-search")
CORP = ["swe", "ohjson"]
QFILE = os.path.join(C.ROOT, ".scratch", "out", "identifier-queries.json")
PASSES, TRIVIAL_REPS = 3, 10


def drun(*args):
    r = subprocess.run(["docker", "run", "--rm", "--name", NAME + "-c", "--memory", "2g", "--cpus", "2", "--user", "1000:1000",
                        "-v", f"{C.DATA}:/in:ro", "-v", f"{DIR}:/w", "--entrypoint", "/clp/clp-s", f"{IMAGE}@{DIGEST}", *args],
                       capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(r.stderr[-600:])
    return r.stdout


def dexec(*cmd):
    t0 = time.perf_counter_ns()
    r = subprocess.run(["docker", "exec", NAME, *cmd], capture_output=True, text=True)
    us = (time.perf_counter_ns() - t0) / 1000.0
    if r.returncode:
        raise RuntimeError(f"{cmd}: rc={r.returncode} {r.stderr[-400:]}")
    return us, r.stdout


def count_of(out):
    return sum(json.loads(l)["count"] for l in out.splitlines() if l.strip())


def search(c, q):
    return dexec("/clp/clp-s", "s", "-i", "--count", f"/w/{c}/arch", q)


def main():
    queries = json.load(open(QFILE))
    shutil.rmtree(DIR, ignore_errors=True)
    meta = {}
    for c in CORP:
        os.makedirs(os.path.join(DIR, c))
        drun("c", "--compression-level", "19", f"/w/{c}/arch", f"/in/{c}.jsonl")
        disk = C.du_sb(os.path.join(DIR, c, "arch"))
        drun("x", "--ordered", f"/w/{c}/arch", f"/w/{c}/out")
        got = {}
        for root, _, fs in os.walk(os.path.join(DIR, c, "out")):
            for fn in fs:
                with open(os.path.join(root, fn), "rb") as f:
                    for line in f:
                        if line.strip():
                            o = json.loads(line); got[o["seq"]] = o["text"]
        shutil.rmtree(os.path.join(DIR, c, "out"))
        recs = C.load(c)
        ok, bad = C.compare(C.digests(recs), got)
        low = [r["text"].lower() for r in recs]
        expect = {t: sum(t in x for x in low) for t in queries[c]}
        meta[c] = dict(disk=disk, ok=ok, bad=bad, expect=expect, raw=C.raw_bytes(c), n=len(recs))
        del recs, low, got
    C.rm_container(NAME)
    C.docker("run", "-d", "--name", NAME, "--memory", "2g", "--cpus", "2", "--user", "1000:1000",
             "-v", f"{DIR}:/w:ro", "--entrypoint", "sleep", f"{IMAGE}@{DIGEST}", "infinity")
    try:
        # untimed warm pass + correctness
        est = 0.0
        for c in CORP:
            m = meta[c]; m["counts"] = {}
            for t in queries[c]:
                us, out = search(c, f"text: *{t}*"); est += us
                m["counts"][t] = count_of(out)
            us, out = search(c, "seq: -1"); est += us * TRIVIAL_REPS
            m["trivial_count"] = count_of(out)
        per_pass_s = est / 1e6
        lat = {c: {"q": [], "trivial": [], "true": [], "passes": []} for c in CORP}

        def one_pass():
            for c in CORP:
                for _ in range(TRIVIAL_REPS):
                    lat[c]["true"].append(dexec("true")[0])
                    lat[c]["trivial"].append(search(c, "seq: -1")[0])
                ps = []
                for t in queries[c]:
                    us, out = search(c, f"text: *{t}*")
                    if count_of(out) != meta[c]["counts"][t]:
                        raise RuntimeError(f"count changed between passes for a {c} query")
                    ps.append(us)
                lat[c]["q"] += ps; lat[c]["passes"].append(C.pct(ps, 50))
        one_hold = per_pass_s * PASSES < 60
        skip = os.environ.get("SKIP_TIMING")
        if skip:
            hold_desc = f"timed passes not run: {skip}"
        elif one_hold:
            with C.bench_lock():
                for _ in range(PASSES):
                    one_pass()
        else:
            for i in range(PASSES):
                if i:
                    time.sleep(C.GAP_S)
                with C.bench_lock():
                    one_pass()
        if not skip:
            hold_desc = (f"{'one bench-lock hold for all 3 passes' if one_hold else 'one bench-lock hold per pass'} "
                         f"(warm-pass estimate {per_pass_s:.1f} s/pass)")
    finally:
        C.rm_container(NAME)
    for c in CORP:
        m, L = meta[c], lat[c]
        mism = {t: (m["counts"][t], m["expect"][t]) for t in queries[c] if m["counts"][t] != m["expect"][t]}
        timing = ("" if not L["q"] else
                  f"per-pass p50 us={[round(x) for x in L['passes']]}; trivial query 'seq: -1' p50={C.pct(L['trivial'], 50):.0f} us, "
                  f"docker exec true p50={C.pct(L['true'], 50):.0f} us; ")
        notes = (f"clp-s s -i --count <arch> 'text: *<token>*' via docker exec into one long-running container "
                 f"(--cpus 2 --memory 2g); 50 queries x {PASSES} passes; wall time client-side incl. docker exec; "
                 f"{hold_desc}; {timing}"
                 f"match counts sum={sum(m['counts'].values())}, queries with 0 matches={sum(v == 0 for v in m['counts'].values())}; "
                 f"count check vs python case-insensitive substring over records: {len(queries[c]) - len(mism)}/{len(queries[c])} equal"
                 + (f" mismatches(clp,python)={json.dumps(mism)}" if mism else "")
                 + f"; archive re-verified by full decompress: {m['ok']}/{m['n']} texts equal")
        C.emit(system="clp-s-search", version=f"clp-s @ y-scope/clp main rev e5351214 (image {DIGEST[:19]}...)",
               variant="level19 archive, -i --count text:*token*", corpus=c, raw=m["raw"], disk_bytes=m["disk"],
               how_measured="du -sb archives dir", exact=not m["bad"], verified_records=m["ok"],
               read20_us_p50=None, read20_us_p99=None, load_s=None, notes=notes,
               search_us_p50=C.pct(L["q"], 50), search_us_p99=C.pct(L["q"], 99),
               trivial_query_us_p50=C.pct(L["trivial"], 50), trivial_query_us_p99=C.pct(L["trivial"], 99),
               docker_exec_true_us_p50=C.pct(L["true"], 50), match_counts=m["counts"],
               timed_queries=len(L["q"]), trivial_query_count=m["trivial_count"])
    shutil.rmtree(DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
