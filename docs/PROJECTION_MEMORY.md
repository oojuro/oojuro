# Disposable projection memory accounting

`record::projection::Index::estimated_heap_bytes()` estimates retained heap allocation for caller-owned aggregate cache admission. `row_count()` and `text_bytes()` continue to report source data, independently of allocation.

The estimate counts string and vector **capacity**, including source IDs/metadata, duplicate ID lookup strings, lexical carrier originals, normalized arguments, documents and postings. It adds a policy charge for opaque standard collection structure: 1,024 bytes for a nonempty postings tree, 256 bytes per tree entry, and hash capacity times the key/value size plus 32 bytes. The tree/table charge is an estimate, because Rust exposes neither allocator size nor private collection layout.

The estimate excludes inline `Index` storage, allocator headers/fragmentation, transient build/query/view work, authority state, runtime memory and OS page cache. Callers must add cache-entry/container backing storage and must budget in-flight work separately. It is **not a hard process RSS cap**. Cache eviction changes reuse only; current authorization, revocation/deletion, payload-key checks and exact-original delivery still belong to the hosted caller.

The fixed synthetic release probe and meaningful behavior checks are recorded in [the qualification report](projection-allocation-budget-2026-10-23.json). 4,096 high-vocabulary rows contain 1.46 MB of source text but receive a 42.31 MB retained-heap estimate. This is why text-only cache accounting can miss substantial postings/metadata cost; it does not establish production memory or hosted capacity.

Reproduce the opt-in probe:

```sh
cargo test --locked --release -p oojuro-engine --lib projection_allocation_measurement -- --ignored --nocapture
```

This API changes no storage format, compression codec, ranking algorithm, query result or dependency. No projection is retained merely by registering an idle journal.
