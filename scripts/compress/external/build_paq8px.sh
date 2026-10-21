#!/bin/bash
# Build paq8px v217 (GPL-2.0; used for measurement only, nothing is linked into oojuro).
#   Source: https://github.com/hxim/paq8px  commit c84f576fc2c522194cd320743708652a154daf6b (PROGVERSION "217")
# No cmake here: we follow the repo's build/build-linux-with-clang.sh flag set
# (-O3 -fno-fast-math -ffp-contract=off -funroll-loops -ftree-vectorize, bundled
# zlib, -fno-rtti, gnu++17, NDEBUG). We add -march=native (= its CMake NATIVECPU=ON
# option) and drop -flto/-static to keep the zig build quick. paq8px archives are
# only guaranteed to decode with the same binary, so compress and decompress use
# this one binary.
# Usage: bash build_paq8px.sh   (EXT=dir overrides the install prefix)
set -euo pipefail
EXT=${EXT:-$HOME/.cache/oojuro-ext}
URL=https://github.com/hxim/paq8px
COMMIT=c84f576fc2c522194cd320743708652a154daf6b
SRC=$EXT/src/paq8px/repo; BIN=$EXT/bin; mkdir -p "$BIN" "$EXT/src/paq8px"
if [ ! -d "$SRC/.git" ]; then
  git init -q "$SRC"; git -C "$SRC" fetch -q --depth 1 "$URL" "$COMMIT"; git -C "$SRC" checkout -q FETCH_HEAD
fi
test "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT"

OBJ=$EXT/build/paq8px; rm -rf "$OBJ"; mkdir -p "$OBJ"
FP="-DNDEBUG -O3 -march=native -fno-fast-math -ffp-contract=off -funroll-loops -ftree-vectorize"
for f in adler32 crc32 deflate gzlib inffast inflate inftrees trees zutil; do
  cc $FP -fexceptions -include unistd.h -I"$SRC/zlib" -c "$SRC/zlib/$f.c" -o "$OBJ/z_$f.o"
done
# Compile the C++ units two at a time (machine budget: <=2 heavy jobs).
ls "$SRC"/src/*.cpp "$SRC"/src/{file,filter,model,text,lstm}/*.cpp |
  xargs -P2 -I{} sh -c 'o="$1/$(echo "$2" | sed "s#.*/src/##; s#/#_#g").o"; c++ $3 -fno-rtti -std=gnu++17 -I"$4/zlib" -c "$2" -o "$o"' _ "$OBJ" {} "$FP" "$SRC"
c++ $FP "$OBJ"/*.o -o "$BIN/paq8px"
echo "built $BIN/paq8px"
"$BIN/paq8px" 2>&1 | head -2 || true
