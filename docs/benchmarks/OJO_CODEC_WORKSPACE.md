# .ojo codec workspace and bounded prefix traversal

This extends engine main `1a9a99ca92660d984cb6af4b87e218aecd714d99`
(the merged PR102). Wire 0.1, public commit durability, exact retries, keys,
corruption checks and archive publication ordering are unchanged. Default raw
builds retain the same four direct dependencies and no compression library.

## Bounded latest-value pages

`Database::scan_prefix(prefix, after, limit, visitor)` supplies bytewise ordered
latest live values with an exclusive key cursor and a 1–1024 value page limit.
The visitor borrows each value and returns `ojo::Result<()>`; an error stops the
call immediately. An empty value is visited, a deleted key is omitted, and
updates are reflected after a commit. The returned cursor is the last visited
key only when another matching live key exists.

The first call builds a separate ordered index of live keys, which increases
retained key memory. Subsequent pages select a bounded key range without reading
the full history. Raw values use one reusable buffer and their SHA-256 checks;
encoded values use at most one verified decoded transaction body within a call.
No decoded values are retained between calls. Ordinary point reads keep the
existing zeroed allocation and hash lookup; their path does not allocate the
ordered key index.

This API preserves the handle's existing lifetime inode lock. Closing a handle
before network work permits a writer; reopening still validates the complete
history and reconstructs indexes. A continuation key is a range bound rather
than a frozen snapshot across writes. Immutable monotonic record keys and
retained checkpoints support application-controlled resumable exports.

Tests cover binary prefixes, exclusive/empty/past-end cursors, empty values,
overwrites and deletes after warming the ordered index, reopen/archive,
oversized bounds, callback errors and corruption after previous reads. These
are correctness tests, not million-record/concurrency qualification.

## Reusable independent codec contexts

Archive-enabled handles lazily retain standard Zstandard compressor/decompressor
contexts rather than recreating them for every transaction. Each transaction
still has a complete independent frame with no external dictionary, the same
level 3 compression and the same decoder limits. Context memory can remain
allocated until the handle is dropped. No source value, decrypted transaction
or authority result is cached between calls.

A dedicated test compares repeated independently decodable frames, alternates
different inputs and validates successful decoding after a deliberately invalid
frame checksum. The engine's frozen independent wire vectors remain unchanged.

## Measured codec-only comparison

The 24 qualified runs compare main 1a9a99c against codec-only runtime 786252f.
The subsequent prefix API and raw-allocation-preserving refactor are covered by
functional gates separately; this table does not measure that new API.

Both binaries have distinct hashes and independent Cargo target directories.
They use identical `io_profile.rs`, Rust 1.99.1 release defaults,
`-Ctarget-cpu=x86-64`, `bench,archive`, CPU 0, persistent Btrfs with inherited
`chattr +m`, and identical synthetic complete-value checks. Each of six cases
runs baseline/candidate/candidate/baseline, with 10,000 timed warm gets,
three scans and five reopens per run. The table contains medians of two run
medians, not pooled percentiles. Shared host activity/frequency is uncontrolled.

| Profile | Metric | Baseline | Codec reuse |
| --- | --- | ---: | ---: |
| Archive 4,096 × 1 KiB, batch 64 | Get p50 | 4.539 µs | 3.486 µs |
| Archive 4,096 × 1 KiB, batch 64 | Reopen+get | 0.683 ms | 0.461 ms |
| Encrypted archive4096×1KiB, batch 64 | Get p50 | 6.025 µs | 5.135 µs |
| Archive 4,096 × 8 KiB, batch 64 | Get p50 | 16.855 µs | 16.082 µs |
| Archive 256 × 128 B, batch 1 | Full scan | 0.485 ms | 0.256 ms |
| Raw 32,768 × 1 KiB, batch 64 | Get p50 | 0.877 µs | 1.011 µs |
| Raw 32,768 × 1 KiB, batch 64 | Full scan | 12.884 ms | 13.575 ms |
| Archive 4,096 × 1 KiB, batch 64 | Archive creation | 53.366 ms | 66.881 ms |

The measured compressed reads improve, especially small frames. Raw results
and archive creation are mixed; the raw get candidate includes a 1.129µs run
versus 0.893µs in its other run. The archive creation regression above is retained,
as are all write/retry timings and `/usr/bin/time` peak RSS measurements.
Archive 8 KiB creation medians are 77.692 versus 79.446 ms; encrypted 1 KiB archive
creation 59.415 versus 60.748 ms. No overall write or archive-creation improvement
is claimed. All paired source/archive file lengths and read/scan signatures match.

The initial shared-target attempt is explicitly invalid: Cargo reused the same
baseline executable for both labels. Its identical hashes were detected, the
attempt was discarded, and builds repeated with independent target directories
and an assertion requiring distinct executables before any qualified timing.
The raw JSON retains that invalid attempt separately from the qualified runs.

## Reproduce and limits

Build main and candidate in separate worktrees and separate target directories,
with the same toolchain/flags. Do not reuse a freshness result between exported
source trees. Check source hashes, distinct binary hashes and no-compression
flags before timing. For each arm run fresh directories in the fixed order:

```sh
RUSTFLAGS=-Ctarget-cpu=x86-64 cargo build --locked --release -p ojo \
  --features bench,archive --example io_profile --target-dir /separate/target
taskset -c 0 /separate/target/release/examples/io_profile \
  archive /persistent-disk/new-run 4096 64 1024 10000
```

Repeat for raw/encrypted 32,768/4,096 × 1 KiB batch 64, archive 4,096 × 8 KiB batch 64,
encrypted-archive 4,096 × 1 KiB batch 64, and archive 256 × 128 B batch 1, as recorded in
the raw report. No build overlaps a timed campaign. These synthetic warm local
profiles establish neither a universal database ranking nor hosted latency,
physical power-cut behavior, a million-record search index or agent integration.

[Qualified runs, invalid-attempt separation, hashes and functional gates](ojo-codec-workspace-2026-10-25.json).
