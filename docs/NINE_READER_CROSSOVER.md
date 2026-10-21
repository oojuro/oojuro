# Balanced reader comparison on the frozen corpus

Protocol fixed before execution on 2026-10-19. This lab command changes no Nine
storage, scoring or default query behavior. The preceding smoothing candidates
failed their evidence gate; the fixed-128 ranker remains delivered.

## Why this control is needed

The original SQLite native-snippet path and the alternative shared Rust reader
retain the same reference facts in separate current runs. The latter records
lower local latency. However, even unchanged Nine packets show substantially
different absolute timings across runs, so those sequential runs alone cannot
isolate a reader-specific speed effect.

`nine-reader-crossover` runs three arms against **one** reopened frozen pair:

1. Nine with the existing Rust reader.
2. SQLite FTS5 with its native snippet seed and the existing bounded expansion.
3. SQLite FTS5 with the same Rust reader as Nine, intentionally loading full
   source bodies. These allocations and reads are timed; this is not a hosted
   wire-byte comparison.

Both SQL arms use the same connection, indexes, query expression, source snapshot
and prepared-statement cache. They must produce identical ranked candidates.
All arms retain the existing both-clock/latest-version semantics, 512-reference
frontier, 32-source cap, 2 KiB excerpts and 16/64 KiB serialized context budgets.
Caller phrase routing, stopwords and optional Porter selection are explicit
flags. No shared-context admission, embedding or LLM is introduced.

## Timing and audit rules

For each question and budget, one warm-up precedes six measured repetitions.
Use all six permutations of the three arms. Each occupies every position twice
and every ordered within-question predecessor pair occurs twice. A multiple of
six repetitions is required. Record and validate both observed count matrices.
Clear Nine's result cache on each call as in the
existing comparison. Time ranking, extraction, serialization and synced query
receipts; exclude reopen, index construction and audit serialization.

Repeated packets must remain identical. Store full actual flat context JSON in
the ignored private audits only after all timed budgets complete. Independent
verification checks every returned origin's bytes, UTF-8 range, header, both
clocks, latest-version eligibility, candidate order and serialized size, as
well as source-specific supporting quotes. Aggregate reports contain no source
text, raw queries or private evidence packets.

Older range-only audits cannot directly prove their omitted wire headers. Their
independent verification is strengthened to check all selected ranges and
eligibility and reconstruct the expected serialized byte count. The new full
private audits permit direct header/text checks without relying on the Rust
builder's copy logic. The original completeness gate stays unchanged.

This is a reused development benchmark, not a generated-answer test, actual
agent-task outcome, throughput test, cold-start measure or hosted SLA. Compare
the complete evidence and byte sizes alongside per-group median/p95 latency.
Keep both SQL controls available, including any stronger quality configuration.

## Result

The final control completes **4,644 stable timed queries**, 18,495 original
cross-checks and **774 full private packet audits**. Every arm occupies each
timed position 516 times; every ordered predecessor pair also occurs 516 times
(self-pairs are zero). Both observed count matrices are independently checked. The source
archive is unchanged. Both SQL modes return the same candidate rankings, and
all 774 audit projections exactly match the corresponding previous controls.

The following numbers include final flat-context JSON serialization in the
timed path. Entries show source-specific complete references and median/p95 ms:

| Workload / context cap | Nine | SQLite native snippet | SQLite shared Rust reader |
| --- | --- | --- | --- |
| History / 16 KiB | 19/21; 10.60 / 15.23 | 18/21; 47.42 / 77.17 | 18/21; 36.69 / 57.70 |
| History / 64 KiB | 19/21; 7.96 / 12.55 | 20/21; 57.21 / 122.21 | 20/21; 32.33 / 51.14 |
| Documents / 16 KiB | 19/20; 7.51 / 13.11 | 18/20; 36.49 / 104.99 | 18/20; 27.29 / 47.74 |
| Documents / 64 KiB | 19/20; 6.72 / 11.72 | 19/20; 36.83 / 73.21 | 19/20; 23.40 / 35.87 |
| Huge tools / 16 KiB | 9/9; 40.43 / 46.57 | 9/9; 80.12 / 133.07 | 9/9; 79.37 / 96.32 |
| Huge tools / 64 KiB | 9/9; 31.03 / 37.84 | 9/9; 79.73 / 104.33 | 9/9; 62.80 / 84.01 |

All arms also preserve 86/86 declaration checks. The SQL Rust-reader median and
p95 are lower than the native-snippet figures in every listed group/cap in this
run. Keep both controls and their measured distributions.
There are only six timed repetitions per question; this is not a production
tail-latency claim or a universal database speed ranking.

At 64 KiB mean history context bytes are 25,380.5 for Nine, 65,433.3 for SQL
native snippets and 65,506.9 for SQL's Rust reader. Document bytes are
46,276.2 / 65,496.9 / 65,494.8 respectively. Nine's smaller history packet also
omits a reference retained by SQL, so this is **not matched-quality token
efficiency**. No actual model-token counts or generated answers are measured.

The verified current default remains faster in this local workload and still
trails wide-budget history completeness. Independent SciFact/FiQA deficits and
actual agent-task outcomes remain unresolved. The superiority goal stays active.

Initial preflights verified the command, full contexts and position balance.
Final whole-context serialization was moved inside the timer (individual
snippets were already serialized for sizing), and all six orderings were added
to balance predecessor effects on the shared SQL caches. Preflights are excluded
from the final latency results.

## Reproduce

```text
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-reader-crossover --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-reader-control --output results/fresh-reader-control.json --repeats 6 --rust-declarations --shared-stopwords
```

Use fresh paths. Add `--sqlite-porter` for that existing alternative tokenizer.
The current recorded run uses the Unicode control. Private inputs and full
audit packets remain outside Git.

Verify the recorded full responses against the frozen originals with the
independent standard-library Python auditor. It also checks that reference
labels are themselves eligible latest sources at each case's cutoff:

```text
python scripts/verify_nine_evidence.py --report results/nine-reader-crossover3.json --audits data/private/nine-reader-crossover3 --sqlite data/private/nine-scale-run3/sources.sqlite --cases data/private/nine-combined-cases-v1.json
```

[Final aggregate measurements](benchmarks/nine-reader-crossover-2026-10-19.json).
