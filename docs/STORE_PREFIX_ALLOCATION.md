# Store prefix allocation and retained cache accounting

This draft changes production `oojuro-store` readers. It is based on PR88 (`7dbd7b2`); no dependency, disk format, public API, CRC, AEAD or digest protocol is changed. The native RecordLog replay path generally reads complete batches, so the prefix result below is **not a native CLI or hosted RAM result**.

## Source and ownership

- `fdb49ec`: Zstd prefix decoding initially allocates metadata and grows to the requested prefix. Previously a short returned prefix retained capacity for the entire raw frame. Cache admission now charges the actual body capacity, all five metadata-vector capacities and the `Decoded` value.
- `d492438`: one expression preserves a single initial full-size allocation for explicit `need=None` full decodes, while prefix requests keep the metadata-first allocation. Target resizing and validation order are unchanged.
- Store record/range reads and RunReader early-row reads can use prefixes. Explicit Store hot-load/training/scans and RunReader scans use full decodes; compaction has a separate unchanged reader.
- The budget excludes Arc control/allocator overhead, cache containers, decoder/dictionary state and frames retained by callers. It is not a bound on process RSS. Useful cached frames remain; this does not clear the cache on every read.

## Qualification

The first runtime passed 13 focused checks, including four new multi-frame/prefix/full-read/cache-pressure/corruption regressions. The successor passed exactly three existing focused checks covering prefix expansion plus full scan, Run originals/ranges/scans, and encrypted hot/cold/reopen. All raw counts and artifacts are archived. Unchanged broad storage/crash suites were not repeated.

Both measurement arms use fresh optimized Store artifacts with no features, actual Rust1.99.1, portable x86-64 compiler flags and the same helper (`67424be4`). The first candidate was refused when Cargo reused the baseline normal library. Raw fingerprint/source-mtime evidence is retained; a distinct package-only clean produced fresh artifacts. The successor also used a package-only clean. No baseline artifact was rebuilt for the second campaign.

## Two preserved fixed campaigns

Each campaign uses 128 synthetic 128 KiB originals in 32 four-row frames and a 1 MiB cache. Store uses strictly sealed Zstd3; RunReader uses plaintext Zstd3; dictionary/dedupe settings are unchanged. Four alternating paired rounds run four separate modes per arm: Store prefix/early-row, Store full scan, RunReader prefix/early-row and RunReader full scan. A control-prepared closed template is copied byte-exact to both arms within the campaign. Logical fields, input digests and settings are identical across campaigns; AEAD nonces/ciphertext can differ.

All reported values are medians of four samples. Phase times include hashing. Both campaigns compare their candidate with PR88; the second does **not** isolate the causal effect of the final expression.

| Campaign/case | Stage RSS before → after (KiB) | Stage time before → after (ms) |
|---|---:|---:|
| First: fdb49, Store early row | 9,362 → 5,944 | 2.892 → 1.770 |
| First: fdb49, Run early row | 8,512 → 5,524 | 3.233 → 1.894 |
| First: fdb49, Store full scan | 6,270 → 6,292 | 5.573 → 5.984 |
| First: fdb49, Run full scan | 5,688 → 5,718 | 8.591 → 8.831 |
| Successor: d492, Store early row | 9,320 → 5,908 | 2.989 → 1.536 |
| Successor: d492, Run early row | 8,546 → 5,440 | 3.054 → 1.899 |
| Successor: d492, Store full scan | 6,200 → 6,262 | 5.809 → 5.528 |
| Successor: d492, Run full scan | 5,674 → 5,680 | 8.466 → 8.551 |

The early-row phase uses less RSS in all four pairs of both campaigns. After the short-prefix stage, retained body capacity falls from 16,781,092 to 7,752 B for Store and from 16,778,240 to 3,072 B for RunReader. That capacity was largely reserved virtual memory: short-prefix RSS is nearly neutral. It is not a resident-memory reduction of that byte count.

Whole-probe peak RSS stays around 18.4–18.6 MiB. It includes open and postmeasurement verification; the data establish no meaningful whole-probe peak reduction. Phase current RSS, cumulative HWM, whole-probe peak and helper-plus-probe cgroup peak remain separate categories in the raw reports.

Full reads have mixed outcomes. The first campaign worsens both median full-scan times. The successor improves the Store median but still has two adverse Store pairs and three adverse RunReader pairs; RunReader median is about 1% slower. This is neither a general full-scan speed win nor a strict no-regression proof.

### Every full-scan pair

| Campaign/round/order | Store before → after (ms) | RunReader before → after (ms) |
|---|---:|---:|
| First/0/before/after | 5.633 → 5.933 | 8.580 → 9.718 |
| First/1/after/before | 5.514 → 6.034 | 8.597 → 8.303 |
| First/2/before/after | 5.856 → 5.387 | 8.586 → 8.960 |
| First/3/after/before | 5.374 → 6.285 | 9.333 → 8.703 |
| Successor/0/before/after | 5.915 → 6.085 | 8.094 → 8.464 |
| Successor/1/after/before | 5.863 → 5.523 | 8.531 → 9.625 |
| Successor/2/before/after | 5.388 → 5.534 | 8.401 → 8.638 |
| Successor/3/after/before | 5.755 → 5.494 | 8.783 → 8.416 |

## Verification and limits

Each measured process verifies all 128 originals after timed work and every Store record field. Original and phase digests match across arms. Each campaign has 33 complete template/copy before-and-after inventories; all current files, modes and pathname blocks match. The first campaign independently proves 72 observed process identities and 34 actual/named cgroups absent; the successor proves 70 identities and 34 cgroups absent. Closed fixtures are retained outside Git.

The helper reads/checks every compressed frame before timing, and Store opens verify hot frames. This is warm, fits-RAM, highly compressible synthetic API evidence with four samples per runtime/mode. It is not OS-cold, larger-than-RAM, tail-latency, native RecordLog CLI, hosted, cost or general-performance evidence. No retune, repeated workload or favorable fallback was used.

## Reproducible safe evidence

See [archive inventory and hashes](benchmarks/store-prefix-allocation-2026-10-24/manifest.json) and the [archive README](benchmarks/store-prefix-allocation-2026-10-24/README.md). The single standard tar.gz retains all 64 raw reports, complete inventory metadata, build/gate/resource proofs, common helper/controllers and the refused Cargo reuse attempt. Source and published digests identify path redactions. No corpus, stores, ciphertext fixture payload, binaries, real keys, credentials or private hosted source are included.

Runtime source remains exactly `d49243820a00424c5ff45042a25d38f6c3753bb5`; documentation commits do not relabel a newly built artifact. This draft authorizes no main merge, release or deployment.
