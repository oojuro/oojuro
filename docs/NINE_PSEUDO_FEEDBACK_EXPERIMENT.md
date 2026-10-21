# Two-pass probabilistic query expansion

Protocol fixed before evaluation on 2026-10-19; source base `e50d2c7`.
Both complementary-context screens failed to recover a missing Nine reference.
They reranked fixed query features. This candidate instead tests adding related
words inferred from the initial retrieved text, without a neural model.

[Diaz and Metzler (2006), Section 2.1](https://ciir.cs.umass.edu/pubfiles/ir-468.pdf)
describes interpolating a relevance model estimated from retrieved documents
with the original query distribution, commonly called RM3. This motivates our
fixed RM3-style candidate; no parameter setting or quality guarantee is borrowed
from that paper. There is no external corpus, LLM or embedding provider.

## Fixed mathematics and implementation

Use the current fixed-128 query-likelihood engine for a first pass with exactly
ten returned sources, independent of the requested final count. Let s(d) be its
original code-gain score, including any already verified usefulness prior.
For each source, count existing normalized retrieval features that consist only
of ASCII letters, have length at least two and are not a normalized existing
stop word. Normalize their counts within that source. Empty sources contribute
no expansion mass. The feedback model and final weighted query are:

```text
p(d|q)  = 2^(s(d)-max(s)) / sum(j in top10, 2^(s(j)-max(s)))
RM(w)   = sum(d in top10, p(d|q) * tf_filtered(w,d)/len_filtered(d))
E       = the 20 greatest RM(w), ties broken by normalized word
PE(w)   = RM(w) / sum(u in E, RM(u)) for w in E
N       = sum(w, original_query_count(w))
q*(w)   = 0.5 * original_query_count(w) + 0.5 * N * PE(w)
```

The expansion contributes exactly half of total query mass N. Preserve every
original query feature with its other half. Repeat the existing historically
eligible query-likelihood retrieval with fractional positive counts q*. Its
background remains query-addressed and must use both clocks, latest versions
and the lower bound before counts are collected. Expansion can widen the match
set. Stop at this second pass; there is no iterative drift or coefficient sweep.

Empty, quoted, underscore-identifier, direct-key and zero-result queries keep
the existing path. No feedback words are generated from ineligible sources.
No source bytes, timestamps or stored assertions change. Treat the first pass
as pseudo relevance, never as a verified outcome or a probability of truth.
Only the final result is cached and durably receipted for the original query;
both passes' posting visits and score work count in metrics and latency.

The sparse query-likelihood kernel will accept fractional counts. Verify it
against a hand-computed dense score and retain existing dense equivalence,
literal, clock, revision, cache, exact-original and reopen checks. Check query
mass, deterministic term ties, seed depth independent of final k, source-byte
preservation and lack of future/late-known leakage in learned query words.

## Quality and cost gate

Same 18,495 frozen originals, all 129 development cases, 512-reference frontier,
32-source cap, 2 KiB reader and 16/64 KiB serialized context limits. Three timed
repeats after warm-up; no result cache, provider or changed return budget.
Require at least one additional complete reference and no individual loss at
either cap versus history 19/21, documents 19/20, declarations 86/86 and tools
9/9. Audit every range and historical reference label. Keep the stronger SQL
reader/query controls and assess the added second-pass latency independently.

If development passes, compare the inspected SciFact/FiQA controls before using
the unused ArguAna gate. No independent corpus is used to rescue a failed
development setting. Actual agent answers and mistake avoidance remain separate.

## Development result: first gate passes

The Rust implementation passes 219 ordinary offline all-features tests, including
hand-computed fractional query scores, an exact feedback-mass example, deterministic
term cutoff ties, filtered noise, first-stage depth independent of final k, both
clocks, lower bounds, original recovery, cache behavior and reopen. The physical
GPU test is ignored. A separate opt-in public-fixture diagnostic is described
below; it does not run in the portable suite.

Two development matrices each cross-check 18,495 originals, complete 1,548 stable
timed queries and pass 516 independent exact-range packet audits. One uses the
existing shared-stopword SQL control; the other uses SQL's full-word query mode.
Nine's evidence is identical across those SQL configurations.

| Complete references | Default, 16 KiB | Candidate, 16 KiB | Default, 64 KiB | Candidate, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| History | 19/21 | 20/21 | 19/21 | 20/21 |
| Documents | 19/20 | 19/20 | 19/20 | 19/20 |
| Declarations | 86/86 | 86/86 | 86/86 | 86/86 |
| Huge tools | 9/9 | 9/9 | 9/9 | 9/9 |

One previously missing history reference moves from rank 39 to 7. No previously
retained individual reference is lost at either cap. Complete history questions
rise from 12/14 to 13/14, and retained supporting quotes from 26/28 to 27/28.
The other history miss moves 109 to 92 and the document miss 342 to 144; neither
fits within the retained source cap. Increasing weights is not presented as
proof of certainty or correctness.

Wide mean history bytes fall from 25,380.5 to 23,643.3; documents grow from
46,276.2 to 51,720.3. The two-pass candidate's history medians are 19.06/18.15 ms
at the two caps and documents 17.47/15.87 ms in the first matrix. These legacy
matrix times exclude final whole-context serialization and are diagnostic only.
The extra pass adds work; these separate runs do not establish a causal latency
ratio to the earlier default. No provider calls are made.

## Inspected public checks

SciFact verifies 5,183 originals and evaluates all 300 questions, with 1,800
stable timed queries and 600 independently recomputed audit metrics. There are
zero query/document ID collisions. SQL's quality exactly matches its prior
control. The candidate does not improve the primary ranking metric:

| SciFact metric | Default Nine | Expanded Nine | SQLite Porter |
| --- | ---: | ---: | ---: |
| nDCG at 10 | 0.678942 | 0.677657 | 0.687646 |
| Recall at 10 | 0.800944 | 0.802056 | 0.824500 |
| Recall at 32 | 0.872222 | 0.858000 | 0.892889 |
| Context relevant-document recall | 0.810111 | 0.807889 | 0.825333 |

The small history gain is insufficient for a general default change. The
unchanged setting is next checked on inspected FiQA; no favorable coefficient
is selected from either public corpus. The ArguAna gate remains unused.

### FiQA seed-exclusion diagnostic

The existing evaluation removes matching query/document IDs from returned
results before packing. A two-pass method also needs to check its first-pass
seeds. The opt-in Rust fixture audit checks all 55 collisions among the 648
FiQA test questions: **none occurs in its ten-source first-pass seed**. It runs
without outcome labels influencing ranking and saves aggregate counts only.
Therefore that specific potential self-feedback issue is absent in this FiQA
comparison. This is dataset-specific evidence, not a general exclusion guarantee.
ArguAna has many such collisions and must not be evaluated with this candidate
until self-document exclusion is enforced before feedback as well as output.

### FiQA result and default decision

All 57,638 exact originals and 648 test questions were checked. The 3,888 timed
queries are stable and an independent standard-library verifier recomputes
all 1,296 saved query-metric audits. SQL quality exactly matches the prior
Porter control. The same fixed candidate regresses on the primary metric:

| FiQA metric | Default Nine | Expanded Nine | SQLite Porter |
| --- | ---: | ---: | ---: |
| nDCG at 10 | 0.240305 | 0.234847 | 0.254469 |
| MRR at 10 | 0.297467 | 0.286858 | 0.311989 |
| Recall at 32 | 0.411487 | 0.415710 | 0.441448 |
| Context relevant-document recall | 0.354420 | 0.343266 | 0.361763 |

In this FiQA run Nine's median/p95 is 57.21/99.59 ms versus SQL's
48.69/102.78 ms. The candidate's median is slower than this same-run SQL arm;
the p95 ordering differs. Legacy final-context serialization exclusions still
apply. More source recall at rank 32 does not offset lower early ranking and
lower retained-context recall.

The history gain is real on these fixtures, but both inspected public sets lose
nDCG and FiQA adds an unacceptable latency tradeoff. Keep the existing default.
The candidate and its tests are preserved as a reproducible Rust patch, rather
than enabled as a general search path. No coefficient, genre-specific exception,
new byte cap or held-out question was used to rescue this setting. ArguAna
remains unused and actual agent-answer/mistake outcomes remain unmeasured.

Across the two development matrices and two public checks: 8,784 stable timed
queries, 99,811 exact-original cross-checks and 2,928 independently verified
packets/metric audits. These are 129 reused development cases and 948 inspected
public questions, not thousands of independent new user tasks. Model calls are
zero. Runtime engine and tests are restored exactly to the fixed-128 baseline;
restored build and compatibility verification are recorded separately below.

## Reproduce the archived candidate

Copy `benches/experiments/rm3-query-expansion-not-default.patch` from the delivered
revision into a clean `e50d2c7` checkout and apply with `git apply --unidiff-zero`.
The patch includes the fractional kernel, first-pass model and implementation
tests. Its optional public-fixture test is ignored in the portable suite and
requires the same local canonical FiQA fixture/source archive.

```text
cargo test --offline --workspace --all-features
cargo test --offline -p oojuro-nine public_fiqa_feedback_seed_self_exclusion_audit -- --ignored --nocapture
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/fresh-feedback --output results/fresh-feedback.json --cases data/private/nine-combined-cases-v1.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords
oojuro-lab nine-beir-bench --dataset data/public/scifact --dataset-name SciFact --source-run data/private/nine-beir-raw1 --run-dir data/private/fresh-feedback-sf --output results/fresh-feedback-sf.json --repeats 3 --sqlite-porter --shared-stopwords
oojuro-lab nine-beir-bench --dataset data/public/fiqa --dataset-name FiQA --source-run data/private/nine-fiqa-shared-raw1 --run-dir data/private/fresh-feedback-fiqa --output results/fresh-feedback-fiqa.json --repeats 3 --sqlite-porter --shared-stopwords
```

Use fresh paths. Omit `--shared-stopwords` in the development command for the
full-word SQL query control. The opt-in fixture audit also refuses to replace
its existing aggregate output. All source fixtures and full audits stay local.

- [Development matrix](benchmarks/nine-pseudo-feedback-development-2026-10-19.json)
- [Full-query SQL control](benchmarks/nine-pseudo-feedback-full-query-2026-10-19.json)
- [SciFact](benchmarks/nine-pseudo-feedback-scifact-2026-10-19.json)
- [FiQA](benchmarks/nine-pseudo-feedback-fiqa-2026-10-19.json)
- [Self-ID seed diagnostic](benchmarks/nine-fiqa-feedback-seed-audit-2026-10-19.json)

## Restoration verified

The exact runtime and portable tests from `e50d2c7` are restored. The complete
offline all-features suite passes 216 tests, zero failed and one physical-GPU
test ignored. A fresh release build confirms model `dirichlet-128-v1`.
Its 1,548 timed development queries are stable, all 18,495 originals are
cross-checked and all 516 previous ordered candidate/source-range/byte audits
are reproduced exactly for both engines and both context caps. The independent
portable auditor checks those 516 packets again. This verifies rollback rather
than a new retrieval gain.

[Restored-default aggregate](benchmarks/nine-feedback-restored-2026-10-19.json).
Including this check, the milestone has 10,332 stable timed queries, 118,306
per-run original cross-checks and 3,444 packet/metric audits. The two offline
selection screens and their reproduction checks are separate from those totals.
No new runtime dependency or default search operator is shipped.
