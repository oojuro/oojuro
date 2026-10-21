#!/usr/bin/env bash
# TraceBench OpenObserve competitor container: pull | start | stop | halt | kill | reset | status | path.
#
# Every server setting is an environment variable below (local mode: SQLite
# metadata, parquet files and WAL on the bind-mounted disk);
# docs/TRACEBENCH_BASELINES.md explains each one. The `openobserve` arm runs
# `reset` when it opens a store, so every run starts from an empty data
# directory. The root credentials only guard this loopback-only, throwaway
# container (TRACEDB_OO_USER / TRACEDB_OO_PASSWORD override them; the arm
# reads the same variables).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
NAME=oojuro-cmp-openobserve
IMAGE=openobserve/openobserve:v1.1.2
PORT=${TRACEDB_OO_PORT:-27540}       # HTTP; gRPC = PORT + 1
USER_EMAIL=${TRACEDB_OO_USER:-tracebench@example.com}
USER_PASSWORD=${TRACEDB_OO_PASSWORD:-Tracebench-local-0nly}
DIR=$ROOT/.scratch/docker/$NAME
DATA=$DIR/data

pull() {
  docker image inspect "$IMAGE" >/dev/null 2>&1 || docker pull "$IMAGE"
}

start() {
  pull
  if docker ps --format '{{.Names}}' | grep -qx "$NAME"; then
    echo "$NAME already running"
    return
  fi
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  mkdir -p "$DATA"
  # --network host: no docker-proxy/NAT hop (bound to 127.0.0.1).
  # --user: files belong to the invoking user (erasure scan reads them).
  # --memory 3g == --memory-swap: hard 3 GB cap, no swap (OpenObserve sizes
  #   its caches and memtable from the cgroup limit).
  # ZO_WAL_FSYNC_DISABLED=false: every ingest request fsyncs the WAL before
  #   it is acknowledged (the default, true, only flushes to the OS).
  # ZO_INGEST_ALLOWED_UPTO: the data's clock is 2026-09; the default accepts
  #   only the last 5 hours.
  # ZO_RESULT_CACHE_ENABLED=false: the result cache treats time ranges older
  #   than ZO_CACHE_DELAY_SECS (300 s) as immutable; every TraceBench event
  #   is older than that, so cached answers would miss later appends.
  docker run -d --name "$NAME" \
    --network host \
    --user "$(id -u):$(id -g)" \
    --memory 3g --memory-swap 3g \
    -e ZO_ROOT_USER_EMAIL="$USER_EMAIL" \
    -e ZO_ROOT_USER_PASSWORD="$USER_PASSWORD" \
    -e ZO_LOCAL_MODE=true \
    -e ZO_LOCAL_MODE_STORAGE=disk \
    -e ZO_DATA_DIR=/data/ \
    -e ZO_HTTP_ADDR=127.0.0.1 \
    -e ZO_HTTP_PORT="$PORT" \
    -e ZO_GRPC_ADDR=127.0.0.1 \
    -e ZO_GRPC_PORT="$((PORT + 1))" \
    -e ZO_WAL_FSYNC_DISABLED=false \
    -e ZO_INGEST_ALLOWED_UPTO=1000000 \
    -e ZO_RESULT_CACHE_ENABLED=false \
    -e ZO_TELEMETRY=false \
    -e ZO_USAGE_REPORTING_ENABLED=false \
    -e RUST_LOG="${TRACEDB_OO_LOG:-warn}" \
    -v "$DATA":/data \
    "$IMAGE" >/dev/null
  for _ in $(seq 1 1200); do  # 0.1 s polls (open time is measured through this)
    if curl -sf "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
      echo "$NAME ready on 127.0.0.1:$PORT"
      return
    fi
    sleep 0.1
  done
  docker logs --tail 50 "$NAME" >&2 || true
  echo "$NAME did not become ready" >&2
  exit 1
}

stop() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}

# halt: clean shutdown (SIGTERM), container and data directory kept.
halt() {
  docker stop -t 300 "$NAME" >/dev/null 2>&1 || true
}

# kill: SIGKILL the server (a crash), data directory kept.
kill_server() {
  docker kill -s KILL "$NAME" >/dev/null 2>&1 || true
}

case "${1:-}" in
  pull) pull ;;
  start) start ;;
  stop) stop ;;
  halt) halt ;;
  kill) kill_server ;;
  reset)
    stop
    rm -rf "$DATA"
    start
    ;;
  status) docker ps -a --filter "name=^${NAME}$" --format '{{.Names}} {{.Status}}' ;;
  path) echo "$DATA" ;;
  *) echo "usage: $0 pull|start|stop|halt|kill|reset|status|path" >&2; exit 2 ;;
esac
