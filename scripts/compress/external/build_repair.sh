#!/bin/bash
# Build two public Re-Pair (Larsson & Moffat recursive pairing) implementations.
#  (a) Gonzalo Navarro's repair/despair (char version, top-level dir of the tarball)
#        Source: https://users.dcc.uchile.cl/~gnavarro/software/repair.tgz
#        sha256 1ad8798e9ba55c26fcef689395492dc13a769f9ba445e595cd9a7e3328b99640
#      Writes <file>.R (alphabet + rules as 2x int32 per rule) and <file>.C
#      (final sequence as int32); per its README it is "not a real compressor"
#      (no entropy coding). makefile flags: gcc -O9 -> here -O3.
#  (b) Raymond Wan's Re-Pair/Des-Pair (GPL-3.0)
#        Source: https://github.com/rwanwork/Re-Pair  commit 7323bc9005db1a249a534f05bad49e8d45ca7daa
#      Writes <file>.prel (phrase hierarchy, interpolative-coded) and <file>.seq
#      (sequence as 4-byte ints, no entropy coder). Flags from its src/CMakeLists.txt:
#      -O3 -DNORMAL_EXPAND.
# Binaries: bin/repair-nav, bin/despair-nav, bin/repair-wan, bin/despair-wan
# Usage: . ~/.sa-agents/oojuro-toolchain/env.sh compress; bash build_repair.sh
set -euo pipefail
EXT=${EXT:-$HOME/.sa-agents/oojuro-compress/.scratch/ext}
BIN=$EXT/bin; mkdir -p "$BIN"

# (a) Navarro
NURL=https://users.dcc.uchile.cl/~gnavarro/software/repair.tgz
NSHA=1ad8798e9ba55c26fcef689395492dc13a769f9ba445e595cd9a7e3328b99640
N=$EXT/src/repair-navarro; mkdir -p "$N"
[ -f "$N/repair.tgz" ] || curl -sSfL -o "$N/repair.tgz" "$NURL"
echo "$NSHA  $N/repair.tgz" | sha256sum -c -
[ -f "$N/x/repair.c" ] || { mkdir -p "$N/x"; tar -xzf "$N/repair.tgz" -C "$N/x"; }
# 2000s-era C: modern clang rejects implicit declarations/int conversions by default.
OLD="-O3 -std=gnu89 -Wno-implicit-function-declaration -Wno-int-conversion -Wno-implicit-int -w"
cc $OLD "$N"/x/{repair,array,hash,heap,records,basics}.c -o "$BIN/repair-nav"
cc $OLD "$N"/x/{despair,basics}.c -o "$BIN/despair-nav"

# (b) Wan
WURL=https://github.com/rwanwork/Re-Pair
WCOMMIT=7323bc9005db1a249a534f05bad49e8d45ca7daa
W=$EXT/src/repair-rwan/repo; mkdir -p "$EXT/src/repair-rwan"
if [ ! -d "$W/.git" ]; then
  git init -q "$W"; git -C "$W" fetch -q --depth 1 "$WURL" "$WCOMMIT"; git -C "$W" checkout -q FETCH_HEAD
fi
test "$(git -C "$W" rev-parse HEAD)" = "$WCOMMIT"
WF="-O3 -DNORMAL_EXPAND -w"
cc $WF "$W"/src/{utils,wmalloc,repair,main-repair,seq,phrase,phrasebuilder,phrase-slide-encode,pair,writeout,bitout}.c -lm -o "$BIN/repair-wan"
cc $WF "$W"/src/{utils,wmalloc,despair,main-despair,bitin,phrase-slide-decode,outphrase}.c -lm -o "$BIN/despair-wan"
echo "built $BIN/repair-nav $BIN/despair-nav $BIN/repair-wan $BIN/despair-wan"
