# Hypergeometric information weighting

Protocol fixed on 2026-10-19 before this candidate is evaluated. Starting source:
`85dafc7`. The delivered Snowball/Dirichlet model improves on the previous engine
but still trails SQLite's independent ranking quality. Its remaining development
history references are at ranks 39 and 109; the remaining public document is at
342. None of these three is an excerpt-clipping failure. Increasing the excerpt
size cannot fix their ranking.

## One fixed candidate

Test a DPH-style hypergeometric information weight over Nine's existing
query-addressed, two-clock historical view C. Retain the same normalized raw
counts, exact identifiers/phrases, explicit outcome evidence, original archive
and literal-first excerpt reader. No semantic model, query expansion, new index
or smoothing-constant sweep is involved.

The established [DPH model](https://github.com/terrier-org/terrier-core/blob/5.x/modules/core/src/main/java/org/terrier/matching/models/DPH.java)
uses hypergeometric information weighting with a frequency-dependent aftereffect.
It was evaluated in the authors' [TREC 2007 report](https://trec.nist.gov/pubs/trec16/papers/fub.blog.final.pdf).
This experiment reimplements the mathematical weight in Rust, using Nine's
conditional historical collection and literal escape prior. It is a conditional
variant, rather than a claim to reproduce Terrier's global-collection system,
query expansion or published results. The formula requires no fitted smoothing
parameter; that does not make it universally accurate.

For each query feature w present in candidate d:

```text
t       = tf(w,d)
n       = length(d)
f       = t/n
PB(w)   = (sum(d in C, tf(w,d)) + P0(w)) / (sum(d in C, length(d)) + 1)
A(t,f)  = (1-f)^2 / (t+1)
I(w,d)  = A(t,f) * [t * log2(f/PB(w)) + 0.5 * log2(2*pi*t*(1-f))]
score   = sum(w in q, tf(w,q) * I(w,d)) + assessed_usefulness_bits(q,d)
```

Absent features contribute zero. At t=n, use the analytic limit I=0, avoiding
an arbitrary floating-point epsilon or a logarithm of zero. Empty sources
have no postings. Literal/direct lookup and time/revision restrictions still
apply before weighting. The term weights are in bits; they are not a normalized
conditional query model or a probability that the source is correct. Accordingly
`best_conditional_query_bits` must be absent for this candidate. Background query
code length and routing ambiguity remain separately labeled diagnostics. Record
the ranking model name in new receipts and benchmark metadata.

## Decision gates

First run the existing 129-case / 149.8 MB development matrix against both ordinary
and Porter SQLite controls, with the same 512 references, 32 excerpts, 2 KiB
excerpt cap and 16/64 KiB packet limits. Preserve complete reference quotes,
literal declarations, historical visibility and exact originals. Reject a
default change that hides a material existing evidence regression.

SciFact and FiQA are now inspected evaluation data. If the development checks
pass, compare their average ranking metrics before considering a new independent
question set. Do not describe these reused datasets as fresh holdouts. Verify
raw-only huge sources, finite edge scores, dense/sparse equivalence and the
complete offline workspace suite. Use no provider calls or result cache.

Any failed candidate and its aggregate remain reproducible. If this default is
rejected, restore the existing Dirichlet model and keep the active superiority
goal open. Faster execution, lower entropy or passing implementation tests do
not substitute for the evidence and ranking gates.

## Result: rejected at the first evidence gate

The fixed candidate passed 210 offline all-features tests, including finite
degenerate scores and an independently reconstructed dense reference. A first
numeric-reference assertion used an incorrect hand-entered decimal; it was
corrected to the independently evaluated formula before the successful run.
Passing these tests did not predict better retrieval.

| Complete supporting-reference pairs | Prior Nine, 16 KiB | Candidate, 16 KiB | Prior Nine, 64 KiB | Candidate, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Redacted history | 19/21 | 17/21 | 19/21 | 18/21 |
| Public document questions | 19/20 | 19/20 | 19/20 | 19/20 |
| Literal declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Synthetic huge-tool facts | 9/9 | 9/9 | 9/9 | 9/9 |

At 16 KiB, complete history questions fell from 12/14 to 10/14. At 64 KiB
they fell to 11/14, and mean history context increased from 25,380 to 65,432
bytes. The document context also increased from 46,276 to 65,493 bytes without
recovering its missing fact. History median latency was 18.47/22.87 ms at the
two caps, compared with the prior same-host 13.08/16.71 ms run. These are
separate timing runs, not a causal speed estimate.

The missing history sources moved from ranks 109 and 39 to 211 and 167. Two
previously retained facts were also lost at the tight cap. The missing public
document moved from 342 to 266 but remained absent from the returned context.
This changed ordering without supplying the missing relationship signal.

Ordinary Unicode SQLite, measured in the candidate's same run, retained
18/21 and 20/21 history pairs at the two caps, and 18/20 and 19/20 document
pairs. Its history medians were 88.91 and 157.09 ms. The candidate's faster
local queries do not compensate for poorer evidence.

All 18,495 exact originals passed verification, all 1,548 timed repeat packets
were stable, and the independent exact-range auditor checked 516 saved packets.
No model calls or full-body SQL fallback occurred. Because the first development
gate failed, no further Porter, SciFact, FiQA or independent holdout test was
used to search for a favorable aggregate. The unused ArguAna gate was only
researched for provenance and licensing; no questions or labels were inspected.

The default scorer was restored. The only retained engine change is an explicit
`RETRIEVAL_MODEL` diagnostic, used by receipts and benchmark metadata, so a
later experiment cannot silently share the previous model label. The original
archive and retrieval formula remain unchanged.

- [Rejected candidate aggregate](benchmarks/nine-dph-rejected-development-2026-10-19.json)
- [Candidate patch](../benches/experiments/conditional-dph-rejected.patch)

The patch applies with `git apply --unidiff-zero` to the restored source delivered
with this report; apply and reverse were verified in an isolated copy. Future
source changes may invalidate its line-addressed hunks. The candidate binary and
private per-query audits remain outside Git. Do not enable the rejected model
in the production/default path.
