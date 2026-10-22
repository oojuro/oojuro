# Encrypted native Record metadata trees

This opt-in derived slice extends the structural paged reader toward foundation SPEC sections 4/6/14. It does not change Record2 authority, allocation, serialized batches or limits. No second WAL exists and a cache can never supply a missing original ACK.

## Explicit APIs and namespaces

`record::paged::Reader::open_disk(authority,key,identity,directory,minimum)` verifies the existing Record2 journal one batch at a time, then opens or rebuilds a distinct encrypted `DIRECTORY`. `record::paged::Index::open_disk(reader,index_directory)` uses a separate `DISK_RECORD_INDEX`; it refuses old cache/authority namespaces. `Reader::open` and `Index::open` retain their original intermediate representation for compatibility. Disk mode retains no population batch locator, request set or segment-reference Vec. `locator_count()` and `retained_reference_count()` expose that structural property, not RSS.

The reader still owns the Store recovery lock. Close it before reopening the existing writer; this is not yet a shared streaming writer/serving owner.

## Actual layout and bounded work

Five authenticated encrypted trees contain:

| Purpose | Ordered key | Value |
| --- | --- | --- |
| Prefix | original batch sequence | original rolling receipt witness/count/time/request tag |
| Request | opaque authenticated original request tag | original batch sequence |
| Record | original numeric ID | original batch sequence + slot |
| KeyVersion | opaque keyed actual-key tag + descending valid/known/ID tuple | original batch sequence + slot |
| Segment | global original row offset | original encrypted posting-segment reference |

All are DATA-class, under the existing destroyable data-key context. Prefix/request support for a retained-control class is purpose-restricted infrastructure, not a managed erasure implementation. Record2 has no erase lifecycle; no retention or crypto-erasure claim is made.

Pages are strict binary nodes, at most 64 KiB, 128 entries/children and depth 16. Keys are at most 64 bytes and values at most 32 KiB. Nodes are packed into at most 64 MiB encrypted files rather than one inode per page. The decoded metadata cache accounts at most 4 MiB and is bound to the exact live cipher object; active bounded cursors/frontiers and batch buffers are additional memory. This accounting is not allocator RSS.

Bootstrap streams the verified source into a sorted prefix/ID builder and 4 MiB bounded external sort runs for request/key trees. Runs merge at most 32 ways through an encrypted run queue; no all-run vector exists. Query winner lookup uses the sorted key-version tree before lower bounds, filters and lexical background statistics. A bounded winner cache avoids repeated lookups; no whole key-winner map exists.

Incremental refresh uses packed copy-on-write tree paths and scans only committed tail rows for new entries. Untouched pages keep their actual birth-prefix binding. An authenticated parent/root binds each reference, and the Record2 adapter independently proves that birth is an ancestor of the currently supplied journal. Matching a numeric generation alone is insufficient. The bounded 64-entry birth-proof cache may require additional sequential source verification for old births; no constant-time cold-open claim is made.

## Authority, parsing and publication

The public watermark seals the rolling commitment. Internal Record2 bindings include identity, origin, sequence, record count and digest only in memory or authenticated ciphertext. Every page binds purpose, key class, identity, exact birth, file/offset/length, count/level and key range before parsing. Referenced ciphertext digest and AEAD must both verify. Unknown fields, invalid ordering/counts/ranges, overflow, excessive depth/length, padding, malformed ciphertext and unverified ancestry refuse. There is no plaintext metadata sidecar.

Before serving a loaded directory, the implementation streams original source coverage and validates every prefix/request/ID/key entry and exact counts. Missing/corrupt/stale/ahead/forked derived state can rebuild only from the independently verified supplied RecordLog. A current live failed/stale handle refuses until explicit refresh/rebuild. A supplied known watermark rejects rollback/fork; absent an independently retained later witness, a wholly restored healthy old authority remains undetectable.

Each update creates a packed generation per touched tree; this removes per-page inodes but does not claim constant file count across un-compacted updates. Explicit compaction/reclamation bounds live derived generations. Immutable packs and posting segments are temp-written, synced, renamed and directory-synced before atomic encrypted root replacement. Root temp sync, rename and directory sync are separate failure boundaries. Index publication failure cannot change the journal Commit. Original IDs, recorded_at, adapter provenance, duplicate logical records, exact original request results and known-time/key winner behavior survive reopen, refresh and merge.

`Reader::compact_directory` streams fresh metadata roots. Disk posting merge holds only an incoming/carry segment and streams a new reference tree. File retirement builds a bounded encrypted sorted inventory, then streams the directory, avoiding a live-file HashSet or repeated whole-reference scans. Copy-on-write parent roots are not silently assigned new birth epochs.

## Exact query behavior

The existing bounded exhaustive paged scorer is unchanged: canonical terms, original f64 arithmetic/F3 padding, F2/opaque full-decimal ID tie ordering, key-winner-before-filter and stored-only literal guard. Old snapshots keep the established native current-manifest literal-DF optimization while visibility/background statistics use the old prefix. The disk representation is compared against the original native RAM RecordIndex, including historical future-appended records and original typed output.

## Honest remaining capacity gates

Current Record2 caps remain 100k records/128 MiB serialized state. Posting caps remain 1024 references/128 MiB encoded and decoded accounting, with existing 8192-row/16 MiB segment bounds. They are not raised by this patch. Source Store still retains frame references, dictionaries and file handles; hot replay and dictionary training/dedupe/compaction need their next bounded source slice. The old RecordLog writer and default RAM/PersistentIndex paths retain their existing population structures.

The managed-v3 owner must supply independently current stable origin, generation and authenticated baseline lineage before physical rewrite/erasure integration. This patch's private trusted authority interface deliberately does not freeze that owner's wire schema or permit physical compaction to masquerade as Record2 append ancestry. No full external-memory engine, 10M/100M result, shared writer concurrency, memory-mapped plaintext files, production erasure, provider/AZ test or measured performance claim follows from this slice.

## Evidence

Focused integrated source gate passed 27 tests with zero failures and three ignored subprocess helpers (2.36 s compile, 12.56 s test execution). Active drivers SIGKILL actual child processes at six metadata pack/root boundaries and six posting/tree/descriptor boundaries, then reopen and recover the exact original ACK and query output. Other regressions cover no-locator/no-reference population, actual COW tail/merge/compaction, historical score bits, malformed codec bounds, same-number forks, coverage forgery, duplicate request refusal, corrupt/missing cache rebuild, namespace/lock/symlink guards, encrypted disk scans and bounded cipher-specific page caching. The full current-source engine gate passed **99 tests, zero failures, seven ignored subprocess helpers** (0.09 s compile, 14.25 s test execution). Strict lint initially found only two mechanical style diagnostics; they were corrected without behavior changes. Final strict lint evidence follows its bounded retry.

All stores and requests are synthetic. Verification uses the worktree's isolated target and shared flock outside bounded actual steps. No AWS/provider calls, original data, Notion writes, package installs, merges or deploys occurred.
