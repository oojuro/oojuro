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

## Baseline

(filled in as the items are measured; see each item)
