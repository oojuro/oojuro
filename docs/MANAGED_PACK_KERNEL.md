# Incremental managed immutable DATA pack kernel

This lower layer is inactive until the separately owned managed control driver composes it. It creates no second WAL, logical allocator, receipt or ACK authority. Old Store/Run writers and their formats remain unchanged. It never opens/replays PagedStore or hydrates legacy Store population maps on append.

## Exact publication boundary

`oojuro_store::managed_pack::Stage` accepts a trusted prepared DATA cipher, canonical Birth and reserved physical file/sequence numbers. Birth format1 binds stable origin, opaque keyed identity, unique epoch, generation, predecessor logical head, actual independently retained parent checkpoint and exact key-version tag. The final control/checkpoint hash is excluded to avoid a hash cycle. Birth is encrypted in the pack header; no plaintext rolling receipt hash or raw key is written. Original logical IDs/times/provenance remain the managed driver's typed carrier, not newly assigned physical sequences.

`push` takes at most1024 borrowed rows and16MiB estimated literal+metadata input per frame, reuses real native metadata and magicless Zstd compression, then seals packed bytes. Frame AEAD binds Birth, opaque pack token, ordinal and exact encoded frame head. No plaintext equality digest is emitted. Initially there are no cross-pack dedupe references. `finish` fsyncs a <=64MiB immutable candidate, atomically creates its final opaque name without replacement using a hard link, syncs the directory and removes its temporary name. This create-only publication intentionally uses hard-link rather than replacement rename semantics. Temp/candidate orphans remain uncommitted; this layer never guesses cleanup safety after an ambiguous control commit.

Actual emitted Locator callbacks feed encrypted COW Frame metadata. A failed callback, bound or write poisons the candidate; finish cannot return a reference. File metadata receives the final exact ciphertext digest/length only after pack durability, avoiding a self-hash. A staged <=16MiB dictionary is separately authenticated and referenced by exact immutable DictionaryRef; reuse opens/verifies that actual dictionary, not a caller-provided unrelated plaintext slice. No dictionary training or cross-pack dedupe/compaction claim is made here.

## Existing tree reuse

`record::managed::paged::physical::{Roots,Contexts,Editor}` owns only DATA Frame/File/Dictionary roots. The separately owned driver supplies distinct opaque root identities and trusted current/ancestor authority. Three new purpose tags are domain-separated from old Record/Key/Segment/Prefix/Request pages. Retained-control encryption still cannot protect these DATA roots.

Per-append COW writes touched tree paths through the existing bounded encrypted tree. It does not rebuild prior packs or scan the payload population. Frame callbacks must form exact contiguous ordinal/offset/count coverage for their final PackRef; unfinished coverage and identity replacement poison/refuse candidates. New physical sequences cannot overlap prior published frames. Greatest-key lookup is a bounded root-to-leaf traversal, not a population map. Old reused pages retain their actual authenticated birth; authority must prove that birth's exact parent checkpoint/epoch/key version as an ancestor. No numeric-generation-only acceptance is substituted.

Inventory methods stream complete file/dictionary references and all reachable metadata pages including reused COW pages. They confer no unlink permission. Actual managed tombstone/control publication, complete source pruning, current floor, sparse original IDs/receipt retention and any erasure audit remain the managed owner's authority.

## Fresh committed reads

`Reader<C:Catalog>` can resolve only the independently current committed catalog. Every read resolves current binding, exact immutable file/frame/dictionary references, authenticated Birth ancestry and fresh exact-version DATA-key availability. It checks current authority again before returning data and drops decoded/file/dictionary/decoder caches on refusal. Cached DATA cannot bypass a revoked/missing key or stale/unavailable current authority. Dropping allocations is not a proof of allocator-memory zeroization or irreversible crypto-erasure.

Opening a resource streams its complete ciphertext digest and validates bounded header/frame coverage; frame reads then authenticate exact contextual AEAD. At most32 file handles and16 dictionary entries/16MiB dictionary bytes remain cached. A caller-selected decoded-cache budget is capped16MiB. One decoded frame, compressed/encrypted temporary buffers, active leases, Zstd internal state, tree cache/frontier and caller output are additional workspace; these representation bounds are not an RSS claim. The reader retains no per-record/frame/file population map. Fresh key resolution and whole-pack verification on resource misses have costs; no constant-time/provider latency or10M/100M throughput claim follows.

The ordinary Store/Run decoder's default header-only AEAD path is byte-compatible. A narrow trusted decrypt callback supplies managed contextual AAD; it is not a wire-selected authenticated flag.

## Verification and remaining gates

Synthetic actual codec/pack tests cover original fields/ranges, incremental immutable old packs, restart, missing/orphan references, exact parent/epoch/key binding, corruption, bounds/callback poison, dictionary inventory, no warm-key fallback,32-handle eviction and SIGKILL at staging/temp durability/final-name publication/directory durability. Physical tests compose actual packs with serialized encrypted roots, tail COW reuse, floor lookup, complete inventory, dictionary immutability and fork/current-authority refusal. Exact final gate results are recorded after source freeze.

This is lower code toward the shared managed paged writer. The production managed current-control adapter, original allocator/ACK/retry/tombstone integration and cumulative strict lint/gates are separate required work. Store dictionary training, streaming whole-store compaction, query integration and real10M/100M SPEC acceptance are unfinished. No cap was raised, provider called, secret/customer data used, new dependency installed, deployment made or branch merged.
