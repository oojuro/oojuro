# Projection validation reuse experiment, 23 October 2026

**Experimental, no default or production pin recommendation.** Search medians
improved in both orders, but durable append sample tails worsened. The storage
and append implementation is unchanged; this does not explain away that result.
No hosted, memory-capacity, write-throughput, pricing or SOTA claim is made.

## Source and change

Baseline is signed PR65 `c42c10d32364d692bccc97030c447dc05ceb85b0`
(runtime `22a1767`), not the deployed compact-only source. Its crates/manifests
are byte-identical to the frozen measured `59ac5a9` binary source. Candidate
runtime is `bfd4d8bdabce1c06eec46074889d012f76cdc068`. No mask, top-k experiment,
replay-cache clearing, owned-text move, new dependency or format change is included.

The projection retains one validated serialized-byte total, a `usize` with no
heap allocation. Full incoming writer/name/count/text-length/ID/key uniqueness
checks still run on every refresh. Every retained row must match every metadata
field and the exact fresh original text before its previous serialized-size
budget can be reused. Every added row is JSON-counted with actual escaping;
per-row 32 MiB and total 128 MiB bounds still apply. Replacements/deletions fully
rebuild and recalculate the budget. Same-view mutations fail closed. Errors
consume the old projection. Current authority, body reads, keys, clocks, excerpts,
originals and caller response checks are unchanged.

Structural validation now precedes size counting. An input violating multiple
constraints can therefore report duplicate `Conflict` before size `Bounds`;
both are rejected. No persisted field or accepted-data contract changes.
The separate retained-memory accounting API can charge the counter through its
existing `size_of(Index)` inline allowance when sources are integrated.

| File | SHA-256 |
| --- | --- |
| `crates/oojuro-engine/src/record/projection.rs` | `ca13fafacd654bf2ba11dd37b58cd5a797e8219ee57453b486ab96bb99e358e0` |
| `crates/oojuro-engine/src/record/projection_tests.rs` | `38cfd010bf39b83006595f82853889cf4b079732b3baec9703bd7f04f5a4a4e0` |

## Matched local measurement

Same Ryzen 7 8845HS Linux/ext4 machine, portable generic x86-64 release binaries,
two build jobs, warm OS cache, separate fresh stores/processes, 3 GiB cgroup,
no swap. Other agents' builds and timed fixtures were held during each matrix.
The existing shared harness and frozen public inputs were reused; raw data and
query text are not committed. Input spec SHA-256 is
`7b0209a64ee89529c5dea7eb5b2817a5817ed9c7eab4b5ccc10b7bc4990df6ec`.

ArguAna: all 8,674 records / 8,984,758 source text bytes, first 15 frozen dev
queries, top 100 plus exact originals, durable batch 100. History: two frozen
SWE-agent dev trajectories, 1,000 background records, 44 steps/process, top 20
plus exact originals, 1,136 final records. Each arm has three fresh processes
per stage; numbers below are medians of the three process statistics. BEIR's
15-sample p99 includes the first cold projection build; it is not a qualified
tail of warmed queries. Fresh reopen checks do not flush the OS cache.

| Metric | First baseline | First candidate | Reverse baseline | Reverse candidate |
| --- | ---: | ---: | ---: | ---: |
| BEIR search+originals p50 ms | 55.490 | 51.908 | 56.750 | 53.073 |
| BEIR sample p99 ms (15/process) | 407.329 | 408.093 | 413.307 | 413.083 |
| BEIR peak RSS MB | 82.047 | 81.949 | 82.178 | 82.010 |
| History search+originals p50 ms | 5.772 | 5.212 | 5.466 | 4.839 |
| History sample search p99 ms (44/process) | 22.478 | 21.863 | 23.980 | 23.523 |
| History append service p50 ms | 6.249 | 6.815 | 1.238 | 1.269 |
| History sample append p99 ms (44/process) | 11.383 | 12.160 | 1.613 | 4.605 |

BEIR median observations are approximately 6.5% lower in each order; history
search medians are 9.7–11.5% lower. These are local workload observations, not a
universal speed claim. Peak RSS changes are too small for a RAM win claim.
The first matrix's durable append/build regime was materially slower than the
reverse and prior campaigns. Both complete regimes are retained; no cause is
assigned and no favorable subset substitutes for the first result. One reversed
comparison did not resolve the append-tail concern, so this candidate stays
experimental pending a separately justified write/tail qualification.

Complete allocated stores are 3,538,944 bytes for every BEIR run and
241,664–245,760 bytes for history. Apparent bytes, metadata, payloads and indexes
are all counted in the raw cards. Timestamps/encryption/compression and allocation
rounding vary across real stores; there is no storage-format improvement claim.
Existing SQLite/JSONL+Zstd comparisons are not rerun or extrapolated from this
native-only A/B. Large/above-RAM histories, concurrency and production remain
outside this measurement.

## Correctness and provenance

- 101 record tests pass; five existing ignored. The new functional test uses
  escaped NUL text to cross real JSON per-row/cumulative bounds after reuse,
  replacement and deletion; raw source text alone would fit.
- All seven native gates pass in both arms across three fresh processes,
  including 18 total SIGKILL trials, exact originals, determinism, freshness,
  two-clock visibility and ordered ranges.
- 45 cross-source full-hit signatures match: IDs, score bits, continuation,
  clocks, metadata, UTF-8 excerpt offsets/content; originals match exactly.
- 12 fresh all-field reopens pass; 66 files and 24 apparent/allocated footprint
  sums match. ArguAna nDCG@10 is `0.5036781581683427`, history
  `0.7477066261975289`; returned originals are 1,036,320 / 828,931 bytes per run.
- Portable release benchmark/profile build and scoped rustfmt check pass.
  Workspace-wide formatting check reports existing unrelated formatting diffs;
  no broad reformat was applied. `git diff --check` passes.

Frozen [first provenance](benchmarks/projection-validated-refresh-2026-10-23/first/provenance.json)
includes code/binary/input hashes, test/build logs, cards, full-hit signatures,
reopen inventories and driver source. It was committed as `d43b4aa` before the
follow-up. [Reverse provenance](benchmarks/projection-validated-refresh-2026-10-23/reverse/provenance.json)
retains the complete reversed cards and host snapshots. Profile timing fields
used to produce exact signatures are diagnostic and are not acceptance timings.
