# Complementary evidence within a fixed context budget

Protocol fixed before evaluation on 2026-10-19; starting source `e50d2c7`.
The previous expansion control recovers all development evidence but multiplies
history/document bytes. This experiment tests selection within the smaller
budget, rather than increasing the return cap or changing the ranker.

## One fixed screen

[Goldstein and Carbonell (1998)](https://aclanthology.org/X98-1025/) describe
Maximal Marginal Relevance: combine query relevance with a penalty for similarity
to previously selected passages. That paper motivates a selection control. It
does not prove this particular lexical variant improves Nine.

Use the existing frozen 512-source / 1 MiB packets from the two expansion runs.
The raw source ranges are bounded, exact originals; histories and source text
remain private. Candidate ranks and source clocks are unchanged. For each
available snippet at original one-based rank r, define:

```text
R(d)       = 1 / r(d)
W(d)       = set(lowercase alphanumeric words in its frozen excerpt,
                 excluding Nine's existing query stop words)
J(a,b)     = |W(a) intersect W(b)| / |W(a) union W(b)|
J(empty,empty) = 0
MMR(d,S)   = 0.5 * R(d) - 0.5 * max(s in S, J(d,s))
```

The empty maximum is zero. Select the greatest feasible MMR value until 32
sources or 64 KiB actual serialized flat-context bytes, breaking ties by original
rank. Update each remaining snippet's maximum similarity only against the new
selection. Return selected snippets in original rank order, preserving headers,
both clocks, verification flag and original ranges. No approximate source merge,
generated summary, embedding, parameter fitting or metadata feature is involved.
Quoted/declaration/underscore queries retain original ordering. This keeps the
current exact-name mode intact; the policy reads only the query string.

The word sets are explicit lexical features, not learned vectors. MMR's linear
combination is a heuristic, not an entropy or confidence theorem. Nine's existing
conditional-description-length ranker supplies the initial order.

## Screen limitations and gate

First compare ordinary rank-ordered packing of the same frozen ranges against
the existing 32/64 KiB audit. A wide packet may omit a very late candidate or use
a different final clipped range. These limits must be reported; this is a cheap
offline screen, not a new end-to-end engine benchmark. A screen success still
requires Rust implementation, fresh extraction, complete local timings, strict
16/64 KiB gates, the stronger SQL controls and independent evaluation.

Use all 129 development cases for both Nine and SQL's shared-Rust reader. Reject
the screen if Nine recovers no complete reference or loses an existing complete
reference at 64 KiB. Inspect individual reference pairs and complete questions.
Do not choose another coefficient or consume independent corpora to rescue it.
If it fails, archive only the reproducible diagnostic and aggregate counts;
keep the runtime unchanged. Private text, queries and full packets stay local.

## Result: reject the MMR selection screen

Every Nine case has all 512 candidate windows available. Ordinary rank-ordered
packing of those windows reproduces the baseline reference totals at 64 KiB.
The fixed MMR variant recovers no new complete Nine reference and loses none:
history stays 19/21, documents 19/20, declarations 86/86 and huge tools 9/9.
It therefore fails the declared improvement gate. No Rust runtime change or
independent-corpus evaluation is justified by this screen.

Mean Nine history bytes fall from 25,380.5 to 24,919.6, and document bytes from
46,276.2 to 39,184.0. These selected excerpts differ from the original context.
Equal retention of a few labeled quotes does not prove that all other facts are
preserved, or that an answer model benefits from the replacement. This is not
lossless compression like the existing exact shared-context codec.

SQL's shared-Rust reader falls from 20/21 history to 19/21 under MMR. Its
document screen has 18/20 even with ordinary ordering, versus 19/20 in the
fresh-extraction baseline: frozen final clipping explains that control mismatch,
so it must not be attributed to MMR. The wide SQL packets omit about 67 history
and 62 document candidate windows on average. That additionally limits the
screen; the unchanged runtime remains the comparison control.

Python selection-only time averages about 21/26 ms for Nine history/documents.
This excludes retrieval and source loading and is not Rust database latency.
The result is a negative qualitative screen, not a new complete benchmark.

## Query-specific follow-up, fixed before evaluation

MMR measures all-word dissimilarity. A separate candidate instead rewards new
query-feature evidence. Keep the same frozen inputs, caps, exact-name bypass,
serialization checks and original-rank tie breaking. For ordinary queries let
q(w) be the existing stop-filtered lowercase raw-word count, with all-word
fallback if the filter empties the query. For eligible available excerpts C:

```text
n(d)     = count(all lowercase alphanumeric word occurrences in d)
PB(w)    = (sum(d in C, tf(w,d)) + 1) / (sum(d in C, n(d)) + |vocabulary(C)| + 1)
P(w|d)   = (tf(w,d) + 128 * PB(w)) / (n(d) + 128)
g(w,d)   = max(0, q(w) * log2(P(w|d) / PB(w)))
F(S)     = sum(w in q, max(d in S, g(w,d)))
Delta(d) = F(S union {d}) - F(S)
```

Unseen query words have zero positive gain. Preserve the first feasible original
ranked source. Choose the next feasible source with greatest Delta. When all
remaining gains are zero, resume original ranking. Stop at 32 sources / 64 KiB,
then restore selected sources' original order. This retains the primary evidence
while exploring complementary term support; there is no tunable mixture weight.
The maximum-per-query-feature form prevents a duplicate passage from earning
more coverage. It is monotone with diminishing returns for this mathematical
surrogate; it is not proof of semantic completeness or answer confidence.

The screen uses raw words to avoid adding a new stemming runtime. A positive
result still requires the existing normalized feature representation in a fresh
Rust implementation and the full 16/64 KiB evidence gates. Reject on no additional
reference or any individual loss. No coefficient or independent-corpus sweep.

### Query-coverage result: rejected

The second fixed screen also retains 19/21 Nine history and 19/20 document
references, with no individually gained or lost reference. All 86 declarations
and nine tool checks remain complete. History context grows to 32,695.1 mean
bytes and documents to 48,787.3, versus 25,380.5 / 46,276.2 previously. It fails
the gate before any new runtime implementation or independent evaluation.

SQL history again drops from 20/21 to 19/21. Its document control remains 18/20
for the same already disclosed frozen-clipping reason. The Python selector
costs about 40/36 ms for Nine history/documents, excluding retrieval and original
loading. These are qualitative offline diagnostics, not complete speed results.

Neither all-word diversity nor this positive query-feature coverage surrogate
recovers the references. Adding a selection formula alone is not supported by
this evidence. The unchanged ranker and stronger SQL readers remain controls.

- [Fixed MMR aggregate](benchmarks/nine-mmr-screen-rejected-2026-10-19.json)
- [Query-feature coverage aggregate](benchmarks/nine-query-coverage-screen-rejected-2026-10-19.json)

The standalone diagnostic scripts under `benches/experiments` accept
`--repo-root` and a fresh `--output`. They read the existing local frozen
private packet/source fixtures; they never print or publish their contents.
