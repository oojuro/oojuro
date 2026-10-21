# Normalized Bayesian word-to-field mapping

Protocol fixed before evaluation on 2026-10-19; source base `0dd23c8` plus the
separate field indexes and SQL control described in the rejected constant-field
experiment. Its 2:1 prior is unchanged. This is a new mapping model, not a new
coefficient or a larger return budget.

[Kim, Xue and Croft (2009), Section 3.2](https://ciir-publications.cs.umass.edu/getpdf.php?id=861)
uses Bayes' rule to infer which field a query word belongs to from observed field
frequencies. That motivates word-specific field evidence. The conditional
alphabet normalization below is our explicit extension; it is not a reproduced
result or a quality guarantee from the paper.

## Fixed conditional alphabet and time

Use the preceding Cb/Cm views, both clocks, latest versions, lower bound, exact
body-only literal/identifier modes and unchanged source data. The code alphabet
A consists of supported normalized query feature types plus OTHER. Its type
dictionary is common to every candidate. All non-query words map to OTHER.
Both original word order/counts and raw source bytes remain available.

Write cf_f(x) for field-view counts, Tf for full field length, and tf_f(x,d) for
source counts. For OTHER subtract the sum of distinct query-feature counts from
the respective full length using integers. Assign P0(OTHER) the remaining prior
mass `1 - sum(query types, P0(w))`. All categories then have a finite model:

```text
B0(x)     = (cf_b(x) + cf_m(x) + P0(x)) / (Tb + Tm + 1)
Bf(x)     = (cf_f(x) + B0(x)) / (Tf + 1)
Ef(x)     = cf_f(x) / Tf, or 0 if Tf=0
lambda_b  = 2*Eb(x) / (2*Eb(x) + Em(x))
lambda_m  = Em(x) / (2*Eb(x) + Em(x))
```

If both empirical rates are zero (possible for OTHER), retain prior (2/3,1/3).
Mapping uses observed frequencies, so a word observed only in the body routes
to the body, and a word observed only in provenance routes to provenance.
Field-specific predictive models use one shared-background pseudocount for
finite probabilities, with the existing source smoothing strength 128:

```text
Pf(x|d)   = (tf_f(x,d) + 128*Bf(x)) / (nf(d) + 128)
hd(x)     = lambda_b(x)*Pb(x|d) + lambda_m(x)*Pm(x|d)
hB(x)     = lambda_b(x)*Bb(x) + lambda_m(x)*Bm(x)
Zd        = sum(x in A, hd(x))
ZB        = sum(x in A, hB(x))
pd(x)     = hd(x)/Zd
pB(x)     = hB(x)/ZB
gain(d)   = sum(w in q, count(w,q)*log2(pd(w)/pB(w))) + usefulness_bits
```

An absent metadata field routes entirely to body. A word-dependent mixture does
not generally sum to one; Zd/ZB are required, including OTHER. The resulting
code is conditional on the shared query-type alphabet, not an unconditional
word model or probability that an answer is true. Field-routing entropy is the
query-count-weighted mean binary entropy of lambda. It measures field ambiguity
and remains distinct from candidate-routing entropy and factual confidence.

With no eligible metadata match, retain the exact original sparse body model.
Use dense O(|C|*|q|) arithmetic for the active model, with field posting counts
and precomputed inverse lengths. The added work is a measured latency cost;
this candidate does not inherit the constant model's sparse speed claim.
No additional index, parameter sweep, provider or learned assertion is involved.

## Gates

Verify proper mass on A, hand probabilities, field-exclusive word routing,
independent dense equivalence, both clocks, nonmatching/future invariance,
revisions, lower bounds, literals/anchors, originals, cache and reopen. Keep the
same fixed 18,495 originals, 129 cases, 512 candidates, 32 sources, 2 KiB excerpts
and 16/64 KiB serialized caps. Require a new complete reference with no individual
loss at either cap, not just matching totals. Preserve the stronger SQL body and
metadata controls. Audit every original range and reference-label eligibility.

Metadata-free public corpora must retain their old rankings if development
passes. They cannot independently validate a new metadata feature. Do not use
the unused ArguAna gate, actual agent outcomes or byte counts to rescue a failed
development candidate. Coefficients and context limits stay fixed.

## Query-view result: rejected

The 221-test suite passes with zero failures and one GPU test ignored, including
normalization, field-exclusive routing, hand scores, independent dense scores
and temporal/original recovery. The matrix cross-checks 18,495 originals and
completes 1,548 stable timed queries. Independent auditing verifies 516 packets.

| Complete references | Prior, 16 KiB | Query-view model, 16 KiB | Prior, 64 KiB | Query-view model, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| History | 19/21 | 16/21 | 19/21 | 17/21 |
| Documents | 19/20 | 15/20 | 19/20 | 18/20 |
| Declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Huge tools | 9/9 | 9/9 | 9/9 | 9/9 |

No new complete reference is recovered. Several previously retained sources
fall outside the frontier or byte budget. Same-run history medians are
12.12/12.73 ms at the caps and documents 9.19/10.00 ms; the legacy final-context
serialization exclusions apply. Wide contexts grow to 45,752.2 / 59,658.6 mean
history/document bytes. Neither normalized probabilities nor low arithmetic
cost establishes relevance. This candidate is rejected without independent
evaluation or a changed prior/context cap.

[Aggregate measurements](benchmarks/nine-query-view-bayes-fields-rejected-2026-10-19.json).
`benches/experiments/query-view-bayes-fields-rejected.patch` preserves the model,
tests and SQL control against a clean `0dd23c8` checkout with `--unidiff-zero`.

## Full-snapshot follow-up, fixed before evaluation

The source paper estimates field frequencies from the full collection of field
instances. Conditioning each field's denominator on query matches can distort
relative field rates differently. The following changes the statistics view,
not the 2:1 prior, source/excerpt caps, normalization or query features.

Use V(t,known,from), the complete set of latest sources satisfying both clocks
and the lower bound. Compute Tb/Tm by summing each field's length over V.
Query-feature numerators already visit every eligible matching posting and
therefore equal full-view counts. OTHER becomes the full field length minus
those counts. Keep candidate discovery restricted to actual body/provenance
matches and verified positive outcomes. No unrelated source becomes a candidate
merely because it contributes to a background length.

All Bayesian mapping, field-specific predictive distributions, OTHER category,
per-source/background normalization and 128-count smoothing remain as above.
The metadata-free exact original fallback remains. Count full-snapshot row visits
separately and include the scan in retrieval timing. No new index or provider.

This explicitly changes one statistics contract: a newly imported nonmatching
source that is visible at the requested cutoffs may change the field background
and scores. A future, late-known or superseded source must not. Preserve the
two-clock invariance tests, and test the legitimate visible-background update
against an independent dense full-view oracle. Do not silently retain the earlier
query-view claim that every nonmatching source leaves scores invariant.

The same strict individual evidence gate applies: recover a new complete
reference without any prior loss at either 16/64 KiB. Keep stronger SQL controls
and audit every returned range. No independent corpus is used to tune or rescue
this fixed follow-up.

### Full-snapshot result: rejected

The complete 221-test suite passes, with zero failures and one GPU test ignored.
Dense full-snapshot probabilities match the implementation. Future/late-known
invariance remains; a visible nonmatching record correctly updates the background.
Another 18,495 originals and 1,548 stable timed queries are checked, with all
516 packets independently audited.

History returns to 19/21 at both caps, with no individually gained or lost
reference relative to the default. Documents retain 16/20 at 16 KiB and 18/20
at 64 KiB, losing three and one previous references respectively and gaining
none. Declarations and huge tools remain 86/86 and 9/9. The old document miss
is rank 353, while a previously retained document slips to rank 34.

Wide history/document means are 34,852.0 / 62,007.3 bytes. Diagnostic medians
vary across caps: history 9.70/21.15 ms and documents 8.94/19.06 ms. The separate
matrices and final-context serialization exclusions do not support causal timing
claims. Correcting the collection view does not correct the evidence deficit.
The full-snapshot candidate is rejected without further coefficient adjustment
or independent evaluation. Stronger original SQL controls remain in scope.

[Full-snapshot aggregate](benchmarks/nine-snapshot-bayes-fields-rejected-2026-10-19.json).
`benches/experiments/snapshot-bayes-fields-rejected.patch` preserves this version
against `0dd23c8` with `git apply --unidiff-zero`.
