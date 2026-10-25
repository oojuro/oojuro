# .juro wire version 0.1

Specification release: **0.1.0-draft.2**, experimental. MUST, MUST NOT, SHOULD
and MAY express conformance requirements. All offsets are zero-based; integers
are unsigned little-endian. Concatenation is written `||`. No field uses native
pointer widths, Rust layouts, varints, implicit padding or engine version IDs.

## 1. File and update model

A database is one regular file: a 64-byte header followed by zero or more atomic
transactions. It needs no WAL, directory manifest, index, dictionary or sidecar
to recover committed keys and values. `.juro` is the canonical new filename
extension; previously published `.ojo` files remain compatible and keep their
paths. The header and version determine the format, never the extension.
The naming convention does not change magic bytes, wire versions or vectors.

Applications update this file directly. Committed bytes are immutable: put and
delete operations append new versions to the same file. No full-file snapshot
rewrite is needed to update a key. The only destructive operation defined here
is removal of a verified incomplete final transaction during exclusive recovery.
Compaction, physical erasure and arbitrary overwrites are not defined in 0.1.

An operation is identified by `(transaction_sequence, operation_index)`, where
operation indexes start at zero. Sequences start at one and increase by one.
The latest committed operation for an exact byte key determines its current
value. A put with an empty value is distinct from a delete. Earlier versions
remain readable. Duplicate keys within one transaction are applied in order.

## 2. Algorithms and limits

`CRC32(x)` is the four-byte little-endian CRC-32/ISO-HDLC value: reflected
polynomial `0xedb88320`, initial register `0xffffffff`, final XOR `0xffffffff`.
For ASCII `123456789` it is integer `0xcbf43926`, bytes `26 39 f4 cb`.
Python `zlib.crc32` and zlib's `crc32` compute this value.

`SHA256(x)` is the 32 raw bytes of standard SHA-256, not hexadecimal text.
`SHA256("abc")` in hex is
`ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad`.

| Item | Required format limit |
| --- | ---: |
| Key bytes | 1..65,536 |
| Put value bytes | 0..16,777,216 |
| Operations per transaction | 1..1,024 |
| Decoded transaction body | at most 33,554,432 bytes (32 MiB) |
| Stored transaction body, including encryption | at most 34,603,008 bytes (33 MiB) |
| Zstandard decoder window | at most 33,554,432 bytes |

All additions, multiplication, conversions, offsets and allocations MUST be
checked before use. A reader MAY impose a smaller documented resource budget;
it MUST return a resource-limit error rather than claim the database is corrupt
or return an apparently complete partial result. The format does not cap the
lifetime transaction count at the reference implementation's 100,000-operation
admission guard. Readers can stream decoded bodies, while retaining or spilling
the bookkeeping needed to check request-ID uniqueness.

## 3. File header: 64 bytes

| Offset | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 8 | Magic: hex `4f 4a 4f 46 49 4c 45 00`, ASCII `OJOFILE` plus NUL |
| 8 | 2 | Wire major: 0 |
| 10 | 2 | Wire minor: 1 |
| 12 | 4 | Flags: 0 plaintext, 1 encrypted; all other values unsupported |
| 16 | 16 | Random file ID; new files use 128 bits from an OS CSPRNG |
| 32 | 8 | Key ID below, or eight zero bytes for plaintext |
| 40 | 20 | Reserved: all zero |
| 60 | 4 | `CRC32(header[0:60])` |

An encrypted file uses one caller-supplied 32-byte uniformly random key. Its
public key ID is the first eight bytes of
`SHA256(ASCII("ojo/key-id/0.1") || 00 || key)`. Keys are not stored in the file.
Passwords are not keys; password derivation, key distribution and rotation are
outside this version. These eight bytes are an identifier, not a MAC.

A reader MUST reject an incomplete/invalid header. It MUST NOT reconstruct or
overwrite it. It MUST reject unknown versions, flags and nonzero reserved bytes.
An encrypted file requires its key. A key-aware open of a plaintext file MUST
fail as a downgrade, rather than silently proceed without encryption.

## 4. Transaction header: 96 bytes

Starting immediately after the file header or previous committed footer:

| Offset | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 8 | Magic: ASCII `OJOTXN1` plus NUL |
| 8 | 8 | Transaction sequence, exactly previous sequence + 1 |
| 16 | 8 | Stored body length, excluding this header and the footer |
| 24 | 8 | Exact decoded body length |
| 32 | 4 | Operation count |
| 36 | 1 | Compression: 0 raw, 1 standard Zstandard frame |
| 37 | 3 | Reserved: all zero |
| 40 | 32 | Previous footer's digest, or 32 zero bytes for transaction 1 |
| 72 | 16 | Caller request/retry ID; at least one nonzero byte |
| 88 | 4 | Reserved: all zero |
| 92 | 4 | `CRC32(transaction_header[0:92])` |

No committed transactions may repeat a request ID. See retry rules below.
Unknown codecs and reserved values are unsupported, not an invitation to skip
the transaction. Header checks precede use of its length fields.

## 5. Body encoding, compression and encryption

The decoded body is exactly `operation_count` consecutive operations:

| Offset relative to operation | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 1 | Kind: 0 put, 1 delete |
| 1 | 3 | Reserved: all zero |
| 4 | 4 | Key length |
| 8 | 8 | Value length; MUST be zero for delete |
| 16 | key length | Exact key bytes |
| next | value length | Exact value bytes; absent for delete |

No trailing decoded bytes are permitted. Keys and values are binary, not
implicitly UTF-8, JSON, paths or executable instructions. Readers MUST preserve
NULs, invalid UTF-8, newline conventions and unknown application fields exactly.
The file has no provider-specific agent kinds or mandatory external schema.

Codec 0 stores the decoded body unchanged. Codec 1 stores exactly one standard
Zstandard frame as defined by [RFC 8878](https://www.rfc-editor.org/rfc/rfc8878),
including its standard frame magic. External dictionaries, magicless frames,
skippable frames, concatenated frames and trailing bytes are forbidden. The
decoder window MUST respect section 2, and decoded length MUST equal the header
value. Any compression level is permitted; compression parameters and search
ranking are not wire compatibility rules.

For a plaintext file, the stored body is these compressed/raw bytes.
For an encrypted file, compress first, then use XChaCha20-Poly1305 with the
file key and a fresh uniformly random 24-byte nonce for EVERY transaction:

```text
AAD = ASCII("oojuro/ojo/0.1/transaction") || 00
      || complete_64_byte_file_header || complete_96_byte_transaction_header
stored_body = nonce[24] || ciphertext || authentication_tag[16]
```

The standard XChaCha20-Poly1305-IETF construction uses HChaCha20(key,
nonce[0:16]) as the subkey, and the 12-byte IETF nonce
`00 00 00 00 || nonce[16:24]`. It then applies ChaCha20-Poly1305 from
[RFC 8439](https://www.rfc-editor.org/rfc/rfc8439), with that subkey, nonce and
AAD. The complete XChaCha construction is described in
[draft-irtf-cfrg-xchacha-03](https://datatracker.ietf.org/doc/html/draft-irtf-cfrg-xchacha-03).
Established libraries expose this as `crypto_aead_xchacha20poly1305_ietf`.
Use a maintained cryptographic implementation rather than reimplementing it.
Authentication MUST succeed before decompression or interpretation. Encryption
is required on EVERY transaction when the file flag is 1; no mixed plaintext
upgrade, downgrade or key change is permitted in this version.

## 6. Commit footer: 48 bytes

Immediately after the declared stored body:

| Offset | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 8 | Magic: ASCII `OJOCMT1` plus NUL |
| 8 | 8 | Sequence, equal to the transaction header |
| 16 | 32 | `SHA256(file_header || transaction_header || stored_body)` |

A transaction is committed only if its complete header, body, footer, digest,
chain, codec, authentication and operations validate. No operation may become
visible before validation of its WHOLE transaction. SHA-256 chaining detects
accidental changes and binds order. Plaintext checksums are not signatures or
protection against an attacker who can rewrite a whole file.

## 7. Locks, acknowledgements and exact retries

Conforming local writers hold an exclusive advisory lock on the OPEN DATABASE
INODE for their entire open lifetime. Readers hold a shared lock for their open
lifetime. On POSIX systems this is `flock(LOCK_EX)` / `flock(LOCK_SH)`; a
nonblocking open uses `LOCK_NB`. Writers and readers cannot run concurrently in
this draft; multiple readers may coexist. Locks are released by closing the
handle or process exit, including a crash. No lock sidecar is required.

All cooperating implementations MUST use this protocol. Never unlink, rename,
replace, compact or rotate an open database to evade its inode lock. Copies
must be made while writers are excluded. Filesystems without equivalent lock,
sync and truncation guarantees are unsupported for writing. This is a local
filesystem protocol, not network/distributed concurrency or replication.

Creation MUST use exclusive create, write and sync the complete header, and
sync the parent directory before reporting success. A failed partial creation
remains an invalid file requiring explicit operator handling.

While holding the exclusive lock, recover as below, construct and validate the
entire next transaction, append its header, body and footer, and sync the FILE
before acknowledging success. `fsync` or an equivalent that durably persists
the appended bytes and necessary file-length metadata is required. A successful
buffered write or flush alone is not acknowledgement. File creation also
requires directory sync; ordinary appends do not create new directory entries.

A failed/ambiguous append invalidates that writer handle. Reopen, recover and
retry with the SAME request ID. Fingerprint a request as SHA-256 of its decoded
operation body. If its ID already committed with the same fingerprint, return
the original commit without writing again. Reuse with a different fingerprint
is a conflict. A client uncertain whether its acknowledgement arrived MUST NOT
generate a new ID for the same logical transaction. Future transactions resume
the committed sequence and digest chain.

Verification after reopen proves byte validity, not successful durability of an
earlier ambiguous write. A complete valid frame can remain visible after its
file sync failed. Before acknowledging a replayed exact retry (or an accepted
application operation reconstructed from that prefix), the writer MUST observe
a successful file sync covering that prefix, unless it already observed such a
successful barrier. A sync failure MUST NOT become a success acknowledgement.
An unchanged healthy handle may reuse its own successful durability barrier;
it need not sync again for each retry. This clarifies acknowledgement semantics
without changing wire bytes, accepted transactions or frozen vectors.

## 8. Recovery and corruption

Scan forward from offset 64, retaining the last fully validated committed end.
Validate every complete transaction, not just the last one. Never scan for a
later magic string to skip corrupted bytes.

1. EOF exactly after the file header or a valid footer is clean.
2. Fewer than 96 bytes after the last valid footer is an incomplete tail.
3. A valid next header whose declared body + footer extends past EOF is an
   incomplete tail. Its version/codec/reserved/bounds, sequence and previous
   digest still must be valid. A duplicate committed request ID is invalid.
4. A complete header with invalid checksum/fields/chain, or a complete declared
   transaction with a bad footer, digest, payload, authentication or operation,
   is CORRUPTION. Fail the open without changing the file. Do not truncate it
   as though it were an unacknowledged append.

A read-only open MAY expose the validated committed prefix of an incomplete
tail, but MUST report the tail length and MUST NOT modify bytes. An exclusive
writable open MUST validate the entire committed prefix first, truncate ONLY
an incomplete tail to that prefix, and sync the truncation before accepting a
new transaction. No incomplete transaction contributes any visible operations.

A complete valid transaction may survive even when its caller saw no
acknowledgement. Exact retry resolves that case. Crash recovery does not infer
which replies reached clients.

## 9. Rollback, deletion and application profiles

A file alone cannot detect replacement by an older valid copy or deletion of a
whole valid suffix. A caller requiring rollback detection MUST retain a trusted
external witness `(file_id, sequence, digest)`. An open accepts that witness only
if the same file ID contains the witnessed sequence with the identical digest;
later valid transactions are permitted. Sequence 0 uses the file ID and 32 zero
digest bytes. Storing a witness alongside the same replaceable file does not
provide independent freshness. This draft has no signed or hosted authority.

Deletes change logical current visibility; original bytes remain in prior
transactions. They are NOT physical erasure or cryptographic destruction.
Unknown trace metadata and attachments belong in values/profile-defined keys;
an independent reader can always recover exact keys, values and versions without
an agent SDK. Capture consent, redaction, retention, tenant authorization,
trace schemas and model execution remain application responsibilities.
