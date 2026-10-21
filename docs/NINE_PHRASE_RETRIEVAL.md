# Literal retrieval and the SQLite native-snippet control

Experiment date: 2026-10-19. This follows the
[original-range experiment](NINE_PASSAGE_EXPERIMENTS.md). The engine remains
model free. Original records, clocks and archive formats are unchanged.

## Query meaning before more output

The previous excerpt experiment retained the correct declaration source more
often than it retained the required declaration text. A caller looking for a
definition can explicitly ask for a literal phrase:

```rust,ignore
let hits = db.find("\"fn example_function\"", from, to, 32)?;
let part = db.passage(&hits.matches[0].id, "\"fn example_function\"", 2048)?;
```

A whole nonempty double-quoted query means a case-sensitive, verbatim phrase.
Alphanumeric and underscore edges require word boundaries. A call to a function
and the declaration of a longer name are not matches for its `fn NAME` phrase.
The source must also satisfy both historical cutoffs and latest-version rules.
Positive usefulness feedback cannot bypass the literal constraint. Missing or
oversized phrases produce no excerpt.

For a phrase containing indexed words, the shortest term posting list supplies
candidates. Every valid match must contain every indexed query word, so this is
a complete generator for this mode. Exact source checks then reject partial
words, different case, different bytes and ineligible versions. Background
counts are recomputed across all query terms in the qualified view; a future
record can change the chosen posting list without changing historical scores.
Punctuation-only phrases scan the eligible timeline. Neither path is a new
semantic encoder or a constant-time positional index.

The byte-range API also adds `expand_range(text, start, end, max_bytes)`, which
expands an already known UTF-8 source range without rerunning lexical retrieval.
Original bytes and exclusive end offsets are checked before a packet is emitted.

## An explicit caller policy shared by both engines

The lab's optional `--rust-declarations` policy recognizes just two question
templates: `Find the implementation of the Rust function NAME` and
`Locate the Rust declaration of NAME`. With a valid ASCII identifier it issues
`"fn NAME"` to **both** engines. It inspects only question text, never gold
source IDs, quotes or the expected answer. Other questions are unchanged.

This is a caller expressing an exact search intent. It is not an automatic
natural-language feature shipped in the database. Its results must remain
separate from the unchanged-question control. It tests declaration lookup,
not arbitrary code understanding, semantic QA or an agent completing a task.

## Stronger SQLite excerpt baseline

The SQLite arm retains its word FTS5 and whole-identifier/RRF retrieval. Quoted
declaration queries use the underscore-aware phrase index. Its native
[`snippet()` function](https://sqlite.org/fts5.html#the_snippet_function) supplies
a fragment of up to 64 tokens. Empty markers preserve raw source bytes. The
fragment is located in the original and expanded to the same 2,048-byte ceiling
using `expand_range`; when no usable fragment is available the shared lexical
extractor is used. Every returned range must exactly equal its original bytes.

A Rust scalar function performs expansion **inside SQLite**, borrowing the
original text and returning only the bounded serialized evidence. The
[`rusqlite` function API](https://docs.rs/rusqlite/0.40.2/rusqlite/functions/index.html)
is enabled only in the experiment lab. Nine has no SQLite dependency. An initial
control copied the whole body across the SQLite interface before expanding it;
that cost is removed in the final control. The regression test verifies identical
evidence between the reference adapter and the scalar-function path.

The two engines may choose different excerpts. The SQLite seed uses native FTS5
token positions, while Nine's ordinary reader scans source features. Ranking,
historical filtering, exact-source access, extraction, JSON packet serialization
and synced query receipts remain inside the measured interval. Source reopen and
whole-corpus verification are outside it. Both engines are warm and run in one
process; Nine's decoded originals/index are memory resident. No result cache,
model or network call is used.

## Source and question controls

The main corpus remains 18,495 records and 149,793,732 text bytes. Its existing
129 questions include the reused redacted engineering history, public documents,
86 mechanical declaration probes, and nine synthetic large-tool facts. Earlier
declaration sets are now development data. Controls use candidate depth 512,
k=32, 16/64 KiB serialized evidence caps, up to 2,048 original bytes per excerpt,
and three timed repeats after warm-up. Per-question packets remain private.

A new [disjoint 40-declaration fixture](../benches/fixtures/nine-retrieval-reserved-v2.json)
uses the next names in the existing deterministic SHA-256 ordering, excluding
all previous probe names. Its questions and reference quotes were frozen before
its first run. It was first exercised with the earlier phrase edge handling;
the final word-boundary correction was driven by an adversarial unit test rather
than tuning names to their outcomes. This remains the same mechanical question
family, not an independently authored semantic holdout.

The separate raw-only store has four roughly 8 MiB originals totaling
33,554,632 bytes, with no pre-split passage copies. Its nine synthetic facts
include old/new version cutoffs. The raw-only control uses k=2 and a 16 KiB cap.

Complete reference-pair retention requires **every reference quote for a source
and question** to be present in returned text. A correct source ID with clipped
evidence is incomplete. These are retrieval/evidence metrics; generated-answer
accuracy and actual model input tokens remain unmeasured.

## Unchanged-question results

Final native-snippet/SQL-expansion control, k=32 and 64 KiB total evidence:

| Workload | Nine complete pairs | SQLite complete pairs | Nine p50 ms | SQLite p50 ms | Nine mean evidence bytes | SQLite mean evidence bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Historical evidence | 19/21 | 19/21 | 13.982 | 275.663 | 24,101 | 65,498 |
| Additional documents | 18/20 | 18/20 | 10.120 | 125.247 | 41,943 | 65,510 |
| Additional declarations | 30/40 | 32/40 | 0.793 | 126.004 | 8,378 | 65,492 |
| Previously reserved declarations | 30/40 | 24/40 | 0.966 | 144.095 | 10,953 | 65,468 |
| Old declarations | 5/6 | 5/6 | 0.832 | 88.014 | 12,593 | 65,457 |
| Large-tool facts with passage copies also present | 9/9 | 9/9 | 64.615 | 219.177 | 10,057 | 65,432 |

At 16 KiB, Nine retains 19/21 historical pairs versus SQLite's 18/21, but only
16/20 document pairs versus SQLite's 18/20. The larger budget helps some groups
and does not resolve every declaration or document miss. These unchanged
questions still prevent a general superiority claim.

Sequential desktop runs showed substantial absolute latency variation even for
unchanged Nine code and identical packets. This table uses one final paired
run, not a subtraction across different runs to attribute speed changes. Earlier
large speed ratios from the full-source-copy/shared-extractor baseline must not
be generalized to the routed declaration workflow.

## Explicit declaration queries

With the shared caller policy, k=32 and a 16 KiB cap:

| Declaration set | Nine complete pairs | SQLite complete pairs | Nine p50 ms | SQLite p50 ms | Nine mean evidence bytes | SQLite mean evidence bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Old six | 6/6 | 6/6 | 0.488 | 1.121 | 2,434 | 2,431 |
| Additional forty | 40/40 | 40/40 | 0.307 | 0.847 | 2,369 | 2,368 |
| Previously reserved forty | 40/40 | 40/40 | 0.348 | 0.904 | 2,416 | 2,417 |
| New disjoint forty | 40/40 | 40/40 | 0.533 | 1.339 | 2,402 | 2,401 |

All 126 declarations retain their reference quotes in both engines, also at the
64 KiB cap. The gain over unchanged declaration questions benefits both engines.
Nine is faster in these paired runs; evidence volume is essentially equal.
The old general-word speed gap does not describe this more selective SQLite
path. Different rows come from the two explicitly separate query sets.

## Huge originals without passage copies

The raw-only control retains **9/9 complete references in both engines**.
Nine's median is **57.724 ms**, with **2,501 mean evidence bytes**; SQLite's
median is **89.528 ms**, with **4,165 mean evidence bytes**. These are complete
local queries including extraction and serialization, not tiny reference-only
lookups. Every output range is an exact slice of its original. Model calls are
zero.

The native archive remains 11,940,960 bytes for 33,554,632 original text bytes,
including the roughly 8 MiB opaque record that does not compress. Individual
structured records and their revision compress as documented in the
[original scale report](NINE_SCALE_BENCHMARK.md#huge-raw-inputoutput-storage).
Storage compression and the smaller returned context are separate mechanisms:
the former is lossless LZ4/delta encoding, the latter selects original ranges.

## Shorter-context control: rejected as a general default

The remaining document gap motivated one additional control: 512-byte excerpts,
the same shared declaration policy, k=32 and a 16 KiB total cap. No engine code
or question changed.

| Workload | Nine complete pairs, 2 KiB / 512 B | SQLite complete pairs, 2 KiB / 512 B |
| --- | ---: | ---: |
| Historical evidence | 19 / 19 of 21 | 18 / 17 of 21 |
| Additional documents | 16 / 15 of 20 | 18 / 14 of 20 |
| Existing declarations | 86 / 86 of 86 | 86 / 86 of 86 |
| Large-tool facts with passage copies also present | 9 / 9 of 9 | 9 / 9 of 9 |

For declaration-signature lookup the smaller cap retains all required quotes
with roughly 790 mean evidence bytes instead of roughly 2,400. That narrow
result does not establish that a 512-byte function excerpt suffices to execute
or modify its implementation. The document control loses evidence in both
engines, despite fitting more source snippets. The general comparison retains
the 2 KiB control rather than declaring success by selecting the shorter cap
where the competitor loses more.

## Verification, limits and next gate

The four final 2 KiB matrices contain **3,630 timed queries** and **55,489
exact-record cross-checks**. All 14 per-arm audit files contain identical
candidate IDs, retained IDs, ranges and serialized sizes to the corresponding
pre-optimization native-snippet adapter. The additional shorter-context matrix
adds 774 timed queries and 18,495 checks, for **4,404 timings and 73,984 original
checks**. Every matrix has zero changed repeat packets, zero SQL full-body
fallbacks and an unchanged original archive digest. The linked aggregates
report the measured SQLite runtime, **3.53.2**.

The all-features workspace passed **203 tests, zero failures, one GPU test
ignored**. New checks cover overlapping phrase occurrences, identifier prefix
collisions, Unicode ranges, cache identity, cutoffs, feedback restrictions,
historical score invariance and SQLite expansion parity. During development,
substring matching initially accepted a longer function-name prefix; the
word-boundary rule and regression test corrected it before the final runs.

These results support a concrete improvement in expressing and serving exact
declaration queries. They do not prove generic semantic superiority, better
generated answers, actual model-token savings, avoidance of repeated agent
mistakes, hosted latency, concurrency or behavior outside RAM. Nine's unchanged
document questions still have an accuracy deficit at the tighter budget.
The next quality gate needs new independently authored natural-language tasks
and agent outcomes, with exact supporting evidence checked under a fixed cap.
The research goal remains active.

### Aggregates

- [Unchanged questions, SQLite native snippet and SQL expansion](benchmarks/nine-native-snippet-final-2026-10-19.json)
- [Shared declaration query policy](benchmarks/nine-rust-query-final-2026-10-19.json)
- [Disjoint forty-name check](benchmarks/nine-declarations-disjoint-2026-10-19.json)
- [Raw originals without passage copies](benchmarks/nine-raw-native-snippet-final-2026-10-19.json)
- [Rejected 512-byte general excerpt control](benchmarks/nine-short-excerpts-2026-10-19.json)

## Reproduction

```text
cargo test --workspace --all-features
cargo build --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/new-native --output results/new-native.json --cases data/private/nine-combined-cases-v1.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet
```

Repeat with fresh paths and `--rust-declarations` for the explicit caller policy.
For the disjoint check, also replace `--cases` with
`benches/fixtures/nine-retrieval-reserved-v2.json`. For the short-range control,
use the combined cases, that policy, `--excerpt-bytes 512` and `--budgets 16384`.

```text
oojuro-lab nine-scale-requery --source-run data/private/nine-raw-excerpts2048-3 --run-dir data/private/new-raw-native --output results/new-raw-native.json --repeats 3 --counts 2 --budgets 16384 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet
```

Full reproduction requires the approved private source stores and reused
historical cases. Raw originals, private questions and per-question audits are
not committed. No provider credentials are required. Fresh report/audit paths
are enforced by the command; appending search receipts does not change originals.
