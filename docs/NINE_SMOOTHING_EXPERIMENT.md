# Smoothed query-likelihood controls

Protocol fixed on 2026-10-19 before candidate evaluation. Starting source:
`015aa5f`. The delivered ranker uses Dirichlet smoothing with fixed `mu = 128`.
Metadata indexing and shorter excerpts were rejected in the previous experiment.

## Motivation and one fixed candidate

[Zhai and Lafferty (2004)](https://dl.acm.org/doi/pdf/10.1145/984321.984322)
study how smoothing affects query-likelihood retrieval. Their Section 10.1
estimates the Dirichlet parameter using leave-one-out likelihood. That estimator
requires token-frequency statistics; document length alone is not its estimate.
The paper motivates examining the parameter, not the heuristic below.

This first low-cost candidate is **our heuristic**, with no optimality claim:
replace the ordinary-word query's fixed prior strength by the median indexed
feature length of its eligible candidate view C. For sorted lengths a:

```text
n       = |C|
mu(C)   = max(1, (a[(n-1)/2] + a[n/2]) / 2)   if n > 0
mu(C)   = 128                                  otherwise
PB(w)   = (sum(d in C, tf(w,d)) + P0(w)) / (sum(d in C, len(d)) + 1)
P(w|d)  = (tf(w,d) + mu(C) * PB(w)) / (len(d) + mu(C))
score   = sum(w in q, tf(w,q) * log2(P(w|d)/PB(w))) + usefulness_bits(q,d)
```

Indices in the median formula use integer division. Compute it with linear
selection and an extra scan for the lower middle, rather than fully sorting.
Both query clocks and the lower time bound apply before this statistic. Future,
late-known and superseded sources cannot alter an earlier view's parameter.
Nonmatching sources do not enter the existing query-addressed background model.

Quoted phrases, recognized complete identifiers, direct keys and empty queries
keep fixed `mu = 128` and the existing path. There is no new event taxonomy,
embedding, language model call, external service, storage index or dependency.
The archive, raw counts, Snowball map, sparse scoring identity, outcome assessment
and 2 KiB reader stay unchanged. Median fitting adds temporary O(|C|) integers
and expected linear selection work. It is not backpropagation or semantic
learning, and a smaller query code is not proof of factual correctness.

## Gates before evaluation

Test sparse-versus-dense probability equations with an independently sorted
median oracle, a hand-computable score, both clocks, revisions, lower bounds,
future/nonmatching invariance, literal paths and reopen behavior. Report fitted
prior values as diagnostics. Passing implementation tests does not establish
retrieval quality.

Use the frozen 18,495 originals and 129 development cases with the same 512
candidates, 32-source cap, 2 KiB reader and 16/64 KiB context budgets. Three
timed repeats follow warm-up; clear result caches and retain synced receipts.
Compare with both the original body-only SQLite control and its previously
stronger optional configurations. No metadata index or shared admission is used
to change this candidate's native quality gate.

Adoption requires additional complete reference evidence without losing any
existing complete check at either budget: history 19/21, documents 19/20,
86/86 declarations and nine huge-tool checks. Check individual retained pairs,
supporting quotes and complete questions, not only aggregate totals. Report
latency and byte sizes independently, with exact-source and range auditing.

If development passes, SciFact and FiQA remain inspected checks; ArguAna is the
unused independent gate. Actual agent-task outcomes, concurrency, hosting and
read/write throughput remain separate requirements for a superiority claim.
Do not sweep coefficients or consume the independent gate to rescue a failed
development candidate.

## First result: median rule rejected

The candidate passes 218 offline all-features tests, zero failed, one GPU test
ignored. The development matrix completes 1,548 stable timed queries and
18,495 original cross-checks. Independent auditing verifies all 516 packets.
Across 272 ordinary native calls including warm-up, fitted prior strength has
median 993, p95 1,054 and range 899–1,057 indexed feature counts. These are not
LLM-token measurements. The fitted value therefore changes materially from 128.

| Complete references | Default, 16 KiB | Median, 16 KiB | Default, 64 KiB | Median, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| History | 19/21 | 17/21 | 19/21 | 19/21 |
| Documents | 19/20 | 19/20 | 19/20 | 19/20 |
| Declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Huge tools | 9/9 | 9/9 | 9/9 | 9/9 |

No new complete reference is gained. Two previously complete history references
are lost at the tight budget. At 64 KiB mean history context grows from
25,380.5 to 57,748.9 bytes, and documents from 46,276.2 to 65,469.1 bytes.
History median/p95 latency is 23.64/33.58 ms at 16 KiB and 24.91/49.09 ms at
64 KiB; document latency is 17.80/38.24 and 19.60/25.53 ms. These are complete
local query/selection/serialization timings, not a causal ratio against a
separate prior run. Larger packets and lost evidence fail the adoption gate.

The original misses remain outside the 32-source prefix: history ranks improve
from 109 to 77 and stay at 39; the document improves from 342 to 139. Better
rank numbers still do not place those sources in the delivered context. No
independent gate is consumed. The median candidate is preserved as a patch.

## Offline combination diagnostic

Before implementing another ensemble, a fixed equal reciprocal-rank sum with
constant 60 combines the saved 512-reference lists from the default, native
BM25, conditional DPH and median rankers. All source/case digests match. It
recovers no additional reference source within the first 32 on the 129 cases;
the original missed sources end at ranks 53, 37 and 246. All groups' top-32
reference sets remain unchanged. No runtime ensemble is introduced.

This diagnostic measures saved source ranks only. It has no new query timings,
supporting-quote/context score, byte count, model-token count or agent outcomes.
It does not turn rank-list processing into a database latency claim.

## Second fixed candidate: equal two-stage smoothing

Before evaluation, test the paper's two-stage model family with fixed neutral
mixing weight 1/2, retaining the first-stage `mu = 128`. This is a model control,
not the paper's automatically estimated parameter procedure or a new theorem:

```text
P1(w|d) = (tf(w,d) + 128 * PB(w)) / (len(d) + 128)
P2(w|d) = (P1(w|d) + PB(w)) / 2
```

For sparse scoring this is exactly equivalent to Dirichlet smoothing with
source-dependent `mu_d = len(d) + 256`, including the document's length penalty.
It adds scalar arithmetic to the existing loops, with no median allocation,
new index or extra corpus pass. Empty assessed sources fall back to PB as in
the existing positive-prior formula. Ordinary word queries use P2; quoted,
identifier, key and empty queries retain the previous fixed-128 path.

The same individual evidence gate and 2 KiB reader apply. Test dense/sparse
equivalence, a hand-computed probability mixture, both clocks, revision and
nonmatching/future invariance, literal modes and reopen behavior. No mixing
weight sweep or independent-gate rescue is planned. The result follows below.

## Second result: equal mixture rejected

The second candidate also passes 218 offline tests, zero failed, one GPU test
ignored, including hand-computed mixture scores. Its 1,548 timed queries have
stable packets and 18,495 original checks; all 516 packets pass independent
range/evidence auditing. The original source archive is unchanged.

| Complete references | Default, 16 KiB | Two-stage, 16 KiB | Default, 64 KiB | Two-stage, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| History | 19/21 | 18/21 | 19/21 | 19/21 |
| Documents | 19/20 | 19/20 | 19/20 | 19/20 |
| Declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Huge tools | 9/9 | 9/9 | 9/9 | 9/9 |

No additional complete reference is gained, and a previously complete history
reference is lost at 16 KiB. Wide-budget mean history context grows to 56,696.6
bytes and documents to 63,856.9. History median/p95 is 19.53/29.15 ms and
22.45/38.72 ms at the two caps; documents are 15.47/24.82 and 16.46/28.14 ms.
This fixed mixture therefore fails the same adoption gate. It is archived,
and the default fixed-128 scorer is restored. No new provider, dependency or
independent holdout is used to justify adoption.

## Restore and strengthen the reader comparison

After the rejected controls, recheck the default with the original SQLite
native-snippet reader. All earlier evidence audits must match exactly. Also
run a current control using the already supported shared Rust reader for
SQLite (omit `--sqlite-native-snippet`), with the same ranking/indexes, 2 KiB
cap and byte budgets. Its explicit full-body reads and allocations are timed;
it is a local in-process control, not a hosted wire-byte comparison. The
native-snippet fallback counter does not count these intentional reads.

This comparison resolves a concrete benchmark risk: attributing time spent in
SQLite's excerpt path to the search engine as a whole. Earlier alternative
reader runs predate the delivered normalization/reader changes, so they do not
prove the performance of the current combination. Preserve both quality and
speed results; do not replace a stronger comparator with a weaker one.

Also diagnose exact reference-quote presence across all returned, historically
eligible snippets. This can reveal duplicated text under multiple source IDs.
Keep it separate from source-specific completeness: finding an identical string
elsewhere does not establish the requested provenance, authority, version or
generated-answer accuracy. It does not weaken the adoption rule or rescue a
rejected candidate. The purpose is to inform a later actual-agent evaluation.

The restored all-features suite passes **216 tests, zero failed, one GPU test
ignored**. Its 516 range-only audit objects exactly match the earlier default;
all candidate rankings and every native packet also remain unchanged in the
alternative-reader run. Both SQL readers retain the same individual reference
sets: history 18/21 and 20/21, documents 18/20 and 19/20 at the two caps. Earlier
optional shared admission remains a stronger tight-history control at 19/21.

The quote diagnostic confirms genuine missing text. Within each history
question, the reference strings are distinct (28 total instances). Nine returns
26/28 at both caps, with all required strings in 12/14 questions; SQL returns
24/28 and 27/28, complete in 11/14 and 13/14. Searching every returned snippet
for an identical quote from another source recovers none of these omissions.
The document diagnostic likewise stays at 19/20 for Nine. It does not justify
weakening the source-specific gate or claiming actual answer accuracy.

Absolute timings vary materially even for identical restored Nine packets.
The [balanced reader crossover](NINE_READER_CROSSOVER.md) therefore compares
both SQL readers and Nine in one process with equal arm-order positions. It
adds final context serialization to the timed path and full actual wire contexts
to the private audits. It confirms the quality counts above and a faster current
local path for Nine, while preserving the stronger SQL reader as a control.

The four delivered pair matrices total 6,192 timed queries and 73,980 original
cross-checks, with 2,064 independently checked packets. The final crossover adds
4,644 timed queries, 18,495 original checks and 774 directly audited full
responses. Together: **10,836 delivered timings, 92,475 original checks and
2,838 audited packets**. The crossover preflights are excluded from these delivered
performance totals. Both failed scoring patches remain outside the default
engine; no independent gate or model provider was used to justify adoption.

Further investigation should address missing retrieval signals and normalization
parity, rather than treating another favorable rank number as retained evidence.
The paired missing history sources are nine eligible timeline positions and
664 seconds apart, with different source-file digests. A one-neighbor or simple
same-file expansion is therefore not an evidenced fix. No temporal expansion
rule is introduced from this diagnostic.

## Reproduction and aggregate records

Use a clean checkout of `015aa5f` for each archived candidate; copy the desired
patch from this revision into it first. The later crossover harness is separate.
Apply one patch at a time, test, build release, and use fresh
audit/report paths with the original query and reader controls:

```text
git apply --unidiff-zero benches/experiments/view-median-smoothing-rejected.patch
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-smoothing --output results/fresh-smoothing.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords
```

For the second candidate use `two-stage-smoothing-rejected.patch` in a separate
clean starting checkout. For the restored default omit either patch. For the
unpaired alternative reader omit `--sqlite-native-snippet`; this does not change
ranking. Use the crossover protocol for the current balanced timing comparison.
Private originals, full responses and per-case reference text remain outside Git.

- [Rejected median rule](benchmarks/nine-median-smoothing-rejected-2026-10-19.json)
- [Offline fixed rank combination](benchmarks/nine-rrf-rank-screen-2026-10-19.json)
- [Rejected equal mixture](benchmarks/nine-two-stage-smoothing-rejected-2026-10-19.json)
- [Restored default](benchmarks/nine-smoothing-restored-2026-10-19.json)
- [Unpaired SQL Rust-reader control](benchmarks/nine-sqlite-rust-reader-2026-10-19.json)
- [Cross-source quote diagnostic](benchmarks/nine-quote-presence-diagnostic-2026-10-19.json)
