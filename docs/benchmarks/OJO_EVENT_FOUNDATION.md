# Event-profile foundation measurements

Runtime source: `b1888d4d902527fdbc27c875f23af1dfe9d6d55e`. Frozen release
`event_bench` SHA-256:
`04d78f8bc17df39f95b291f5fb2ade822850374ea01a74f1918936cf77702e45`.
These are new Event-profile measurements; historical KV/search numbers do not
apply. Full per-run receipts are in
[events-foundation-2026-10-25.json](events-foundation-2026-10-25.json).

Each of four rounds wrote 1,024 synthetic 1-KiB originals, one event per durable
transaction. Arm order varied. One process ran at a time under the shared build/
benchmark exclusion lock on persistent Btrfs. Filesystem compression was
explicitly disabled and checked on every retained data file. CPU affinity was
unrestricted; these were warm local runs, not a dedicated-device or cold-cache
campaign. No concurrent cooperating build or benchmark ran during the cohort.

The numbers below are medians of four per-run p95s. Reopen and scan are medians
of four whole-operation timings. They describe this small workload only.

| Arm | Durable append p95 | Exact warm get p95 | Reopen | Verified full scan | Raw file |
| --- | ---: | ---: | ---: | ---: | ---: |
| Event raw, plaintext | 19.80 ms | 1.79 µs | 3.11 ms | 2.59 ms | 1,249,541 B |
| Event raw, encrypted | 19.68 ms | 8.36 µs | 8.12 ms | 4.54 ms | 1,290,541 B |
| SQLite timestamp/BLOB | 20.74 ms | 7.84 µs | 0.56 ms | 1.96 ms | 1,409,024 B |

Append p95s varied from 18.38–21.32 ms for plaintext Event and
18.19–21.05 ms for SQLite; this small cohort does not establish a durable-write
win. Encrypted Event varied 12.02–21.77 ms. Plaintext Event warm gets were faster
in these runs; encrypted gets were mixed. Event reopen was slower, consistent
with verifying its entire history and rebuilding in-memory bookkeeping.
SQLite's ordinary open does not perform an equivalent full-history verification.

SQLite was configured with WAL, FULL synchronous, prepared insertion/lookup
statements, a 64-MiB page cache and an integer-primary-key timestamp/BLOB table.
It omits persistent retry identities and Ojo's per-read original checksum.
These contract differences remain part of interpreting the comparison.

Separate compressed archives preserved every original and logical cursor.
Their sizes were 270,082 B plaintext and 311,082 B encrypted. Median archive
creation was 68.03 ms and 59.70 ms respectively; median per-run p95 archived
gets were 2.11 µs and 3.92 µs. The archive result includes a separate new file
with its final sync before publication. The original remains unchanged.
This highly repetitive synthetic payload is not a universal compression ratio
or a full-text-search result. Normal Event writes were uncompressed.

The actual non-agent Rust sensor example also produced a raw Event file that
the independent Python reader decoded with identical time, cursors, binary data
and empty data. The source qualification receipt records both binary hashes and
the resulting file hash. All owned benchmark datasets were removed afterward;
compact receipts and frozen proof artifacts remain for review.

Reproduce on persistent storage with filesystem compression disabled:

```sh
cargo build --locked --release -p ojo --features bench,archive --example event_bench
target/release/examples/event_bench ojo /persistent-disk/new-event-run 1024 1024
target/release/examples/event_bench ojo-encrypted /persistent-disk/new-encrypted-run 1024 1024
target/release/examples/event_bench sqlite /persistent-disk/new-sqlite-run 1024 1024
```

No indexed search, million-record acceptance, ten-client concurrency, hosted
latency, billing, authorization or public-release qualification is established
by these microbenchmarks. Those remain separate goal gates.
