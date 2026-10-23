# Explicit journal attachments (review candidate)

The Node 24 `@oojuro/v2-client/attachments` module connects the existing user-file protocol to a small journal event. This candidate is not deployed and is not automatic local-file capture. An application must select bytes with the user's consent and provide a separate ordinary native file-write/read credential, a metadata-only capture credential, and a project read credential. Capture keys do not gain file read or write authority.

## Credential scope

| Credential | Current authority |
| --- | --- |
| Integration capture key | Discover its producer and provision/append to its bound session journals. No history reads or Blob file reads/writes. |
| Project read key | Read currently attached journals' complete retained history and their visible linked files, subject to current membership and revocation. |
| Ordinary native writer grant used by the uploader | Read, write and retry for its granted writer. This includes that writer's journal records and Blob data; it is **not a file-only or upload-only capability**. |

The current helper requires the ordinary writer grant for the capture producer.
Supply it only to a trusted uploader that may access all data under that writer;
do not give it to a consumer intended to see only one file or one project. Keep
it out of agent prompts/configuration unless that broader authority is intended.
A narrower file-only grant is not implemented. This is a separate explicit
grant; existing capture credentials are not broadened.

## Write and read

1. Select one file explicitly. Compute its exact byte count and SHA-256 with bounded reads. Persist its descriptor and the selected source privately before transport. The module never opens or discovers local files.
2. Construct `prepareJournalAttachment(uploader, capture, descriptor)` and call `send(asyncByteSource)`. Descriptor fields are documented in `sdk/typescript/attachments.d.ts`. Persist the returned receipt privately.
3. Read the event through project records/search. Its `oojuro_attachment` attribute identifies the immutable file version. Search indexes the display label, not the file contents.
4. Call `downloadJournalAttachmentTo(projectClient, receipt, transactionalSink)`. `write` receives tentative verified parts; `commit` is called only after full SHA-256, length and a final current manifest/authorization check. `abort` must remove tentative output. A filesystem sink should create a private temporary file, fsync, then atomically rename on commit.

Each transfer uses 256 KiB parts. The linked attachment API accepts at most 16 MiB / 64 parts; the older generic file protocol has its own 256 MiB limit. Neither is a measured hosted capacity promise. One source iterator may retain its own buffers; the caller must keep those bounded too.

Use a new upload ID and event ID for each new version, including the same display name. The protocol logical file name is `oojuro-attachment/v1/{journalId}/{uploadId}`. Existing bytes are referenced through their exact manifest and part revisions; a changed manifest cannot silently replace an event's original evidence. Project download requires the event still be visible in that logical journal and current project membership, credential and native read guards remain valid at each retained body boundary.

## Failure, deletion and limits

There is no automatic retry or durable spool. On an unknown outcome, retry the exact persisted descriptor with the exact selected bytes. Do not allocate fresh IDs. Accepted parts are create-only and exact retries reuse their revisions. File publication precedes the event: an interruption can leave unlinked parts or a published unlinked file. This is not a cross-resource atomic transaction.

The existing `@oojuro/v2-client/file-deletion` selected-version protocol can remove that exact logical file manifest and selected part revisions with an ordinary write credential. Removing the manifest makes the linked download fail. Removing a journal event or detaching its journal removes that project's retrieval path; it does not physically purge all originals. There is no automatic orphan collection, retention SLA, or cross-file deduplication in this candidate. Keep failed-upload descriptors for deliberate cleanup.

The initial joined qualification uses an explicitly enrolled physical journal.
The [session SDK candidate](SESSION_SDK_SETUP.md) adds an explicit already
provisioned `session` descriptor for integration keys: the journal/run/agent and
physical source must match before selected bytes are consumed, then the event
uses the exact bound session route. Missing/foreign descriptors refuse before
upload. Legacy descriptors and fixed-journal behavior remain compatible. This
new path now passes its dedicated actual Node/HTTP/PostgreSQL fixture, including
exact bound download, lost part/link ACK retries and current project/key refusal
through retained EOF. This synthetic joined result does not qualify actual
owner setup, process restart, automatic attachment spooling or deployment.

All MIME types are opaque original bytes. PDF/Office parsing, OCR, image understanding, audio/video transcription, executable preview and derived-content search are unsupported here. Optional future extractors must stay outside the core engine and link their output to the exact source version. Never claim arbitrary binary bytes become natively understood.

The hosted Blob wire representation currently serializes bytes as a JSON numeric array before encryption/compression. Exact originals once does not imply 1x physical storage: manifests, transactions, encrypted objects, audit and incomplete objects add overhead. Measure those separately from the engine archive codec and pricing.
