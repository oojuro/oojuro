# Predictive query codes with document scope mass

Protocol fixed before evaluation on 2026-10-19, after rejecting direct LZ4
query-code ordering. The candidate changes word-model mass, not source storage,
the output budget or the historical candidate universe.

## Basis and deliberate differences

[Cummins, Paik and Lv (2015)](https://arxiv.org/html/1502.00804v2) separate
document scope from verbosity using a Pólya-urn model. The author's
[implementation notes, Sections 3.1–3.3](https://www.dcs.gla.ac.uk/~ronanc/papers/cumminsSPUD_notes15.pdf)
use distinct word types as document mass and document-frequency background
statistics. This motivates the representation below; it is not new invention.

We retain the existing fixed 128-count background prior rather than fit the
paper's collection concentration or adopt its smoothing coefficient. Statistics
come from Nine's existing query-addressed two-clock/latest view, rather than
the whole collection. This is therefore a fixed scope-mass variant, not a
claim to reproduce published SPUD results. No coefficient is fitted to cases.

## Predictive distribution and compression score

For a normalized source d, n_d is its token count, u_d its distinct term count,
and tf(w,d) the raw frequency. Within the existing eligible candidate view V:

```text
U        = sum(d in V, u_d)
df(w)    = number of sources in V containing w
B(w)     = (df(w) + P0(w)) / (U + 1)
a(w,d)   = u_d * tf(w,d) / n_d
P(w|d)   = (a(w,d) + 128*B(w)) / (u_d + 128)
L(q|d)   = -sum(w in supported normalized q, count(w,q)*log2(P(w|d)))
gain(d)  = L(q|B) - L(q|d) + existing verified_usefulness_bits
```

P0 is the existing literal prefix-code prior with escape mass. Each source's
a values sum to u_d, while df summed over the vocabulary equals U. Thus the
background and source predictive distributions retain proper mass with escape.
These distributions normalize conditional on the selected view and feature
alphabet. The view itself is found from the query, so these are conditional
retrieval costs, not a standalone compressor of an unknown query or factual
confidence. Exact original bytes continue to use the existing archive codecs.

Repeating an entire source k times multiplies n and tf by k while leaving u,
df, U, a, B and scores unchanged. This is a full-view repetition invariant
because we do not refit a collection parameter from token lengths. New distinct
words can change scope. This does not claim semantic equivalence under arbitrary
paraphrase, nor invariance when source identities or timestamps change.

Use the exactly equivalent sparse computation, with N=sum query counts:

```text
gain_without_outcomes = -N*log2(1 + u_d/128)
  + sum(w with postings, count(w,q)
        *log2(1 + u_d*tf(w,d)/(n_d*128*B(w))))
```

Empty verified sources have u=n=0, contribute zero document evidence and retain
their existing usefulness prior; no posting evaluates the division at zero.
Future, late-known, superseded and unrelated nonmatching sources must not
change the addressed background. Keep literal, known underscore, direct-key
and empty-query scoring exactly on the preceding token-mass path.

## Cost and gates

Cache one usize distinct-type count per source row, rebuilt from originals.
No new postings, persistent format, metadata index, codec, dependency or provider.
On a 64-bit target 18,495 rows add 147,960 bytes of counters before allocator
effects. Candidate-time document frequencies use the same posting visits.

Verify hand probabilities and mass, an independent dense historical oracle,
full-source repetition invariance, cache/reopen, original recovery, both clocks,
lower bounds, finite scores and unchanged addressed paths. Run the full offline
suite, then the same 18,495 sources / 129 cases, 512 candidates, 32 final sources,
2 KiB excerpts and 16/64 KiB caps with three timed repeats. Audit every range,
quote, clock, latest version and byte count; retain the stronger SQL controls.

Require a new complete reference with no individual loss versus history 19/21,
documents 19/20, 86 declarations and nine huge-tool checks at either cap. Do not
tune the mass prior, introduce a mixture, expand context or consume independent
queries to rescue a development failure. A passing development result still
needs independent ranking, complete latency and actual agent-outcome gates.

## Result

**Retained as an incremental improvement. Overall superiority remains open.**
The fixed candidate passes the individual development evidence gate and improves
every previously measured SciFact/FiQA quality average over the preceding Nine
model. No prior, coefficient, candidate cap or excerpt limit was changed after
those results. These two public sets were already inspected in earlier work;
they are regression gates, not untouched holdouts for this candidate.

### Exact development evidence

All 18,495 originals pass cross-checks. The 129-case matrix completes 1,548
stable timed queries and all 516 packets pass independent original-range,
quote, two-clock, latest-version, byte-budget and label-eligibility audits.

| Ranking | History / 16 KiB | History / 64 KiB | Documents / 16 KiB | Documents / 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Preceding Nine | 19/21 | 19/21 | 19/20 | 19/20 |
| Scope-mass Nine | 20/21 | 21/21 | 19/20 | 19/20 |
| Stronger Unicode SQL control | 18/21 | 20/21 | 18/20 | 19/20 |

The additional history references are individually verified, with no previously
retained reference lost at either limit. All 86 declaration and nine huge-tool
checks stay complete. Unlike the earlier expansion control, this recovers every
history reference at the unchanged 32-source / 64 KiB limit.

The new ranking sends more bytes than the old Nine: mean wide history increases
from 25,380.5 to 40,931.3 bytes and documents from 46,276.2 to 59,298.2. Better
history coverage and independent ranking justify the model change; it is not a
blanket reduction in context size. Actual model-token counts remain unmeasured.

### Public ranking and paired effects

The completed SciFact and FiQA comparisons check 5,183 and 57,638 exact
originals and execute 1,800 and 3,888 stable timed queries respectively. A new
portable independent auditor checks all 600 and 1,296 saved source-ID packets
against canonical labels, query order, corpus identities, identical-ID policy
and recomputed metric averages. Six deliberately corrupted synthetic fixtures
are rejected. Optimized Python invocation fails closed rather than disabling
assertions. Audit byte values are bounded, not independently reconstructed:
these public audit packets omit original excerpt ranges and text.

| Dataset / metric | Preceding Nine | Scope-mass Nine | SQLite Porter |
| --- | ---: | ---: | ---: |
| SciFact nDCG@10 | 0.678942 | 0.697356 | 0.687646 |
| SciFact MRR@10 | 0.646731 | 0.660079 | 0.651546 |
| SciFact recall@10 | 0.800944 | 0.830333 | 0.824500 |
| SciFact recall@32 | 0.872222 | 0.883889 | 0.892889 |
| FiQA nDCG@10 | 0.240305 | 0.253024 | 0.254469 |
| FiQA MRR@10 | 0.297467 | 0.308613 | 0.311989 |
| FiQA recall@10 | 0.293995 | 0.308091 | 0.314659 |
| FiQA recall@32 | 0.411487 | 0.426535 | 0.441448 |

Relevant source-ID context recall also improves over old Nine: SciFact
0.810111 to 0.831167; FiQA 0.354420 to 0.361409. Questions retaining all labeled
source IDs improve from 0.793333 to 0.813333 and 0.208333 to 0.212963. These
labels evaluate document relevance, not supporting sentences or true answers.
The unchanged SQL candidate orders are checked against the earlier audits.

Paired nDCG@10 differences versus old Nine are +0.018414 on SciFact and
+0.012719 on FiQA. Fixed-seed 5,000-resample query bootstrap percentile intervals
are [0.007000, 0.030227] and [0.006794, 0.018856]. SciFact has 40 improved,
243 equal and 17 lower per-query scores; FiQA has 106 improved, 492 equal and
50 lower scores. An improved average is not an improvement on every query.

The comparison with SQL remains mixed. SciFact's nDCG point difference is
+0.009710, interval [-0.005959, 0.025076]; FiQA's is -0.001445, interval
[-0.011003, 0.008639]. Neither interval demonstrates a clear ranking advantage
over SQL. Recall@32 still trails SQL on both. Resampling paired queries from
inspected test sets does not account for all dependencies among related queries
and shared documents, or certify future workload performance.

An earlier FiQA attempt has only an initial verification log and no live process
or completed report. It is not counted. The completed fresh-path second attempt
is the reported result; nothing was restarted solely because an observation
timed out.

### Complete response latency

A separate balanced crossover compares the same pair and both SQL readers in
one process, with all six execution orderings. The timer includes uncached
retrieval, synchronized query receipts, exact original expansion and **final
flat-context JSON serialization**. This completes 4,644 stable timed queries,
18,495 original cross-checks and 774 full private packet audits. Every timed
position and ordered distinct predecessor pair has 516 observations; the
recorded matrices are checked. Source archive and case digests remain unchanged.

| Workload / cap | Nine references; median / p95 ms | Faster SQL Rust reader | SQL native snippets |
| --- | --- | --- | --- |
| History / 16 KiB | 20/21; 9.86 / 13.86 | 18/21; 34.55 / 48.69 | 18/21; 43.43 / 65.59 |
| History / 64 KiB | 21/21; 9.63 / 13.82 | 20/21; 36.10 / 48.25 | 20/21; 64.71 / 110.05 |
| Documents / 16 KiB | 19/20; 7.69 / 12.35 | 18/20; 25.78 / 38.78 | 18/20; 33.21 / 92.02 |
| Documents / 64 KiB | 19/20; 8.23 / 12.50 | 19/20; 24.94 / 36.85 | 19/20; 41.56 / 95.44 |
| Huge tools / 16 KiB | 9/9; 33.31 / 38.65 | 9/9; 67.43 / 78.61 | 9/9; 72.21 / 105.38 |
| Huge tools / 64 KiB | 9/9; 36.33 / 39.38 | 9/9; 75.32 / 83.42 | 9/9; 95.23 / 109.15 |

At 64 KiB the SQL Rust reader sends 65,506.9 mean history bytes and 65,494.8
document bytes, versus Nine's 40,931.3 and 59,298.2. The document comparison has
matched 19/20 coverage; history coverage is higher for Nine. These byte results
do not measure answer-model tokens. Public BEIR and the legacy development
matrix have different timing boundaries; use this full-response crossover for
the complete local latency claim. No hosted, cold, concurrent, out-of-RAM,
universal SQL throughput or write-speed conclusion follows.

### Delivery and remaining gates

The final offline workspace/all-features suite passes **218 tests**, zero failed
and one physical-GPU test ignored. Equations match a dense historical oracle;
repetition, original recovery, addressed fallback, clocks, revisions, lower
bounds, cache, empty sources and reopen are checked. The engine adds **22 code
lines** and one source counter; no posting, persistent source format or dependency
changes. `cloc` 2.11 measures 944 main-engine lines, or 1,924 including the entire
shared core, optional range reader and shared-context codec. Separate local
test files total 902 code lines. Third-party source remains outside that scope.

Across the four completed matrices: **11,880 stable timed queries, 99,811 per-run
original checks and 3,186 independently audited packets**. Repeated tests on
the same fixtures are not additional independent quality samples. All provider
counts are zero. Only aggregate reports are copied to Git after recursive
privacy-key checks; histories, source text, questions and individual audits stay
outside Git.

Retain this fixed scope-mass model as a measured improvement over the preceding
Nine. At this delivery, ArguAna was untouched. The subsequent
[frozen ArguAna comparison](NINE_ARGUANA_GATE.md) is now complete and records a
new-task advantage over SQL, together with the preceding-model MRR tradeoff.
Actual agent-task outcomes and repeated-mistake avoidance, model-token use,
hosted operation and current write/throughput comparisons remain open. The
goal of demonstrating superior Nine remains active.

## Reports and reproduction

- [Development evidence](benchmarks/nine-scope-mass-2026-10-19.json)
- [SciFact](benchmarks/nine-scope-scifact-2026-10-19.json)
- [FiQA](benchmarks/nine-scope-fiqa-2026-10-19.json)
- [Balanced complete responses](benchmarks/nine-scope-reader-2026-10-19.json)
- [SciFact paired effects](benchmarks/nine-scope-scifact-effects-2026-10-19.json)
- [FiQA paired effects](benchmarks/nine-scope-fiqa-effects-2026-10-19.json)
- [Scoped source counts](benchmarks/nine-scope-code-count-2026-10-19.json)

Use fresh output paths with cached dependencies:

```text
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-scope --output results/fresh-scope.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords
oojuro-lab nine-reader-crossover --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-scope-reader --output results/fresh-scope-reader.json --repeats 6 --rust-declarations --shared-stopwords
oojuro-lab nine-beir-bench --dataset data/public/scifact --dataset-name SciFact --source-run data/private/nine-beir-raw1 --run-dir data/private/fresh-scope-sf --output results/fresh-scope-sf.json --repeats 3 --sqlite-porter --shared-stopwords
oojuro-lab nine-beir-bench --dataset data/public/fiqa --dataset-name FiQA --source-run data/private/nine-fiqa-shared-raw1 --run-dir data/private/fresh-scope-fiqa --output results/fresh-scope-fiqa.json --repeats 3 --sqlite-porter --shared-stopwords
python scripts/verify_nine_evidence.py --report results/fresh-scope-reader.json --audits data/private/fresh-scope-reader --sqlite data/private/nine-scale-run3/sources.sqlite --cases data/private/nine-combined-cases-v1.json
python scripts/verify_nine_beir.py --report results/fresh-scope-fiqa.json --audits data/private/fresh-scope-fiqa --dataset data/public/fiqa
python benches/experiments/compare_binary_beir.py --before-report results/nine-fiqa-snowball1.json --before-audits data/private/nine-fiqa-snowball1 --after-report results/fresh-scope-fiqa.json --after-audits data/private/fresh-scope-fiqa --output results/fresh-scope-fiqa-effects.json
```

Omit `--source-run` from public runs to construct new source stores from the
canonical dataset instead of reusing the verified originals. The dedicated
range auditor checks the development and reader packets; the BEIR auditor
checks document-ID ranking metrics and explicitly does not certify sentence
support or reconstruct public flat-context byte counts.
