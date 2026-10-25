# Minimal .ojo: uncompressed ACID benchmark

This report describes the initial standalone engine. The subsequent
[I/O optimization qualification](OJO_IO_OPTIMIZATION.md) includes a matched
before/after comparison, encrypted/archive profiles and adverse controls.

The fresh file engine was measured against bundled SQLite 3.53.2 with WAL and FULL synchronous on a local Btrfs filesystem. Both engine and filesystem compression were disabled. A fresh parent directory was marked `chattr +m`; every resulting database retained the inherited no-compression flag. These results cover this synthetic workload and shared host, not a general database ranking.

SQLite used a prepared statement, `INTEGER PRIMARY KEY` and a 64 MiB page cache. Its ordinal keys correspond to the same eight-byte ordinal keys in Ojo; arbitrary binary keys require a different SQLite schema. Payload bytes and transaction sizes were identical. Ojo retains request IDs/history and verifies value digests; SQLite does not implement those additional contracts in this comparison. All returned payloads were checked.

| Records | Value | Batch | Ojo records/s | SQLite records/s | Ojo get p50 µs | SQLite get p50 µs |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,024 | 128 B | 64 | 6,997 | 5,814 | 0.37 | 1.73 |
| 1,024 | 1,024 B | 64 | 6,872 | 4,800 | 0.74 | 1.20 |
| 1,024 | 8,192 B | 64 | 5,862 | 3,688 | 3.31 | 4.25 |
| 32,768 | 1,024 B | 64 | 6,167 | 4,139 | 1.18 | 3.65 |

The table uses medians of two alternating runs. Throughput includes creation and final close/checkpoint. The JSON reports synced commit p50/p95/p99 separately, plus warm point reads, full scans, reopen, stored bytes, source hashes and the exact binary hash. The 1,024-record batched runs have only 16 commits each; the 32,768-record runs have 512. Run-to-run variation on this host is substantial.

Single-record commits were mostly limited by sync latency, around 125–132 records/s in the paired run medians for both engines. Ojo does not establish a universal win there. SQLite reopened in roughly 0.12–0.22 ms, versus Ojo's 0.32–9.04 ms at 1,024 records and 32.44 ms at 32,768 records. SQLite also won several sequential scans. Ojo validates all transactions and rebuilds its indexes at open, and verifies hashes on reads/scans; those costs remain visible rather than being moved outside the measured operation.

The earlier exploratory runs used transparent Btrfs Zstd compression and are retained only in the local scratch directory. They are not the evidence for the uncompressed results above. A separate preliminary tuned run also showed SQLite winning some larger warm point reads; performance depends on cache state and workload.

## Reproduce

Use a new directory on a local persistent filesystem. On Btrfs, disable compression before creating any database files:

```sh
mkdir /persistent-disk/ojo-bench
chattr +m /persistent-disk/ojo-bench
cargo build --locked --release -p ojo --features bench --example acid_bench
target/release/examples/acid_bench ojo /persistent-disk/ojo-bench/ojo-1 1024 64 1024
target/release/examples/acid_bench sqlite-rowid /persistent-disk/ojo-bench/sqlite-1 1024 64 1024
```

Repeat with fresh directories, alternating engine order, values of 128/1,024/8,192 bytes, batches of 1/64, and the 32,768-record case. The optional `sqlite` arm measures `BLOB PRIMARY KEY WITHOUT ROWID` with its default page cache; it is not the tuned baseline in the table. For 1 KiB values that schema can spill payloads to overflow pages, so its size is not evidence that Ojo intrinsically beats every SQLite layout.

[Raw measurements](ojo-minimal-acid-uncompressed-2026-10-24.json). The benchmark description text was clarified after measurement; runtime engine code, key schemas and measurement loops are unchanged. The raw report records the measured source hashes and binary hash. No power-cut, dedicated-device or encrypted performance qualification is claimed.
