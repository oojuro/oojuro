#!/usr/bin/env bash
# TraceBench Quickwit competitor container: pull | start | stop | halt | kill | reset | status | path.
#
# Every node setting is in the generated config below; index settings are in
# crates/oojuro-tracebench/src/arms/quickwit.rs. docs/TRACEBENCH_BASELINES.md
# explains each one. The `quickwit` arm runs `reset` when it opens a store,
# so every run starts from an empty data directory (splits, metastore, WAL).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
NAME=oojuro-cmp-quickwit
IMAGE=quickwit/quickwit:0.9.2
PORT=${TRACEDB_QW_PORT:-27280}   # REST; gRPC = PORT + 1; gossip (UDP) = PORT
DIR=$ROOT/.scratch/docker/$NAME
DATA=$DIR/qwdata
CONF=$DIR/conf

write_config() {
  mkdir -p "$CONF"
  cat >"$CONF/tracebench.yaml" <<EOF
version: 0.8
node_id: tracebench
cluster_id: tracebench
listen_address: 127.0.0.1
rest:
  listen_port: $PORT
data_dir: /quickwit/qwdata
# caches sized so indexer heap + caches stay inside the 3 GiB container
searcher:
  fast_field_cache_capacity: 512M
  split_footer_cache_capacity: 256M
EOF
}

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
  write_config
  # --network host: no docker-proxy/NAT hop (the node listens on 127.0.0.1).
  # --user: files belong to the invoking user (erasure scan reads them).
  # --memory 3g == --memory-swap: hard 3 GB cap, no swap.
  docker run -d --name "$NAME" \
    --network host \
    --user "$(id -u):$(id -g)" \
    --memory 3g --memory-swap 3g \
    -e QW_CONFIG=/quickwit/config/tracebench.yaml \
    -e QW_DISABLE_TELEMETRY=1 \
    -e RUST_LOG=warn \
    -v "$CONF/tracebench.yaml":/quickwit/config/tracebench.yaml:ro \
    -v "$DATA":/quickwit/qwdata \
    "$IMAGE" run >/dev/null
  for _ in $(seq 1 1200); do  # 0.1 s polls (open time is measured through this)
    if curl -sf "http://127.0.0.1:$PORT/health/readyz" >/dev/null 2>&1; then
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

# halt: clean shutdown (SIGTERM), container and data directory kept; the next
# `start` runs a new container on the same data directory.
halt() {
  docker stop -t 300 "$NAME" >/dev/null 2>&1 || true
}

# kill: SIGKILL the node (a crash), data directory kept.
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
