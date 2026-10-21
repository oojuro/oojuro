# Exact shared context with complete provenance

Protocol fixed on 2026-10-19 before implementation/evaluation. Source base
`94c7fda`. Three ranking alternatives failed the existing evidence gate. The
next experiment changes context representation and admission, not relevance
scores or original storage.

The current saved packets contain measurable exact text repetition. At 16 KiB,
Nine history averages 22.5 snippets with 3.643 duplicate texts; at 64 KiB it
averages 32 snippets with 4.857 duplicates. The public document questions average
0.8 and 2.6 duplicates. These are retrospective diagnostic counts on the reused
129-question fixture, not independent generalization evidence. No private text
was printed. Approximate overlap is not enough to merge evidence.

## Lossless representation

Share an exact text block with a primary source and optional `also_from`
origins. Every origin preserves ID, key, both clocks, verification flag and exact
original byte range. Alias records carry an empty text field and the admitted
source ordinal; decoding restores the shared text and the complete original
source order. Sharing requires byte-for-byte equal UTF-8 text, never a similarity
threshold or semantic guess. Nothing changes in the raw archive or eligibility.

When adding an equal-text source, compare the actual incremental serialized
cost of sharing its provenance with the cost of another flat snippet:

```text
Delta_flat  = bytes(JSON(snippet)) + array_separator
Delta_share = bytes(JSON(alias_header)) + measured_alias_field_separators
choose the smaller feasible addition under the packet byte limit
```

This is a local measured-description-length choice, not an optimal global MDL
algorithm. No alias field is serialized when unused, so ordinary flat packets
retain their previous JSON shape. A normalization, embedding model, generated
summary or probability of truth is not involved. Header/ordinal overhead is
counted in full. The builder must avoid reserializing a growing alias list on
each addition; update sizes from exact fragment lengths instead.
Very short repeated strings may stay flat when their alias overhead costs more.
`also_from[].at` is a zero-based source-order position, not a timestamp. Both
clocks remain explicit fields in each origin.
Both wire and reconstructed flat JSON are bounded by the engine's 32 MiB frame
limit. The decoder computes the expanded cost before cloning shared text into
every origin. This preserves the existing frame bound even for highly repeated
input; ordinary 2 KiB excerpts remain far below it.

## Controls and gates

First encode the existing selected packets without changing their sources or
ranges. Decoding must reproduce every original evidence object and its order;
all supporting-quote metrics must therefore remain identical. Measure bytes,
not estimated LLM tokens. Preserve unequal bytes, differing timestamps/keys,
Unicode ranges, source order, provenance and strict byte limits. Reject invalid
alias ordinals or nonempty alias text during decoding.

A separate admission experiment may spend saved bytes on more references from
the same ranked frontier. Its cap is explicitly **32 distinct text blocks**, not
32 source origins. Report both numbers and total serialized bytes. Apply the
same shared representation/admission policy to Nine and SQLite; retain the
prior flat-source-count control. Source-rank metrics remain unchanged and must
not be described as improved ranking. Verify every decoded range/quote against
the original source. Never count an alias as new semantic evidence merely
because its source ID differs.

Use the same 512-reference frontier, 2 KiB maximum excerpt, 16/64 KiB packet
caps, clocks and literal query policy. Excerpts that do not fit may use the
existing smaller-range fallback; every such selection still needs exact-byte
checks. Stop at the first new block after the block cap. Full request latency
must include candidate scans, extraction, sharing, serialization and synced
receipts. Keep any pure codec benefit separate from admission-policy failures.

The codec must be exercised through the Rust library and remain optional for
callers expecting the existing flat interface. Run the complete offline suite.
If development admission passes, test raw-only tools and reused public relevance
data before using the still-uninspected ArguAna questions. Actual model-token
and agent-answer benefits need their own measurement; a smaller JSON packet
does not prove either. No result cache or provider calls are used here.

## Result: retain the optional codec

All 516 existing context packets were re-encoded without changing any selected
source, original range, metadata or source order. Three measured repeats after
warm-up produced 1,548 stable round trips. The source archive matched the frozen
report before and after replay. An independent standard-library Python decoder
checked every restored body/range/key/clock/verification flag against the
separate SQLite originals. Selection and all prior supporting-quote metrics
remain identical. Labels never enter the codec.

| Existing Nine context | Flat mean bytes | Shared mean bytes | Bytes saved |
| --- | ---: | ---: | ---: |
| History, 16 KiB cap | 16,345.64 | 14,935.21 | 8.63% |
| History, 64 KiB cap | 25,380.50 | 22,956.79 | 9.55% |
| Documents, 16 KiB cap | 16,293.20 | 15,795.30 | 3.06% |
| Documents, 64 KiB cap | 46,276.20 | 43,972.15 | 4.98% |
| Synthetic huge-tool contexts, both caps | 10,057.33 | 8,492.67 | 15.56% |

History encoding medians were about 0.08/0.13 ms at the two caps; separate
decode/equality-verification medians were 0.13/0.22 ms. These are **codec-only**
costs, excluding replay and retrieval, not submillisecond database latency.
SQLite's existing history packets also shrink 6.54%/6.81%, and documents
2.66%/5.03%. Its huge-tool packets have no worthwhile exact sharing. This
representation is useful for either backend; it is not a uniquely better ranker.

`ContextPacket` and `decode_context` are optional Rust-library interfaces.
Existing find/reference/flat APIs keep their behavior. Passing decoded flat
evidence to an LLM restores the original payload; possible prompt-token savings
require a client/agent that understands shared JSON. Actual tokenizer counts
and agent-answer accuracy remain unmeasured.

## Admission: keep the stronger comparison control

The separate `--shared-context` benchmark spends saved bytes under **32 distinct
text blocks**, rather than 32 source origins, with the same 16/64 KiB caps. The
policy applies identically to both engines and stays opt-in in the lab.

| Complete reference pairs | Nine flat | Nine shared | SQLite flat | SQLite shared |
| --- | ---: | ---: | ---: | ---: |
| History, 16 KiB | 19/21 | 19/21 | 18/21 | 19/21 |
| History, 64 KiB | 19/21 | 19/21 | 20/21 | 20/21 |
| Documents, 16 KiB | 19/20 | 19/20 | 18/20 | 18/20 |
| Documents, 64 KiB | 19/20 | 19/20 | 19/20 | 19/20 |

Both engines retain 86/86 declarations and 9/9 synthetic tool facts. Nine's
history misses remain at ranks 109/39 and its document miss at 342. Candidate
rankings are unchanged. Its wide-budget mean origins grow from 32 to 37.43
(history) and 39.90 (documents), with 32 distinct texts. Mean bytes rise to
26,213/47,631 without more complete answers. Aliases and extra origins are not
extra independently verified semantic facts.

Same-run Nine history median/p95 latency is 19.46/43.93 ms at 16 KiB and
20.37/32.60 ms at 64 KiB. SQLite's is 141.78/202.30 and 180.90/369.46 ms.
Scans, full/smaller-range attempts, sharing, serialization and decode validation
are included. These are separate runs from the previous flat matrix, so do not
attribute a pure causal percentage to the timing differences. More work without
a Nine evidence gain does not justify a default selection change. The option
is retained because it improves SQLite's tight-budget control and should be
available for fair future comparisons.

The shared and post-change flat matrices total 3,096 timed queries and 36,990
per-run original cross-checks, with stable repeats and zero full-body SQL
fallbacks. Independent range/metadata/visibility auditing checked 516 shared
packets, including both clocks and latest-version eligibility. The flat replay
checked another 516 and reproduced the earlier full audits exactly for both
engines and caps. These are repeated controls over 129 reused questions.

## Verification and limits

The complete offline all-features suite passes **216 tests, zero failed, one
physical-GPU test ignored**. New checks cover interleaved source order, Unicode,
different provenance/verification flags, byte boundaries, invalid aliases and
bounded expansion. The paired fixture excludes future and superseded records
even when their text is identical. No new storage index or dependency is added.

The engine/reader/codec plus the entire shared core measures 1,902 code lines
under the existing scope. The codec is 182 lines; its 153-line test file is
separate. Context sharing does not change the raw archive or retrieval model.

SciFact/FiQA ranking gaps remain; no public gate is rerun to claim a ranking gain.
ArguAna remains unused. Actual repeated agent mistakes, answer accuracy, model
tokens, hosted/concurrent behavior and read/write throughput still need separate
experiments. Overall superiority is not established by the byte savings.

A later diagnostic found two raw query words in the missed public document's
stored path metadata that are absent from its body. Opaque source keys add no
new raw query words across the 20 document cases. Searching textual provenance
is a grounded next experiment; stemming equivalence and uniform treatment of
both backends still need verification. No metadata-index or question-specific
rule is introduced here.

## Reproduction and aggregates

- [Fixed-evidence round trips](benchmarks/nine-context-repack-2026-10-19.json)
- [Shared-admission control](benchmarks/nine-shared-admission-control-2026-10-19.json)
- [Post-change flat compatibility](benchmarks/nine-context-flat-control-2026-10-19.json)

```text
cargo test --offline --workspace --all-features
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-context-repack --source-run data/private/nine-scale-run3 --audit-run data/private/nine-scoring-restored1 --audit-report results/nine-scoring-restored1.json --cases data/private/nine-combined-cases-v1.json --run-dir data/private/new-repack --output results/new-repack.json --repeats 3
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --cases data/private/nine-combined-cases-v1.json --run-dir data/private/new-shared --output results/new-shared.json --repeats 3 --counts 32 --budgets 16384,65536 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords --shared-context
```

Use fresh paths. Omit `--shared-context` for the original flat/source-count
control; add `--sqlite-porter` for the alternate tokenizer. Shared context can
also be selected explicitly in `nine-beir-bench`; that option has not consumed
a new public gate here. Private encoded packets remain outside Git; only
aggregates are published.
