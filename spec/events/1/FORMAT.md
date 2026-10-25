# .juro Event profile 1

Profile specification: **1.0.0-draft.1**, experimental. This version is
independent of engine releases and the underlying wire version. Normative
MUST/MUST NOT requirements supplement [wire 0.1](../../0.1/FORMAT.md).
Together these two documents define every byte needed to read an Event file;
no application schema, cloud service, or reference-engine code is required.
Wire 0.1 itself is unchanged. Ordinary wire 0.1 key/value files are not Event files.

Use `.juro` for new filenames. Existing `.ojo` filenames remain supported;
renaming is never required for compatibility. Profile detection uses the
validated header/version and marker below. The marker still contains `ojo`;
the filename convention does not rename cryptographic domains or wire bytes.

## Logical records

An event contains exactly an engine acceptance timestamp and opaque data bytes.
The timestamp is a signed 64-bit count of microseconds since 1970-01-01
00:00:00 UTC, ignoring leap seconds. Data is 0..16,777,216 bytes, including
empty values, NUL, invalid UTF-8 and arbitrary binary attachments. A reader
MUST preserve data byte for byte. No caller key or provider, source, severity,
agent, session, request, or trace field is required in the logical record.

An engine assigns the timestamp when it accepts a new append intent. Preparation
alone is not a durable acknowledgement. Commit success still requires the
wire's file sync. An exact retry MUST preserve the original timestamp, append
position and data. Repeated timestamps and backward clock movement are valid.
Append order comes from the transaction sequence, never from the wall clock.
Two independent append calls with identical data are two distinct events.
The timestamp records the engine clock; it is not an external time attestation
or necessarily the time described by the application data.

## Profile marker

Every Event file MUST start with exactly one put operation in transaction 1:

| Field | Exact value |
| --- | --- |
| Key | hex `00 6f 6a 6f 2e 65 76 65 6e 74 73 2f 31` (`NUL` + `ojo.events/1`) |
| Value, bytes 0..8 | hex `4f 4a 4f 45 56 54 31 00` (`OJOEVT1` + `NUL`) |
| Value, bytes 8..24 | Logical journal ID: 16 CSPRNG bytes, at least one nonzero |

The marker value is exactly 24 bytes. Its wire request ID follows the normal
wire rules; it need not equal the logical journal ID. A complete empty Event
journal contains this transaction and no events. A header-only wire file is
not a complete Event journal.

## Event transactions

Every subsequent committed transaction MUST contain exactly one put and no
other operation. Its value is the exact logical data, with no application
wrapper or timestamp prefix. Its key is exactly 36 bytes:

| Offset | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 4 | hex `00 65 76 74` (`NUL` + `evt`) |
| 4 | 8 | Event ordinal: unsigned **big-endian**, wire transaction sequence minus 1 |
| 12 | 8 | Acceptance timestamp: signed two's-complement **big-endian** i64 |
| 20 | 16 | Engine request nonce, exactly the transaction header's request ID |

Profile key integers use big-endian; wire headers and operation lengths retain
their little-endian grammar. Ordinals start at 1 with no gaps. Deletions,
replacement versions, application keys, multi-operation transactions and extra
marker transactions are invalid for this profile. An Event reader MUST check
the full wire grammar and these restrictions, including nonce equality.
Its writable open MUST refuse a profile mismatch before any tail truncation.
Event appenders MUST generate nonzero random request nonces and keep the wire's
request uniqueness and exact-retry rules. Nonce collisions require a fresh
nonce before a new intent is accepted; a retained retry keeps its original nonce.

Compression and authenticated encryption follow wire 0.1 without changes.
Readers without the required codec/key MUST explicitly refuse the file.
Compaction that preserves this profile MUST retain the marker, logical ID,
all events, acceptance times, ordinals and request IDs in order. A compressed
archive receives a new physical file ID under the wire rules; logical positions
remain valid. Physical checkpoints still bind their particular physical file.

## Opaque cursors and retained retry intents

A portable cursor is exactly 40 bytes:
`logical_journal_id[16] || ordinal_u64_be[8] || request_nonce[16]`.
Every component MUST match the selected committed event. An event get may
report an absent cursor, but a scan/follow resume MUST refuse an unknown,
future, foreign or divergent cursor rather than silently skip a prefix.
Copies share their existing logical prefix. Independent future appends on
different copies are separate branches; there is no distributed ordering,
replication or cross-branch idempotency guarantee.

The optional portable engine-issued retry intent is exactly 80 bytes:

| Offset | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 8 | ASCII `OJOINT1` + NUL |
| 8 | 16 | Logical journal ID |
| 24 | 16 | Nonzero engine request nonce |
| 40 | 8 | Original acceptance timestamp, signed i64 big-endian |
| 48 | 32 | SHA-256 of exact data bytes |

Retain the intent with its data before submitting a write when its acknowledgement
can be lost. Reject a different journal, different data, or a changed timestamp
under an already committed nonce. After an ambiguous I/O error, drop the
poisoned handle, recover the same journal and reuse this same intent. If its
request already committed, return its original cursor/time without appending.
If not committed, use its original time and the next append ordinal. A caller
that discards the intent cannot distinguish a retry from a separate identical
append. These bytes are private retry bookkeeping, not an authentication token
or permission to write; file permissions, encryption and the lock protocol apply.

## Retrieval and concurrency

Generic database operations include exact original get/read, append-order
scan, timestamp ranges, indexed lexical/full-text search and live follow.
Implementations may keep disposable indexes separately; originals and profile
recovery never depend on them. Search/index compatibility is independent of
this storage grammar. Valid UTF-8 data may be indexed as text; opaque binary
data stays exactly readable even when it has no text-search representation.
Application interpretations of data belong outside this profile.

Timestamp ranges use [from,to) and order equal timestamps by append ordinal.
Follow uses append cursors and MUST NOT skip an event because its time moved
backward. Bounded pages report a resume cursor and whether more records existed
in that call's snapshot. Caught-up callers retain their last cursor.

Wire 0.1 lifetime inode locks and crash recovery still apply: one exclusive
writer excludes external read handles. An embedded collector may own the
healthy writer and serialize local operations, releasing its mutex while
followers wait. A process service may expose that same owner to clients. This
does not introduce a concurrent-writer wire protocol or permit replacing a
locked file. No remote I/O is necessary for local durability.

## Reference implementation limits and qualification

The initial Rust Event facade retains event metadata in memory, validates the
profile during the existing wire replay, and builds a timestamp index lazily.
Its existing wire admission limits still apply. Scan pages contain at most
1,024 records. The embedded collector additionally caps a returned batch at
64 MiB of original data and bounds a follow wait at 60 seconds. Reopen validates
the complete committed history; a long-lived collector avoids per-call reopen.

The independent Python example deliberately supports plaintext raw files only
and imposes a 10,000-record/64-MiB-original resource budget. Its codec/key and
resource refusals are explicit. This demonstrates a second implementation of
this profile, not performance at scale or full encrypted/codec conformance.
Event benchmarks must be reported separately from historical key/value and
legacy search results. Specification source does not publish a stable format,
engine release, or usable hosted deployment.
