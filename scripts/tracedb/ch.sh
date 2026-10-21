#!/usr/bin/env bash
# TraceBench ClickHouse baseline container: pull | start | stop | halt | kill | reset | status | path | client.
#
# Server settings live in the generated override below (mounted over the
# image's docker_related_config.xml so nothing listens beyond 127.0.0.1);
# table and query settings are in crates/oojuro-tracebench/src/arms/ch.rs.
# docs/TRACEBENCH_BASELINES.md explains each one. The `ch` arm runs `reset`
# when it opens a store, so every run starts from an empty server directory.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
NAME=oojuro-tracedb-ch
IMAGE=clickhouse/clickhouse-server:26.9.23.15
HTTP_PORT=${TRACEDB_CH_HTTP_PORT:-56520}
TCP_PORT=${TRACEDB_CH_TCP_PORT:-56521}
DIR=$ROOT/.scratch/docker/$NAME
DATA=$DIR/data
LOGS=$DIR/log
CONF=$DIR/conf

write_config() {
  mkdir -p "$CONF"
  cat >"$CONF/tracebench.xml" <<EOF
<clickhouse>
    <!-- loopback only, on ports no other local container uses -->
    <listen_host>127.0.0.1</listen_host>
    <listen_try>0</listen_try>
    <http_port>$HTTP_PORT</http_port>
    <tcp_port>$TCP_PORT</tcp_port>
    <mysql_port remove="remove"/>
    <postgresql_port remove="remove"/>
    <interserver_http_port remove="remove"/>
    <keep_alive_timeout>60</keep_alive_timeout>
    <!-- the image ships trace-level logging; production runs information -->
    <logger>
        <level>information</level>
        <log>/var/log/clickhouse-server/clickhouse-server.log</log>
        <errorlog>/var/log/clickhouse-server/clickhouse-server.err.log</errorlog>
        <size>100M</size>
        <count>3</count>
    </logger>
    <!-- text_log mirrors the server log into a table; off (query_log,
         part_log, metric logs etc. keep their defaults) -->
    <text_log remove="remove"/>
    <max_server_memory_usage_to_ram_ratio>0.9</max_server_memory_usage_to_ram_ratio>
</clickhouse>
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
  mkdir -p "$DATA" "$LOGS"
  write_config
  # --network host: no docker-proxy/NAT hop (server listens on 127.0.0.1).
  # --user + /etc/passwd: files belong to the invoking user (erasure scan).
  # --memory 3g == --memory-swap: hard 3 GB cap, no swap; ClickHouse reads
  #   the cgroup limit (max_server_memory_usage = 0.9 x 3 GB).
  docker run -d --name "$NAME" \
    --network host \
    --user "$(id -u):$(id -g)" -v /etc/passwd:/etc/passwd:ro \
    --memory 3g --memory-swap 3g \
    --ulimit nofile=262144:262144 \
    -v "$CONF/tracebench.xml":/etc/clickhouse-server/config.d/docker_related_config.xml:ro \
    -v "$DATA":/var/lib/clickhouse \
    -v "$LOGS":/var/log/clickhouse-server \
    "$IMAGE" >/dev/null
  for _ in $(seq 1 1200); do  # 0.1 s polls (open time is measured through this)
    if docker exec "$NAME" clickhouse-client --port "$TCP_PORT" -q 'SELECT 1' >/dev/null 2>&1; then
      echo "$NAME ready on 127.0.0.1:$HTTP_PORT"
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

# halt: clean shutdown (SIGTERM / the image's stop signal), container and data
# directory kept; the next `start` runs a new container on the same data
# directory (used by the ycsb suite to time opening an existing store).
halt() {
  docker stop -t 300 "$NAME" >/dev/null 2>&1 || true
}

# kill: SIGKILL the server (a crash), data directory kept; the next `start`
# recovers from it (ycsb crash-recovery test).
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
    rm -rf "$DATA" "$LOGS"
    start
    ;;
  status) docker ps -a --filter "name=^${NAME}$" --format '{{.Names}} {{.Status}}' ;;
  path) echo "$DATA" ;;
  client) shift; docker exec -i "$NAME" clickhouse-client --port "$TCP_PORT" "$@" ;;
  *) echo "usage: $0 pull|start|stop|halt|kill|reset|status|path|client [args]" >&2; exit 2 ;;
esac
