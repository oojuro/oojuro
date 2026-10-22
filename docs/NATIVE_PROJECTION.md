# Native externally resolved query projection

`record::projection::Index` is a pure, bounded in-memory lexical projection of an immutable row set supplied by a trusted caller. It writes no files and owns no Store, WAL, ingestion clock, DEK, local durability receipt or remote authorization adapter. It is deliberately separate from RecordLog: using RecordLog::append to materialize remote committed rows would assign new IDs/times and create a second journal.

## API and trust

- `Binding { namespace: [u8;32], writer: String, view: [u8;32] }` is opaque caller metadata, **not** proof of commit, freshness, authorization or provenance. `Index::build(binding, Vec<Row>)` validates bounds/uniqueness and tokenizes that immutable snapshot. `search(&binding, &Query)` refuses a different binding.
- Each Row keeps its original opaque string ID, optional already-resolved unique key, event/valid time, reported ingestion-time metadata, text, initial eligibility and typed filtering fields. `reported_recorded_at` is imported data; no engine clock or historical-as-of authority is inferred from it. The caller resolves live versions/tombstones first. Duplicate IDs or keys fail; this module never picks a replacement using event time.
- The caller must independently verify its authoritative prefix, grants, lifecycle and key availability before every reuse and before delivering response bytes. A warm projection is never a fallback for any failed verification. It cannot verify those external systems itself.
- Full original provenance stays with the caller's original records. The engine does not manufacture harness receipts, source consent, causal links or a flat Record durability claim from these imported rows.

## Scoring and identity

Private dense ordinals are posting handles only. The original full string IDs participate in F4 **before top-k truncation**, including the anchored and continuation passes. Mapping numeric winners back to opaque IDs after top-k would choose the wrong original records on ties. Existing v1 capture and local Record paths pass no original-ID override and retain their existing numeric-as-full-decimal rule and score bits.

Time and typed filtering precede identifier anchoring, candidate selection, F3 whole-view background statistics and exact pruning. This is a nonempty content-search API: initially unindexed rows cannot reenter by direct key or punctuation-only literal scan. Its caller can use its separate authorized get/timeline APIs for stored-only data. There is no deduplication of distinct logical IDs merely because texts match.

Hits include original ID, bits, continued flag, original valid/reported times and an exact original UTF-8 passage range. Passage selection is ported from the old reader and checked against it using a dev-only oracle. It never generates text; `text[start..end] == excerpt`. No overlap falls back to a bounded prefix and implies no relevance or truth claim.

## Bounds and boundaries

Limits reuse the bounded native slice: 100k rows / 128 MiB serialized row metadata, 16 MiB text, 32 MiB serialized row, original ID/key at most 1024 bytes; 64 KiB query, k <= 10k, passage 1..65536 bytes, exact serialized output <=64 MiB. Logical byte bounds are not exact RSS or hard wall-clock limits. A failed build produces no usable partial index. Snapshot rows are immutable; build a new index for another view.

No persistent external projection cache, remote commit driver, authorization service, cross-writer score calibration, ranker integration, historical memory reconstruction or full-SPEC corpus/scale acceptance is provided. The engine's existing Record cache and authority formats are unchanged. No Nine/Trace dependency or import is added to the engine's normal production graph.

## Evidence

The current engine gate passes 80 tests with 4 existing opt-in measurements/helpers ignored; strict selected engine lib/tests clippy passes. Eight new checks cover the original opaque-ID top-k order with reversed input, 1200 candidates, F1/F2/F3/F4 and f64 bitwise dev-only direct-Nine parity, typed filter-before-floor behavior, duplicate binding/key/ID and output refusal, stored-only eligibility and exact Unicode passage ranges/differentials. Old capture/Record/receipt/cache cases remain in that engine run. These are bounded synthetic tests, not a launch corpus, throughput, multi-host durability or provider acceptance result.
