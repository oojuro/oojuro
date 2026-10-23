# Candidate union mask: median observations and acceptance limits

**Candidate, not deployed.** Final signed runtime source is
`774e39ba5cac89a0fc2af530e93d20085b2279db`. It keeps zero/one unique candidate
inline and allocates the bounded mask only on a second distinct row. The first
variant's sparse regression below remains frozen; no previous data was replaced.

## Final source qualification

[Final raw matrices, exact hashes and tools](benchmarks/query-mask-2026-10-23/final/provenance.json)
use the same input/settings described below, with another three fresh processes
per source. Baseline is signed PR65 (compact postings + build-slack trim), not
the deployed source. No known concurrent compilation/test during final timing.

| Workload | Metric | Baseline | Final mask |
| --- | --- | ---: | ---: |
| ArguAna | Search + original p50 ms | 66.385 | 60.531 |
| ArguAna | Sample p99 ms | 462.656 | 456.094 |
| ArguAna | Peak RSS MB | 82.141 | 82.100 |
| History | Search + original p50 ms | 6.161 | 6.158 |
| History | Search + original sample p99 ms | 23.499 | 24.686 |
| History | Durable append p50 ms | 1.288 | 1.287 |
| History | Durable append sample p99 ms | 1.507 | 2.289 |

The final ArguAna search median is 8.8% lower in this matched run; per-run p50
ranges do not overlap (65.655–66.781 vs 60.286–61.429 ms). Peak RSS is essentially
unchanged. The earlier dense variant improved medians in both measurement
orders, but its sampled tails were mixed. **No universal tail, write throughput,
storage, hosted latency or RAM-capacity improvement is established.** Each
ArguAna tail uses only 15 query samples/run; each history tail only 44 steps/run.
The worse history search/append tails remain explicit acceptance gaps before
production selection; p95 and statistically useful end-to-end tails require
broader matched qualification.

The exact same bounded sparse component check confirms the refinement fixes
the eager-mask overhead. Medians of three per-process p50 values, 2,000 samples
per case/process after 100 warmups, same CPU2 and 100k-row synthetic population:

| Case | Baseline ns | Final ns |
| --- | ---: | ---: |
| Literal miss | 261 | 260 |
| First direct key | 341 | 320 |
| Scoped first key | 340 | 321 |

Raw p95/p99 and samples are archived; this component result is not extrapolated
to end-to-end performance. Empty/singleton paths allocate no word mask. Multirow
mask size is at most 12,504 bytes at 100k rows; sparse multirow cases may pay this
bounded scratch overhead. The result row vector reserves exactly candidate count.

Final source passes 23 lexical tests/two ignored and 100 record tests/five
ignored, portable release build and `git diff --check`. The boundary test
forces singleton→mask transition across 63/64/65/100k populations, duplicates,
phrases, direct keys and time exclusion. All seven gates pass both sources
×three runs, including 18 SIGKILL trials. Forty-five full-hit signatures match
IDs, exact score bits, continuation, clocks, metadata, UTF-8 excerpts and original
checks. Twelve independent all-field reopens, 66 file inventories and 24 complete
apparent/allocated footprint values pass. ArguAna nDCG@10 remains
0.5036781581683427 with 1,036,320 original bytes/run; history remains
0.7477066261975289 with 828,931 bytes/run. Storage differences are allocation/
compression variability from genuine clocks/encryption; no format change.

Broader many-journal/concurrency, histories larger than RAM, attachment and
matched SQLite/Postgres/JSONL+Zstd qualifications remain separate. Neither
rejected cache release nor rejected top-K materialization is included here.

## First source and regression record

First signed source `2679653fa83dce115b3d9d0f0b2d0adaf466633b` starts from signed
PR65 `c42c10d` and excludes the replay-cache and top-K experiments. A u64-word
mask replaces candidate-union BTreeSet inserts. Set bits are extracted in the
same ascending row order; scoring, full sorting, scope/visibility, formats,
original bytes, clocks and durability are unchanged. Popcount reserves the
exact row-vector size. At the public 100k-row limit the mask uses at most
12,504 bytes, independent of posting occurrences; this is scratch, not a
retained-index or process-memory cap.

## Frozen first source

[First provenance/raw matrices/tools](benchmarks/query-mask-2026-10-23/first/provenance.json)
are retained before refinement. Same frozen public ArguAna 8,674 rows/15 dev
queries/top100 plus originals; three fresh processes/stores per source and
order, warm OS cache, generic x86-64, shared workstation, 3 GiB/no-swap scopes.

| Order | Source | Peak RSS MB | Search+original p50 ms | Sample p99 ms |
| --- | --- | ---: | ---: | ---: |
| A/B | Baseline | 82.469 | 68.107 | 464.760 |
| A/B | Mask | 82.178 | 58.034 | 435.561 |
| B/A | Mask | 82.170 | 61.635 | 460.582 |
| B/A | Baseline | 82.334 | 62.987 | 452.150 |

Medians of three run statistics, only 15 query samples per p99. Search medians
improve in both orders, while the reverse sampled tail worsens; this is not a
general tail or throughput claim. RSS changes are small. The history p50 is
5.434→5.439 ms (essentially equal); append p50 1.257→1.214 ms is an observation,
not a write improvement claim. Correctness/quality and returned original bytes
match. Existing lexical tests plus sparse-bound test: 23 passed/two ignored;
record tests: 100 passed/five ignored; build passed; seven gates pass both
sources ×three runs (18 SIGKILL trials), 45 full-hit signatures match and all
12 fresh full-field store reopens/24 footprint values pass.

## Sparse risk found and retained

A bounded component probe uses the same synthetic 100k-row index, CPU affinity
2, three fresh processes per source, 2,000 timed queries per case after 100
warmups. Source runtime hashes and identical diagnostic code are recorded. The
probe is excluded from shipping source. It exercises no I/O, original get,
hosted authority or provider call and is **not an end-to-end performance claim**.

| Case | Baseline p50 ns | Eager mask p50 ns |
| --- | ---: | ---: |
| Literal miss | 270 | 1,272 |
| First direct key | 361 | 1,372 |
| Scoped first key | 340 | 1,373 |

The eager allocation/popcount adds roughly one microsecond to these very fast
empty/singleton paths. This source is not recommended unchanged. A separately
measured refinement will keep zero/one unique candidate inline and allocate
the dense mask only for a second distinct row. Original measurements remain
frozen, including the regression. No AWS/deployment/public release/main merge
or hosted capacity/cost claim is included.
