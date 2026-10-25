# Initial encrypted disk projection diagnostics

Runtime source: `f69b18f60155e2b20ca0725a798bb6bcbb1a9793` (PR107).
This is a new optional derived index, not an optimization comparison against
SQLite, JSONL, the standalone event API or the existing directory-backed WAL.
The full sample/RSS/command receipts are in
[the JSON report](OJO_DISK_PROJECTION_INITIAL_2026_10_25.json).

The profiler generated synthetic rows with exact UTF-8/CRLF/NUL originals,
opaque IDs/keys, imported metadata, indexed/stored-only distinctions and time
variation. Source callbacks regenerated exact fixture rows. All supplied rows
contributed to one global model. No remote provider or source `.ojo` read,
permission check, network response or durable source write was measured.
Functional tests separately compare exact score bits/excerpts with the existing
ranker and check incorrect originals/bindings/keys, corruption and quota refusal.

The frozen release binary used Rust 1.99.1, generic x86-64 instructions and CPU 0
affinity. The shared benchmark flock excluded concurrent compilation and other
lanes' timings. Cache files used persistent Btrfs beneath an inherited
`chattr +m` parent, so filesystem compression did not supply hidden compression.
This differs from the deployment's filesystem/hardware. Builds have one
observation; each query has three rounds per client. Each query process first
runs all cases, warming pages; these are not cold-cache measurements.

| Rows | Cache encoding | Build seconds | Retained apparent MB | Observed build allocation peak MB | Query peak RSS MiB,10 clients |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1,000 | Raw | .323 | 1.16 | 2.45 | 6.8 |
| 1,000 | Zstd | .293 | .75 | 2.04 | 7.1 |
| 100,000 | Raw | 12.33 | 115.68 | 244.05 | 32.6 |
| 100,000 | Zstd | 13.66 | 75.16 | 203.54 | 20.1 |
| 1,000,000 | Raw | 150.46 | 1158.72 | 2444.32 | 135.9 |
| 1,000,000 | Zstd | 165.36 | 753.86 | 2039.46 | 131.4 |

One million rows retained an estimated 120.48 MB of heap including the reserved
page cache. Build process peak RSS was 134.4 MiB raw and 135.3 MiB Zstd. Temporary
sort/spool files are material: retained size alone understates cold-build disk
reservation. Allocated observations are sampled at close/removal/phase
boundaries; no hard arbitrary-filesystem quota or continuous peak claim follows.

| Rows | Encoding | Clients | Common term p95 ms | Identifier anchor p95 ms | Filtered common p95 ms |
| ---: | --- | ---: | ---: | ---: | ---: |
| 100,000 | Raw | 1 | 6.4 | 7.6 | 7.3 |
| 100,000 | Raw | 10 | 80.8 | 93.1 | 90.2 |
| 100,000 | Zstd | 1 | 7.9 | 7.2 | 7.4 |
| 100,000 | Zstd | 10 | 137.5 | 111.1 | 116.8 |
| 1,000,000 | Raw | 1 | 72.9 | 63.7 | 59.0 |
| 1,000,000 | Raw | 10 | 705.9 | 625.5 | 625.4 |
| 1,000,000 | Zstd | 1 | 68.9 | 62.0 | 60.5 |
| 1,000,000 | Zstd | 10 | 743.6 | 679.0 | 650.6 |

These cases **fail the 100 ms ten-client target** at one million rows; several
compressed 100k cases fail too. They establish diagnostic capacity, not the
requested concurrent latency acceptance. Full-query work remains linear in
candidate postings and currently runs two exact passes. Unique-term queries
also scan numeric columns when the ranker's floor needs full visible mass.

The report retains adverse cases. A dense exact phrase over only 1,000 rows
already reached 188.2 ms raw and 242.1 ms Zstd p95 with ten clients; this opens and
checks many cached exact originals before ranking. Compression is not a
universal speed improvement. No dense literal million-row latency qualification
was performed. Rebuilding the full index after an append cannot satisfy the
250 ms refresh requirement; incremental updates remain separate required work.

Default legacy in-memory admission limits were not raised. The new index has
explicit independent limits and does not retain a population of original text
or full Row/String objects. It is disposable: original recovery, wire 0.1
durability, encryption and exact retry behavior remain independent.
