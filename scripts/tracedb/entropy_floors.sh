#!/usr/bin/env bash
# Whole-stream compressor floors for the TraceBench tier-1 text (no random access).
# Usage: entropy_floors.sh <floors_dir>   (after entropy.py wrote text.append/text.session)
# Appends one JSON line per (order, codec) to <floors_dir>/floors.jsonl.
set -u
d=$1; ext=${EXT:-$HOME/.cache/oojuro-ext}/bin
cd "$d"
out=floors.jsonl
# run <order> <codec> <input> <cmd...>: the command must write its output to cmp.tmp
run() {
  local order=$1 codec=$2 in=$3; shift 3
  rm -f cmp.tmp
  local t0=$(date +%s.%N); "$@" >/dev/null 2>&1; local t1=$(date +%s.%N)
  local i=$(stat -c %s "$in") o=$(stat -c %s cmp.tmp 2>/dev/null || echo 0)
  echo "{\"order\":\"$order\",\"codec\":\"$codec\",\"input\":$i,\"output\":$o,\"ratio\":$(python3 -c "print(round($o/$i,5))"),\"seconds\":$(python3 -c "print(round($t1-$t0,1))")}" | tee -a $out
}
for o in append session; do
  run $o zstd-3 text.$o sh -c "zstd -3 -T1 -c text.$o > cmp.tmp"
  run $o zstd-19-long27 text.$o sh -c "zstd -19 --long=27 -T4 -c text.$o > cmp.tmp"
  run $o zstd-22-long31 text.$o sh -c "zstd --ultra -22 --long=31 -T4 -c text.$o > cmp.tmp"
  run $o bsc-b1000 text.$o $ext/bsc e text.$o cmp.tmp -b1000
done
# slower coders on prefixes (whole-stream runs would take hours)
for o in append session; do
  head -c 134217728 text.$o > p128.$o; head -c 8388608 text.$o > p8.$o
  run $o xz-9e-p128MiB p128.$o sh -c "xz -9e -T1 -c p128.$o > cmp.tmp"
  run $o zstd-19-long27-p128MiB p128.$o sh -c "zstd -19 --long=27 -T1 -c p128.$o > cmp.tmp"
  run $o bsc-p128MiB p128.$o $ext/bsc e p128.$o cmp.tmp -b1000
  run $o zstd-19-p8MiB p8.$o sh -c "zstd -19 -T1 -c p8.$o > cmp.tmp"
  run $o xz-9e-p8MiB p8.$o sh -c "xz -9e -T1 -c p8.$o > cmp.tmp"
  run $o paq8px-5-p8MiB p8.$o $ext/paq8px -5 p8.$o cmp.tmp
done
rm -f cmp.tmp
