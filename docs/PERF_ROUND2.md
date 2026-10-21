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
`main` (fixture, 140 KB), gets the answers `main` gave, and appends,
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
host's entry list; cached per call for session reads). New writes of captured
records compute the split and keep the BLAKE3 they already did. Search now
returns the earliest live copy of a text on the recorder path too (the P1
semantics, "identical texts are one search document"); `forget` of one copy
keeps the text while another copy is live (the proof counts live copies). A
journal written after this change should not be opened by a build from before
it once a repeat with provenance exists.
