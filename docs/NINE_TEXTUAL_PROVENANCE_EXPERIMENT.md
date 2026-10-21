# Textual provenance as retrieval input

Protocol set before the candidate's development matrix on 2026-10-19. The
starting implementation is commit `322aadb`.

## Reason and fixed candidate

A diagnostic found query words in a missed public document's stored path
metadata but not its body. This motivates searching supplied textual provenance
uniformly, rather than adding a document-specific lookup rule. Metadata is
caller-supplied context, not proof that a body contains or supports a claim.

The candidate adds word counts from **all metadata values**, without metadata
field names or the separate source-ID/revision-key fields. Values may themselves
contain IDs; no type is inferred. There is no allowlist of path names and no query or
relevance label is used during indexing. Words inside metadata identifiers are
split into components; complete underscore identifier anchors remain derived
from original bodies. Exact quoted phrases still require original body bytes.

For body counts `b(w,d)` and metadata-value counts `m(w,d)`:

```text
tf'(w,d) = b(w,d) + m(w,d)
len'(d)  = sum_w tf'(w,d)
```

Use these counts and lengths in the existing conditional query-code-length
equations with unchanged Dirichlet `mu = 128`, raw frequencies and Snowball
normalization. No new ranking weight, learned parameter, embedding or LLM is
introduced. Both time cutoffs, latest-version selection and original archives
stay unchanged; derived indexes rebuild from stored originals.

SQLite receives the same original body and all metadata values in a two-column
FTS5 index, with equal default BM25 weights. Its complete-identifier/literal
index remains body-only. The native snippet reader still returns exact ranges
of the body. The optional index is built before timing over the frozen sources;
its stored body copies and index size must be disclosed, not counted as free.
It is a snapshot benchmark control and refuses source appends after enabling.

## Development gate

Use the same 18,495 sources, 129 reused questions, 512-reference candidate depth,
32-source cap, 2 KiB excerpt cap and 16/64 KiB serialized context budgets. Three
timed repetitions follow warm-up. Compare with the default-flat report from
`322aadb`; run the same policy on Nine and SQLite. Do not change source selection
or enable shared-context admission during this experiment.

Adoption requires at least one additional complete reference without losing
another existing check: 19/21 history reference pairs, 19/20 document facts,
86/86 declaration checks and 9/9 synthetic tool checks at both caps. Also report
complete questions, latency, byte sizes and new index costs. A regression rejects
the candidate as the default even if its implementation tests pass. Exact
original/range/visibility auditing remains required.

Independent SciFact/FiQA ranking deficits and actual agent-task outcomes remain
separate gates. Their metadata-free corpora do not test the new input source.
ArguAna stays unused for this development experiment. No result below should be
presented as generated-answer accuracy or fewer repeated agent mistakes.

## First result and selection follow-up

The two new historical/scope tests pass in the complete 218-test candidate suite
(zero failed, one physical-GPU test ignored). The 1,548-query matrix has stable
packets and 18,495 original checks; independent auditing verifies 516 packets.

At 64 KiB Nine recovers the previously missed document: 20/20 rather than 19/20.
At 16 KiB the recovered document displaces a different reference. The latter
falls from rank 6 to 7 and no longer fits after earlier 2 KiB excerpts consume
the budget. Aggregate 19/20 conceals this individual regression, so the first
candidate does **not** meet the adoption gate. History stays 19/21 and all 86
declaration and nine huge-tool checks remain complete.

SQLite's corresponding document counts fall from 18/20 to 17/20 at 16 KiB and
19/20 to 18/20 at 64 KiB; history remains 18/21 and 20/21. The research database
file totals 471,875,584 bytes including existing body/identifier/Porter indexes
and the new contentful metadata index. This is not an isolated production index
size or a fair total-memory comparison against Nine's raw archive. Nine's
postings rise from the current default's 4,671,429 to 4,840,069 with the same
originals (168,640 additional postings, about 3.61%). The earlier scale report's
4,217,393 count predates later normalization/identifier changes and is not the
correct baseline for this comparison.

The metadata run's local Nine history median/p95 is 15.61/24.26 ms at 16 KiB
and 14.45/20.09 ms at 64 KiB. Document median/p95 is 13.01/20.50 and
14.30/23.06 ms. SQLite history median/p95 is 89.48/139.49 and 104.69/203.63 ms;
document median/p95 is 72.98/273.12 and 88.53/184.08 ms. These measure the
complete local retrieval/selection/serialization path including synced query
receipts, excluding source replay and index construction. They are separate
runs from the original body-only matrix, not a causal timing comparison or a
hosted/concurrent performance claim.

Before another matrix, fix a uniform selection follow-up: lower the optional
excerpt cap from 2 KiB to 1 KiB for both backends and every question. Keep other
limits unchanged and check individual evidence losses as well as totals. This
is a second development candidate, not a retroactive pass of the first rule.
If it helps, a body-only 1 KiB control is required to separate metadata gains
from context selection changes. Do not claim a metadata-specific improvement
from the combined change alone.

## Follow-up rejected

The 1 KiB follow-up is worse: Nine retains **17/21 history reference pairs and
15/20 document facts at both budgets**. It recovers the original metadata miss
but loses two previously complete history references and five document facts.
SQLite retains 18/21 and 20/21 history references at the two caps, with 15/20 and
16/20 document facts. Both engines retain all 86 declarations and nine huge-tool
checks. This run adds another 1,548 stable timed queries and 18,495 original
checks; independent verification audits all 516 packets.

| Complete references | Nine body / 2 KiB | Nine metadata / 2 KiB | Nine metadata / 1 KiB | SQLite body / 2 KiB | SQLite metadata / 2 KiB | SQLite metadata / 1 KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| History, 16 KiB | 19/21 | 19/21 | 17/21 | 18/21 | 18/21 | 18/21 |
| History, 64 KiB | 19/21 | 19/21 | 17/21 | 20/21 | 20/21 | 20/21 |
| Documents, 16 KiB | 19/20 | 19/20, different miss | 15/20 | 18/20 | 17/20 | 15/20 |
| Documents, 64 KiB | 19/20 | 20/20 | 15/20 | 19/20 | 18/20 | 16/20 |

The stronger original SQLite control remains in the comparison. The earlier
optional body-only shared-context control additionally retains 19/21 history
references at the tight budget; this experiment does not supersede it with a
weaker metadata configuration.

The reference quote spans are small enough to fit the shorter windows, yet the
reader chooses different windows that omit those quotes. A source's presence
and a nominally adequate excerpt size do not prove that supporting evidence is
present. Smaller excerpts and more returned sources do not guarantee accuracy.
Because this combined candidate fails, no body-only 1 KiB control is used to
justify adoption or claim a metadata-specific benefit.

A diagnostic of the displaced tight-budget document found no identical selected
texts and no useful recent prefix/suffix repetition. The existing exact-sharing
codec therefore cannot free the required space in that packet. No new context
codec or further retrieval matrix is claimed for this diagnostic.

The metadata candidate and uniform 1 KiB follow-up are **rejected as defaults**.
The prior body-only index and 2 KiB experimental reader remain delivered. The
two-test candidate patch is preserved for reproduction; no new runtime option,
metadata index or dependency is retained in Nine or its default benchmark path.
The separate SQLite metadata tables generated by the experiment remain only in
the ignored private research database and are not queried by the restored path.

## Restoration and reproduction

After removing the candidate, the full offline all-features suite passes
**216 tests, zero failed, one physical-GPU test ignored**. The rebuilt release
default completes another 1,548 stable timed queries with 18,495 original
cross-checks. All 516 restored packets pass independent range/evidence auditing
and exactly reproduce the previous default's complete audit objects at both
caps, for both backends. Original source archive bytes are unchanged.

Across the two rejected candidates and restoration, 4,644 timed queries and
55,485 per-run original checks complete, with 1,548 independently audited
packets. The candidate patch applies cleanly to the starting implementation;
formatting and whitespace checks pass. No provider call, independent holdout or
actual agent-task success measurement was used to justify a candidate.

The superiority goal remains active: remaining history misses are ranking
failures, and the independent SciFact/FiQA quality deficits are unchanged. A
grounded next candidate is a principled query-language smoothing estimate.

Apply the [candidate patch](../benches/experiments/textual-provenance.patch) to
the starting implementation, then build with cached dependencies:

```text
git apply --unidiff-zero benches/experiments/textual-provenance.patch
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-provenance --output results/fresh-provenance.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords --textual-provenance
```

Use fresh report/audit paths. For the separate failed selection follow-up change
only the excerpt cap to 1024 and use new paths. Raw histories, queries, original
source text and per-question evidence audits remain outside Git. Published JSON
contains aggregate measurements only. The public document questions and the
retrospective history are reused development cases, not a new holdout.

- [Metadata with 2 KiB excerpts](benchmarks/nine-textual-provenance-2026-10-19.json)
- [Rejected 1 KiB follow-up](benchmarks/nine-textual-provenance-1k-2026-10-19.json)
- [Restored default control](benchmarks/nine-provenance-restored-2026-10-19.json)
