#!/bin/bash
# Build zpaq 7.15 (public domain / Unlicense; embedded divsufsort MIT) from the
# official release zip, exactly as its readme says:
#   g++ -O3 -march=native -Dunix zpaq.cpp libzpaq.cpp -pthread -o zpaq
#   Source: http://mattmahoney.net/dc/zpaq715.zip
#   sha256 e85ec2529eb0ba22ceaeabd461e55357ef099b80f61c14f377b429ea3d49d418
# The zip also ships Windows .exe files; they are never executed.
# Usage: bash build_zpaq.sh   (EXT=dir overrides the install prefix)
set -euo pipefail
EXT=${EXT:-$HOME/.cache/oojuro-ext}
URL=http://mattmahoney.net/dc/zpaq715.zip
SHA=e85ec2529eb0ba22ceaeabd461e55357ef099b80f61c14f377b429ea3d49d418
D=$EXT/src/zpaq715; BIN=$EXT/bin; mkdir -p "$D" "$BIN"
[ -f "$D/zpaq715.zip" ] || curl -sSfL -o "$D/zpaq715.zip" "$URL"
echo "$SHA  $D/zpaq715.zip" | sha256sum -c -
[ -f "$D/x/zpaq.cpp" ] || { mkdir -p "$D/x"; unzip -q -o "$D/zpaq715.zip" -d "$D/x"; }
# zig clang turns __DATE__ into -Werror=date-time; silence it (the date is only in the banner).
c++ -O3 -march=native -Dunix -Wno-date-time "$D/x/zpaq.cpp" "$D/x/libzpaq.cpp" -pthread -o "$BIN/zpaq"
echo "built $BIN/zpaq"
"$BIN/zpaq" 2>&1 | head -2 || true
