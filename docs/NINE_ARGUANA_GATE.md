# Frozen scope scorer on untouched ArguAna queries

Protocol fixed before any ArguAna ranking result on 2026-10-19. The preceding
scope experiment improved development and already inspected public data. This
gate evaluates the frozen model on another task before further adjustment.

## Canonical data and unavailable positives

Use all 8,674 corpus records, 1,406 test queries and 1,406 binary positive pairs.
The downloaded archive matches the MD5 in the
[official BEIR dataset list](https://github.com/beir-cellar/beir/wiki/Datasets-available).
All three local corpus/query/qrel files exactly match their archive entries.
No individual query text or ranking outcome has been inspected for this design.

Five positive documents are absent from that canonical corpus, affecting five
queries. Missing source references are an
[acknowledged upstream dataset issue](https://github.com/beir-cellar/beir/issues/172).
Do not invent documents, repair labels or remove those queries. Keep every
canonical query and positive label, count an unavailable positive as a miss,
and report all 1,406 queries in every metric denominator. All engines face the
same five unavoidable misses. No corrected-subset result replaces this primary
evaluation. A perfect retriever can score at most 1,401/1,406 here.

The benchmark normally rejects unavailable qrels. Introduce the explicit
`--allow-unavailable-qrels` benchmark option, keeping defaults strict and
recording the number of unavailable positive pairs, affected queries and
distinct absent documents. The independent auditor recomputes those counts
and refuses undeclared unavailable labels or dropped queries. This modifies
only the experiment loader/reporting, not the database or relevance formulas.
The upstream BEIR evaluator passes canonical qrels to its relevance evaluator;
we preserve those labels rather than selecting a favorable subset.

There are 1,298 identical query/document IDs. Preserve the existing standard
exclusion before context admission and metric scoring. Labels do not enter
retrieval, candidate expansion, source eligibility or feedback. No pseudo
feedback seed, model call or learned query representation is used.

## Frozen native models and common controls

Compare delivered `dirichlet-scope-128-v1` with its immediate preceding native
`dirichlet-128-v1` implementation and SQLite FTS5 Porter with shared stopwords.
Use a separate checkout of the preceding revision; copy only the common BEIR
loader/reporting change into it. Verify that native scorer sources still equal
their respective frozen revisions. Reuse cached dependencies offline and save
distinct binaries. Preserve and restore the exact current release executable
after compiling the prior revision; verify its byte hash.

Build and measure sequentially, with no concurrent compile or other timed run.
Use identical canonical title-plus-newline-plus-body originals, 512 candidate
depth, 32 selected sources, 2 KiB excerpts and 16 KiB serialized context cap.
Disable result caches, retain synchronous receipts, use three measured repeats
after warm-up and alternate native/SQL order within each run. Construct one
native/SQL source pair in the prior run and reopen those exact originals for
the scope run. Verify every source and archive/file digests before comparing.
Keep the same SQLite candidate orders and report any packet changes.

Report nDCG@10, MRR@10, recall@10/32, source-ID context retention and byte cost
separately. Recompute all per-query and aggregate quality metrics independently.
Report paired query differences and fixed-seed 5,000-resample intervals without
using their results to adjust this frozen candidate. Any observed regression
remains visible. Failure against SQL must not be relabeled as superiority.
The goal stays open if ranking, actual agent outcomes or other requirements
remain unproved.

These legacy BEIR query times include receipt, retrieval and snippet sizing;
final whole-context serialization is not independently timed in that command.
Treat them as diagnostic, and keep the preceding balanced complete-response
experiment as the local full-response timing evidence. Separate builds/runs
do not establish a causal speed difference between the two native revisions.
This gate does not measure generated answers, supporting-sentence correctness,
model tokens, repeated agent mistakes, hosted service behavior or write speed.

## Result

The policy implementation passes the full offline 220-test suite, with zero
failures and one physical-GPU test ignored. Tests require explicit permission
for unavailable qrels, count repeated missing references across queries, and
keep an unavailable positive at zero in every quality denominator. A separate
synthetic mixed-label audit retains 1/2 recall rather than dropping its missing
positive, and rejects five corrupt policy/count/metric variants. The updated
auditor also verifies the existing 600 SciFact and 1,296 FiQA packets, whose
labels all resolve and require no opt-in.

An initial build hit the existing report macro's recursion limit after adding
the availability fields. Those fields are now assigned after constructing the
existing JSON object, rather than increasing the crate's recursion limit.
Both frozen release builds succeed. Their common harness bytes are identical;
the prior native source remains exactly its old revision, and the current
release executable is restored byte for byte after the prior build.

No database source or dependency changes. The protocol and shared policy were
committed and pushed before inspecting either run's ranking results. The
completed results below use those fixed binaries, with no later adjustment.

### Frozen ranking result

Both runs complete all 1,406 queries, with one warm-up and three timed passes.
Each supplies 8,436 measured queries and 8,674 original cross-checks. Independent
auditing verifies all 2,812 saved packets per run. Canonical dataset digests and
the native source archive match across runs. All 1,406 SQL audit objects are
identical, including their candidate order, selected sources and quality values.
The five unavailable positives remain zero on every relevant metric in all
four arms. No identical-ID source enters a candidate packet or context.

| Metric | Preceding Nine | Frozen scope Nine | SQLite Porter |
| --- | ---: | ---: | ---: |
| nDCG@10 | 0.541922 | 0.545277 | 0.434472 |
| MRR@10 | 0.461837 | 0.458498 | 0.348259 |
| Recall@10 | 0.795164 | 0.820768 | 0.712660 |
| Recall@32 | 0.882646 | 0.926031 | 0.857752 |
| Relevant source ID retained in context | 0.854908 | 0.877667 | 0.730441 |
| Mean evidence bytes | 16,144.1 | 16,272.4 | 16,333.0 |

Every ArguAna query has one labeled positive, so source-ID context recall and
the fraction of queries retaining all relevant source IDs are equal. Current
Nine retains the positive in 1,234 contexts versus SQL's 1,027. At the same
32-source / 16 KiB limits, 236 questions gain that source and 29 lose it relative
to SQL, for a net gain of 207. The same current model still misses 172 positives,
including the five unavailable ones. This is not perfect retrieval.

Against SQL, the paired nDCG difference is +0.110805, with a fixed-seed
5,000-query-resample 95% percentile interval [0.096634, 0.125280]. There are 614
higher, 600 equal and 192 lower per-query nDCG scores. This is a substantial
average ranking advantage on this previously untouched dataset, not an advantage
on every query or a comparison with vector search or all SQL configurations.

Attribution to the latest model change is smaller: preceding Nine was already
strong on this dataset. Current versus preceding nDCG changes by +0.003355,
interval [-0.003887, 0.010613], with 210 higher, 961 equal and 235 lower scores.
That interval does not establish a clear nDCG improvement over preceding Nine.
MRR falls by 0.003339. Recall@32 improves by 0.043385, with 62 newly retained
positives in the first 32 ranks and one lost. Context retention gains 47 and
loses 15, for a net 32 additional successful source-ID contexts.

The frozen model is retained for its earlier development gains, SciFact/FiQA
improvements and additional ArguAna coverage. Its MRR tradeoff remains visible;
we do not describe it as a uniform improvement over the preceding model.
No query-size rule, coefficient sweep, alternative tokenization or extra context
was used to rescue or enlarge this result. ArguAna is now inspected evaluation
data and must not be called untouched in a future tuning experiment.

### Diagnostic timing and storage

In the scope run, median/p95 query times are 12.58/19.15 ms for Nine and
83.58/162.33 ms for SQL. In the preceding-model run they are 9.72/15.55 and
67.12/139.64 ms. Both SQL quality and all its packets are identical despite the
timing change. Separate sequential runs do not establish a causal latency
effect for the scope model. These legacy timings exclude final whole-context
serialization; they are diagnostic, not a new full-response or hosted SLA.
Use the earlier balanced history reader comparison for complete local responses.

The source archive is 9,080,537 bytes against 8,990,733 original text bytes.
Envelope/frame overhead exceeds compression savings on this corpus. The
archive size excludes derived RAM indexes, query receipts and other service
costs. No compression ratio or model-token advantage is implied by these sizes.
Current context bytes are only about 61 bytes lower than SQL's mean; this is
not a substantial measured context-size advantage.

### Verified delivery and next requirement

The final all-features offline workspace suite passes **220 tests**, zero failed
and one physical-GPU test ignored. Across both runs: **16,872 stable timed
queries, 17,348 per-run original cross-checks and 5,624 independently audited
packets**. No provider call, dependency or database-source change. The current
release executable still matches its frozen scope build byte for byte.

This gate supports a clear average retrieval advantage over the tested SQL
control on a new task, with qualified model-to-model tradeoffs. FiQA still
slightly trails SQL, and actual agent-tool execution, repeated-mistake avoidance,
model-token usage and the current PostgreSQL read/write comparison remain open.
The superiority goal stays active. Those missing operational outcomes should
be measured before adding another ranking hypothesis solely to improve test
scores. Hosted performance is outside this local run.

## Aggregate reports and reproduction

- [Preceding native model and SQL](benchmarks/nine-arguana-prior-2026-10-19.json)
- [Frozen scope model and SQL](benchmarks/nine-arguana-scope-2026-10-19.json)
- [Paired effects](benchmarks/nine-arguana-effects-2026-10-19.json)
- [Frozen binary and source provenance](benchmarks/nine-arguana-provenance-2026-10-19.json)

Use the delivered common harness for both frozen native revisions, with fresh
output paths and the canonical data. The first command creates the source pair:

```text
prior-oojuro-lab nine-beir-bench --dataset data/public/arguana --dataset-name ArguAna --run-dir data/private/fresh-arguana-prior --output results/fresh-arguana-prior.json --repeats 3 --sqlite-porter --shared-stopwords --allow-unavailable-qrels
scope-oojuro-lab nine-beir-bench --dataset data/public/arguana --dataset-name ArguAna --source-run data/private/fresh-arguana-prior --run-dir data/private/fresh-arguana-scope --output results/fresh-arguana-scope.json --repeats 3 --sqlite-porter --shared-stopwords --allow-unavailable-qrels
python scripts/verify_nine_beir.py --report results/fresh-arguana-prior.json --audits data/private/fresh-arguana-prior --dataset data/public/arguana
python scripts/verify_nine_beir.py --report results/fresh-arguana-scope.json --audits data/private/fresh-arguana-scope --dataset data/public/arguana
python benches/experiments/compare_binary_beir.py --before-report results/fresh-arguana-prior.json --before-audits data/private/fresh-arguana-prior --after-report results/fresh-arguana-scope.json --after-audits data/private/fresh-arguana-scope --output results/fresh-arguana-effects.json
```

Do not run builds or other timed comparisons concurrently. The prior checkout
contains only the common evaluation-harness patch; its native scorer is the
unchanged immediate predecessor of the scope model. Native stores and public
per-query audits stay in ignored data directories; only aggregate and provenance
reports are copied to Git after recursive privacy-key checks.
