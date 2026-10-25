# One-million-event actual-file diagnostic

A real synthetic `.juro` source now has one million Event-profile records.
[Complete receipts](JURO_ACTUAL_FILE_MILLION_2026_10_25.json) contain all 21
observations and 26 matching baseline/candidate signatures. The candidate is
frozen `7260a278638479a813d7ef70bcf8683e44ab7f06`, compared with main
`00ad5e10d454f76ec41ee83bdb76218a8194bfdb` reusing the baseline/candidate binaries and common harness
from the [1k/100k actual-file qualification](JURO_ACTUAL_FILE_PROJECTION.md).

Source writes use NOCOMPRESS and engine-issued time/cursors, with up to 1,024
prepared events per durability barrier. The 1M source is plain; encryption has
actual-file coverage through 100k in the earlier report. Both raw and Zstd
indexes use the same plain source. Actual `Journal.scan/get`, complete original
checks, checkpoint equality and exact score/cursor/time/excerpt signatures are
retained. This does not regenerate originals instead of fetching them.

Every child runs on one CPU with a 2 GiB virtual-address-space cap. OS caches are
retained. Retained clients share one healthy source owner; index ranking is
concurrent and selected original reads serialize through its mutex. Fresh
ranking bypasses the final-result cache but reference queries already warm
index pages/postings. Each retained client executes three rounds. Reopen and
full rebuild have one observation each. The benchmark lock excludes concurrent
builds/timing campaigns and is released between encodings.

| 1M events, ten retained clients | Raw main → candidate p95 ms | Zstd main → candidate p95 ms |
| --- | ---: | ---: |
| Fresh common word | 1,282.8 → 30.1 | 955.7 → 23.7 |
| Fresh identifier anchor | 488.6 → 103.8 | 291.7 → 61.0 |
| Reused common result | 169.7 → 7.8 | 105.0 → 1.3 |
| Reused identifier result | 70.0 → 24.8 | 45.8 → 20.8 |

The raw identifier case still exceeds 100 ms. New/distinct query sets and dense
phrases remain adverse in the [broader index diagnostics](JURO_DISK_QUERY_HOTPATH.md).
These small retained cohorts do not establish a general 100 ms target, hosted
service acceptance or a causal speed advantage for one codec over another.

The candidate's cold single-client source/index reopen and common query takes
1.60 seconds raw and 1.06 seconds Zstd. A full source reopen, fresh index build
and query takes **296.6 seconds raw** and **202.4 seconds Zstd**. These include
new-handle release, and existing reference handles stay resident for exact
comparison. “Cold” means fresh handles/index here; no OS-cache eviction or
machine-boot measurement occurs. The first index builds separately take
224.1 seconds raw and 221.4 seconds Zstd. One observation per build cannot
establish a repeatable codec difference.

The source contains 82,949,860 indexed text bytes and 1,004 exact binary events:

| Encoding | Source bytes | Retained index bytes | Source + index bytes | New-index peak logical bytes |
| --- | ---: | ---: | ---: | ---: |
| Raw index | 278,960,161 | 1,106,391,854 | 1,385,352,015 | 2,396,655,933 |
| Zstd index | 278,960,161 | 759,609,940 | 1,038,570,101 | 2,049,873,451 |

Persisted construction metrics were read back through authenticated index opens
in a separately identified inspection, with matching original-result signatures.
Observed allocated build peaks are 2,396,708,864 bytes raw and 2,049,912,832 bytes
Zstd; both report 191 peak files. These peaks cover the new index generation,
including temporary build files, and exclude the source, retained old indexes,
other leases and filesystem allocator slack. Admission must add those costs.
They are measured Linux allocations, not an arbitrary-filesystem quota promise.

Healthy source/index query processes peak around 581–595 MiB RSS in these
observations. Full rebuild processes with reference handles peak around
1,172–1,173 MiB. Estimated index heap alone is 137.1 MB and precharges its cache
capacities; it excludes the source's replayed metadata, transient query/build
work and application state. Passing a child virtual-memory cap does not qualify
a hosted application's 2 GiB cgroup or a ten-project workload.

Ten independent cold reopened sources are not run at 1M: the actual 100k
cohort already peaks near 566 MiB for that topology, making a 2 GiB target
inappropriate without a different ownership/resource plan. A retained source
owner avoids that duplication. No production activation follows automatically.

Source generation takes 13.42 seconds with the stated group durability. This
is not individual append latency or a 250 ms incremental-refresh result. The
combined storage remains much larger than the text population; the current
index duplicates original text and carries per-row framing/metadata. No
“third of the space” or competitor advantage is claimed. Persistent sufficient
statistics/positions and a compact explicitly versioned derived layout remain
follow-up work; source wire 0.1 and the Event profile are unchanged.
