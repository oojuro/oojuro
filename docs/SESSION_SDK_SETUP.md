> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/journal/blob/main/docs/SESSION_SDK_SETUP.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Session capture SDK setup (review candidate)

This source candidate adds the integration-key session contract to the existing
TypeScript/JavaScript client. Core repository source and review topics are public;
the npm package remains `private: true`, with no package release or production
activation included. Node 24.4+ on a POSIX filesystem
is required for the optional durable `node:sqlite` spool. The core engine gains
no dependencies.

Use the [CLI installer](AGENT_SESSION_INSTALLER.md) for existing Codex/Claude
transcript capture. Use this SDK for an application you instrument explicitly.
It does not discover/open local files, import past history or modify agent
configuration. Capture and project read credentials remain distinct.

## One durable journal per application session

Read an integration key from your protected local credential input. Keep it out
of chat, argv, logs and agent configuration. Construct `OojuroCaptureClient`
with that key, then `CaptureSpool.open({directory, client, sessionLabel})`.
`sessionLabel` is the role used for new journal names; the default is the
integration key's discovery label. Existing run labels never change when the
role changes. The directory is absolute, private and owned by you.

Each enqueued `CaptureEvent` supplies the application's **stable 64-hex run ID**,
a stable event ID and complete original input. Hash a real provider session ID
with the adapter/domain if necessary, and keep it stable across restart/resume;
a genuinely new session uses a new run ID. Persist the source event identity and
selected content before treating them as captured. `enqueue` atomically saves
the original event plus its first run/label intent; it returns local queueing,
not a server receipt. For example, using the source checkout:

```js
import { OojuroCaptureClient } from "./sdk/typescript/index.js";
import { CaptureSpool } from "./sdk/typescript/capture.js";

const client = new OojuroCaptureClient({
  baseUrl: "https://oojuro.com", token: process.env.OOJURO_API_KEY,
});
delete process.env.OOJURO_API_KEY;
const info = await client.discover();
const spool = await CaptureSpool.open({
  directory: "/absolute/private/application-capture", client,
  sessionLabel: "Code reviewer",
});
// applicationEvent has persisted eventId/runId and intentional original input.
spool.enqueue({ ...applicationEvent,
  input: { ...applicationEvent.input, agent_id: info.agent_id } });
const acknowledged = await spool.flush();
const acceptedRoute = spool.session(applicationEvent.runId);
spool.close();
```

Supply the application's actual event; the example is not a transcript parser.
Do not log client configuration, originals or credentials during setup. `flush`
persists its one-run batch identity before provisioning. The same run/label is
retried after a lost setup response, and the accepted physical source, agent,
run, logical journal and exact relative events path are validated/persisted
before sending the unchanged framed event. New runs get distinct journals;
resume reuses the mapping. The physical source project and its Root3 namespace
are separate IDs and are both pinned.

Enqueue accepts individual events. Flush deliberately packs each persisted batch
for one journal/run. An explicit `prepareBatch` with a session descriptor refuses
mixed runs or agents before effects; it never splits a caller's supplied batch.
The original durable pending request/events survive unknown outcomes and offline
reopen. No timer or global shutdown hook implicitly flushes or discards data.

Session mappings are retained and bounded to 1024 per spool; the backend's
account/source limits are independently enforced, so this is not a capacity
promise. New integration spools use an explicit v2 private schema/binding.
Existing legacy v1 schema/identity/route/frame behavior remains unchanged and can
reopen offline. A legacy spool cannot silently become a session spool: use a new
private directory for that deliberate setup change.

### Key replacement limit

The first session-spool candidate refuses key rotation while an old request is
pending. Native related-key replacement may resume a bound run, but a pending
request receipt belongs to its original actor. This SDK does not repack, allocate
a new request ID, or rebind an uncertain batch automatically. Keep its original
spool/credential for reconciliation. Owner-friendly replacement of pending
requests remains a separate acceptance gate. Legacy v1 key-rotation behavior is
unchanged.

## Low-level explicit session calls

Applications with their own durable store can use:

```js
// Persist runId + label before this mutating request.
const session = await client.prepareSession({ runId, label }).send();
// Persist the accepted complete route, original request ID and exact events.
const prepared = client.prepareBatch({ requestId, events, session });
const receipt = await prepared.send();
```

Retain the original run/label, request ID and event bytes after an unknown
outcome. The SDK makes no automatic retry. Provisioning requires session-mode
discovery and validates the exact canonical `/api/capture/journals/ID/events`
path; it cannot redirect events to a server-selected origin or arbitrary path.
`captureSession` validates a descriptor's shape; a descriptor/ID is not a grant.
Current server authority still controls every operation. Legacy fixed-journal
calls without a session retain their existing `/api/capture/events` behavior;
integration keys must use the explicit bound route.

## Previous history and intentional files

Use a separately authorized `OojuroProjectClient` or
[guided MCP setup](HOSTED_MCP.md) to read previous attached sessions. The project
read key sees current journal membership and retained history, including earlier
sessions. Capture authority grants no reads or project administration. Metadata
`records` is enough to verify access; do not run a metered search just to test
credentials.

For an explicitly selected attachment, obtain/persist its accepted session first
(for example `spool.session(runId)` after a successful flush). Pass that route as
`session` in `JournalAttachmentDescriptor`, and set its `journal`, `runId` and
`agentId` to that same logical journal/run/agent. The helper validates that match
and the separate file uploader's physical source/ownership before consuming
selected byte parts. It then publishes the immutable file version and links a
small event through that session's exact bound path. A separate ordinary
file-write grant and `consent: "intentional-user-file"` are still required.
That ordinary writer grant can also read and write the producer writer's other
journal/Blob data; it is not file-only or upload-only. Give it only to the trusted
uploader that should have that scope. Capture keys gain no file access. See
[credential scopes and attachment limits](JOURNAL_ATTACHMENTS.md).

The helper has no automatic attachment spool. Persist the full descriptor,
accepted route and exact selected source version; unknown outcomes require the
same descriptor/bytes. Interrupted publication/linking can leave unlinked files.
A route is not authority and can become unusable if its current grant changes.
This does not make binary contents searchable or understood: only the event
label is indexed; parsing/OCR/transcription/preview remain unsupported.

## Copy-paste application setup prompt

> Connect this application's new agent activity with the checked-out Oojuro
> session SDK and private durable spool. Read my integration key from its
> protected local file; keep it out of chat, argv, logs and source control. Use
> the real stable provider session/event IDs, one new journal per new session,
> and preserve the mapping on resume. Keep original pending events and labels
> after uncertain responses. Preserve any existing integration and agent
> configuration; show what will be connected before instrumenting new events.
> Configure previous-project history only if I separately supplied its project
> read grant. Upload files only when I explicitly select them and provide the
> separate file-write grant. Verify synthetic new capture and exact prior
> context/file retrieval against the intended service, and report each result.
> Do not import existing history, publish packages or deploy the application.

## Qualification

Runtime `e42553d885f76552928e447ce4999efcb98d982b` passes 111 Node tests, strict
declaration checks and offline npm pack/install/export checks. The dedicated
actual Node/HTTP/PostgreSQL fixture also passes: two logical journals, three
transcript events, stable resumed routing, exact prior context, a bound selected
262145-byte file over two parts, and accepted setup/event/part/link ACK losses
retried without changing original intent. Capture read/file access and mixed-run
dispatch refuse. Project detachment and key revocation refuse fresh downloads
and retained part bodies through clean EOF. This is disposable PostgreSQL with
TLS/SCRAM, native test authority and real loopback HTTP; no provider deployment.

The source/SDK subtree and module digests are pinned in the dedicated hosted
fixture. SQLite handles reopen twice in one Node process; actual process/host
restart is not claimed. Earlier CLI/MCP/physical attachment gates remain separate
evidence. Real owner history, model-driven agent calls, systemd, owner-friendly
pending-key replacement and production activation remain acceptance steps.
The [dated qualification report](session-sdk-qualification-2026-10-23.json)
records identities, checks and limitations.
