# Lossless projection RAM comparison — 23 October 2026

**Recommend compact posting rows only.** On the frozen larger workload, median
peak process RSS fell 93.229 → 84.005 MB (9.9%). The separate owned-text move did
not reduce full-pipeline peak further and is not part of the shipping candidate.
These are local API measurements, not hosted AWS capacity or a total-RAM cap.

## Versions and exact work

- Baseline engine `6cec2064aba3c4a6e96ebf587d0d1cad651e266a`.
- Compact postings `7d81635c2e19`: private in-memory `(usize,u32)` posting handles
  become checked `(u32,u32)`; existing 100k-row limits remain. Lookups widen to
  usize; persisted codec already uses u32 and its bytes/authority are unchanged.
- Compact plus owned-text move `6610d0d56678`: separate experimental ownership
  change; borrowed callers retain independent original text. Not recommended as
  an additional RAM improvement based on these results.
- Ignored allocation diagnostic `8c4835b`; first-stage freeze `605c414`.

Same copied ArguAna 8,674-record/8,984,758-byte corpus and first 15 frozen dev
queries/top100 plus original get, batch100; three fresh processes/stores per
regime. Second suite: two fixed SWE-agent dev trajectories, 1,000 background
records, 44 steps/run, top20+originals, identical thinking/metadata/clocks. The
existing release harness is reused, MemoryMax=3G, no child swap, shared lock,
warm OS cache, local Ryzen 8845HS/ext4; host counters/order are archived. No
reduced topK, reduced returned content, new score model or weaker ACK requirement.

45 full-hit samples per regime match exactly across ranked IDs, score bits,
continuation, clocks, metadata, UTF-8 excerpt offsets/bytes and originals.
BEIR nDCG@10 is 0.5036781582 and returned originals 1,036,320 bytes/run for all
three. History nDCG@10 is 0.7477066262 and returned bytes 828,931 for all three.
All 18 final stores reopen in fresh processes with every original field exact
(BEIR versus frozen source, histories consistent across regimes); 99 files and
36 apparent/allocated footprint values independently match the scorecards.
Standalone seven correctness/crash gates pass in three runs for the first two
regimes, and separately for the owned-move regime. Meaningful bitwise oracle,
current-view, projection/record and cache hydration tests pass.

## Final three-way results

Medians of three per-run values. MB is decimal; RSS is process VmHWM, not total
cgroup/page-cache/allocator usage. Latency p50 is a median of per-run p50s over
15 BEIR queries or 44 history steps. These are small shared-workstation samples.

| ArguAna regime | Peak RSS MB | Search+originals p50 ms | Sample p99 ms | Allocated full store MB |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 93.229 | 58.823 | 434.236 | 3.539 |
| Compact postings | 84.005 | 58.061 | 409.164 | 3.539 |
| Compact + owned move | 84.267 | 60.747 | 441.946 | 3.539 |

| History regime | Peak RSS MB | Search+originals p50 ms | Allocated store MB |
| --- | ---: | ---: | ---: |
| Baseline | 18.436 | 5.274 | 0.242 |
| Compact postings | 17.736 | 6.028 | 0.242 |
| Compact + owned move | 17.805 | 5.636 | 0.242 |

The compact result reproduces the separately frozen first-stage RSS reduction
93.360 → 84.439 MB. Latency varies: compact is slightly faster on the larger
corpus but slower on this small history run. No universal speedup is claimed.
Sample p99 over 15 queries is effectively a per-run maximum, not a production
tail estimate. The owned move removes a clone at construction, but no additional
full-pipeline RSS improvement was observed; it must not be described as a RAM win.

## What the RAM consists of

The ignored lexical-only diagnostic measures retained vector/string capacities:

| Component | Baseline bytes | Compact bytes |
| --- | ---: | ---: |
| 886,115 postings, including Vec spare capacity | 20,846,592 | 10,423,296 |
| Exact original text capacity | 8,984,758 | 8,984,758 |
| Document array | 3,538,944 | 3,538,944 |
| Term string capacity | ~159,223 | ~159,239 |

The posting tuple is 16 bytes with usize padding on this 64-bit host versus eight
with u32. Checked insertion/hydration prevents narrowing overflow. These counts
exclude BTree nodes, IDs/row metadata, allocator overhead and RecordLog/view state;
they are not a complete heap/RSS partition. Isolated lexical diagnostic peak RSS
was about 51,004 → 42,764 KiB while holding a separate caller corpus.

The benchmark retains RecordLog originals and imported metadata, a configured
4 MiB decode cache, projection originals/postings/metadata, and temporary fresh
view/materialization/query buffers. In a separate copied-store reopen diagnostic,
RSS was 47.690 MB before projection search and about 91.939 MB after query15;
after dropping RecordLog/projection it still read about 90.006 MB. This shows why
RSS cannot be equated to live projection data: allocator retention and process
mappings remain. A separate installed glibc allocation interceptor observed an
83.368 MB heap peak; it was not used for latency. These separate phase probes
must not be summed into an invented exact partition of the fresh-build benchmark.

Remaining peak includes mandatory fresh row materialization alongside the retained
index, RecordLog state, query/codec work and retained allocator capacity. The
owned move targets cold construction but does not remove those costs. Further
large reductions likely require separately qualified disk-backed/bounded derived
index work, with exact originals and current authority preserved. No further
broad profiling or architecture change is included here.

## Evidence and reproducibility

[First compact stage](benchmarks/projection-ram-2026-10-23/compact-first/provenance.json)
was frozen before the ownership experiment. [Final matrix and source/binary hashes](benchmarks/projection-ram-2026-10-23/final/provenance.json)
preserve each regime, raw scorecards, exact-hit digests, source/test logs,
independent reopens and file hashes. Original executables/copied stores and input
spec remain under `work/projection-ram` and `work/local-efficiency-20261023a`.
Exact orchestrator/reopen tools are archived with the final evidence; they call
the existing harness and consume those frozen fixtures, not a new scoring arm.
No AWS/production operation, public package, main merge or runtime activation.
