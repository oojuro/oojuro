#!/bin/bash
# Measure external lossless codecs on the s64 agent-data corpora.
#
# Usage:
#   . ~/.sa-agents/oojuro-toolchain/env.sh compress
#   bash run_external.sh verify <suite>...   # one unlocked repetition: ratio + byte-exact roundtrip (+ indicative speed)
#   bash run_external.sh time   <suite>...   # one more locked, timed repetition of every fast config
#   bash run_external.sh aggregate           # rebuild .scratch/out/ext-codecs.jsonl from the per-run rows
#   suites: bsc openzl repair zpaq paq8px cmix     env: CORPORA="swe ..." restricts corpora
#   Full protocol used for the leaderboard:
#     verify bsc openzl repair zpaq cmix paq8px ; 3 x (time bsc openzl zpaq) ; aggregate
#
# Lock policy (coordinator ruling, 2026-10-20): only codecs that finish a 64 MiB
# run in under ~60 s are timed under the shared bench lock, 3 repetitions
# (bsc, OpenZL, zpaq -method 1/3). Everything slower (zpaq -method 5, paq8px,
# cmix, Re-Pair) is run once, unlocked; its speed is reported with
# "timing":"unlocked-indicative" (ratio is what matters for them). `time`
# mode refuses those suites/configs.
#
# Binaries come from the build_<codec>.sh scripts next to this file (pinned
# sources: see each script). Inputs are the raw-text corpora
# .scratch/data/s64/<corpus>.txt; slow codecs use a byte prefix (head -c N).
#
# One "repetition" = copy the input into a private work dir as `in`, delete old
# outputs (untimed), run the compressor once, run the decompressor once, and
# byte-compare (cmp) the decoded file with `in`. Every repetition, timed or not,
# is a verified roundtrip and is appended as one row to
# .scratch/out/ext-codecs.runs.jsonl.
#   * time mode: each codec invocation runs as
#       systemd-run --user --scope -p MemoryMax=3G -p OOMPolicy=continue -- flock $OOJURO_BENCH_LOCK <timer> <codec ...>
#     (one codec invocation per lock hold). The wall clock (date +%s%N) starts
#     after the lock is acquired, so lock waits are excluded.
#   * verify mode: the same without flock. Its wall times are reported only
#     for configs that have no locked runs, labelled "unlocked-indicative".
#   * /usr/bin/time adds user+sys CPU seconds and peak RSS of the codec process.
#   * MB/s = raw bytes / 1e6 / seconds (convention of crates/oojuro-store
#     codecs.rs); decompress MB/s is measured against raw bytes too.
#   * compressed size = bytes of every file the tool writes for the data
#     (archive, or grammar + sequence for Re-Pair); all repetitions must agree.
# The machine is shared and its load is not controlled: the 1-min loadavg at
# the start of each timed invocation is recorded, and CPU-time MB/s is
# reported next to wall-clock MB/s.
set -uo pipefail
ROOT=$HOME/.sa-agents/oojuro-compress
EXT=$ROOT/.scratch/ext; BIN=$EXT/bin; DATA=${DATA:-$ROOT/.scratch/data/s64}
OUT=${OUT:-$ROOT/.scratch/out/ext-codecs.jsonl}
ROWS=${ROWS:-${OUT%.jsonl}.runs.jsonl}
LOCK=$HOME/.sa-agents/oojuro-bench.lock
ALL_CORPORA="swe ohjson othink hermes ohsft fiqa"
P8=8388608
WORK=${WORK:-$EXT/work}   # per-session work dir (parallel runs must not share one)
mkdir -p "$(dirname "$OUT")" "$WORK"

# run1 <tag> <cmd...>: one memory-capped invocation in the cwd (locked in time mode).
# Writes <tag>.t = "wall_ns user_s sys_s maxrss_kb rc load1"; codec output -> <tag>.log
run1() {
  local tag=$1; shift
  local lock=(); [ "$MODE" = time ] && lock=(flock "$LOCK")
  # OOMPolicy=continue: on hitting the cap the kernel kills the codec, not this timer, so the
  # failure (rc 137, peak RSS) is still recorded.
  systemd-run --user --scope -q -p MemoryMax=3G -p OOMPolicy=continue -- "${lock[@]}" bash -c '
    t=$1; shift
    l=$(cut -d" " -f1 /proc/loadavg)
    s=$(date +%s%N)
    /usr/bin/time -f "%U %S %M" -o "$t.time" "$@" >"$t.log" 2>&1; rc=$?
    e=$(date +%s%N)
    echo "$((e-s)) $(tail -n1 "$t.time") $rc $l" >"$t.t"' _ "$PWD/$tag" "$@"
  [ -s "$tag.t" ] || echo "0 0 0 0 255 0" >"$tag.t"   # the scope itself failed
}

# Per-codec hooks. The work dir holds `in`; compress writes $OUTS; decompress yields $DEC.
setup_codec() { # codec args
  case $1 in
    bsc)        OUTS="in.bsc"; DEC=out
                C=( "$BIN/bsc" e in in.bsc $2 ); D=( "$BIN/bsc" d in.bsc out ) ;;
    zpaq)       OUTS="in.zpaq"; DEC=x/in
                C=( "$BIN/zpaq" a in.zpaq in $2 -threads 1 ); D=( "$BIN/zpaq" x in.zpaq -to x -threads 1 ) ;;
    openzl)     OUTS="in.zl"; DEC=out
                C=( "$BIN/zli" compress -p serial $2 in -o in.zl -f ); D=( "$BIN/zli" decompress in.zl -o out -f ) ;;
    paq8px)     OUTS="in.paq"; DEC=out
                C=( "$BIN/paq8px" $2 in in.paq ); D=( "$BIN/paq8px" -d in.paq out ) ;;
    cmix)       OUTS="in.cmix"; DEC=out
                C=( "$BIN/cmix" -c in in.cmix ); D=( "$BIN/cmix" -d in.cmix out ) ;;
    repair-nav) OUTS="in.R in.C"; DEC=out
                C=( "$BIN/repair-nav" in ); D=( "$BIN/despair-nav" out ) ;;
    repair-wan) OUTS="in.prel in.seq"; DEC=in.u
                C=( "$BIN/repair-wan" -i in ); D=( "$BIN/despair-wan" -i in ) ;;
    *) echo "unknown codec $1" >&2; return 1 ;;
  esac
}

# measure <codec> <version> <args> <corpus> <prefix_bytes> <notes>: one repetition -> one row
measure() {
  local codec=$1 ver=$2 args=$3 corpus=$4 prefix=$5 notes=$6
  # RESUME_WK=<work dir>: reuse a repetition whose compress step already finished
  # there (c.t written); only the decompress + verification are run.
  local WK resume=0
  if [ -n "${RESUME_WK:-}" ] && [ -s "$RESUME_WK/c.t" ]; then WK=$RESUME_WK; resume=1; else WK=$(mktemp -d "$WORK/m.XXXXXX"); fi
  ( cd "$WK" || exit 1
    setup_codec "$codec" "$args" || exit 1
    if [ $resume = 0 ]; then
      if [ "$prefix" -gt 0 ]; then head -c "$prefix" "$DATA/$corpus.txt" >in; else cp "$DATA/$corpus.txt" in; fi
      run1 c "${C[@]}"
    else
      notes="$notes; compress step resumed from $(basename "$WK") (it ran under the bench lock before the lock-policy change), decompress unlocked"
    fi
    stored=0; for f in $OUTS; do [ -f "$f" ] && stored=$((stored + $(stat -c %s "$f"))); done
    [ "$codec" = repair-nav ] && { ln -f in.R out.R; ln -f in.C out.C; }  # Navarro's despair rebuilds <f> from <f>.R/.C
    if [ "$(cut -d' ' -f5 c.t)" = 0 ]; then run1 d "${D[@]}"; else echo "0 0 0 0 254 0" >d.t; echo "not run: compress failed" >d.log; fi
    if [ -f "$DEC" ] && cmp -s in "$DEC"; then ok=1; else ok=0; fi
    python3 -I - "$codec" "$ver" "$args" "$corpus" "$prefix" "$(stat -c %s in)" "$stored" "$ok" \
        "$(cat c.t)" "$(cat d.t)" "$MODE" "$notes" "$ROWS" <<'PY'
import json, sys, time
codec, ver, args, corpus, prefix, raw, stored, ok, ct, dt, mode, notes, rows = sys.argv[1:14]
def parse(s, tag):
    w, u, sy, rss, rc, load = s.split()
    log = open(tag + ".log", errors="replace").read().replace("\r", "\n")
    return dict(wall_ns=int(w), cpu_s=float(u) + float(sy), rss_kb=int(rss), rc=int(rc), load1=float(load),
                log_tail=log[-240:] if int(rc) != 0 else "")
row = dict(codec=codec, version_or_commit=ver, args=args, corpus=corpus, prefix_bytes=int(prefix),
           raw=int(raw), stored=int(stored), verified=ok == "1", timed=mode == "time",
           c=parse(ct, "c"), d=parse(dt, "d"), utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), notes=notes)
open(rows, "a").write(json.dumps(row) + "\n")
mb = row["raw"] / 1e6
sp = lambda x: f"{mb / (x['wall_ns'] / 1e9):.3f}" if x["wall_ns"] else "-"
print(f"[{mode}] {codec} {args!r} {corpus} prefix={prefix} stored={stored} ratio={int(stored)/int(raw):.4f} "
      f"ok={ok} c={sp(row['c'])}MB/s d={sp(row['d'])}MB/s rss={row['c']['rss_kb']//1024}/{row['d']['rss_kb']//1024}MiB", flush=True)
PY
  )
  rm -rf "$WK"   # temp copies and outputs are large; keep nothing
}

corpora() { echo "${CORPORA:-$ALL_CORPORA}"; }

suite() {
  case $MODE:$1 in
  time:repair|time:paq8px|time:cmix) echo "skip: $1 is unlocked-indicative only (lock policy)" >&2; return 0 ;;
  esac
  case $1 in
  bsc)
    V="libbsc 3.3.12 @4178579ff11d878863ea0d2758c125a0987d7b9b"
    N="bsc e/d; single-threaded build (no OpenMP/CUDA); -b<N> = block size in MB; default -m0 (BWT) -cf, LZP on; -e1 static QLFC unless -e2 (adaptive QLFC)"
    for c in $(corpora); do for a in -b16 -b64 -b16e2 -b64e2; do measure bsc "$V" "$a" "$c" 0 "$N"; done; done ;;
  openzl)
    V="openzl 0.3.0 @2d30fdebd9e43eb03d7bca3ef3e2f15732d8ab7a"
    N="zli compress -p serial (ACE graph with default ZL_GRAPH_LZ, 16 MiB serial segmenter), untrained; -l 6 is the default level; levels >=7 gave identical output on a 1 MiB probe; built without the optional xgboost ML-selector trainer"
    for c in $(corpora); do for a in "-l 6" "-l 7"; do measure openzl "$V" "$a" "$c" 0 "$N"; done; done ;;
  repair)
    for c in ${CORPORA:-hermes ohsft swe}; do
      measure repair-nav "Navarro repair.tgz sha256:1ad8798e9ba55c26fcef689395492dc13a769f9ba445e595cd9a7e3328b99640" "" "$c" $P8 \
        "first 8 MiB; stored = <f>.R (alphabet + 2 x int32 per rule) + <f>.C (int32 final sequence) exactly as the tool writes them; no entropy coding (its README: not a real compressor)"
      measure repair-wan "rwanwork/Re-Pair @7323bc9005db1a249a534f05bad49e8d45ca7daa" "-i" "$c" $P8 \
        "first 8 MiB; stored = <f>.prel (interpolative-coded phrase hierarchy) + <f>.seq (4-byte ints, no entropy coder) exactly as the tool writes them; one block (default max buffer)"
    done ;;
  zpaq)
    local methods="1 3 5"; [ "$MODE" = time ] && methods="1 3"   # -method 5 takes >60 s per 64 MiB: unlocked only
    V="zpaq 7.15 (zpaq715.zip sha256:e85ec2529eb0ba22ceaeabd461e55357ef099b80f61c14f377b429ea3d49d418)"
    N="zpaq a <archive> in -method N -threads 1 (fresh archive each run); includes zpaq fragment dedup + SHA-1; default block size (m1: 16 MiB, m3/m5: 64 MiB); decode = zpaq x -threads 1"
    for c in $(corpora); do for m in $methods; do measure zpaq "$V" "-method $m" "$c" 0 "$N"; done; done ;;
  paq8px)
    for c in ${CORPORA:-hermes ohsft swe}; do
      measure paq8px "paq8px v217 @c84f576fc2c522194cd320743708652a154daf6b" "-8" "$c" $P8 \
        "first 8 MiB; level -8, no LSTM; README memory for -8: 2433 MB"
    done ;;
  cmix)
    for c in ${CORPORA:-hermes swe}; do
      measure cmix "cmix v21 @194af9cd133b8b741d2a53afca13ed0ca453c276" "-c (no dictionary)" "$c" 1048576 \
        "first 1 MiB; under the mandated MemoryMax=3G scope"
    done ;;
  *) echo "unknown suite $1" >&2 ;;
  esac
}

aggregate() {
  python3 -I - "$ROWS" "$OUT" <<'PY'
import json, statistics as st, sys, collections
rows_path, out = sys.argv[1:3]
groups = collections.OrderedDict()
for line in open(rows_path):
    r = json.loads(line)
    groups.setdefault((r["codec"], r["args"], r["corpus"], r["prefix_bytes"]), []).append(r)
recs = []
for (codec, args, corpus, prefix), rs in groups.items():
    raw = rs[0]["raw"]; mb = raw / 1e6
    ok = all(r["verified"] and r["c"]["rc"] == 0 and r["d"]["rc"] == 0 for r in rs)
    sizes = sorted({r["stored"] for r in rs})
    tr = [r for r in rs if r["timed"]]
    locked = bool(tr)
    if not locked:   # slow codecs: one unlocked run, speeds are indicative only
        tr = [r for r in rs if r["verified"] and r["c"]["rc"] == 0 and r["d"]["rc"] == 0]
    def sp(d):
        v = [mb / (r[d]["wall_ns"] / 1e9) for r in tr if r[d]["wall_ns"] > 0]
        return (st.median(v), min(v), max(v)) if v else (None, None, None)
    def cpu(d):
        v = [mb / r[d]["cpu_s"] for r in tr if r[d]["cpu_s"] > 0]
        return st.median(v) if v else None
    c, d = sp("c"), sp("d")
    rec = dict(codec=codec, version_or_commit=rs[0]["version_or_commit"], args=args, corpus=corpus,
        prefix_bytes=prefix, raw=raw, stored=sizes[0] if ok else None, ratio=sizes[0] / raw if ok else None,
        c_mbps_median=c[0], c_mbps_min=c[1], c_mbps_max=c[2],
        d_mbps_median=d[0], d_mbps_min=d[1], d_mbps_max=d[2],
        peak_rss_mib=round(max(max(r["c"]["rss_kb"], r["d"]["rss_kb"]) for r in rs) / 1024, 1),
        c_peak_rss_mib=round(max(r["c"]["rss_kb"] for r in rs) / 1024, 1),
        d_peak_rss_mib=round(max(r["d"]["rss_kb"] for r in rs) / 1024, 1),
        roundtrip=ok, status="ok" if ok else "failed", deterministic=len(sizes) == 1,
        timed_runs=len(tr) if locked else 0, indicative_runs=0 if locked else len(tr),
        verified_runs=sum(r["verified"] for r in rs),
        c_cpu_mbps_median=cpu("c"), d_cpu_mbps_median=cpu("d"),
        loadavg_1m=[x for r in tr for x in (r["c"]["load1"], r["d"]["load1"])],
        threads=1,
        timing=("locked" if locked else "unlocked-indicative") if tr else None,
        timing_detail=("median/min/max over the locked runs: wall clock per codec process, each invocation under flock "
                       "$OOJURO_BENCH_LOCK in a systemd scope MemoryMax=3G" if locked else
                       "single unlocked run (not under the bench lock), shared machine; indicative only") + "; MB = 1e6 bytes",
        notes=rs[0]["notes"])
    if not ok:
        bad = [r for r in rs if not (r["verified"] and r["c"]["rc"] == 0 and r["d"]["rc"] == 0)][0]
        rec["failure"] = dict(c_rc=bad["c"]["rc"], d_rc=bad["d"]["rc"], c_peak_rss_mib=round(bad["c"]["rss_kb"] / 1024, 1),
                              c_log_tail=bad["c"]["log_tail"], d_log_tail=bad["d"]["log_tail"])
    recs.append(rec)
with open(out, "w") as f:
    for rec in recs: f.write(json.dumps(rec) + "\n")
print(f"wrote {len(recs)} records to {out}")
PY
}

MODE=${1:-}; shift || true
case $MODE in
  verify|time) for s in "$@"; do suite "$s"; done ;;
  aggregate) aggregate ;;
  *) sed -n 2,12p "$0"; exit 2 ;;
esac
