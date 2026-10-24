# Explicit disk-backed Record2 work

This is the generic embedded database path. It does not change o-cloud's
`record::projection::Index` consumer, implement SQL compatibility, add agent
capture, or implement a standalone `.ojo` container. The authority remains a
sealed Record2 directory. The original `RecordLog`, `Reader::open`, and
`Reader::open_disk` retain their 100,000-record / 128 MiB admission limits.

The opt-in API takes a trusted disk admission policy:

```rust,ignore
use oojuro_engine::record::paged::{DiskLimits, Index, Writer};

let limits = DiskLimits::new(1_000_000, 1 << 30, 1 << 30, 4096)?;
let mut writer = Writer::open_disk(
    source, key, identity, locator_directory, minimum_watermark, limits,
)?;
let mut index = Index::open_disk(writer.reader()?, lexical_directory)?;
let receipt = writer.append_indexed(&mut index, request_id, inputs)?;
```

A policy is an admission limit, not a performance guarantee or a total directory
quota. Source bytes count encoded header and authority batches. Index bytes
count active encoded and accounted decoded posting segments and visibility
pages. Metadata trees, WAL/frame overhead, dictionary files, obsolete COW packs,
interrupted publication files and maintenance temporaries are additional disk
usage. The scale diagnostic inventories **all** files before and after the
client writes. `Writer::compact_directory` and `Index::merge` are explicit
maintenance; their time and temporary space must be measured separately.

A write constructs the same IDs, monotonic engine clocks, fingerprints,
commitments, retry receipts and sealed authority bytes as `RecordLog`. It syncs
one authority batch and publishes prefix, request, record and key-version trees.
The tree editor accepts a bounded batch and rewrites each affected path once.
The writer retains neither every original nor every request receipt in RAM.

`append` acknowledges authority and locator publication. An older search index
then fails its current-prefix check until `refresh` succeeds.
`append_indexed` returns only after the derived lexical index is current. A
cache failure can occur after the authority committed: retain the request ID
and exact inputs, reopen, and retry. An uncertain authority/directory failure
poisons that writer; it cannot supply a readable snapshot or acknowledge a
second write before reopen. Replaying the same request returns the original
receipt; changed inputs conflict. There is no rollback of committed originals.

The source chain is fully verified at open. Store's owned, sealed sequence slots
are immutable through this API. An ancestry-cache miss therefore authenticates
that slot's already-verified watermark rather than replaying genesis through the
slot. The ingest clock remains engine assigned. Caller event time never replaces
it, and old snapshots remain bound to their original prefix and two clocks.

`Reader::get_ids` validates one snapshot, accepts at most 1000 IDs and an output
byte budget, and decodes each selected authority batch once within that request.
It preserves order, duplicates and missing IDs. Selected search hits use the
same bounded grouping. `read_ordinals` supplies bounded ID-order original
windows for full integrity checks without repeatedly sorting/scanning the whole
project. These APIs retain existing scope and exact-original semantics.

New disk-index references can carry independently sealed visibility side pages.
They retain valid/known clocks, key, eligibility and typed filter fields, without
original text or attributes. Full cache open compares every side-page field to
independently verified authority. Word-query statistics and scores walk those
pages and immutable postings; selected hits read exact originals. Literal
verification additionally reads possible matching originals. Old posting-only
references retain the exhaustive compatibility path. Disk descriptors distinguish
this disposable representation with version 2; old representations still load,
and a reader that does not understand a derived representation must rebuild it
from authority. Authority bytes and Record2 format stay unchanged.

Each authority append remains at most 1000 records / 32 MiB. Each lexical segment
and visibility page has a separate 16 MiB encoded/decoded bound. The metadata
page cache and source decode cache retain their existing bounds. Per-operation
working space also includes the selected batch, one segment/visibility page and
bounded top-k/output; those bounds are not a whole-process RSS guarantee when
several clients operate concurrently.

Qualification includes exact originals, scoped/past snapshots, both clocks,
key-version winners, typed filters, eligibility, literal score bits, merge,
reopen, missing/corrupt cache, lost acknowledgements, same-origin forks and more
than 64 ancestry generations. The actual nonlegacy writer is killed at Store,
COW page/root, visibility page, segment and descriptor publication boundaries.
A source change and these correctness checks do not establish the one-million
record / ten-client latency target. Exhaustive posting/metadata work remains a
scale cost to measure; no SLA or hosted latency result follows from this API.

## Bounded group commit

`Writer::append_group` and `append_group_indexed` accept independent borrowed
`Append { request, inputs }` entries. Caller order assigns new request sequences,
IDs and monotonic engine clocks. Repeated identities retain their first exact
receipt, even when retries are reordered or regrouped. Changed inputs conflict.
Validation, conflict and capacity checks for the entire submitted group finish
before any new authority append.

A group accepts at most 1000 requests, 1000 submitted input rows (including
retries), and 32 MiB total new encoded authority batches. It adds no collection
wait: a queueing caller must separately bound its queue, group size and wait.
One synced Store append contains the unchanged Record2 batches, followed by one
COW directory publication. `append_group_indexed` additionally requires current
lexical publication before returning any receipts. Search probes belong outside
the ACK timer when measuring ACK latency; their queueing effects still belong in
a concurrent workload. Unknown outcomes retain each request identity for reopen
and exact retry; the group API does not promise a transactional rollback.

The group test matrix checks duplicate and previous receipts, regrouped retries,
per-request prefixes, order/clocks, all-or-reject admission failures, format
compatibility with `RecordLog`, and SIGKILL across the actual grouped Store,
directory, visibility, segment and searchable descriptor publication. Direct and
grouped performance must be reported separately. A grouped native run is not an
equivalent comparison to SQLite/JSONL runs that sync every request separately.
