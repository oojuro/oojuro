# Shared write log

One durable log under many per-writer stores: commits of different stores
share one group commit and one `fdatasync`, while each store keeps its own
index, segments, compressed runs and manifest, and stays its own erasure
domain. Code: `crates/oojuro-nine/src/shared.rs` (the log),
`crates/oojuro-nine/src/v4.rs` (`Options::shared_log`, `submit_shared`),
`crates/oojuro-trace/src/lib.rs` (`Options::shared_log`). Off by default;
TraceBench: `--arm oojuro:log=shared`.

## Why

Tier-1 ingest, 16 writers, one durable commit per turn batch:

| | commits/s | p50 | p99 |
|---|---:|---:|---:|
| Postgres 17 (`synchronous_commit` on) | 10,513 | 0.9 ms | 2.6 ms |
| Oojuro (a journal per agent) | 3,962 | 2.4 ms | 8.2 ms |

Each of the 16 agent stores syncs its own journal. Two things cost:

1. **One sync per store per round.** Postgres has one WAL; one flush
   releases every waiting committer.
2. **Appending syncs.** A journal that grows makes each `fdatasync` also
   commit the new file size through the ext4 journal (a second device flush
   on this DRAM-less NVMe). Postgres overwrites preallocated WAL segments, so
   its `fdatasync` flushes data only. The track's T7 probe (one shared,
   appending file: 1,461 commits/s at 16 writers, slower than 16 files) shows
   coalescing alone does not win on this disk. The shared log does both:
   one sync per round, into zero-filled space.

## Format

Directory (`<db>/shared-log/` under TraceDb): `LOCK`, `LOGID` (16-byte log
identity + CRC), `log-G.nine` (generation G; the highest is active),
`spare-*.nine` (zero-filled, prepared in the background), `log-G.dead`
(being zeroed by a scrub).

- File header, 48 B: magic `NINESL01`, salt u64, generation u64, log id
  (16 B), reserved, CRC32.
- Frame header, 48 B: payload length u32, flags (MORE), seq u64 (per
  file), **round** u64 (seq of the round's first frame), **store** u64,
  **first_row** u64, payload CRC32, header CRC32 over (salt, header).
- Payload: LEB128-length-prefixed entries, one encoded record per row (the
  same entry bytes as the private journal).

A **batch** is one store commit: its frames are contiguous, all but the
last flagged MORE, and recovery delivers it only when the last frame is
valid (all-or-nothing). A **round** is the set of batches one leader writes
with one `pwrite` and one `sync_data`; the next round is written only after
that sync returns.

## Write path

- `enqueue`: the caller encodes and CRCs its frames, pushes a ticket, and
  the first ticket of an idle queue becomes leader.
- Leader: takes every queued ticket, rotates if the file is full, fills in
  seq/round/header CRC, writes once, syncs once, completes every ticket,
  then **hands leadership** to the oldest waiting ticket (no thread leads
  more than one round, so none starves under sustained load).
- Per store, rows enter the log contiguously and in order (checked per
  ticket; a gap is refused, not written).
- Pipelining (`Nine::submit_shared`): under the store's writer lock a batch
  takes its rows after every batch already queued, its ids are checked
  against batches in flight and then against the state, and it is queued.
  The lock is released during the sync, so a store's next batch can join the
  same or the next round. Durable batches are applied in row order under the
  writer lock (no flush or merge swap in between) and are searchable before
  the call returns. A failed batch fails every later one of that store, which
  is then poisoned until reopen (as for the private journal).
- Preallocation: a background thread zero-fills and syncs a spare file the
  size of a log file (32 MiB). Rotation writes the header over the spare's
  zeros, syncs it, renames it into `log-G.nine` and syncs the directory.
  Rounds then overwrite zeros, so a round's sync never changes the file size.
  Without a ready spare the log keeps appending (correct, slower) and
  switches at the next round once one is ready.

## Recovery

`SharedLog::open` reads every `log-G.nine` in generation order:

- **Sealed files** (all but the newest) were complete and synced when they
  were sealed. Every frame must parse; after the last complete batch only
  zeros (never-used preallocated space) may follow. Anything else fails the
  open.
- **The newest file** may end in a torn round. At the first frame that
  fails a check, the rest of the file (up to its last non-zero byte) is
  searched for valid frames (salted header CRC, seq at least the expected
  one, payload CRC). A valid frame of a **later round** proves the damaged
  round was synced, so this is damage to committed history: opening fails
  and nothing is modified. Frames of the damaged round itself are allowed,
  because inside preallocated space a torn write can persist later pages
  before earlier ones. Then the file is truncated to the end of the last
  complete batch.
- A newest file shorter than its header (crash inside create) is removed;
  `spare-*` files are removed; `.dead` files are zeroed and removed.
- Every open writes into a new file; recovered files stay until absorbed.

Stores replay at open (`attach`): the store passes the rows its manifest
already covers, and the log returns the entries of every later row of that
store in order. A gap, an overlap, a batch that fails its CRC on reread,
or a store manifest behind rows the log already retired fails the open.
Bindings fail closed. The manifest records `{log id, store id}`. A bound
store opens only with that log and id, a private store cannot join a log,
an id cannot be attached twice, and a new store cannot take an id the log
still holds rows for. Store creation syncs the parent directory before the
first commit.

## Truncation

After each flush a store reports `absorbed(rows)` (its manifest durably
covers them). A sealed file is removed once every store with frames in it
has absorbed them; batch locations of removed files are dropped. A store
that writes rarely pins old files. TraceDb runs a checkpointer: when the
log exceeds `log_max_bytes` (256 MiB), it flushes the stores returned by
`pinning()` (unabsorbed rows in the oldest sealed file). `TraceDb::flush`
and `SharedLog::checkpoint` rotate and remove everything once all rows are
absorbed.

## Erasure

Erased text can sit in the log after its store has erased its segments,
because other stores' unabsorbed rows keep the file alive. `TraceDb::reclaim`
therefore, after erasing each affected store's segments:

1. flushes every store (every row absorbed; appends wait on the reclaim
   lock);
2. `SharedLog::scrub()`: refuses (changes nothing) if any frame is not
   absorbed; otherwise rotates to a new file, renames every other log file
   to `.dead`, syncs the directory, overwrites each with zeros, syncs it and
   removes it. Untracked leftovers (failed removals, older `.dead`) are
   renamed to `.dead` first and then zeroed and removed too, so a crash
   mid-zeroing never leaves a `log-G.nine` with a wiped header; the next
   open finishes any `.dead` file.

Proof: the trace reopen/reclaim gate runs in both modes and byte-scans
every file of the database for deleted text after reclaim.
`shared_log_reclaim_erases_text_kept_in_the_log_by_other_agents` first
proves the text is in the log, then that no file holds it after reclaim.
`erasure_scrubs_the_shared_log_once_every_store_absorbs` does the same at
store level, including the refusal while another store has unabsorbed rows.
The guarantee is per file, the same as the private journal, whose retired
files and old segments are unlinked without zeroing. Zeroing is
best-effort against the device; flash may keep remapped blocks.

Rejected alternative: per-store keys with key destruction. Erasing rows
without destroying the whole store would need key rotation per erase plus
absorbing that store's frames under the old key, and a homemade stream
cipher (no crypto crate is vendored). The cost of scrubbing is one flush of
every store at reclaim time, which is rare.

## Options

- Nine: `Options::shared_log = Some(LogBinding { log, store })`; not with
  `deferred_sync`. `snapshot` of a bound store flushes it and writes a
  standalone copy with a private journal.
- TraceDb: `shared_log`, `log_segment_bytes` (32 MiB), `log_preallocate`
  (true), `log_max_bytes` (256 MiB); requires `erase_segments` (no rewrite
  reclaim) and no `deferred_sync`.
- TraceBench arm: `oojuro:log=shared`, plus `log_mb=32`,
  `log_prealloc=on|off`, `log_max_mb=256`. `log=private` or no `log` key
  gives the unchanged default. Ingest records gain `sync_counters`
  (`journal_syncs`, `shared_log_rounds`, `shared_log_batches`).

## Evidence (untimed; the machine was busy)

`examples/shared_log_bench.rs`, 16 writers × 200 commits of 3 records, on
tmpfs (no disk I/O; sync counts are exact, throughput is not meaningful):

| setup | journal syncs per commit | batches per round |
|---|---:|---:|
| private journals, any latency | 1.000 | 1 |
| shared, syncs free (tmpfs: nothing to wait for, so little to coalesce) | 0.87 | 1.1 |
| shared, 1 ms simulated sync, writer bound to store | 0.094 | 10.6 |
| shared, 1 ms simulated sync, writers spread over stores (TraceBench shape), pipelined | 0.102 | 9.8 |
| same, before per-store pipelining | 0.164 | 5.4 |

Latency is about two rounds (a batch arriving during a round waits for it,
then for its own). So throughput ≈ batches per round / round time, and the
round time on this disk is what the timed run must measure.

## Open risks

- The win depends on the data-only sync being much cheaper than the
  appending sync on this ext4 + DRAM-less NVMe (the Postgres numbers say it
  is). Compare `log_prealloc=off` to measure it. If that is the main win,
  preallocating the private journals is a cheaper alternative.
- Zero-filling a 32 MiB spare is background write traffic (one extra copy
  of the log volume) and its sync can delay a round's flush (p99).
- A store that never flushes pins log files until the checkpointer's
  budget forces a flush; one log failure poisons every store on it until
  the database is reopened.
