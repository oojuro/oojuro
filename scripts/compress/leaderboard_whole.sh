#!/bin/bash
# Whole-sample codec leaderboard on the 64 MiB append-order sample of each corpus.
# Pass 1 (rep 1): every codec x corpus -> ratio (deterministic) + one speed.
# Pass 2 (reps 2-3): a fixed speed panel on swe and othink, so panel speeds are
# medians of 3; other speeds are single runs and are flagged in the report.
# Resumable: (corpus, codec, rep) already present in $OUT are skipped. Each
# invocation takes the shared bench lock separately so other sessions interleave.
set -uo pipefail
LOCKFILE=${OOJURO_BENCH_LOCK:-$HOME/.cache/oojuro-bench.lock}; mkdir -p "$(dirname "$LOCKFILE")"
cd "$(dirname "$0")/../.."
# Local defaults: a 3 GiB scope and the shared bench lock. On a box that runs one
# job at a time (no user systemd), run with SCOPE= LOCK=.
SCOPE=${SCOPE-systemd-run --user --scope -q -p MemoryMax=3G --}
LOCK=${LOCK-flock $LOCKFILE}
BIN=target/release/codecs
OUT=${OUT:-.scratch/out/whole.jsonl}
SAMPLE=67108864
ALL="zstd:-5 zstd:1 zstd:3 zstd:6 zstd:9 zstd:12 zstd:15 zstd:19 zstd:22 zstd:19:w27:ldm zstd:22:w27:ldm
lz4 lz4hc:9 lz4hc:12 snappy deflate:1 deflate:6 deflate:12 brotli:1:24 brotli:5:24 brotli:9:24 brotli:11:24
brotli:9:30 brotli:11:30 xz:6 xz:9 xz:9e bzip2:9 ppmd7:6:256 ppmd7:8:256 ppmd8:6:256 ppmd8:8:256
ppmd8:12:1024 ppmd8:16:1024 tok:zstd:19 tok:xz:9e tok:ppmd8:8:256"
PANEL="zstd:1 zstd:3 zstd:9 zstd:19 zstd:19:w27:ldm lz4 lz4hc:12 snappy deflate:6 brotli:9:24 brotli:11:24 xz:9e bzip2:9 ppmd8:8:256"
touch "$OUT"
done_already() { # rep corpus codec
  python3 -c "
import json,sys
for l in open('$OUT'):
    d=json.loads(l)
    if d['rep']==$1 and d['corpus']=='$2' and d['codec']=='$3': sys.exit(0)
sys.exit(1)"
}
run() { # rep corpus codec
  done_already "$1" "$2" "$3" && return
  $LOCK $SCOPE \
    $BIN whole ".scratch/data/corpus/$2.rec" $SAMPLE "$3" | sed "s/^{/{\"rep\":$1,/" >> "$OUT"
}
for c in swe ohjson othink hermes ohsft fiqa; do
  for k in $ALL; do run 1 $c "$k"; done
done
for rep in 2 3; do
  for c in swe othink; do
    for k in $PANEL; do run $rep $c "$k"; done
  done
done
