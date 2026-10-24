# Request-local streaming projection refresh

Source-only candidate from current composition `4a9d99f`. No compiler, tests, workload or performance claim yet. The qualified current comparison remains mixed; this candidate is not included in that evidence.

The existing owned `Index::build` and `Index::refresh(Vec<Row>)` contracts remain. `begin_refresh(binding, exact_count)` consumes an old index, and each `push(RowRef)` validates the complete fresh row immediately. `push_owned(Row)` moves new/changed text without an additional copy. Borrowed and owned representations serialize the same fields in the same order. No input reference survives a push.

Matched rows retain only old ordinals after every metadata field and the entire fresh original matched in this invocation. New or changed rows own their incoming text. Exact count, names, duplicate keys/IDs, per-row serialization and total state bounds still apply. An error poisons the builder and drops old and fresh text; a partial builder cannot yield a searchable index.

Complete unchanged/growth refresh preserves old private ordinals and appends new IDs in incoming order. Changed same-view data remains a conflict. Replacement/deletion rebuilds in incoming order, cloning matched originals only after their full fresh comparison in the same invocation, then dropping the old index before rebuilding. No source reread, digest-only equality, across-request proof or authority cache is introduced.

Temporary maps/sets/ordinal markers are bounded by the existing 100k-record limit; their allocation is additional to the retained projection. Changed/new originals can still consume the full existing state budget. This is not a hard process RSS bound, and no peak or steady RAM saving has been measured.

Four new source tests cover exact JSON bytes/budgets, fresh-build score-bit/passage parity across growth/reorder/replacement/deletion/every field, error/count/same-view behavior, and direct internal retained-text/move/poison invariants. Existing refresh parity tests continue to exercise the owned API. Tests are unrun.

Native borrowed View callers and hosted streaming body authentication are separate future patches. Actual native CLI `record::Index` is a different path. Every caller must still establish fresh snapshot/authority and current EOF checks; this disposable builder cannot do so. No cache limit, format, dependency, scorer or storage change.
