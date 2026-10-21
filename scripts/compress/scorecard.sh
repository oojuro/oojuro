#!/bin/bash
# Store scorecard: full corpora, 3 reps. Each timed phase takes the shared
# bench lock separately. Output: JSON lines (build + read per rep).
# Bench stores live under .scratch/stores/score/<corpus> and are removed after
# each rep (they are rebuilt from .scratch/data/corpus/<corpus>.rec).
set -uo pipefail
LOCK=${OOJURO_BENCH_LOCK:-$HOME/.cache/oojuro-bench.lock}; mkdir -p "$(dirname "$LOCK")"
cd "$(dirname "$0")/../.."
B=target/release/store-bench
DATA=${DATA:-.scratch/data/corpus}
D=.scratch/stores/score-${LABEL:-default}
mkdir -p "$D"
export SYNC_BATCHES=${SYNC_BATCHES:-2000} COMPACT_EVERY=${COMPACT_EVERY:-67108864}
for rep in ${REPS:-1 2 3}; do
  for c in ${CORPORA:-swe ohjson othink hermes ohsft fiqa}; do
    tag="{\"rep\":$rep,\"label\":\"${LABEL:-default}\","
    flock "$LOCK" systemd-run --user --scope -q -p MemoryMax=3G -- \
      $B build "$DATA/$c.rec" "$D/$c" | sed "s/^{/$tag/"
    flock "$LOCK" systemd-run --user --scope -q -p MemoryMax=3G -- \
      $B read "$D/$c" 1000 | sed "s/^{/$tag\"corpus\":\"$c\",/"
    rm -rf -- "${D:?}/$c"
  done
done
