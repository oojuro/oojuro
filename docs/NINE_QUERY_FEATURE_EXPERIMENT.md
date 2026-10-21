# Preserve query information in probability scoring

Protocol fixed before evaluation on 2026-10-19; starting source `ac6775a`.
The default fixed-128 model remains the baseline. Median-strength and equal
background-mixture controls lost evidence and were rejected.

## Inspection and fixed candidate

The two existing history misses have 21/20 raw query words; filtering removes
8/9 occurrences. Several removed words occur in the missing 1,155-byte source.
This motivates testing the representation before another weighting formula.
It does not establish that the removed words are helpful or that they measure
truth. No private query or source text is published.

Keep candidate discovery based on the existing filtered query, including its
all-function-word fallback and identifier backoff. Once this eligible candidate
view C is fixed, restore the filtered word occurrences for **ordinary query
scoring only**. Do not widen candidates using those words. Quoted phrases,
known identifiers, direct keys and empty queries keep their current path.

For ordinary queries with at least one nonfiltered word, write Qc for the
existing query features and Qf for the discarded normalized occurrences:

```text
Qs       = Qc + Qf
PB(w)    = (sum(d in C, tf(w,d)) + P0(w)) / (sum(d in C, len(d)) + 1)
P(w|d)   = (tf(w,d) + 128 * PB(w)) / (len(d) + 128)
score    = sum(w in Qs, tf(w,Qs) * log2(P(w|d)/PB(w))) + usefulness_bits(q,d)
```

Words unseen throughout C remain omitted from relative scoring as before.
Compute restored-word background counts only from C after both clocks,
lower-bound and latest-version checks. Additional eligible but content-unmatched
records must not change scores. The full match set is unchanged; ordering and
the returned 512-reference frontier may change.

The index already contains these words, so no new index, dependency, embedding,
LLM or stored-text migration is needed. Additional posting visits and score
operations must be included in timing. The 2 KiB literal-first reader continues
using its existing filtered query. This isolates scoring from excerpt selection.

## Gates

Test hand-computed scores, independent dense/sparse equivalence, all-filtered
queries without double counting, unchanged candidate support, unknown queries,
both clocks, revisions, range bounds, literals, identifiers and reopen behavior.

Use the existing 18,495 originals, 129 development cases, 32-source cap, 512
reference depth and 16/64 KiB serialized context budgets. Require an additional
complete reference with no individually lost reference at either cap relative
to the default: 19/21 history, 19/20 documents, 86 declarations and nine huge-tool
checks. Independently audit every returned range and reference label eligibility.
Passing implementation tests alone does not pass the evidence gate.

Keep the stronger existing SQL controls. If the development candidate passes,
also check SQL's existing full-word query mode, then the inspected SciFact/FiQA
checks before consuming the still-unused ArguAna gate. No changed byte budget,
extra returned count, coefficient sweep or independent-gate rescue is planned.
Actual agent tasks, model-token counts, throughput and hosting remain separate.

## Result: rejected

The candidate passes 217 offline all-features tests, with zero failures and one
GPU test ignored. Independent dense-score checks, a hand-computed example,
candidate support, both clocks, revision history and reopen checks pass.

The unchanged 18,495 originals were cross-checked and 1,548 timed queries
completed with no changed repeat packets. Independent auditing checks all 516
returned packets and their original ranges and historical reference labels.

| Complete references | Default, 16 KiB | Candidate, 16 KiB | Default, 64 KiB | Candidate, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| History | 19/21 | 18/21 | 19/21 | 18/21 |
| Documents | 19/20 | 18/20 | 19/20 | 19/20 |
| Rust declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Huge tools | 9/9 | 9/9 | 9/9 | 9/9 |

No previously missing reference is recovered. One previously retained history
reference drops at both caps, and one document drops at 16 KiB. Wide history
contexts also grow from 25,380.5 to 31,307.8 mean serialized bytes. SQL controls
retain exactly the same individual evidence as before the change. The proposed
ranker fails the evidence gate and is restored to the default fixed-128 model.
No inspected or unused independent corpus gate is consumed.

The result does not show that function words are always harmful. It rejects this
specific unconditional restoration rule on these development fixtures. No
coefficient fitting or post hoc exception is used to retain it.

[Aggregate measurements](benchmarks/nine-full-query-scoring-rejected-2026-10-19.json).
The rejected implementation and its equation tests are preserved in
`benches/experiments/full-query-scoring-rejected.patch`. Starting with `ac6775a`,
copy the patch into a clean checkout and apply with `git apply --unidiff-zero`.
The zero-context patch includes no private query or history text.
