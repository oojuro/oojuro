#!/usr/bin/env bash
# Final timed rerun for docs/PERF_SEARCH.md, on a quiet machine, from the
# repository root at the v1-perfsearch commit. Compares the reference (the
# merged v1 engine plus the probe, scripts/perfsearch/reference-probe.patch)
# with this branch: exactness on all tier-1 turn-loop queries, then the
# timed probe (tier 1 per agent and a 1.7M-row single store, query sizes
# 200 B to 256 KB, alternating arms, ROUNDS rounds) and the turnloop suite,
# all under the bench lock and a 3 GiB memory scope.
#
#   DATA=$TRACEDB_DATA ROUNDS=3 scripts/perfsearch/rerun.sh
#
# Optional: QTHREADS="1 4 8" adds query-thread arms for the final engine.
set -euo pipefail
DATA=${DATA:-${TRACEDB_DATA:-$HOME/.cache/oojuro-tracedb-data}}
W=${W:-.scratch/perfsearch}
REF=${REF:-v1}
ROUNDS=${ROUNDS:-3}
QTHREADS=${QTHREADS:-1}
LOCK=${LOCK:-${OOJURO_BENCH_LOCK:-$HOME/.cache/oojuro-bench.lock}}
SCOPE=(systemd-run --user --scope -q -p MemoryMax=3G -p MemorySwapMax=0 --)
mkdir -p "$W/bin" "$W/stores" "$W/results" "$W/dumps"
OUT="$W/results/probe.jsonl"

# 1. Builds (untimed).
cargo build --release -p oojuro-tracebench
cp target/release/searchprobe "$W/bin/final"
cp target/release/tracebench "$W/bin/tracebench-final"
if [ ! -x "$W/bin/ref" ]; then
  rm -rf "$W/ref" && mkdir -p "$W/ref"
  git archive "$REF" | tar -x -C "$W/ref"
  (cd "$W/ref" && patch -p1 < "$OLDPWD/scripts/perfsearch/reference-probe.patch" && cargo build --release -p oojuro-tracebench)
  cp "$W/ref/target/release/searchprobe" "$W/bin/ref"
  cp "$W/ref/target/release/tracebench" "$W/bin/tracebench-ref"
fi

# 2. Stores (untimed): tier 1 per agent (the turnloop split), and every agent
# of tier 2 in one store, first 1.7M rows, record dedupe off (dedupe would
# fold the replica back to the distinct rows).
[ -d "$W/stores/t1" ] || "${SCOPE[@]}" "$W/bin/final" build --data "$DATA/base1m.tbe" --frac 0.1 --dir "$W/stores/t1"
[ -d "$W/stores/single" ] || "${SCOPE[@]}" "$W/bin/final" build --data "$DATA/base1m.tbe" --tier 2 --single --no-dedupe --records 1700000 --dir "$W/stores/single"

# 3. Exactness (untimed): top-20 ids and score bits on all 30,905 tier-1
# turn-loop queries; with --entropy all the routing entropy bits too.
"$W/bin/ref" run --dir "$W/stores/t1" --size 0 --dump "$W/dumps/ref.txt" > /dev/null
"$W/bin/final" run --dir "$W/stores/t1" --size 0 --dump "$W/dumps/final.txt" > /dev/null
"$W/bin/final" run --dir "$W/stores/t1" --size 0 --entropy all --dump "$W/dumps/final-all.txt" > /dev/null
"$W/bin/final" compare "$W/dumps/ref.txt" "$W/dumps/final-all.txt"
"$W/bin/final" compare "$W/dumps/ref.txt" "$W/dumps/final.txt" || true # entropy differs by design (TopK)

# 4. Timed probe, arms alternating within each round, under the lock.
probe() { # arm store size queries [args...]
  local arm=$1 store=$2 size=$3 nq=$4; shift 4
  flock "$LOCK" "${SCOPE[@]}" "$W/bin/$arm" run --dir "$W/stores/$store" --size "$size" --queries "$nq" \
    --label "$arm-$store" --out "$OUT" "$@" > /dev/null
}
for r in $(seq 1 "$ROUNDS"); do
  for size in 0 200 1024 4096 16384 65536 262144; do
    nq=1000; [ "$size" -ge 4096 ] && nq=200; [ "$size" -ge 65536 ] && nq=100
    probe ref t1 "$size" "$nq"
    for q in $QTHREADS; do probe final t1 "$size" "$nq" --qthreads "$q"; done
    nq=200; [ "$size" -ge 4096 ] && nq=50; [ "$size" -ge 65536 ] && nq=30
    probe ref single "$size" "$nq"
    for q in $QTHREADS; do probe final single "$size" "$nq" --qthreads "$q"; done
  done
done

# 5. The turnloop suite (16 agents appending and searching), both engines.
for r in $(seq 1 "$ROUNDS"); do
  for arm in ref final; do
    flock "$LOCK" "${SCOPE[@]}" "$W/bin/tracebench-$arm" turnloop --arm oojuro --data "$DATA/base1m.tbe" \
      --run "$r" --work "$W/work" --out "$W/results/turnloop.jsonl"
  done
done
echo "results: $OUT, $W/results/turnloop.jsonl"
