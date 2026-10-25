# Disposable encrypted lexical index

`oojuro_engine::record::projection::disk` adds an experimental persistent
projection alongside the existing compatibility API. It is derived data: the
authoritative database retains every original and the index can be deleted and
rebuilt. It neither changes `.ojo` wire 0.1 nor converts historical stores.

`Builder::create(directory, cache_key, binding, limits)` accepts a private cache
directory, a disposable 32-byte cache key, the caller's snapshot `Binding`, and
explicit limits. `push(Row)` streams already-resolved live source rows;
`finish()` publishes the index only after all encrypted files are synced. IDs
must be unique. Raw keys may repeat across distinct caller-resolved source
scopes; `key:` lookup returns all supplied matching live rows. The existing
in-memory projection retains its original contracts and bounds.

`Index::open` checks the encrypted descriptor and numeric metadata. An immutable
`Index` supports concurrent `search(&Binding, &Query, fetch_original)` calls.
`rank` returns a bounded list of IDs first; `materialize` permits the caller to
fetch only those selected originals. Every original's complete text bytes and
metadata are compared before producing the existing exact excerpts. Ranking
uses one global plan/statistical model across the entire supplied snapshot,
with the same floating-point operation order as the existing lexical ranker.
It does not combine scores from independently ranked shards.

The caller must authenticate its current source snapshot, selection, permissions
and keys before reuse and again before delivering a response. `Binding` is a
caller assertion. A cache hit does not prove freshness, authorization, an
unchanged source prefix or permission to retain a customer key. Hosted callers
can use a process-owned disposable cache key without retaining customer keys.

The application-neutral facade is also exported at the crate root:

```text
EventBuilder::create(directory, cache_key, Binding, Limits)
EventBuilder::push(cursor: &[u8], timestamp: i64, data: &[u8]) -> Result<bool>
EventBuilder::finish() -> Result<EventIndex>
EventIndex::search(&Binding, &EventQuery, fetch: FnMut(&[u8]) -> Result<EventRow>)
EventIndex::rank / EventIndex::materialize
```

`EventRow` has only `timestamp` and `data`. Timestamps are signed Unix
microseconds assigned by the source engine at first acceptance; retries retain
the source's original timestamp and cursor. Cursors are opaque 1–512-byte
identities and do not derive ordering from a wall clock. `EventQuery::new(text)`
provides inclusive `from`/`to` timestamp bounds, a result `limit`, excerpt and
serialized output budgets. A nonUTF8 original returns `false` from `push`, so it
is omitted from this text index; its exact bytes remain readable in the
authoritative database. No agent, source, severity or metadata map is required.

Numeric query metadata remains in RAM; exact originals and complete source row
populations do not. The current numeric representation uses 112 bytes per row
on the qualified x86-64 compiler, plus directory/manifest metadata and the
reserved decoded page cache. `estimated_heap_bytes` includes the full 4 MiB
decoded-page policy before warming and an additional 4 MiB allowance for small
allocation bookkeeping. This is an admission estimate, not a hard process RSS
limit. Builder analysis vocabulary and run sorting have separate bounded
working sets; measure process peak RSS when choosing a build reservation.

`Limits.index_bytes` bounds logical bytes of the new generation's entire build,
including encrypted text, row spool, sort queues/runs, tree packs, numeric
columns and the descriptor. Each extending write reserves its bytes first;
successful deletion credits temporary files. Old generations remain charged to
their owner. `build_metrics()` reports peak reserved logical bytes, observed
allocated bytes at close/removal/phase boundaries, and peak tracked file count.
Allocation observations are not continuous sampling or a filesystem quota.

`index_bytes()` reports retained apparent file bytes;
`allocated_index_bytes()` reports retained Linux allocation blocks and
directories. Filesystem preallocation, allocation blocks, directory growth and
other generations require additional reservation. `BuildMetrics::reservation`
helps add explicit per-file and directory slack to a configured logical limit.
A hard allocated-byte promise requires a filesystem quota or separately bounded
volume. There is no arbitrary-filesystem hard quota claim in this API.

Raw and Zstd representations are selectable for the encrypted cache. Both use
the same query model and exact-original checks. Zstd frames are independent,
bounded and require no external dictionary. Compression changes derived cache
files, never source bytes or source durability.

Current qualification covers exact legacy ranking across raw/Zstd, filter/time
bounds, excerpts, reopen, duplicate raw keys, wrong keys/bindings, corrupted
files, failed/over-budget builds, ten simultaneous immutable-handle queries,
and the neutral event adapter. The source currently rebuilds on a changed
snapshot; incremental refresh, million-row resource/latency acceptance and
hosted end-to-end qualification remain open. No 1M, 250 ms or public service SLA
claim follows from these functional checks.

The opt-in `profile_disk_projection` example reports build, reopen and concurrent
synthetic query diagnostics. Its callback regenerates exact fixture rows rather
than reading a `.ojo` file or provider, and its initial queries warm pages. Keep
those measurements separate from event-log, durable storage and hosted results.
