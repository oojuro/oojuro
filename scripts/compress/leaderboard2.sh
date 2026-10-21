#!/bin/bash
# Leaderboard v2 (2026-10-21): every in-process codec on all six public
# 64 MiB samples plus a 64 MiB sample of the owner's journal, whole-sample and
# random-access blocks (per record, 4/16/64/256 KiB, with and without a
# trained dictionary), plus Oojuro's own run format (flush, default, archive
# profiles). Sizes are deterministic and run unlocked; speeds and excerpt
# latencies come from separate timed passes, each invocation under the shared
# bench lock (short holds: one codec, or one (corpus, block size) group on a
# 16 MiB prefix).
#
#   scripts/compress/leaderboard2.sh ratio-whole|ratio-blocks|ratio-run|time-whole|time-blocks|time-run [corpus...]
#
# Output: JSON lines in $OUT_DIR (default .scratch/out): whole2.jsonl,
# blocks2.jsonl, run2.jsonl; each row carries "pass" (ratio|time). Resumable:
# a (pass, corpus, block, dict, codec) already present is skipped.
set -uo pipefail
cd "$(dirname "$0")/../.."
BIN=${BIN:-target/release/codecs}
OUT_DIR=${OUT_DIR:-.scratch/out}
PUB=${PUB:-$HOME/.sa-agents/oojuro-compress/.scratch/data/s64}
OWNER=${OWNER:-.scratch/owner/owner.rec}
SCOPE=(systemd-run --user --scope -q -p MemoryMax=3G --)
LOCK=(flock "$HOME/.sa-agents/oojuro-bench.lock")
NICE=(nice -n 19)
S64=67108864
S16=16777216
mkdir -p "$OUT_DIR" .scratch/runtmp
rec() { [ "$1" = owner ] && echo "$OWNER" || echo "$PUB/$1.rec"; }
ALL_CORPORA="swe ohjson othink hermes ohsft fiqa owner"

WHOLE="zstd:-5 zstd:1 zstd:3 zstd:6 zstd:9 zstd:12 zstd:15 zstd:19 zstd:22 zstd:19:w27:ldm zstd:22:w27:ldm
lz4 lz4hc:9 lz4hc:12 snappy deflate:1 deflate:6 deflate:12 brotli:1:24 brotli:5:24 brotli:9:24 brotli:11:24
brotli:9:30 brotli:11:30 xz:6 xz:9 xz:9e bzip2:9 ppmd7:6:256 ppmd7:8:256 ppmd8:6:256 ppmd8:8:256
ppmd8:12:1024 ppmd8:16:1024 tok:zstd:19 tok:xz:9e tok:ppmd8:8:256"
WHOLE_PANEL="zstd:1 zstd:3 zstd:9 zstd:19 zstd:22 zstd:19:w27:ldm lz4 lz4hc:12 snappy deflate:6 deflate:12 brotli:9:24 brotli:11:24 xz:9e bzip2:9 ppmd7:8:256 ppmd8:8:256 ppmd8:16:1024"
BLOCK_SIZES="4096 16384 65536 262144"
NODICT="zstd:1 zstd:3 zstd:9 zstd:19 zstd:22 lz4 lz4hc:12 snappy deflate:6 deflate:12 brotli:5:22 brotli:9:22 brotli:11:24 xz:6 bzip2:9 ppmd7:6:16 ppmd8:6:16 fsst"
DICTED="zstd:1 zstd:3 zstd:9 zstd:19 zstd:22 lz4 brotli:5:22 brotli:9:22 brotli:11:24"
# per record (block_bytes 0): FSST's natural mode, against dictionary codecs
REC_NODICT="fsst zstd:3 lz4"
REC_DICTED="zstd:3 zstd:19 lz4 brotli:5:22 brotli:11:24"
DICT_BYTES=114688
# cold archive tier, in-process coders at archive block sizes (cold_tier.sh runs bsc / zpaq)
COLD_SIZES="1048576 4194304 16777216"
COLD="zstd:19:w24 zstd:22:w27 xz:9 brotli:11:24 ppmd7:8:256 ppmd8:8:256"

done_keys() { # file pass -> "corpus|block|dictflag|codec" lines already present
  [ -f "$1" ] || return 0
  python3 -I -c '
import json, sys
for l in open(sys.argv[1]):
    d = json.loads(l)
    if d.get("pass") != sys.argv[2]: continue
    print("|".join(str(x) for x in (d["corpus"], d.get("block_bytes", "-"), int(d.get("dict_req", 0) > 0), d["codec"])))' "$1" "$2"
}
todo() { # file pass corpus block dictflag codecs... -> codecs not yet done
  local f=$1 p=$2 c=$3 b=$4 dflag=$5; shift 5
  local have; have=$(done_keys "$f" "$p")
  for k in "$@"; do grep -qxF "$c|$b|$dflag|$k" <<<"$have" || echo "$k"; done
}
tag() { sed -u "s/^{/{\"pass\":\"$1\",\"dict_req\":$2,/"; }

ratio_whole() {
  local f=$OUT_DIR/whole2.jsonl
  for c in ${@:-$ALL_CORPORA}; do
    for k in $(todo "$f" ratio "$c" - 0 $WHOLE); do
      "${NICE[@]}" "${SCOPE[@]}" "$BIN" whole "$(rec "$c")" $S64 "$k" | tag ratio 0 >> "$f"
    done
  done
}
time_whole() {
  local f=$OUT_DIR/whole2.jsonl
  for c in ${@:-othink owner}; do
    for k in $(todo "$f" time "$c" - 0 $WHOLE_PANEL); do
      "${LOCK[@]}" "${SCOPE[@]}" "$BIN" whole "$(rec "$c")" $S64 "$k" | tag time 0 >> "$f"
    done
  done
}
blocks_group() { # pass corpus block dict sample queries codecs...
  local p=$1 c=$2 b=$3 d=$4 s=$5 q=$6; shift 6
  local f=$OUT_DIR/blocks2.jsonl dflag=0; [ "$d" -gt 0 ] && dflag=1
  local list; list=$(todo "$f" "$p" "$c" "$b" "$dflag" "$@" | tr '\n' ' ')
  [ -n "${list// }" ] || return 0
  local pre=("${NICE[@]}"); [ "$p" = time ] && pre=("${LOCK[@]}")
  QUERIES=$q EXCERPT=1024 "${pre[@]}" "${SCOPE[@]}" "$BIN" blocks "$(rec "$c")" "$s" "$b" "$d" $list | tag "$p" "$d" >> "$f"
}
ratio_blocks() {
  for c in ${@:-$ALL_CORPORA}; do
    for b in $BLOCK_SIZES; do
      blocks_group ratio "$c" "$b" 0 $S64 20 $NODICT
      blocks_group ratio "$c" "$b" $DICT_BYTES $S64 20 $DICTED
    done
    blocks_group ratio "$c" 0 0 $S64 20 $REC_NODICT
    blocks_group ratio "$c" 0 $DICT_BYTES $S64 20 $REC_DICTED
  done
}
time_blocks() {
  for c in ${@:-swe othink owner}; do
    for b in $BLOCK_SIZES; do
      blocks_group time "$c" "$b" 0 $S16 200 $NODICT
      blocks_group time "$c" "$b" $DICT_BYTES $S16 200 $DICTED
    done
    blocks_group time "$c" 0 0 $S16 200 $REC_NODICT
    blocks_group time "$c" 0 $DICT_BYTES $S16 200 $REC_DICTED
  done
}
ratio_cold() {
  for c in ${@:-$ALL_CORPORA}; do for b in $COLD_SIZES; do blocks_group ratio "$c" "$b" 0 $S64 1 $COLD; done; done
}
time_cold() { # one lock hold per (corpus, size, codec); 64 MiB so 16 MiB blocks are not a single block
  for c in ${@:-swe othink owner}; do for b in $COLD_SIZES; do for k in $COLD; do blocks_group time "$c" "$b" 0 $S64 1 "$k"; done; done; done
}
# thinking text alone: records grouped by kind (GROUP=kind), per-kind ratios
ratio_think() {
  for c in ${@:-othink swe owner}; do for b in 262144 $COLD_SIZES; do
    local f=$OUT_DIR/think2.jsonl
    [ -f "$f" ] && python3 -I -c 'import json,sys; sys.exit(0 if any((d["corpus"],str(d["block_bytes"]))==(sys.argv[2],sys.argv[3]) for d in map(json.loads,open(sys.argv[1]))) else 1)' "$f" "$c" "$b" && continue
    GROUP=kind QUERIES=1 "${NICE[@]}" "${SCOPE[@]}" "$BIN" blocks "$(rec "$c")" $S64 "$b" 0 zstd:19:w24 xz:9 ppmd8:8:256 | tag ratio 0 >> "$f"
  done; done
}
# Oojuro run profiles: flush = what a small store holds (zstd 3, starter
# dictionary); default = merged/archive-tier runs today (zstd 19, trained
# dictionary <= 512 KiB at ~1/256 of the data, 64 KiB frames, 1 KiB CDC);
# archive = the track's archive profile without P-frames (not in the run
# format; E6/E11): 256 KiB frames, 512 B chunks, 512 KiB dictionary.
run_profile() { # pass corpus profile
  local p=$1 c=$2 prof=$3 f=$OUT_DIR/run2.jsonl
  [ -n "$(todo "$f" "$p" "$c" - 0 "oojuro-run:$prof")" ] || return 0
  local env=()
  case $prof in
    flush)   env=(LEVEL=3 DICT=0 STARTER=1) ;;
    default) env=(LEVEL=19) ;;
    archive) env=(LEVEL=19 FRAME=262144 CHUNK=512 DICT=524288 DICT_SCALE=128) ;;
    # cold tier with no new codec: the same run format with 4 / 16 MiB frames
    cold4)   env=(LEVEL=19 FRAME=4194304) ;;
    cold16)  env=(LEVEL=19 FRAME=16777216) ;;
  esac
  local pre=("${NICE[@]}") q=20; [ "$p" = time ] && { pre=("${LOCK[@]}"); q=400; }
  env "${env[@]}" QUERIES=$q EXCERPT=1024 THREADS=1 "${pre[@]}" "${SCOPE[@]}" "$BIN" run "$(rec "$c")" $S64 "$prof" .scratch/runtmp | tag "$p" 0 >> "$f"
}
RUN_PROFILES=${RUN_PROFILES:-flush default archive cold4 cold16}
ratio_run() { for c in ${@:-$ALL_CORPORA}; do for prof in $RUN_PROFILES; do run_profile ratio "$c" $prof; done; done; }
time_run() { for c in ${@:-swe othink owner}; do for prof in $RUN_PROFILES; do run_profile time "$c" $prof; done; done; }

cmd=${1:-}; shift || true
case $cmd in
  ratio-whole) ratio_whole "$@" ;;
  time-whole) time_whole "$@" ;;
  ratio-blocks) ratio_blocks "$@" ;;
  time-blocks) time_blocks "$@" ;;
  ratio-run) ratio_run "$@" ;;
  time-run) time_run "$@" ;;
  ratio-cold) ratio_cold "$@" ;;
  time-cold) time_cold "$@" ;;
  ratio-think) ratio_think "$@" ;;
  *) sed -n 2,16p "$0"; exit 2 ;;
esac
