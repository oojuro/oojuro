# Candidate row union: review source and measured limits

Runtime-only pin `c2431fc4d3f979e5e38b7040e6999d64989ecff9` contains two runtime/
test files atop signed PR65 `c42c10d`. It is byte-identical across `crates/`,
Cargo.toml and Cargo.lock to measured source
`774e39ba5cac89a0fc2af530e93d20085b2279db`; only commit metadata/evidence differ.
Both source commits carry DCO signoffs. No ignored diagnostic enters this tree.

Candidate union uses an inline zero/one-row state, then a u64-word mask for
multiple distinct rows. The mask is bounded to 12,504 bytes at the existing
100k-row limit; its output row vector reserves exact candidate count. Set bits
retain canonical ascending order. Scoring arithmetic, final tie comparator,
visibility, formats, originals, clocks and durability are unchanged. No new
dependency, provider behavior, persistent index or retained cache is added.

## Frozen evidence

- [Full report and first/final regimes](https://github.com/oojuro/oojuro/blob/9688890be1233f2287edb7f9159b05320817d11d/docs/QUERY_CANDIDATE_MASK_2026_10_23.md)
- [First raw matrix and sparse regression](https://github.com/oojuro/oojuro/blob/9688890be1233f2287edb7f9159b05320817d11d/docs/benchmarks/query-mask-2026-10-23/first/provenance.json)
- [Final raw matrix, samples, build/test logs and exact hashes](https://github.com/oojuro/oojuro/blob/9688890be1233f2287edb7f9159b05320817d11d/docs/benchmarks/query-mask-2026-10-23/final/provenance.json)

Same frozen public ArguAna 8,674 rows, 15 dev queries/top100 + originals, three
fresh processes per source, warm OS cache, generic x86-64 workstation build,
3 GiB/no-swap child scope. Final search+original p50 is **66.385 → 60.531 ms**
(8.8% lower in this matched run); peak RSS **82.141 → 82.100 MB** is essentially
unchanged. The earlier variant's two orders improve medians but have mixed
sample tails. No hosted, universal speed, memory-capacity or cost claim follows.

History p50 remains **6.161 → 6.158 ms**, durable append p50 **1.288 → 1.287 ms**.
Its sample search p99 worsens **23.499 → 24.686 ms**, append p99
**1.507 → 2.289 ms**. Only 44 history steps/run and 15 ArguAna queries/run:
statistically useful p95/p99, write-tail and scale qualification remain
acceptance gaps before production selection. No write/storage win is claimed.

The first eager mask added about 1 µs to empty/singleton component paths at
100k rows. The final inline state fixes this: baseline→final p50 literal miss
261→260 ns, direct first key 341→320 ns, scoped first key 340→321 ns. These are
component-only results, 2,000 samples/case after 100 warmups ×three processes,
same CPU2; exact signatures match. They are not extrapolated to end-to-end gains.
Sparse multirow cases may pay the bounded mask scratch overhead.

## Compatible qualification and source identity

- Lexical oracle/cache/boundary tests: 23 passed, two ignored; record/recovery
  tests: 100 passed, five ignored; portable release benchmark/profile build passed.
- All seven correctness gates pass both sources ×three runs, including 18
  SIGKILL trials. Forty-five full-hit signatures match IDs, exact score bits,
  continuation, clocks, metadata, excerpts and original checks.
- Twelve independent all-field store reopens/24 footprint checks pass. Quality,
  original byte work and storage format are unchanged. Actual timestamp/
  compression/allocation variation is retained in the raw results.
- Source identity and `git diff --check` pass; no repeat solely for cherry-pick
  metadata was used to claim extra independent test runs.

| File | SHA-256 |
| --- | --- |
| `crates/oojuro-engine/src/lexical/prepared.rs` | `237371f32e881bd1a3e87ba9bf9be444306551c1926c0da7756b8edac4a53786` |
| `crates/oojuro-engine/src/lexical/tests.rs` | `5364a4f3eada21b72504fee5b1e3f64c178bb2a5cff4488b30950e9aaa5179ae` |

Candidate only; no AWS mutation, deployment, main merge or public release.
Replay-cache release and top-K materialization experiments remain excluded.
Many-journal/concurrency, histories above RAM and attachment qualifications are
separate workstreams. Integration must retain the existing hosted authority,
revocation, deletion, exact payload read and audit checks.
