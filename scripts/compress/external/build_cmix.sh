#!/bin/bash
# Build cmix v21 (GPL-3.0; used for measurement only, nothing is linked into oojuro).
#   Source: https://github.com/byronknoll/cmix  commit 194af9cd133b8b741d2a53afca13ed0ca453c276 (README: "cmix version 21")
# Flags are the repo makefile's `all` target:
#   -std=c++17 -DP1_MODE=2 -DP3_BATCH -DP2_FASTMATH -DLSTM_QUANT=8 -Ofast -march=native -DNDEBUG
# (makefile uses clang++-17; here zig's bundled clang). The `cmix` target only;
# the enwik9 preprocessors are not needed.
# Usage: . ~/.sa-agents/oojuro-toolchain/env.sh compress; bash build_cmix.sh
set -euo pipefail
EXT=${EXT:-$HOME/.sa-agents/oojuro-compress/.scratch/ext}
URL=https://github.com/byronknoll/cmix
COMMIT=194af9cd133b8b741d2a53afca13ed0ca453c276
SRC=$EXT/src/cmix/repo; BIN=$EXT/bin; mkdir -p "$BIN" "$EXT/src/cmix"
if [ ! -d "$SRC/.git" ]; then
  git init -q "$SRC"; git -C "$SRC" fetch -q --depth 1 "$URL" "$COMMIT"; git -C "$SRC" checkout -q FETCH_HEAD
fi
test "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT"

OBJ=$EXT/build/cmix; rm -rf "$OBJ"; mkdir -p "$OBJ"
F="-std=c++17 -DP1_MODE=2 -DP3_BATCH -DP2_FASTMATH -DLSTM_QUANT=8 -Ofast -march=native -DNDEBUG"
# Same unit list as the makefile's cmix recipe; two compiles at a time.
UNITS="coder/decoder coder/encoder context-manager contexts/bit-context contexts/bracket-context
 contexts/combined-context contexts/context-hash contexts/indirect-hash contexts/interval-hash
 contexts/interval contexts/sparse mixer/byte-mixer mixer/lstm-layer mixer/lstm mixer/mixer-input
 mixer/mixer mixer/sigmoid mixer/sse models/bracket models/byte-model models/direct-hash models/direct
 models/indirect models/fxcmv1 models/match models/paq8 models/ppmd predictor preprocess/dictionary
 preprocess/preprocessor runner states/nonstationary states/run-map"
printf '%s\n' $UNITS | xargs -P2 -I{} sh -c 'c++ $1 -c "$2/src/$3.cpp" -o "$4/$(echo "$3" | tr / _).o"' _ "$F" "$SRC" {} "$OBJ"
c++ $F "$OBJ"/*.o -o "$BIN/cmix"
echo "built $BIN/cmix"
"$BIN/cmix" 2>&1 | head -1 || true
