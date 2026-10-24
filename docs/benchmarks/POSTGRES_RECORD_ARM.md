# Optional PostgreSQL record baseline

Source implementation only: **uncompiled, unrun and not a performance result**.
The existing recorded native/SQLite/JSONL+Zstd numbers remain unchanged. This
work does not establish a SOTA result or hosted latency/cost improvement.

## Scope and dependencies

Linux benchmark only: build oojuro-bench explicitly with --no-default-features
--features postgres-arm. This adds the already locked postgres0.19 client and
libc process-control primitives only to this optional comparison. Normal engine,
CLI and benchmark default selections do not select this arm. No crypto, database
format, production indexing/ranking or cloud/provider implementation changes.

The benchmark owns a foreground PostgreSQL process, private0700 cluster/socket,
initdb/server logs and prepared statements. Specify pg_bin and optional pg_lib
in Open configuration, or the equivalent task-child environment variables
OOJURO_BENCH_POSTGRES_BIN and OOJURO_BENCH_POSTGRES_LIB. The qualification
controller must hash-pin binaries, compatibility libraries and data/settings.
There is no external DSN or unaccounted shared server. Socket path must be short.

Prepared17.11 Debian binaries are a fixed baseline, not a claim of latest
PostgreSQL. initdb uses UTF8/C locale, a private trust-auth Unix socket and no TCP.
Postgres runs fsync/synchronous_commit/full_page_writes on, shared_buffers32MB,
work_mem4MB, maintenance_work_mem32MB,10connections, autovacuum on and normal WAL.
Actual version/settings, postmaster/backend PIDs and arm/postmaster cgroups are
returned in stats. The controller must additionally retain process start ticks,
actual full cgroup membership/caps and memory.events.

## Equivalent durability and original evidence

A successful AppendBatch follows one successful transaction COMMIT. A failed
batch rolls back; a lost reply is uncertain, not an ACK. IDs, original text,
effective revision keys and original key fields use BYTEA so NUL round-trips.
Metadata is JSON text and malformed rows fail hard. Indexing alone maps NUL to
space, uses explicit English tsvector with GIN, and refuses oversized vector
errors rather than truncating text. ts_rank_cd differs from Oojuro/SQLite BM25;
report retrieval quality and returned original bytes alongside latency.

Search ORs distinct alphanumeric tokens through parameterized plainto_tsquery.
The latest visible (valid_at,recorded_at,seq) winner is checked against ALL rows
of the effective key, including newer nonmatching rows. Equal clocks use insertion
sequence, ties use original ID bytes, and stopword-only/empty/k0 queries return no
hits. Get preserves requested order/cardinality/missing IDs; range reads preserve
full original keys/metadata and valid-time/insertion order.

## Whole-process ownership and accounting

ProcessArm now retains the actual generated scope InvocationID+ControlGroup.
Kill signals every process in that exact scope, requires hierarchical populated0,
and then reaps the owned leader. Normal EOF/finish is bounded and requires empty
scope. Drop performs bounded cleanup. Failed control-plane reads/kills are NOT a
proof of cleanup; the independent outer controller must reconcile the exact unit
and retained PIDs before recovery, inventory or deleting scratch. Before Hello /
scope capture fails there is no captured authority yet; outer inventory-before-
launch/exact-unit reconciliation remains necessary. PostgreSQL starts only at Open.

PostgreSQL children establish separate process groups; killing only the arm or
postmaster group is insufficient. GroupChild reserves its leader unreaped until
its last signal, avoiding post-reap PID/PGID reuse. Normal SIGINT shutdown relies
on the postmaster stopping its own children. Failed startup/close/deadline or
crash must kill the ENTIRE arm cgroup; no process-group-only cleanup claim.
See [PostgreSQL17 source](https://raw.githubusercontent.com/postgres/postgres/REL_17_STABLE/src/backend/postmaster/postmaster.c)
and [shutdown guidance](https://www.postgresql.org/docs/17/server-shutdown.html).

All isolated arms on this topic use finite600s scope runtime,512tasks,0swap,5s
stop, plus explicit requested MemoryMax. The outer build/test/measurement unit
must independently cap runtime, memory/tasks, core dumps and stop time. Existing
strict launchers that pin the old argv must be updated and reviewed; do not run
this binary through an unchanged old12-argument shim.

Use whole-cgroup current/peak/anonymous/file counters for ALL arms. Client RSS/HWM
is explicitly not total PostgreSQL memory; never sum shared process RSS. Bootstrap
is included in main RAM/disk totals and separately identified, not subtracted to
improve ratios. Closed inventory now fails on errors/symlinks/cross-device/special
or hard-linked files, counts every regular file and directory block including WAL,
GIN/btree/catalogs/logs/temp, and records a closed-inventory.json sidecar outside
the store. This pathname footprint excludes shared filesystem inode/journal cost.
Allocation totals therefore have a stricter definition than earlier reports;
remeasure all compared arms under the same new definition before ratios.

## Qualification still required before timing

1. Peer review exact source; freeze optional/default dependency graphs and source.
2. One canonical bounded build; exact parser/schema, fullDoc/NUL/metadata/clock/
   winner/transaction/reopen fixture, partial-cluster refusal and checked-inventory
   refusal. One real different-session child fixture must prove whole-scope kill.
3. Actual whole-server/cgroup SIGKILL at0/5/30ms; every ACKed original is exact and
   searchable, the in-flight batch0orALL, no false ACK. Prove old cgroup/PIDs absent
   before reopen. Retain failed attempts; no favorable rerun.
4. Complete quiescent cluster copy, fresh owned server/socket reopen, fullDoc,
   exact originals, score-bit/query determinism, settings and complete inventory.
5. Review one fixed matched dynamic schedule against the same native/SQLite/
   JSONL+Zstd inputs/queries/durability/output quality, with all whole-cgroup RAM,
   complete storage and all startup/build overhead disclosed. No cold, concurrent,
   larger-than-RAM, production-provider or cost claim from a warm single-client run.

Meaningful ignored fixtures are explicit installed-server/systemd checks and must
never auto-start databases in a normal cargo test. No DB or test has been started
as part of this source-only implementation.
