# Model-free Nine: local SQLite FTS5 comparison

Measured on 2026-10-18 in the local timezone of the test machine. The current engine uses no embeddings, LLM or generated
summaries. The [aggregate measurement](benchmarks/nine-local-v2-pilot-2026-10-18.json)
contains no private source text, questions or retrieved packets.

**Result:** Nine was faster on this small local workload, while SQLite FTS5
retained more supporting evidence. This does not establish overall superiority,
answer accuracy, token savings or fewer repeated agent failures.

## Workload and timing boundary

The frozen retrospective pilot contains 51 approved redacted engineering-history text
passages, 24,583 UTF-8 source-text bytes and 14 source-supported questions. The
reference has 21 question/source pairs and 28 quoted excerpts, drawn from eight
distinct source records. Multiple excerpts may cite one source; a source needed
by different questions is counted for each question. It was already used to assess
the historical v1 prototype, so it is not a fresh held-out production workload.
The current comparison has no tool execution or agent task rollout.

Each method ran 420 timed queries after a warm-up pass, with alternating order.
Complete local timing includes query processing, ranking, a synchronously durable
query receipt, exact-source hydration and evidence serialization. It excludes
database open/replay, network transport and a future service's authorization,
queuing and accounting. There is no query embedding time to omit: these methods
make zero model calls. These are not hosted-service latency measurements.

Both methods use the same source texts and clocks, up to 64 reference candidates,
complete-source context of at most two or eight records, and a 16,384-byte
serialized evidence budget. Cached controls use exact frozen query/time identity.
Uncached means the result cache is bypassed, not that OS/disk caches are cold.
No source or outcome mutations occur between repetitions; packets were identical
across all 3,360 timed queries.

## Results

| Method | Local p50 ms | Local p95 ms | Question/source pairs retained / 21 | Questions retaining all references / 14 | Mean context bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Nine, uncached, two sources | 0.133 | 0.361 | 15 | 10 | 1,522 |
| SQLite FTS5, uncached, two sources | 0.308 | 0.725 | 18 | 11 | 1,770 |
| Nine, cached, two sources | 0.087 | 0.298 | 15 | 10 | 1,522 |
| SQLite FTS5, cached, two sources | 0.119 | 0.327 | 18 | 11 | 1,770 |
| Nine, uncached, eight sources | 0.144 | 0.400 | 18 | 11 | 3,857 |
| SQLite FTS5, uncached, eight sources | 0.329 | 0.680 | 21 | 14 | 5,879 |
| Nine, cached, eight sources | 0.099 | 0.321 | 18 | 11 | 3,857 |
| SQLite FTS5, cached, eight sources | 0.147 | 0.375 | 21 | 14 | 5,879 |

Rounded values above are a reading aid; the JSON records the actual samples'
summary. Smaller context is not automatically token efficiency: Nine omitted
some required evidence. No LLM answered these packets, and no model tokenizer
or usage counter measured input tokens. Routing entropy is not an accuracy score.

## Baseline configuration

The Rust lab uses actual bundled SQLite 3.53.2 through rusqlite 0.40.2 and FTS5
BM25. The durable source store uses WAL, `synchronous=FULL`, a 128 MiB cache
allowance, prepared statements and one committed ingestion transaction per
record. Ordinary source reads and durable query receipts use that store.

FTS5's collection statistics are global to a virtual table. To prevent future
records contaminating historical ranking, the benchmark prebuilds a separate
eligible latest-revision view for each question in memory. That view preparation
is excluded from timing, giving the baseline a favorable static historical
snapshot. It is not the cost of maintaining a general temporal SQLite service.

References: [SQLite FTS5](https://sqlite.org/fts5.html),
[rusqlite Connection API](https://docs.rs/rusqlite/0.40.2/rusqlite/struct.Connection.html).
The previous [ordinary hybrid comparison](NINE_PILOT_BENCHMARK.md) remains
specific to Nine v1. This run does not freshly measure Gemini hybrid retrieval
or PostgreSQL; their results must not be inferred from SQLite's numbers.

## Storage and ingestion

All 51 complete input records serialize to 47,484 bytes, including metadata.
Nine's native source archive is 44,400 bytes and has 38 bounded delta records;
the source text alone is only 24,583 bytes. Thus the archive exceeds the plain
text size on this tiny corpus. Original text, metadata and digests are retained.
Exact complete-record recovery was verified after reopen.

The SQLite source table and persistent FTS5 index occupy 106,496 bytes. This is
not an equivalent total-residency comparison: Nine rebuilds its RAM text/postings
index, while SQLite persists its full-text index. The separate prebuilt SQLite
historical views are also memory resident. No per-engine RAM measurement was
made, and receipt growth/backup cost is not included in these archive figures.

Nine ingested the corpus in 20.29 ms, SQLite in 20.02 ms. Nine reopen, including
source reconstruction, digest verification and index rebuild, took 15.23 ms.
These are single observations, not throughput distributions or a scale claim.

## Reproduce locally

Approved private projections, frozen cases, raw timings and packets remain in
ignored local storage. With an equivalent private dataset:

```text
cargo run --release -p oojuro-lab -- nine-local-bench --dataset data/private/pilot-benchmark-v1 --run-dir data/private/local-new-run --output results/local-new-run.json --repeats 30
```

Use a new run directory and report filename. The program refuses to overwrite
them. It checks literal reference support and historical availability, recovers
all originals exactly, and writes only aggregate results to the public report.

## Next proof obligations

Improve evidence retention on a development set, then freeze fresh held-out
questions. Measure actual generated answers and model usage with an external
test agent; that model is separate from the Nine engine. Add verified task
executions before claiming fewer repeated mistakes. Measure larger tool-output
corpora, total residency, startup, updates and hosted request overhead before
claiming cost or latency advantages.
