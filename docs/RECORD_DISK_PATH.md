# Explicit disk-backed Record2 work

This is the retained `oojuro-engine::record` directory/lexical path for existing
consumers. The minimal standalone byte engine is [crates/ojo](../crates/ojo/README.md)
with its independent experimental wire 0.1 format. Measurements and limits here
do not describe that file engine. This change adds no SQL compatibility or agent
capture and does not migrate o-cloud's `record::projection::Index` consumer.
Record2 authority remains a sealed directory. The original `RecordLog`,
`Reader::open`, and `Reader::open_disk` retain their 100,000-record / 128 MiB
admission limits.

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
Input validation, per-request conflicts and authority admission bounds for the
entire submitted group finish before any new authority append. Derived index
publication can still fail afterward, as described above.

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

## Request-local search profiling

`Index::search_profiled` returns the same search result plus phase times and work
counts for that one authorized request. It separates the term-frequency pass,
visible-term presence, statistics, scoring and selected-original reads. Byte
counts describe encrypted segment/visibility bytes processed, not physical disk
I/O. Corpus row visits, fallback authority visits and logical original requests
are distinct; original requests can decode an entire authority batch internally.
The ordinary search entrypoint does not collect phase clocks.

The `profile_record_search` example opens an existing synthetic `profile_disk`
fixture and probes broad words, a common compound, a missing term, a half-history
time bound, a typed source filter and a rare literal. It checks no new latency
contract. Broad phrase verification and cold-open work remain separate costs;
a rare-literal probe cannot qualify them. Phase profiles must be measured on an
available benchmark lane before choosing a larger-scale optimization.

`append_group_indexed_profiled` separates admission, the authoritative
`Store::append(sync=true)` call, locator publication and lexical publication.
The Store phase includes compression and encryption as well as source sync; the
derived phases include building and validating pages as well as sync. These are
operation phases, not kernel-only fsync measurements. Ordinary write entrypoints
do not collect phase clocks.

`profile_record_write` generates a private synthetic fixture, records these
phases, removes all dedicated derived directories after indexed ACKs, and opens
with the retained minimum watermark. It compares complete original/clock digests,
every receipt and exact retry, current search visibility and an old pinned prefix.
Passing its 1k/10k probes does not qualify million-record recovery time.

## Explicit rebuildable caches

The default remains `CacheDurability::Durable`. The separate
`Writer::open_disk_with_cache_policy(..., CacheDurability::Rebuildable)` API keeps
**source/header/receipt writes synced** and requires complete current locator
and lexical publication before an indexed ACK. It omits sync for disposable
cache pages, segments and descriptors; those caches may be lost after a crash.
The policy is not passed to authoritative Store options. Retained-control tree
contexts reject rebuildable policy.

Cache policy is authenticated in derived roots/descriptors and page/segment AAD,
including the decoded-page-cache key. Durable mode cannot reuse weak material
and claim it was synced. Switching modes rebuilds derived material from verified
source. An old weak index object is rejected before an indexed source append
with a durable writer. Saved source watermarks, IDs, clocks, exact receipts and
pinned snapshots remain independent of cache policy. Existing durable caches
retain their old AAD/default representation.

Opening still verifies source and the caller's minimum watermark before trusting
or rebuilding a cache. Missing/torn cache files cannot establish record authority.
Publication failure after source commit keeps the same unknown-outcome/retry
contract. The mode tests cover actual grouped SIGKILL publication, full cache
loss, torn roots/descriptors, stale/same-origin fork roots, I/O failure, both mode
switch directions and exact retries. Source erasure/retention APIs are unchanged;
this direct Record2 path adds no physical-erasure guarantee.

The diagnostic reports dirty and explicitly checkpointed apparent/allocated
bytes, plus the explicit checkpoint duration outside ACK timing. That duration
includes redundant clean-file sync calls and is not a precise measurement of
incremental writeback debt. Checkpointing does not relabel weak cache pages as
durable; a later durable open still rebuilds. Full cache-loss recovery is measured
separately. A fast source-synced ACK does not remove the physical storage or
recovery work, and small-fixture probes do not qualify one-million-record
recovery or ten-client latency.
