# Documentation

## Product

| Document | What it covers |
|---|---|
| [DESIGN](DESIGN.md) | the record model, capture boundary, stores, settings, explicit use, and the principle of staying simple and horizontal |
| [OJO_FORMAT](OJO_FORMAT.md) | the adopted `.ojo` extension, current directory storage, and remaining standalone container work |
| [SUPPORTED_INTERFACES](SUPPORTED_INTERFACES.md) | native local journal and current hosted clients; retirement of legacy front doors |
| [CAPTURE](CAPTURE.md) | attach-and-record ingestion of agent logs, the recording proxy, redaction on ingest |
| [SHARED_LOG](SHARED_LOG.md) | the shared write log and the measurements behind making it the default |
| [ACTIVITY_SEARCH](ACTIVITY_SEARCH.md) | design and measurements for finding moments by date and by how the activity looked (no engine code yet) |

## Benchmarks and performance

| Document | What it covers |
|---|---|
| [TRACEBENCH](TRACEBENCH.md) | the agent-trace database workload and how to run it |
| [TRACEBENCH_BASELINES](TRACEBENCH_BASELINES.md) | how Postgres, ClickHouse, SQLite and every competitor arm are configured, and the results |
| [BENCH_HARNESS](BENCH_HARNESS.md) | `oojuro-bench`: BEIR, turn-loop replay, memory QA and correctness gates |
| [PERF_ROUND2](PERF_ROUND2.md) | perf round 2: per-store cache, dedupe key, lean index, read paths, long queries, fsync audit |
| [PERF_SEARCH](PERF_SEARCH.md) | search latency at scale |
| [PERF_STORE](PERF_STORE.md) | the compressed store: dictionaries, tiers, compaction |
| [SEARCH_FIXES](SEARCH_FIXES.md) | two search bugs found by an outcome evaluation, their fixes and the measurements |
| [TRACK_COMPRESS](TRACK_COMPRESS.md), [TRACK_LEXICAL](TRACK_LEXICAL.md), [TRACK_TRACEDB](TRACK_TRACEDB.md) | research logs of the compression, lexical-search and trace-database work: hypotheses, measurements, what was kept and rejected |
| [benchmarks/](benchmarks/) | the machine-readable results behind the documents above (aggregate numbers only) |

## Research record (the Nine engine experiments)

`ENGINE_CONTRACTS.md` and the `NINE_*` documents record the experiments that led to the current text
engine (`crates/oojuro-nine`): scoring models, lexical normalization, excerpt readers, comparisons with
SQLite FTS5, PostgreSQL and other engines, and the experiments that were rejected. They are published as the
record of what was tried and measured, including negative results.

These experiments were run with a research lab and private evaluation data that are **not part of this
repository**. The commands in those documents (`oojuro-lab ...`) and the crates they name (other than
`oojuro-nine`) do not exist here, and the pilot corpus is not distributed. The aggregate results are in
[benchmarks/](benchmarks/); the rejected-experiment patches are in [`../benches/experiments/`](../benches/experiments/).

| Document | What it covers |
|---|---|
| [ENGINE_CONTRACTS](ENGINE_CONTRACTS.md) | what any retrieval or memory engine has to prove to be worth adopting |
| [NINE_CODE_BUDGET](NINE_CODE_BUDGET.md) | implementation size and scope of the engine |
| [NINE_COMPETITORS](NINE_COMPETITORS.md), [NINE_LOCAL_BENCHMARK](NINE_LOCAL_BENCHMARK.md), [NINE_POSTGRES_BENCHMARK](NINE_POSTGRES_BENCHMARK.md), [NINE_PILOT_BENCHMARK](NINE_PILOT_BENCHMARK.md), [NINE_SCALE_BENCHMARK](NINE_SCALE_BENCHMARK.md) | comparisons with other engines and the large-payload runs |
| [NINE_RETRIEVAL_IMPROVEMENTS](NINE_RETRIEVAL_IMPROVEMENTS.md), [NINE_LEXICAL_NORMALIZATION](NINE_LEXICAL_NORMALIZATION.md), [NINE_COUNT_EXPERIMENT](NINE_COUNT_EXPERIMENT.md), [NINE_SCOPE_MASS_EXPERIMENT](NINE_SCOPE_MASS_EXPERIMENT.md), [NINE_ARGUANA_GATE](NINE_ARGUANA_GATE.md) | scoring and normalization of the shipped engine, with independent public gates |
| [NINE_PHRASE_RETRIEVAL](NINE_PHRASE_RETRIEVAL.md), [NINE_PASSAGE_EXPERIMENTS](NINE_PASSAGE_EXPERIMENTS.md), [NINE_READER_CROSSOVER](NINE_READER_CROSSOVER.md), [NINE_CONTEXT_FRONTIER_EXPERIMENT](NINE_CONTEXT_FRONTIER_EXPERIMENT.md), [NINE_SHARED_CONTEXT_EXPERIMENT](NINE_SHARED_CONTEXT_EXPERIMENT.md) | exact-source excerpts, readers and context budgets |
| [NINE_BAYES_FIELD_MAPPING_EXPERIMENT](NINE_BAYES_FIELD_MAPPING_EXPERIMENT.md), [NINE_COMPLEMENTARY_CONTEXT_EXPERIMENT](NINE_COMPLEMENTARY_CONTEXT_EXPERIMENT.md), [NINE_CONDITIONAL_COMPRESSION_EXPERIMENT](NINE_CONDITIONAL_COMPRESSION_EXPERIMENT.md), [NINE_FIELD_INFORMATION_EXPERIMENT](NINE_FIELD_INFORMATION_EXPERIMENT.md), [NINE_FIELD_MIXTURE_EXPERIMENT](NINE_FIELD_MIXTURE_EXPERIMENT.md), [NINE_HYPERGEOMETRIC_EXPERIMENT](NINE_HYPERGEOMETRIC_EXPERIMENT.md), [NINE_NATIVE_BM25_EXPERIMENT](NINE_NATIVE_BM25_EXPERIMENT.md), [NINE_ORDERED_PAIR_EXPERIMENT](NINE_ORDERED_PAIR_EXPERIMENT.md), [NINE_PSEUDO_FEEDBACK_EXPERIMENT](NINE_PSEUDO_FEEDBACK_EXPERIMENT.md), [NINE_QUERY_FEATURE_EXPERIMENT](NINE_QUERY_FEATURE_EXPERIMENT.md), [NINE_SMOOTHING_EXPERIMENT](NINE_SMOOTHING_EXPERIMENT.md), [NINE_TEXTUAL_PROVENANCE_EXPERIMENT](NINE_TEXTUAL_PROVENANCE_EXPERIMENT.md) | experiments, most of them rejected, each with its measurements |
