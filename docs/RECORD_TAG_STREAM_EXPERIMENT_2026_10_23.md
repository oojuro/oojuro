# Bounded keyed JSON streaming experiment, 23 October 2026

**Unbuffered candidate is experimental only, not recommended as a default.**
It improves the large-batch diagnostic but slows small and heavily escaped
inputs. No full search/ingest pipeline qualification was pursued after that
concrete rejection signal. No hosted, capacity, storage, pricing or SOTA claim.

Baseline is signed PR65 `c42c10d32364d692bccc97030c447dc05ceb85b0`.
Candidate runtime is `dc0e9a593ee19009126a6626a94049ffca1d9359`. It generalizes
the existing bounded JSON writer over an `io::Write` sink: `Vec<u8>` for encoding,
`io::sink()` for counting, the existing BLAKE3 `Hasher` for keyed intent tags.
The complete plaintext tag Vec and its volatile clearing loop are removed.
Hash implementation internal state remains; this is not a zero-copy or secret
zeroization guarantee. No dependency, unsafe block, cache, format or durability
behavior is added. Request/domain/key/serialized bytes, size checks and error
mapping remain the same. Independent tests compare streamed hashes against the
prior one-shot keyed hash over exact JSON, including escaping and bound edges.

## Matched component diagnostic

Same local Ryzen 7 8845HS Linux/ext4 machine; portable x86-64 release test
binaries, CPU affinity 2, 3 GiB cgroup/no swap, fresh process/store per sample
group, fixed synthetic clock and public fixture key. Existing `BeforeStore`,
`AfterStore` and `BeforeAck` hooks separate preparation, Store sync and replay.
The driver reuses the benchmark lock, alternates AB/BA/AB, has a 120 s workload
deadline, 30 s per process and exact owned-scope stop on timeout. All 18 processes
completed in 20.58 s while other agents held builds/timed fixtures.

Three fresh processes per arm/case. Values below are medians of process p50s,
not pooled samples or qualified tails. Small: 64 commits of eight records with
at most 1 KiB UTF-8 text each. Large: six commits of 1,000 records with at most
16 KiB text each. Escaped: four commits of one 4 MiB NUL record, expanding to
over 24 MiB in JSON. All include unchanged validation, compression/encryption,
durable acknowledgment and replay. Source inputs are stored-only; this is not
a search/ranking workload. Data is generated, never real user history.

| Case | Prepare ms baseline → candidate | Replay ms baseline → candidate | Durable total ms baseline → candidate | Append peak RSS MB baseline → candidate |
| --- | ---: | ---: | ---: | ---: |
| Small | 0.0498 → 0.0555 | 0.0556 → 0.0593 | 1.1530 → 1.1620 | 7.422 → 7.578 |
| Large | 46.934 → 33.550 | 38.904 → 35.290 | 104.964 → 87.721 | 231.928 → 228.536 |
| Escaped | 108.739 → 125.238 | 100.628 → 115.896 | 240.686 → 270.480 | 144.429 → 144.515 |

Streaming reduces the large sample's total median by about 16.4%, while escaped
inputs grow about 12.4%. Small samples also worsen slightly. Many tiny serializer
writes are a concrete risk of unbuffered hashing; these results do not prove a
single exclusive cause. Peak RSS is measured before verification/reopen; those
later phases are reported separately in raw rows. Large append RSS falls only
about 1.5%, while escaped/small RSS offers no improvement. The remaining live
canonical/encoded/parsed records and Store buffers are not a measured heap
partition. No write-throughput or p99 claim is justified by six/four commits.

Every assigned input is compared in memory and after a fully fresh reopen.
Cross-source complete assigned-record signatures, including fixed native IDs
and clocks, match in all 18 processes. 101 record tests pass, five existing
ignored; scoped formatting, release diagnostic builds and diff checks pass.
No AWS activity or production activation occurred. Rejected cache-clear/top-k,
mask and validation-budget experiments are excluded from this source.

[Frozen provenance](benchmarks/record-tag-stream-2026-10-23/provenance.json)
retains all raw per-commit phase samples, peak RSS values, signatures, test/build
logs, source/binary hashes and the exact synthetic diagnostic/driver. Only hashes
and generated benchmark summaries are archived, not raw transcripts/datasets.
