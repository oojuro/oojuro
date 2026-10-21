# Route query words by source discrimination information

Protocol fixed before evaluation on 2026-10-19; base `0dd23c8` plus the same
separate field indexes and fixed SQL control used by the rejected field models.
The fixed mixture and two Bayesian frequency-mapping variants recovered no new
reference. This candidate changes the mapping objective to source discrimination.

## Information quantity

For N temporally eligible latest sources, assume a uniform source-identity prior.
If a word occurs in field f of df_f(w) sources, observing its presence in that
field reduces identity entropy from log2(N) to log2(df_f(w)). Define the positive
observation's information gain (not an expected mutual information or confidence):

```text
I_f(w) = log2(N / df_f(w))   if df_f(w)>0
I_f(w) = 0                  if that field never contains the word
lambda_b(w) = 2*I_b(w) / (2*I_b(w) + I_m(w))
lambda_m(w) = I_m(w) / (2*I_b(w) + I_m(w))
```

If both gains are zero, retain prior (2/3,1/3). A word present in every source's
metadata has zero discrimination there, regardless of its repetition count.
This directly tests whether frequent boilerplate field values distorted the
earlier frequency mapping. Query counts and source term frequencies still enter
the likelihood model; df affects only the mapping weights.

Use the complete two-clock/latest/lower-bound snapshot from the previous
candidate, and count document frequencies only from its eligible postings.
No source is added to candidates solely for contributing a background count.
Future, late-known and superseded sources must affect neither N nor df.

## Unchanged code model and gates

Keep the existing hierarchical per-field backgrounds, 128-count source smoothing,
query-type-plus-OTHER alphabet and explicit per-source/background normalization.
OTHER retains its previous empirical Bayesian mapping, since it is not a requested
word. An absent metadata field uses body only. Keep exact body fallback when
there is no eligible metadata hit, and the original literal/identifier/key paths.
The 2:1 prior, candidate cap, context caps, excerpt reader and SQL weights do not
change. Field entropy now describes normalized information-allocation weights;
it is not a calibrated field posterior or factual confidence.

No learned embedding, LLM, data-type taxonomy, new source assertion or coefficient
sweep is introduced. The actual archive remains lossless and unchanged.

Verify field-exclusive and ubiquitous terms, zero-gain fallback, hand code
probabilities, independent dense full-view scores, temporal invariance, visible
background updates, source/range recovery, cache and reopen. Run the full offline
suite before the same 18,495-source/129-case development matrix, with 512-reference
depth, 32 returned sources, 2 KiB excerpts and 16/64 KiB serialized limits.

Require an additional complete reference without losing any individual prior
reference at either cap: history 19/21, documents 19/20, 86 declarations and nine
tool checks. Keep the stronger body-only and prior SQL controls. Independent
auditing checks every exact range and reference label's temporal eligibility.
No independent corpus, larger context or alternate prior is used to rescue a
failed setting. A passing development result still needs fresh metadata-rich
validation and the unresolved agent/independent-ranking gates.

## Result

**Rejected.** All 222 offline tests pass, with zero failures and one physical-GPU
test ignored. Hand probabilities and an independent dense historical oracle
agree with the implementation. Ubiquitous or absent field terms, exclusive
terms, temporal eligibility, visible background changes, exact originals,
cache and reopen are checked. A correct implementation does not imply a useful
ranking change.

The matrix checks the same 18,495 originals and completes 1,548 stable timed
queries. All 516 private packets pass the independent exact-range, quote,
source-clock, latest-version, budget and reference-eligibility audit.

| Scorer | History / 16 KiB | History / 64 KiB | Documents / 16 KiB | Documents / 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Retained body default | 19/21 | 19/21 | 19/20 | 19/20 |
| Fixed body/provenance mixture | 18/21 | 19/21 | 19/20 | 19/20 |
| Query-view Bayesian field mapping | 16/21 | 17/21 | 15/20 | 18/20 |
| Full-snapshot Bayesian field mapping | 19/21 | 19/21 | 16/20 | 18/20 |
| Source-discrimination information | 18/21 | 19/21 | 16/20 | 19/20 |

These are complete source-specific references, including their supporting
quotes, on the reused development cases. Identical totals do not imply the
same evidence. The information variant newly recovers one document reference
at 64 KiB, now rank 20 instead of 342, but loses another previously retained
reference. At 16 KiB it loses three prior document references and one history
reference without gaining any. Every variant still retains 86/86 declarations
and 9/9 huge-tool checks. No variant passes the individual no-loss quality gate.

The fixed 2:1 SQLite metadata control retains 18/21 and 20/21 history and 17/20
and 18/20 documents. It also loses document evidence relative to the retained
body-only control, which remains a required, stronger comparator. Beating a
weaker metadata configuration would not justify adopting this candidate.

The information variant's mean wide history/document contexts grow to
38,538.0 / 62,930.6 bytes from the default's 25,380.5 / 46,276.2. Its diagnostic
history medians are 22.33/22.61 ms, and document medians are 21.04/20.80 ms.
These separate matrices include the background scan but exclude final whole
context serialization and do not establish a causal timing comparison. They
provide no reason to accept additional work after the evidence gate fails.

The source archive format and originals are unchanged. The candidate adds
217,989 metadata postings and a separate 13,133-word metadata vocabulary to
the body's 4,671,429 postings. Full-view row visits and both field posting
streams are counted; the query path also allocates two document-frequency maps.
Those structures and the dense candidate arithmetic are additional costs.
No independent corpus is consumed to rescue the failed development result,
and no actual model-token or agent-task outcome is measured.

The field models are archived rather than retained in the engine. Another
field-weight coefficient or context-cap adjustment is not justified by this
sequence. Source statistics help construct a retrieval hypothesis; neither
smaller routing entropy nor a larger corpus guarantees factual accuracy.
Explicit context expansion has its own measured quality/byte frontier and
remains separate from a scoring improvement.

[Aggregate measurements](benchmarks/nine-source-information-fields-rejected-2026-10-19.json).
The [candidate patch](../benches/experiments/source-information-fields-rejected.patch)
applies to `0dd23c8` with `git apply --unidiff-zero` and includes the Rust model,
independent tests and the same weighted SQL control. The
[fixed-mixture protocol](NINE_FIELD_MIXTURE_EXPERIMENT.md) and
[Bayesian protocols](NINE_BAYES_FIELD_MAPPING_EXPERIMENT.md) preserve the three
earlier failures and their aggregate measurements.

## Verified restoration

All three modified runtime/test files are restored to the starting default.
The full offline workspace suite then passes 216 tests with zero failures and
one physical-GPU test ignored. The rebuilt release completes another 1,548
stable timed queries and cross-checks all 18,495 originals. All 516 packets pass
independent evidence/range auditing and exactly equal the earlier default's
entire private audit objects, for both backends at both caps. Source archive
and case digests are unchanged; the scorer reports `dirichlet-128-v1` and zero
embedding/LLM calls. All four archived patches apply cleanly to this source
base with `--unidiff-zero`.

Across four candidates and restoration, the milestone completes 7,740 stable
timed queries, 92,475 per-run original checks and 2,580 independently audited
packets. This is repeated verification on fixed data, not additional independent
quality samples. The [restored aggregate](benchmarks/nine-fields-restored-2026-10-19.json)
preserves the final measurement. No field index or extra field-scoring code
remains in the runtime, and no dependency changed.

Nine still trails wide-budget SQL history completeness and the independently
measured SciFact/FiQA ranking quality. Actual agent outcomes, repeated mistake
avoidance, model-token usage and hosted behavior remain unmeasured. The goal
of demonstrating overall superiority remains active. A future candidate should
test a different fixed-budget compression/ranking mechanism rather than tune
these failed field weights against reused cases.

## Reproduce

Use a separate clean checkout and fresh output paths:

```text
git apply --unidiff-zero benches/experiments/source-information-fields-rejected.patch
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-information --output results/fresh-information.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords --textual-provenance
python scripts/verify_nine_evidence.py --report results/fresh-information.json --audits data/private/fresh-information --sqlite data/private/nine-scale-run3/sources.sqlite --cases data/private/nine-combined-cases-v1.json
```

Raw histories, source text, private questions and per-question packets remain
outside Git. Only the recursive privacy-checked aggregate report is published
in this repository.
