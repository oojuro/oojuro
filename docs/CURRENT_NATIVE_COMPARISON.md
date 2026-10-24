# Current native versus Native84

**Status:** fixed campaign passed; independently verified by C and the coordinator. Four alternating pairs, all eight copied stores and known resource cleanup completed.

This measures the combined counted-result, borrowed-text ownership and append-lifetime source on the existing long-lived native projection workload. The separate Store prefix experiment and rejected traversal/offset/budget proposals are excluded. It does not isolate one commit.

The frozen 695,520,625 B public SWE input, existing 20,000-background selection, 20,623 final records and 201 timed queries per run are unchanged. Four runs per arm give 804 timed queries each. The same 52 labelled queries repeat each round; all eight saved result pools match the original Native84 reference byte for byte. This does not retain or prove equality of all 804 rank lists.

## Results

Medians of four run summaries, not pooled percentiles:

| Metric | Native84 | Current native |
|---|---:|---:|
| Search p50, ms | 131.345 | 133.023 |
| Search p99, ms | 170.478 | 170.379 |
| Search + original p50, ms | 131.947 | 133.561 |
| Search + original p99, ms | 171.166 | 171.076 |
| Append p50, ms | 1.494 | 1.452 |
| Append p99, ms | 8.275 | 8.997 |
| Peak process RSS, MiB | 147.801 | 147.904 |
| Peak whole cgroup, MiB | 145.973 | 145.791 |
| Steady process RSS, MiB | 74.301 | 70.615 |
| Steady whole cgroup, MiB | 72.527 | 68.451 |
| Steady anonymous, MiB | 69.148 | 65.287 |
| Steady file cache, MiB | 2.260 | 2.242 |

Steady RSS is about 4.96% lower and anonymous memory about 5.58% lower; both improve in every pair. Peak RSS is essentially unchanged, slightly higher in the median. Whole-cgroup and process RSS are separate accounting scopes.

Search p50 and search-plus-original p50 are slower in every pair. Search p99 is near neutral with two adverse pairs. Append p50 improves, while append p99 worsens in three pairs and in the median. This is a memory/latency tradeoff, not a general performance win or a strict no-regression proof.

## Every pair

| Round/order | Search p50 before → after (ms) | Search p99 before → after (ms) | Append p99 before → after (ms) |
|---|---:|---:|---:|
| 0/84/current | 131.901 → 132.823 | 170.247 → 170.371 | 8.122 → 9.649 |
| 1/current/84 | 131.183 → 132.003 | 166.399 → 169.342 | 7.747 → 9.492 |
| 2/84/current | 130.635 → 133.269 | 175.183 → 173.927 | 8.689 → 8.483 |
| 3/current/84 | 131.508 → 133.223 | 170.709 → 170.386 | 8.429 → 8.502 |

## Verification and limits

Quality was independently recomputed from all saved labelled lists. Both arms returned exactly 4,944,282 original bytes per run. Each of eight copied stores returned all 20,623 complete records, metadata and originals, including ordered range results and all ID lookups. Current source/copy bytes and modes match retained inventories. Closed allocated storage is 2,760,704 B for every arm; tiny apparent-byte differences are retained and are not a storage reduction claim.

All 21 recorded process identities and 17 actual/named cgroups are absent; closed evidence stores remain retained. Both source/artifact identities, portable Rust1.99.1 release configuration and exact finite cgroup controls are recorded. No PostgreSQL comparison, new baseline build or favorable rerun was performed.

This is warm local fits-RAM evidence for the fixed selection, not all raw rows, larger-than-RAM, OS-cold, hosted, concurrent or SOTA evidence. No hosted RAM, cost, pooled-tail or individual-commit gain is inferred. The companion JSON retains every run summary and all memory/tail categories.
