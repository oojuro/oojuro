#!/bin/bash
# Cold archive tier candidates at archive block sizes (1, 4, 16 MiB): each
# block is an independent stream, so reading one excerpt decodes one block.
# External block coders run per block: libbsc (BWT; -e1 and -e2) and zpaq
# -method 3 / 5 (context mixing). In-process coders (PPMd, xz, zstd, brotli)
# use `codecs blocks` at the same block sizes (see leaderboard2.sh).
#
#   nice -n 19 systemd-run --user --scope -p MemoryMax=3G -- scripts/compress/cold_tier.sh size [corpus...]   # sizes + roundtrip, unlocked
#   scripts/compress/cold_tier.sh time [corpus...]   # one block per (codec, size) encoded and decoded under the bench lock
#
# Input: the first 64 MiB of each sample's raw text (s64/<c>.txt; owner:
# .scratch/owner/owner.txt), cut into blocks with `split -b`. Output: JSON
# lines in .scratch/out/cold-ext.jsonl: stored bytes (sum of every block's
# output file; zpaq archives include their own small headers), CPU seconds
# (user+sys, /usr/bin/time) and wall time per block. zpaq -method 5 decodes
# only the first block of each (corpus, size) in the size pass (it runs at
# ~0.2 MB/s each way); every other configuration decodes and compares every
# block.
set -uo pipefail
cd "$(dirname "$0")/../.."
EXT=${EXT:-$HOME/.sa-agents/oojuro-compress/.scratch/ext/bin}
PUB=${PUB:-$HOME/.sa-agents/oojuro-compress/.scratch/data/s64}
W=${W:-.scratch/cold}
OUT=${OUT:-.scratch/out/cold-ext.jsonl}
LOCKFILE=$HOME/.sa-agents/oojuro-bench.lock
SIZES=${SIZES:-1 4 16}
CONFIGS=${CONFIGS:-bsc:e1 bsc:e2 zpaq:3 zpaq:5}
mkdir -p "$W" "$(dirname "$OUT")"
txt() { [ "$1" = owner ] && echo .scratch/owner/owner.txt || echo "$PUB/$1.txt"; }

# t <outfile> <cmd...>: run, write "wall_s cpu_s maxrss_kb rc"
t() {
  local o=$1; shift
  local s e
  s=$(date +%s%N)
  /usr/bin/time -f "%U %S %M" -o "$o.time" bash -c '"$@"' _ "$@" >/dev/null 2>"$o.err"; local rc=$?
  e=$(date +%s%N)
  read -r u sy m < <(tail -n1 "$o.time")
  echo "$(( (e - s) / 1000 )) $u $sy $m $rc" > "$o"
}
enc() { # cfg in out
  case $1 in
    bsc:*) "$EXT/bsc" e "$2" "$3" "-b$MB${1#bsc:}" ;;
    zpaq:*) rm -f "$3"; local o; o=$(realpath -m "$3"); (cd "$(dirname "$2")" && "$EXT/zpaq" a "$o" "$(basename "$2")" -method "${1#zpaq:}" -threads 1) ;;
  esac
}
dec() { # cfg in out
  case $1 in
    bsc:*) "$EXT/bsc" d "$2" "$3" ;;
    zpaq:*) rm -rf "$3.d"; "$EXT/zpaq" x "$2" -to "$3.d" -threads 1 && find "$3.d" -type f -exec mv {} "$3" \; ; rm -rf "$3.d" ;;
  esac
}
export -f enc dec
export EXT
# locked <cmd...>: run under the bench lock in time mode (one hold per block op)
locked() { if [ "$MODE" = time ]; then ( flock 9; "$@" ) 9>"$LOCKFILE"; else "$@"; fi; }

chunks() { # corpus size -> directory of blocks
  local d=$W/$1/$2
  if [ ! -f "$d/.done" ]; then
    rm -rf "$d"; mkdir -p "$d"
    head -c 67108864 "$(txt "$1")" | split -b "$(( $2 << 20 ))" -d -a 3 - "$d/b"
    touch "$d/.done"
  fi
  echo "$d"
}

emit() { python3 -I -c '
import json, sys
c, cfg, mb, nblk, raw, stored, ce, de, cpu_c, cpu_d, wall_c, wall_d, verified, mode, rss = sys.argv[1:16]
f = lambda x: float(x)
print(json.dumps(dict(mode=mode, corpus=c, codec=cfg, block_mib=int(mb), blocks=int(nblk), raw=int(raw), stored=int(stored),
    ratio=int(stored) / int(raw), blocks_encoded=int(ce), blocks_decoded=int(de), c_cpu_s=f(cpu_c), d_cpu_s=f(cpu_d),
    c_wall_s=f(wall_c), d_wall_s=f(wall_d), verified=verified == "1", peak_rss_kb=int(rss))))' "$@"; }

measure() { # mode corpus cfg size
  local mode=$1 c=$2 cfg=$3 mb=$4
  local d; d=$(chunks "$c" "$mb")
  local wk=$W/work/$c-$mb-${cfg/:/-}; rm -rf "$wk"; mkdir -p "$wk"
  local raw=0 stored=0 ne=0 nd=0 cc=0 cd=0 wc=0 wd=0 ok=1 rss=0 i=0
  for b in "$d"/b*; do
    [ "$mode" = time ] && [ $i -ge 1 ] && break
    local bn; bn=$(basename "$b")
    export MB=$mb
    locked t "$wk/$bn.c" enc "$cfg" "$b" "$wk/$bn.z"
    read -r w u s m rc < "$wk/$bn.c"
    [ "$rc" = 0 ] || ok=0
    raw=$((raw + $(stat -c %s "$b"))); stored=$((stored + $(stat -c %s "$wk/$bn.z")))
    ne=$((ne + 1)); cc=$(python3 -c "print($cc+$u+$s)"); wc=$(python3 -c "print($wc+$w/1e6)"); [ "$m" -gt "$rss" ] && rss=$m
    if [ "$cfg" != zpaq:5 ] || [ $i = 0 ]; then
      locked t "$wk/$bn.d" dec "$cfg" "$wk/$bn.z" "$wk/$bn.out"
      read -r w u s m rc < "$wk/$bn.d"
      cmp -s "$b" "$wk/$bn.out" || ok=0
      nd=$((nd + 1)); cd=$(python3 -c "print($cd+$u+$s)"); wd=$(python3 -c "print($wd+$w/1e6)"); [ "$m" -gt "$rss" ] && rss=$m
    fi
    rm -f "$wk/$bn.out"
    i=$((i + 1))
  done
  emit "$c" "$cfg" "$mb" "$i" "$raw" "$stored" "$ne" "$nd" "$cc" "$cd" "$wc" "$wd" "$ok" "$mode" "$rss" | tee -a "$OUT"
  rm -rf "$wk"
}

MODE=${1:-}; shift || true
case $MODE in
  size|time) ;;
  *) sed -n 2,19p "$0"; exit 2 ;;
esac
for c in ${@:-swe ohjson othink hermes ohsft fiqa owner}; do
  for mb in $SIZES; do
    for cfg in $CONFIGS; do
      if python3 -I -c '
import json, sys
for l in open(sys.argv[1]):
    d = json.loads(l)
    if (d["mode"], d["corpus"], d["codec"], str(d["block_mib"])) == tuple(sys.argv[2:6]): sys.exit(0)
sys.exit(1)' "$OUT" "$MODE" "$c" "$cfg" "$mb" 2>/dev/null; then continue; fi
      measure "$MODE" "$c" "$cfg" "$mb"
    done
  done
done
