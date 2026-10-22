# Native encrypted derived segments: bounded opt-in slice

This extends the native lexical port in PR17. `lexical::segments::PersistentIndex`
is an explicit library API. `LocalCapture` still defaults to the memory index.
CaptureLog is the **only** commit authority. There is no derived WAL, receipt
rewriter, raw reparsing, policy rerun, hosted switch or oracle runtime dependency.

## API and recovery

```rust,ignore
let mut index = PersistentIndex::open(&capture_log, cache_directory)?;
let hits = index.search(&capture_log, &query)?;
// After the independently acknowledged CaptureLog::commit:
index.refresh(&capture_log)?;
index.merge(&capture_log)?; // optional bounded RAM merge
```

The caller supplies the independently opened, authenticated CaptureLog on every
read. An authority change makes an old handle unavailable until refresh. A
publication failure invalidates the handle but never retracts, changes or
fabricates an already returned capture commit outcome. Reopen rebuilds from
committed parsed receipts. A corrupt/unavailable authority cannot be repaired by
an index. Derived files may be removed in their entirety while the handle is
closed; the next open rebuilds them.

Cache open authenticates an encrypted manifest and every referenced immutable
segment, validates all counts/ranges/prefixes and reconstructs the canonical
host/dedupe mapping from exact committed Event receipts. It does **not** tokenize
or stem their text on a valid-cache path. Events, provenance and literal matching
come from those receipts, not copied plaintext cache rows. A stale valid prefix
extends only newly committed batches. Stored-only eligibility, normalized-args
byte semantics, original host IDs and the PR17 scorer are unchanged. The existing
native scorer consumes restored postings directly, preserving arithmetic order
and score bits across reopens, deltas and merge.

Missing, corrupt, unsupported-format, ahead or forked caches rebuild before
serving; directory misuse and unavailable I/O fail closed. `Opened` reports
Loaded, Extended or Rebuilt. A dedicated private directory and exclusive cache
publisher lock prevent competing publication. Unknown files and symlinks are
rejected; an authority directory cannot be used as a cache directory.

## Authority binding and encryption

CaptureLog computes a rolling BLAKE3 digest during its existing replay and commit
of **all** exact records: header identity, registrations, raw bytes and authorized
parsed receipts. Each cache prefix includes record high-water, parsed-batch count
and digest. This is in-memory derived bookkeeping, with no capture format change
and no independent authority file.

A minimal read-only Store accessor hashes the first sealed frame's ciphertext.
Its randomized nonce distinguishes independently created authorities with equal
caller-supplied Identity/key. Exact copied authority files retain that origin;
diverged descendants are rejected by the committed prefix digest. Compaction can
change the ciphertext origin and deliberately forces cache rebuild. It is an
incarnation fingerprint for caches, **not** a durable global log UUID or an
anti-rollback external authority. Restoring an old complete local capture log
still requires the separately scoped freshness/backup/key-control solution.

The existing XChaCha20-Poly1305 FrameCipher encrypts every manifest, segment and
temporary payload. Distinct native manifest/segment AAD domains bind the capture
identity digest, sealed origin and format. Segment AAD additionally binds its
opaque random filename, exact committed prefix, global row start/count and byte
count. The authenticated manifest binds ciphertext digests and the complete
current prefix/counts. Authentication precedes JSON/codec deserialization and
hydration. There are no text, tool, argument, posting-term or key plaintext
sidecars. Cache uses the already supplied capture key; this adds no key service,
crypto-erasure, retained-key or production confidentiality guarantee. Plaintext
still exists in RAM and may reach swap/core dumps according to platform policy.

## Native binary format and SPEC source map

This is native derived format **1**, not a silently compatible oracle v3 file.
Required version, exact reserved bytes, offsets and lengths are checked. The
manifest has strict JSON fields and bounded size. Segments use genuine binary
codecs rather than JSON serialization of the Index or a whole Event vector.

| Foundation SPEC 4.2 / read-only oracle | Native implementation |
| --- | --- |
| 192-byte fixed segment header | `segments/codec.rs`, required magic/version/offsets/totals |
| 128-row frame-of-reference packing (`nine/rows.rs`) | exact u64 IDs, u32 length/scope, minimal LSB widths |
| 96-bit ID fingerprints | BLAKE3 of decimal event ID, plus exact ID checked against authority |
| Rice gaps + Elias gamma TF (`nine/segment.rs`) | same gap-minus-one, k choice, bit order; bounded decoder |
| 32-term front-coded dictionary | restart prefixes, canonical lengths, checked block index |
| CRC footer | checked CRC32 plus reserved/end marker; AEAD is the security boundary |
| Streaming writer and compactor | **not implemented**; bounded segment buffers and RAM merge |

Rows retain only IDs, length/scope and fingerprints. Postings carry local row
ordinals and positive TF. Decoder rejects truncation, corrupt CRC, invalid UTF-8,
noncanonical integers, duplicate/unsorted terms or rows, invalid prefix lengths,
nonzero padding/reserved fields, overflow, bad offsets and expansion beyond the
explicit accounting bound. The cache layer additionally verifies exact canonical
IDs, authorized-prefix membership, row fingerprint and per-row TF/scope totals.

## Publication, bounds and unfinished gates

Each <=8192-row segment is encoded, encrypted, written create-new to a private
0600 temporary file, fsynced, renamed immutable and followed by directory fsync.
Only after every referenced segment is durable is the encrypted manifest written
to another create-new temporary file, fsynced, atomically renamed and the
directory fsynced. Orphan known cache files cannot become visible through reads;
a successful publication reaps unreferenced files. Actual SIGKILL regressions
exercise segment-temp, segment-published, manifest-temp, manifest-published and
manifest-durable boundaries. Local fsync/rename tests do not establish network,
AZ or region durability.

Bounds: capture's existing 100k events /128MiB serialized parsed metadata;
16MiB encoded and decoded accounting per segment; 128MiB aggregate candidate
encoded and decoded accounting; <=1024 references; <=1MiB manifest. Accounting
charges 32 bytes/row, 64/term, UTF-8 term bytes and 8/posting. These are checked
logical representation bounds, **not an exact process RSS limit**: Rust container
and authority/receipt/text allocations add overhead. Merge uses the existing RAM
projection and one bounded segment buffer, re-encodes immutable replacement
segments, publishes atomically, then retires old cache files. It is not an
external-memory streaming merge. Native text tokenization/query allocations and
full-receipt replay retain PR17's bounded-prototype limitations.

No claim of completed SPEC disk engine, 10M/100M scale, external streaming merge,
compaction schedules, disk-backed query reader, mmap cache, retention/erasure,
backup freshness, learned outcomes/relevance gains, final privacy/history,
full-corpus 30,905-query gate, or production deployment follows from this slice.
Queries still use restored in-memory postings. No new runtime dependency on
Nine/Trace/Core exists; those remain test-only differential oracles.

## Verification

Pending final bounded gates; synthetic tests cover codec adversarial refusal,
exact reopen/delta/merge scores and provenance, no valid-cache retokenization,
copied-origin fork and equal-high-water rejection, corruption/loss/ahead rebuild,
publication failure after acknowledged capture commit, multi-segment row
boundaries, encrypted file scan, exclusive scope and actual process SIGKILL.
Local 1000-row measurement is opt-in and not a large-corpus performance claim.
