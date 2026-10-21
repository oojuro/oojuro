#!/bin/bash
# Timed runs for track/compress on the bench box (one job at a time there, so
# no bench lock and no systemd scope). Phases, one box job each:
#   store   scorecard: default (6 corpora), archive (3), 32 MiB prefixes (5), 3 reps
#   search  archive search latency (4 corpora) + turn loop (record vs coarse arms)
#   codecs  whole-sample speed panel + random-access block leaderboard, 3 reps
# Submit from the dev machine:
#   ~/.sa-agents/oojuro-benchbox/bench-remote compress \
#     --sync ~/.sa-agents/oojuro-compress/.scratch/data/corpus \
#     --sync ~/.sa-agents/oojuro-compress/.scratch/data/s64 \
#     --sync ~/.sa-agents/oojuro-compress/.scratch/data/p32 \
#     -- 'bash scripts/compress/box_job.sh store'
set -euo pipefail
cd "$(dirname "$0")/../.."
phase=${1:?store|search|codecs}
export SCOPE= LOCK=
out=bench-results/box/$phase
mkdir -p "$out" .scratch/out .scratch/stores
cargo build -q --release -p oojuro-store --features codecs,baselines
cargo build -q --release -p oojuro-bench --bins
{ nproc; lscpu | grep -E "Model name|^CPU\(s\)"; uname -r; git rev-parse HEAD; } > "$out/host.txt"
case $phase in
store)
  scripts/compress/scorecard.sh > "$out/scorecard-default.jsonl"
  LABEL=archive COLD_FRAME=262144 DEDUPE=512 DICT=1048576 DICT_SCALE=128 CORPORA="swe ohjson othink" \
    scripts/compress/scorecard.sh > "$out/scorecard-archive.jsonl"
  DATA=.scratch/data/p32 LABEL=p32 CORPORA="swe ohjson othink hermes ohsft" \
    scripts/compress/scorecard.sh > "$out/scorecard-p32.jsonl"
  ;;
search)
  OUT="$out/archive-search.jsonl" scripts/compress/archive_search.sh
  A="hot=524288&window=8388608&cache=67108864"
  target/release/oojuro-bench turnloop --set swe-agent --split dev --trajectories 30 --background 50000 --runs 3 \
    --work .scratch/bench-work \
    --arm "rec=archive-rec?$A" --arm "coarse=archive-coarse?$A" \
    --arm "rec-q32=archive-rec?qterms=32&$A" --arm "coarse-q32=archive-coarse?qterms=32&$A" \
    --arm "coarse-q32-f8=archive-coarse?qterms=32&max_frames=8&$A" \
    --arm "coarse-q32-f16=archive-coarse?qterms=32&max_frames=16&$A" \
    --arm "coarse-q32-f32=archive-coarse?qterms=32&max_frames=32&$A" \
    --out "$out/turnloop-dev.json"
  # Held-out confirmation of the frame-cap result (cluster-prior effect).
  target/release/oojuro-bench turnloop --set swe-agent --split heldout --trajectories 60 --background 50000 --runs 3 \
    --work .scratch/bench-work \
    --arm "rec=archive-rec?$A" --arm "rec-q32=archive-rec?qterms=32&$A" \
    --arm "coarse-q32-f8=archive-coarse?qterms=32&max_frames=8&$A" \
    --arm "coarse-q32-f16=archive-coarse?qterms=32&max_frames=16&$A" \
    --out "$out/turnloop-heldout.json"
  ;;
codecs)
  OUT="$out/whole.jsonl" scripts/compress/leaderboard_whole.sh
  OUT="$out/blocks.jsonl" scripts/compress/leaderboard_blocks.sh
  ;;
*) echo "unknown phase $phase" >&2; exit 2 ;;
esac
