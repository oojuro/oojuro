#!/usr/bin/env bash
# TraceBench driver: one invocation per (arm, suite, run), each in a fresh
# process inside a 3 GiB memory scope, timed runs under the shared bench lock.
# Usage: scripts/tracedb/run.sh <arm> <suite> <runs> <tier> [extra tracebench flags...]
# Results append to .scratch/results/tier<tier>.jsonl.
set -euo pipefail
arm=$1 suite=$2 runs=$3 tier=$4; shift 4
cd "$(dirname "$0")/../.."
LOCK=${OOJURO_BENCH_LOCK:-$HOME/.cache/oojuro-bench.lock}; mkdir -p "$(dirname "$LOCK")"
data=${TRACEDB_DATA:-.scratch/data}/base1m.tbe
out=.scratch/results/tier${tier}.jsonl
mkdir -p .scratch/results .scratch/work .scratch/bin
# Freeze the binary: queued runs must not pick up later rebuilds.
bin=.scratch/bin/tracebench-$(git rev-parse --short HEAD)-$(sha256sum target/release/tracebench | cut -c1-8)
[ -x "$bin" ] || cp target/release/tracebench "$bin"
for ((r = 0; r < runs; r++)); do
  flock "$LOCK" \
    systemd-run --user --scope -q -p MemoryMax=3G -p MemorySwapMax=0 -- \
    "$bin" "$suite" --arm "$arm" --data "$data" --tier "$tier" \
      --run "$r" --work .scratch/work --out "$out" "$@" > /dev/null
  echo "done: $arm $suite run $r"
done
