# Conditional LZ4 query codes as a ranking experiment

Protocol fixed before evaluation on 2026-10-19, after four field models were
rejected and the original scorer was verified restored. This laboratory
reranker uses the cached codec directly, with no change to the engine default.

## Primary evidence and limits

[Hurwitz, Nicholas and Raff (2024)](https://arxiv.org/html/2410.15280v1)
find that compression rate alone does not predict compression-distance
classification quality. Their sequence classification result does not prove
short-query retrieval quality. Our asymmetric query code is not their NCD
formula or a claim to compute Kolmogorov complexity.

The [official LZ4 block format](https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md)
requires five trailing literal bytes and the last match to start at least
twelve bytes before the end. The cached `lz4_flex` 0.11.6 exposes external
dictionary compression/decompression. No package or provider is installed.

## Fixed code and dictionary

For an existing eligible candidate d, use its exact source bytes as dictionary
if at most 8 KiB. Otherwise use the existing query-addressed exact range reader
with an 8 KiB cap. This dictionary is separate from the unchanged 2 KiB final
excerpt. No metadata, generated assertion or original mutation is introduced.

Let Q be the exact UTF-8 query bytes followed by twelve zero bytes. This fixed
reversible padding lets query suffixes participate in matches; decoding removes
the twelve bytes. Do not normalize, stem, summarize or omit query words. C0 is
eight times the ordinary size-prefixed compressed Q length. Cd is the matching
external-dictionary code length. Both include the u32 uncompressed-size header.

A whole short source needs no location given d. A query-selected dictionary
from a long source needs its start a and byte length n transmitted:

```text
gamma(x) = 2*floor(log2(x)) + 1, x >= 1
R(d,q)   = 0                                      if |d| <= 8192
R(d,q)   = gamma(a+1) + gamma(n+1)                otherwise
L(q|d)   = 1 + min(C0, Cd + R(d,q))
```

The one-bit selector chooses the independent or dictionary branch. The same
fixed-width source address applies to every candidate in the supplied pool and
does not affect ranking. The selected range is paid for, so decoding does not
need q to locate the dictionary. Codes are a retrieval hypothesis, not factual
confidence. Compression bytes are distinct from answer-model tokens.

Order the existing top-512 pool by increasing L, retaining original rank for
ties. Keep literal, underscore-addressed, direct-key and empty queries on the
old path. Do not change the pool, final source count, final excerpts or budgets.
Do not sweep codecs, blend weights, padding, windows or normalization after
seeing development results.

## Implementation and gates

Use `--conditional-compression` on the Rust lab's `nine-scale-requery`. Engine
and archive formats stay unchanged. The durable initial receipt still records
the base ranking; label this lab boundary explicitly. If quality passes, move
the accepted model into the engine before receipt, cache and routing-entropy
computation and verify that contract before any default/latency claim.

Test lossless padded-query roundtrips, Unicode, short/empty queries, independent
fallback, location penalties, stable ties and addressed bypass. The reranker
only permutes an existing eligible pool. Test that future and late-known
records cannot enter that pool or change past evidence.

Run the full offline suite, then the same 18,495-source/129-case matrix with
three timed repeats, 32 final sources, 2 KiB excerpts and 16/64 KiB caps.
Include dictionary extraction, compression, sorting and sizing inside lab
timing. Count evaluated candidates, dictionary bytes and actual code savings.
Legacy timing excludes final whole-context serialization and cannot support
complete modified-engine or hosted latency claims.

Require a newly complete reference and zero individual losses at either cap
versus history 19/21, documents 19/20, 86 declarations and nine huge-tool checks.
Audit every original range, quote, budget, latest version and both clocks.
Keep stronger original SQLite controls. Only after development passes run
independent quality and full-engine latency gates. No independent corpus or
actual agent outcomes are used to rescue a development failure.

## Result

**Rejected.** The implemented candidate passes 219 offline tests with zero
failures and one physical-GPU test ignored. The three new tests cover exact
padded UTF-8 recovery, dictionary and independent branches, explicit location
costs, gamma lengths, stable ordering, pool preservation, address bypass,
superseded sources and future/late-known invariance. An initial test wrongly
assumed a fixed 256-bit location charge must exceed every possible dictionary
saving. It was corrected to use the independent code length as a guaranteed
fallback bound before any benchmark data was evaluated; the model did not change.

The frozen matrix checks all 18,495 exact originals and completes 1,548 stable
timed queries. All 516 packets pass the independent quote, range, source-clock,
latest-version and serialized-budget audit. Source archive and case digests
equal the preceding default run. Every one of the 258 SQLite audit packets is
identical to that run, including its candidate order and exact ranges.

| Ranking | History / 16 KiB | History / 64 KiB | Documents / 16 KiB | Documents / 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Existing body default | 19/21 | 19/21 | 19/20 | 19/20 |
| Conditional LZ4 order | 14/21 | 17/21 | 16/20 | 17/20 |

At 16 KiB the candidate loses five prior history references and three document
references, gaining none. At 64 KiB it recovers one old history miss but loses
three other references; document evidence loses two and gains none. One history
question exchanges its two different source references, so question ordinals
alone must not be mistaken for the same retained fact. None of these totals
passes the individual no-loss gate. All 86 declarations and nine huge-tool
checks stay complete on their existing addressed paths.

Across both caps, warm-up and timed repeats, the reranker processes 272 ordinary
query calls (34 distinct questions), 139,264 candidates and 188,258,152 dictionary
bytes. It finds positive net dictionary savings for 119,176 candidates (85.58%)
and for at least one candidate in every ordinary query. The sum of candidate
savings is 8,419,840 bits across those repeated hypothetical query codes. This
does not represent compressed source-storage savings or model-token savings.
The broad availability of shorter query codes does not identify the best
supporting evidence in this experiment.

Wide history/document mean context sizes grow to 59,786.6 / 64,475.7 bytes,
versus 25,380.5 / 46,276.2 for the retained default. Candidate diagnostic median
times are 14.09/15.25 ms for history and 12.06/13.01 ms for documents across
16/64 KiB. Separate matrices, final-context serialization exclusions and the
base-rank receipt boundary prohibit a full modified-engine timing claim.
The failed quality gate removes any reason to adopt its additional query work.

The Rust implementation, tests, manifest/lockfile changes and lab flag are
[archived as one patch](../benches/experiments/conditional-lz4-query-rejected.patch).
It applies cleanly to the post-field-experiment source with `--unidiff-zero`.
The [aggregate report](benchmarks/nine-conditional-lz4-query-rejected-2026-10-19.json)
contains only recursively privacy-checked aggregate measurements. Raw histories,
private questions, source text and individual response audits remain outside Git.

The engine and storage implementation never changed. All experimental lab and
manifest changes are removed from the active build. The full default suite
passes 216 tests, with zero failures and one physical-GPU test ignored; the
release build succeeds. All crate sources and Cargo files exactly match the
starting checkout, and the archived patch applies cleanly. No new restored
performance claim is made.
The preceding default's independently verified evidence benchmark remains the
reference. No independent corpus, blend parameter, alternate dictionary size,
tokenization or larger context is used to rescue this failed candidate.
Actual agent outcomes, repeated mistake avoidance, model-token use and hosted
behavior remain unmeasured, and independent ranking deficits remain unresolved.

## Reproduce

Use a separate clean checkout with fresh paths:

```text
git apply --unidiff-zero benches/experiments/conditional-lz4-query-rejected.patch
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/fresh-lz4 --output results/fresh-lz4.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords --conditional-compression
python scripts/verify_nine_evidence.py --report results/fresh-lz4.json --audits data/private/fresh-lz4 --sqlite data/private/nine-scale-run3/sources.sqlite --cases data/private/nine-combined-cases-v1.json
```
