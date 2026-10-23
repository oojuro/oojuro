#!/bin/bash
# Store scorecard: full corpora, 3 reps. Each timed phase takes the shared
# bench lock separately. Output: JSON lines (build + read per rep).
# Bench stores live under .scratch/stores/score/<corpus> and are removed after
# each rep (they are rebuilt from .scratch/data/corpus/<corpus>.rec).
# Build first: cargo build --release -p oojuro-store --features bench-tools --bin store-bench
set -uo pipefail
LOCKFILE=${OOJURO_BENCH_LOCK:-$HOME/.cache/oojuro-bench.lock}; mkdir -p "$(dirname "$LOCKFILE")"
cd "$(dirname "$0")/../.."
# Local defaults: a 3 GiB scope and the shared bench lock. On a box that runs one
# job at a time (no user systemd), run with SCOPE= LOCK=.
SCOPE=${SCOPE-systemd-run --user --scope -q -p MemoryMax=3G --}
LOCK=${LOCK-flock $LOCKFILE}
B=target/release/store-bench
DATA=${DATA:-.scratch/data/corpus}
D=.scratch/stores/score-${LABEL:-default}
mkdir -p "$D"
export SYNC_BATCHES=${SYNC_BATCHES:-2000} COMPACT_EVERY=${COMPACT_EVERY:-67108864}
for rep in ${REPS:-1 2 3}; do
  for c in ${CORPORA:-swe ohjson othink hermes ohsft fiqa}; do
    tag="{\"rep\":$rep,\"label\":\"${LABEL:-default}\","
    $LOCK $SCOPE \
      $B build "$DATA/$c.rec" "$D/$c" | sed "s/^{/$tag/"
    $LOCK $SCOPE \
      $B read "$D/$c" 1000 | sed "s/^{/$tag\"corpus\":\"$c\",/"
    rm -rf -- "${D:?}/$c"
  done
done
