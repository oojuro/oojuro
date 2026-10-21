#!/usr/bin/env bash
# TraceBench Qdrant competitor container: pull | start | stop | halt | kill | reset | status | path.
#
# Every server setting is an environment variable below (everything else is
# the image's config/config.yaml); collection and payload-index settings are
# in crates/oojuro-tracebench/src/arms/qdrant.rs. docs/TRACEBENCH_BASELINES.md
# explains each one. The `qdrant` arm runs `reset` when it opens a store, so
# every run starts from an empty storage directory.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
NAME=oojuro-cmp-qdrant
IMAGE=qdrant/qdrant:v1.19.2
HTTP_PORT=${TRACEDB_QDRANT_PORT:-56590}
GRPC_PORT=${TRACEDB_QDRANT_GRPC_PORT:-56591}
DIR=$ROOT/.scratch/docker/$NAME
DATA=$DIR/storage
SNAP=$DIR/snapshots

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
  mkdir -p "$DATA" "$SNAP"
  # --network host: no docker-proxy/NAT hop (server listens on 127.0.0.1).
  # --user: files belong to the invoking user (erasure scan).
  # --memory 3g == --memory-swap: hard 3 GB cap, no swap.
  docker run -d --name "$NAME" \
    --network host \
    --user "$(id -u):$(id -g)" \
    --memory 3g --memory-swap 3g \
    -v "$DATA":/qdrant/storage \
    -v "$SNAP":/qdrant/snapshots \
    -e QDRANT__SERVICE__HOST=127.0.0.1 \
    -e QDRANT__SERVICE__HTTP_PORT="$HTTP_PORT" \
    -e QDRANT__SERVICE__GRPC_PORT="$GRPC_PORT" \
    -e QDRANT__TELEMETRY_DISABLED=true \
    -e QDRANT_INIT_FILE_PATH=/qdrant/snapshots/.qdrant-initialized \
    -e QDRANT__LOG_LEVEL=WARN \
    "$IMAGE" >/dev/null
  for _ in $(seq 1 1200); do  # 0.1 s polls (open time is measured through this)
    if curl -sf "http://127.0.0.1:$HTTP_PORT/readyz" >/dev/null 2>&1; then
      echo "$NAME ready on 127.0.0.1:$HTTP_PORT"
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
    rm -rf "$DATA" "$SNAP"
    start
    ;;
  status) docker ps -a --filter "name=^${NAME}$" --format '{{.Names}} {{.Status}}' ;;
  path) echo "$DATA" ;;
  *) echo "usage: $0 pull|start|stop|halt|kill|reset|status|path" >&2; exit 2 ;;
esac
