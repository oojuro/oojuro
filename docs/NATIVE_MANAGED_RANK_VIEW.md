# Managed snapshot rank-view capability

This opt-in adapter exposes genuine managed-v3 records to a ranking/table owner
without constructing a Record2 log or treating a physical rewrite as append lineage.
It does not itself integrate the ranking pipeline, train a table, or create a query
cache. The snapshot writer's 100k original-ID/128MiB serialized-state bounds and
O(live) append cost remain; this is not the incremental/paged writer or scale gate.

## Authority and selection

`ManagedLog::with_rank_view(scope, as_of, filter, callback)` holds the current retained
control mutex through projection, the callback, and a final witness/data recheck.
The scope must match the enrolled writer and owner. Logical denial, a stale owner,
changed enrollment, an old data generation, or a stale retained witness refuses the
operation. Callbacks must not reenter the same control owner; they would wait on
their own guard.

The private constructor supplies original Records from the current managed snapshot:
IDs, ingest times, source generation, policy/eligibility and adapter remain intact.
The shared Record2 current-key kernel first chooses the greatest visible
`(valid_at, recorded_at, id)` per key, then applies indexed eligibility, lower time
bound, and filters. A stored-only or filtered-out current winner never revives an
older version. Unkeyed visible records remain independent, sorted by original ID.

## Derived capability

The view supplies a genuine current `Binding`, a keyed opaque context stamp,
selected immutable records, and generation-owned directory path.
`directory()` never creates a file/directory. Explicit table builders may call
`create_derived_directory()`; readers/loaders must not. Only the already-reviewed
managed registry names (`RANK_TABLE`, `LOCK`, and bounded random temporary names)
may be persisted there. Normal managed generation retirement removes those files.

`seal_derived` and `open_derived` use the actual managed data key without exposing
it. Their distinct AAD binds the exact authority/generation/control binding, AsOf,
Filter and a bounded static purpose. Each plaintext is at most 16MiB; encoded
ciphertext is bounded before decrypt. A change in purpose, selection context or
generation cannot open the prior cache frame. The keyed stamp does not publish
an unkeyed guessing oracle for private filter strings.

The capability and its borrowed Records cannot outlive the callback. A caller may
still copy returned authorized plaintext: subsequent logical denial cannot recall
that copy, a copied native key, or an external backup. No permanent destruction or
every-backup erasure claim follows from this adapter.

## Evidence

Focused tests exercise real managed append/prune/denial, original-ID/time projection,
winner-before-filter behavior, purpose/time/filter/generation binding, read-only
directory access and explicit-builder cache retirement. Four focused managed adapter tests and package strict lib+tests lint passed.
A full engine gate remains required because the existing View winner kernel was
extracted without changing its selection semantics.
