#!/bin/bash
# Build the OpenZL CLI `zli` (BSD-3-Clause) without cmake or make.
#   Source: https://github.com/facebook/openzl  commit 2d30fdebd9e43eb03d7bca3ef3e2f15732d8ab7a
#   Deps (the exact tarballs + sha256 pinned in the repo's top-level Makefile):
#     https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz
#       sha256 eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3
#     https://github.com/lz4/lz4/releases/download/v1.10.0/lz4-1.10.0.tar.gz
#       sha256 537512904744b35e232912055ccf8ec66d768639ff3abe5788d90d792ec5f48b
# We replay the Makefile's `zli` target by hand: libopenzl (all of src/ except
# tests), zstd's static lib (single-threaded, as zstd's own libzstd.a), lz4's lib,
# and the ZLI_DIRS C/C++ units. Flags = its ADAPTIVE zli flags
# (-O3 -g0 -DNDEBUG, C11 / C++17 "c++1z"; no -march=native, as upstream).
# One deviation: the optional ML-selector *trainer* (tools/ml_selector/
# ml_selector_trainer.cpp) needs xgboost, which the Makefile builds with cmake;
# it is left out and OPENZL_HAS_ML_SELECTOR_TRAINER is not defined, which the
# sources support (tools/training/train.cpp #if-guards it). Compression and
# decompression paths are unaffected.
# Usage: . ~/.sa-agents/oojuro-toolchain/env.sh compress; bash build_openzl.sh
set -euo pipefail
EXT=${EXT:-$HOME/.sa-agents/oojuro-compress/.scratch/ext}
URL=https://github.com/facebook/openzl
COMMIT=2d30fdebd9e43eb03d7bca3ef3e2f15732d8ab7a
D=$EXT/src/openzl; SRC=$D/repo; BIN=$EXT/bin; mkdir -p "$BIN" "$D/dl"
if [ ! -d "$SRC/.git" ]; then
  git init -q "$SRC"; git -C "$SRC" fetch -q --depth 1 "$URL" "$COMMIT"; git -C "$SRC" checkout -q FETCH_HEAD
fi
test "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT"

fetch() { # name url sha256 -> unpacked into repo/deps/<name> (what the Makefile's fetch_dependency does)
  local f=$D/dl/$(basename "$2")
  [ -f "$f" ] || curl -sSfL -o "$f" "$2"
  echo "$3  $f" | sha256sum -c -
  if [ ! -f "$SRC/deps/$1/lib/$1.h" ]; then rm -rf "$SRC/deps/$1"; mkdir -p "$SRC/deps/$1"; tar -xzf "$f" -C "$SRC/deps/$1" --strip-components=1; fi
}
fetch zstd https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3
fetch lz4  https://github.com/lz4/lz4/releases/download/v1.10.0/lz4-1.10.0.tar.gz      537512904744b35e232912055ccf8ec66d768639ff3abe5788d90d792ec5f48b

OBJ=$EXT/build/openzl; rm -rf "$OBJ"; mkdir -p "$OBJ"/{zstd,lz4,lib,zli}
cd "$SRC"   # the Makefile compiles with repo-relative include paths
INC="-I. -Iinclude -Isrc -Icpp/include -Icpp/src -Ideps/zstd/lib -Ideps/lz4/lib"
OPT="-O3 -g0 -DNDEBUG"
# compile <objdir> <compiler + flags...> -- reads source paths on stdin, 2 at a time
compile() { local o=$1; shift; xargs -P2 -I{} sh -c 'o="$1"; s="$2"; shift 2; "$@" -c "$s" -o "$o/$(echo "$s" | tr / _).o"' _ "$o" {} "$@"; }

# zstd static lib (zstd's lib/Makefile defaults: -O3, single-threaded static lib)
find deps/zstd/lib/common deps/zstd/lib/compress deps/zstd/lib/decompress deps/zstd/lib/dictBuilder \
     \( -name '*.c' -o -name '*.S' \) | sort | compile "$OBJ/zstd" cc -O3 -DNDEBUG -Ideps/zstd/lib -Ideps/zstd/lib/common
# lz4 lib
ls deps/lz4/lib/lz4.c deps/lz4/lib/lz4hc.c deps/lz4/lib/lz4frame.c deps/lz4/lib/xxhash.c | compile "$OBJ/lz4" cc -O3 -DNDEBUG
# libopenzl: every C/asm unit under src/ except tests
find src \( -name '*.c' -o -name '*.S' \) -not -path '*/tests/*' | sort | compile "$OBJ/lib" cc $OPT -std=c11 $INC
# zli: cli/zli.cpp + ZLI_DIRS units (minus tests, other mains, the Folly-only pytorch parser, the xgboost trainer)
ZLI_DIRS="cli custom_parsers cpp/src tools/arg tools/io tools/logger tools/ml_selector tools/sddl/compiler
 tools/sddl2/assembler tools/sddl2/compiler tools/training"
find $ZLI_DIRS \( -name '*.c' -o -name '*.cpp' \) -not -path '*/tests/*' \
  -not -name main.cpp -not -name pytorch_model_compressor.cpp -not -name ml_selector_trainer.cpp | sort > "$OBJ/zli.list"
grep '\.c$'   "$OBJ/zli.list" | compile "$OBJ/zli" cc  $OPT -std=c11   $INC -DZDICT_STATIC_LINKING_ONLY
grep '\.cpp$' "$OBJ/zli.list" | compile "$OBJ/zli" c++ $OPT -std=c++1z $INC -DZDICT_STATIC_LINKING_ONLY \
  -Ideps/xgboost/include -DDMLC_LOG_STACK_TRACE=0
c++ $OPT "$OBJ"/zli/*.o "$OBJ"/lib/*.o "$OBJ"/zstd/*.o "$OBJ"/lz4/*.o -lm -lpthread -o "$BIN/zli"
echo "built $BIN/zli"
"$BIN/zli" --help 2>&1 | head -5 || true
