"""CLP (YScope) clp-s comparator, run from their published container image.

clp-s c --compression-level {3,19} <archives> <corpus>.jsonl  (all six fields, as-is)
Size = du -sb of the archives dir. Verify: clp-s x --ordered <archives> <out>, parse every
output JSON line, sha256(text) vs the input for every record (all corpora).
No point-read latency (archive search engine, no primary-key get; not natural).
Run: python3 clp_run.py [corpus ...]
"""
import glob, json, os, shutil, subprocess, sys
sys.path.insert(0, os.path.dirname(__file__))
import common as C

IMAGE = "ghcr.io/y-scope/clp/clp-core-x86-ubuntu-jammy:main"
DIGEST = "sha256:b2d550bf9ff60e431118658cc4916455bd7ce8da3adc27b164f388e3f92d48de"
NAME = "oojuro-compress-clp"
DIR = os.path.join(C.SYS, "clp")
LEVELS = [3, 19]


def clp(*args):
    r = subprocess.run(["docker", "run", "--rm", "--name", NAME, "--memory", "2g", "--cpus", "2", "--user", "1000:1000",
                        "-v", f"{C.DATA}:/in:ro", "-v", f"{DIR}:/w", "--entrypoint", "/clp/clp-s", f"{IMAGE.split(':')[0]}@{DIGEST}",
                        *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"clp-s {args}: rc={r.returncode} {r.stderr[-800:]}")
    return r.stdout


def run(c):
    recs = C.load(c)
    n, raw = len(recs), C.raw_bytes(c)
    exp = C.digests(recs)
    del recs
    for lvl in LEVELS:
        shutil.rmtree(DIR, ignore_errors=True); os.makedirs(DIR)
        stats = clp("c", "--compression-level", str(lvl), "--print-archive-stats", f"/w/arch", f"/in/{c}.jsonl")
        disk = C.du_sb(os.path.join(DIR, "arch"))
        nfiles = sum(len(f) for _, _, f in os.walk(os.path.join(DIR, "arch")))
        clp("x", "--ordered", "/w/arch", "/w/out")
        got, dup, order_ok, prev = {}, 0, True, -1
        for p in sorted(glob.glob(os.path.join(DIR, "out", "**", "*"), recursive=True)):
            if not os.path.isfile(p):
                continue
            with open(p, "rb") as f:
                for line in f:
                    if not line.strip():
                        continue
                    o = json.loads(line)
                    s = o["seq"]
                    dup += s in got
                    order_ok &= s > prev; prev = s
                    got[s] = o["text"]
        ok, bad = C.compare(exp, got)
        detail = []
        if bad:
            src = {r["seq"]: r["text"] for r in C.load(c)}
            detail = [{"seq": s, "why": w, **(C.describe_diff(src[s], got[s]) if got.get(s) is not None else {})} for s, w in bad[:5]]
        notes = (f"clp-s c --compression-level {lvl} (all 6 fields), archive files={nfiles}; verify = clp-s x --ordered, all records, "
                 f"returned={len(got)} dup={dup} in_seq_order={order_ok}; stats={stats.strip()[:600]}; "
                 f"compression untimed (size-only, outside bench lock)")
        if bad:
            notes += f"; mismatches={len(bad)} detail={json.dumps(detail)}"
        C.emit(system="clp-s", version=f"clp-s @ y-scope/clp main rev e5351214 (image {DIGEST[:19]}..., created 2026-10-19)",
               variant=f"level{lvl}", corpus=c, raw=raw, disk_bytes=disk, how_measured="du -sb archives dir",
               exact=not bad, verified_records=ok, read20_us_p50=None, read20_us_p99=None, load_s=None, notes=notes)
        shutil.rmtree(DIR, ignore_errors=True)


if __name__ == "__main__":
    for c in C.corpora(sys.argv):
        run(c)
