# Rejected score mixture and exact-source excerpts

Experiment date: 2026-10-19. This continues the
[sparse-scoring experiment](NINE_RETRIEVAL_IMPROVEMENTS.md). The default retrieval
path keeps its previous ranking model. A proposed score mixture was
tested and rejected. The library adds an optional exact-range reader; whole
source reads and search references remain available.

The subsequent [literal-query and native-snippet control](NINE_PHRASE_RETRIEVAL.md)
supplies the stronger SQLite positional-fragment comparison. It also separates
unchanged word queries from a shared caller policy for declaration lookup.

## Score mixture: mathematically valid, empirically unhelpful

The experiment combined the existing smoothed frequency model with a source
term-presence model over the same historically eligible candidates C. If p(w)
is the fraction of C containing w, its presence-model code gain was:

```text
sB(d) = sum(w in q, count(w,q) * log2((1[w in d] + p(w)) / (2 * p(w))))
pF(d) = 2^sF(d) / sum(j in C, 2^sF(j))
pB(d) = 2^sB(d) / sum(j in C, 2^sB(j))
pM(d) = 0.5 * pF(d) + 0.5 * pB(d)
```

Stable log-space evaluation and dense-equation tests passed. The conditional
candidate-address code satisfies the ordinary mixture inequality:

```text
-log2(pM(d)) <= min(-log2(pF(d)), -log2(pB(d))) + 1 bit
```

That bound concerns code length, not relevance or factual correctness. Fixed
equal weights gave no coverage improvement on the additional public questions,
the reserved symbol set or the historical pilot. At k=128 and 1 MiB, historical
retention stayed 20/21, while Nine's median rose from 11.2 ms to 14.9 ms and mean
context grew from 131,497 to 994,890 bytes. The mixture was reverted.

- [Additional-query aggregate](benchmarks/nine-mixture-public-2026-10-19.json)
- [Reserved-query aggregate](benchmarks/nine-mixture-reserved-2026-10-19.json)
- [Historical-query aggregate](benchmarks/nine-mixture-history-2026-10-19.json)
- [Archived experiment patch](../benches/experiments/frequency-presence-mixture.patch)

The patch records the rejected change against the earlier frequency-based
implementation; it is not enabled in the delivered engine. A successful
normalization test was insufficient reason to retain a slower, larger result.

## Exact source ranges

```rust,ignore
let references = db.find("garnet resolved_epoch", from, to, 50)?;
let part = db.passage(&references.matches[0].id,
                      "garnet resolved_epoch", 2048)?.unwrap();
assert_eq!(part.text, &db.read(&references.matches[0].id).unwrap()[part.start..part.end]);
```

`passage` returns a borrowed original substring with a start byte offset and an
exclusive end byte offset. `extract_passage` exposes the same deterministic
reader for supplied text. It uses no model, summary, network call or generated
text. Unicode boundaries and byte caps are enforced. An empty budget returns no
range; no lexical overlap produces a prefix with no claim of useful evidence.

For source feature count n, query-feature count K, source frequency r(w), and
query count c(w), the reader assigns local surprisal weights:

```text
I(w) = c(w) * log2((n + K + 1) / (r(w) + 1))
coverage(W) = sum(w represented by a match in W, I(w))
```

The denominator corresponds to query features plus an OTHER category, each with
one pseudocount. Ordinary words and underscore names/components form the source
feature stream. A sliding window selects high weighted coverage under a byte
cap, preferring shorter observed match spans on ties, then adds bounded nearby
context. This is a lexical excerpt heuristic, not a semantic sufficiency proof
or a global MDL solution.

The current reader scans the selected original at expansion time. It has O(source
bytes + matching feature occurrences) work and stores matching occurrences
temporarily. It is not a positional-index lookup. Large raw inputs can therefore
cost far more to expand than a small indexed source reference costs to find.
Lowercase ASCII features are borrowed during scanning to avoid per-word token
vector allocations. Original and archive bytes are unchanged.

## Fairness and evidence checks

Both Nine and SQLite receive the same excerpt reader. SQLite's stronger word and
identifier/RRF indexes are retained. Three source sets supply 129 questions:
the original 29, 60 added public development questions and the 40 symbol probes
that are now also inspected development data. Each engine uses 512 candidate
references, 32 returned snippets, 16/64 KiB evidence caps, and three timed repeats
after warm-up. Complete timings include exact-source loading, extraction, JSON
serialization and the existing synced search receipt.

Excerpt packets include `source_range: [start, end]`; the measured byte budget
includes those offsets. Whole-source packets omit the optional field. Native
reads borrow the already decoded original, while SQLite loads its selected text.
Neither comparison establishes cold-buffer-pool or hosted behavior. This SQLite
comparison does not use its native snippet/highlight functions; a positional
snippet baseline is still needed before a general excerpt-performance claim.

The metric now distinguishes source IDs from retained evidence. A source pair
counts as complete only if **every reference quote for that source and question**
appears in the returned text. Raw quote counts and question completion are also
reported. A regression test proves that a correct ID with one required quote
missing is not scored as complete. Every emitted range is checked against the
original before serialization.

## Completed excerpt results

The table uses **complete reference pairs**, requiring every supporting quote
for each source/question pair. Controls: k=32, 64 KiB total evidence, up to
2,048 original bytes per excerpt. Both engines use the identical extractor.

| Workload | Nine complete pairs | SQLite complete pairs | Nine p50 ms | SQLite p50 ms | Nine mean evidence bytes | SQLite mean evidence bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Historical evidence | 19/21 | 19/21 | 5.175 | 60.424 | 24,101 | 65,510 |
| Additional documents | 18/20 | 18/20 | 4.163 | 26.995 | 41,943 | 65,497 |
| Additional declarations | 30/40 | 30/40 | 0.266 | 29.638 | 8,378 | 65,461 |
| Previously reserved declarations | 30/40 | 30/40 | 0.317 | 31.112 | 10,953 | 65,456 |
| Old declarations | 5/6 | 6/6 | 0.366 | 24.573 | 12,593 | 65,401 |
| Large-tool facts with passage copies also present | 9/9 | 9/9 | 26.707 | 63.150 | 10,057 | 65,431 |

This is not a universal quality improvement. The additional-declaration arm
contains the right source IDs for 38/40 queries but only 30/40 required quotes.
The earlier whole-source control retained 37/40 quotes for Nine and 38/40 for
SQLite. A correct source link alone would therefore overstate excerpt quality.
Nine's document coverage improves from its earlier whole-source 17/20 to 18/20,
but a 4,096-byte excerpt control falls back to 17/20: the wider snippets displace
other evidence under the global cap. The larger-range control improves additional
declaration quotes to 33/40 and reserved declaration quotes to 32/40 for Nine;
it still fails to retain the earlier whole-source declaration coverage.

The reader remains optional. Agents must expand an original when an excerpt
does not supply enough evidence. Neither the new reader nor the rejected score
mixture resolves every existing candidate/ranking failure.

### Large raw originals without pre-split passage copies

A fresh store contains exactly four originals: structured input, old output,
revised output and an opaque result. Each is roughly 8 MiB; total raw text is
33,554,632 bytes. There are no derived passage records. The nine synthetic fact
queries include historical revision cutoffs. Both engines recover all originals
exactly and use the same on-demand extractor.

At k=2 and a 16 KiB total cap, Nine retains **9/9 complete references**, returning
2,501 mean evidence bytes at 25.895 ms median. SQLite also retains 9/9, returning
4,168 bytes at 70.796 ms median. This final comparison enables SQLite's stronger
identifier/RRF channel and uses three timed repeats. Every output includes its
exact original range. The source archive independently reopened and validated
all four originals; it measures 11,940,960 bytes, excluding the RAM index.
The earlier whole-record experiment retained none within a small cap and needed
about 16 MB of returned evidence for complete coverage at the huge cap. These are
separate runs; the measured byte feasibility improvement does not isolate one
codec or establish a general latency distribution from nine synthetic questions.

The raw original is scanned to choose its excerpt. That work dominates these
queries; their complete latency is **not** the sub-millisecond reference lookup
seen on small literal matches. No embedding or summarization latency is hidden.

### Aggregates

- [Initial unoptimized excerpt adapter](benchmarks/nine-excerpts-initial-2026-10-19.json)
- [Optimized 2 KiB corpus excerpts](benchmarks/nine-excerpts-2048-2026-10-19.json)
- [4 KiB corpus excerpts](benchmarks/nine-excerpts-4096-2026-10-19.json)
- [Raw-original-only excerpts](benchmarks/nine-raw-excerpts-2026-10-19.json)
- [Raw-original excerpts with stronger SQLite retrieval](benchmarks/nine-raw-excerpts-strong-2026-10-19.json)

## Development corrections

The initial excerpt adapter could load bodies after too little packet space
remained. It now applies a serialized minimum-header bound before loading. The
first scanner also allocated several intermediate token vectors per word; it now
borrows ordinary lowercase ASCII text. Both engines receive these changes.
The first completed corpus aggregate is retained as a diagnostic. Its initial
raw-only continuation left no completed aggregate and no live process; that
incomplete result is excluded and a fresh raw-only run is used.

The all-features workspace passed **200 tests, zero failures, one GPU test
ignored**. Excerpt tests cover distant facts, exact Unicode ranges, identifiers,
empty/small budgets and insufficient evidence despite a correct source ID.
The two completed optimized corpus matrices have 3,096 timed queries and 36,990
exact-record cross-checks. The raw-only matrix has 216 timed queries and eight
exact-record checks; its stronger-baseline requery adds 54 timings and four
cross-checks. All completed matrices have stable repeat packets. The
initial corpus adapter and the three rejected-mixture reports remain diagnostic
evidence, not additional independent accuracy questions.

Generated-answer accuracy, actual model-token counts, repeated agent tasks,
concurrency, out-of-RAM operation and hosted service behavior are unmeasured.
Reference coverage is not answer accuracy. Truncation is evidence selection;
lossless archive compression remains the separate LZ4/delta mechanism. The source
can always be expanded deliberately by ID and original byte range.

## Reproduction

```text
cargo test --workspace --all-features
cargo build --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/new-excerpts --output results/new-excerpts.json --cases data/private/nine-combined-cases-v1.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048
oojuro-lab nine-scale-bench --dataset data/private/pilot-benchmark-v1 --public-root ../../work/upstream --run-dir data/private/new-raw-excerpts --output results/new-raw-excerpts.json --repeats 3 --raw-only --excerpt-bytes 2048
oojuro-lab nine-scale-requery --source-run data/private/new-raw-excerpts --run-dir data/private/new-raw-strong --output results/new-raw-strong.json --repeats 3 --counts 2 --budgets 16384 --sqlite-identifiers --excerpt-bytes 2048
```

The combined cases concatenate the three earlier case files, preserving their
questions and evidence. Full reproduction requires the approved private sources;
those questions, source texts and per-question audits are not committed. Fresh
run/report paths are mandatory. Repeat with 4096 excerpt bytes for the larger
range control. No provider credentials or model calls are needed.
