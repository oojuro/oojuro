# Incremental derived-cache refresh diagnostics

The complete [raw receipts](OJO_DISK_APPEND_INITIAL_2026_10_25.json) compare
`07078c5` with `5fd096c7b54f27944c7b76f2b4b145c4714463eb`. Each process opened
and warmed an existing synthetic index, then timed creation/publication of a
new cache generation containing 32 additional immutable rows. Prefix validation
was a caller assertion; durable source append, provider reads, current
authorization and response delivery are excluded. These are three observations
per size/encoding, not an end-to-end write latency percentile or hosted SLA.

| Prior rows | Encoding | Initial refresh, ms | Batched persistence refresh, ms |
| ---: | --- | --- | --- |
| 1,000 | Raw | 242.5,243.6,255.4 | 99.4,123.5,115.8 |
| 1,000 | Zstd | 241.8,229.6,247.0 | 118.2,126.9,114.8 |
| 100,000 | Raw | 283.9,291.7,251.9 | 132.3,135.8,133.7 |
| 100,000 | Zstd | 250.3,250.8,259.2 | 131.1,126.4,127.1 |
| 1,000,000 | Raw | 280.6,279.8,310.6 | 173.9,154.4,153.4 |
| 1,000,000 | Zstd | 298.7,285.8,287.3 | 185.4,143.3,211.1 |

Initial refresh often failed 250 ms before adding source or service work. The
candidate batches directory persistence until the generation is complete,
avoiding redundant directory syncs per tree. Tiny temporary sort/row spools
are flushed for local reads and deleted before publication, so they need no
durable sync. Larger temporary files still receive explicit writeback.
Every retained tree/blob pack is synced, then the completed generation
directory, generation publication, encrypted descriptor and its publication
are synced. Legacy source authorities retain their original sync behavior.

Result IDs, exact score bits, timestamps and exact excerpt signatures matched
between both candidates on every run. Raw and compressed cases are retained,
including the 211 ms candidate tail. Cache-only observations below 250 ms do not
establish the full write target. A separate functional oracle compares append,
retirement, old-snapshot immutability and reopen with a complete global rebuild.

Append uses copy-on-write paths, shared numeric blocks and immutable encrypted
pack hard links. One million plus 32 rows retained about 1.168 GB raw or 762 MB
Zstd apparent bytes. Old generations remain owned separately. The benchmark
cloned fixtures using hard links and removed every per-observation clone after
capturing receipts; the shared baseline datasets remain for query optimization.

The compiler/CPU/filesystem and source-callback limitations are the same as
[the initial projection diagnostics](OJO_DISK_PROJECTION_INITIAL.md). There
were no overlapping builds or competing lane timings under the shared flock.
