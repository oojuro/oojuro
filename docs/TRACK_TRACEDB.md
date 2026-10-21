# Trace-DB track log (`track/tracedb`)

Question: how far can a pure information-theoretic Rust engine (compression
near entropy + code-length ranking, no LLMs) go as the database for agent
traces, and can it replace Postgres for them? Answered by measurement on
TraceBench (`docs/TRACEBENCH.md`), not argument. No architecture decision
here: every design below is an experiment until one is approved.

Branch `track/tracedb` from main `34c8620` (entropy-NaN hotfix), with
`origin/track/lexical` (v4 engine, `bb615e8`) and `origin/track/compress`
(`oojuro-store`, `f009961`) merged in. Design decisions 2026-10-20: per-turn
loop (top-20 search before each turn, every tool call searchable next turn);
hot per-agent store ≤ ~2 GB plus a shared 10–100 GB+ archive; one
group-commit sync per finished turn; store-only thinking.

## Status

Milestone 1 in progress: spec, data and harness done; Oojuro v0 passes the
oracle at tier 1; baselines and timed runs running.

## Experiment log

Each entry: hypothesis, change, delta, decision.

**T0 — TraceBench v1 (kept).** 1,000,071 original events (+318,811 status
revisions) from four public sources, 16 agents over 30 days, turn batches as
the unit of durability; tiers by replication with remapped ids. The
`TraceStore` contract has 18 operations; an in-memory oracle defines every
deterministic answer (`tracebench verify`, 1,237 checks per arm).

**T1 — oojuro-trace v0: composition (kept as the first arm).** One v4 engine
per agent (group-commit journal, fresh + mmapped segments, code-length
search scoped to the agent by construction), originals in the compression
track's run files behind v4's `Archive` boundary, typed fields in record
metadata mirrored into an in-memory column table with 4,096-event time zone
maps, status revisions as unindexed records keyed `r<call>`, deletes as a
synced tombstone log, `reclaim` = rewrite each affected agent store with live
records and remove the old files. One v4 change: `insert_batch_rows` returns
the first row of a committed batch and `record_at(row)` reads it back without
an id lookup (rows are stable under merges). Oracle: 1,237/1,237 on the
27k-event smoke file and on tier 1, including after deletes, purge and
reclaim.

**T2 — bulk path: one commit per agent per load chunk (kept).** Hypothesis:
the bulk load is fsync-bound when every turn is its own commit. Change:
`append_many` groups a chunk's 256 turn batches by agent into one atomic
journal batch (the analogue of COPY in one transaction). Smoke: load
10.2 s → 1.5 s (6.8×); tier 1: 104 s for 1.32M events (12.1k events/s at
load average ~40–60; untimed run).

**T3 — dictionary only for merged runs (kept).** Hypothesis: training a zstd
dictionary per 16 MiB flush stalls the agent's writer. Smoke, end-of-load
flush of 16 agents: 4.9 s with a dictionary per run, 1.5 s without (1.1 s
also without dedupe), so ~0.2 s per flush under the writer lock. Flushed runs
(zstd 3) now have no dictionary; merged runs (≥ 48 MiB, zstd 19) train one.
Storage effect at tier 1: pending.

**D1 — typed fields carry ~22 bits per event (measured).** Empirical
conditional entropies on tier 1 (`scripts/tracedb/entropy.py`): kind given
the previous kind 0.66 bits, tool given kind 2.56, status 0.26, exit code
given status 0.001, parent delta 0.67, ts delta class 1.64 (+ mantissa),
turn delta 0.82, session switch 0.10, revision duration class 3.39. Upper
bound for all typed fields: 22.2 bits/event = 3.7 MB for 1.32M events, 0.4%
of the 934 MB of text. Caveat: timestamps and token counts are synthetic and
low-entropy by construction; real clocks would add ~10–20 bits/event.

**T4 — sealed column blocks (kept, v1).** Hypothesis: the typed columns are
far below their 64 B/event in-memory width. Measured first
(`examples/colblock_exp.rs`, tier 1, per-field LEB128 zigzag-delta streams,
zstd over each block): 4,096-event blocks 7.2 B/event at zstd 1, 7.0 at
zstd 3, 6.1 at zstd 19; 65,536-event blocks 6.0; plain varints 17.3. The
bound from D1 is ~2.8 B/event, so sealed blocks sit at 2.1–2.5× the
information content and 9–10× below the plain table. Change: `ColStore`
seals a block once all its ids are present and the stream is two blocks
past it (status revisions land first; late ones go to an override map);
liveness is a bitset outside the compressed bytes, so deletes never thaw a
block; each sealed block carries per-tool and per-session aggregates so
whole-block windows skip decoding; a CRC'd checkpoint (`cols.ckpt`) plus
per-agent row watermarks replaces the decode-everything rebuild on open.
Gates: oracle 1,237/1,237 (sealed and plain), and a reopen test (checkpoint
+ replay, tombstone replay, reclaim with a byte scan, missing and damaged
checkpoint → rebuild) passes. Timed effect on RSS/latency: queued
(`oojuro` vs `oojuro:cols=hot`).

**T5 — crypto-shredding per session (outside the obvious line; measured,
not adopted).** Idea: encrypt each session under its own key so deleting it
= destroying the key (instant, provable, no rewrite). Cost: no frame,
dedupe chunk or content-trained dictionary may span sessions. Sessions split
in halves by hash; H1 trains a "public" dictionary, everything is measured
on H2 (8,944 sessions, 458 MB; `examples/shred_exp.rs`, +28 B per frame for
nonce and tag):

| layout | bytes/raw | vs shared |
|---|---:|---:|
| shared run, session order (today's archive: zstd 19, 64 KiB, dedupe, own dictionary) | 0.0910 | — |
| shared run, append order | 0.0935 | +3% |
| per-session 64 KiB frames, no dictionary | 0.1813 | +99% |
| per-session 64 KiB frames + public dictionary | 0.1098 | +21% |
| one frame per session + public dictionary | 0.1026 | +13% |

Reading: erasure by key destruction costs 13–21% more bytes if a
dictionary trained on non-erasable data is allowed; without it, 2×. Rewriting
on reclaim costs nothing in bytes but O(store) I/O per purge. A product
decision (privacy guarantee vs bytes), not ours: listed as an open decision.

**T6 — search over compressed frames (the "index the compressed data"
question; measured).** One agent (42,751 indexed events, 39.4 MB, 610 frames
of 64 KiB), its last 2,000 turn-loop queries (`examples/sketch_exp.rs`):

| index | size | of raw | frames to decode per query |
|---|---:|---:|---:|
| docid postings, information bound (Σ df·(2+log2 N/df)) | 2.14 MB | 5.4% | 0 (excerpts only) |
| frame-id postings, information bound | 0.34 MB | 0.9% | 20.8 (rarest word) – 127.8 (3 rarest) |
| Bloom filter per frame, 10 bits/word | 0.74 MB | 1.9% | 26.6 (rarest word) – 141.8 (3 rarest) |
| Bloom, 6 / 16 bits/word, 3 rarest | 0.45 / 1.19 MB | | 206.9 / 129.3 |

Reading: exact frame-level postings beat Bloom sketches on size and have no
false positives. Searching inside compressed frames trades a ~6× smaller
index for decoding 20–130 frames (1.3–8 MB) per query; at the measured frame
decode speed (bounds probe, queued) that is milliseconds per query vs the
docid index's postings walk. Frame postings are the natural index for the
cold archive tier; docid postings stay for the hot per-turn loop.

**D2 — whole-stream floors (no random access), tier 1 text (934 MB):**

| codec | append order | session order |
|---|---:|---:|
| zstd 3 | 0.1367 | 0.0839 |
| zstd 19 --long=27 | 0.0671 | 0.0543 |
| zstd 22 --long=31 | 0.0557 | 0.0518 |
| bsc, xz, paq8px (prefixes) | running | running |

Session order is worth 39% at zstd 3 and 7–19% at long-window levels:
evidence for session-clustered merges (queued).

**D3 — durable writes on this disk (`fsync-probe`, under the lock, load ~40).**
Crucial E100 (DRAM-less NVMe), ext4. One writer, append + `fdatasync`:

| append | commits/s | p50 ms | p99 ms |
|---:|---:|---:|---:|
| 256 B | 64 | 2.70 | 158 |
| 2.5 KiB (one turn batch) | 54 | 5.00 | 146 |
| 64 KiB | 42 | 3.02 | 344 |
| 1 MiB | 15 | 77.4 | 463 |

W writers, 2.5 KiB commits: a file per writer (one journal per agent) vs one
shared file with leader/follower group commit:

| writers | files: commits/s | p50 / p99 ms | shared: commits/s | syncs | p50 / p99 ms |
|---:|---:|---:|---:|---:|---:|
| 1 | 222 | 3.0 / 27.9 | 318 | 1,592 | 2.7 / 11.0 |
| 4 | 878 | 4.1 / 9.6 | 635 | 1,316 | 5.4 / 22.0 |
| 16 | 4,747 | 2.9 / 8.1 | 1,461 | 883 | 8.6 / 43.0 |
| 64 | 13,758 | 3.8 / 14.7 | 9,152 | 1,432 | 5.5 / 27.4 |

Durability window (open loop: 64 writers each offering one 2.5 KiB commit
per ms, acknowledged on write; one fdatasync every T ms makes everything
before it durable):

| window T | commits/s | ack p50 | ack p99 | acknowledged but not yet durable, max |
|---:|---:|---:|---:|---:|
| 0 (sync per commit, 64 per-writer files) | 13,758 | 3.8 ms | 14.7 ms | 0 |
| 1 ms | 59,600 | 0.8 µs | 43 µs | 60 commits |
| 5 ms | 54,355 | 1.1 µs | 1.0 ms | 272 |
| 20 ms | 56,123 | 1.1 µs | 0.6 ms | 1,122 |
| 100 ms | 47,242 | 2.9 µs | 1.0 ms | 4,724 |

The open-loop rows are bound by the offered load, not the disk (330 syncs/s
at T = 1 ms). A 1 ms window turns a 3.8 ms durable ack into a ~1 µs ack and
exposes at most 1 ms of turns; an agent turn (model + tool) lasts seconds,
so "durable before the agent's next call" (T9) costs the agent nothing.

**T7 — one shared journal for all agents (rejected by measurement).**
Hypothesis: one fsync covering every agent's turn beats a journal per
agent. On ext4 + NVMe, fsyncs of different files proceed in parallel (jbd2
batches them), so per-file journals scale almost linearly (16 writers:
4,747 commits/s) while one log serializes its syncs (16 writers: 1,461/s,
p50 8.6 ms). The per-agent journal of v0/v1 stays. The floor for a durable
turn on this disk is one ~3 ms flush; the tail (p99 150–460 ms for a lone
writer) is the device under contention, not the engine.

**D4 — decode vs memory bandwidth (`examples/bounds_probe.rs`).** One-thread
memcpy 6.0 GB/s. zstd decode of 64 KiB frames of tier-1 text: level 3
929 MB/s (70 µs/frame, 15% of memcpy), level 3 + 512 KiB dictionary 633 MB/s
(103 µs), level 19 828 MB/s (79 µs), level 19 + dictionary 386 MB/s
(170 µs, 6%). Ratios 0.211 / 0.176 / 0.185 / 0.139. The dictionary buys
25% fewer bytes and costs ~2× decode time (its 512 KiB working set misses
L2); a top-20 excerpt fetch decodes at most one frame per hit.

**D5 — search cost vs postings bits (`bounds_probe`, agent 3: 85,121
records, 56.9 MB raw, its last 2,000 turn-loop queries, one thread).** The v4
index of the agent store is 13.5 MB: 2.54M postings at 42.4 bits each, vs an
information bound of ~7.4 bits/posting for docid postings (T6: 2.14 MB for
2.33M postings), so 5.8× above the bound. A query visits 132k postings
(12.2k candidates) and takes p50 5.3 ms, p99 33 ms, mean 8.2 ms: 62 ns per
posting visited, vs 0.12 ms to stream those postings' bits at memcpy speed
(~70× above the bandwidth bound). The default candidate-normalized ranking
needs the full union of the query words' posting lists, so it cannot prune
top-k (lexical track E2); a 200-byte observation has many common words.
Without any index, a one-word brute-force scan of the agent's text costs
199 ms (naive scan; memcpy floor 9.5 ms). Owner of the gap: the lexical
track (postings layout, pruning); recorded here as the trace-DB search bound.

**T10 — one engine per agent at tenant scale (measured; `examples/many_agents.rs`).**
N agents, 5 small turns each (untimed, load ~40):

| agents | threads | open files | RSS | disk files | reopen |
|---:|---:|---:|---:|---:|---:|
| 16 | 17 | 69 | 7 MB | 114 | 0.01 s |
| 256 | 257 | 1,029 | 59 MB | 1,794 | 0.10 s |
| 1,024 | 1,025 | 4,101 | 144 MB | 7,170 | 0.69 s |

Fixed cost per agent: one merge thread, ~4 descriptors, ~7 files, ~130 KB.
**T10b (kept):** v4 `background_merges = false` + one trace-level merge thread
compacting every agent store in turn: 1,024 agents → 2 threads (was 1,025),
RSS 110 MB (was 144). Descriptors stay ~4 per agent; closing idle agent
stores is the next step for 10k+ tenants (queued).

**D6 — where the index bytes go.** A v4 segment stores, per row and
uncompressed (random access through mmap): 40 B of row metadata (valid_at,
recorded_at, length, scope, text_len, flags, id prefix), 20 B id
fingerprint, 20 B key fingerprint and 4 B of time order = 84 B/row, before
any posting (LEB128 row gap + tf, ~2–3 B per posting). Agent 3 (85k rows):
7.1 MB of the 13.5 MB index is that fixed part, ~6.3 MB postings. On the
27k-event smoke store the index (6.8 MB) is as large as the compressed
originals (6.6 MB, flush-level zstd 3). Agent-trace events are short (tool
calls ~190 B, results ~1.5 KB median-ish), so per-row bytes matter more than
on BEIR documents. The typed fields carry ~22 bits/event (D1); the row
table's clocks, lengths and fingerprints are mostly derivable or
low-entropy, so a bit-packed row table is a large, mechanical saving.
Reported to the lexical track's domain (segment format); in this track the
trace columns (6–7 B/event) already hold the typed fields.

**T11 — first timed run, v1 vs Postgres 17 (tier 1, one run each, load ~25)
and the fixes it led to (v2).** Selected rows (p50 ms unless noted):

| op | Oojuro v1 | Postgres 17 |
|---|---:|---:|
| bulk load, events/s | 18.9k | 20.4k |
| bytes / raw | 0.456 (index 0.232, archive 0.211, columns 0.013) | 1.78 |
| RSS after load | 973 MiB | 1.11 GiB (container anon + shmem) |
| get | 0.65 | 0.32 |
| session | 4.4 | 1.5 |
| agg_session | 0.075 | 0.68 |
| time_range 1 h (≤ 1,000) | 75.8 | 16.0 |
| filters over a day | 4.6–8.7 | 1.1–9.5 |
| agg_tools 30 days | 2.3 | 159 |
| top sessions of a day | 4.6 | 12.8 |
| mixed, 8 threads, ops/s | 1.7k | 14.1k |
| export, MB/s | 12.6 | 82.2 |
| purge 7 days (logical) | 0.05 s | 1.39 s |
| reclaim | 193 s | 47 s (VACUUM FULL) |
| deleted needles in files before / after reclaim | 0 / 0 (compressed) | 14 / 0 of 20 |

Diagnosis, all engineering rather than information-theoretic: (1) the
compression track's run reader caches partially decoded frames, and a
whole-record read decodes a frame only up to that record, so reading
consecutive rows re-decoded the same frame again and again (sessions, time
ranges, exports, and erase verification); (2) at tier 1 an agent store
(~58 MB raw) never reaches the merge fanout, so every archive run stayed at
the flush level (zstd 3, no dictionary: 0.211); (3) ~63% of anonymous RSS
was glibc arena slack after flush cycles (smoke store: 148 → 54 MB after
`malloc_trim`). Fixes (v2, `90d43a3`): whole-frame cached reads (`ZRun::get`
scans the row, which decodes and caches the frame), fetches in (agent, row)
order, a shared tool-name snapshot instead of a clone per read, major
compaction to the heavy tier (`archive=on`: every agent store rewritten as
one zstd-19 + dictionary run), and heap trimming after flushes and merges.
Timed v2 runs queued.

**B1 — baselines built and verified (settings: `docs/TRACEBENCH_BASELINES.md`).**
Postgres 17, ClickHouse 26.9 and SQLite 3.53 pass the oracle (1,237/1,237)
on smoke and tier 1. Deliberate deviations from the brief, each measured:
Postgres `args` is `json` (jsonb reorders keys and fails byte-exact
readback); Postgres ranks with `ts_rank` (`ts_rank_cd` took 1,371 ms vs
224 ms on a typical 25-word OR query); ClickHouse deletes use patch parts
(mutation deletes took ~35 s per session); ClickHouse status revisions go
to a ReplacingMergeTree side table joined with `argMax` (lightweight UPDATE
measured 215 ms p50, ~64/s under 16 writers); ClickHouse `old_parts_lifetime`
is shortened so reclaim does not wait ~8 minutes for old parts. Findings on
the way: Postgres DELETE + plain VACUUM leaves 14 of 20 deleted needles in
plaintext files (VACUUM FULL + checkpoints with `wal_recycle=off` → 0);
ClickHouse has no relevance ranking (token match ranked by distinct query
words; ~1.05 s per turn-loop search because scoring re-reads every hit) and
a durable per-turn append costs ~275 ms p50 under 16 writers (async insert
with `wait_for_async_insert=1` + fsync). Timed comparison: bench box job
(local timings indicative only).

**T12 — storage by tier of the store (size runs, untimed, tier 1).**

| Oojuro variant | archive (MiB) | index (MiB) | columns | bytes / raw | settle time |
|---|---:|---:|---:|---:|---:|
| flush tier (zstd 3, no dictionary) | 187.7 | 206.8 | 11.4 | 0.456 | 6 s |
| archive tier (`archive=on`: one zstd-19 + dictionary run per agent) | 126.4 | 201.2 | 11.4 | 0.381 | 107 s |
| archive tier, dedupe off | 134.8 | 201.2 | 11.4 | 0.390 | 93 s |

Readings: (1) dedupe inside one agent's store buys 6%; (2) per-agent stores
compress worse than one shared run (archive 0.142 of raw vs 0.091 for a
shared run over the same kind of data in T5): no dictionary or dedupe spans
agents, and SWE-agent repeats a lot across runs of the same task; (3) the
index is now 56% of the store and ~25% of the index is the per-row id/key
fingerprint tables the trace layer never reads (D6): the next storage win.

**Product model (2026-10-21; research direction, not Gate A).** A record
is writer + timestamps + text; kind, tool, status and paths are inferred by
the engine. One private append-only store per writer, plus shared stores a
write may also be published to; search = ~15 from the writer's own store +
≥ 5 from shared stores; erasing a writer = dropping its store; dedupe never
crosses stores. This track's per-agent design already matches the storage
side (T12 (2) is the measured price of "never across stores"). Plan for
milestone 2 (TraceBench v2): minimal records with inferred fields (cost and
accuracy against TraceBench's typed ground truth, using track/toolcall's
`oojuro-fields` extractor), private + shared stores with per-write
publishing, vs Postgres and ClickHouse holding one table with a writer_id.

## Capability gaps (Oojuro trace v1 vs the baselines)

| capability | Postgres 17 | ClickHouse | SQLite | Oojuro trace v1 |
|---|---|---|---|---|
| ad-hoc queries | full SQL | analytic SQL | full SQL | none: the 18 TraceBench operations only |
| joins | yes | yes (hash joins, limited) | yes | none (parent/session links via API) |
| multi-statement / multi-table transactions | ACID, MVCC | no (atomic per insert block) | ACID, one writer | atomic per turn batch within one agent; nothing across agents |
| replication | streaming + logical | ReplicatedMergeTree | none built in | none; segments are immutable files (copy-friendly), journal shipping not built |
| point-in-time recovery | base backup + WAL archive | backups / partition copies | file copy | logical as-of reads (two clocks) on live data; physical PITR not built |
| access control | roles, row-level security | users, row policies | none (file permissions) | one directory per agent; no policies |
| schema changes | ALTER TABLE | ALTER TABLE | ALTER TABLE (limited) | fixed event schema + JSON args |
| full-text ranking | ts_rank over GIN | none native (token match) | FTS5 bm25 | code-length (SPUD-Dirichlet), as-of, per agent |
| bitemporal as-of reads | by hand | by hand | by hand | native |
| physical erasure | VACUUM FULL; WAL keeps text until recycled | mutations / partition drops | VACUUM | segment erase (rows blanked, terms and dictionary rebuilt); crypto-shredding measured (T5) |
| ecosystem | drivers, ORMs, BI, extensions | BI, Grafana, drivers | everywhere | a Rust library; no network server, no drivers |

None of the four can promise erasure below the filesystem: SSDs remap
blocks, so deleted bytes can survive in flash until garbage-collected. Only
per-session encryption with key destruction (T5) gives that guarantee.

## Ideas queue

| idea | state | one line |
|---|---|---|
| bulk path per agent per chunk (T2) | tested, kept | 6.8× faster load |
| dictionary only on merged runs (T3) | tested, kept | removes 0.2 s flush stalls |
| compressed column blocks (T4) | tested, kept | 6–7 B/event vs 64; 2.1–2.5× the information bound |
| persisted column blocks (T4) | tested, kept | checkpoint + watermark replay |
| one shared journal for all agents (T7) | rejected | parallel per-file fsyncs scale 3.2× better at 16 writers on ext4/NVMe |
| time-aligned segments, drop whole segments on purge | queued | purge without rewriting, like ClickHouse partitions |
| crypto-shredding per session (T5) | measured, product decision | +13–21% bytes with a public dictionary, 2× without; O(1) provable erasure |
| search over compressed frames (T6) | measured | frame postings 0.9% of raw vs docid postings 5.4%; 20–130 frame decodes per query |
| Bloom sketches per frame (T6) | rejected | larger than exact frame postings (1.9% vs 0.9%) and 28% false-positive decodes |
| session-clustered merges | rejected | per-agent stores already keep sessions together; archive gain 3% (T5 shared-session vs shared-append) |
| segment-level erasure (T8) | built | `Nine::erase_rows` rewrites only segments holding dead rows; timed comparison queued |
| per-agent row-aligned column blocks (T4b) | built | sessions touch 1–2 blocks instead of ~70; decode ~0.1 ms per 1,024 rows under load |
| deferred durability: ack on write, sync before the agent's next call (T9) | built, product decision | measured as an option; violates the one-sync-per-finished-turn ruling unless the turn counts as finished at the next call |
| shared merge thread (T10b) | tested, kept | 1,024 agents: 1,025 → 2 threads, 144 → 110 MB |
| close idle agent stores | queued | ~4 descriptors per open agent store |
| lean index: no id/key fingerprint tables, bit-packed row table (D6) | queued | ~25% of the index is per-row fingerprints the trace layer never reads |
| inferred fields from text (product model) | queued, M2 | measure extraction cost and accuracy vs typed ground truth (`oojuro-fields`) |
| private + shared stores with per-write publishing (product model) | queued, M2 | TraceBench v2 deployment shape |
