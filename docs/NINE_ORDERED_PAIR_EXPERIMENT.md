# Word order as an additional compression feature

Protocol fixed on 2026-10-19 before candidate evaluation. Source base `e0e1f11`
restores the delivered Snowball/Dirichlet scorer after the hypergeometric
candidate lost history evidence. Reweighting the same unigram counts did not
supply information about which words occur together. This candidate adds that
signal while preserving the existing word model and original text.

## Fixed model

Maintain a separate derived posting index of adjacent **content words**. Split
on non-alphanumeric characters except underscores, lowercase, remove the same
fixed stopwords, then normalize ordinary words with the existing Snowball map.
Count successive pairs in this filtered sequence. Function words and punctuation
may intervene in the original; these are statistical content pairs, not exact
phrases or evidence of entailment. Exact quoted searches retain their existing
verbatim semantics. A query containing underscores or requesting a direct key
uses the original word/identifier policy, without this extra channel.

For channel j in {words, pairs}, use the same current code-gain equation:

```text
PB_j(z) = (sum(d in C, tf_j(z,d)) + P0(z)) / (sum(d in C, n_j(d)) + 1)
P_j(z|d)= (tf_j(z,d) + 128 * PB_j(z)) / (n_j(d) + 128)
G_j     = sum(z in Q_j, tf_j(z,Q_j) * log2(P_j(z|d) / PB_j(z)))
score   = G_words + G_pairs + assessed_usefulness_bits(q,d)
```

The typed channels use separate lengths/backgrounds, not one inflated word
length. Pair keys join their two word forms with an internal NUL separator;
the word tokenizer cannot generate that key. Existing word candidates and
historical eligibility define C; pairs cannot add a source or cross either
clock/latest-version restriction. As with word features, a query pair absent
from every eligible candidate is omitted. No active pair means exactly the
previous score. A future-only pair must not change a past query.

This sums two feature-code gains with equal unit coefficients. It is a
composite code for a redundant representation, not an assertion that words and
pairs are statistically independent, a normalized probability for raw query
text, an optimal MDL theorem or factual confidence. Background/conditional bit
diagnostics must describe both encoded feature channels. Routing entropy still
describes score ambiguity only. Query repetition alone does not train the model.

The pair index is reconstructed from unchanged exact originals. It adds RAM,
ingestion/replay work and postings visited; record pair vocabulary/posting counts
separately. Its per-source normalization cache is bounded at 4,096 raw forms;
overflow must change cost, never counts or matching. No GPU, learned embedding,
LLM, generated summary, new original format or network call is introduced.

## Gates

First preserve the current development evidence with the same 129 questions,
18,495 sources / 149.8 MB text, 512 candidate references, 32 excerpts, 2 KiB
per excerpt and 16/64 KiB packet caps. Compare ordinary and Porter SQLite;
keep the shared stopword and original-range controls. Reject a material existing
accuracy regression even if a different task improves. Check raw-only huge
sources and all offline storage, literal, time, cache and dense-equation tests.

If development passes, evaluate the inspected SciFact and FiQA data, clearly
labeled as reused checks. A new ArguAna counterargument-retrieval gate may then
test independent questions; it has not been downloaded or inspected at protocol
time. Its canonical corpus/query IDs overlap, so the evaluator's existing
identical-ID exclusion is mandatory. No per-question feature, source-ID rule,
coefficient sweep or post-holdout default selection is planned.

## Result: rejected

The candidate passed 211 offline all-features tests, including independent
dense/composite scoring, a hand-computable word-order contrast, future-only pair
invariance, revisions, original recovery and cache overflow. The measured
development corpus still lost required evidence:

| Complete supporting-reference pairs | Prior, 16 KiB | Pair model, 16 KiB | Prior, 64 KiB | Pair model, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Redacted history | 19/21 | 19/21 | 19/21 | 19/21 |
| Public document questions | 19/20 | 18/20 | 19/20 | 18/20 |
| Literal declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Synthetic huge-tool facts | 9/9 | 9/9 | 9/9 | 9/9 |

The remaining history sources moved from ranks 109/39 to 300/68. The previously
missing public source left the top-512 list, and another required document moved
to rank 61. No clipped excerpt explains these misses. At 64 KiB, history context
fell from 25,380 to 20,022 mean bytes and document context from 46,276 to 41,590,
but fewer document facts were retained. The smaller packet is not an accuracy win.

The new index contained 2,659,263 pair keys and 8,096,511 source/pair postings,
in addition to 341,227 word keys and 4,671,429 word postings. These are counts,
not measured RAM bytes. The extra index/replay cost has no demonstrated benefit
on this gate. Same-run history median query latency was 16.41/15.51 ms at the
two caps; ordinary SQLite retained 18/21 and 20/21 pairs at 92.56/127.08 ms.

All 18,495 exact sources passed verification, all 1,548 timed repeat queries
were stable, and independent byte-range auditing verified 516 saved packets.
No provider calls or full-body SQL fallback occurred. The failed first gate
ends this candidate; no Porter or public-label run is used to hide the loss.

The original scorer and index were restored, with 209 passing offline workspace
tests and one physical-GPU test ignored. The pair index/helper/metrics are not in
the default engine. ArguAna was downloaded and checksum-verified while the
release binary built, without inspecting question text or individual labels;
it remains an unused independent gate.

- [Rejected pair aggregate](benchmarks/nine-pairs-rejected-development-2026-10-19.json)
- [Reversible candidate patch](../benches/experiments/content-pairs-rejected.patch)

The patch targets `e0e1f11` with `git apply --unidiff-zero`; application and
reversal were verified in an isolated copy. Private source/audit files and the
candidate executable remain outside Git. The mathematical signal could be
useful elsewhere, but this fixed model does not justify its costs here.
