# Experimental top-K materialization: no default recommendation

Signed source `ebd94ba13d56e0d7aa3af57859a11c536ef31f32`, based on signed PR65
allocation-slack baseline `c42c10d`; replay-cache release is excluded. The scorer
retains row/score pairs, selects/sorts the exact top-K using the existing full
score/time/textual-ID comparator, then builds final Hit structs. Scoring,
pruning arithmetic, eligibility, formats, durability and returned work do not
change. This is **experimental only** because the matched fixtures show no
compelling full-pipeline RAM/latency gain to justify the extra runtime steps.

[Frozen raw matrices, tools and hashes](benchmarks/query-topk-2026-10-23/provenance.json)
preserve both measurement orders, without replacing the first result. Same
frozen public ArguAna 8,674 rows/15 dev queries/top100 plus originals, three
fresh processes/stores per source and order, warm OS cache, generic x86-64,
shared workstation, 3 GiB/no-swap child scope. No known build/test overlap.

| Order | Source | Peak RSS MB | Search+original p50 ms | Sample p99 ms |
| --- | --- | ---: | ---: | ---: |
| A/B | Baseline | 82.293 | 63.399 | 459.974 |
| A/B | Top-K | 82.231 | 68.211 | 461.456 |
| B/A | Top-K | 82.063 | 59.129 | 403.101 |
| B/A | Baseline | 82.264 | 59.160 | 414.751 |

Median of three per-run statistics; p99 has only 15 query samples/run. RSS
ranges overlap. The first search median is slower and the reverse is essentially
equal, so neither a speedup nor a useful full-pipeline RAM saving is established.
The 44-step history run similarly reports baseline→candidate 6.142→6.465 ms
search+original p50 and 1.213→1.265 ms durable append p50, with overlapping
small-sample timing ranges. No write or storage improvement is claimed.

## Qualification

- Existing lexical/oracle/cache tests plus new focused top-K test: 23 passed,
  two ignored. Record/projection/recovery tests: 100 passed, five ignored.
- The new test compares selected prefixes with complete order for decimal and
  opaque ID/time ties, zero/small/exact/over-count K, public invalid K, and
  injected internal NaN/Inf statistics retaining `total_cmp` behavior. Public
  inputs provide no raw scores. Existing independent oracle tests verify actual
  score bits, visibility, phrases, identifier continuation and ordering.
- Portable release benchmark/profile build passed; `git diff --check` passed.
- All seven harness gates pass both sources across three runs, including
  18 SIGKILL recovery trials. Forty-five full-hit signatures match across exact
  score bits, continuation, clocks, metadata, UTF-8 excerpts and original bytes.
- Twelve independent full-field reopens, 66 file inventories and 24 complete
  apparent/allocated footprint values match scorecards. Allocation rounding and
  compressed timestamps vary; no format/storage saving is established.
- Quality and returned original work remain identical. ArguAna nDCG@10 is
  0.5036781581683427, original bytes/run 1,036,320; history nDCG@10 is
  0.7477066261975289, bytes/run 828,931.

The candidate is preserved separately. Subsequent work starts from PR65,
excluding both top-K and replay-cache experiments. No hosted/provider change,
AWS operation, public release, main merge or production claim is included.
