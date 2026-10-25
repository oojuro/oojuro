# Minimal .ojo I/O optimization qualification

This change optimizes `crates/ojo` against `origin/main` at
`c892586650dde963ffbc4d228a296fcc0f984b33`. It preserves the public API,
experimental wire 0.1, default raw commits and four default runtime dependencies.
It does not change the retained directory-backed search engines or establish a
search-latency/storage-ratio marketing claim for `.ojo`.

## Implementation

- Raw point reads use one positioned read on Unix instead of seek plus read.
  The selected value's SHA-256 is still verified.
- Sequential open/scan combines body/footer reads and buffers small frames.
  Large frames bypass the buffer to avoid copying large transaction bodies.
- Commits serialize into one frame allocation; encryption/decryption uses the
  existing authenticated cipher in place. A decoded view avoids moving nonce
  prefixes. Associated data, nonces, tags and stored bytes keep the same grammar.
- Encoded value offsets are derived at commit/open. Point reads still verify
  and decode the entire containing transaction, including tombstone reads.
- Open validates all transactions and operation grammar, but defers the extra
  derived retry fingerprint until the first retry of that transaction. This
  improves open while making the first retry slower; subsequent retries cache
  only that fingerprint. It is not a persistent index or a value cache.
- Archive construction syncs the completed private temporary before linking
  its final name. No internal transaction is acknowledged externally. Public
  commits continue to call `sync_all` individually before returning success.

## Method

The primary campaign has 76 runs: six raw workloads with two runs each of
baseline Ojo, candidate Ojo and tuned SQLite, followed by ten Ojo profiles with
two runs per implementation. Raw order is baseline/candidate/SQLite/SQLite/
candidate/baseline; profile order is baseline/candidate/candidate/baseline.
Both implementations use identical benchmark source. Every returned value and
scanned record is checked, along with retry receipts and unchanged checkpoints.

Rust 1.99.1, release defaults and `RUSTFLAGS=-Ctarget-cpu=x86-64`; both examples
built with `bench,archive`. SQLite is the bundled 3.53.2, WAL/FULL, prepared
statements, `INTEGER PRIMARY KEY`, 64 MiB cache. All executable runs are pinned
to CPU 0 on an Intel Core Ultra 7 258V. The host uses its existing powersave
governor; frequency and other host activity are not controlled. No compilation
runs during timings. Build/timing campaigns honor the shared benchmark lock.

Stores use persistent Btrfs, with an inherited `chattr +m` no-compression flag
checked on every database. Normal Ojo writes are uncompressed. Archive profiles
explicitly measure Zstandard archives separately; both original and archive
are retained during measurement. These are synthetic bytes and warm serial
reads, with no network, real agent history or dedicated-device qualification.

The primary tables use the median of two run medians, not pooled latency
percentiles or confidence intervals. Each profile has 1,000 timed gets after
32 warmups, three scans and five reopens. Scan/reopen p95/p99 in the JSON are
descriptive maxima of tiny samples. Commit counts range from one to 1,024;
short cases cannot establish durable tail latency. The raw harness uses 2,000
gets, one scan and five reopens, as in the existing published benchmark.

## Primary before/after profiles

| Workload | Metric | Baseline | Optimized |
| --- | --- | ---: | ---: |
| 1,024 × 128 B, batch 1 | Scan | 1.986 ms | 0.677 ms |
| 32,768 × 1 KiB, batch 64 | Reopen + get | 31.149 ms | 22.084 ms |
| 32,768 × 8 KiB, batch 64 | Reopen + get | 214.537 ms | 149.233 ms |
| 32,768 × 1 KiB, batch 64 | Warm get p50 | 0.900 µs | 0.847 µs |
| Encrypted 4,096 × 1 KiB, batch 64 | Warm get p50 | 50.261 µs | 49.051 µs |
| Archive 4,096 × 1 KiB, batch 64 | Archive creation | 545.632 ms | 42.840 ms |
| Archive 4,096 × 8 KiB, batch 64 | Archive creation | 667.077 ms | 82.531 ms |
| Encrypted archive 4,096 × 1 KiB, batch 64 | Archive creation | 547.054 ms | 66.894 ms |

Small-transaction scans improve about 2.9×; raw reopen improves about 29–30%
in the larger profiles. Archive creation improves about 8–13×. Raw full scans
with large frames change little. Encryption still requires authenticating and
decoding the full containing transaction, so large encrypted batches remain
much more expensive than raw selected-value reads.

## Raw comparison with SQLite

| Records / value / batch | Baseline records/s | Optimized records/s | SQLite records/s | Optimized get p50 µs | SQLite get p50 µs |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1,024 / 128 B / 1 | 108 | 123 | 94 | 0.275 | 3.094 |
| 1,024 / 128 B / 64 | 6,463 | 7,363 | 4,808 | 0.254 | 0.847 |
| 1,024 / 1 KiB / 64 | 6,495 | 7,247 | 4,672 | 0.512 | 0.889 |
| 1,024 / 8 KiB / 64 | 5,621 | 5,505 | 3,993 | 2.868 | 1.889 |
| 32,768 / 1 KiB / 64 | 5,780 | 7,637 | 7,304 | 0.813 | 4.684 |
| 1,024 / 8 KiB / 1,024 | 20,331 | 22,902 | 9,908 | 2.743 | 2.022 |

Ojo keeps persistent retry IDs/history and verifies cryptographic value digests;
SQLite does not implement those extra contracts in this comparison. SQLite's
integer schema is a favorable control for ordinal keys, not an arbitrary-binary
key schema. SQLite still reopens much faster (roughly 0.09–0.30 ms), wins several
scans and wins the 8 KiB point reads above. Single-record writes remain limited
by device sync latency. Durable throughput varies substantially between runs;
these results do not establish a universal ingestion improvement.

Reported Ojo file lengths match baseline in every paired profile. These are
logical file bytes, not allocated filesystem blocks. For example,
32,768 × 1 KiB with batch 64 uses 34,414,656 bytes in either implementation,
versus 44,855,296 bytes for this SQLite layout. Single-record 128 B commits use
303,168 bytes in Ojo versus 155,648 bytes in SQLite. No new compression or
universal space saving is claimed.

## Adverse controls and limits

The first retry after reopening is deliberately slower. With 32,768 × 1 KiB,
batch 64, it increases from 21.7 to 60.9 µs; cached retries remain about 16.6 µs.
With an 8 MiB transaction it increases from 2.44 to 9.91 ms. Encrypted first
retries also pay for authentication/decryption. Applications dominated by
reopening then retrying a single transaction may prefer the previous eager
fingerprint behavior. Full validation at open has not been deferred.

The primary 32,768 × 8 KiB raw profile reported lower write throughput
(6,822 → 4,800 records/s), and encrypted archive get medians were higher
(8.17 → 10.65 µs). These observations are retained in the raw report.
Follow-up controls use four runs per implementation in reversed balanced order,
with 10,000 timed gets, to investigate these cases rather than discard them.
The follow-up raw write medians are 6,242 versus 6,199 records/s (within 1%),
with overlapping run ranges: baseline 6,052–6,470, candidate 6,022–6,777.
The entire candidate process also uses less user CPU (median 2.03 versus
2.47 seconds); this includes reads and reopens, not just writes. Encrypted
archive get medians are 6.193 versus 5.992 µs. Plain archive gets change little
(4.575 versus 4.492 µs). The initial negative observations do not reproduce
consistently, and the report retains both campaigns. No general write-throughput
or large archive-read improvement is claimed.

## Correctness and reproduction

The default and archive test suites cover exact bytes, binary keys, empty values,
tombstones, ordering, locks, incomplete-tail recovery, complete corruption,
ambiguous writes/syncs, persistent retries, wrong keys and frozen independent
wire vectors. New tests check deferred retry conflicts and encoded offsets,
including corruption of a neighbor when reading a tombstone.

`scripts/verify-ojo-sync.py` traces public commits and archive publication using
synthetic data. Four public commits produce four transaction file syncs, plus
the initial header sync. The archive has a header sync and a final data sync
before its hard link. Injecting `EIO` at the final data sync prevents publication,
removes the private temporary and leaves the source readable byte-for-byte.
This is a syscall/fault test, not a physical power-cut test.

Default tests (14 ACID, two compatibility, one doctest) and archive tests
(16 ACID, two compatibility, one doctest) pass. Formatting, strict Clippy,
the default release build, dependency policy and its refusal tests pass.
Cross-version checks pass on 26 files, including raw, encrypted and archived
files in both reader/writer directions, with unchanged file hashes after retries.

Build the frozen baseline in a separate worktree. Copy only the supplemental
`io_profile.rs` and its `[[example]]` entry to the baseline; preserve all baseline
engine code. Use distinct target directories, identical flags/toolchain and
fresh data directories for each run:

```sh
cargo test --locked -p ojo
cargo test --locked -p ojo --features archive
RUSTFLAGS=-Ctarget-cpu=x86-64 cargo build --locked --release -p ojo \
  --features bench,archive --example acid_bench --example io_profile
taskset -c 0 target/release/examples/acid_bench \
  ojo /persistent-disk/new-raw-run 32768 64 1024
taskset -c 0 target/release/examples/acid_bench \
  sqlite-rowid /persistent-disk/new-sqlite-run 32768 64 1024
taskset -c 0 target/release/examples/io_profile \
  encrypted-archive /persistent-disk/new-profile-run 4096 64 1024 1000
python3 scripts/verify-ojo-sync.py \
  --binary target/release/examples/io_profile --output /persistent-disk/new-sync-check
```

The interop helper can be compiled against each implementation's archive-enabled
library, without altering either library. Set `OJO_RLIB` to the corresponding
`target/release/deps/libojo-*.rlib` built with `bench,archive`:

```sh
rustc --edition 2024 -Copt-level=3 scripts/verify-ojo-interop.rs \
  -L dependency=target/release/deps --extern ojo="$OJO_RLIB" -o /tmp/ojo-interop
/tmp/ojo-interop /persistent-disk/other-build/data.ojo 4096 64 1024 true
```

It scans every synthetic record, samples gets and verifies persistent exact
retry receipts against files written by the other version. An outer SHA-256
check ensures retries change no file bytes. Use `false` for plaintext; verify
archive files as well as originals. The helper uses the existing public API.

See [raw runs, source/binary hashes and qualification receipts](ojo-io-optimization-2026-10-24.json).
