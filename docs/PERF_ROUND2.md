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
