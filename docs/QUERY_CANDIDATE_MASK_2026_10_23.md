# Candidate union mask: measured work in progress

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
