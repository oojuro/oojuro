# Native byte-message capture authority (engine stage 1)

This explicit fresh lane stores original arbitrary body bytes and authorized parsed
receipts atomically in the existing sealed Store. It does not reinterpret a body
as a source line, wrap it in invented JSONL, or create a second WAL. This milestone
contains an engine API only: actual OTLP/proxy parsers, local adapters, network
receiver/forwarder and CLI wiring are not implemented here.

## Fresh format and trust

MessageLog::create requires a nonexistent directory and creates a private marker
plus the sealed Store. MessageLog::open requires the exact existing MESSAGE_FORMAT
marker and regular existing MANIFEST before invoking the Store path. Existing or
partial enrollment never silently resets. Failed setup may leave a partial root;
automatic retry does not overwrite it. Capture1 line roots retain their original
header, tag2 carrier, default cipher and line validation. Message roots use a
distinct header/tag3 and reject line APIs. A copied marker cannot convert a root.

A fixed message-purpose KDF over the supplied master key and canonical Identity
produces a separate Store encryption key. Store compression happens BEFORE frame
AEAD; no nested ciphertext hides original data from compression. Existing frame
and dictionary AAD is unchanged. Wrong identity/framing/key cannot decrypt this
namespace. The temporary derived-key copy is cleared, without a forensic memory
erasure guarantee. The caller owns/protects the master key.

The caller supplies source-generation descriptors, parser context, authorization
and provenance. These are reported facts, not independent proof that an agent,
run, tool, protocol or person produced the content. The engine does not grant raw
consent or inspect a harness file: a future adapter must obtain explicit raw-byte
consent BEFORE source inspection. Original bytes can contain excluded or secret
text even when parsed output is withheld.

## Original message intent and atomic ACK

A Request requires the original nonempty request ID, per-source monotonic sequence,
and bounded exact JSON-object metadata bytes. Metadata records protocol/body
boundaries and remains sealed, never a public plaintext fingerprint. Original
lookup checks source, ID, sequence, metadata, raw start and exact raw bytes BEFORE
a current parser or policy is consulted. Exact historical retries return the
original ACK even after later commits or policy changes; changed intent conflicts.

A MessageBatch contains exact raw bytes, complete parser context, original intent,
byte-covering MessageUnits, policy decisions, events and the proposed outcome.
Each unit explicitly reports Parsed or Malformed and every stored/withheld emission.
Stored IDs, eligibility, event byte ranges, parent links and progress must agree.
Empty bodies require an explicit zero-emission unit and retained request metadata;
their sequence advances although their byte cursor does not.

One Store commit seals raw bytes, request, parser context, policy decisions,
outcomes and assigned IDs together. Validation/capacity refusal advances no byte
cursor or event ID and returns no progress ACK. After no ACK or unknown completion,
the caller must retain the original prepared body/metadata and retry that exact
intent. There is no implicit durable spool. A future stdin adapter cannot safely
acknowledge lost input after refusal.

## Bounded engine surfaces

Raw bodies are initially limited to 256 KiB per commit. Larger messages/streaming
bodies remain an implementation gap, not an implicit truncation mode. Requests
have at most 4 KiB metadata, 1024 units, 128 KiB parser context and 8 MiB encoded
batch metadata. Existing 100000-event/128 MiB aggregate metadata limits apply.
Serialization stops at the metadata ceiling rather than allocating an unbounded
candidate JSON buffer. These are work/state bounds, not exact resident-memory
claims.

MessageReceiptView supports bounded page/get/stats/export and authenticated
snapshot cursors. Exports explicitly label framing, byte_range and message_kind;
legacy Event line_start/end fields are byte offsets in this mode. Malformed and
empty-output statistics count explicitly named units, not body requests or source
lines. Request metadata
and parser context never appear in these receipt projections. Public Capture1
batches/ReceiptView refuse Messages. The checked immutable authority getter lets
the native lexical/segment projection use committed, authorized indexed events;
it provides no new authentication boundary. Stored-only outputs remain available
to an authorized receipt read and absent from content search.

Line-format bytes and receipt JSON remain unchanged. Message snapshots hide later
appends and refuse foreign scope, independently created authority forks and invalid cursors. Compaction preserves raw
intent/IDs while disposable physical cache/cursor identities can require rebuilding.
A byte-identical backup with the same key/identity is not independently freshened
or protected by an external witness in this local capture profile. The existing
Capture1-to-Record ingest bridge does not accept Messages yet.

## Verification boundary

Synthetic tests cover arbitrary binary/pretty JSON/SSE-shaped bytes as opaque
bodies (not protocol conformance), empty messages, exact original retry, policy
withholding, indexed eligibility, wrong mode/key/identity/marker/tag/schema,
coverage/capacity refusal, bounded exports, compression, compaction and actual
process SIGKILL before write, after durable write, before ACK and after ACK.
Final source-bound test/lint counts are recorded in the review handoff. There are
no real private logs, provider calls, AWS operations or production acceptance.

The final engine-only source passed 189 engine tests (10 existing/helper tests
ignored), 83 capture unit tests, 9 capture integration tests and strict engine
lib/tests Clippy. The message selection contributes 11 active tests and one
ignored process-helper fixture. Actual old-line LF validation and old relative
parent handling each fail their targeted semantic controls; the restored source
passes both. Negative and positive builds use separate physical Cargo targets:
an initial shared-target positive run reused a negative-control binary and was
discarded, not counted as passing evidence.

This component is stacked on the Capture1 ingest-bridge source. It does not
establish compatibility with the later read-service existing-only constructor;
the cumulative integration must retain that constructor's missing/partial-root
refusal and run its actual read-service stories separately.
