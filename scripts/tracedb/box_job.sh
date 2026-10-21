#!/usr/bin/env bash
# One TraceBench job on the dedicated bench box (or any idle machine): build
# once, check every arm against the oracle on the smoke file, measure the
# disk and codec bounds, then run every suite for every arm, interleaving
# arms within each repetition so drift hits all of them alike.
#
# Usage: scripts/tracedb/box_job.sh <tier> <runs> [arms...]
# Data:  $TRACEDB_DATA (default ~/.cache/oojuro-tracedb-data) with
#        base1m.tbe and smoke.tbe (built by scripts/tracedb/build_events.py).
# Out:   bench-results/tracedb/tier<tier>.jsonl (+ probes, logs)
set -uo pipefail
tier=${1:?tier}; runs=${2:?runs}; shift 2
arms=("$@"); [ ${#arms[@]} -gt 0 ] || arms=(oojuro pg ch sqlite)
DATA=${TRACEDB_DATA:-$HOME/.cache/oojuro-tracedb-data}
# Sample sizes (the same for every arm): the turn loop replays the last
# TURN_FRAC of the turn batches (0.03 = ~11k turns at tier 1); ingest
# appends the first INGEST batches. ClickHouse's ~1 s searches and ~0.3 s
# durable appends make the full 10% / 60k replay exceed the box's job limit.
TURN_FRAC=${TRACEDB_TURN_FRAC:-0.03}
INGEST=${TRACEDB_INGEST:-30000}
OUT=bench-results/tracedb; mkdir -p "$OUT" .scratch/work
log() { echo "[$(date -u +%T)] $*" | tee -a "$OUT/job.log"; }

log "build"
cargo build --release -p oojuro-tracebench --bins 2>&1 | tail -2
cargo build --release -p oojuro-trace --example bounds_probe 2>&1 | tail -1
B=target/release/tracebench

# Memory cap: a 3 GiB user scope where available (embedded arms), else a
# system scope via passwordless sudo, else none (recorded in box.json);
# docker arms always run with --memory 3g.
if systemd-run --user --scope -q -p MemoryMax=3G true 2>/dev/null; then
  SCOPE=(systemd-run --user --scope -q -p MemoryMax=3G -p MemorySwapMax=0 --); cap=user-scope-3G
elif sudo -n systemd-run --scope -q -p MemoryMax=3G --uid="$(id -u)" --gid="$(id -g)" true 2>/dev/null; then
  SCOPE=(sudo -n systemd-run --scope -q -p MemoryMax=3G -p MemorySwapMax=0 --uid="$(id -u)" --gid="$(id -g)"
    --setenv=HOME="$HOME" --setenv=PATH="$PATH" --setenv=TMPDIR="$TMPDIR" --working-directory="$PWD" --)
  cap=system-scope-3G
else
  SCOPE=(); cap=none
fi
log "memory cap for embedded arms: $cap"
echo "{\"memory_cap\":\"$cap\",\"nproc\":$(nproc),\"cpu\":\"$(lscpu | sed -n 's/Model name: *//p' | head -1)\"}" > "$OUT/box.json"

for img in postgres:17 clickhouse/clickhouse-server:26.9.23.15; do
  docker pull -q "$img" > /dev/null && log "pulled $img"
done

log "oracle check on smoke"
for arm in "${arms[@]}"; do
  if "${SCOPE[@]}" $B verify --arm "$arm" --data "$DATA/smoke.tbe" --work .scratch/work \
      --out "$OUT/verify.jsonl" > /dev/null 2>> "$OUT/job.log"; then
    log "verify $arm: $(tail -1 "$OUT/verify.jsonl" | python3 -c 'import json,sys; print(json.load(sys.stdin)["verify"]["failures"])')"
  else
    log "verify $arm FAILED"
  fi
done

if [ "$tier" = 1 ]; then
  log "fsync probe"
  cargo build --release -p oojuro-tracebench --bin fsync-probe 2>&1 | tail -1
  target/release/fsync-probe .scratch/fsync 5 >> "$OUT/fsync.jsonl"
  target/release/fsync-probe .scratch/fsync 5 async >> "$OUT/fsync.jsonl"
  log "bounds probe"
  "${SCOPE[@]}" target/release/examples/bounds_probe "$DATA/base1m.tbe" .scratch/bounds 3 >> "$OUT/bounds.jsonl"
fi

clean_work() {
  find .scratch/work -mindepth 1 -maxdepth 1 -exec rm -r {} +
}

one() { # arm suite run [flags]
  local arm=$1 suite=$2 r=$3; shift 3
  local t0
  t0=$(date +%s)
  if "${SCOPE[@]}" $B "$suite" --arm "$arm" --data "$DATA/base1m.tbe" --tier "$tier" --run "$r" \
      --work .scratch/work --out "$OUT/tier$tier.jsonl" "$@" > /dev/null 2>> "$OUT/job.log"; then
    log "ok   $arm $suite run $r ($(( $(date +%s) - t0 )) s)"
  else
    log "FAIL $arm $suite run $r ($(( $(date +%s) - t0 )) s)"
  fi
  clean_work
}

for ((r = 0; r < runs; r++)); do
  for arm in "${arms[@]}"; do one "$arm" reads "$r"; done
  for arm in "${arms[@]}"; do one "$arm" turnloop "$r" --turnloop-frac "$TURN_FRAC"; done
  for arm in "${arms[@]}"; do one "$arm" ingest "$r" --ingest-batches "$INGEST"; done
done

# variants, once: non-durable / deferred commit, and the archive tier
if [ "$tier" = 1 ]; then
  for arm in pg:sync=off oojuro:deferred=on; do
    case " ${arms[*]} " in
      *" ${arm%%:*} "*) one "$arm" turnloop 0 --turnloop-frac "$TURN_FRAC"; one "$arm" ingest 0 --ingest-batches "$INGEST" ;;
    esac
  done
  case " ${arms[*]} " in *" oojuro "*) one oojuro:archive=on reads 0 ;; esac
fi
log "done"
