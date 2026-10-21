#!/bin/bash
# Matched system baselines on the same corpora (sizes; load times are not scorecard metrics).
# Nine (current engine journal) keeps every original and posting in RAM, so it
# runs on a 32 MiB append-order prefix; our store is measured on the same prefix.
set -uo pipefail
cd "$(dirname "$0")/../.."
B=target/release/baselines
S=.scratch/stores/baselines; mkdir -p $S
PG=${PG:-127.0.0.1:55871}
P32=$((32*1024*1024))
run() { systemd-run --user --scope -q -p MemoryMax=3G -- "$@"; }
for c in ${CORPORA:-ohsft hermes swe ohjson othink fiqa}; do
  R=.scratch/data/corpus/$c.rec
  run $B nine $R $S/nine-$c $P32
  run $B nine-read $S/nine-$c | sed "s/^{/{\"corpus\":\"$c\",/"
  rm -rf $S/nine-$c
  run $B sqlite $R $S/$c.sqlite 0; rm -f $S/$c.sqlite*
  for t in lz4 zstd:19; do run $B tantivy $R $S/tv-$c 0 $t store; done
  run $B tantivy $R $S/tv-$c 0 zstd:19 index
  run $B tantivy $R $S/tv-$c 0 zstd:19 index-only
  rm -rf $S/tv-$c
  for t in pglz lz4; do run $B postgres $R $PG 0 $t; done
done
