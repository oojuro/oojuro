# Separate body and provenance probability models

Protocol fixed before evaluation on 2026-10-19; source base `0dd23c8`.
Concatenating all metadata into body counts recovered one document but displaced
other tight-budget evidence. Unconditional pseudo feedback helped history but
regressed on inspected public corpora. This candidate changes the representation
of supplied provenance without inferring new query words.

[Ogilvie and Callan (2003)](https://www.cs.cmu.edu/~callan/Papers/sigir03-pto.pdf)
studies combining document representations through mixtures of statistical word
models. It motivates the representation split, not our exact parameters or a
claim of novelty. The model below is our fixed query-addressed variant.

## Representation and clocks

Keep original body counts b(w,d) and lengths nb(d). Build separate counts m(w,d)
and lengths nm(d) from all metadata values, excluding field names and the
separate immutable ID/revision-key fields. Split metadata identifier components
as ordinary words; complete underscore anchors remain body-only. Use the same
Snowball map and raw frequencies as the current engine. Originals and archive
format stay unchanged; both indexes rebuild from stored immutable records.

For ordinary queries define Cb as the original eligible body candidate view
(including already verified positive outcomes), and Cm as eligible metadata
matches. Both clocks, latest revisions and the lower bound apply before either
view's counts. Let C be their union. Collect body statistics over Cb and metadata
statistics over Cm. No future or late-known record may affect an earlier view.
Quoted phrases, recognized identifiers, direct keys and empty queries keep the
current body path. If Cm is empty, retain the original sparse scoring exactly.

## Conditional code-length model

Only query features observed in at least one eligible field enter scoring, as
the existing engine already projects out unsupported query words. P0 is the
existing literal prefix-code prior and mu remains 128:

```text
Tb       = sum(d in Cb, nb(d))
Tm       = sum(d in Cm, nm(d))
B(w)     = (sum(Cb,b(w,d)) + sum(Cm,m(w,d)) + P0(w)) / (Tb + Tm + 1)
Pb(w|d)  = (b(w,d) + 128*B(w)) / (nb(d) + 128)
Pm(w|d)  = (m(w,d) + 128*B(w)) / (nm(d) + 128)
P(w|d)   = (2*Pb(w|d) + Pm(w|d)) / 3       if nm(d) > 0
P(w|d)   = Pb(w|d)                         otherwise
gain(d)  = sum(w in q, count(w,q)*log2(P(w|d)/B(w))) + verified_usefulness_bits
```

The 2:1 field prior makes provenance's code one bit more expensive than body's:
`-log2(1/3) - [-log2(2/3)] = 1`. This fixed preference is not fitted to labels
and is not a truth probability. The active-field shared background changes;
body term counts and lengths are not concatenated with metadata. A shared
background gives every observed query feature positive mass in both channels,
including terms found only in metadata, without numeric zero-probability hacks.

Use an algebraically equivalent sparse calculation. For a candidate d set
u = (2/3)/(nb+128), v = (1/3)/(nm+128), or u = 1/(nb+128), v = 0 when nm=0.
With z = 128*(u+v) and N = sum(query counts):

```text
gain_without_outcomes = N*log2(z)
  + sum(w with a nonzero posting, count(w,q)
      * log2(1 + (u*b(w,d) + v*m(w,d)) / (B(w)*z)))
```

Merge body/metadata posting contributions before each logarithm. Do not scan all
query features for every candidate. Count both posting streams and all work in
metrics and latency. Report separate metadata vocabulary/posting costs; the new
index is not free. No new dependency, embedding, LLM, event taxonomy or inferred
assertion is involved. The archive still uses its existing lossless codecs.

## Gates and controls

Test a hand-computed two-field example, independent dense/sparse equivalence,
the metadata-free exact fallback, metadata-only matches, no metadata literals
or anchors, both clocks, revisions, lower bounds, finite long-term scores,
original metadata/body recovery, cache invalidation and reopen. Run the complete
offline suite. Only then benchmark the frozen 18,495 originals and 129 cases at
512 candidates, 32 sources, 2 KiB excerpts and 16/64 KiB serialized caps.

Require at least one new complete reference with no individually lost reference
at either cap versus 19/21 history, 19/20 documents, 86 declarations and nine tool
checks. Keep quote, complete-question, byte and latency metrics separate.
Independently audit all ranges and reference-label temporal eligibility.

SQLite receives the same body plus metadata values in its existing two-column
FTS control, with fixed BM25 field weights 2:1. Literal/identifier routes remain
body-only. Retain the stronger original body-only and prior equal-weight SQL
controls; a weaker new SQL configuration cannot justify a win. Metadata table
construction is outside query timing and source appends are refused afterward.

If development passes, metadata-free public corpora must retain their original
rankings; they do not independently validate a metadata feature. The unused
ArguAna gate, actual agent tasks, tokens, hosting and throughput remain separate.
Do not change the prior or excerpt cap to rescue a failed development candidate.

## Result: fixed mixture rejected

The 220-test candidate suite passes, zero failed and one GPU test ignored. The
independent dense model, hand probabilities and sparse implementation agree.
The frozen matrix cross-checks 18,495 originals, completes 1,548 stable timed
queries and passes 516 independent packet audits.

No new complete Nine reference is recovered. History falls from 19/21 to 18/21
at 16 KiB and stays 19/21 at 64 KiB; documents remain 19/20 at both caps. All 86
declaration and nine tool checks remain complete. The lost history source moves
from rank 1 to 12 and no longer fits at the tighter budget. The missing document
improves from rank 342 to 35 but still exceeds the 32-source cap. No return cap,
excerpt size or field prior is adjusted to rescue this candidate.

Mean wide history context rises from 25,380.5 to 45,138.3 bytes, and documents
from 46,276.2 to 62,468.8. The same-run history medians are 11.21/11.01 ms at
16/64 KiB; documents 9.01/9.03 ms. These legacy timings exclude final whole-packet
serialization. Better local speed than the SQL arm does not pass the quality gate.

The 2:1 weighted SQL metadata control retains 18/21 and 20/21 history, with
17/20 and 18/20 documents. Stronger body-only and earlier equal-weight controls
remain evidence; this weaker new configuration is not used to claim superiority.

The body index still has 4,671,429 postings. The separate metadata index has
13,133 word types and 217,989 postings, about 4.67% additional postings. The
earlier concatenated index added fewer postings because it merged overlapping
body/metadata terms in a row; it is not the same index structure. Original
archive bytes and metadata remain unchanged. No model provider or independent
public gate is used to rescue this failed development setting.

[Aggregate measurements](benchmarks/nine-constant-field-mixture-rejected-2026-10-19.json).
The rejected Rust implementation, math tests and SQL control are preserved in
`benches/experiments/constant-field-mixture-rejected.patch`, applicable to a clean
`0dd23c8` checkout with `git apply --unidiff-zero`.
