# Nine's competitors and comparison boundaries

Research checked 2026-10-19. The current Nine is a native compressed text timeline
with keyword candidates, conditional query code-length ranking and explicit
assessed feedback. It makes no embedding or LLM calls. A fast result must still
retain the evidence an agent needs.

## Reproduced results on the engineering-history pilot

The completed in-process comparison ran 6,720 timed queries over 51 real passages
and 14 frozen retrospective questions. Exact originals were checked, all repeat
packets were stable and no models were called. The source and serialized context
budgets matched. The table below uses eight returned sources, a 64-candidate pool
and a 16 KiB serialized context ceiling; each timing arm has 420 observations.
The 21 reference pairs count each question/source combination; they come from
eight distinct source records, with 28 supporting excerpts. This small probe
cannot establish general retrieval quality.

| Engine | Uncached p50 ms | Uncached p95 ms | Exact-query cache p50 ms | Question/source pairs retained / 21 | Questions with all evidence / 14 | Mean context bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Nine | 0.202 | 0.570 | 0.130 | 18 | 11 | 3,857 |
| Tantivy 0.27.0 | 0.320 | 0.709 | 0.155 | 21 | 14 | 5,858 |
| SQLite FTS5 3.53.2 | 0.487 | 1.175 | 0.199 | 21 | 14 | 5,879 |
| Memvid 2.0.140, lexical | 5.548 | 8.113 | 0.453 | 19 | 12 | 5,461 |

[Aggregate report, including two-source and cached controls](benchmarks/nine-competitors-pilot-2026-10-19.json).
Nine is faster in these uncached paths but loses reference evidence. This does
not establish superior answer accuracy, token efficiency or task completion.
Memvid is measured through its public primary search API, which also performs
snippet/enrichment work; this is not a claim that its bare BM25 scorer takes
5.5 ms. Its vector features were disabled.

The [separate PostgreSQL/pgvector experiment](NINE_POSTGRES_BENCHMARK.md) includes
loopback transport through equal-limit Linux containers. Nine's eight-source
retrieval p50 is 3.448 ms, PostgreSQL full-text 4.417 ms, and PostgreSQL hybrid
4.818 ms with an existing query embedding. The complete fresh Gemini-plus-hybrid
path is 434.524 ms. These service timings must not be mixed with the in-process
table. PostgreSQL retains all reference evidence at eight results; Nine retains
18 of 21. PostgreSQL also wins the small-write groups in this run.

### Harness corrections and stopped runs

The first original-recovery check caught a benchmark adapter error: Memvid's
insert return value is a WAL sequence, not a committed frame ID. The adapter now
resolves committed source URIs and hydrates both text and metadata from Memvid.
Another preliminary run used Memvid's implicit-AND natural-language query while
other engines used OR terms. The final comparison gives the lexical engines the
same deduplicated, quoted OR terms and does not use that failed query setup's
zero-recall result.

Two further attempts stopped during historical-view creation or ingestion on
Windows. The final harness builds and closes historical Memvid files, then
reopens only the current case's file before timing. Cases are processed in order
with rotated engine arms and a warm-up per case. This bounds the number of live
Memvid views; it does not establish the underlying cause of the prior failures
or a vendor reliability defect. The successful run's setup includes roughly
24.4 seconds to build its historical Memvid views, outside query timing.

## Relevant products and engines

| System | Why it matters for Nine | What to measure |
| --- | --- | --- |
| [Memvid](https://github.com/memvid/memvid) | Particularly close: a Rust, compressed single-file agent-memory engine with versioned history, model-free lexical search and optional vector features. | Actual lexical search, source recovery, source/index size, startup and updates; distinguish its public primary search API from fallback search. |
| [Mem0](https://mem0.ai/blog/mem0-the-token-efficient-memory-algorithm) | A directly competing memory product: add-only LLM extraction with retained history and semantic, keyword and entity retrieval. | Full ingestion and query costs, retrieved evidence, answer quality and token use. The managed platform has proprietary optimizations absent from its open-source SDK. |
| [Zep / Graphiti](https://help.getzep.com/v4/llms.txt) | Temporal context graphs, evolving facts and cross-session agent memory; Graphiti is the open framework and Zep the managed product. | Both time cutoffs, correction handling, indexing readiness, full query latency and verified task outcomes. Do not treat a local Graphiti run as a measured Zep Cloud result. |
| [Supermemory](https://supermemory.ai/research/) | A managed memory competitor with published conversation and agent-memory evaluations. | Reproduce retrieval/answer quality with the same answer model, context budget and complete latency boundary; distinguish recall from answer accuracy. |
| [Letta](https://docs.letta.com/v1-sdk/concepts/stateful-agents) | An adjacent stateful-agent platform; its documented memory blocks and retained messages place state in the agent's execution loop. | Whole agent task completion, context management and memory changes, not just a storage lookup. The linked source describes its v1 concepts. |
| [Tantivy](https://github.com/quickwit-oss/tantivy) | A strong direct Rust retrieval baseline: BM25, compressed stored originals and persistent indexes, without requiring a model. | In-process query time, matching source budgets, cold/cached controls, exact recovery and index residency. It is an engine library rather than a managed agent-memory product. |
| [PostgreSQL / Supabase](https://supabase.com/docs/guides/database/overview) | A customer may already operate PostgreSQL through Supabase; a separate database must justify its extra operational burden. | Durable indexed append, exact read, historical retrieval, concurrency, total residency and cost. A local PostgreSQL measurement excludes the hosted Supabase service. |
| [pgvector](https://github.com/pgvector/pgvector) | The vector extension inside PostgreSQL provides a conventional semantic and keyword-hybrid alternative. | Exact and approximate recall, index build/update cost and complete fresh-query latency including encoding. |
| [Qdrant](https://qdrant.tech/documentation/quickstart/) | A Rust vector engine and hosted service, with sparse/dense search paths; self-hosted query encoding remains a separate component. | Comparable source/candidate filters, encoder identity, fresh query encoding, and client-visible service latency. |

The [large-corpus and return-budget controls](NINE_SCALE_BENCHMARK.md) extend
the experiment to 18,495 records. Returning 32 results recovers all frozen small
pilot reference pairs. On the large corpus, even 128 results miss one historical
reference, and the mechanical code-declaration probes favor FTS5 on coverage.
Those controls separate ranking, result count and the serialized evidence cap.

SQL is a query language. PostgreSQL is a database engine; Supabase runs PostgreSQL
as part of a managed platform; pgvector extends PostgreSQL. These names should
not become three independent engine results in a comparison chart.

## Published performance is context, not a reproduced comparison

Mem0's April 2026 report gives 92.5 on LoCoMo with 6,956 mean retrieved tokens,
and 94.4 on LongMemEval with 6,787. These are vendor-reported, managed-platform
results under a single-pass evaluation, not measurements on the engineering-history pilot.
[Mem0 methodology and limitations](https://mem0.ai/blog/mem0-the-token-efficient-memory-algorithm).

Supermemory reports 97% Recall@20 with aggregation on 500 LongMemEval-S questions.
This is a recall metric under that protocol; it must not be placed beside Nine's
reference coverage or Mem0's answer score as if they were the same measure.
[Supermemory research](https://supermemory.ai/research/).

Zep's older v3 documentation reports sub-200 ms retrieval targets and historical
benchmark observations, while current v4 documents describe a different API and
operational guidance. Version, processing stage and deployment must be fixed
before reproducing a latency claim. [v3 retrieval philosophy](https://help.getzep.com/v3/retrieval-philosophy),
[v4 performance guidance](https://help.getzep.com/v4/performance.md).

Memvid advertises fast search and portable versioned memory; Tantivy publishes a
full-text benchmark framework. Their numbers come from different workloads and
cannot be compared directly with a 51-passage in-process Nine measurement.
[Memvid architecture](https://memvid.com/blog/introducing-memvid-v2-portable-deterministic-memory-for-ai),
[Tantivy benchmark and scope](https://github.com/quickwit-oss/tantivy).

## Reproducible local comparisons

The existing [SQLite FTS5 pilot](NINE_LOCAL_BENCHMARK.md) measures actual engines,
matched source/candidate budgets and durable query receipts. It found lower local
latency and weaker reference retention for Nine. It does not establish an answer
accuracy or token-use advantage.

The optional `competitors` lab feature adds actual Memvid 2.0.140 in lexical mode,
Tantivy 0.27.0, PostgreSQL and pgvector. These are benchmark dependencies; the
Nine engine remains independent of them. Private data stays in ignored run
directories, and aggregate reports may be committed.

```text
cargo run --release -p oojuro-lab --features competitors -- nine-competitor-bench --dataset data/private/pilot-benchmark-v1 --run-dir data/private/new-comparison --output results/new-comparison.json --repeats 30
```

Local engine comparisons build eligible historical snapshots outside timing to
avoid future information influencing corpus statistics. They return equal source
and byte budgets and include exact original hydration, serialization and a synced
query receipt. Both uncached and exact-query-cache controls are included.
The source/index batch builds commit once for Tantivy and Memvid, while Nine
syncs each source append; those setup durations are not matched write latencies.

The PostgreSQL comparison uses two local Linux containers with equal CPU/RAM
caps. Both are accessed through persistent loopback connections. Nine's framing
protocol exists solely for this controlled benchmark; it has no production
authentication or general tenant service contract. PostgreSQL must have fsync,
full-page writes and synchronous commit enabled. No production database is used.
This run used local services; no hosted Supabase project is
provisioned or queried.

Bare writes and exact reads are separate from retrieval. Writes are individually
acknowledged; exact full records are checked. Payloads include approved redacted pilot
originals and public deterministic 1 KiB, 8 KiB and 64 KiB text fixtures. The first
run is single-client and does not establish concurrent database throughput.
The pilot corpus contains visible conversation excerpts; its raw tool
payloads were excluded during preparation. The larger tool-output-shaped write
fixtures are synthetic, so they do not establish compression on real tool logs.

For semantic retrieval, 3,072-dimensional frozen document embeddings are indexed
in PostgreSQL. Small-corpus queries use exact full-precision pgvector search.
At larger scale, compare an appropriate ANN configuration as well: pgvector's
documented HNSW type limits require `halfvec` or another supported representation
for 3,072 indexed dimensions. A vector column can still store those dimensions
for exact search.

Cached query-vector timings are labeled separately. An explicit live test can
time Gemini query encoding plus PostgreSQL hybrid retrieval together under the
existing provider budget; the Nine server still makes no model calls. Encoding
documents during ingestion is an additional cost and is not included in a bare
text-write measurement. [Supabase's hybrid-search guide](https://supabase.com/docs/guides/ai/hybrid-search)
describes the conventional full-text/vector fusion baseline.

## Hosted and agent-task claims still require evidence

Local PostgreSQL is useful engine evidence for the Supabase alternative. Real
hosted Supabase latency additionally includes its selected compute tier, region,
connection path, network, authorization policy and API layer. A dedicated hosted
test project would be needed for a future managed-service result. This run used
local measurements only.

No locally reproduced Mem0 Platform, Zep Cloud, Supermemory or hosted Supabase
result is implied by their published scores. Run matched deployments before
ranking those services by latency or cost. Measure answer quality and actual
model usage separately, and use verified task executions before claiming fewer
repeated agent mistakes. Source size, compression or entropy alone proves none
of these product outcomes.

## Next decision gate

Freeze a larger unseen corpus and questions before tuning Nine again, including
real redacted tool outputs, corrections, paraphrases and temporal distractors.
Compare latency and context size at matched evidence coverage. Then run the
same agent tasks and verifier with and without memory to measure actual token
usage, task correctness and repeated failures. Keep the answer model, tool state,
source access and evaluation budget fixed. The current 14-question pilot is
useful for finding failures; it is not enough to select a hosted database product.
