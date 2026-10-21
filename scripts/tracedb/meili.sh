#!/usr/bin/env bash
# TraceBench Meilisearch competitor container: pull | start | stop | halt | kill | reset | status | path.
#
# Every server setting is on the command line below; index settings are in
# crates/oojuro-tracebench/src/arms/meili.rs. docs/TRACEBENCH_BASELINES.md
# explains each one. The `meili` arm runs `reset` when it opens a store, so
# every run starts from an empty data directory.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
NAME=oojuro-cmp-meili
IMAGE=getmeili/meilisearch:v1.55.0
PORT=${TRACEDB_MEILI_PORT:-56580}
# Production mode needs a master key; this one guards a loopback-only
# benchmark server and protects nothing.
KEY=oojuro-tracebench-local-key
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
  # --network host: no docker-proxy/NAT hop (server listens on 127.0.0.1).
  # --user: files belong to the invoking user (erasure scan).
  # --memory 3g == --memory-swap: hard 3 GB cap, no swap; Meilisearch reads
  #   the cgroup limit (default max indexing memory = 2/3 of it).
  docker run -d --name "$NAME" \
    --network host \
    --user "$(id -u):$(id -g)" \
    --memory 3g --memory-swap 3g \
    -v "$DATA":/meili_data \
    -e MEILI_ENV=production \
    -e MEILI_MASTER_KEY="$KEY" \
    -e MEILI_HTTP_ADDR="127.0.0.1:$PORT" \
    -e MEILI_DB_PATH=/meili_data/data.ms \
    -e MEILI_DUMP_DIR=/meili_data/dumps \
    -e MEILI_SNAPSHOT_DIR=/meili_data/snapshots \
    -e MEILI_NO_ANALYTICS=true \
    -e MEILI_LOG_LEVEL=WARN \
    "$IMAGE" >/dev/null
  for _ in $(seq 1 1200); do  # 0.1 s polls (open time is measured through this)
    if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
      echo "$NAME ready on 127.0.0.1:$PORT"
      return
    fi
    # a server that exited (bad flag, port in use) will not become ready
    [ -n "$(docker ps -q --filter "name=^${NAME}$")" ] || break
    sleep 0.1
  done
  docker logs --tail 50 "$NAME" >&2 || true
  echo "$NAME did not become ready" >&2
  exit 1
}

stop() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}

# halt: clean shutdown, data kept; kill: SIGKILL (a crash), data kept.
halt() {
  docker stop -t 300 "$NAME" >/dev/null 2>&1 || true
}

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
