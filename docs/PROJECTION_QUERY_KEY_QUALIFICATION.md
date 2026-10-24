# Cached projection query-key qualification — 2026-10-24

`record::projection::Index::search` previously cloned every source key into a
temporary `BTreeMap` for every query. The shared lexical scorer consults only
one key: the complete raw query, or the suffix of an explicit `key:` query.
The candidate resolves that one candidate from the caller-resolved unique rows.
It preserves the existing eligibility mask, time bounds, ranking and excerpts.

On this bounded synthetic local profile, 100k-row unique-term p95 changed from
20.6–26.6 ms to 6.86–7.11 ms; an early bare-key lookup changed from
16.3–22.2 ms to 1.76–1.79 ms. Retained heap is unchanged. Common-term results
are mixed, and adverse build/refresh controls are retained below. These results
cover an already-built local projection, not source replay, Store I/O, Journal,
Cloud latency, concurrency or the million-record target.

## Semantic review and correctness

The only projected key lookup is in `lexical/prepared.rs`. Literal queries
skip direct-key lookup. An explicit missing key has no whole-query fallback;
`key:key:value` strips one prefix. Bare keys, raw Unicode, whitespace and case
keep their existing exact-match interpretation. Candidate lookup considers
unfiltered source keys before the unchanged eligibility mask: filtering keys
early could incorrectly introduce lexical fallback for an excluded direct key.
`Index::build`/`refresh` still refuse duplicate keys.

The new 90-case oracle compares public search with the previous complete-key
map across ten query forms, three kind selections and three time windows,
including a reversed window. It checks opaque IDs, exact score bits,
continuation/time fields and original excerpt slices. Existing tests cover the
Nine oracle, UTF-8 passages, stored-only rows, filter-before-top-k behavior,
binding/byte limits, refresh growth/reorder/replacement/removal, every-field
view changes, and metadata spare-capacity accounting.

Local checks passed on the runtime change:

```sh
cargo test --locked --offline --no-default-features -p oojuro-engine --lib record::projection:: -- --test-threads=1
# 9 passed, 1 existing ignored explicit allocation-measurement test
cargo test --locked --offline --no-default-features -p oojuro-nine --lib -- --test-threads=1
# 94 passed
```

No public signature, dependency, persisted format, source replay, authority,
binding or admission limit changes. `Binding` remains a caller assertion;
consumers must establish current source/authorization state themselves.
GitHub Actions is disabled; these are local checks, not remote CI results.

## Profile and provenance

The public helper is
[`profile_projection_keys.rs`](../crates/oojuro-engine/examples/profile_projection_keys.rs).
It uses only public projection APIs and deterministic invented rows. Each
source runs twice per size in baseline/candidate/candidate/baseline order, with
one warm-up and 20 timed calls per query. Percentiles use nearest rank. Twelve
cases cover unique/common/missing lexical terms; early, late, nested, absent,
stored-only and Unicode keys; a literal phrase; an identifier anchor; and a
query combining all filter fields and a time window.

Each sample verifies a signature of opaque IDs, exact f64 score bits,
continuation, both time fields, excerpt offsets and original bytes. The helper
also checks UTF-8 boundaries. Across all 12 profiles, every query signature,
post-refresh signature, text count and estimated retained heap value matched.
At 1k/10k, refresh appends 32 rows and reverses input order; at the 100k admission
ceiling it replaces one row's text and reverses input order. The old binding
is refused after refresh.

Both artifacts were built from separate fresh target directories, frozen and
SHA-256 checked. The actual baseline tracked tree is unchanged `128b76f`; only
the identical public example was copied into that detached worktree. Runtime
change `419cf4a` and helper percentile correction `e01c036` compose the candidate.

| Artifact | Source | SHA-256 |
| --- | --- | --- |
| Baseline | `128b76f8a462817910ed5ca622adf156ed1efcc6` | `de6a43c8dd6d6566ea01b7eb4b5918aca55698c62ec4f96cfca5729c039c26a4` |
| Candidate | `e01c03625408993cfe88ad1805b05ad0ca797a69` | `97c7c2d4d59a3e5b85ac132070ba8723c1aebe4a8dcad4c9e9907945be0a9bec` |

Rust/Cargo 1.99.0; release `--no-default-features`, `RUSTFLAGS=-C target-cpu=x86-64`,
`CFLAGS=-march=x86-64 -mtune=generic`; Linux 7.2.11 on an Intel Core Ultra 7
258V with eight logical CPUs. This shared laptop uses frequency scaling and no
CPU pinning. A cooperating exclusive benchmark lock separated this campaign
from other team builds/timings. The complete sorted samples, signatures,
harness/lock hashes and provenance are in the
[machine-readable receipt](benchmarks/PROJECTION_QUERY_KEY_2026_10_24.json).

## Query results

Each range spans the two per-process p95 observations in milliseconds; it is
not a pooled percentile or a confidence interval. All twelve cases and their
samples remain in the receipt.

| Rows | Query | Baseline p95, ms | Candidate p95, ms |
| ---: | --- | ---: | ---: |
| 1,000 | `lexical_unique` | 0.111–0.125 | 0.060–0.064 |
| 1,000 | `bare_key_first` | 0.092–0.095 | 0.018–0.019 |
| 1,000 | `explicit_key_last` | 0.100–0.109 | 0.021–0.051 |
| 1,000 | `lexical_common` | 0.320–0.395 | 0.273–0.278 |
| 1,000 | `literal_phrase` | 0.374–0.408 | 0.281–0.294 |
| 1,000 | `filtered_common` | 0.094–0.097 | 0.018–0.022 |
| 10,000 | `lexical_unique` | 1.175–1.370 | 0.519–0.522 |
| 10,000 | `bare_key_first` | 0.820–0.896 | 0.091–0.093 |
| 10,000 | `explicit_key_last` | 0.819–0.958 | 0.202–0.207 |
| 10,000 | `lexical_common` | 3.330–3.810 | 2.391–2.669 |
| 10,000 | `literal_phrase` | 4.116–4.549 | 3.278–3.625 |
| 10,000 | `filtered_common` | 1.034–1.038 | 0.234–0.246 |
| 100,000 | `lexical_unique` | 20.597–26.599 | 6.861–7.108 |
| 100,000 | `bare_key_first` | 16.311–22.168 | 1.760–1.787 |
| 100,000 | `explicit_key_last` | 16.547–21.070 | 4.001–4.154 |
| 100,000 | `lexical_common` | 44.199–60.893 | 40.416–52.330 |
| 100,000 | `literal_phrase` | 51.251–60.200 | 43.508–49.715 |
| 100,000 | `filtered_common` | 21.832–25.421 | 6.370–7.244 |

The second 100k candidate common-term trial is slower than the final baseline
trial (52.33 versus 44.20 ms). This small campaign supports removing avoidable
key-map construction; it does not establish a universal search speedup.
Visibility-mask construction and source-key scanning remain O(N).

## Memory and adverse controls

Retained heap before/after queries and after refresh is exactly equal between
sources. The estimator includes source metadata capacities but excludes query
temporaries, allocator overhead and process RSS. Eliminating temporary key
clones is not a retained-memory reduction.

| Rows | Original text bytes | Estimated retained heap bytes, both sources |
| ---: | ---: | ---: |
| 1,000 | 75,044 | 1,339,213 |
| 10,000 | 760,346 | 15,053,167 |
| 100,000 | 7,703,324 | 141,780,270 |

The 100k retained estimate is about 135.2 MiB. The separate 128 MiB serialized
row admission limit is not a heap bound. Computing the estimate is itself a
scan: 100k p95 is 4.59–5.52 ms baseline and 5.43–5.55 ms candidate.

Build and refresh each have one observation per process, two per source/size;
these are descriptive controls, not p95 measurements. At 100k, baseline build
was 329–386 ms versus 394–447 ms candidate; replacement refresh was 312–360 ms
versus 335–518 ms candidate. Build/refresh code is untouched. These adverse
observations remain in the receipt, and no build/refresh improvement is claimed.
At 10k, append/reorder refresh was 7.40–8.00 ms versus 7.48–8.19 ms.

## Reproduce from immutable source

Use an installed compatible Rust/C toolchain and cached dependencies for
`--offline`; omit that flag only if intentionally allowing dependency fetches.
The copied example is the same in both sources. Target directories and frozen
binaries must remain separate.

```sh
bench_dir="$(mktemp -d)"
git worktree add --detach "$bench_dir/baseline" 128b76f8a462817910ed5ca622adf156ed1efcc6
git worktree add --detach "$bench_dir/candidate" e01c03625408993cfe88ad1805b05ad0ca797a69
cp "$bench_dir/candidate/crates/oojuro-engine/examples/profile_projection_keys.rs" \
  "$bench_dir/baseline/crates/oojuro-engine/examples/profile_projection_keys.rs"
for arm in baseline candidate; do
  CARGO_TARGET_DIR="$bench_dir/target-$arm" \
    RUSTFLAGS='-C target-cpu=x86-64' CFLAGS='-march=x86-64 -mtune=generic' \
    cargo build --manifest-path "$bench_dir/$arm/Cargo.toml" \
      --locked --offline --no-default-features --release \
      -p oojuro-engine --example profile_projection_keys
  cp "$bench_dir/target-$arm/release/examples/profile_projection_keys" "$bench_dir/$arm-profile"
done
python3 scripts/bench-projection-query-key.py \
  --baseline "$bench_dir/baseline-profile" --candidate "$bench_dir/candidate-profile" \
  --output "$bench_dir/receipt.json"
python3 scripts/bench-projection-query-key.py \
  --verify-report docs/benchmarks/PROJECTION_QUERY_KEY_2026_10_24.json
```

The wrapper refuses matching binary hashes and mismatched result/heap/refresh
signatures. Offline verification and a deliberately altered-signature rejection
were checked independently. Raw samples contain no real records or credentials.
