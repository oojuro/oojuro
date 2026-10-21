# Nine: implementation size and scope

The revised direction is a simple Rust database organized around time,
lossless compression and statistical text retrieval. The new Nine path must not
require Gemini, any embedding model, an LLM or generated summaries. The existing
vector/summary experience prototype is archived in `oojuro-nine-v1`. The revised
engine is implemented experimentally; separate benchmarks must establish any
advantage.

## Measured comparison

Development checkouts were cloned from the official repositories on 2026-10-18.
`cloc` 2.11 counted code lines, excluding blank and comment lines.

| Scope | Files | Code lines |
| --- | ---: | ---: |
| Redis `src`, C and headers | 226 | 145,213 |
| PostgreSQL `src`, C and headers | 2,199 | 994,943 |
| Historical Nine v1 `src/lib.rs`, Rust | 1 | 1,667 |
| Historical Nine v1 `src/tests.rs`, Rust, counted separately | 1 | 936 |

Upstream counts exclude directories named `test`, `tests` and `unit`; root
third-party dependencies, optional modules/contrib and documentation were not
counted. Generated C/header files and inline tests inside otherwise counted
production files may remain. These are scoped source-tree measurements, not
whole-repository totals. C and Rust line counts are not equivalent measures of
features or implementation difficulty.

The existing Nine core count does not include its shared runtime, adapters,
providers, durable jobs or the experimental benchmark. The new engine's budget
below must count the first-party storage helpers it actually depends on, rather
than moving essential implementation into an uncounted helper crate.

Source repositories: [Redis](https://github.com/redis/redis/tree/unstable/src),
[PostgreSQL](https://github.com/postgres/postgres/tree/master/src).
Pinned snapshot identities, count scopes and language totals are in
[the measurement record](benchmarks/source-line-counts-2026-10-18.json).

## Recommended initial budget

The implemented local replacement currently measures **944 code lines** in its
main Rust source, plus **279** in the optional exact-range reader module,
including that module's inline tests, and **182** in the optional shared-context
codec. Counting the **entire 519-line shared core
file**, including unused helpers and inline tests, gives a conservative
**1,924-line** combined count.
The separate local CLI measures 108 lines. Current local-engine tests measure
902 code lines in separate test files, including 153 codec test lines; archived
v1 code, benchmark code and third-party dependencies are
separate scopes. Counts use cloc 2.11, excluding blanks and comments.

The [scope-mass scoring change](NINE_SCOPE_MASS_EXPERIMENT.md) adds 22 engine
code lines and one distinct-term counter per source without adding postings,
dependencies or changing stored source formats. Its new equation/repetition
checks increase separate engine tests by 131 code lines. The
[updated scoped measurement](benchmarks/nine-scope-code-count-2026-10-19.json)
records all six counted files; the two test files are excluded from the combined
engine/core count above.

The 2026-10-19 sparse-scoring and exact-identifier changes added 54 engine code
lines to the previously measured 762-line implementation. The range reader adds
an optional, scanned original-text expansion operation and serialized offsets.
The rejected frequency/presence score mixture is archived as an experiment patch
and is not included in the delivered engine. The test growth
checks equation equivalence, historical behavior and exact identifier retrieval;
benchmark and fixture preparation are outside the engine count.
The later phrase lookup and explicit original-range expansion add 88 combined
engine/reader code lines. SQLite's snippet callback and question routing live
only in the experiment lab, which now enables rusqlite's `functions` feature.
The later count-model diagnostic adds one engine code line. The rejected
logarithmic-count weighting and independent SciFact evaluator are recorded
separately; the delivered model still uses raw counts.

The subsequent fixed normalization and literal-first reader add 104 combined
engine/reader code lines, including reader regressions. Shared storage remains
unchanged. English normalization adds a direct `rust-stemmers` 1.2.0 dependency,
already cached transitively in the benchmark environment. Its complete `src`
tree measures **18,686 Rust code lines in 23 files**, mostly generated language
algorithms, including inline tests. This dependency is outside the first-party
counts above; those counts are not the application's total source footprint.
The normalizer is a fixed algorithm, with no learned model or network call.
An explicit ranking-model diagnostic adds one engine line. The subsequently
rejected hypergeometric scorer is archived separately, not retained in the core.

The optional shared-context codec adds 182 implementation lines plus two module
and export lines. It shares exact text while preserving every source origin,
range and timestamp; six tests add 153 separate test lines. It adds no storage
index or dependency. Optional shared-admission policies and their paired
SQLite control live in the experiment lab, outside this engine count.

These counts keep the engine within its budget. They do not cover a hosted
service, operational systems or SDKs; those remain separate work.

**Target: 1,500–2,000 readable Rust implementation lines. Review the design if
the local core exceeds 2,500.** Tests and benchmark fixtures are counted separately;
they should not be removed to satisfy an implementation budget. Dependencies and
generated files must be reported separately, not presented as nonexistent code.

| Responsibility | Planning allowance |
| --- | ---: |
| Append log, checksums and recovery, including needed shared helpers | 500 |
| Lossless codec choice, deduplication or bounded deltas | 300 |
| Time index and exact reads | 200 |
| Token postings and conditional code-length scoring | 450 |
| Small public API and diagnostics | 200 |
| Total planning allowance | 1,650 |

Allow roughly another 1,000–2,000 lines for meaningful correctness tests and
repeatable benchmarks. These numbers are design budgets, not measured future
implementation sizes or permission to omit required recovery/verification work.

The initial public operations should be write, search and read. Time organizes
durable records; a small content index avoids a full scan when the query does
not specify an exact timestamp. Originals stay recoverable exactly. Conditional
code length is a statistical relevance model; routing entropy is not factual
confidence or a substitute for an actual ranking algorithm.

Avoid adding SQL, table schemas, a workflow runtime, a model client, a hosted
service, an agent framework or multiple client SDKs to this core. A hosted
service needs a separate service and operations layer; this source-size
budget applies only to the engine. Add capabilities
only when a measured requirement justifies them. Source size controls review and
maintenance complexity; performance must still be measured from complete input
query to returned evidence, with durability, storage size and accuracy reported.

## Current completion boundary

The earlier benchmark remains unfavorable to the existing MDL context selector.
This document records the new scope and a measured size recommendation.
The model-free replacement now has recovery, temporal, cache, exact-expansion
and outcome tests. That does not establish that entropy beats vector retrieval
or that Nine is faster than PostgreSQL. The active research goal is still to
verify a superior Nine under the revised constraints.
