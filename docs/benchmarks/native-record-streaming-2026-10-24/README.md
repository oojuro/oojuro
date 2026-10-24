# Native adapter: fewer query copies

In one fixed local SWE replay, streaming the same public record pages and moving owned text/attributes reduced **median search+original p50 by 9.82%** and **peak process RSS by 10.83%**. This changes the benchmark adapter; core runtime, storage formats and retained engine state are unchanged.

**The final round had a worse p99.** This is a review candidate, not evidence of a universal latency improvement.

| Metric | Current f726 before | Streamed ebc after |
| --- | ---: | ---: |
| Median of 4 run search+original p50s |147.317 ms |132.857 ms |
| Median peak process RSS |165.520 MiB |147.602 MiB |
| Median cgroup peak |163.760 MiB |146.107 MiB |
| After-prefill RSS range |74.219–74.371 MiB |74.031–74.254 MiB |
| Median allocated complete footprint |2.625 MiB |2.625 MiB |
| Original bytes returned, every run |4,944,282 |4,944,282 |
| Complete records, every store |20,623 |20,623 |

Peak process RSS fell 17.918 MiB. After-prefill memory was sampled **before** the query projection; it does not measure loaded-index residency. Process peak is Linux VmHWM, while cgroup peak includes anonymous memory and file cache. Neither isolates exact retained index bytes.

## All four rounds

| Round/order | Before p50 ms | After p50 ms | Before p99 ms | After p99 ms | Before RSS MiB | After RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
|0 AB |146.810 |132.266 |185.649 |169.355 |165.531 |147.609 |
|1 BA |148.339 |132.967 |180.639 |168.848 |165.508 |147.578 |
|2 AB |146.858 |132.747 |177.870 |168.161 |165.348 |147.594 |
|3 BA |147.776 |137.474 |183.122 |211.007 |165.625 |147.676 |

All 4 p50/RSS directions agree. The final BA p99 was 15.23% worse; its cause is unproven and no repeat was used to seek a favorable result. Each p99 comes from 201requests, with few tail observations. No pooled p99/SLA or throughput improvement is claimed.

The equal-query-weight arithmetic mean of the 4 **run p50s** is147.446→133.864 ms (9.21% lower). It is not the mean of individual request latencies. Raw request-duration series and per-arm elapsed sums were not retained, so a true pooled median/mean or wall-weighted request summary cannot be reconstructed. Entire campaign wall time 256.532s includes both arms, verification and cleanup; it is not per-arm throughput.

Append code is unchanged. Its per-run p50 varied 1.448–7.376 ms before and1.463–6.971 ms after: **no write gain**. Complete apparent median footprint was2.60603→2.60725 MiB, with unchanged allocated median: **no storage gain**.

## Verification and setup

- Source before `f726bab51d982896a129d4bdf2fd2d3a2edc2fff`; after signed `ebc9771e8a9d9c018b61bf84743f47e3535b6e01`, based on composition82. Only native benchmark adapter code/test changed. Other benchmark arms and core library source are identical.
- Same retained public SWE corpus, full 695,520,625B SHA256 `3106438b52feadab2bcf3c0e7ff7fb40ac8625b3969312e8cf64d4c3782089c2`; first 10 dev trajectories, 20k background, thinking indexed, k 20, batch 1000, 201 queries/store.
- Exactly AB/BA/AB/BA: 8 fresh retained stores,804 successful queries/arm. All 4 labelled result+score pools byte-equal; 52 retrospective quality labels were repeated, not 208 independent labels. Same nDCG20=.368154661 and recall20=.593269231.
- Build and 2 focused adapter tests passed. Existing 14 correctness gates and 6 SIGKILL injections passed, preserving 1,200 ACKed originals. All 8 copied-store reopens compare complete Doc metadata/text/clocks/IDs; complete closed footprints agree with cards.
- Actual Rust/Cargo 1.99.1, portable x86-64 release opt 3, locked/offline, same sqlite-arm feature profile. Before artifact `a4b861d7…086a51`; after `5c56be58…c9f87`. No SQLite workload was repeated.
- Per-arm 3 GiB/no-swap/300 s; bounded outer units. All 32 exact scope identities, recorded PID/startticks, live-observed outer/inner cgroups and outer PID are independently absent. Stores remain retained. The gate live-property observer failed before saving its raw fields; that limitation is preserved, and no gate was repeated. Workload raw properties were saved before assertions and independently corroborated.

## Limits

Warm local APIs on one Ryzen 8845HS host, not hosted/HTTP/S3/KMS/SQL. The workload fits RAM and does not establish cold-OS or larger-than-RAM behavior. No production price, deployment, database superiority or SOTA claim follows. RecordLog still retains decoded originals; its projection retains originals/postings; public page selection still scans/sorts each page. Further core RAM work is separate.

## Reproduction archive

[summary.json](summary.json) and the four raw scorecards retain every numeric run statistic. [PLAN.md](PLAN.md) fixes the exact public input, sources, commands, limits and result/reopen oracles. [manifest.json](manifest.json) distinguishes raw retained hashes from path-redacted public evidence. Runtime code is the preceding signed ebc9771 commit; the following evidence commit changes documentation only.

The controller-adaptation snapshot records the pre-build NOT_FROZEN guard; candidate-hash-freeze.diff records its only subsequent substitution. The actual frozen controller is controller.py.txt. Placeholder paths must be resolved to owned local directories before use. The earlier input receipt is historical; both actual phases independently rehashed the full input before launch. Full binary/data/store evidence remains local and is intentionally excluded.
