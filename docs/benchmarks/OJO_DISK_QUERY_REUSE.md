# Bounded query reuse and sparse identifier work

Runtime source: `244e11a584ebdff0699c7c538c74bb25e8ccfaf1`. The
[complete receipts](OJO_DISK_QUERY_REUSE_2026_10_25.json) retain all fresh,
first-burst and warmed cases. Every case's ID/score-bit/timestamp/excerpt
signature matched the frozen `f69b18f` baseline at 1k/100k/1M, raw and Zstd.
The [original diagnostic limits](OJO_DISK_PROJECTION_INITIAL.md) still apply:
generated exact originals, CPU affinity, warmed filesystem/page state, three
rounds per client and no provider, authorization or durable source timing.

The candidate reserves 4 MiB for at most 128 ranking results and coordinates at
most 32 in-flight computations. Identical concurrent queries can share ranking;
each response independently fetches and checks its selected exact originals.
The cache is per immutable Index/Binding. Append/retirement creates a new empty
query cache. Semantic query/filter/time/limit fields are included in its key;
excerpt/output budgets still apply during each materialization.

Unfiltered probability-floor calculations reuse complete-view mass computed
from authenticated numeric columns at open and maintained on append/retirement.
Identifier passes visit anchored postings and advance other posting cursors by
bounded binary skips. When the ranker's floor needs full-view statistics, the
same complete view is visited separately; the exact scoring model is unchanged.
Identifier presence checks stop after finding an eligible witness. Existing
literal global document-frequency decisions remain complete.

| 1M rows,10 clients | Encoding | Common p95 ms | Identifier anchor p95 ms | Filtered common p95 ms |
| --- | --- | ---: | ---: | ---: |
| Forced fresh ranking | Raw | 757.5 | 290.9 | 587.6 |
| Forced fresh ranking | Zstd | 782.9 | 331.4 | 652.6 |
| Empty rank cache at mixed burst | Raw | 149.9 | 86.6 | 53.9 |
| Empty rank cache at mixed burst | Zstd | 168.2 | 88.9 | 67.7 |
| Rank cache warmed before cohort | Raw | 17.4 | 34.0 | 23.2 |
| Rank cache warmed before cohort | Zstd | 30.4 | 33.3 | 17.1 |

Warmed repeated queries improve materially, but **fresh and first-burst cases
still fail the 100 ms target**. Warmed results are not evidence for new/distinct
queries, a changed snapshot, cold storage or hosted acceptance. Fresh common
query tails remain adverse; the single-client fresh common p95 was about 71 ms
for both encodings. Identifier work improved, but its fresh ten-client cases
also remain over target. No general one-million-row concurrency claim follows.

At 100k, forced-fresh common p95 was 91.0 ms raw and 115.6 ms Zstd; the compressed
case still fails. Complete smaller and dense-literal observations are retained
in the JSON. Peak process RSS at 1M/10 clients ranged from117.8–138.9 MiB across
these modes. Estimated retained heap reserves the extra 4 MiB even before query
reuse warms, bringing the 1M estimate to about 124.7 MB. Query/source work and
application bookkeeping require separate admission.

The optional `rank_uncached` path deliberately bypasses result reuse for
diagnostics. Immutable authenticated page reuse can still occur. First-burst
processes obtain reference signatures through this path before the measured
cohort, warming pages while leaving result reuse empty. Warmed processes obtain
signatures through ordinary rank reuse. Fresh processes bypass result reuse on
every measured query. Exact original validation occurs in all three modes.
