# Lexical normalization with shared query controls

This records the source delivered at `85dafc7`. Later experiment/model labels
may differ; the research patches below target that snapshot.

Protocol recorded on 2026-10-19 before candidate evaluation. The starting
engine is `41fbb7f`, which keeps raw counts and performs no stemming. Its
SciFact ranking quality trails SQLite's Porter index. This experiment tests
one fixed language normalization change rather than another frequency weight.

## Fixed candidate

Normalize ordinary ASCII alphabetic words with the English Snowball algorithm
from `rust-stemmers` 1.2.0, already present in the local dependency cache. Keep
whole underscore identifiers, digits and non-ASCII words literal after the
existing lowercase operation. Apply the same mapping to document and query
features and to the original-range reader. Quoted queries still require exact,
case-sensitive original bytes and word edges after candidate generation.

```text
g(w)       = EnglishSnowball(w)   for ordinary ASCII alphabetic words
g(w)       = w                   for other features
tf_g(z,d)  = sum(w where g(w)=z, tf(w,d))
n_g(d)     = sum(z, tf_g(z,d))
PB_g(z)    = (sum(d in C, tf_g(z,d)) + P0(z)) / (sum(d in C, n_g(d)) + 1)
P_g(z|d)   = (tf_g(z,d) + 128 * PB_g(z)) / (n_g(d) + 128)
score_g    = sum(z in g(q), tf(z,g(q)) * log2(P_g(z|d) / PB_g(z)))
             + assessed_usefulness_bits
```

This merges feature counts; it does not add a neural encoder, generated summary,
new storage format or another frequency hyperparameter. Original records,
timestamps, exact identifiers and verified outcome rules remain unchanged.
Derived indexes rebuild on open. The source reader may cache up to 4,096 word
forms per call; cache overflow must change cost only, never matching or offsets.

The information-theoretic query-likelihood model remains the previous one over
normalized features. Stemming is an established lexical technique, not a new
theorem or a guarantee of semantic equivalence. The dependency implements
[Snowball algorithms](https://github.com/CurrySoftware/rust-stemmers).

For a word variable W and its deterministic class Z=g(W), the entropy chain
rule gives `H(W)=H(Z)+H(W|Z)`. A smaller class vocabulary discards within-class
word identity. Exact originals preserve that information; an excerpt must still
retain the words that establish the requested fact. Lower class entropy alone
does not imply higher accuracy. Every background count above is computed only
from sources eligible under the valid-time and known-time cutoffs and the
latest-version rule; future sources cannot train a past query's distribution.

## Controls and gates

Before the candidate is built, run raw-count/un-stemmed Nine with SQLite's
Porter index and the **same fixed query stopword list**. Both keep original
questions, literal phrase constraints and the all-stopword fallback. Record
this control separately from the earlier SQLite pipeline that retained query
stopwords. Porter and Snowball are different stemmers; this is an explicit
pipeline comparison, not an identical tokenizer experiment.

The previously inspected 129-question history/public fixture and SciFact are
development checks. Preserve complete supporting quotes under the existing
16/64 KiB caps, literal declaration retrieval, time cutoffs, exact original
recovery and raw-only huge tools. Inspect candidate development results before
its fresh independent gate. Reject a default change that hides a material
existing accuracy regression. No smoothing or per-question rule sweep is planned.

## Fresh independent gate

Use [FiQA-2018 in BEIR](https://huggingface.co/datasets/BeIR/fiqa): 57,638 public
documents and 648 test questions, with 1,706 binary positive relevance pairs.
The canonical archive matches the checksum published by the BEIR authors. Its
maintainer's dataset card lists CC BY-SA 4.0. The raw fixture remains outside Git.
No question text or relevance case was inspected while choosing this candidate.
The initial generic evaluator called document text an "abstract" in one
boilerplate limitation. FiQA's stored originals and cross-checks used its exact
finance answer/document text; the label did not alter input. The current
evaluator uses the correct generic wording.

Both engines ingest exactly the same title/newline/text originals. Only query
text, query identity and common time/limits enter retrieval. Relevance labels
remain evaluator-only. FiQA uses overlapping numeric query/document IDs;
exclude a matching raw ID from ranked results and context before packing,
following BEIR's default evaluation policy. No positive test pair has that
same-ID collision. Report the collision count explicitly instead of rejecting
the valid dataset or silently counting a self result.

Use three timed repeats after warm-up, alternating engine order, up to 512
candidate references, 32 returned excerpts, 2 KiB per excerpt and 16 KiB total
serialized evidence. Measure nDCG/MRR@10, recall@10/32, context source retention,
full query latency and evidence bytes. Original loading/replay and verification
are outside query time; tokenization, retrieval, excerpt expansion, serialization
and synced receipts are inside. No result cache or provider call is allowed.

These labels measure document relevance, not retained answer rationales,
generated answers, repeated agent mistakes or financial correctness. All data
and per-question audits remain local. After inspection, this gate becomes
evaluation data; further model selection needs another independent check.

## Development correction before the independent gate

The first candidate improved SciFact nDCG@10 from 0.6536 to 0.6789, while
SQLite with Porter and shared stopwords scored 0.6876. The original history
coverage stayed 19/21. Document coverage rose from 16/20 to 17/20 at 16 KiB
but fell from 18/20 to 17/20 at 64 KiB. Inspection showed that the correct
sources ranked higher: the excerpt reader had moved to different sections and
omitted two previously retained quotes. This version was not promoted.

The revised reader preserves its original literal-word coverage rule whenever
the selected source has any literal query-word overlap. It uses normalized
matching only when that literal pass finds no overlap. Quoted queries retain
their strict exact-byte rule. This changes neither the ranking model nor its
parameters; it is an explicit evidence-selection ablation, fixed before the
candidate is evaluated on FiQA. A regression fixture checks that a shorter
variant-only span cannot displace a literal match, and cache-overflow coverage
still exercises normalized fallback. No reference IDs, quotes or question-specific
rules enter the reader.

## Large-field comparison correction

The first Porter/shared-stopword run on the larger development corpus was
stopped after confirming a cost pathology in the bundled SQLite snippet
implementation. Its candidate-window loop calls a scorer that scans all match
instances again. Dense repeated matches can therefore require quadratic work
even when the requested snippet is only 64 tokens. This was confirmed in the
cached SQLite 3.53.2 source as well as the
[upstream implementation](https://github.com/sqlite/sqlite/blob/master/ext/fts5/fts5_aux.c).
The interrupted run produced no completed comparison and is excluded.

The comparator now uses a fixed 1 MiB source-size guard: native snippets remain
enabled for smaller fields; larger fields use the same streaming original-range
reader inside SQLite. A lazy SQL CASE avoids invoking native snippet selection
on those large fields. Only bounded evidence crosses the SQL interface; this
does not copy whole large bodies into the client. The report records both the
size limit and the number of large-source streaming reads, separately from any
full-body fallback. An exact-source fixture checks the selected route, bounded
evidence and recovered original bytes. This is a shared cost control and does
not depend on a relevance label, reference ID or expected quote.

## Completed development checks

All of the following use exact sources, uncached queries, synced receipts and
zero model/provider calls. Serialized evidence bytes are measured; model tokens
and generated answer accuracy are not measured.

On the frozen 18,495-record / 149,793,732-text-byte corpus, the revised candidate
retained these complete supporting-reference pairs. This corpus includes linked
passage copies of synthetic tool sources; the separate raw-only check below
does not. The identifier/declaration caller policy is shared by both engines.

| Workload | Nine, 16 KiB | SQLite Porter, 16 KiB | Nine, 64 KiB | SQLite Porter, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Redacted history | 19/21 | 17/21 | 19/21 | 18/21 |
| Public document questions | 19/20 | 16/20 | 19/20 | 18/20 |
| Literal declarations, three groups combined | 86/86 | 86/86 | 86/86 | 86/86 |
| Synthetic huge-tool facts | 9/9 | 9/9 | 9/9 | 9/9 |

Nine's complete document coverage was 16/20 and 18/20 at the two caps before
this normalization/reader change; history remained 19/21. Complete history
**questions** are 12/14, rather than 19/21: some questions need multiple sources.
SQLite Porter is one explicit baseline configuration, not a claim that stemming
always improves SQLite or that it is the best configuration on every workload.

At 16 KiB, median/p95 local query latency was 14.97/20.97 ms for Nine history
and 89.18/163.70 ms for SQLite Porter. The document medians/p95s were
14.33/28.49 ms and 97.76/647.01 ms respectively. At 64 KiB, Nine history used
25,380 mean evidence bytes versus 65,386 for Porter; both still missed evidence.
All 18,495 original-record cross-checks passed, repeat packets were unchanged,
and SQLite used 72 large-source streaming reads with zero full-body fallbacks.

The raw-only check used four exact originals totaling 33,554,632 text bytes
(roughly 8 MiB each), 2 returned snippets and a 16 KiB cap. Both engines retained
9/9 synthetic facts. Nine used 2,501 mean evidence bytes at 71.20 ms median /
86.44 ms p95; SQLite Porter used 4,167.67 bytes at 178.96 / 229.96 ms. This
removes prebuilt passage copies as an explanation for success on those facts.
It does not establish general semantic retrieval, real tool-log performance,
bounded-memory operation or unlimited input. Original text and indexes remain
RAM resident, and selected huge sources are scanned for exact excerpts.

The clean revised-reader SciFact run scored 0.678942 nDCG@10 versus Porter's
0.687646. Nine improved over its raw-word 0.653613 baseline but still trailed
SQLite. Median/p95 latency was 3.31/6.04 ms versus 15.66/27.08 ms. All 5,183
exact originals were verified. Context source retention measures relevant
document IDs; SciFact labels do not certify that the excerpt proves the claim.

### Ordinary SQLite control

Re-running the same delivered reader with SQLite's ordinary Unicode tokenizer
keeps Nine's evidence packets identical and gives a stronger history baseline:

| Workload | Nine, 16 KiB | SQLite Unicode, 16 KiB | Nine, 64 KiB | SQLite Unicode, 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Redacted history | 19/21 | 18/21 | 19/21 | 20/21 |
| Public document questions | 19/20 | 18/20 | 19/20 | 19/20 |

The declarations remain 86/86 and huge-tool facts 9/9 for both engines. At
16 KiB, history median/p95 is 13.08/18.60 ms for Nine versus 80.17/107.20 ms;
document latency is 10.84/16.94 ms versus 63.11/195.17 ms. These are paired
same-run comparisons. At 64 KiB, Unicode SQLite retains one more history
reference than Nine, using 65,433 mean evidence bytes versus 25,380. Returning
less text is not a win when it omits needed evidence. Overall superiority is
still not established.

### Fresh FiQA result

The fixed candidate was evaluated on all 648 FiQA test questions after the
development checks. No individual FiQA question or label was inspected during
candidate selection. The first exposure was the completed aggregate result.

| Metric | Nine raw-word control | Nine delivered | SQLite Porter/shared stopwords |
| --- | ---: | ---: | ---: |
| nDCG@10 | 0.218747 | 0.240305 | 0.254469 |
| MRR@10 | 0.269250 | 0.297467 | 0.311989 |
| Recall@10 | 0.276535 | 0.293995 | 0.314659 |
| Recall@32 | 0.367383 | 0.411487 | 0.441448 |
| Relevant source IDs retained in context | 0.335292 | 0.354420 | 0.361763 |
| Questions retaining all relevant source IDs | 0.191358 | 0.208333 | 0.211420 |

The delivered same-run latency median/p95 is **19.54/45.87 ms** for Nine and
**78.31/181.62 ms** for SQLite. Mean evidence is 16,304.51 versus 16,307.85
bytes: this workload supplies no material context-size advantage. Both use
1,944 timed queries after warm-up. The earlier raw-word run's 19.64 ms median
is a separate run; do not attribute a speed change to normalization from it.

All 57,638 exact-source checks passed. Both candidate rankings and packets
were stable across repeats, no large-source or full-body SQL fallback was
needed, and both providers were unused. There were 55 labeled query/document
ID collisions; matching IDs were excluded before packet packing. None was a
positive test pair. Metrics were independently recomputed from saved ranks and
canonical labels, including this exclusion policy.

This is a measured average ranking gain over Nine's previous model. Nine still
loses to the Porter baseline on every listed quality metric. The experiment does
not measure generated answers, error prevention, user satisfaction, model-token
savings, concurrent service latency or performance versus hosted PostgreSQL.
FiQA is now inspected evaluation data; it is not a fresh gate for later tuning.

### Compression scope

Normalization does not change the original archive or codec. Original text
bytes and native source-frame bytes in the verified stores are:

| Store | Original text bytes | Native source archive bytes |
| --- | ---: | ---: |
| Raw-only synthetic tools | 33,554,632 | 11,940,960 |
| Mixed development corpus | 149,793,732 | 67,955,792 |
| SciFact | 7,778,381 | 6,562,326 |
| FiQA | 44,334,118 | 46,982,245 |

Native archives include source metadata, frame headers and checksums. They
exclude RAM indexes, query/outcome logs and filesystem allocation overhead.
FiQA's archive exceeds its plain text bytes; a compact archive is not a
guarantee for short records or arbitrary data. The synthetic opaque payload
also does not shrink. Lossless storage compression and bounded query excerpts
solve different problems.

## Delivered decision and verification

Retain the fixed Snowball counts with the literal-first excerpt reader: it
improves both externally labeled averages and the existing document-evidence
checks, while preserving the checked history, literal, temporal and raw-tool
behavior. This is an incremental model-free improvement. The goal of superior
quality remains open, including the stronger Unicode history control.

The full offline all-features workspace suite passed **209 tests, zero failed,
one physical-GPU test ignored**. Tests include independent dense/sparse scoring,
word-form aggregation, literal identifiers, Unicode offsets, both time cutoffs,
normalization-cache overflow, large SQLite source routing and legacy receipts.
New query receipts identify the count and normalization models; older receipts
remain readable. The source archive format is unchanged. Query repetition alone
does not teach factual correctness or update model weights.

The five delivered matrices contain 8,838 timed queries, 99,815 per-run
original-record cross-checks and zero changed repeat packets. An independent
standard-library audit verified 2,946 saved delivered packets: canonical public
relevance arithmetic and exact-original quote retention for the development/raw
matrices. It also verified the 1,896 raw-word public baseline packets. Private
questions, quotes and raw sources remain excluded from Git.

## Aggregates and reproduction

- [Shared-control SciFact baseline](benchmarks/nine-scifact-shared-raw-2026-10-19.json)
- [Delivered SciFact](benchmarks/nine-scifact-snowball-2026-10-19.json)
- [Fresh FiQA raw-word baseline](benchmarks/nine-fiqa-shared-raw-2026-10-19.json)
- [Delivered FiQA](benchmarks/nine-fiqa-snowball-2026-10-19.json)
- [Development versus Porter](benchmarks/nine-snowball-development-2026-10-19.json)
- [Development versus Unicode](benchmarks/nine-snowball-unicode-control-2026-10-19.json)
- [Raw-only huge tools](benchmarks/nine-snowball-raw-tools-2026-10-19.json)
- [Rejected normalized-only reader](benchmarks/nine-normalized-only-rejected-2026-10-19.json)

For the public checks, download the canonical BEIR archives, verify the published
checksums and extract them outside Git. FiQA's published archive MD5 is
`17918ed23cd04fb15047f73e6c3bd9d9`. The evaluator supports binary-positive qrels;
it does not silently treat arbitrary graded relevance as binary.

```text
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-beir-bench --dataset data/public/scifact --dataset-name SciFact --run-dir data/private/new-sf --output results/new-sf.json --repeats 3 --sqlite-porter --shared-stopwords
oojuro-lab nine-beir-bench --dataset data/public/fiqa --dataset-name FiQA --run-dir data/private/new-fiqa --output results/new-fiqa.json --repeats 3 --sqlite-porter --shared-stopwords
```

Use fresh paths; writes refuse to replace existing results. Public source replay
and exact verification are outside query time. Query processing, expansion,
packet serialization and synced receipts are inside. The local single-client
Windows runs use memory-resident sources/indexes, not a hosted server or a
bounded disk cache. Neither result cache is used.

Two research patches apply to source `85dafc7` using
`git apply --unidiff-zero`. Both were checked to apply and reverse under Git
line-ending normalization in isolated copies. The
[raw-word control](../benches/experiments/raw-word-control.patch) restores the
previous raw count/word engine and reader, keeping current evaluator controls
and diagnostic labels. It is a reconstruction for quality reproduction, not a
promise of bit-for-bit historical timing; the original baselines used the
saved pre-normalization executable. The
[normalized-only reader](../benches/experiments/normalized-only-reader.patch)
replays the rejected reader choice. The new literal-priority regression test
is expected to fail with that rejected choice. These patches are not enabled
in the delivered build; only that build has the 209-test result above.
