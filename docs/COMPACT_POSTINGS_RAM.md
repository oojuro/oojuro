# Compact postings: shipping review candidate

Runtime source `5197b8ca4dacecd1563b14c24e036eaf7cda2921`, based on frozen engine
`6cec2064aba3c4a6e96ebf587d0d1cad651e266a`. Only four runtime files change.
Private posting row handles narrow from usize to checked u32, with widening at
lookups; the current 100k-row limits and existing u32 disk codec are unchanged.
No score arithmetic/order, tokenization, topK, originals, durability or authority
check changes. No ignored profiler or owned-text move is included.

## Frozen evidence and tradeoff

[First-stage evidence](https://github.com/oojuro/oojuro/blob/4acd81cc7a58d9ce6c4c980056f5979b6633617b/docs/benchmarks/projection-ram-2026-10-23/compact-first/provenance.json)
was frozen before the ownership experiment.
[Final three-way matrix](https://github.com/oojuro/oojuro/blob/4acd81cc7a58d9ce6c4c980056f5979b6633617b/docs/benchmarks/projection-ram-2026-10-23/final/provenance.json)
and [full scope/report](https://github.com/oojuro/oojuro/blob/4acd81cc7a58d9ce6c4c980056f5979b6633617b/docs/PROJECTION_RAM_2026_10_23.md)
retain exact artifacts, all results and limits. MB is decimal. Three fresh
processes/stores per regime, same ArguAna 8,674 rows/15 dev queries/top100+originals;
warm OS cache, shared workstation, 3 GiB child limit/no swap.

| Regime | Peak RSS MB | Search+originals p50 ms | Allocated store MB |
| --- | ---: | ---: | ---: |
| Baseline `6cec206` | 93.229 | 58.823 | 3.539 |
| Recommended compact `7d81635` | 84.005 | 58.061 | 3.539 |
| Excluded compact+owned move `6610d0d` | 84.267 | 60.747 | 3.539 |

The recommended four files are byte-identical to the measured `7d81635` runtime
files. Posting capacity falls 20,846,592 → 10,423,296 bytes; peak RSS improves
9.9% on this corpus. All regimes return the same 1,036,320 original bytes/run,
nDCG@10 0.5036781582 and 45 identical full-hit signatures (IDs/score bits/clocks/
metadata/excerpts/originals). All 18 final stores reopen exactly; 99 files and
36 footprint values match independently. This is not a hosted RAM/capacity cap.

Latency is workload dependent: on the small 44-step history replay, baseline to
compact search+originals p50 was 5.274 → 6.028 ms while peak RSS fell
18.436 → 17.736 MB. Reported latencies are medians of three run statistics, with
small samples and order/host variation. No universal speedup is claimed. The
owned move has **no additional observed full-pipeline RAM gain** and is excluded.
Remaining RAM includes RecordLog/view state, originals, query/materialization,
codec/cache and allocator overhead; no disk-backed-index redesign is included.

## Compatible clean-source gates

- Portable release build passed from `5197b8c`; generic x86-64 target.
- Existing bitwise lexical/oracle/cache tests: 22 passed, two ignored.
- Existing record/projection/paged-cache tests: 100 passed, five ignored.
- [Clean-source harness gates](benchmarks/compact-postings-2026-10-23/gates.json):
  all seven passed over three runs, including nine SIGKILL recovery trials,
  exact originals/reopen, clocks, freshness, range order and deterministic ranking.
- `git diff --check` passed. Raw build/test logs are retained privately under
  `work/projection-ram/shipping-*.log`.

## Exact production-file SHA-256

| File | SHA-256 |
| --- | --- |
| `crates/oojuro-engine/src/lexical.rs` | `d8f924226e63f06a0e0c25e89e882fecab767f0427a1b32e122750a76c0caaaf` |
| `crates/oojuro-engine/src/lexical/prepared.rs` | `3776a3001b1856bb4e640fed1239eb92c1925a31b2a0c7f7035a37018948cabc` |
| `crates/oojuro-engine/src/lexical/record_cache.rs` | `b38cb3e52a1de38a32fa7ab4f9422ff365878ddebaf4d6147aaeeaff3f745e84` |
| `crates/oojuro-engine/src/lexical/segments.rs` | `20345fba33c699de0d11eb6ffc15cb7d2828e776cdffd9a8c0cd8618a105ed65` |

No o-cloud pin/deployment, production/AWS mutation, public package or main merge
is part of this PR. Qualification of the integrated hosted build remains separate.
