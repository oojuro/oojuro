# Native borrowed-row projection comparison

Status: all four rounds and eight full copied-store reopens passed. Parent qualification completed: raw rankings, quality, originals, metrics and cleanup checked. The offline expected-data/inventory helpers are shared; this is not a second independently implemented oracle.

The candidate combines consuming projection refresh (`9190cb7`) with borrowed native record pages and winner selection (`4c2331f`). Baseline is the retained current-native build (`4a9d99f`), including the previously qualified ownership changes. No new dependency, storage format, ranker or authorization cache.

## Median of four rounds

| Metric | Before | After | Change |
|---|---:|---:|---:|
| Search p50 | 147.705 ms | 90.438 ms | -38.77% |
| Search p99 | 197.638 ms | 131.084 ms | -33.67% |
| Search + originals p50 | 148.321 ms | 90.984 ms | -38.66% |
| Search + originals p99 | 198.347 ms | 131.728 ms | -33.59% |
| Append p50 | 1.397 ms | 1.367 ms | -2.18% |
| Append p99 | 4.194 ms | 4.138 ms | -1.32% |
| Peak RSS | 148.051 MiB | 124.207 MiB | -16.11% |
| Peak charged cgroup memory | 146.016 MiB | 122.520 MiB | -16.09% |
| Steady RSS | 70.695 MiB | 70.766 MiB | +0.10% |
| Steady anonymous memory | 65.293 MiB | 65.297 MiB | +0.01% |

All four search p50/p99 and peak-memory pairs improved. Steady memory is essentially unchanged. Closed allocated storage was 2,760,704 bytes for both medians; tiny apparent-size differences are randomized ciphertext variation. Charged cgroup memory and RSS are distinct measurements.

## Every round

| Round/order | Search p50 before→after (ms) | Search p99 before→after (ms) | Append p99 before→after (ms) | Peak RSS before→after (MiB) |
|---|---:|---:|---:|---:|
| 0/AB | 140.505→89.949 | 188.484→130.845 | 9.500→4.369 | 148.043→124.320 |
| 1/BA | 143.820→91.794 | 196.774→130.024 | 6.468→4.042 | 148.059→124.348 |
| 2/AB | 151.590→90.125 | 204.738→131.324 | 1.774→4.235 | 148.109→123.945 |
| 3/BA | 151.891→90.751 | 198.502→131.780 | 1.920→1.743 | 147.699→124.094 |

The third append-tail pair regressed (1.774→4.235 ms). These are separate round percentiles, not pooled p99s or a universal latency improvement.

## Correctness and scope

Same 695,520,625-byte public SWE input, first ten dev trajectories, 20,000 background records, fixed 201-query schedule per run: 804 timed queries per arm. All eight saved pools (52 labelled lists each) match the reference byte for byte, including IDs and score bits; quality was independently recomputed. This does not retain or prove 804 separate ranked lists. Existing per-query freshness, future-time, byte and original-return guards also passed.

All eight copied stores reopened with all 20,623 complete Docs and originals (14,716,773 raw text bytes each); source byte/mode inventories remained unchanged. All 21 recorded PID/start identities, 16 inner scope cgroups and the outer cgroup were absent after completion. Stores and evidence remain retained.

Warm, local, fits-RAM, optimized native record benchmark only. This compares the shared refresh and borrowed caller together. It does not establish a hosted or different CLI `record::Index` gain, cold-cache or larger-than-RAM behavior, general write/storage gain, cost savings or SOTA. Earlier mixed/adverse current-vs-84 evidence remains valid; no rerun or retuning occurred.

## Source and reproduction

Measured runtime: `4c2331fd7c9c16e0eb2e864786a252d59b83cd6b`, including shared consuming refresh `9190cb737865eea3c50de79e5d1bc8b31d461be2`; retained current-native control: `4a9d99fd2ec9b8eaf8507cad9e33b7d5b98cad4a`. Candidate optimized benchmark SHA-256: `bcf133459366fa5d75b44a32f395c72da70fbacff408a610111f66fedb2a55a1`; control: `c129bfb3df95c626371486a69f77ade9ab39963f8f5b79aaa2c5306db1635f6b`.

The candidate's normal engine, benchmark and both libtests were fresh builds from an isolated target. Eight affected core/caller checks passed, retaining the earlier shared-core qualification of twelve checks and one existing ignored measurement. No benchmark rerun occurred. This public branch retains PR91's earlier documentation and merges the measured source byte for byte; no binary was rebuilt from that documentation merge.

[Evidence manifest](benchmarks/native-borrowed-projection-2026-10-24/manifest.json), [archive and reproduction notes](benchmarks/native-borrowed-projection-2026-10-24/README.md).
