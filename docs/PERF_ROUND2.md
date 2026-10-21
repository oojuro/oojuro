# Perf round 2 (`perf-1-store-cache` ... `perf-6-fsync-audit`)

Request (2026-10-21): "keep improving the performance". Improve Oojuro's
performance in order of value, with measurements before and after every
change. The principle stays (`DESIGN.md`): the core is simple and horizontal.
Anything that does not measurably pay is cut, and nothing adds write-path cost
without a clear win.

Base: `main` at `df8fa85`. One branch per item, each cut from the previous
one, so the stack reviews item by item:

| # | branch | item |
|---|---|---|
| 1 | `perf-1-store-cache` | per-project store-once cache |
| 2 | `perf-2-dedupe-key` | record dedupe key without `src` / `rt` |
| 3 | `perf-3-lean-index` | leaner fixed index tables |
| 4 | `perf-4-read-paths` | single-record `get` and 1-hour `time_range` |
| 5 | `perf-5-long-queries` | long queries on big single stores |
| 6 | `perf-6-fsync-audit` | one flush per batch, both log modes |

An item that cannot meet its bar stops, is documented here with its numbers
(negative results too) and the work moves on.

## The proof required for every item (frozen before work started)

1. **Tests.** `cargo test --workspace --locked` passes (baseline 258 passed,
   2 ignored) and the item adds no clippy warning (`cargo clippy --workspace
   --all-targets --locked`, warnings compared with the baseline of `main`).
2. **TraceBench `verify`** is 1,237/1,237 for `oojuro` and `oojuro:log=private`
   on `smoke.tbe`.
3. **Search is bit-identical** where the item claims "lossless": the ranked
   lists of the 30,905 tier-1 turn-loop queries compared with
   `scripts/perfsearch/prefix_compare.py`; and the retrieval-quality evals are
   equal (SciFact, NFCorpus, FiQA dev, LongMemEval dev, the agent-trace turn
   loop).
4. **Before / after numbers** for the item's own target metric, plus the guard
   metrics: ingest commits/s and p99, turnloop turn p50 / p99, search probe
   p50 / p99, and disk size.
5. **A format change keeps existing stores readable** (a reader for the old
   format, or a transparent rebuild on open), with a test that opens a store
   written by `main` and one written by the new code.

## Method

- Environment: the project's build environment script; everything at
  `nice -n 19`, `TMPDIR` on disk, a scratch `CARGO_TARGET_DIR` per worktree
  (a shared target dir needs `cargo clean -p <workspace crates>` first; none
  is shared here). The "before" binaries are built from `main` in their own
  worktree and target dir; the "after" binaries from the item's branch.
- The machine is shared and loaded (load 3 to 50 during this work). Timed
  comparisons **alternate** before / after runs, several rounds, each run in a
  short hold of the shared benchmark lock file (`$OOJURO_BENCH_LOCK`); every table reports the
  median with min-max, and the 1-minute load average is recorded. When noise
  swamps a difference this document says so instead of claiming it.
- Data: `smoke.tbe` (20k events) and `base1m.tbe` (1.32M events), both
  public. Real-data checks copy a sample through the read-only `oojuro
  export` into a scratch directory, report aggregates only, and delete it.
- Nothing here touches a running recorder, its journal, or any remote service.

## Tools

All probes and drivers used below are in the repository or reproducible from
it: `crates/oojuro/examples/multistore.rs` (item 1), `tracebench` and
`searchprobe` (`crates/oojuro-tracebench`), `scripts/perfsearch/prefix_compare.py`.
The alternation driver, the summaries (median, min-max) and the proof script
are small scripts kept outside the repository (they only call the binaries
above; each table says what was run).

## Item 1. Per-project store-once cache

**Finding.** `Owner::append` (hooks, OpenTelemetry, a process serving many projects) keeps
the "store once" keys (`append-seen.bin`, up to 262,144 keys) in one
process-wide static: a single slot behind one mutex, reloaded from the file
whenever a different store appends, and held across the whole append,
including its fsyncs. Journals of different projects in one process therefore
took turns, and with several stores in use every call re-read up to 4 MB.

**Change.** The cache is a field of the open store (the journal's `Reader`
state, loaded on the store's first append, dropped with the store). Same keys,
same dedupe results; `Owner::append` is otherwise unchanged. A cache that
dies with its journal also cannot go stale across a close, another process's
writes and a reopen, which the old slot could.

**Tests** (`crates/oojuro/tests/stores.rs`, `src/append.rs`): six stores
appended from six threads give exactly the `Appended` results of the same work
done serially (ids, deduped, withheld, empty, redacted); a key seen in one store
is new in another and stays so with the stores' calls interleaved; the keys
survive a close and reopen; and store B's append completes while store A's
cache lock is held (the independence is structural, not a timing).

**Measurement** (`multistore`: N stores, one `Journal` each, 3-record
batches over 3 writers per store, a quarter of the records repeating an
earlier key, 400-byte texts). `serial` runs the stores one after the other,
`parallel` one thread per store; before = `main`, after = this branch, the four
runs alternating in each of 5 rounds, each run in a hold of the bench lock.
The `digest` (a hash of every `Appended` returned) is identical in every run of
every configuration: same dedupe results and ids as the serial run and as
`main`.

On tmpfs (no device flushes, so the lock is the only thing left to wait on;
calls per second, median (min-max); the machine had 6-11 other load, and 16
hardware threads):

| stores | main, serial | main, N threads | this branch, serial | this branch, N threads | speedup N threads / serial |
|---:|---:|---:|---:|---:|---:|
| 1 | 3518 (3493-3532) | 3507 (3461-3526) | 3531 (3522-3536) | 3506 (3470-3527) | 1.0x (unchanged) |
| 2 | 3527 (3507-3533) | 3615 (3590-3615) | 3507 (3502-3537) | **6835 (6773-6957)** | 1.95x |
| 4 | 3494 (3271-3533) | 3648 (2647-4161) | 3528 (3450-3529) | **13350 (12640-13550)** | 3.8x |
| 8 | 3529 (3438-3537) | 3460 (3103-3684) | 3513 (3412-3530) | **25700 (18470-25900)** | 7.3x |
| 16 | 3388 (3211-3491) | 2959 (2823-3036) | 3455 (3360-3494) | **27740 (25740-33190)** | 8.0x |

`main` does not scale at all (N threads = 1.0x serial, with p99 growing from
0.3 ms to 18 ms at 16 stores: the threads queue on one lock); this branch is
linear up to the cores left free by other jobs (16 threads, load 6-10). p99 per
call with 16 threads: 18.3 ms -> 4.4 ms. Single store: unchanged (3531 vs 3518
calls/s serial; the difference is noise).

On the NVMe disk (every call fsyncs: this probe measures about 4.7 syncs per
call, see item 6) the disk, not the lock, bounds the gain, and the machine's
other jobs made fsync latency swing by 10x between runs; medians:

| stores | main, serial | main, N threads | this branch, N threads | p99 ms, main -> this branch (N threads) |
|---:|---:|---:|---:|---:|
| 1 | 246 (185-248) | 246 (185-247) | 247 (246-248) | 6.7 -> 4.9 |
| 2 | 248 (226-248) | 197 (142-249) | **371 (370-423)** | 25 -> 6.9 |
| 4 | 198 (46-245) | 111 (38-283) | **180 (97-496)** | 191 -> 85 |
| 8 | 130 (76-226) | 199 (59-249) | **538 (48-844)** | 241 -> 17 |
| 16 | 118 (58-187) | 199 (126-239) | **984 (113-1067)** | 403 -> 48 |

So with 16 stores that shape goes from 199 to 984 calls/s (4.9x) and the
p99 from 403 ms to 48 ms, and even with fsync-bound calls the threads no longer
slow each other down (`main` with 4 stores: N threads slower than serial). The
remaining distance to linear on the disk is the number of syncs per call
(item 6).

**Guard metrics.** The change is in the `oojuro` library crate only. The engine
crates that `tracebench`, `searchprobe` and the retrieval evals link
(`oojuro-nine`, `oojuro-store`, `oojuro-trace`) are byte-identical to `main`
(`git diff main perf-1-store-cache -- crates/oojuro-nine crates/oojuro-store
crates/oojuro-trace crates/oojuro-tracebench` is empty), so ingest commits/s and
p99, turn-loop p50/p99, the search probe, the retrieval evals and disk size
cannot differ and were not re-run for this item. Single-store append: unchanged
(above).

## Item 2. Dedupe key without `src` and `rt`

**Finding.** Record dedupe (`PERF_STORE` P1) compares (indexing class, tool,
args, text). Every captured record's args carry its own source position
(`src`) and receive time (`rt`), so on the recorder path no two records were
ever equal: on a 30-day export of a real recorded journal (323,006 records,
866 MB of export), **0** records deduplicated, and all of the first-copy
bookkeeping paid for by P1 (window, entries) did nothing. (The same study on a
60-day export found 12.3% would repeat without the two values.)

**Change** (`oojuro-trace`: `dedupe.rs`, `lib.rs`, `columns.rs`).

- The comparison leaves out the *values* of the top-level `src` and `rt`
  members (replaced by `0` before hashing). Only args in the exact form
  capture writes them take part: a `src` that is a string of digits and
  colons, an integer `rt`, each once at the top level. Anything else
  (no such members, other forms, duplicate members, nested ones, not JSON)
  compares whole, exactly as before: TraceBench events and every store that
  never had `src` / `rt` behave byte for byte as on `main`.
- A repeat keeps its own `src` and `rt` as two more fields of its entry in the
  host record's `B` list (`rt` delta-coded), and a read puts them back into the
  args of the record that holds the text, so every event reads back with
  exactly the args it was appended with (checked byte for byte by the tests
  below). Crash recovery reads `src` through `get`, so a repeat is found by its
  own position and is not appended twice.
- A holder (the text row of a deleted event that live repeats still use) keeps
  neither value of the deleted event. Reclaim carries the repeats' provenance
  when it rewrites entry lists (erase and rewrite modes).
- One more flag on the column slot (bit 7 of the status byte; absent = 0 in old
  checkpoints, which stay readable).

**Format.** Stores written by `main` open unchanged (their entries have no
provenance). Entries with provenance (17 fields instead of 15) are not
readable by older binaries; stores that never got a relaxed repeat are
unchanged. `crates/oojuro-trace/tests/compat.rs` opens a store written by
`main` (fixture, 160 KB), gets the answers `main` gave, and appends,
flushes, merges, deduplicates repeats of the old texts and erases on top of
it.

**Tests.**
`dedupe.rs` (unit): split / apply / key round trips and 13 shapes that must
compare whole. `tests/dedupe.rs`: the existing erase/rewrite-mode gates run a
second time with captured-style args on every record (dedupe ratio, exact
reads of every event including its own `src` and `rt`, reopen, rebuild without
a checkpoint, delete, reclaim, holders, last reference dying, no text left in
any file), plus nine pairs of args forms (two that relax, seven that must compare whole), read back exactly after a reopen and a rebuild. `oojuro-capture`
`repeated_texts_store_once_and_a_crash_still_neither_duplicates_nor_loses`:
five turns whose tool output repeats, a crash before the state save, restart:
all 15 records keep their own source position, 15 are skipped on re-read, only
the new turn is stored. `crates/oojuro/tests/duplicates.rs`: four copies of one
tool result through `Owner::append` store one text and keep four receive times;
`forget` of the first copy hands the text to the next (search too), the proof
holds at every step, a reopen answers the same, and when the last copy goes the
text is in no file.

**Measurement.** The real 30-day sample replayed into a fresh store with the
recorder's options (`examples/replay_export.rs`, sequential, 8 batches per
call), sizes after settling merges (MB; the 67.1 MB shared-log files are the
same in both and left out):

| | `main` | this branch | change |
|---|---:|---:|---:|
| engine records stored | 323,006 | 284,673 | -11.9% (38,333 repeats) |
| index (`index`) | 66.5 | 61.0 | **-8.3%** |
| originals (`archive`) | 132.2 | 123.2 | **-6.8%** |
| column checkpoint | 2.0 | 2.3 | +0.3 |
| total without the log | 200.7 | 186.4 | **-7.1%** |

Two runs of each give the same sizes (the replay is sequential, so the result
is deterministic). The fraction found matches the earlier study (12.3% of 60
days). TraceBench data has no `src` / `rt`: with a deterministic load
(`searchprobe build --load-threads 1`) the tier-1 stores of `main` and of this
branch have byte-identical files except one agent's two last segments (merge
timing), the 30,905 tier-1 turn-loop queries return identical ranked ids and
score bits (`prefix_compare.py`: 30,905 identical, 0 continued, 0 other), and
this branch reading the store `main` built answers identically too (30,905
identical).

**Guard metrics** (TraceBench `base1m`, `main` and this branch alternating, 5
rounds each (turn loop 5), median (min-max); the machine carried 6-11 of other
load, which dominates the tails):

| metric | `main` | this branch |
|---|---:|---:|
| ingest, shared log (16 writers, 30,000 batches): commits/s | 11,760 (9,690-12,600) | 12,410 (7,031-12,660) |
| ... p50 / p99 ms | 0.88 (0.86-1.02) / 1.62 (1.49-4.30) | 0.87 (0.86-1.33) / 2.05 (1.52-10.2) |
| ... disk bytes | 100,676,881 | 100,676,881 |
| ingest, `log=private`: commits/s | 3,674 (2,963-3,800) | 3,762 (3,441-4,078) |
| ... p50 / p99 ms | 2.53 (2.20-2.71) / 15.96 (9.7-25.8) | 2.40 (2.08-2.80) / 9.6 (7.3-14.2) |
| search probe, tier 1, 1,000 turn-loop queries, one thread: p50 / p99 ms (thread CPU) | 1.049 (1.013-1.151) / 1.911 (1.852-2.159) | 1.044 (1.009-1.156) / 2.006 (1.877-2.101) |
| turn loop (16 writers): turn p50 ms | 3.90 (3.01-4.84) | 4.26 (3.59-4.62) |
| ... turn p99 ms | 92 (11-121) | 22 (12.9-298) |
| ... search top-20 p50 / p99 ms | 1.89 (1.73-2.07) / 5.15 (4.52-7.13) | 1.99 (1.85-2.09) / 6.12 (5.28-8.28) |
| ... disk bytes | 350,252,923 | 350,232,032 |

Every min-max range overlaps: the differences in the tails
(ingest p99, turn p99, top-20 p99) go in both directions between rounds and
are load, not code (the one-thread search probe, which does not run while
anything else of ours runs, is equal to 0.5%: this change does not touch
search). Disk size is the same to 0.01%.

**Cost and risk.** A relaxed repeat's read does one more record read (its
host's entry list; cached per call for session reads) and a read of its text
row's frame: item 4 measures what that costs on this sample (a one-hour window
of 400 events reads about 1.7 ms longer at p50, and `query_threads` brings the
mean back under `main`'s). New writes of captured
records compute the split and keep the BLAKE3 they already did. Search now
returns the earliest live copy of a text on the recorder path too (the P1
semantics, "identical texts are one search document"); `forget` of one copy
keeps the text while another copy is live (the proof counts live copies). A
journal written after this change should not be opened by a build from before
it once a repeat with provenance exists.

## Item 3. Leaner fixed index tables

**Finding** (`TRACK_COMPRESS` index study). A segment kept 84 bytes of fixed
tables per row: 40 B of row metadata, a 20 B id entry, a 20 B key entry and a
4 B time-order entry. After item 2 on the real sample they are 39% of the
index (59% on TraceBench tier 1).

**Change** (`oojuro-nine`: `rows.rs`, `segment.rs`, `merge.rs`, `v4.rs`).
Segment version 3:

- **rows**: the same seven fields packed per block of 128 rows as frames of
  reference (block minimum + each row's difference in the fewest bits that
  fit; the first 8 id bytes drop the trailing zero bits that every row of the
  block shares). About 10 bytes a row; reading one field of one row is a few
  shifts, and `scope_length` (read for every candidate of a query) touches two
  fields. `recorded_at` is stored only in blocks where it differs from
  `valid_at`.
- **keys**: only the entries whose key is not the row's own id. Almost every
  event's key is its id, so the id table answers for it: `rows_with_key(fp)` is
  the id rows of `fp` that are not exceptions, plus the exception entries of
  `fp`; the sorted stream a merge reads is rebuilt from the same two sources.
  The writer takes the id and key tables together (`fingerprints`) and finds the
  exceptions with one merge-join of the two sorted streams.
- **time order**: not written. The first reader that needs it (a range read)
  sorts it out of `valid_at` once per segment and keeps it; a merge no longer
  builds it at all.
- Version 1 and 2 segments still open, merge (into version 3) and erase; a
  file's CRC and its row-offset table are checked at open.

**Tests.** `rows.rs`: round trips over 35 value shapes (extremes of every field,
all-equal blocks, descending, 128-row and short blocks) at 10 sizes, the size
of ascending decimal ids with equal clocks (under 9 B a row), damaged offset
tables refused. `segment.rs`: the same synthetic rows, id and key tables written
as version 1 and as version 3 answer identically for every row field,
`rows_with_id`, `rows_with_key` (every id and key present, and absent ones),
the id and key entry streams, the time order and `time_lower_bound`, with
0-33% key exceptions (versions of a key, a key equal to another row's id, keys
of their own); and a file smaller in every case. The writer refuses a table
that misses or repeats a row. `oojuro-trace/tests/compat.rs`: the version 2
store written by `main` opens, answers as it did, and merges with version 3
segments (and a store this build writes gives the same 48 digests).

**Measurement.**

| tier-1 store (899,987 events, 16 agent stores, deterministic load) | `main` | this branch | change |
|---|---:|---:|---:|
| index files (`*.idx`) | 112,437,344 | 77,050,253 | **-31.5%** |
| originals (`*.src`), column checkpoint | 138,239,464; 12,553,999 | same | 0 |
| whole store directory | 263,238,320 | 227,851,234 | -13.4% |

The real 30-day sample (item 2 is in both numbers; MB after settling):

| | `main` | item 2 | item 3 |
|---|---:|---:|---:|
| index | 66.5 | 61.0 | **46.0** |
| total without the shared log | 200.7 | 186.4 | 171.5 |

On the real sample item 3 alone takes the index down 24.6% (item 2 + 3 together
30.8% below `main`); on TraceBench tier 1, where 36% of the rows are status
revisions and all events are in one clock, -31.5%. The bar was -30% of the
index: **met on TraceBench, missed on the real sample by about five points.**
What is left of the fixed tables there is the id table (12% of the new index,
20 B a row of 128-bit fingerprints). It cannot go without changing what Nine
promises: duplicate ids are refused at insert, version groups are rebuilt from
key fingerprints alone at open, and both need a lookup by fingerprint in every
older segment; fingerprints cut to 96 bits would save only 2.4% more and would
weaken the exactness of key lookups, so they were not taken. (The study's
"drop `cf`" is not available either: the padded background of anchored queries
reads it.)

Search is exactly the same. The 30,905 tier-1 turn-loop queries return identical
ranked ids and score bits (a) from this branch reading the store `main` built
(version 2 segments), and (b) from the store this branch built against the one
`main` built (deterministic load): 30,905 identical, 0 continued, 0 other. The
retrieval evals (`oojuro-bench`, nine arm, dev splits, one run each) are equal
in every metric:

| eval | nDCG@10 (main = this branch) | other | store bytes main -> this branch |
|---|---:|---|---:|
| SciFact | 0.6767 | recall@100 0.8935 | 7,875,482 -> same (no segment written) |
| NFCorpus | 0.2925 | recall@100 0.2431 | 5,860,639 -> same |
| FiQA | 0.2879 | recall@100 0.5809 | 39,790,074 -> 36,969,070 |
| LongMemEval | turn nDCG@10 0.6969 | turn recall@20 0.8560 | - |
| agent-trace turn loop (50 trajectories) | 0.2607 | recall@20 0.5343 | 18,373,131 -> same |

**Search latency and the guard metrics** (`main` and this branch alternating,
5 rounds, turn loop 3; median (min-max); the load average was 18-22 during
this series, so wall-clock tails say nothing and the one-thread probe is
reported in thread CPU time):

| metric | `main` | this branch |
|---|---:|---:|
| search probe, 200 B turn-loop queries: CPU p50 / p99 ms | 1.892 (1.816-1.922) / 3.35 (3.18-3.44) | 1.887 (1.878-1.928) / 3.40 (3.31-3.46) |
| ... 4 KB queries | 4.73 (3.17-4.78) / 11.5 (7.7-11.7) | 4.71 (3.23-4.91) / 11.6 (7.8-11.9) |
| ... 64 KB queries | 9.29 (9.17-9.41) / 13.1 (12.0-14.7) | 9.23 (9.14-9.91) / 12.7 (12.0-14.3) |
| ingest, shared log: commits/s | 11,730 (10,520-12,180) | 11,120 (5,189-11,370) |
| ingest, `log=private`: commits/s | 2,928 (2,775-3,483) | 3,359 (1,003-3,795) |
| turn loop: turn p50 / p99 ms | 3.62 (3.25-9.64) / 12.4 (9.8-65) | 3.82 (3.25-4.13) / 14.0 (11.5-14.5) |
| ... search top-20 p50 / p99 ms | 2.02 (1.85-2.48) / 5.8 (4.8-26) | 2.07 (1.77-2.09) / 6.3 (4.7-7.1) |
| turn-loop store, bytes on disk | 350,225,310 | 313,168,868 (-10.6%) |

The search probe is equal at all three query sizes (differences under 1.5%, inside
the spread of the rounds). The first version of this change read (scope, length)
of each candidate by parsing the candidate's block header again, and measured
+2.9% at 200 B (1.244 vs 1.209 ms CPU p50, ranges just touching): the scan
reads those two fields for every candidate in row order, so it now keeps the
current block's parameters (`MassCursor`) and parses a header once per block.
Ingest never reaches a segment in the 30,000-batch run (its store is only
journal: the disk size is identical to the byte), so the format cannot change
it; the first series above had `main` ahead in four of five rounds by 1-11%,
which an extra alternation of 8 rounds of `main`, item 2 and this branch under a
load of about 25 explains as run-to-run spread: medians 8,640 (828-11,350),
8,594 (1,183-11,760) and 8,782 (848-12,050) commits/s, p50 1.12, 1.02 and
1.08 ms. The turn-loop differences (+5% turn p50; this branch worse in two
rounds and better in one) are inside the spread of the `main` series itself
(3.25-9.64 ms): no change is claimed in either direction, and the thread-CPU
search probe, which does not wait for anything, shows none.


## Item 4. Read paths we lose on

`tracebench reads` on tier 1 (Oojuro / SQLite / Postgres, from `TRACK_TRACEDB`):
single-record `get` 0.146 / 0.009 / 0.039 ms, 1-hour `time_range` 4.8 / 1.4-3.0 ms.

**Where the time goes** (a throwaway build with timers, not kept; tier-1 store,
one thread, load about 8). A cold `get` of an event, p50 100 us: column lookup
1 us (its p90 is 96 us: the first touch of each sealed column block), store
lookup 0.1 us, **reading the row from the archive 99 us (99%)**, decoding the
record 0.6 us, building the event 0.2 us. A row read is the frame: read and
CRC-check about 20 KB, decompress 64 KiB with zstd and the dictionary, check
the BLAKE3 of the 64 KiB (13 us). `frame_probe` on a run file with no cache,
random rows, load 5: **a whole-frame read 74-90 us, a read of only the frame's
bytes up to the row 55-62 us, and even a read that needs one byte of the row
55 us**: 55 us is the floor of any cold frame access (read, CRC, meta,
decoder state), whatever is decoded after it. A repeated get of the same id is
0.4 us (decoded-frame cache), so a cache cannot help random ids and
already serves repeated ones. A 1-hour `time_range` of ~980 events costs
0.31 ms (column scan) + 0.04 ms (sort) + 5.3 ms fetching: about 37 frames
(26 events a frame), each read once; 0.6 us per event for everything that is
not a frame read. So both reads lose to SQLite and Postgres on frame
decompression, which they do not have (their pages are not compressed).

**Changes** (`oojuro-nine`, `oojuro-trace`):

- `Run::get_point`: a lone `get` / `text_range` decodes only the frame's bytes
  up to the row and skips the frame digest (`Nine::record_at_point`); a batch
  read keeps decoding and caching whole frames for the neighbouring rows. A
  frame decoded in part is decoded whole by the next batch that needs it.
- `EventOut` takes the record's text instead of cloning it.
- Reads of many rows (`time_range`, `session`, `export`) cut the (agent, row)
  order into contiguous runs and give them to `query_threads` threads (default
  1: unchanged); a frame at a cut may be read by both sides.
- The provenance of a relaxed repeat (item 2) is found by scanning its host's
  entry list for the ids wanted, instead of decoding every entry of the list
  (the first version of item 2 measured a p99 of 2.25 ms for a `get` on the
  real sample, against 0.47 ms on `main`).

**Measurements.** `read_probe` (3,000 random event ids, then 300 one-hour
windows, limit 1000, on the tier-1 store `main` built; `main` and this branch
alternating over the same ids, 8 pairs, load 4; median (min-max)):

| tier-1 (no repeats) | `main` | this branch |
|---|---:|---:|
| cold `get` p50 ms | 0.098 (0.093-0.236) | **0.083 (0.081-0.086)** |
| ... mean / p99 ms | 0.107 / 0.324 | 0.097 / 0.283 |
| 1-hour `time_range` p50 / mean ms | 3.98 / 3.94 | 3.97 / 4.08 |

The cold `get` is faster in 8 of 8 pairs (-15% p50, -10% mean, -13% p99); the
`time_range` with one thread is equal (the spread of `main` alone is 3.7-4.6).
With `query_threads` of 1 / 2 / 4 / 8 (this branch, 6 seeds, load 9) the same
`time_range` has p50 5.10 / 3.24 / 2.49 / 2.03 ms and p99 13.4 / 7.8 / 5.9 /
6.0 ms. The official suite (`tracebench reads`, `main` and this branch
alternating, 4 rounds, load 3-10; the store is loaded in the same process):

| `reads` suite | `main` | this branch |
|---|---:|---:|
| `get` p50 / mean / p99 ms | 0.184 (0.174-0.188) / 0.174 / 0.377 | 0.161 (0.160-0.250) / 0.165 / 0.410 |
| `get_range_512` p50 ms | 0.167 (0.158-0.177) | 0.151 (0.142-0.242) |
| `time_range_1h_1000` p50 / p99 ms | 6.12 (5.63-6.63) / 11.1 | 6.57 (5.70-7.86) / 16.0 |
| `session` p50 ms | 0.413 (0.351-0.450) | 0.444 (0.381-0.789) |

`get` and `get_range_512` are 9-12% faster in three of four rounds (the fourth
round, at load 9.7, was slower for both); `time_range` and `session` are not
changed by this branch with one thread, and the 6.12 vs 6.57 ms difference is
inside `main`'s own spread. They remain 2-4 times SQLite's: that is frame
decompression (above).

**What item 2 costs to read.** The real 30-day sample store (12% of its events
are repeats whose text lives in an earlier frame), same probe, `main`'s store
read by `main`, this branch's store read by this branch, 6 pairs, load 4:

| real sample | `main` | this branch |
|---|---:|---:|
| cold `get` p50 / mean / p99 ms | 0.084 / 0.115 / 0.479 | 0.070 / 0.116 / 0.669 |
| 1-hour `time_range` (400 events) p50 / mean ms | 0.94 / 4.17 | 2.66 / 6.21 |
| ... with `query_threads` 4 / 8: p50 / mean ms | | 2.00 / 3.56, 1.67 / 3.08 |

A repeat is read from two frames, its text row's (an older one, usually not
cached) and its host's entry list; a one-hour window holds about 48 repeats,
so it can read up to some 48 more frames than the store without them (1.7 ms
longer at p50, 2 ms at the mean). This is the price of storing a text once, paid at
read time only for repeats; with `query_threads` of 4 or more the mean is back
under `main`'s. It is the reason item 2 saves 7% of the bytes but is not free.

**Not done, and why.** A larger decoded-frame cache (random ids have no
locality; a repeated id already costs 0.4 us); smaller frames or an
uncompressed hot tier (the ratio and the file format; the floor of 55 us per
cold frame is the decoder and CRC setup); dropping the BLAKE3 digest on full
frame reads (13 us of ~80, the only check of the decoded body, and a lone read
no longer pays it); matching SQLite's 9 us (a page-cache hit on uncompressed
data) is not reachable with compressed 64 KiB frames.
