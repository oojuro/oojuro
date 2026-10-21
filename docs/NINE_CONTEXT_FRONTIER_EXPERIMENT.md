# How much evidence does a larger return count recover?

Protocol fixed on 2026-10-19 before this experiment. This is a budget control,
not a new ranking algorithm. Use the retained default after the separately
declared query-information experiment; a failed candidate must be restored first.

## Question and fixed controls

The question is whether increasing what reaches the agent can improve accuracy.
Separate the number of candidate references returned from the serialized context
budget. Keep the existing 18,495 originals and 129 cases, two clocks, 512-reference
retrieval frontier, same 2 KiB excerpt reader and identifier policy. Give Nine
and SQLite identical limits. Originals and linked passages are unchanged.

Run a full matrix of returned-source caps 32, 128 and 512 against 64 KiB and
1 MiB serialized context caps. Three timed repeats follow warm-up; alternate
engine order and disable result caches. Include SQLite's existing identifiers
and shared query-stopword control. Its native-snippet reader remains enabled.
Repeat the matrix using SQLite's existing full-body/shared-Rust reader as a
stronger excerpt control. The earlier balanced crossover established that this
reader was faster than native SQL snippets, and its clipping behavior differs.
Require identical Nine evidence across both runs and identical SQL candidate
orders across readers. This control is specified before the first result.

Compare individual reference retention and complete questions at every setting,
not only counts. Audit all ranges, source clocks, latest-version eligibility,
reference-label eligibility, serialized sizes and stable repeated packets.
Compare complete references with their possible coverage within the existing
512 frontier. A reference absent from that frontier cannot be rescued by a
larger return count. A source present without its supporting quote is a miss.

## Interpretations decided in advance

- A larger context can recover evidence at a higher byte cost. That is a
  measured resource tradeoff, not an algorithmic accuracy improvement.
- If 512/1 MiB retains all development references, it establishes only a
  ceiling for these reused fixtures. It does not establish better generated
  answers, fewer repeated mistakes, semantic retrieval or independent accuracy.
- Legacy requery timing includes selection, individual snippet serialization
  and synchronized query receipts. It excludes final whole-context serialization.
  Use the balanced full-response crossover for the existing final latency claim;
  this run's timing is diagnostic only.
- No LLM, embedding service, actual model-token measurement, remote hosting,
  concurrency or universal SQL read/write speed claim is part of this control.
- The 150 MB stored-text corpus and 8 MiB raw tools are finite in-memory pilot
  sizes. Index residency and out-of-RAM scaling remain separate questions.

Do not change defaults merely to return more bytes. Preserve the smaller
explicit budget, let callers request expansion, and report the measured quality
and byte frontier. The independent ArguAna gate is not used for this diagnostic.

## Nine result

The restored fixed-128 ranker is confirmed by the run metadata. The first matrix
cross-checks 18,495 exact sources, completes 4,644 stable timed queries and passes
1,548 independent packet audits. Every one of the 86 declaration checks and
nine huge-tool checks remains complete at every setting.

| Source cap | Context cap | Complete history references | Mean history bytes | Complete document references | Mean document bytes |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 32 | 64 KiB | 19/21 | 25,381 | 19/20 | 46,276 |
| 32 | 1 MiB | 19/21 | 25,381 | 19/20 | 46,276 |
| 128 | 64 KiB | 20/21 | 65,494 | 19/20 | 65,487 |
| 128 | 1 MiB | 21/21 | 103,600 | 19/20 | 147,984 |
| 512 | 64 KiB | 20/21 | 65,494 | 19/20 | 65,487 |
| 512 | 1 MiB | 21/21 | 576,843 | 20/20 | 592,227 |

Increasing bytes alone leaves the 32-source result unchanged. Increasing the
source cap alone while retaining 64 KiB recovers one history reference but not
the other history reference or the missing document. Both limits matter.
The original missing sources are already in the 512 frontier: the two history
misses are at ranks 109 and 39, and the document miss is at rank 342. Expansion
recovers evidence already ranked there; it does not improve those ranks.

The smallest measured fully complete history setting uses about 4.1 times the
mean bytes of the 32-source control. Fully complete documents use about 12.8
times its mean bytes. A mean is not a sufficient per-query budget: these results
used the explicit caps in the table, not a budget set equal to that mean.

All 21 history pairs, all 28 history quotes and all 14 history questions are
complete at 128/1 MiB. At 512/1 MiB every labeled reference, quote and question
in all 129 development cases is retained. This is development evidence coverage,
not 100% generated-answer accuracy or independent semantic generalization.

The unchanged nine huge-tool checks need only 10,057 mean serialized bytes even
at the largest setting. The raw 8 MiB input/output records still need linked
passages and bounded original excerpts. Expanding the context is unnecessary
for these particular checks. Byte compression on disk does not itself reduce
the number of expanded text tokens supplied to an answer model.

## SQL reader controls

The native-snippet SQL control also reaches every development reference at
512/1 MiB. Its mean history/document contexts are 1,048,158 / 1,045,951 bytes.
At 128/1 MiB it also reaches 21/21 history and 19/20 documents, using
298,076 / 301,826 mean bytes. This is a matched-coverage byte comparison on
these fixed fixtures, not an actual tokenizer or generated-answer comparison.

The full-body/shared-Rust reader also reaches 21/21 history at 128/1 MiB and all
development references at 512/1 MiB. All six native audit sets are identical
across the two matrices, and every SQL candidate order is identical across the
two readers. Original and case digests are unchanged. The 32/64 KiB controls
exactly reproduce both engines' previous individual evidence audits.

| Same quality and limits | Nine mean bytes | SQL native snippets | SQL shared-Rust reader |
| --- | ---: | ---: | ---: |
| History, 21/21, 128 sources / 1 MiB | 103,600 | 298,076 | 297,972 |
| Documents, 20/20, 512 sources / 1 MiB | 592,227 | 1,045,951 | 1,045,862 |

The two expansion matrices complete 9,288 stable timed queries, cross-check
36,990 originals and pass 3,096 independent packet audits. Including the rejected
query-feature experiment, this milestone has 10,836 stable timed queries, 55,485
original cross-checks and 3,612 audited packets. No provider calls are made.

SQL's shared-Rust reader is materially faster in this control too: at the
512/1 MiB setting, its history/document medians are 61.77 / 52.48 ms, versus
379.53 / 226.98 ms for native SQL snippets in the other matrix. These separate
matrices are diagnostic; they exclude final whole-context serialization and do
not balance both readers in the same process. Use the existing balanced
[full-response crossover](NINE_READER_CROSSOVER.md) for its 32-source final
latency measurements, rather than treating this as a controlled causal speedup.

The fixed ranker and defaults are retained. The result supports explicit
expansion at a measured byte cost, while preserving the smaller initial return.
It does not establish that the entropy of a routing distribution is a calibrated
answer-confidence score, or that a larger packet makes an agent use evidence
correctly. Independent semantic ranking deficits and actual agent-task outcomes
remain unresolved; the superiority goal remains active.

[Native-snippet matrix](benchmarks/nine-context-frontier-native-2026-10-19.json).
[Shared-Rust-reader matrix](benchmarks/nine-context-frontier-rust-2026-10-19.json).

## Reproduce

Use the retained default and fresh output paths:

```text
cargo build --offline --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/fresh-frontier --output results/fresh-frontier.json --cases data/private/nine-combined-cases-v1.json --repeats 3 --counts 32,128,512 --budgets 65536,1048576 --sqlite-identifiers --excerpt-bytes 2048 --sqlite-native-snippet --rust-declarations --shared-stopwords
python scripts/verify_nine_evidence.py --report results/fresh-frontier.json --audits data/private/fresh-frontier --sqlite data/private/nine-scale-run3/sources.sqlite --cases data/private/nine-combined-cases-v1.json
```

For the stronger full-body/shared-Rust SQL reader, omit
`--sqlite-native-snippet` and choose another fresh run/output path. Run the
independent auditor against that report and its corresponding private audits.
Keep raw histories and full audit packets outside Git.
