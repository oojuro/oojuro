# Canonical-buffer lifetime experiment, 23 October 2026

**Experimental only; rejected as a default recommendation in this campaign.**
Reported peak RSS falls for synthetic large writes, while normal search medians
and large append totals are worse in both measured orders. This is
the direct local `RecordLog` path, not hosted S3 capture, and it does not establish
hosted capacity, instance savings, storage pricing or SOTA.

## Source

Baseline is signed PR65 `c42c10d32364d692bccc97030c447dc05ceb85b0` (runtime
`22a1767`), not deployed compact-only source. The baseline binary's measured
`59ac5a9` crates/manifests are byte-identical to this signed source. Candidate
runtime `d204189450f4c2fc86cbdf77266aa895734b2d3f` changes exactly two lines in
`record/authority.rs`: drop each canonical JSON Vec after `chain()` has returned
its owned digest, before later append/replay allocations. Existing useful caches,
buffers, hash operations and validation remain. No dependency, format, authority,
clock, receipt, error, encryption, fsync or acknowledgment behavior changes.
Unbuffered streaming, validation-counter, mask, top-k, replay-cache clearing and
owned-text experiments are excluded.

## Synthetic durable append component

Same Ryzen 7 8845HS Linux/ext4 host, portable x86-64 release binaries, CPU2,
fresh process/store, 3 GiB cgroup/no swap. Existing phase hooks measure preparation,
Store sync, replay and complete caller-observed append including cleanup. Same
diagnostic SHA and baseline binary as the separately rejected tag-stream test.
The driver alternates AB/BA/AB, holds the benchmark lock, bounds workload to
120 s with 30 s per process and exact owned-scope stop on timeout. Other agents'
compilers/timed fixtures were held. Eighteen processes completed in 21.39 s.

Small: 64 commits of eight stored-only records, at most 1 KiB UTF-8 text each.
Large: six commits of 1,000 stored-only records, at most 16 KiB text each.
Escaped: four commits of one 4 MiB NUL record, over 24 MiB serialized JSON per
record. Values are medians of three process p50s; six/four commits do not qualify
a tail. Exact per-commit data and all fields after fresh reopen are retained.

| Case | Durable total ms baseline → candidate | Reported append peak RSS MB baseline → candidate |
| --- | ---: | ---: |
| Small | 1.22377 → 1.22325 | 7.778 → 7.811 |
| Large | 106.458 → 112.257 | 232.595 → 215.982 |
| Escaped | 244.999 → 246.828 | 144.912 → 119.509 |

Large and escaped peak observations fall about 7.1% and 17.5%; all three process
ranges are disjoint. Small memory is unchanged. Large/escaped latency is worse
in this first comparison; no speed claim or default recommendation follows from
the RAM reduction. Phase medians do not sum to total medians; shifting frees also
shifts phase boundaries. Kernel `VmHWM` is approximate, not an exact live-heap or
capacity measure. Three large candidate runs report later high-water snapshots
0.07–0.18 MB lower; raw values are retained without forcing monotonicity. The
16.6/25.4 MB observed gaps do not establish a memory cap.

## Normal full native pipeline

Existing shared harness, frozen public inputs, same machine/portable builds,
three fresh processes/stores per arm, 3 GiB/no swap and warm OS cache. ArguAna:
8,674 records, 8,984,758 text bytes, 15 frozen dev queries, top100 plus originals,
durable batch100. History: two frozen SWE-agent trajectories, 1,000 background
records, 44 steps/process, top20 plus originals, 1,136 final records. Data/query
text is not committed. Input spec SHA-256:
`7b0209a64ee89529c5dea7eb5b2817a5817ed9c7eab4b5ccc10b7bc4990df6ec`.

| First comparison metric | Baseline | Candidate |
| --- | ---: | ---: |
| BEIR search+originals p50 ms | 60.557 | 63.146 |
| BEIR sample p99 ms (15/process) | 451.373 | 421.607 |
| BEIR peak RSS MB | 82.063 | 82.223 |
| History search+originals p50 ms | 5.560 | 5.893 |
| History sample search p99 ms (44/process) | 23.567 | 23.796 |
| History durable append service p50 ms | 1.269 | 1.255 |
| History sample append p99 ms (44/process) | 1.661 | 1.983 |

Normal pipeline peak RSS offers no reduction. First BEIR p99 includes cold
projection construction; it does not represent a qualified warmed-query tail.
No cross-campaign absolute comparison is used. All first results, including
regressions, were frozen before any order follow-up.

One final reversed-order pipeline and large-only component comparison inspected
those concrete latency/order risks. No source change, favorable subset selection
or additional optional timing followed.

| Reversed comparison metric | Baseline | Candidate |
| --- | ---: | ---: |
| BEIR search+originals p50 ms | 60.881 | 64.219 |
| BEIR sample p99 ms (15/process) | 425.332 | 431.161 |
| History search+originals p50 ms | 5.520 | 5.860 |
| History sample search p99 ms (44/process) | 24.375 | 22.908 |
| History append service p50 ms | 1.212 | 1.293 |
| History sample append p99 ms (44/process) | 1.516 | 1.686 |
| Large component durable total p50 ms (6 commits/process) | 104.297 | 108.672 |
| Large component reported append peak RSS MB | 232.124 | 215.712 |

Large peak RSS repeats about 7.1% lower, but total median append is about 4.2%
slower (first order about 5.4%). Normal BEIR search medians are 4.3–5.5% worse;
history search is about 6% worse in both orders. These observations do not prove
one exclusive cause, particularly where the query code is unchanged, but do not
support accepting the change. Small-sample tails are mixed. Useful caches and
buffers are preserved; a lower large-write RSS alone does not meet the balanced
read/write/search/storage requirement or the repository's performance gate.
The branch/evidence is retained for later analysis, not deployment.

Complete allocated stores span 3,534,848–3,543,040 bytes in the first BEIR stage
and exactly 3,538,944 in reverse; history spans 241,664–245,760. All payloads,
metadata and indexes count. Genuine clocks/encryption/compression/allocation
rounding vary; no storage-format change or compression advantage is claimed.

## Correctness and provenance

- 100 record tests pass; five existing ignored. Existing independent ranking,
  two-clock, retry, metadata, forge/recovery and crash tests cover the executed
  path. No implementation-mirroring test of two `drop` statements was added.
- All seven native gates pass in both arms across three runs, including
  18 SIGKILL trials. A failed/uncertain append remains subject to existing poison
  and recovery rules.
- 45 full-hit signatures match across sources: IDs, score bits, continuation,
  clocks, metadata, UTF-8 excerpts and exact original text. All 18 component
  processes also match complete assigned inputs/IDs/clocks after fresh reopen.
- 12 fresh pipeline reopens pass, every field exact; 66 files and 24 full-store
  apparent/allocated sums match. ArguAna nDCG@10 is `0.5036781581683427`, history
  `0.7477066261975289`; returned original bytes are 1,036,320 / 828,931 per run.
- Portable release builds, scoped formatting and diff checks pass. No AWS
  operation, production activation or competitor speed claim occurred.

[First component provenance](benchmarks/record-buffer-lifetime-2026-10-23/component/provenance.json)
was frozen at `dde9f6a` before full pipeline measurement.
[First pipeline provenance](benchmarks/record-buffer-lifetime-2026-10-23/pipeline/provenance.json)
was frozen at `b6911d0` before an order follow-up. Both include raw observations,
source/binary hashes, diagnostic/driver source, gates and exactness logs. Large
histories above RAM, concurrency, attachments, SQLite/Postgres/JSONL+Zstd and
hosted behavior require separate matched work; no ratio is extrapolated here.
The complete [reversed evidence](benchmarks/record-buffer-lifetime-2026-10-23/reverse/provenance.json)
includes both pipeline cards and all six large-only component processes.
