#!/bin/bash
# Build the libbsc `bsc` CLI (Apache-2.0) from a pinned commit, single-threaded.
#   Source: https://github.com/IlyaGrebnov/libbsc  commit 4178579ff11d878863ea0d2758c125a0987d7b9b (VERSION 3.3.12)
# No cmake on this machine, so we compile the translation units listed in the
# repo's CMakeLists.txt directly. Flags mirror its Release build
# (-O3 -DNDEBUG -march=native, C99 / C++17). OpenMP and CUDA are left OFF, so
# the binary is single-threaded (what the leaderboard measures).
# Usage: bash build_libbsc.sh   (EXT=dir overrides the install prefix)
set -euo pipefail
EXT=${EXT:-$HOME/.cache/oojuro-ext}
URL=https://github.com/IlyaGrebnov/libbsc
COMMIT=4178579ff11d878863ea0d2758c125a0987d7b9b
SRC=$EXT/src/libbsc/repo
BIN=$EXT/bin; mkdir -p "$BIN" "$EXT/src/libbsc"

# Fetch exactly the pinned commit (shallow) into its own directory.
if [ ! -d "$SRC/.git" ]; then
  git init -q "$SRC"
  git -C "$SRC" fetch -q --depth 1 "$URL" "$COMMIT"
  git -C "$SRC" checkout -q FETCH_HEAD
fi
test "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT"

OBJ=$EXT/build/libbsc; rm -rf "$OBJ"; mkdir -p "$OBJ"
CF="-O3 -DNDEBUG -march=native"
cc  $CF -std=c99   -c "$SRC/libbsc/bwt/libsais/libsais.c" -o "$OBJ/libsais.o"
for f in adler32/adler32 bwt/bwt coder/coder coder/qlfc/qlfc coder/qlfc/qlfc_model \
         filters/detectors filters/preprocessing libbsc/libbsc lzp/lzp platform/platform st/st; do
  c++ $CF -std=c++17 -c "$SRC/libbsc/$f.cpp" -o "$OBJ/$(echo "$f" | tr / _).o"
done
c++ $CF -std=c++17 "$SRC/bsc.cpp" "$OBJ"/*.o -o "$BIN/bsc"
echo "built $BIN/bsc"
"$BIN/bsc" 2>&1 | head -3 || true
