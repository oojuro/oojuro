# Hosted Journal framed TypeScript client

This additive experimental surface uses the same OojuroClient authentication,
fixed project origin, abort, bounded response, and private-safe error machinery.
It adds no enrollment, redaction, migration, automatic retry, or raw capture.

## Explicit operations

- journalDescriptorsPage takes the same root/query/page options as journalPage.
  Each descriptor contains the outer opaque id, native record metadata WITHOUT
  text, text_bytes, and original generation. This permits bounded discovery of
  records too large for a full JSON page. Metadata and full-page cursors have
  different server projections; continuation cannot cross between them.
- prepareJournalRecordFramed takes the SAME explicit-consent JournalRecordInput
  as the JSON writer. The prepared operation privately owns the original frame.
  Each send makes one attempt and gives the transport a fresh owned byte copy;
  a mutated transport buffer cannot change a later original retry.
- journalRecordFramed({expectedRoot,id}, readOptions) returns
  {generation,value:JournalRecord}. Generation comes from the actual response
  header. No untransmitted watermark is invented.

The framed writer detaches text through own-data property descriptors, counts
exact well-formed UTF8 bytes, snapshots/validates metadata, and only then allocates
one frame and uses encodeInto. Text is never JSON-encoded: 16 MiB of NUL text
stays 16 MiB on this path instead of expanding into JSON escapes. Invalid inputs,
accessors, custom prototypes, metadata/text budgets and malformed Unicode refuse
before frame allocation or network dispatch.

## Exact wire contract

POST /journal/records/framed uses application/vnd.oojuro.record.v1, identity
encoding, and exact Content-Length. The body is u32 big-endian metadata length,
UTF8 JSON metadata (1..64 KiB), then exact UTF8 text (0..16 MiB). Metadata has
request_id, consent, expected_root, writer, adapter, text_length, and native input
WITHOUT text. The server reconstructs the same canonical input as the JSON route;
same original request identity/input has the same receipt across media.

GET /journal/records/{id}/framed sends the exact media Accept plus
x-oojuro-journal-root containing unpadded base64url Root JSON. Response metadata
is {schema:1,text_length,id,record:<native metadata WITHOUT text>}, followed by
exact text. The client checks media/identity encoding, lengths, duplicate metadata
keys including escaped aliases, safe integer source lexemes, native metadata,
requested opaque ID and full UTF8. BOM text is preserved. No result is returned
before clean bounded EOF; truncation/abort/refusal returns no partial record.
Framed responses copy each delivered chunk immediately into one owned contiguous
buffer bounded by maxFramedResponseBytes. No borrowed parts list accumulates;
zero-length and SharedArrayBuffer chunks refuse. Legacy JSON collectors are
unchanged by this opt-in path.

## Separate budgets and limits

maxFramedRequestBytes and maxFramedResponseBytes independently default to, and may
not exceed, 16 MiB + 64 KiB + 4. They can be lowered. Calling a framed method is
explicit opt-in to these bounded allocations. These budgets do not raise existing
8 MiB JSON client request/10 MiB response ceilings, nor the 1 MiB Journal JSON POST
ceiling. Server limits can restrict further. Exact JavaScript safe-integer limits
still apply to native integer fields.

This is a bounded in-memory single-record transfer, not arbitrary file streaming.
Callers retain original input/request ID across unknown writes. There is no hidden
durable spool. Descriptor metadata/provenance remains caller-reported, and logical
deletion does not establish permanent erasure.

## Evidence

Focused Node fixtures cover full 16 MiB worst JSON escaping, mutated transport
buffers and recreated retries, exact empty/BOM/Unicode text, own-data preflight,
malformed/truncated/unsafe/duplicate frames, lower budgets, abort and original
JSON limits. The descriptor case consumes the actual Rust HTTP fixture from the
preceding JSON milestone. Final evidence: 13 framed/discovery cases and 71 preceding SDK cases pass with
strict cached TypeScript. The actual small-frame Rust HTTP fixture retains
request/response bytes and headers, plus framed-to-JSON identical-receipt and
changed-input conflict controls. Separate server-owned focused tests passed full
16 MiB POST/GET and the unread-body deadline; aggregate server lifecycle gates
remain separate. Source-before-fix borrowed-chunk mutation and cross-realm shared
buffer controls fail, then pass after correction. A hostile error-code/status
getter also remains inside fixed private-safe errors: normalization reads only
own-data descriptors and never invokes error accessors. No live provider, AWS, or production acceptance claim.
