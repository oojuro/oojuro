# Opt-in paged Store metadata read ownership

This is the next structural read/query slice after PR37/41. It does not raise Record2's100k/128MiB caps or add a writer/ACK/control authority. Existing Store/Run defaults retain their current representations and wire formats.

## APIs and composition

`oojuro_store::store::paged::PagedStore::open(existing_source, Options, derived_directory)` requires a previously strictly sealed Store, its actual key and existing MANIFEST/LOCK/marker. It exposes len/get/get_range/record/scan_time and the same first-ciphertext origin digest. It never constructs the old Store first, creates a missing source MANIFEST, appends records, allocates IDs/times or returns a new ACK.

`record::paged::Reader::open_disk_store(source,key,identity,store_metadata_directory,record_directory,minimum_watermark)` explicitly composes this owner with exact native Record directory verification. `Index::open_disk` then uses genuine binary posting segments and disk reference trees. Both constructors fail closed; no old RAM Store fallback exists. Existing Reader::open/open_disk and default writers are unchanged. Use distinct sibling source/Store-metadata/Record-directory/posting directories; namespace checks reject authority files or unrelated entries.

## Actual retained representation

Source replay streams MANIFEST CRC/IDs and cold index CRC/varints, and decrypts/verifies hot and cold frames with the actual shared decoder. It verifies sequence/count/raw-size/dictionary/time coverage and logical text sums before recording a locator. Only the last hot tail can be repaired using the exact bounded later-frame rule. Dictionaries are independently authenticated even if unused. Source absence/corruption cannot be hidden by a healthy old derived cache.

Locators, source-file inventory and dictionary IDs are fixed-width encrypted64KiB table pages, not population Vecs or maps. Frame ordinal lookup is direct; sequence/file/dictionary lookup uses bounded page reads and binary search. Source-file ordering initially uses repeated numeric directory passes, so file discovery may be quadratic in file count; no linear bootstrap/large-store timing claim is made.

The owner retains three table roots/counts and at most one decoded page per table. File resources retain at most32cached handles, dictionary resources at most16entries/16MiB payload, and at most4dictionary decoder pools with one idle context each. Active resource leases, source/metadata lock and table handles, decoded frames, caller-configured decoded-cache budget and compression-library internal state are additional memory. These are representation bounds, not exact RSS. Dictionary/header IDs are not silently copied into a population map. Dictionaries larger than16MiB and source frame metadata larger than1MiB/131072records explicitly refuse in this opt-in mode; the old Store remains format compatible with its own limits.

Each fresh table generation is DATA-key encrypted with purpose/width/build/page identity. An authenticated encrypted root binds that unique generation, exact independently verified physical source digest, original first-ciphertext origin and all table/record counts. Root equality is checked against the source-verified in-memory expectation before public reads. No plaintext low-entropy prefix digest or metadata sidecar is written. Incomplete/corrupt/old metadata can only rebuild from independently verified source. This first implementation re-verifies source and rebuilds its bounded metadata tables on every open; it does not claim an incremental cold-open optimization or reuse an unverified saved root.

Tables are temp-written/fsynced/renamed/directory-synced before the root's temp+fsync+rename+directory-sync publication. Old metadata retirement happens after successful publication under the owning lock. This is derived state, with no second WAL or source ACK. Active ownership excludes the old Store writer. A managed independently current control/floor adapter and shared paged writer must come next; retaining an old reader across external rewrite/revocation is not permitted by a future managed driver.

## Authority limits

A supplied native Record minimum witness detects a shortened or same-count forked Record history after source verification. PagedStore alone cannot identify a wholly restored healthy old source without an independently retained later authority. Record2 origin changes under physical compaction, and this implementation does not reinterpret such a rewrite as append ancestry. The separate managed-v3 owner retains stable origin/generation/receipt lineage serialization ownership. No erase, whole-snapshot managed scalability, host/AZ durability or10M/100M acceptance is implied.

The pending shared managed paged writer/control-tail adapter must eliminate live managed snapshot retention and preserve original sparse IDs/times/fingerprints/receipts, current authority fencing and erasure/audit. Raising caps first would not solve those structures.

## Evidence

Initial four source-backed synthetic tests passed (1.11s compile/0.39s runtime): actual hot+cold/dedupe originals and origin parity, cache loss/corruption/reopen, root replacement refusal, healthy old cache unable to bypass corrupted source AEAD/wrong key/missing authority, exact final-tail repair, source/cache locking, namespace/symlink refusal and encrypted metadata disk scan. An initial file-lease coercion compile error was corrected before these tests ran. Native composition/crash and final lint evidence follows its gates.

No provider/AWS/Notion/customer data/new packages/merges/deployment. Heavy gates use this isolated target and the shared lock outside bounded actual steps.
