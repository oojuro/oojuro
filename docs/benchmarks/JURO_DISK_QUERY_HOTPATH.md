# Bounded postings and numeric selections

This candidate speeds repeated lexical work and sparse filtered selections, but
it does **not** meet a general one-million-row, ten-client, 100 ms target.
Distinct common queries and dense literal phrases still exceed that target.
The [complete receipts](JURO_DISK_QUERY_HOTPATH_2026_10_25.json) retain every
observation, sample, command, binary hash and adverse case.

The frozen candidate is `33731c3cf049a481a012669f0087116302d1920c`, rebased
without index-runtime changes onto main `00ad5e10` as `bbc8ce2`. The repeated
query baseline is `244e11a`; the distinct/dense baseline is main `0e424122`.
Each matched distinct/dense binary uses the same benchmark source; source and
binary hashes are recorded. All 72 repeated-query signatures match the earlier
baseline, and all 46 distinct/dense pairs match exactly: IDs, score bits,
timestamps, continuation markers and original excerpt bytes.

These observations use generated synthetic originals, an actual encrypted disk
index, one CPU, and retained filesystem caches. They exclude actual Event-file
opens/reads, provider authorization, network and source durability. The
separately opt-in `profile_juro_projection` example measures the actual-file
path; no actual-file result is inferred from this report. No hosted acceptance
or storage advantage over SQLite follows from these observations.

The candidate reserves 12 MiB for validated postings, sufficient statistics,
exact shape summaries and numeric selection bitmaps. It reserves before load;
active reader leases remain charged. It falls back to exact disk cursors when
this pool is full. Equal `(length,scope,term frequency)` rows share one exact
score calculation; the best 32 rows per shape suffice for result limits up to
32. Heterogeneous shapes and larger limits retain complete scoring. Filtered
queries can reuse a bounded eligibility bitmap and visit only sparse eligible
ordinals. These caches never replace source authority or exact original reads.

| 1M rows, ten clients, forced fresh final ranking | Raw before → after p95 ms | Zstd before → after p95 ms |
| --- | ---: | ---: |
| Common word | 757.5 → 41.6 | 782.9 → 54.8 |
| Identifier anchor | 290.9 → 106.0 | 331.4 → 111.1 |
| Filtered common word | 587.6 → 62.2 | 652.6 → 75.1 |

“Fresh” bypasses the final-result cache; initial reference queries already warm
index pages and posting work. The mixed benchmark rotates six cases per worker
for three rounds. The identifier case still exceeds 100 ms in both encodings.
The immutable index's estimated retained heap at 1M is about 137.3 MB, including
all cache policies before warming. Caller/source handles and transient work
require separate reservations.

The broader distinct workload rotates seven common terms and three mixed
queries across ten workers. At 100k, common-word p95 improves from 132–165 to
40–76 ms raw and from 157–184 to 41–67 ms Zstd. Mixed queries remain 110–143 ms
in the candidate. At 1M, concurrent common posting lists exceed the bounded pool:
common p95 remains 1.17–1.28 seconds and mixed p95 remains 1.79–2.06 seconds.
Increasing the RAM cap until this fixture fits would conceal that limitation.

Dense literal queries still read candidate text to establish exact substring
membership. The new caches do not remove this dominant work:

| Dense literal | Raw main → candidate p95 ms | Zstd main → candidate p95 ms |
| --- | ---: | ---: |
| 1k rows, ten clients | 234.1 → 233.3 | 315.1 → 331.8 |
| 100k rows, one client | 3,932.1 → 3,932.5 | 4,438.5 → 4,438.6 |
| 1M rows, one client | 47,832.0 → 47,943.1 | 49,130.0 → 49,764.5 |

The compressed 1k and 1M observations are adverse. These small cohorts do not
establish that each difference is reproducible, and no dense-phrase improvement
is claimed. The next work is durable sufficient statistics and positions, with
an explicit disposable-index format/rebuild policy and unchanged exact ranking
and source bytes. The existing index also duplicates source text; total source
plus index storage must be measured before making a space claim.
