#!/usr/bin/env bash
# TraceBench Postgres baseline container: pull | start | stop | halt | kill | reset | status | path | psql.
#
# Every server setting is on the command line below (nothing is left to a
# config file); docs/TRACEBENCH_BASELINES.md explains each one. The `pg` arm
# runs `reset` when it opens a store, so every run starts from a fresh
# cluster (initdb) with no WAL or files left by an earlier run; that keeps
# the erasure scan honest. The data directory is a bind mount owned by the
# invoking user, so the benchmark process can read every file directly.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
NAME=oojuro-tracedb-pg
IMAGE=postgres:17
PORT=${TRACEDB_PG_PORT:-56510}
DIR=$ROOT/.scratch/docker/$NAME
DATA=$DIR/pgdata

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
  # --network host: no docker-proxy/NAT hop between the runner and the server
  #   (the server listens on 127.0.0.1 only).
  # --user + /etc/passwd: files belong to the invoking user (erasure scan).
  # --memory 3g == --memory-swap: hard 3 GB cap, no swap.
  # --shm-size: dynamic shared memory for parallel query (shared_buffers is
  #   an anonymous shared mapping and counts against the 3 GB cap).
  docker run -d --name "$NAME" \
    --network host \
    --user "$(id -u):$(id -g)" -v /etc/passwd:/etc/passwd:ro \
    --memory 3g --memory-swap 3g --shm-size 256m \
    -e POSTGRES_USER=tracebench -e POSTGRES_PASSWORD=tracebench -e POSTGRES_DB=tracebench \
    -v "$DATA":/var/lib/postgresql/data \
    "$IMAGE" postgres \
    -c listen_addresses=127.0.0.1 \
    -c port="$PORT" \
    -c max_connections=100 \
    -c shared_buffers=1GB \
    -c effective_cache_size=2GB \
    -c work_mem=32MB \
    -c maintenance_work_mem=256MB \
    -c autovacuum_work_mem=-1 \
    -c huge_pages=off \
    -c fsync=on \
    -c synchronous_commit=on \
    -c full_page_writes=on \
    -c wal_level=replica \
    -c wal_sync_method=fdatasync \
    -c wal_compression=lz4 \
    -c wal_buffers=-1 \
    -c wal_writer_delay=200ms \
    -c commit_delay=0 \
    -c commit_siblings=5 \
    -c wal_recycle=off \
    -c wal_init_zero=on \
    -c wal_keep_size=0 \
    -c max_wal_size=8GB \
    -c min_wal_size=1GB \
    -c checkpoint_timeout=15min \
    -c checkpoint_completion_target=0.9 \
    -c default_toast_compression=lz4 \
    -c random_page_cost=1.1 \
    -c effective_io_concurrency=200 \
    -c max_worker_processes=8 \
    -c max_parallel_workers=8 \
    -c max_parallel_workers_per_gather=2 \
    -c max_parallel_maintenance_workers=2 \
    -c jit=on \
    -c autovacuum=on \
    -c gin_pending_list_limit=4MB \
    -c default_text_search_config=pg_catalog.english \
    -c log_min_duration_statement=-1 \
    -c log_checkpoints=on \
    >/dev/null
  for _ in $(seq 1 1200); do  # 0.1 s polls (open time is measured through this)
    if docker exec "$NAME" pg_isready -q -h 127.0.0.1 -p "$PORT" -U tracebench 2>/dev/null; then
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
    rm -rf "$DATA"
    start
    ;;
  status) docker ps -a --filter "name=^${NAME}$" --format '{{.Names}} {{.Status}}' ;;
  path) echo "$DATA" ;;
  psql) shift; docker exec -i -e PGPASSWORD=tracebench "$NAME" psql -h 127.0.0.1 -p "$PORT" -U tracebench -d "${TRACEDB_PG_DB:-tracebench}" "$@" ;;
  *) echo "usage: $0 pull|start|stop|halt|kill|reset|status|path|psql [args]" >&2; exit 2 ;;
esac
