#!/bin/bash
# Archive-tier search over compressed frames vs a record-level index
# (unlabelled corpora; the labelled swe turn loop runs through oojuro-bench).
# Two groups per corpus so top-20 overlap is always against the matching
# record-level arm: full queries, and queries reduced to the 32 highest-idf
# terms. Only the query loops take the shared bench lock (BENCH_LOCK).
set -uo pipefail
cd "$(dirname "$0")/../.."
# Local defaults: a 3 GiB scope and the shared bench lock. On the bench box
# (one job at a time, no user systemd) run with SCOPE= LOCK=.
SCOPE=${SCOPE-systemd-run --user --scope -q -p MemoryMax=3G --}
LOCK=${LOCK-flock $HOME/.sa-agents/oojuro-bench.lock}
BIN=target/release/archive-search
OUT=${OUT:-.scratch/out/archive-search.jsonl}
W=.scratch/search; mkdir -p "$W"
C="cache=67108864"
# Timed runs go to the bench box; locally the lock is not taken (BENCH_LOCK unset).
for c in ${CORPORA:-swe ohjson hermes othink}; do
  $SCOPE $BIN .scratch/data/s64/$c.rec "$W" ${QUERIES:-200} \
    "archive-rec?$C" "archive-coarse?$C" "archive-coarse?nobound=1&$C" \
    | sed 's/^{/{"group":"full",/' >> "$OUT"
  $SCOPE $BIN .scratch/data/s64/$c.rec "$W" ${QUERIES:-200} \
    "archive-rec?qterms=32&$C" "archive-coarse?qterms=32&$C" "archive-coarse?qterms=32&max_frames=16&$C" \
    "archive-coarse?qterms=32&nobound=1&$C" "archive-coarse?qterms=32&frame=16384&$C" \
    "archive-coarse?qterms=32&frame=262144&$C" "archive-coarse?qterms=32&cache=4194304" \
    | sed 's/^{/{"group":"q32",/' >> "$OUT"
done
