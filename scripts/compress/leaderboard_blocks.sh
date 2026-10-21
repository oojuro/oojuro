#!/bin/bash
# Random-access codec leaderboard: records packed in append order into
# independent blocks of 4 KiB-256 KiB, cold excerpt fetches of 2 KiB for
# 200 queries x 20 records. Pass 1: every codec without a dictionary.
# Pass 2: zstd and LZ4 with a 112 KiB trained dictionary (LZ4 uses its last
# 64 KiB). 64 MiB samples, 3 reps, one bench-lock hold per invocation.
set -uo pipefail
LOCKFILE=${OOJURO_BENCH_LOCK:-$HOME/.cache/oojuro-bench.lock}; mkdir -p "$(dirname "$LOCKFILE")"
cd "$(dirname "$0")/../.."
# Local defaults: a 3 GiB scope and the shared bench lock. On a box that runs one
# job at a time (no user systemd), run with SCOPE= LOCK=.
SCOPE=${SCOPE-systemd-run --user --scope -q -p MemoryMax=3G --}
LOCK=${LOCK-flock $LOCKFILE}
BIN=target/release/codecs
OUT=${OUT:-.scratch/out/blocks.jsonl}
ALL="zstd:1 zstd:3 zstd:9 zstd:19 lz4 lz4hc:12 snappy deflate:6 deflate:12 brotli:5:22 brotli:9:22 brotli:11:24 xz:6 ppmd8:6:16 fsst"
DICTED="zstd:1 zstd:3 zstd:9 zstd:19 lz4"
for rep in 1 2 3; do
  for c in swe ohjson; do
    for b in 4096 16384 65536 262144; do
      QUERIES=200 $LOCK $SCOPE \
        $BIN blocks ".scratch/data/s64/$c.rec" 0 $b 0 $ALL | sed "s/^{/{\"rep\":$rep,/" >> "$OUT"
      QUERIES=200 $LOCK $SCOPE \
        $BIN blocks ".scratch/data/s64/$c.rec" 0 $b 114688 $DICTED | sed "s/^{/{\"rep\":$rep,/" >> "$OUT"
    done
  done
done
