# Native encrypted Record segments (bounded opt-in)

`record::persistent::PersistentIndex` adds a disposable encrypted cache to the
explicit RecordLog namespace. `record::Index` remains the default memory API;
there is no CLI/runtime switch, oracle dependency, tmpfs requirement or migration
of CaptureLog/legacy segments. RecordLog's sealed Store is the only commit/ACK
authority. Call append first, retain its original Commit, then refresh the cache.
Cache publication failure cannot retract/fabricate that Commit; the old handle
fails closed and reopen rebuilds from the independently verified supplied log.

## API

```rust,ignore
let mut index = record::persistent::PersistentIndex::open(&log, cache_dir)?;
let results = index.search(&log, &query)?;
// after a separately acknowledged RecordLog::append:
index.refresh(&log)?;
index.merge(&log)?; // optional, bounded RAM re-encoding
```

`search_view` also accepts an existing protected View. Every read independently
validates the cached exact prefix and requires a current cache. Open reports
Loaded, Extended or Rebuilt. Valid reopen hydrates original committed rows and
postings without tokenization; stale valid caches tokenize only the committed
row tail. Refresh of a failed, foreign or forked live handle refuses until reopen.
The cache never turns a directory into authoritative data or manufactures a
receipt. A missing/corrupt/ahead/forked cache is rebuilt only from the supplied
independently opened RecordLog; unavailable/corrupt authority is not repaired.

## Identity, crypto and publication

A dedicated RECORD_INDEX encrypted manifest and immutable seg-opaque.idx files
use distinct `oojuro/native-record/{manifest,segment}/v1` FrameCipher AAD domains.
The existing supplied RecordLog seals/unseals; no raw key accessor or separate
key/authority/WAL file is added. Every manifest binds origin, keyed identity,
format, row counts, segment references/ciphertext digests and exact Watermark.
Every segment binds filename, exact prefix, origin/identity, row start/count,
encoded/decoded accounting. RecordLog.validate decrypts the existing prefix
commitment and verifies its rolling committed-receipt digest before hydration.
That digest is never exported as a plaintext sidecar. Equivalent Watermarks may
have different sealing nonces; comparisons authenticate the witness first,
then compare origin/identity/sequence/count. Equal-high-water divergent branches
cannot pass the authority's digest check.

All manifest/segment/temp payload bytes are encrypted before filesystem writes.
Fresh immutable segment temp -> fsync -> rename -> directory fsync precedes
manifest temp -> fsync -> atomic rename -> directory fsync. Only that manifest
selects visible cache files. A successful publish retires known orphan files;
unknown filenames, symlinks, authority directories and competing cache publishers
are refused. Cache directories/files are private 0700/0600. Names, text, attrs,
agent/tool metadata and terms have no plaintext disk sidecars. Payloads/provenance
are taken from original committed Record rows, not cache-generated carriers.
Plaintext exists in RAM; platform swap/core-dump/key handling remains external.
This uses the existing writer key, not a new cryptographic-erasure authority.

## Binary codec and Record semantics

Reuses the genuine native binary codec in lexical/segments/codec.rs: 192-byte
header, 128-row minimum-width FOR packing with original u64 IDs and 96-bit
fingerprints, Rice posting gaps/gamma TF, 32-term front-coded dictionary, CRC
footer and canonical bounded decoding (foundation SPEC 4.2). Native Record cache
format1 has its own authenticated envelope/domain; it does not claim to read
oracle v3 or CaptureLog caches. The codec visibility changes only within this
crate; shared scoring/query arithmetic is unchanged.

Hydration validates exact row ID/order against the authenticated Record prefix,
fingerprint, per-row TF sum/scope, indexing eligibility and segment boundaries.
All duplicate logical Record IDs survive duplicate text: capture-host dedupe is
never applied. Original valid_at, recorded_at, adapter/provenance and key versions
come from RecordLog. Existing View selection applies known_at, valid-time/key
winner and typed filters BEFORE anchoring, candidates, F3 statistics and pruning.
Stored-only punctuation/literal queries retain the existing Record-only guard.
Score/time/full-decimal-ID tie order and f64 score bits are unchanged. Tail
refresh appends original committed rows; merge re-encodes postings without
reassigning IDs/times or changing record eligibility.

## Bounds and explicit unfinished work

RecordLog already caps 100,000 rows and 128MiB serialized authority state. Cache
additionally caps 8,192 rows /16MiB encoded AND accounted decoded bytes per segment,
128MiB aggregate candidate/encoded/decoded accounting, 1,024 segment references,
and 1MiB manifest. Accounting charges 32/row,64/term,UTF8 term bytes,8/posting;
it is a checked representation bound, not exact RSS (authority text, container
and encoding allocations add overhead). Large candidates split to smaller
segments, or refuse before publication when no row fits. Merge uses the current
RAM projection and bounded segment buffers; it is not external-memory streaming.
Queries use restored RAM postings, not disk/mmap query readers. Synchronous cache
refresh/merge is explicit caller work, not a nonblocking background compactor.

Origin is the existing sealed first-frame incarnation fingerprint: compaction
that changes it forces rebuild; same-origin fork/truncation fails validation.
A wholly restored healthy old authority plus old cache is not independently
current: callers need an external known witness for anti-rollback. RecordLog has
no retention/erase yet; this cache adds no such promise or key-destruction proof.
No 10M/100M, full-corpus relevance/performance, full SPEC/format freeze, raw-harness
migration, hosted/provider/AZ or commercial-readiness claim is established.

## Verification

Nine new executed synthetic regressions cover exact f64/provenance/key/known-time parity
across reopen/tail/merge, duplicate text original IDs, stored-only punctuation,
zero-tokenization valid load and tail-only tokenization, exact retry no new
segment, loss/corruption/ahead/malformed/foreign rebuild, same-origin equal-count
fork/truncation, ghost row refusal, lock/symlink/authority-directory/privacy scan,
post-ACK publication failure and actual SIGKILL at all five durable segment/
manifest boundaries. SIGKILL/fsync tests are local process evidence, not disk
power-loss or distributed durability. Current full engine lib tests passed **72 tests,0 failed,4 ignored helpers**
(11.45s compile,3.56s test runtime). This includes all nine new Record cache tests,
including the real five-boundary SIGKILL driver, plus previous native CaptureLog,
segments/codec, receipt, Record/version and dev-only Nine bitwise regressions.
Ignored helpers are deliberately launched by their active subprocess drivers or
reserved for optional measurement. Test executables/paths were verified from
this worktree's own `.scratch/target`.

Strict `cargo clippy -p oojuro-engine --lib --tests --no-deps -- -D warnings`
passed in4.81s; only existing dev-oracle dependency warnings were emitted. Both
commands acquired the shared verification lock outside their actual-step timeout
(180s tests,120s lint) and used an isolated target. Logs are ignored local
`.scratch/record-segments-test.txt` and `.scratch/record-segments-lint.txt`.
Independent source review of runtime c87464e found no concrete new P1/P2 in this
bounded slice; it did not execute duplicate tests. The evidence follow-up changes
documentation only; no broad hosted/customer/provider claim follows.
