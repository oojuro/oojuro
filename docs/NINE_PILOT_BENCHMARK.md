# The Nine versus hybrid retrieval: engineering-history pilot

Run on 2026-10-18. **The current Nine selector did not improve this workload.**
It discarded useful evidence, answered fewer questions and took more local CPU
time. Its smaller prompt was also achievable with a simpler hybrid configuration.

This is a small retrospective engineering-history pilot, not a production
acceptance test or an independently verified agent-task benchmark. Source text,
questions, answer keys, model answers and source identities remain local and
ignored. Only the Rust harness and aggregates are published here.

## Results

Free-response answers saw no multiple-choice options. Correctness was graded by
the same model family in a separate call with anonymous, rotated arm labels and
source-supported references. These are **blind model grades, not human gold**.

| Method | Free responses correct | Mean LLM input tokens | Mean LLM output tokens | Local retrieval p50 | Local retrieval p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| BM25 + Gemini vectors, up to 8 snippets | 14/14 | 1675.4 | 149.6 | 0.828 ms | 1.442 ms |
| BM25 + Gemini vectors, up to 2 snippets | 14/14 | 653.9 | 141.6 | 0.784 ms | 1.401 ms |
| The Nine, MDL selection, up to 8 snippets | 2/14 | 686.4 | 109.5 | 30.229 ms | 57.253 ms |
| No memory | 0/14 | 176.8 | 53.9 | Not applicable | Not applicable |

The Nine abstained on 12 free-response questions. On the 11 factual questions it
scored 0/11; on the three corrective-action questions it scored 2/3. Both hybrid
variants scored 11/11 and 3/3 respectively. The no-memory arm abstained on all 14.
Nobody recommended the designated known bad action in the free-response test.
These observations do not show that The Nine prevents repeated agent mistakes.
Abstention is not a repeated mistake, and it is not a completed task.

The Nine used about 59% fewer input tokens than eight-snippet hybrid, but about
5% more than two-snippet hybrid. Mean marginal context tokens, measured by the
provider-reported prompt-token difference from the identical no-memory request,
were 1498.6, 477.1 and 509.6 for hybrid-8, hybrid-2 and Nine. Across 14 free responses,
input plus output tokens were 25,550, 11,136 and 11,142 respectively. Summarization,
index construction, embeddings, graders and question-authoring calls are not
included in those answer-token totals. There was no generative summary in the
retrieval corpus for this experiment, so both methods used the same source text.

Median local retrieval for Nine was about 36.5 times hybrid-8 in this small store.
This ratio applies only to the measured local retrieval and context preparation,
including synchronous query journaling. It excludes store opening, embeddings,
network and LLM generation. The one-time shared remote query-embedding measurements
had p50 409.6 ms and p95 530.4 ms; both methods used those same vectors. Do not add
component percentiles and call the sum an end-to-end percentile.

## What failed

All 21 reference-cited passages were present in both methods' ranked candidate
pools. The context actually sent to the LLM retained 21/21 for hybrid-8, 19/21 for
hybrid-2 and 2/21 for Nine. Reference citations can be redundant and are not an
independently established minimal evidence set, but the large drop and the
free-response results both identify context selection as the immediate problem.

Nine minimizes measured LZ4 representation bytes plus a weighted uncovered-feature
penalty. Its features are words, episode/role labels and recorded conditions.
Those coverage features did not preserve the question-specific facts in this
corpus. A lower compression objective did not imply a better answer. The selector
also repeatedly serializes and compresses proposed context packets, adding CPU
work. No engine algorithm, weight or threshold was adjusted after seeing results.

Across the final repeated-query timing run, serialized context packets were
unchanged from the frozen retrieval pass for every method. Merely appending query
history did not improve these outputs. This build has no learned response cache;
no task-outcome feedback or gold evidence assessments were injected in the test.
This does not test what a future calibrated feedback policy could achieve.

## Data and question construction

- Twelve related local engineering sessions from one private project checkout
  were selected. The redacted-history provider boundary and one shared
  USD 5 budget applied.
- The importer produced 52 visible user/assistant messages. Exact duplicates were
  removed, leaving 51 lossless passages and 24,583 UTF-8 text bytes. Internal
  reasoning, raw tool arguments/results and execution metadata were excluded.
- Both methods received identical UTF-8 chunks, source times, source IDs, model
  embeddings and prompt serialization. All source claims stayed unverified.
  The 6000-byte chunk ceiling was shared. No LLM summaries or answer cache were used.
- Question authoring requested 12 factual and up to six genuine failure/correction
  probes in small batches. Eighteen proposals yielded 14 accepted questions:
  11 factual and three corrective-action probes. Four were rejected for invalid
  source support, malformed fields or failed audit checks.
- Quoted evidence had to be an exact substring of the cited passage. A separate
  full-evidence model call checked support, ambiguity and genuine correction
  status before questions were frozen. The authors and auditor did not see
  retrieval output. These checks do not replace independent human verification.
- Every question's historical cutoff was one second after its latest cited
  source. Sources newer than either query time cutoff were invisible to both
  methods. Questions are newly authored paraphrases, not actual held-out user
  requests. They were frozen before embedding and retrieval.

## Matched retrieval and timing

The baseline is an independent Rust BM25 implementation plus exhaustive cosine
similarity over the same Gemini Embedding 2 vectors at 3072 dimensions, fused by
reciprocal rank with constant 60. BM25 uses the same tokenization, k1=1.2 and b=0.75.
A test confirms its candidate ordering matches Nine with adaptation disabled.
The baseline has precomputed term postings, document frequencies scoped to the
historical visible rows, and vector norms; it is not an intentionally slow text
scan. It does not use PostgreSQL, SQLite or a third-party vector service.

Both methods have a 64-candidate pool, 128 candidates per channel, 16,384-byte
serialized context limit and exact model-matched vectors. Hybrid packs ranked
snippets directly, skipping whole snippets that exceed the byte budget. Nine
uses its existing default coverage penalty of 4096, up to eight context records,
and its normal entropy/episode behavior. Session membership is the available
episode grouping; fine causal tool links were not reconstructed.

Each local query includes a synchronous native query-journal append. Baseline and
Nine retain candidate and context IDs in their traces. Neither receives a
RAM-only durability advantage. Store open/replay is outside the measured interval.
The final release-build run alternated method order, used two warmup rounds, and
measured 30 rounds over all 14 questions: 420 samples per method. The initial run
overlapped a build and was kept privately as a diagnostic; the table uses the
subsequent run after compilation/tests/provider calls completed.

Hardware: Windows, Intel Core Ultra 7 258V, eight logical processors, 31.51 GiB
physical memory. Rust 1.99.0, optimized release build. No GPU scoring was used.
Operating-system background activity and power management were not controlled.
This tiny store does not establish throughput, scale or production p99 latency.

## Model evaluation and the multiple-choice trap

All arms used the same Fireworks GLM 5.3 Flash model, temperature zero, evaluation
reasoning setting low, system prompt and bounded completion allowance. Request
order rotated by question. Document/query embeddings were generated once and
reused by every method. Each answer was sampled once per method and mode;
successful calls were cached, not silently retried or resampled.

The initial multiple-choice diagnostic scored hybrid-8 14/14, hybrid-2 14/14,
Nine 12/14 and no-memory 10/14. All methods avoided the three known bad options.
The high no-memory score made that diagnostic a poor measure of memory benefit.
The subsequent free-response diagnostic removed options while preserving the
same frozen questions, corpus, retrieval packets and model. It was added after
observing that weakness, so it is explicitly a follow-up diagnostic rather than
an independently preregistered confirmatory experiment. Full free-response
aggregate results are reported above; the favorable multiple-choice numbers are
not presented as Nine's answer accuracy.

No real tool calls, deploys, repairs or user tasks were executed. The corrective
action score is a read-only planning proxy. Preventing repeated real failures
requires longer agent rollouts with independently verified outcomes and
opportunities to repeat specific mistakes. The tiny correlated single-project
sample, model-generated labels and shared model family limit generalization.

## Execution, cost and reproduction

All-features workspace tests: **183 passed, zero failed, one opt-in GPU test
ignored**. New tests cover historical baseline visibility and byte limits,
independent fusion parity, parser/percentile behavior and budgeted evaluation
completion handling. `cargo fmt --all --check` and `git diff --check` passed.

The first large authoring request hit its output ceiling. Its pessimistic spending
reservation remains retained. Smaller bounded batches completed; malformed or
unsupported cases were rejected. All calls used the existing shared ledger.
Cumulative published-rate settlements for the entire provider experiment were
USD 0.0327081, with USD 0.02005635 retained reservations: USD 0.05276445 accounted
or reserved against the USD 5 cap. These totals include earlier smoke tests;
they are not provider invoices and are not the online per-query operating cost.
Configured credentials were kept in process memory. Private inputs/outputs,
source paths, model payloads, vector caches and native stores are not committed.

The harness is the research lab's `nine_eval.rs` (not part of this repository).
[Aggregate machine-readable results](benchmarks/nine-pilot-2026-10-18.json)
contain no private questions or source text. Reproducing exact historical scores
requires the private frozen inputs and provider behavior; this source checkout
alone does not contain that dataset.

With separately authorized redacted imported JSONL files, an inputs JSON array
listing those projections, configured provider environment variables and the
single existing shared budget ledger, the phases are:

```text
cargo build --release -p oojuro-lab
target/release/oojuro-lab nine-eval-build --dir data/private/NEW_RUN --inputs data/private/inputs.json
target/release/oojuro-lab nine-eval-author --dir data/private/NEW_RUN
target/release/oojuro-lab nine-eval-embed --dir data/private/NEW_RUN
target/release/oojuro-lab nine-eval-retrieve --dir data/private/NEW_RUN
target/release/oojuro-lab nine-eval-answer --dir data/private/NEW_RUN
target/release/oojuro-lab nine-eval-open --dir data/private/NEW_RUN
target/release/oojuro-lab nine-eval-latency --dir data/private/NEW_RUN
target/release/oojuro-lab nine-eval-report --dir data/private/NEW_RUN
```

Use `.exe` on Windows. Frozen corpus/cases/reports refuse overwrites; failures are
recorded without automatic paid retries. A new dataset directory does not create
a new spending allowance: model calls still use `data/private/provider-budget`.

## Decision

Use ordinary hybrid retrieval as the working baseline. The next research change
should make evidence retention and query relevance explicit constraints on
compression, with a safe retrieval fallback and held-out evaluation. The current
MDL surrogate is not supported as an accuracy or latency improvement. This result
does not establish that information-theoretic approaches cannot work; it rejects
the present selector for this measured workload.
