#!/bin/bash
# Reproduce sys-comparators.jsonl. Builds are size-only (no bench lock, at most two at a time);
# then the timed point-read phases run one system at a time, each holding the bench lock only
# around a single 200-query loop with a 15 s gap between holds.
set -euo pipefail
cd "$(dirname "$0")/../../.."
export TMPDIR=$PWD/.scratch/tmp UV_CACHE_DIR=$PWD/.scratch/sys/uv-cache
S=scripts/compress/systems
PY=.scratch/sys/venv/bin/python
[ -x $PY ] || { ~/.local/bin/uv venv .scratch/sys/venv && ~/.local/bin/uv pip install --python $PY rocksdict==0.3.29; }
run() { systemd-run --user --scope -q -p MemoryMax=3G -- "$@"; }

# 1) builds / sizes / verification (no lock)
run python3 $S/clickhouse_run.py --build-only &
run $PY $S/rocksdb_run.py --build-only
wait
run python3 $S/opensearch_run.py --build-only &
run python3 $S/victorialogs_run.py
STREAM=none run python3 $S/victorialogs_run.py   # single-stream contrast (scope inherits env)
wait
run python3 $S/clp_run.py
run python3 $S/loki_run.py

# 2) timed point reads, one system at a time
run python3 $S/clickhouse_run.py --resume
run $PY $S/rocksdb_run.py --resume
run python3 $S/opensearch_run.py --resume
