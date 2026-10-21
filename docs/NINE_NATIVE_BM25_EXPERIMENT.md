# Native BM25 with historical collection statistics

Protocol fixed on 2026-10-19 before evaluation; source base `33a6701`. The
hypergeometric and ordered-pair candidates lost existing evidence and were
removed. This experiment tests a standard lexical baseline directly in the
Rust engine, using the existing word postings instead of another large index.
The adoption hypothesis is measured evidence quality with lower local cost,
lossless history and explicit time correctness, not a novel retrieval theorem.

## Fixed ordinary-word model

For a query with ordinary word candidates, define V as all latest sources
eligible at both query clocks and the lower time bound, including nonmatching
sources. Let N be its source count and avgLen its mean indexed feature length.
Compute document frequency df(z) only from eligible sources containing feature z.

Use the fixed [SQLite BM25 reference](https://sqlite.org/fts5.html#the_bm25_function)
constants k1=1.2 and b=0.75, expressed in base-two information units:

```text
R(z)   = log2((N - df(z) + 0.5) / (df(z) + 0.5))
IDF(z) = R(z) if R(z)>0, otherwise 1e-6 / ln(2)
T(z,d) = tf(z,d) * 2.2 / (tf(z,d) + 1.2 * (0.25 + 0.75 * len(d)/avgLen))
score  = sum(z in distinct normalized query features, IDF(z) * T(z,d))
         + assessed_usefulness_bits(q,d)
```

Each normalized query feature contributes once; repetition alone does not add
another term. This is explicit set-query semantics for ordinary BM25, similar
to the comparator's deduplicated OR expression. Snowball and Porter remain
different normalization algorithms. Sources, underscore-component features and
literal byte checks are unchanged. No query labels enter the score.

The global historical N/avgLen are obtained by resolving each revision-key's
visible version once. Future records and superseded revisions cannot change
past IDF or length statistics. This adds a per-key background pass for ordinary
queries. Quoted phrases, recognized identifiers, direct keys and empty queries
retain the previous conditional query-likelihood scorer and fast candidate
path. Do not impose a full-corpus background scan on literal lookups.

BM25 weights are log-odds-derived relevance scores, not a conditional query code
or factual probability. For that path, `best_conditional_query_bits` must be
absent; retain the separately labeled unigram-background diagnostic and report
BM25's historical source count/average length. Routing entropy is still score
ambiguity only. Keep the same explicit bounded usefulness contribution.

## Gates

No coefficient sweep or per-question rule is planned. Preserve the existing
129-question evidence checks at fixed 16/64 KiB caps, exact literals, both clocks,
revisions, cache behavior and raw-only huge sources. Test independent dense
scores and a tiny SQLite calculation with matching raw tokenization, plus
future/late-revision statistic invariance. The entire offline workspace suite
must pass. Compare both SQLite configurations before claiming an improvement.

If the development gate passes, reuse SciFact and FiQA as inspected checks.
ArguAna's frozen canonical fixture remains an unused independent gate. All
latencies include ranking, exact excerpt selection, serialization and synced
receipts; indexing/replay cost and the per-key statistics pass must be disclosed.
No result cache or provider calls are allowed in the comparisons.

## Result: rejected

The fixed candidate passed 211 offline all-features tests, including a numerical
comparison with SQLite on five documents with identical word features. The
same tests covered full historical document counts, nonmatching documents,
late/future corrections, revision replacement, and the unchanged literal/key
path. The comparison verifies the scoring formula under its controlled
tokenization, not equivalence of complete pipelines on real corpora.

| Complete supporting-reference pairs | Prior, 16 KiB | Native BM25, 16 KiB | Prior, 64 KiB | Native BM25, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Redacted history | 19/21 | 17/21 | 19/21 | 19/21 |
| Public document questions | 19/20 | 18/20 | 19/20 | 19/20 |
| Literal declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Synthetic huge-tool facts | 9/9 | 9/9 | 9/9 | 9/9 |

Complete tight-budget history questions fell from 12/14 to 10/14. At 64 KiB,
the candidate recovered one previously missing history reference but lost a
different previously retained one: matching aggregate counts do not mean the
same evidence. The missing document fell outside the top-512 list, while a
previously retained document reached rank 21 and was lost at the tight cap.

At 64 KiB, mean history context grew from 25,380 to 65,398 bytes, and document
context from 46,276 to 65,497. History query medians were 17.69/20.35 ms at the
two caps. Ordinary SQLite in the same run retained 18/21 and 20/21 history
pairs, with 84.24/122.05 ms medians. Native latency remained lower, but neither
context efficiency nor accuracy improved. The candidate added 47 engine lines
and no new index; low code cost alone does not justify the regression.

All 18,495 exact sources passed cross-checks, all 1,548 timed repeated packets
were stable, and independent exact-range auditing verified 516 saved packets.
There were no provider calls or full-body SQL fallbacks. The first development
gate failed, so no Porter, reused public-label or independent holdout run was
used to search for a favorable outcome. ArguAna remains uninspected evaluation
data apart from its already frozen metadata/count checks.

The default engine and tests were restored from `33a6701`; native BM25 and its
candidate-only parity/metric additions are not enabled. This outcome rejects
this particular historical/Snowball/set-query configuration, not BM25 generally.
SQLite still differs in normalization, collection statistics, tie ordering and
excerpt selection. A fixed formula is not a guarantee of better evidence.

- [Rejected native-BM25 aggregate](benchmarks/nine-bm25-rejected-development-2026-10-19.json)
- [Reversible candidate patch](../benches/experiments/native-bm25-rejected.patch)

The patch targets `33a6701` with `git apply --unidiff-zero`; apply/reverse were
verified in an isolated copy. Private audit files and the candidate executable
remain outside Git. The active superiority objective remains unfulfilled.

## Restored release verification

The default was rebuilt in release mode after restoration. Its complete offline
workspace suite passed 209 tests, zero failed and one physical-GPU test ignored.
The new [restoration matrix](benchmarks/nine-scoring-restored-2026-10-19.json)
again verified every original and reproduced **all 516 evidence audit packets
exactly** against the previous Unicode-control run, across both engines and
both context caps. This checks full ordered candidates, selected source IDs,
original byte ranges and serialized context sizes, not just aggregate coverage.
Repeated packets remained stable and originals unchanged.

The restored Nine retains 19/21 history pairs and 19/20 document facts at both
caps, 86/86 literal declarations and 9/9 synthetic tool facts. Its measured
history medians were 13.78/11.90 ms and document medians 9.83/11.36 ms. Treat
these as another same-host run, not an improvement caused by rejecting code.

Across the three rejected candidates and this restoration check: 6,192 timed
queries, 73,980 per-run exact-record checks and 2,064 independently verified
audit packets. These are repeated controls over the same 129 questions, not
thousands of independent accuracy examples. The rejected pair index is absent;
the only retained engine change across the experiments is the explicit model
diagnostic. The default remains 1,718 first-party implementation code lines
under the existing shared-core/reader counting scope.

The unresolved ranking gaps call for new evidence or a more useful context
selection policy; these experiments do not justify more output, an entropy
confidence claim or a default weight change. Actual agent outcomes, answer-model
tokens and hosted/concurrent performance still need separate verification.
