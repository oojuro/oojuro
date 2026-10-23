# Full-build projection allocation slack

Candidate `59ac5a9f6036fc51c133fc5b5eb056b1045ae411`, based on clean compact
source `f01345f92a2570ddce7a9577b7703fddfc8eaee7` (runtime `5197b8c`).
Five allocation-only lines reserve the iterator's known document count and
shrink posting vectors after a complete record index build. Incremental posting
appends retain ordinary amortized growth. No public API, disk format, score
arithmetic, ordering, tokenizer, topK, original bytes or authority check changes.
No dependency, Store codec, hosted runtime or AWS change is included.

## Matched results and limits

[Raw matrix, tools and hashes](benchmarks/projection-slack-2026-10-23/provenance.json)
freeze the original A/B run and a separate B/A check; neither was replaced.
Same frozen public ArguAna 8,674 records/15 dev queries/top100 plus originals,
three fresh processes per arm and order, warm OS cache, generic x86-64 release
build, shared Ryzen 8845HS/ext4 workstation, 3 GiB child limit/no swap.
MB below is decimal; values are medians of three run statistics, not pooled tails.

| Order | Source | Peak RSS MB | Search + originals p50 ms | Sample p99 ms |
| --- | --- | ---: | ---: | ---: |
| A/B | Compact baseline | 84.337 | 73.927 | 556.581 |
| A/B | Trimmed | 82.063 | 87.107 | 582.425 |
| B/A | Trimmed | 81.973 | 64.544 | 464.797 |
| B/A | Compact baseline | 84.148 | 70.293 | 466.742 |

The repeated **2.6–2.7% peak RSS reduction** is modest. Latency changes direction
between orders, so these samples establish no speed improvement or systematic
regression. The first matrix overlaps a short root incremental build and the
dependency audit's one-core recovery test; host snapshots are retained. The
reverse check has no known concurrent compilation/test. Each p99 has only 15
query samples per run and is unsuitable as a production tail estimate.

Separate copied-store diagnostics preserve 45 full-hit samples per source:
IDs, exact score bits, continuation, clocks, metadata, UTF-8 excerpt offsets and
contents match across sources, with exact original checks. Median pure cold
projection build time across those samples is 417.613 → 419.437 ms. The profile
also executes a reused projection; its cold and reused timings are separate.
One process per arm's phase/RSS diagnostic reports after-query RSS 83.289 →
81.146 MB. This includes allocator-retained pages, not just live index memory.

The 44-step history workload (two frozen SWE-agent dev trajectories, 1,000
background records, top20 plus originals) reports 18.465 → 18.055 MB peak RSS,
6.604 → 7.558 ms search+originals p50 and 1.268 → 1.304 ms durable append p50.
Append sample p99 is 2.072 → 2.402 ms; no write throughput win is claimed.
These small history latency samples overlap in range and the first-run host
activity applies. There are 1,136 complete stored records, including canaries.

## Correctness, storage and qualification

- Existing lexical/oracle/cache tests: 22 passed, two ignored. Existing
  record/projection/recovery tests: 100 passed, five ignored.
- Portable release benchmark/profile build passed; `git diff --check` passed.
- All seven harness gates pass across both sources and three runs, including
  18 SIGKILL recovery trials, exact text, clocks, freshness and determinism.
- ArguAna nDCG@10 remains 0.5036781581683427; original bytes/run remain
  1,036,320. History nDCG@10 remains 0.7477066261975289 and bytes/run 828,931.
- Twelve independent process/store reopens compare all retained original fields
  exactly; 66 files and 24 apparent/allocated footprint values match scorecards.
- ArguAna allocated store bytes remain 3,538,944. Apparent bytes vary with
  genuine timestamps/encryption and compressibility. History allocated bytes
  span 241,664–245,760 for both arms; this is not a storage-format saving.

The retained cache, materialized originals, metadata, query work and allocator
overhead remain. No hosted capacity/instance saving or compression pricing
claim follows from these local measurements. Owned-text move remains excluded.
