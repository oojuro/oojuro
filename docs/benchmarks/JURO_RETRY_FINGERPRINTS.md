# Bounded retry fingerprints

A bounded optional retry cache replaces the fingerprint stored in every
transaction's replay metadata. The source format, transaction headers/digests,
exact originals, recovery and durable confirmation rules are unchanged.
Final runtime: `b74cc3a2509583e13cd6a6c7719df725eca02045`; matched baseline:
`ad0085ef80b53a0af448f3071d4a10c520b8dabd`.
[Complete receipts](JURO_RETRY_FINGERPRINTS_2026_10_25.json) preserve all 66
preliminary, 56 final and 16 larger point-read observations, including adverse
cases. Every observation passes its exact-byte/checkpoint assertions.

The cache holds at most 1,024 canonical fingerprints, enough for a maximum
Event batch. It is allocated only on a write or retry; ordinary reopened
read-only handles allocate none. Eviction/reopen computes the original
fingerprint from the complete verified transaction body before comparing the
incoming request. This retains whole-frame checksum, encryption/context and
exact operation-order/empty/delete validation. A cache hit never removes the
normal file/directory confirmation fence or permits a poisoned handle to ACK.

The frozen benchmarks use distinct source-built binary hashes, identical
harnesses, one CPU, retained OS caches and synthetic files. Replay uses actual
Event source opens and exact original cursor/time/data reads. Raw sources use
NOCOMPRESS; Zstd sources are separate actual compressed archives. Plain replay
covers one million events and encrypted replay covers 100k. Three independent
process observations are retained for each case. Point/retry fixtures use
2,049 ordinary KV transactions with 64-byte or 8 KiB values.

| Actual Event source | Baseline peak RSS KiB | Candidate peak RSS KiB |
| --- | ---: | ---: |
| Plain 1M, raw | 521,652–521,928 | 485,932–486,028 |
| Plain 1M, Zstd archive | 522,024–522,044 | 486,156–486,332 |
| Encrypted 100k, raw | 43,740–43,784 | 39,652–39,972 |
| Encrypted 100k, Zstd archive | 43,804–43,852 | 39,988–40,128 |

The observed saving is about 35 MiB at 1M and about 3.7–4.0 MiB at 100k. Other
source metadata remains population-sized. These are process observations, not
hard RAM limits or hosted acceptance. Replay time is similar: about 804–842 ms
candidate versus 826–880 ms baseline for the plain 1M raw source. Encrypted
100k replay remains about 201–218 ms. No general replay latency gain is claimed.

A bounded cache intentionally trades historical retry I/O for retained memory.
The retry test confirms the source once, warms the stated working set, then
measures three cyclic passes. Input fingerprints are still computed on every
call, and every result returns its original sequence without changing source
bytes or the latest checkpoint.

| Retry workload | Baseline p95 µs | Candidate p95 µs |
| --- | ---: | ---: |
| Plain raw, 8 KiB, 1,024 identities | 2.365 | 2.388 |
| Encrypted raw, 8 KiB, 1,024 identities | 2.426 | 2.611 |
| Plain raw, 8 KiB, 2,049 identities | 2.471 | 8.398 |
| Plain Zstd, 8 KiB, 2,049 identities | 2.605 | 6.715 |
| Encrypted raw, 8 KiB, 2,049 identities | 2.617 | 14.413 |
| Encrypted Zstd, 8 KiB, 2,049 identities | 2.655 | 8.381 |

The larger cyclic working set exceeds the cache and repeatedly reads verified
frames. Those adverse tails are real observations; this is a memory reduction,
not a universal retry speedup. The full receipts also retain the small-value
and preliminary cases, including a 64-byte encrypted archived retry tail.

Ordinary exact point reads do not use this cache. The larger-value follow-up
retains all cases: raw 8 KiB p95 is 2.500→2.556 µs plain and 8.549→8.560 µs
encrypted. Zstd 8 KiB p95 is adverse at 1.629→2.015 µs plain and 3.074→3.467 µs
encrypted. These short retained loops do not identify a cause or establish a
repeatable codec regression; no point-read improvement is claimed. Tiny replay
reads and their process-to-process variation remain in the receipt as well.

Correctness gates pass with and without archive support: frozen independent
wire/Event vectors, corrupt/torn data, locks, retries, checked-open refusal,
batch semantics, recovery and exact originals. New tests exceed the cache policy,
revisit old/new batches without reapplying versions, distinguish reordered
operations and empty/delete changes, and corrupt an evicted frame before retry.
The real Linux fsync fault runner passes ten file/directory/alias/group cases,
plain and encrypted. Strict default/archive Clippy and changed-file formatting
checks pass. No dependency or source-format change is introduced.
