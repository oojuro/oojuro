# TraceBench baselines: Postgres 17, ClickHouse 26.9, SQLite 3.53, and competitor arms

How each baseline arm stores the TraceBench events and answers every
operation, every setting it runs with, and why. Spec: `docs/TRACEBENCH.md`.
Code: `crates/oojuro-tracebench/src/arms/{pg,ch,sqlite}.rs`. Containers:
`scripts/tracedb/{pg,ch}.sh` (every server setting is on the command line or
in the generated override there; nothing else is changed from the image).
Competitor arms (search/vector engines Tantivy 0.27, LanceDB 0.40,
Meilisearch 1.55 and Qdrant 1.19; log-search engines Quickwit 0.9 and
OpenObserve 1.1; embedded key-value stores redb 4.4, fjall 3.1 and RocksDB
11.8) are documented in their own part below
([Competitor arms](#competitor-arms)).

## Running the baselines

Prerequisites on a fresh Linux box: Docker usable by the invoking user
(member of the `docker` group), cgroup v2 with Docker's `systemd` cgroup
driver (`mem_bytes` reads `/sys/fs/cgroup/system.slice/docker-<id>.scope/
memory.stat`; with another layout it reports 0), ports 56510, 56520 and 56521
free on 127.0.0.1 (override with `TRACEDB_PG_PORT`, `TRACEDB_CH_HTTP_PORT`,
`TRACEDB_CH_TCP_PORT`; the arms read the same variables).

One-time setup (pulls the pinned images so no run pays for a download):

```bash
scripts/tracedb/pg.sh pull
scripts/tracedb/ch.sh pull
```

Nothing has to be running before a run: opening a `pg`/`ch` store runs
`scripts/tracedb/<arm>.sh reset` itself (pulls if still missing, removes the
container and its data directory, starts a fresh one, waits until it
answers) and dropping the store removes the container
(`TRACEDB_KEEP_CONTAINER=1` keeps it for inspection). The `ycsb` suite
(`docs/TRACEBENCH.md` §6) opens its stores with `keep=on` and removes the
container itself at the end; to time opening an existing store it runs
`<arm>.sh halt` (`docker stop`, a clean shutdown, data kept) and reopens with
`existing=on` (`<arm>.sh start` on the kept data directory, no schema
change), and its crash test runs `<arm>.sh kill` (`docker kill -s KILL`,
data kept) before reopening. The scripts are found
relative to the crate the binary was built from; `TRACEDB_ROOT=<checkout>`
overrides that. Commands (same flags as every arm, see `docs/TRACEBENCH.md`):

```bash
B=target/release/tracebench; D=.scratch/data/base1m.tbe
$B verify   --arm pg     --data $D --work .scratch/work --out .scratch/results/verify.jsonl
$B reads    --arm pg     --data $D --run 0 --work .scratch/work --out .scratch/results/tier1.jsonl
$B turnloop --arm pg     --data $D --run 0 --work .scratch/work --out .scratch/results/tier1.jsonl
$B ingest   --arm pg     --data $D --run 0 --work .scratch/work --out .scratch/results/tier1.jsonl --ingest-batches 60000
# the same with --arm sqlite, --arm ch, --arm pg:sync=off (ingest + turnloop), --arm pg:part=day
```

Must not run concurrently: any two processes using a `pg*` arm (`pg`,
`pg:sync=off`, `pg:part=day` share the container `oojuro-tracedb-pg`, its
port and its data directory, and each run resets them), likewise any two
`ch` runs (`oojuro-tracedb-ch`). `scripts/tracedb/{pg,ch}.sh reset|stop`
while such a run is going kills it. SQLite runs have no shared state but, like
every timed run, should have the machine to themselves.

Common ground for all three:

- **Fresh store per run.** The docker arms run `<arm>.sh reset` when the
  runner opens a store: the container is removed, its bind-mounted data
  directory deleted, and the server re-initialised (initdb / empty
  ClickHouse). No WAL, part or log of an earlier run is ever on disk during
  a run, so disk, memory and erasure numbers belong to that run alone. The
  container is removed again when the store is dropped. SQLite uses one file
  in the run's private work directory.
- **Containers**: `--memory 3g --memory-swap 3g` (3 GiB, no swap, same cap as
  the runner's scope), `--network host` with the server bound to 127.0.0.1
  (no docker-proxy/NAT hop between client and server; ports 56510 Postgres,
  56520/56521 ClickHouse), `--user <invoking uid>:<gid>` with a bind-mounted
  data directory under `.scratch/docker/<container>/` so the benchmark
  process can read every store file for the erasure scan. Images:
  `postgres:17` (17.11) and `clickhouse/clickhouse-server:26.9.23.15`.
- **Memory** (`mem_bytes`): docker arms read the container cgroup's
  `anon + shmem` (Postgres shared_buffers are shmem and count once touched;
  the file cache does not count). SQLite runs inside the benchmark process:
  `RssAnon` of that process (its page caches are heap).
- **Status revisions** (a tool call completes): applied the way the engine
  is normally used, see each arm. In the bulk path (`load`), a revision whose
  call is in the same commit is folded into the call before writing (what a
  bulk loader does; the stored result is identical to insert-then-update).
- **Search query words** (the query is the first 200 bytes of a tool output
  or user message, so OR semantics): maximal runs of word characters,
  lowercased, deduplicated, at most 64 (in practice ~25). Each engine's
  tokenizer then normalises them its own way.
- **Ties**: `search` breaks score ties by newest first (ts desc, id desc);
  every other order is the oracle's.
- **Byte-exact text, tool, args**: stored as given (empty strings stay empty),
  `exit_code` null is SQL NULL.

## Postgres 17 (`pg`, `pg:sync=off`, `pg:part=day`)

### Schema

```sql
CREATE EXTENSION btree_gin;
CREATE TABLE events (
  id bigint, session_id bigint, parent bigint, ts_us bigint, duration_us bigint,   -- 8-byte columns first:
  agent_id int, turn int, seq int, tokens_in int, tokens_out int, exit_code int,   -- no alignment padding
  kind smallint, status smallint,
  tool text, args json COMPRESSION lz4, text text COMPRESSION lz4,
  tsv tsvector COMPRESSION lz4 GENERATED ALWAYS AS (
    CASE WHEN kind IN (1,3,4,5) THEN to_tsvector('english', left(text, 131072)) END) STORED,
  PRIMARY KEY (id)
) WITH (fillfactor = 90);
CREATE INDEX events_session_seq ON events (session_id, seq);   -- session(), agg_session(), export
CREATE INDEX events_kind_ts     ON events (kind, ts_us);       -- filter/count by kind+time, agg_tools
CREATE INDEX events_ts_id       ON events (ts_us, id);         -- time_range (ORDER BY ts,id LIMIT), purge, top_sessions
CREATE INDEX events_search      ON events USING gin (agent_id, tsv);  -- search: agent + full text in one index
```

- Typed columns; `ts_us` is µs since the epoch as `bigint` (the same 8 bytes
  as `timestamptz`, exact comparisons, no conversions).
- **`args` is `json`, not `jsonb`.** The contract requires reading back the
  exact bytes; the data's args are compact JSON (`{"argv":"..."}`) and `jsonb`
  would return them re-serialised (`{"argv": "..."}`, keys reordered,
  duplicates dropped), failing `get`/`session`/`export` equality. `json`
  validates the input and supports the same operators (`->`, `->>`) by
  re-parsing at query time. Empty args (not valid JSON) are stored as NULL
  and read back as `''`.
- **`(ts_us, id)` index** beyond the two required btrees: `time_range` and
  `top_sessions` filter on time across all kinds and `purge_before` deletes
  by time; without it they are full scans (Postgres 17 has no skip scan over
  `(kind, ts_us)`).
- **tsvector prefix**: Postgres caps a tsvector at 1 MB of lexemes; the
  first 131,072 characters of each text are indexed (41 base1m texts exceed
  256 KB, the longest is 1.7 MB). Stored generated column, NULL for thinking
  and system events, so non-indexed kinds never match.
- **`english` text search configuration** (the server default): stemming
  and stop words. `ts_rank` has no IDF, so without stop-word removal the
  ~25-word OR queries would rank by "the"/"a" counts.
- **fillfactor 90**: every tool call is updated by its revision one turn
  later; free space on the page lets that be a HOT update (no new entries in
  the five indexes, in particular no re-insertion of the call's lexemes into
  the GIN index).
- **lz4 TOAST** compression for text, args and tsv (`default_toast_compression
  = lz4` as well).
- **`pg:part=day`** (variant): the same table `PARTITION BY RANGE (ts_us)` with
  70 daily partitions (2026-08-25 .. 2026-11-02, every tier's clock) plus a
  DEFAULT partition, fillfactor 90 per partition, and primary key `(id,
  ts_us)` (a partitioned table's unique key must contain the partition key,
  so `get(id)` and the revision UPDATE probe every partition's PK index).
  `purge_before` drops whole partitions. Verified on base1m (1,237 checks,
  no failures); not part of the timed runs so far.

### Operations

| op | how |
|---|---|
| `append` | one statement = one implicit transaction = one durable commit: `WITH ins AS (INSERT … SELECT FROM unnest($1::int8[], …))`, `upd AS (UPDATE events … FROM unnest(revision arrays))`; one prepared statement for any batch size. Same-batch revisions are folded into the call first (the contract allows them; the data has none). |
| `load` | per chunk (256 turn batches of one loader's sessions): one transaction with a binary `COPY events FROM STDIN` plus one `UPDATE … FROM unnest` for revisions of calls committed by an earlier chunk. Indexes are live during the load (a store that keeps receiving appends); no drop-and-rebuild. |
| `finish_load` | `VACUUM (ANALYZE) events` (visibility map, hint bits, GIN pending list merged, planner stats), `CHECKPOINT`. |
| `get` | `WHERE id = $1` (PK). |
| `get_range` | `substring(convert_to(text,'UTF8') FROM a+1 FOR b-a)` (byte slice as `bytea`; text cut mid-character is legal). |
| `session` | `WHERE session_id = $1 ORDER BY seq` (index). |
| `time_range` | `WHERE ts_us >= $1 AND ts_us < $2 ORDER BY ts_us, id LIMIT n` (index). |
| `filter`/`count` | dynamic WHERE over agent/kind/tool/status/time; `status` is the latest status (revisions are applied in place). Prepared per filter shape. |
| `search` | `SELECT id FROM events, to_tsquery('english', 'w1 \| w2 \| …') q WHERE agent_id = $1 AND tsv @@ q AND ts_us <= $3 ORDER BY ts_rank(tsv, q) DESC, ts_us DESC, id DESC LIMIT k`. Plan (EXPLAIN ANALYZE): Bitmap Index Scan on `events_search` with both `agent_id = $1` and `tsv @@ q` as index conditions, heap recheck, top-N sort. Every OR match is ranked (no top-k pruning in Postgres FTS): a typical base1m query matches ~26k of the agent's rows. `ts_rank` (default normalisation 0), not `ts_rank_cd`: on that query `ts_rank_cd` took 1,371 ms, `ts_rank` 224 ms, detoasting the tsvectors alone 143 ms; cover density (proximity of all query terms) means little for a 25-word OR query. |
| `agg_session` | one aggregate over the session index; text bytes via `octet_length(text)` (reads the TOAST header, does not detoast). |
| `agg_tools` | `WHERE kind = 4 AND ts window GROUP BY tool`; sorted bytewise in the client (the server collation is `en_US.utf8`). |
| `top_sessions` | `GROUP BY session_id ORDER BY sum DESC, session_id LIMIT n` over the time index. |
| `delete_session`, `purge_before` | `DELETE` (autocommit, durable). With `part=day`, whole days below the cutoff are dropped partitions and only the boundary day is a `DELETE`. |
| `reclaim` | `VACUUM (FULL, ANALYZE) events` (rewrites heap, TOAST and every index into new files from live rows; old files are truncated at commit and unlinked at the next checkpoint), `CHECKPOINT`, `pg_switch_wal()`, `CHECKPOINT`. See erasure below. |
| `export` | one transaction, portal over `SELECT … ORDER BY session_id, seq`, fetched 4,096 rows at a time (bounded client memory). |
| `disk_bytes` | `pg_total_relation_size('events')` (heap + TOAST + FSM/VM + all indexes; for `part=day` summed over the partition tree). **WAL is not included** (`pg_wal` is bounded by `max_wal_size` = 8 GB and recycled by checkpoints); the store name says so. |
| `data_paths` | every file under the bind-mounted `PGDATA` (including `pg_wal`). |

Connections: a pool, one connection per concurrent caller (16 writers + up to
8 readers), each with its prepared statements.

### Server settings (`scripts/tracedb/pg.sh`)

| setting | value | why |
|---|---|---|
| `fsync`, `full_page_writes` | on | durable, torn-page safe (defaults) |
| `synchronous_commit` | on (`pg:sync=off`: off per session, labelled NON-DURABLE) | a commit returns after its WAL is flushed |
| `wal_sync_method` | fdatasync | Linux default, explicit |
| `commit_delay`, `commit_siblings` | 0, 5 | defaults. Group commit still happens: concurrent committers waiting on the same WAL flush are released by one fdatasync. A non-zero `commit_delay` trades latency for fewer flushes; not used. |
| `wal_compression` | lz4 | smaller full-page images in WAL |
| `wal_buffers` | -1 (auto: 1/32 of shared_buffers, capped at 16 MB) | default |
| `max_wal_size`, `min_wal_size` | 8GB, 1GB | no checkpoint storms during the bulk load (default 1GB would checkpoint every few seconds) |
| `checkpoint_timeout`, `checkpoint_completion_target` | 15min, 0.9 | spread checkpoints |
| `wal_recycle` | **off** | old WAL segments are unlinked instead of renamed for reuse with their old bytes inside; required for `reclaim` to leave no deleted text in `pg_wal` (otherwise recycled segments keep it until overwritten). Cost: a new segment is created (and zero-filled, `wal_init_zero = on`) every 16 MB of WAL. |
| `wal_keep_size` | 0 | no replication; nothing pins old WAL |
| `shared_buffers` | 1GB | ~1/3 of the 3 GiB container |
| `effective_cache_size` | 2GB | planner hint for the container's cache |
| `work_mem`, `maintenance_work_mem` | 32MB, 256MB | sorts/aggregates; VACUUM FULL / index rebuilds |
| `default_toast_compression` | lz4 | faster than pglz at a similar ratio |
| `random_page_cost`, `effective_io_concurrency` | 1.1, 200 | NVMe SSD |
| `max_parallel_workers_per_gather` | 2 | default; full-table counts/aggregates may use 2 workers |
| `jit` | on | default (only kicks in for the full-window aggregates) |
| `autovacuum` | on | default |
| `gin_pending_list_limit` | 4MB | default: GIN `fastupdate` buffers new entries; the backend that overflows the list merges it (an occasional slow append) |
| `default_text_search_config` | english | explicit (server default) |
| data checksums | off | Postgres 17 initdb default |
| `listen_addresses`, `port` | 127.0.0.1, 56510 | |

### Durability and erasure

- `append` returns after the commit record is flushed (`synchronous_commit
  = on`). `pg:sync=off` returns before the flush: up to ~3 × `wal_writer_delay`
  (600 ms) of acknowledged turns can be lost in a crash; it is reported only
  as a non-durable reference.
- After `delete_session`/`purge_before` the text is still in heap/TOAST pages
  (dead tuples) and in WAL (the original inserts, plus full-page images the
  deletes write after a checkpoint). `reclaim` rewrites the table into new
  files and checkpoints twice around a WAL switch so every segment before the
  new redo point is removed; with `wal_recycle = off` removed segments are
  unlinked, not reused. Remaining caveats: unlinked blocks are not scrubbed on
  the SSD (filesystem level), and texts stored compressed (lz4 TOAST) are not
  plaintext in the files, so a plaintext needle scan cannot see them either
  way.
- Measured (base1m `reads` suite, 3 local runs, identical each time): the 20
  needles were found 14 times in store files after the deletes and 0 times
  after `reclaim`; `reclaim` took 22–47 s and shrank the relations from
  1.66 GB to 1.25 GB.

## ClickHouse 26.9 (`ch`): the Langfuse-style analytics trace store

### Schema

```sql
CREATE TABLE events (
  id UInt64 CODEC(Delta, ZSTD(1)), session_id UInt64 CODEC(Delta, ZSTD(1)), agent_id UInt32 CODEC(ZSTD(1)),
  turn UInt32 CODEC(Delta, ZSTD(1)), seq UInt32 CODEC(Delta, ZSTD(1)), parent UInt64 CODEC(Delta, ZSTD(1)),
  kind UInt8 CODEC(ZSTD(1)), status UInt8 CODEC(ZSTD(1)), exit_code Nullable(Int32) CODEC(ZSTD(1)),
  tokens_in UInt32 CODEC(T64, ZSTD(1)), tokens_out UInt32 CODEC(T64, ZSTD(1)),
  ts_us Int64 CODEC(Delta, ZSTD(1)), duration_us Int64 CODEC(T64, ZSTD(1)),
  tool LowCardinality(String) CODEC(ZSTD(1)), args String CODEC(ZSTD(3)), text String CODEC(ZSTD(3)),
  INDEX idx_id    id       TYPE bloom_filter(0.001) GRANULARITY 1,      -- get/get_range by id
  INDEX idx_agent agent_id TYPE set(16) GRANULARITY 1,                  -- agent-scoped search/filters
  INDEX idx_text  text     TYPE text(tokenizer = splitByNonAlpha, preprocessor = lower(text))  -- search
) ENGINE = MergeTree
PARTITION BY intDiv(ts_us, 86400000000)          -- one partition per day
ORDER BY (session_id, ts_us, id)
SETTINGS <TABLE_SETTINGS>;

CREATE TABLE call_status (id UInt64, session_id UInt64, status UInt8, exit_code Nullable(Int32),
                          duration_us Int64, rev UInt64)          -- rev = the revision's event id
ENGINE = ReplacingMergeTree(rev) ORDER BY id SETTINGS <TABLE_SETTINGS>;

CREATE TABLE ingest (<all event columns>, revises UInt64) ENGINE = Null;
CREATE MATERIALIZED VIEW events_mv      TO events      AS SELECT <event columns> FROM ingest WHERE revises = 0;
CREATE MATERIALIZED VIEW call_status_mv TO call_status AS SELECT revises AS id, session_id, status, exit_code,
                                                              duration_us, id AS rev FROM ingest WHERE revises != 0;
```

`TABLE_SETTINGS = fsync_after_insert = 1, fsync_part_directory = 1,
enable_block_number_column = 1, enable_block_offset_column = 1,
min_compressed_bytes_to_fsync_after_merge = 1, min_rows_to_fsync_after_merge = 1,
old_parts_lifetime = 10, cleanup_delay_period = 1, max_cleanup_delay_period = 2,
cleanup_delay_period_random_add = 0` (index_granularity stays 8192,
index_granularity_bytes 10 MB).

- Shape follows Langfuse's ClickHouse schema (MergeTree ordered by the
  trace/session key and time, day/month partitions, ZSTD(3) on the large text
  columns, `LowCardinality` names, bloom-filter skip indexes on ids).
- **Status revisions: separate ReplacingMergeTree + argMax at read time.**
  ClickHouse has no cheap in-place update. Options measured on this server:
  lightweight `UPDATE … WHERE session_id = s AND id = c` (patch parts):
  ~215 ms p50 and ~64 updates/s total under 16 concurrent writers; one async
  insert into a Null table fanned out by two materialized views: ~275 ms p50
  for the whole turn (bounded by the async-insert flush window, see below)
  with no extra round trip. So a turn is one insert; the revision becomes a
  row in `call_status`, and every read takes `argMax(tuple(status, exit_code,
  duration_us), rev)` per call (correct before and after background merges,
  no `FINAL`) and LEFT JOINs it onto the call. The right side is restricted
  to the ids the query touches (`id IN (SELECT id FROM events WHERE <same
  condition> AND kind = 4)`, primary-key lookups in `call_status`). This is
  the Langfuse pattern (re-insert a newer version, resolve the latest at
  read time) without re-sending the call's text.
- **Text index** (`text`, GA in 26.x): `hasAnyTokens(text, [words])` (OR) is
  answered from the index (direct read; EXPLAIN shows the
  `__text_index_idx_text_hasAnyTokens` prewhere column). The index covers
  every kind's text (a skip index cannot be conditional); thinking and system
  rows are excluded by `kind IN (1,3,4,5)` in the query. The tokenizer splits
  on ASCII non-alphanumerics (non-ASCII bytes are word characters, `_` is a
  separator); the preprocessor lowercases ASCII, so query words are ASCII-
  lowercased in the client.
- **No relevance ranking (capability gap).** ClickHouse has no BM25/tf-idf
  for its text index. Hits are ordered by the number of distinct query words
  each hit contains (`hasTokenCaseInsensitive(text, w1) + …`, which re-reads
  the text of every hit), then newest first.
- `set(16)` skip index on `agent_id`: rows are ordered by session and every
  session belongs to one agent, so most granules hold a few agents and the
  rest are skipped for agent-scoped queries.
- Bloom filter on `id` for point reads (the key is the session). Part-level
  min/max statistics on `id` (automatic in 26.x) already prune to one part;
  the bloom filter picks the granule. A point read still decompresses one
  8,192-row granule of `text`.

### Operations

| op | how |
|---|---|
| `append` | the batch (events + revisions) as one RowBinary `INSERT INTO ingest` over HTTP with `async_insert=1, wait_for_async_insert=1`: the server buffers small inserts from all clients and flushes them as one part (adaptive busy timeout 50–200 ms, the 26.9 defaults), acknowledging each client after the part holding its rows is written; with `fsync_after_insert`/`fsync_part_directory` the part's files and directory are fsynced first. ClickHouse's recommended path for many small inserts; per-turn synchronous inserts would create one part per turn. |
| `load` | per chunk: one synchronous RowBinary insert (`async_insert=0`) of the chunk (revisions of calls in the same chunk folded in). |
| `finish_load` | wait until `system.merges` is empty for 2 s (background merges of the load settle; at most 15 min). |
| `get`, `session`, `time_range` | rows of `events` matching the condition (ordered/limited inside), LEFT JOIN latest status, re-ordered. `get` by bloom filter + id statistics, `session` by primary key, `time_range` by partition/min-max pruning on `ts_us`. |
| `get_range` | `substring(text, a+1, b-a)` (bytes) `WHERE id = …`. |
| `filter`/`count` | condition on events; with a status condition the latest status is joined first and the condition applies to it. |
| `search` | `WHERE agent_id = A AND kind IN (1,3,4,5) AND ts_us <= T AND hasAnyTokens(text, [w…]) ORDER BY (Σ hasTokenCaseInsensitive(text, w)) DESC, ts_us DESC, id DESC LIMIT k`. |
| `agg_session`, `agg_tools` | aggregates over events LEFT JOIN latest status (errors and duration use the latest revision). `length(text)` = bytes. |
| `top_sessions` | `GROUP BY session_id ORDER BY sum(tokens_in + tokens_out) DESC, session_id LIMIT n`. |
| `delete_session` | `DELETE FROM call_status/events WHERE session_id = s` with `lightweight_delete_mode = lightweight_update_force`: a patch part masks the rows (`_row_exists = 0`) and every read applies it. Measured ~60 ms. The default mode (`alter_update`) runs a mutation over every part: ~35 s per session on the 27k-event smoke tier under this box's load (3 background threads), so 100 deletes would take an hour. Returns when the patch is written (`lightweight_deletes_sync = 2`). |
| `purge_before` | status rows of purged calls deleted first; whole days below the cutoff: `ALTER TABLE events DROP PARTITION ID`; boundary day: lightweight `DELETE … WHERE ts_us < cutoff`. |
| `reclaim` | per table `ALTER … APPLY PATCHES` (materialise the delete masks into the parts), `ALTER … APPLY DELETED MASK` (rewrite those parts without the masked rows), both `mutations_sync = 2`, then wait until no inactive part of either table is left on disk. |
| `export` | latest status of every call loaded into a client hash map, then one streamed `SELECT … ORDER BY session_id, seq FORMAT RowBinary` (`max_bytes_before_external_sort = 512 MB` so the sort spills instead of failing in the 3 GiB cap); the client patches statuses. |
| `disk_bytes` | `sum(bytes_on_disk)` of active parts of `events` and `call_status` (data, marks, primary index, skip and text indexes). ClickHouse has no WAL for MergeTree. |
| `data_paths` | every file under the bind-mounted `/var/lib/clickhouse` (includes the system log tables). |

Client: a hand-written HTTP/1.1 keep-alive pool over `std::net` (no
async runtime), RowBinary both ways, one connection per concurrent caller.

### Server settings (`scripts/tracedb/ch.sh`, override of `config.d/docker_related_config.xml`)

| setting | value | why |
|---|---|---|
| `listen_host`, `http_port`, `tcp_port` | 127.0.0.1, 56520, 56521 | loopback only; mysql/postgresql/interserver ports removed |
| `logger.level` | information | the image ships `trace` (every query and part event logged) |
| `text_log` | removed | it mirrors the server log into a table; `query_log`, `part_log`, `metric_log`, `trace_log`, … keep their defaults |
| `max_server_memory_usage_to_ram_ratio` | 0.9 | default, explicit: 0.9 × the 3 GiB cgroup limit |
| `keep_alive_timeout` | 60 s | keep pooled client connections |
| everything else | 26.9 defaults | `async_insert` defaults (busy timeout adaptive 50–200 ms, 10 MB / 450 queries per flush), `max_threads` = cores, `background_pool_size` = 3 (auto-sized by the server for this container), mark cache and other caches capped by `cache_size_to_ram_max_ratio` |

### Durability and erasure

- **Durable on return**: an async insert is acknowledged only after the part
  containing it is written (`wait_for_async_insert = 1`) and, with
  `fsync_after_insert = 1` and `fsync_part_directory = 1`, fsynced with its
  directory. Merge results are fsynced too (`min_*_to_fsync_after_merge = 1`),
  which is what makes the short `old_parts_lifetime` safe (the default 480 s
  exists to keep source parts until un-fsynced merge results survive a
  crash). A turn's insert is split per day partition when it spans midnight
  (two parts, each atomic). There are no multi-statement transactions; the
  events and status rows of a turn are one insert routed by views.
- Lightweight delete patches and mutations (APPLY PATCHES / APPLY DELETED
  MASK) are not covered by `fsync_after_insert`; a crash right after a delete
  could resurrect it until the patch is rewritten.
- Text is ZSTD(3)-compressed in column files, so a plaintext needle scan of
  the files finds nothing before or after reclaim and proves nothing.
  Structural proof instead: after `reclaim` every part that had masked rows
  has been rewritten without them and no inactive part remains on disk
  (`system.parts`, checked by `reclaim` itself). The text index stores
  lowercased tokens of deleted texts until its part is rewritten (same
  rewrite). Query texts in `system.query_log` contain search words (not raw
  texts). Measured (smoke tier): 0 needle hits before and after `reclaim`
  (as expected for compressed columns); `reclaim` 69 s, almost all of it
  waiting for the rewritten parts' predecessors to leave the disk.

## SQLite 3.53 (`sqlite`, rusqlite 0.40 bundled)

### Schema

```sql
CREATE TABLE events (
  id INTEGER PRIMARY KEY,                    -- the rowid: get() is a rowid lookup
  session_id INTEGER NOT NULL, agent_id INTEGER NOT NULL, turn INTEGER NOT NULL, seq INTEGER NOT NULL,
  parent INTEGER NOT NULL, kind INTEGER NOT NULL, status INTEGER NOT NULL, exit_code INTEGER,
  tokens_in INTEGER NOT NULL, tokens_out INTEGER NOT NULL, ts_us INTEGER NOT NULL, duration_us INTEGER NOT NULL,
  tool TEXT NOT NULL, args TEXT NOT NULL, text TEXT NOT NULL      -- text last: column reads stop before it
) STRICT;
CREATE INDEX events_session_seq ON events (session_id, seq);
CREATE INDEX events_kind_ts ON events (kind, ts_us);
CREATE INDEX events_ts ON events (ts_us);    -- (ts_us, rowid): time_range, purge, top_sessions
CREATE VIRTUAL TABLE events_fts USING fts5(
  agent, body, content='', contentless_delete=1, tokenize='porter unicode61');
```

- **Contentless FTS5 with `contentless_delete=1`** (SQLite ≥ 3.43): the index
  does not store a second copy of the text (an external-content table would
  need the old text to delete an entry; a normal FTS5 table stores it again),
  and rows can still be deleted by rowid. Only indexed kinds are inserted.
- The `agent` column holds one token (`a<agent>`); search matches
  `agent : "a7" AND body : ("w1" OR "w2" …)`, so the agent filter is part of
  the full-text match, and `bm25(events_fts, 0.0, 1.0)` gives that column
  zero weight. `porter unicode61` stems like Postgres' `english` config; FTS5
  has no stop words (bm25's IDF discounts common words).
- `args` is plain TEXT (SQLite's JSON functions operate on text).

### Operations

| op | how |
|---|---|
| `append` | one `BEGIN IMMEDIATE` transaction on the single writer connection: prepared INSERTs into `events` and `events_fts`, UPDATEs for revisions; `COMMIT` fsyncs the WAL (`synchronous = FULL`). Concurrent appenders queue on the writer (SQLite has one writer). |
| `load` | the same, one transaction per chunk of 256 turn batches. |
| `finish_load` | `PRAGMA wal_checkpoint(TRUNCATE); ANALYZE`. |
| reads | a pool of connections (one per concurrent reader, `query_only`), WAL readers run concurrently with the writer; same SQL shapes as Postgres. `get_range` = `substr(CAST(text AS BLOB), a+1, b-a)`; text bytes = `length(CAST(text AS BLOB))`. |
| `search` | `SELECT e.id FROM events_fts f JOIN events e ON e.id = f.rowid WHERE events_fts MATCH ? AND e.ts_us <= ? ORDER BY bm25(events_fts, 0.0, 1.0), e.ts_us DESC, e.id DESC LIMIT k`. Plan: FTS5 scan (`INDEX 0:M2`) as the outer loop, rowid lookup into `events`, temp B-tree for the order. |
| `delete_session`, `purge_before` | one transaction: delete the FTS rows by rowid, then the events. |
| `reclaim` | `INSERT INTO events_fts(events_fts) VALUES('optimize')` (merge the FTS index into one segment, dropping deleted entries), `wal_checkpoint(TRUNCATE)`, `VACUUM` (rebuild the file from live pages; the freelist with deleted text is gone), `wal_checkpoint(TRUNCATE)` (VACUUM writes through the WAL; truncate it to 0 bytes). |
| `export` | one streamed `SELECT … ORDER BY session_id, seq` (index order). |
| `disk_bytes`, `data_paths` | the database file, `-wal` and `-shm`. |

### Settings

| pragma | value | why |
|---|---|---|
| `journal_mode` | WAL | readers do not block the writer |
| `synchronous` | FULL | every commit fsyncs the WAL: durable on return (NORMAL would only fsync at checkpoints) |
| `cache_size` | -65536 (64 MiB per connection) | the default 2 MiB thrashes on a 1 GB store; per connection, so up to ~1 GiB with 16 connections |
| `mmap_size` | 0 | default; reads go through the page cache counted in `RssAnon` (memory-mapped reads would hide it in the file cache) |
| `temp_store` | FILE | sort spills and VACUUM's temporary copy go to `$TMPDIR` (on disk) |
| `wal_autocheckpoint` | 1000 pages | default |
| `secure_delete` | OFF | default; deleted text stays in free pages until `reclaim` |
| `busy_timeout` | 600 s | writers wait instead of failing |
| `page_size` | 4096 | default |

### Durability and erasure

- `append` returns after the WAL fsync of its commit. No group commit: 16
  writers take turns on one connection, one fsync each.
- After deletes the text stays in freed pages of the database file and in
  WAL frames; `reclaim` rebuilds the file and truncates the WAL. Text is
  stored uncompressed, so the plaintext scan is a real test here. Measured
  (smoke tier, 20 deleted sessions + 7-day purge): 10 needle hits after the
  deletes, 0 after `reclaim` (0.7 s); the file shrank from 44 MB to 32 MB.

## Competitor arms

Nine more arms time the same events, operations and suites (`verify`,
`ingest`, `reads` + purge, `size`, `turnloop`, `ycsb`) head to head with the
baselines: the Rust search and vector engines an agent-memory builder would
otherwise reach for, two log-search engines, and three embedded key-value
stores. Each runs the way its own documentation recommends for durable
writes.

| arm (spec) | engine | how it runs | code |
|---|---|---|---|
| `tantivy`, `tantivy:commit=periodic` | Tantivy 0.27.0 | embedded library, one index in the run's work directory | `arms/tantivy.rs` |
| `lance`, `lance:fsync=off` | LanceDB 0.40.0 on Lance 13.0.0 | embedded library, one table in the work directory; `--features arm-lance` | `arms/lance.rs` |
| `meili`, `meili:wait=poll` | Meilisearch v1.55.0 | container `oojuro-cmp-meili`, `scripts/tracedb/meili.sh` | `arms/meili.rs` |
| `qdrant` | Qdrant v1.19.2 | container `oojuro-cmp-qdrant`, `scripts/tracedb/qdrant.sh` | `arms/qdrant.rs` |
| `quickwit` | Quickwit 0.9.2 | container `oojuro-cmp-quickwit`, `scripts/tracedb/quickwit.sh` | `arms/quickwit.rs` |
| `openobserve`, `openobserve:settle=off` | OpenObserve v1.1.2 | container `oojuro-cmp-openobserve`, `scripts/tracedb/openobserve.sh` | `arms/openobserve.rs` |
| `redb` | redb 4.4.0 | embedded, one file in the work directory | `arms/kv.rs`, `arms/kv_redb.rs` |
| `fjall` | fjall 3.1.12 | embedded LSM, one directory | `arms/kv.rs`, `arms/kv_fjall.rs` |
| `rocksdb` | RocksDB 11.8.1 (`rocksdb` crate 0.25.0, bundled) | embedded LSM, one directory | `arms/kv.rs`, `arms/kv_rocks.rs` |

**Unsupported operations.** Where an engine cannot do an operation under
the contract (no full-text index, no server-side aggregation or join, no
delete that is invisible on return), the arm returns the typed error
`oojuro_tracebench::Unsupported` instead of an approximation, and the
suites skip it and list it under `unsupported` with the arm's reason:
`verify` does not count it as a check (the oracle does not apply a skipped
delete either, so later checks compare the same state); `reads` writes
`{"unsupported": reason}` in place of that operation's latency summary, and
the 8-thread mix runs without it and is labelled `partial_mix`; `purge`
reports which of delete/purge/reclaim were skipped, checks invisibility only
of what was deleted, and skips the needle scans unless both deletes ran;
`turnloop` and the search half of `fetch` (search-driven) are skipped as a
whole, `{"turnloop": {"unsupported": ...}}`, probed on the empty store
before any load. Nothing is emulated client-side beyond what is stated per
arm (excerpts cut from a fetched field, revisions folded onto returned rows,
result sets sorted where noted).

**Server arms** share one std-only HTTP/1.1 keep-alive client
(`arms/http.rs`) and the Postgres/ClickHouse container conventions: pinned
image, container named `oojuro-cmp-*`, `--network host` with the server
bound to 127.0.0.1, `--memory 3g --memory-swap 3g`, `--user <invoking
uid>:<gid>`, data bind-mounted under `.scratch/docker/<container>/`;
opening a store runs `<arm>.sh reset` (container and data removed, fresh
start, waits until it answers), dropping it removes the container
(`TRACEDB_KEEP_CONTAINER=1` keeps it), and the `ycsb` suite uses `start` /
`halt` / `kill` exactly as for `pg`/`ch`. Ports (loopback): Meilisearch
56580 (`TRACEDB_MEILI_PORT`), Qdrant HTTP 56590 / gRPC 56591
(`TRACEDB_QDRANT_PORT`, `TRACEDB_QDRANT_GRPC_PORT`), Quickwit REST 27280 /
gRPC 27281 (`TRACEDB_QW_PORT`), OpenObserve HTTP 27540 / gRPC 27541
(`TRACEDB_OO_PORT`). Quickwit and OpenObserve sit below Linux's ephemeral
range (32768-60999): a smoke run once failed to start Quickwit on 47281
because an outgoing connection held it as its local port. Never run two runs
of the same server arm at once (each resets its container).

### Supported operations

| op | tantivy | lance | meili | qdrant | quickwit | openobserve | redb / fjall / rocksdb |
|---|---|---|---|---|---|---|---|
| append / load (and ycsb `append_many`) | yes | yes | yes | yes | yes | yes | yes |
| get / get_range / session | yes | yes | yes | yes | yes (revisions folded client-side) | yes (same) | yes |
| time_range / filter / count | yes | yes | yes | yes | yes | yes | yes |
| filter / count on the latest status | yes | yes | yes | yes | **unsupported** | yes (SQL join) | yes |
| search | BM25 | BM25 | ranking rules (first 10 words) | filter only, newest first | BM25 (no stemming) | matched-word count, newest first | **unsupported** |
| agg_session | yes (fast fields) | yes (SQL) | **unsupported** | **unsupported** | yes (from session) | yes (from session) | yes |
| agg_tools | yes (fast fields) | yes (SQL) | **unsupported** | **unsupported** | **unsupported** | yes (SQL join) | yes |
| top_sessions | yes (fast fields) | yes (SQL) | **unsupported** | **unsupported** | yes (terms aggregation) | yes (SQL) | yes |
| delete_session / purge_before | yes | yes | yes | yes | **unsupported** | **unsupported** | yes |
| reclaim | merge + GC | compaction + prune | index compaction | vacuum (WAL keeps text) | **unsupported** | **unsupported** | compaction (fjall: journal keeps text) |
| export | yes | yes | yes | yes | yes | yes | yes |
| ycsb open / crash phases | yes | yes | yes | yes | yes | yes (recovery wait, below) | yes |
| disk_bytes / mem_bytes | dir / RssAnon | dir / RssAnon | dir / cgroup | allocated / cgroup | allocated / cgroup | allocated / cgroup | allocated / RssAnon |

### Running the competitor arms

Build (release). Tantivy, redb, fjall and RocksDB are in the default
features; Meilisearch, Qdrant, Quickwit and OpenObserve need nothing extra.
RocksDB is compiled from source by `librocksdb-sys` (C++; its bindgen needs
libclang: on the local box `LIBCLANG_PATH` points at a directory holding a
`libclang.so` symlink to `/usr/lib/x86_64-linux-gnu/libclang-21.so.21`, and
`BINDGEN_EXTRA_CLANG_ARGS` at zig's clang headers; C/C++ go through the zig
`cc`/`c++` shims). LanceDB pulls Arrow and DataFusion (a ~20 min build with
2 jobs) and its build scripts need `protoc`; the arm is behind a feature:

```bash
export LIBCLANG_PATH=<dir with libclang.so>                 # rocksdb (bindgen)
export BINDGEN_EXTRA_CLANG_ARGS=-I<clang builtin headers>   # zig: <zig>/lib/include
cargo build --release -p oojuro-tracebench                                               # all but lance
PROTOC=/path/to/protoc cargo build --release -p oojuro-tracebench --features arm-lance   # + lance
for s in meili qdrant quickwit openobserve; do scripts/tracedb/$s.sh pull; done          # one-time image pulls
```

`protoc`: any recent release works (validated with the prebuilt
protoc 36.2 for linux-x86_64 from github.com/protocolbuffers/protobuf,
sha256 `121f6c7a…d4278` of the zip). lancedb 0.40.0 does not compile
without its `remote` feature (its `job.rs` names a remote-only error
variant), so the arm enables it; the arm never contacts a server. A
default build reports "the lance arm is not built" for `lance`; `run.sh`
freezes `target/release/tracebench`, so build with `--features arm-lance`
before queuing `lance` runs.

Commands (the same suites and flags as every arm; timed runs go through
`scripts/tracedb/run.sh <arm> <suite> <runs> <tier>`, which takes the bench
lock and the 3 GiB scope and reads `.scratch/data/base1m.tbe` of the
checkout it runs from):

```bash
ALL="tantivy lance meili qdrant quickwit openobserve redb fjall rocksdb"
for arm in $ALL; do
  scripts/tracedb/run.sh $arm verify 1 1
  scripts/tracedb/run.sh $arm ingest 3 1 --ingest-batches 30000
  scripts/tracedb/run.sh $arm reads  3 1          # includes the purge part
  scripts/tracedb/run.sh $arm size   3 1
done
# search-driven per-turn loop (the key-value arms have no search and skip it):
for arm in tantivy lance meili qdrant quickwit openobserve; do
  scripts/tracedb/run.sh $arm turnloop 3 1 --turnloop-frac 0.03
done
# NON-DURABLE references (ingest + turnloop only, never compared as durable):
for arm in tantivy:commit=periodic lance:fsync=off; do
  scripts/tracedb/run.sh $arm ingest   3 1 --ingest-batches 30000
  scripts/tracedb/run.sh $arm turnloop 3 1 --turnloop-frac 0.03
done
# traditional suite, as for the baselines (server arms reset themselves);
# 1200 s crash wait: Quickwit acknowledges a durable write after ~2 s, so
# 2,000 acknowledgements from 4 writers take ~1,000 s
for arm in $ALL; do
  scripts/tracedb/run.sh $arm ycsb 3 1 --ycsb-ops 10000 --ycsb-cell-seconds 120 --ycsb-crash-wait-s 1200
done
# classic YCSB records: add --ycsb-data synthetic --ycsb-records 1000000 to the ycsb line
# variants: fjall:compression=lz4, redb:quick_repair=on, <kv arm>:cache_mb=N
```

Expected costs on this box (from smoke): Quickwit's forced commit is ~2 s,
so 30,000 ingest batches take ~1 h per run at 16 writers, the 3% turnloop
(~11k turns) ~25 min, and each ycsb write cell runs into
`--ycsb-cell-seconds`; OpenObserve's `finish_load` adds ~10 min to every
`reads`/`size`/`turnloop`/`ycsb` run (settle), and its ycsb open and crash
phases ~10 min each (recovery); Lance runs maintenance inline every 1,000
commits (see its section).

### Validation (smoke tier, untimed, 2026-10-21)

After merging both competitor branches (one `Unsupported` mechanism, one
HTTP client), on the smoke tier (27,482 events incl. revisions, 28.7 MB
raw; local, loaded machine):

| arm | `verify` checks | failures | skipped as unsupported | `size`: disk / raw after load |
|---|---:|---:|---|---:|
| `oojuro` | 1,237 | 0 | - | 2.73 |
| `oojuro:log=private` | 1,237 | 0 | - | 0.39 |
| `tantivy` | 1,237 | 0 | - | 1.08 |
| `lance` (`--features arm-lance`) | 1,237 | 0 | - | 0.75 |
| `meili` | 977 | 0 | agg_session, agg_tools, top_sessions | 11.13 |
| `qdrant` | 977 | 0 | agg_session, agg_tools, top_sessions | 2.11 |
| `quickwit` | 1,084 | 0 | filter1/3, count1/3 (latest status), agg_tools, delete_session, purge_before, reclaim | 1.49 |
| `openobserve` | 1,236 | 0 | delete_session, purge_before, reclaim | 0.78 |
| `redb` | 1,237 | 0 | - | 2.77 |
| `fjall` | 1,237 | 0 | - | 2.07 |
| `rocksdb` | 1,237 | 0 | - | 0.69 |

(`size` is each arm's `disk_bytes` after `finish_load` over the raw bytes;
small stores are dominated by fixed costs such as preallocated logs and
Meilisearch's map size, so these ratios say little about base1m.) `ingest`
(2,000 batches, 400 for Quickwit, 16 writers) ran for every arm. Earlier,
per branch: `ycsb --ycsb-check` passed with 0 failures and a valid SIGKILL
crash test (0 missing acknowledged writes) for all nine arms (search
engines: 3,000 records, workloads A and D; key-value and log engines: the
TRACEBENCH.md smoke flags, OpenObserve with `settle=off`); `turnloop` ran
with 0 freshness failures except `tantivy:commit=periodic` (by design, see
below); `tantivy:commit=periodic`, `lance:fsync=off`, `meili:wait=poll` and
`openobserve:settle=off` also passed `verify`.

### Search and vector engines: common ground (`tantivy`, `lance`, `meili`, `qdrant`)

- **Text routing (indexing rule).** Text of indexed kinds (user, output,
  tool call, tool result) is stored in `text`, the only full-text field;
  thinking and system text goes into `raw`, stored and never indexed. So
  "thinking is stored, not searched" holds without a kind filter at query
  time, and no text is stored twice.
- **`text_len`** (UTF-8 bytes) is stored with each event so engines that
  can sum (Tantivy fast field, Lance SQL) answer `agg_session.text_bytes`
  without reading text; Meilisearch and Qdrant store it too.
- **Query words** as for the baselines (maximal runs of word characters,
  lowercased, deduplicated, at most 64), then each engine's analyser.
- **Excerpts** (`get_range`): none of the four reads part of a field, so
  the arm fetches the event's text and cuts the bytes client-side.
- **Status revisions**: same-batch revisions folded in first (as for the
  baselines); a revision of an earlier call uses the engine's update path
  (per arm below).


### Tantivy 0.27 (`tantivy`, `tantivy:commit=periodic`)

#### Schema

One document per event:

| field | options | used by |
|---|---|---|
| `id`, `session`, `agent`, `kind`, `status` | u64, INDEXED + FAST + STORED | term lookups/deletes, filters, aggregates |
| `ts` | i64, INDEXED + FAST + STORED | range filters, as-of, (ts, id) ordering |
| `seq`, `tokens_in`, `tokens_out`, `duration` | FAST + STORED | ordering, aggregates |
| `turn`, `parent`, `exit_code` | STORED | rows |
| `tool` | STRING (raw token) + FAST + STORED | filters, per-tool aggregates |
| `args`, `raw` | STORED | rows |
| `text` | TEXT: tokenizer `en_stem`, `WithFreqs`, STORED | search |
| `text_len` | u64 FAST | `agg_session` |

- **`en_stem`** is Tantivy's built-in English pipeline: simple tokenizer
  (splits on non-alphanumerics), remove tokens over 40 bytes, lowercase,
  English Snowball stemmer. No stop words (BM25's IDF discounts them).
- **Frequencies, no positions**: BM25 needs term frequencies; there are no
  phrase queries, so positions would only cost space.
- Index settings are the defaults: doc store LZ4, 16 KB blocks, default
  (log) merge policy. Writer: 4 indexing threads, 256 MB arena in total.

#### Operations

| op | how |
|---|---|
| `append` | under the single writer: one `IndexWriter::run` group (for each revision of an earlier call: delete the `id` term, add the call again with the new status; then add the batch), then `commit()` and a reader reload: **one commit per turn batch**, visible (read-your-writes) and durable on return. Concurrent appenders queue on the writer (Tantivy has one writer). |
| `load` | the same, one group and one commit per 256-batch chunk. |
| `get`, `session`, `get_range` | `TermQuery` on `id` / `session`, stored documents; session sorted by `seq`; excerpt cut from the stored text. |
| `time_range`, `filter` | `BooleanQuery` of term and range clauses, top-n by the sort key `(ts, id)` read from fast fields (exact order). |
| `count` | `Count` collector. |
| `search` | `BooleanQuery`: OR (`Should`) of the query's `en_stem` terms on `text`, `Must` zero-scored (`ConstScoreQuery(…, 0)`) clauses for the agent term and `ts <= as_of`; `TopDocs::order_by_score` (BM25, block-max pruning). Score ties within the k are ordered newest first; a tie straddling position k is resolved by Tantivy's doc order. |
| `agg_session`, `agg_tools`, `top_sessions` | matching doc set (`DocSetCollector`), then fast-field columns per segment (tokens, kind, status, duration, `text_len`, `tool` dictionary), summed in the arm: the columnar path Tantivy's own aggregation collectors use. |
| `delete_session`, `purge_before` | `delete_query` (term / `ts < cutoff` range) + `commit()`: invisible and durable on return. |
| `reclaim` | wait for in-flight merges (writer replaced), merge all segments into one (deleted documents dropped, doc store and postings rewritten), reload the reader, `garbage_collect_files()` (every file not referenced by `meta.json` is deleted). |
| `export` | (session, seq, address) of every live doc from fast fields, sorted, then the stored documents in that order. |
| `disk_bytes`, `mem_bytes` | bytes of the index directory; `RssAnon` of the process. |

#### Durability and erasure

- `commit()` writes the new segment's files and `sync_data`s each,
  syncs the directory, then writes `meta.json` atomically (temp file,
  `sync_data`, rename, directory sync). Counted with strace: 1,843
  `fdatasync` for 100 appends (~18 per commit).
- **`tantivy:commit=periodic` (NON-DURABLE, not read-your-writes)**: `append`
  only adds the batch; a background thread commits every `commit_ms`
  (1000) ms. Up to that window of acknowledged turns is lost in a crash,
  and a turn is invisible to readers until the next commit (the turnloop's
  freshness gate fails by design: 171 of 180 smoke turns). Deletes still
  commit at once. Reported only as a non-durable reference.
- The doc store is LZ4-compressed, so the plaintext needle scan cannot see
  stored text either way (0 hits before and after on smoke). Structural
  proof: `reclaim` leaves one segment without deletes and no unreferenced
  file (smoke: 15.7 → 11.5 MB). Stemmed tokens of deleted texts leave the
  term dictionary with the merge.

### LanceDB 0.40 (`lance`, `lance:fsync=off`)

#### Table

Arrow schema, one row per event: `id`, `session`, `parent`, `text_len`
UInt64; `agent`, `turn`, `seq`, `tokens_in`, `tokens_out` UInt32; `kind`,
`status` UInt8; `exit_code` Int32 (nullable); `ts`, `duration` Int64; `tool`,
`args` Utf8; `text`, `raw` Utf8 (nullable, exactly one set). No vector
column: text only.

Indexes, created empty at open (`train(false)`) and filled by maintenance:

- **FTS (inverted) index on `text`** with LanceDB's default analyser:
  `simple` tokenizer, lowercase, ASCII folding, English stemming, English
  stop words, tokens over 40 chars dropped, no positions. Rows not yet in
  the index are still searched (LanceDB scans unindexed fragments at query
  time), so search is complete between maintenance runs.
- BTree on `id`, `session`, `ts`; Bitmap on `agent`, `kind`, `status`,
  `tool` (LanceDB's guidance: BTree for high-cardinality, Bitmap for few
  distinct values).

#### Operations

| op | how |
|---|---|
| `append` | **one commit (one table version) per turn batch**: `add` of the batch, or, when it carries revisions of earlier calls, one `merge_insert` on `id` (`when_matched_update_all`, `when_not_matched_insert_all`) whose source is the batch plus each revised call re-read (`id IN (…)`) with its new status: an upsert, atomic with the batch; the old call row gets a deletion-vector entry. Writes are serialised in the arm (one writer at a time) rather than letting 16 writers retry LanceDB's optimistic commit conflicts. |
| `load` | the same, one commit per 256-batch chunk. |
| `finish_load` | maintenance (below). |
| `get`, `session`, `time_range` | `query().only_if(…)` (scalar indexes prefilter), `order_by` `seq` / `(ts, id)` with `limit`. |
| `get_range` | `select(text, raw)` of the row, cut client-side. |
| `filter`, `count` | `only_if` + `order_by (ts, id)` + `limit`; `count_rows(filter)`. |
| `search` | `full_text_search(FullTextSearchQuery::new(words).with_column("text"))`: BM25, OR over the analysed tokens (match query, operator OR, no fuzziness), prefiltered by `agent = A AND ts <= T`, `limit k`; ties within the k ordered newest first. |
| `agg_session`, `agg_tools`, `top_sessions` | Lance SQL (DataFusion) over the dataset: `count`/`sum` with `CASE` for calls and errors, `GROUP BY tool`, `GROUP BY session ORDER BY sum DESC, session LIMIT n`. |
| `delete_session`, `purge_before` | `delete(predicate)`: one commit writing deletion vectors; durable on return. |
| `reclaim` | compaction with `materialize_deletions_threshold = 0` (every fragment with a deleted row is rewritten from its live rows), every index retrained from live rows (`replace(true)`), then `Prune { older_than: 0, delete_unverified: true }`: every old version and every file it alone referenced is deleted. |
| `export` | (session, seq, row id) of every row, sorted, then `take_row_ids` in chunks of 1,000. |
| `disk_bytes`, `mem_bytes` | bytes of the table directory (data, deletion files, indexes, manifests, transactions); `RssAnon` of the process. |

#### Maintenance

Every commit adds a fragment and a manifest that lists every fragment, so
LanceDB's guidance for small frequent writes is to compact and optimise
regularly. Every `optimize_every` commits (default 1,000; 0 = never) and in
`finish_load`, the writer that crosses the mark runs compaction (default
options: fragments up to 1 Mi rows), index optimisation (unindexed rows
added to every index) and pruning of every old version (`older_than = 0`,
`delete_unverified = true`: safe because only this process writes and every
other operation is held off by a lock while it runs). That time shows up in
the latency of the append that triggers it; `details.maintenance_s` reports
the total. Without pruning, the per-commit manifests of a 30,000-turn
ingest would accumulate as old versions.

#### Durability and erasure

- **Lance's local writes are not fsynced by default**: its direct local
  writer persists a temp file by rename, and `object_store`'s
  `LocalFileSystem` (which handles the manifest put) has fsync off. Counted
  with strace: 0 fsyncs for 100 appends with the default store. The arm
  registers its own provider for the `file` scheme in the LanceDB session:
  Lance's default store with `LocalFileSystem::with_fsync(true)` as the
  inner store and a store prefix that turns off the direct local
  reader/writer, so every data, deletion, index and manifest file is
  fsynced with its directory before the commit returns (1,062 `fsync` for
  100 appends, ~10.6 per commit). Reads then also go through
  `object_store` instead of Lance's direct local reader.
- **`lance:fsync=off` (NON-DURABLE)**: LanceDB's out-of-the-box local store
  (atomic, crash-consistent on process kill, but acknowledged commits can
  be lost on power loss). Reported only as a reference.
- Session caches: 1 GiB index cache and 256 MiB metadata cache (Lance's
  defaults, 6 GiB and 1 GiB, exceed the 3 GiB memory cap of every arm).
- Data files are compressed (plaintext needle scan: 0 hits before and after
  on smoke). Structural proof: after `reclaim` no fragment has a deletion
  file and only the latest version's files exist (smoke: 20.8 → 17.2 MB).

### Meilisearch 1.55 (`meili`, `meili:wait=poll`)

#### Server (`scripts/tracedb/meili.sh`)

| setting | value | why |
|---|---|---|
| `MEILI_ENV`, `MEILI_MASTER_KEY` | production, a fixed local key | production mode (development mode serves the search preview); the key guards a loopback-only server |
| `MEILI_HTTP_ADDR` | 127.0.0.1:56580 | loopback only |
| `MEILI_DB_PATH` | bind-mounted data directory | erasure scan, disk |
| `MEILI_NO_ANALYTICS`, `MEILI_LOG_LEVEL` | true, WARN | |
| max indexing memory / threads | defaults: 2/3 of the cgroup limit (2 GiB of the 3 GiB cap), default threads | Meilisearch reads the cgroup limit itself |
| experimental `tasksStreamingRoute` | on (set by the arm), unless `wait=poll` | waiting for tasks without polling delay |
| everything else | 1.55 defaults (LMDB without `WRITE_MAP`, auto-batching of enqueued tasks) | |

#### Index `events` (primary key `id`)

| setting | value | why |
|---|---|---|
| `searchableAttributes` | `["text"]` | only indexed kinds' text; `raw` is stored, not searchable |
| `displayedAttributes` | `*` | rows come back whole |
| `filterableAttributes` | `agent`, `kind`, `status`, `tool`: equality only; `ts`, `session`, `seq`: equality + comparison; facet search off | the filters TraceBench runs; granular rules (1.14+) so no comparison or facet-search structures are built that nothing uses |
| `sortableAttributes` | `ts`, `id`, `session`, `seq` | (ts, id) order, session order, newest-first ties, export keyset |
| `pagination.maxTotalHits` | 1,000,000,000 | exact counts and whole sessions (default 1,000 caps them) |
| ranking rules, typo tolerance, proximity precision | defaults (`words, typo, proximity, attribute, sort, exactness`; typos on; by word) | the engine's native ranking |

#### Operations

| op | how |
|---|---|
| `append` | one `PUT /indexes/events/documents` (NDJSON, add-or-update): new events as full documents, each revision of an earlier call as a partial document `{id, status, exit_code, duration}` merged into its call; then wait until **that task has succeeded**. The scheduler auto-batches tasks enqueued by all writers into one indexing batch and one LMDB write transaction, whose commit fsyncs: so `append` is durable and visible on return (read-your-writes). Waiting: the experimental `GET /tasks/stream` (server-sent events, one shared connection; a fallback poll every 100 ms catches events from before it connected); `meili:wait=poll` polls `GET /tasks/{uid}` every 5 ms instead (the SDKs' method; their default interval is 50 ms). |
| `load` | the same per 256-batch chunk. |
| `get`, `get_range` | `GET /indexes/events/documents/{id}` (`fields=text,raw` for excerpts, cut client-side). |
| `session`, `time_range`, `filter` | placeholder search (`q = ""`) with `filter` and `sort` (`seq:asc` / `ts:asc, id:asc`), `limit`. |
| `count` | placeholder search in page mode (`hitsPerPage = 0`): exhaustive `totalHits`. |
| `search` | `q` = the query words joined by spaces, `filter: agent = A AND ts <= T`, `sort: [ts:desc, id:desc]` (the `sort` rule, after `attribute`, so it breaks relevance ties newest first), `limit k`, default matching strategy `last`. |
| `agg_session`, `agg_tools`, `top_sessions` | **unsupported**: Meilisearch has facet counts but no sums or group-by over sums. |
| `delete_session`, `purge_before` | `POST /indexes/events/documents/delete` with `filter` (`session = S` / `ts < T`), waited for like an append. |
| `reclaim` | `POST /indexes/events/compact` (index compaction task): the index's LMDB file is copied with compaction (live pages only, so free pages that held deleted documents are not copied) and replaces the original. |
| `export` | keyset pagination: sorted placeholder searches (`session:asc, seq:asc`, 1,000 per page) filtered to after the last (session, seq) seen. |
| `disk_bytes`, `mem_bytes` | bytes of `data.ms` (index, task queue, auth); container cgroup anon + shmem (LMDB pages are file cache and do not count). `details` = `GET /stats` (`databaseSize` and `usedDatabaseSize`). |

#### Caveats

- **Query length**: Meilisearch keeps only the first 10 words of a query
  (`words_limit`, not configurable); the TraceBench query has ~25. With the
  default matching strategy (`last`) documents matching all kept words rank
  first and words are dropped from the end until enough documents match:
  this is not OR over all words. **No stemming** (its tokenizer
  normalises and lowercases; prefix matching applies to the last word
  only), unlike the stemmed BM25 arms.
- An LMDB commit fsyncs the data file; Meilisearch does not use `NOSYNC`
  or `MAPASYNC`. The task queue (also LMDB) records the enqueued task
  before the indexing commit.
- Size: on smoke the index file was 335 MB for 28.7 MB of raw text
  (`usedDatabaseSize` 155 MB, the rest LMDB free pages; 136 MB after
  compaction): word, prefix, position and proximity databases.
- Erasure (smoke purge): 5 of 20 needles in the index file after the
  deletes, 0 after compaction in one run and 1 in another (in the index
  file: a compacting LMDB copy keeps whole live pages, so stale bytes in a
  live page's unused space may survive). Task records keep filters and
  counts, not document text; payload files are deleted once processed.

### Qdrant 1.19 (`qdrant`)

#### Server (`scripts/tracedb/qdrant.sh`)

Environment over the image's `config/config.yaml`: `QDRANT__SERVICE__HOST
= 127.0.0.1`, HTTP port 56590, gRPC port 56591 (unused), telemetry off, log
level WARN, init-file marker in the snapshots directory (non-root user).
Everything else is the 1.19 default: payload on disk (`on_disk_payload`),
WAL in 32 MB segments with one closed segment retained, flush every 5 s,
optimizer defaults (vacuum at 20 % deleted points in segments of at least
1,000), segment count chosen from the CPUs.

#### Collection `events`

- **No vectors.** Created with an empty vector config (`"vectors": {}`):
  every event is a payload-only point (point id = event id). Qdrant is a
  vector database; here it is used purely as a filtered payload store with
  a full-text payload index. BM25 in Qdrant needs sparse vectors (client-
  or server-side inference), which this arm deliberately does not use.
- Payload: the event fields, `text` / `raw` as above, `text_len`.
- Payload indexes: `text`: full-text, `word` tokenizer, lowercase, English
  Snowball stemmer, English stop words (no ranking exists, so stop words
  would only make every document match), no phrase index; `agent`,
  `session`, `kind`, `status`: integer, lookup only; `tool`: keyword; `ts`:
  integer, range only, `is_principal` (the time filter most queries carry).

#### Operations

| op | how |
|---|---|
| `append` | one `POST /collections/events/points/batch?wait=true`: an `upsert` of the batch's points plus a `set_payload` (`status`, `exit_code`, `duration`) per revision of an earlier call. `wait=true` makes Qdrant flush (msync) its WAL before applying the operations and answer after they are applied: durable and visible on return. Qdrant has no multi-operation transactions: each operation of the batch is its own WAL record (a crash between them could keep the events without the status update). |
| `load` | the same per 256-batch chunk (split above 16 MB). |
| `finish_load` | wait until no optimisation is queued or running and the collection is green. |
| `get`, `get_range` | `GET /points/{id}`; `POST /points` with `with_payload: {include: [text, raw]}`, cut client-side. |
| `session` | scroll with `session` match (up to 100,000 points), sorted by `seq` in the arm. |
| `time_range`, `filter` | scroll with the filter and `order_by ts asc`, `limit n`; points tied on the last timestamp are completed by an equality query and ordered by id, so the (ts, id) order is exact. |
| `count` | `POST /points/count` with `exact: true`. |
| `search` | scroll with `must: [agent match, ts <= as_of, text match {text_any: words}]` (OR over the index's tokens), `order_by ts desc`, `limit k`. **No relevance ranking**: a full-text payload condition is a filter, so hits are the k newest matching events. |
| `agg_session`, `agg_tools`, `top_sessions` | **unsupported**: Qdrant has facet counts but no sums or group-by over sums. |
| `delete_session`, `purge_before` | `POST /points/delete?wait=true` with the filter. |
| `reclaim` | Qdrant has no compaction call: the arm lowers the vacuum thresholds to their minimum (`deleted_threshold = 0`, `vacuum_min_vector_number = 100`), waits until the optimizers are idle and the collection green, and restores the defaults. The vacuum optimizer rebuilds every segment with deleted points. |
| `export` | scroll every point's (session, seq), sort, fetch the points by id in chunks of 500. |
| `disk_bytes`, `mem_bytes` | **allocated** bytes of the storage directory (Qdrant preallocates sparse 32 MB page files for payload storage, the text index and WAL segments: on smoke 59 MB allocated vs 867 MB apparent; `details` has both); container cgroup anon + shmem. |

#### Durability and erasure

- `wait=true` → `wal.flush()` (msync of the new WAL range) before the
  update is applied (`update_worker.rs`); without it Qdrant acknowledges
  before the WAL is flushed.
- **The WAL keeps deleted text**: the open WAL segment and one retained
  closed segment (32 MB each) hold the original upserts until they roll
  over, and there is no API to rotate or truncate them. Smoke purge: all 20
  needles still found after `reclaim`, every hit in `wal/open-1`; the
  vacuum optimizer did rebuild the segment (telemetry: 14,800 live points,
  0 deleted). On a large store the window is the last ~32–64 MB of writes.

### Quickwit 0.9.2 (`quickwit`)

Image `quickwit/quickwit:0.9.2` (build 2026-10-16, commit 6a1e3f2), one node
running every service, file-backed metastore and splits on the bind-mounted
`qwdata` (local storage), ingest v2 (the 0.9 default). Node config
(`scripts/tracedb/quickwit.sh`): listen 127.0.0.1:27280 (gRPC 27281; `TRACEDB_QW_PORT` overrides),
searcher `fast_field_cache_capacity` 512M and `split_footer_cache_capacity`
256M (defaults 1G/500M; the indexer heap plus caches must fit the 3 GiB
container), telemetry off, everything else default.

Index `events` (strict doc mapping): `id`, `session`, `agent`, `revises`,
`kind`, `status` u64 fast + indexed; `seq` u64 fast; `ts` i64 fast +
indexed (exact time filters and sorts); `t` datetime (RFC 3339 with µs, fast
precision microseconds) as the **timestamp field**, so time-bounded queries
prune splits; `tok` = tokens in + out (fast, for top_sessions); `tool` text
with the `raw` tokenizer (exact term filter), fast; `args`, `text` stored
only; **`body`** = the text of indexed kinds (user, output, tool_call,
tool_result) with tokenizer `default` (split on non-alphanumerics,
lowercase, drop tokens over 255 bytes; no stemming: `en_stem` is not
registered in this image), `record: freq`, field norms on: BM25. Thinking
and system text is stored in `text`, not indexed. Indexing settings: default
(`commit_timeout_secs` 60, docstore zstd level 8, stable_log merge policy,
maturation 48 h) except `resources.heap_size` 1 GB (default 2 GB, the 3 GiB
container).

- **Revisions**: Quickwit has no update; a status revision is its own
  document (`revises` = call id). Reads that return rows fetch the
  revisions of the calls they return (`terms` on `revises`) and apply the
  newest (largest id) client-side; get = one query (`id` OR `revises`).
- **append** = one NDJSON request to `/api/v1/events/ingest?commit=force`.
  It returns once the split holding the batch is published: durable (the
  local storage writes the split to a temp file, `sync_data`, renames it and
  syncs the directory; the metastore update goes through the same `put`)
  and searchable (read-your-writes; the turnloop freshness gate passes). The
  WAL itself (`mrecordlog`) only flushes to the OS (`PersistAction::Flush`).
  **A forced commit takes ~2.0 s** on this box regardless of size, and
  concurrent forced commits are coalesced (16 concurrent appends all return
  after ~2.0 s), so 16 writers sustain ~8 appends/s. 429/503 answers (shards
  opening or scaling) are retried with backoff for up to 10 min.
- **load** = `commit=auto` requests (at most 8 MiB each); `finish_load`
  waits until every document written is searchable (the 60 s commit
  timeout).
- **search** = Elasticsearch-compatible API: `bool` filter (`agent` term,
  `ts <= as_of`, the `t` range for pruning) + `match` on `body` with the
  query words (OR), sort `_score` desc then `ts` desc, `_source_includes=id`.
- **filter/count** (no status): `bool` filter on `ts`, `t`, `revises = 0` and
  the set fields; `count` = `track_total_hits` (exact). Pages of up to
  10,000 hits (Quickwit's cap) are chained with `search_after`.
- **top_sessions**: `terms` aggregation on `session` (size and shard_size
  65,000, `sum_other_doc_count` must be 0, else an error) with `sum(tok)`,
  ordered on the client.
- **export**: every revision (paged), then every event sorted by
  (`session`, `seq`) with `search_after`.
- **Unsupported**: filter/count on the latest status and `agg_tools` (they
  need the newest revision of every call in the range: Quickwit has no join,
  and resolving it client-side would fetch every candidate document);
  `delete_session` / `purge_before` (the delete API creates asynchronous
  delete tasks that apply only to mature splits, 48 h by default;
  retention drops whole splits on a schedule: nothing is invisible on
  return); `reclaim`.
- **disk_bytes**: allocated bytes of `indexes/` (splits, metastore), `wal/`
  and `queues/` (the ingest WALs are sparse 128 MiB files). The indexer's
  split cache (`indexer-split-cache/`, copies of recent splits kept for
  merges) is excluded and reported in `details` with the published split
  count. **mem_bytes**: container cgroup anon + shmem.
- **ycsb open/crash**: `quickwit.sh halt` / `kill`, then `start` on the same
  `qwdata`; `open` waits until a search succeeds (the REST API answers
  before the searcher joins the cluster; ~2.2 s on smoke).
- Smoke (untimed): turnloop 90 turns, 0 freshness failures, 20 hits per
  search; ycsb `--ycsb-check` 21,446 checks, 0 failures, crash test valid
  (40 acknowledged, 0 missing).

### OpenObserve 1.1.2 (`openobserve`)

Image `openobserve/openobserve:v1.1.2` (2026-10-20), local mode: SQLite
metadata, WAL + memtables, hourly parquet files and tantivy indexes on the
bind-mounted disk, DataFusion SQL. Server settings
(`scripts/tracedb/openobserve.sh`, environment): HTTP 127.0.0.1:27540
(gRPC 27541; `TRACEDB_OO_PORT` overrides), `ZO_WAL_FSYNC_DISABLED=false`
(**durable**: every ingest request fsyncs the WAL before it is acknowledged;
the default `true` only flushes to the OS), `ZO_INGEST_ALLOWED_UPTO=1000000`
hours (the data's clock is 2026-09; the default accepts only the last 5
hours), `ZO_RESULT_CACHE_ENABLED=false` (the result cache treats time ranges
older than `ZO_CACHE_DELAY_SECS` = 300 s as immutable, and every TraceBench
event is older than that, so cached answers would miss later appends),
telemetry and usage reporting off; everything else default (memtable, caches
and DataFusion memory are sized by OpenObserve from the 3 GiB cgroup limit).
Root credentials only guard the loopback, throwaway container
(`TRACEDB_OO_USER` / `TRACEDB_OO_PASSWORD`).

Stream `events` (logs), created with its whole schema (Int64 numbers,
Utf8 strings; queries on an empty stream need it), `full_text_search_keys:
[body]` and `bloom_filter_fields: [id, session, revises]` (OpenObserve adds
them to its secondary index too). Every record carries every field (a
missing exit code is the sentinel -2147483649); `_timestamp` = the event
time, so time-bounded queries prune hourly partitions. `body` = the text of indexed kinds, `text` = thinking and
system text (not a full-text key).

- **Revisions**: no update; a revision is its own record (`revises` = call
  id). Rows (get, session, time_range, export) fold the newest revision
  client-side (as in the Quickwit arm); filters, counts and `agg_tools` on
  the latest status are one SQL statement: `LEFT JOIN (SELECT revises,
  first_value(status ORDER BY id DESC), first_value(exit_code ...),
  first_value(duration ...) ... WHERE revises IN (calls matching the
  filter) GROUP BY revises)` (the ClickHouse arm's argMax pattern).
- **append / load** = `POST /api/default/events/_json` (JSON array, at most
  8 MiB per request); the response must report every record successful.
  Records are searchable from the memtable as soon as the request returns
  (the turnloop freshness gate passes).
- **finish_load settles** (default; `openobserve:settle=off` skips it):
  `PUT /node/flush` persists every memtable to parquet in the WAL directory;
  the node moves those files into storage only once they are
  `ZO_MAX_FILE_RETENTION_TIME` (600 s, default) old, and **flushed files are
  not searchable until they are moved** (measured: a 3,000-record stream
  counted 0 from the flush until the move ~10 min later, then 3,000). So
  `finish_load` waits until no raw WAL segment or WAL-side parquet is left
  and every record written is searchable again: about 10 minutes per load,
  timed as `finish_s`. Without the flush an idle memtable is never persisted
  (15 min observed), so the size suite would measure the raw WAL. Nothing
  reads during `finish_load`; after it, appends go to fresh memtables.
- **search** = `WHERE agent = A AND ts <= as_of AND (match_all('w1') OR
  match_all('w2') ...)` (full-text key `body`). **No relevance function**:
  hits are ordered by the number of query words they contain
  (`str_match_ignore_case`, i.e. case-insensitive substring), then newest
  first, like the ClickHouse arm. `match_all` is a substring match on data
  still in the memtable and a token match once the inverted index exists.
- **filter/count/top_sessions** without status: SQL over the event window
  (partition pruning by `start_time`/`end_time`). Queries that need
  revisions use the whole time range (a revision is later than its call).
- **export**: the newest revision of every call (one grouped query), then
  events in blocks of 256 sessions ordered by (session, seq).
- **Unsupported**: `delete_session` (no delete by query; only whole streams
  or time ranges), `purge_before` (`DELETE .../data_by_time_range` is an
  asynchronous compactor job over whole files: not exact at the cutoff, not
  invisible on return), `reclaim`.
- **disk_bytes**: allocated bytes of `wal/` (raw WAL and memtables persisted
  to parquet), `stream/` (parquet files and indexes in storage) and `db/`
  (metadata); `cache/`, `tmp/` and the bundled GeoIP databases (`mmdb/`) are
  excluded and reported in `details`. **mem_bytes**: container cgroup anon +
  shmem.
- **Recovery after a restart** (ycsb open and crash phases:
  `openobserve.sh halt` / `kill`, then `start` on the same data directory):
  the node turns its WAL into parquet files in the WAL directory, and those
  are **not searchable until it moves them into storage**, once they are
  600 s old. Measured: 3,000 acknowledged records counted 0 for 10 minutes
  after a SIGKILL restart, then 3,000 (nothing lost). Without waiting, the
  ycsb crash test read every acknowledged record as missing. Opening an
  existing store therefore waits until no WAL file is pending and a query
  answers (at most 20 min), so `open_to_first_read_s` and the crash test's
  `reopen_s` include this delay (~10 min each).
- Smoke (untimed): turnloop 90 turns, 0 freshness failures, 20 hits per
  search; ycsb `--ycsb-check` 24,223 checks, 0 failures, crash test valid
  (200 acknowledged, 0 missing, reopen 605 s); before the reopen wait
  existed the same run read all 200 as missing.

### Embedded key-value stores (`redb`, `fjall`, `rocksdb`)

One shared layout (`kv.rs`) over a four-method engine trait, so the three
store the same keys and answer every query with the same plan; only the
engine differs. Keyspaces (a redb table / fjall keyspace / RocksDB column
family each), big-endian keys so byte order is numeric order, `ts` with its
sign bit flipped:

| keyspace | key | value |
|---|---|---|
| `meta` | id | session, agent, turn, seq, parent, kind, latest status, exit code, tokens, ts, duration, text length, tool |
| `body` | id | args length, args, text |
| `sess` | session, seq, id | - |
| `time` | ts, id | - |
| `kind` / `agent` / `status` | value, ts, id | - |
| `tool` | tool length (u16), tool, ts, id | - |

- **Writes.** `append` = one atomic batch across keyspaces = one durable
  commit (see each engine). A status revision is a read-modify-write of the
  call's `meta` (status, exit code, duration) that also moves its `status`
  key, in the same batch as the rest of the turn; `body` is never rewritten
  (update in place, like Postgres/SQLite). Concurrent revisions of one call
  serialise on 4,096 striped locks; nothing else is locked by the arm, so
  the engines' own commit paths run concurrently (RocksDB group-commits;
  redb and fjall serialise commits internally). Same-batch revisions are
  folded into the call (as in every arm). Bulk-load chunks are written
  without a sync; `finish_load` makes them durable and settles (below).
- **Reads.** `get` = two point reads; `session` / `time_range` = an index
  range, then point reads. `filter`/`count` pick the narrowest index range
  (tool, agent, a call status, kind, the status `none`, time) and check the
  remaining conditions on `meta`; when the range itself covers every
  condition (e.g. kind + time, status + time) `count` counts keys only.
  `agg_tools` scans the `kind = tool_call` range, `top_sessions` the time
  range, both over `meta`. `export` walks `sess` in key order = (session,
  seq). Every read runs on one snapshot (redb read transaction, fjall /
  RocksDB snapshot).
- **No full-text search** (by design of these arms): `search` returns
  `Unsupported`; `turnloop` is skipped. They exist for `ycsb`, `ingest`,
  `reads` and `size`.
- **Memory** (`mem_bytes`): `RssAnon` of the runner process (engine caches
  and memtables are heap). Block/page cache 256 MiB in all three
  (`cache_mb=N` changes it; redb's own default is 1 GiB).
- **Disk** (`disk_bytes`): allocated bytes (`du`, st_blocks x 512) of every
  file in the store directory, journals/WAL included; `details` also gives
  the logical size and per-keyspace numbers. Allocated rather than logical
  because fjall's journals are sparse 64 MiB `set_len` files (logical size
  64 MiB from the first byte).
- Unit tests (`cargo test -p oojuro-tracebench kv`) run each engine against
  the owning oracle: load, appends with revisions (also same-batch),
  filters on every index, aggregates, export, delete, purge, reclaim, reopen.
- Smoke (untimed): `ycsb --ycsb-check` ~32,760 checks, 0 failures and a
  valid crash test (500 acknowledged, 0 missing) on each; `ingest` (8,975
  batches, 16 writers), `size` and `purge` ran.

#### redb 4.4.0 (`redb`)

- Copy-on-write B-trees in one file, one writer at a time (a second
  `begin_write` waits).
- **Durability**: each `append` is a write transaction committed with
  `Durability::Immediate` (redb's default 1-phase commit: data and the
  flipped commit slot are written, then one fsync; checksums pick the valid
  slot after a crash). Concurrent appenders queue on the single writer, one
  fsync each (no group commit). Bulk-load chunks commit with
  `Durability::None`; `finish_load` commits an empty `Immediate`
  transaction, which persists them.
- `quick_repair` off (redb's default): reopening after a crash runs a full
  repair (walks the tree, rebuilds the allocator state), which is what the
  ycsb crash test times. `redb:quick_repair=on` saves the allocator state
  every commit (2-phase commit, slower commits, near-instant recovery).
- `reclaim` = `Database::compact()` until it reports no progress (exclusive
  access; the arm blocks reads and writes meanwhile). Smoke purge: 20 needle
  hits before reclaim, 0 after (text is stored uncompressed, so the
  plaintext scan is a real test).
- No compression: the size suite will show redb's free/fragmented pages
  (`details`: `stored_bytes`, `fragmented_bytes` per table).

#### fjall 3.1.12 (`fjall`)

- LSM-tree per keyspace, one shared journal, fjall's defaults: leveled
  compaction, bloom filters, 64 MiB memtables, 4 KiB blocks, **compression
  none in L0 and L1, LZ4 from L2** (`fjall:compression=lz4` = LZ4 at every
  level, like the `rocksdb` arm).
- **Durability**: each `append` is a `WriteBatch` (atomic across keyspaces)
  committed with `durability(Some(PersistMode::SyncAll))`: journal written
  and fsynced (data and metadata) before the commit returns. The journal
  writer is a mutex: concurrent appenders take turns, one fsync each.
  `finish_load` persists the journal (`SyncAll`), flushes every memtable
  (`rotate_memtable_and_wait`) and waits until flushes and compactions are
  idle (at most 15 min).
- `reclaim` = flush every memtable, `major_compact` every keyspace, persist.
  **Erasure gap**: fjall rotates its journal only once it passes 64 MB and
  offers no call to rotate it on demand, so deleted text stays in the active
  journal until enough later writes push it out. Smoke purge: 38 needle hits
  before and after reclaim (all in the journal). `reclaim` is reported (the
  tables are rewritten) and the needle scan shows the leftover; read it as
  a failure of the erasure requirement.

#### RocksDB 11.8.1 (`rocksdb`, rust-rocksdb crate `rocksdb` 0.25.0, bundled)

- Built from source by `librocksdb-sys` (bindgen needs libclang: on the
  local box `LIBCLANG_PATH` points at a `libclang.so` symlink to
  `/usr/lib/x86_64-linux-gnu/libclang-21.so.21`, `BINDGEN_EXTRA_CLANG_ARGS`
  at zig's clang headers; C/C++ through the zig `cc`/`c++` shims; no other
  patches). Only the `lz4` compression feature is compiled in.
- Options (the rest RocksDB defaults: leveled compaction with dynamic level
  bytes, 64 MiB write buffers, 2 background jobs): LZ4 at every level, one
  LRU block cache of 256 MiB shared by the column families, full bloom
  filters at 10 bits/key, `db_write_buffer_size` 256 MiB across all
  memtables.
- **Durability**: each `append` is a `WriteBatch` (atomic across column
  families) written with `WriteOptions::set_sync(true)`: the WAL is synced
  before the write returns; concurrent writers are group-committed (one WAL
  sync can cover several writers). `finish_load` syncs the WAL, flushes
  every column family and waits until flushes and compactions are idle (at
  most 15 min).
- `reclaim` = flush every column family, then `compact_range` over the
  whole key space of each with `BottommostLevelCompaction::Force` (every SST
  rewritten without the deleted keys); WALs older than the flush are
  obsolete and deleted by RocksDB. Smoke purge: 0 needle hits before and
  after (LZ4 SSTs, and the WAL holding the inserts was already gone after
  `finish_load`'s flush): the structural proof is the forced rewrite.

### Competitor caveats and fairness notes


- **Outside their sweet spot.** Meilisearch is built for short,
  typo-tolerant, user-typed queries over product-sized documents; TraceBench
  queries are 200-byte excerpts of tool output over multi-KB texts.
  Qdrant is a vector database used here without vectors. LanceDB targets
  batch/columnar and vector workloads; one durable commit per turn means a
  fragment and a manifest per turn and periodic compaction. Tantivy is the
  closest fit (an embedded BM25 library) but has a single writer and no
  read-your-writes without a commit.
- **Ranking is not comparable across these arms** (as for the baselines,
  `verify` does not compare search results): BM25 (Tantivy, Lance; Tantivy
  without stop words, Lance with), ranking rules on 10 words (Meilisearch),
  none (Qdrant: newest matches), BM25 without stemming (Quickwit's
  `default` tokenizer; Postgres and SQLite stem), matched-word count with
  substring semantics on unflushed data (OpenObserve, like ClickHouse), and
  none at all for the key-value arms (no search). Retrieval quality belongs
  to the shared quality harness, not these latencies.
- **Aggregates** exist only where the engine computes them (Tantivy fast
  fields, Lance SQL); the `reads` mix of Meilisearch and Qdrant is
  `partial_mix` (without `agg_session`), so its throughput is not directly
  comparable to the full mix.
- **Server vs embedded**: Meilisearch and Qdrant pay an HTTP + JSON round
  trip per operation (as Postgres/ClickHouse pay theirs); Tantivy and Lance
  run in the benchmark process, and their memory is the process's
  `RssAnon` (which includes the runner).
- **Write concurrency**: Tantivy (single writer) and the Lance arm
  (serialised commits) queue the 16 writers; Meilisearch batches concurrent
  tasks into one commit (group commit by design); Qdrant applies writes
  through its update queue. Durability per arm is as stated above; the two
  NON-DURABLE variants exist only as references.
- **Lance maintenance** runs inline in the append that crosses the
  1,000-commit mark (all other operations wait): expect a p99/max spike per
  1,000 turns and read `details.maintenance_s`.
- **Indicative smoke numbers** (local, loaded machine, untimed, smoke tier
  = 20,043 events, 28.7 MB raw): disk after load Tantivy 16.4 MB, Lance
  21.9 MB, Meilisearch 335 MB (155 MB used), Qdrant 59 MB allocated.
- **Durability of the log and key-value arms is equal in kind, not in
  mechanism.** Each acknowledges an append after an fsync covering it: redb
  one fsync per commit (single writer), fjall one journal fsync per commit (journal mutex), RocksDB a WAL
  sync with group commit, OpenObserve a WAL fsync per request
  (`ZO_WAL_FSYNC_DISABLED=false`, not its default), Quickwit a published,
  fsynced split per `commit=force` (its WAL is not fsynced; the split is).
  Only RocksDB merges concurrent writers' syncs; the others serialise.
- **Quickwit's ~2 s forced commit** dominates its ingest, turnloop and ycsb
  write numbers; `commit=auto` would ingest far faster but is neither
  durable on return nor searchable on return, so it is not offered.
- **Update model.** The KV arms update a call's status in place (read-
  modify-write of ~70 bytes); the log engines append a revision document
  and fold it at read time (client-side for rows), like ClickHouse's
  `call_status`.
- **Compression**: redb none; fjall none in L0/L1 then LZ4 (its default);
  RocksDB LZ4 everywhere; Quickwit zstd doc store; OpenObserve parquet
  (zstd). Sizes after `finish_load` are the settled state of each (LSMs
  flushed and compacted, OpenObserve moved to parquet storage, Quickwit's
  splits merged only as far as its merge policy got).
- **Caches**: the KV arms get 256 MiB of block/page cache each; the docker
  arms keep their own sizing inside the 3 GiB container (Quickwit's
  searcher caches reduced to fit); memory is RssAnon for embedded arms and
  cgroup anon + shmem for docker arms, as for the baselines.
- **OpenObserve's settle and recovery waits** are its file-move cadence
  (`ZO_MAX_FILE_RETENTION_TIME` = 600 s, left at the default); a shorter
  setting would shorten both and make smaller files.
- **Unsupported is not slow**: an `unsupported` label means the engine
  cannot meet the contract for that operation (no join, no synchronous
  delete, no full-text index), not that it was too slow to run.
- **fjall erasure**: `reclaim` rewrites its tables, but deleted text stays
  in the active journal until 64 MB of later writes rotate it; the needle
  scan reports it.
- **Real-data YCSB records are tool calls with real text**; the KV arms
  store and index them exactly like trace events (five secondary keys per
  record), which a YCSB key-value table would not have; synthetic records
  keep the payload in unindexed `args` but still carry the index keys.

## Known costs and caveats of the baselines (measured while building the arms)

Local, shared, overloaded machine (load 3–55 during these runs): indicative
only; the published numbers come from the dedicated bench box.

- **Postgres search**: every OR match is ranked; a typical base1m query
  matches ~26k of one agent's rows (224 ms single-threaded with `ts_rank`,
  1.4 s with `ts_rank_cd`). In the turnloop (16 agents) search is 132 ms p50,
  the whole turn 129 ms p50, 118 turns/s; append 1.4 ms p50 (2 local runs).
- **ClickHouse search** scores hits by re-reading their text (no relevance
  function): ~1.05 s per query on base1m when idle versus 0.08–0.15 s for the
  index-only match; ~85 % of turns search. A point read costs 9–12 ms (one
  8,192-row granule of `text`), and a durable append waits for the async
  insert flush (~275 ms p50 at 16 writers). Projected from the smoke tier and
  these costs, a full ClickHouse `turnloop` (36,756 turns, 30,905 searches)
  takes well over 40 minutes on this machine; budget for it.
- **ClickHouse deletes**: see `delete_session` (patch parts ~60 ms versus
  ~35 s per session for the default mutation-based mode).
- **ClickHouse `background_pool_size`** is auto-sized to 3 in the 3 GiB
  container; merges after the bulk load and the reclaim mutations queue on it.
- **`args`** is `json` in Postgres (exact bytes) instead of `jsonb` (would
  normalise and fail the byte-exact contract).
- **tsvector prefix**: Postgres indexes the first 131,072 characters of a
  text (0.003 % of base1m events are longer); SQLite and ClickHouse index all.
