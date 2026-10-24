# Explicit PostgreSQL frequency-ranked comparison

Qualified locally at runtime source `0c4cf99`: fresh release builds, two actual-server checks, four fixed comparison pairs and eight complete copied-store checks. Draft work; default ranker unchanged.

`postgres` retains its existing English GIN and `ts_rank_cd` cover-density ranking. The new explicitly selected `postgres-frequency` arm uses PostgreSQL's standard `ts_rank` frequency ranking. Both require the existing optional `postgres-arm` feature on Linux, stay out of the default arm list and use the same owned server, indexes, original bytes, query terms, revision visibility, transaction durability and resource lifecycle. The chosen ranker appears in arm identity and server settings. No new dependency or engine/default feature change is introduced.

This is a different baseline, not a behavior-preserving optimization. Its scores and ordering can differ. A completed comparison must report relevance alongside latency, full returned originals, append/durability, total server memory and complete cluster storage. The original incomplete cover-density comparison remains evidence; no successful repetition, timeout extension or workload reduction is implied.

The alternative was motivated by a separate copied-store diagnostic. The complete comparison below provides the relevant latency and quality evidence; diagnostic rank-function timings are not used as a substitute.

The focused installed-server test exercises the same exact-originals, two-clock latest revision, deterministic ties, atomic failed batch and clean reopen story for each arm. It additionally asks the actual server for both ranking functions, verifies the selected output scores exactly and requires the fixture to distinguish the two functions. Both actual-server tests now pass, once each. The existing crash/copy tests cover the unchanged storage lifecycle; new qualification will identify its actual source/artifact and executed checks before any comparison claims.

## Complete measured comparison

Updated 2026-10-24T14:23:41.017489+00:00. All four fixed alternating pairs and all eight closed-copy original checks passed. Values below are medians of four run summaries, not a pooled-query percentile.

| Metric | Oojuro native84 | Postgres 17.11 frequency |
| --- | ---: | ---: |
| Search p50, ms | 131.522 | 170.082 |
| Search p99, ms | 175.912 | 550.987 |
| Search + original p50, ms | 132.196 | 171.053 |
| Append p50, ms | 1.446 | 1.011 |
| Append p99, ms | 8.319 | 2.009 |
| Prefill records/s | 41,287.935 | 9,431.610 |
| Peak whole-cgroup, MiB | 145.859 | 198.861 |
| Steady whole-cgroup, MiB | 72.412 | 196.791 |
| Steady anonymous, MiB | 69.148 | 11.891 |
| Steady file cache, MiB | 2.256 | 175.701 |
| Closed allocated disk, MiB | 2.631 | 140.445 |
| nDCG@20 | 0.368 | 0.128 |
| Recall@20 | 0.593 | 0.154 |

## What the result supports

For this warm local SWE-agent fixture with 20,623 records, Oojuro searches faster and retains less total charged memory and closed disk. PostgreSQL appends faster and uses substantially less anonymous memory. Its charged file cache and complete initialized cluster footprint are much larger. These memory categories must remain separate. The empty closed PostgreSQL cluster alone contains 38.444 MiB apparent / 38.625 MiB allocated. A small-database disk ratio does not predict a large deployment.

The explicit alternative is `postgres-frequency`/`ts_rank`; default `postgres`/`ts_rank_cd` remains unchanged. The earlier default-ranker600s timeout is preserved as incomplete, not converted into a successful result. Frequency ranking completed here but had much lower quality on the 52 labelled queries. Native and PG returned 4,944,282 versus 27,852,154 original bytes per run: this is the same query schedule with different ranked results and retrieval work.

## All four pairs

| Round/order | Native search p50, ms | Postgres p50, ms | Native append p50, ms | Postgres p50, ms |
| --- | ---: | ---: | ---: | ---: |
| 1/native→PG | 131.567 | 174.016 | 1.437 | 1.005 |
| 2/PG→native | 131.476 | 165.551 | 1.468 | 1.033 |
| 3/native→PG | 133.547 | 167.734 | 1.418 | 1.009 |
| 4/PG→native | 131.067 | 172.429 | 1.456 | 1.013 |

## Verification and limits

- 201 timed queries per run, four runs/arm:804timed queries per arm. All 52 labelled result lists/run are retained; their quality metrics were independently recomputed. The four repetitions use the same 52 labelled queries, not 208 different queries. Full per-query timing lists for all 804 were not separately retained.
- Each of eight copied stores reopens and returns all 20,623 full records and originals; IDs, keys, text, clocks and metadata match the pinned raw fixture. Root independently parses all eight protocol results and rehashes current source/copy inventories. Source stores remain unchanged.
- Actual PostgreSQL server settings confirm fsync/synchronous_commit/full_page_writes on, selected ts_rank and complete arm/server cgroup ownership. Parent-only PG RSS is not used for total-memory comparison.
- 103 observed process identities and 18 actual/named cgroup paths are independently absent. Closed evidence stores are intentionally retained.
- Native encrypted compressed Store +in-memory projection versus plaintext PostgreSQL English GIN; ranking/tokenization differ. Warm, local, fits RAM, one writer/query stream; no AWS, hosted capacity, cost, OS-cold, larger-than-RAM or SOTA conclusion.

The next performance work must address native append tail latency and anonymous projection memory as well as retaining the search/storage advantages.

## Provenance and retained failures

The actual release benchmark SHA-256 is `0f9e075507eee011ddd8970ec8ff6eba4b095625c533c6ed3dd53eeca365343d`; the actual libtest is `275c4c8c4142d92fa38e2a4ce52b004f15a0d0c89bd5dfb5df85d32d6dd04204`. Native84 is the independently frozen `5c56be5874a2c56d7d7db5d1d47c1db4ac633de8615ae94442cb3fd1f57c9f87`. The harness records AMD Ryzen7 8845HS,16logical CPUs, release build, ext4, kernel7.0.0-34; each arm has its finite3GiB/zero-swap cgroup. This is local single-stream evidence, not production or a universal database ranking.

The first frequency build failed `E0509` before artifacts; a minimal constructor correction preserves default initialization. Both failed/corrected build receipts, actual installed-server outputs, all four scorecards/eight saved ranked pools, controllers, independent full-original verification metadata and original default-ranker timeout are archived in the [manifest](benchmarks/postgres-frequency-2026-10-24/manifest.json).31metadata/log/source files are individually gzip-compressed; host paths are redacted, with original/published/stored digests and transformations recorded. No corpus payload, engine binary, PGDATA, credentials or hosted source is committed. Documentation/evidence commits after `0c4cf99` do not relabel a newly built artifact.

This optional benchmark arm introduces no foundational dependency or engine/default feature change. No ranker adoption, main merge, release or deployment is authorized.
