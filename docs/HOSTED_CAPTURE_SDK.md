> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/HOSTED_CAPTURE_SDK.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Hosted capture SDK and durable queue

Private candidate, Node 24.4+ on POSIX. This extends the existing
`sdk/typescript` package; it is not published. Hosted append-only capture uses
`OojuroCaptureClient` with an `ojc_` key. The older `OojuroClient` and its
`ojv2_` project protocol remain explicitly separate versions. Capture keys cannot
read or administer projects. Use the separate [project read adapter](PROJECT_READ_SDK.md)
with an `ojp_` key for common metadata pages and exact content.

The server must support discovery of `producer_id` alongside journal, agent and
key IDs. This producer is stable across **Replace**, while a newly created
producer can differ. The client binds queued originals to HTTPS origin, journal,
agent and producer; replacing a key for that producer changes the batch request
ID while retaining exact event/run IDs and input. A different binding is refused.

```js
import { randomBytes } from "node:crypto";
import { OojuroCaptureClient } from "./sdk/typescript/index.js";
import { CaptureSpool } from "./sdk/typescript/capture.js";

const client = new OojuroCaptureClient({
  baseUrl: "https://oojuro.com",
  token: process.env.OOJURO_CAPTURE_KEY,
});
// Parent directories must be owned/protected; this creates a private 0700 leaf.
const spool = await CaptureSpool.open({ directory: "/private/app/oojuro-spool", client });
const run = randomBytes(32).toString("hex");
spool.enqueue({
  eventId: randomBytes(32).toString("hex"),
  runId: run,
  adapter: "my-integration",
  input: {
    agent_id: spool.agentId, parent_id: null, valid_at: Date.now() * 1000,
    kind: "output", tool: null, status: null, exit: null, key: null,
    text: "Apply your capture/redaction policy before enqueueing.",
    indexed: true, attrs: {},
  },
}); // Local durable enqueue only, never a server ACK.
try {
  await spool.flush({ timeoutMs: 30000 });
} finally {
  spool.close(); // Leaves undelivered originals for next process; no implicit flush.
}
```

This is the supported delivery primitive for an integration's new-event callback.
Importing it does not observe arbitrary applications. Framework/CLI instrumentation,
automatic worker scheduling, customer source provisioning and release/install
acceptance remain separate work. Existing-history import must remain explicit.
The example endpoint is the target deployment, not a claim that its current
public domain already serves this candidate.

## Persistence, limits and failure handling

The queue uses the Node built-in SQLite driver, with no new dependency. SQLite
uses DELETE journaling, `synchronous=EXTRA`, secure deletion and bounded page count.
A committed enqueue persists original input; the batch request/key ID/selected
range is committed before HTTP. Only a complete validated ordered receipt removes
those events. A missing, invalid or truncated receipt leaves the original outcome
unknown and the queue retained. Exact retry sends the same body/request. Later
packing and key replacement retain event/run identities so server deduplication
can recover records without another capture charge.

Defaults: 64 MiB of serialized queued events and 10,000 events; configurable up to
256 MiB and 100,000 events. Quotas are checked inside the SQLite write transaction.
Page count also caps database growth with bounded overhead; freed pages can remain
allocated. JSON escaping counts toward the queue-byte cap even when raw wire text
is smaller. Native batches respect 100 events, 64 KiB metadata and 16 MiB aggregate
raw UTF-8. A full queue throws `spool_full`; it never evicts an unacknowledged event.
A changed original under a queued event ID throws `event_conflict`.

The first open discovers and retains the source binding. Existing queues reopen
offline so new events can still be retained during outages. Every flush rechecks
current discovery before sending; a revoked/unavailable/wrong producer cannot
silently drain a queue. Flush has one total deadline (30 seconds by default,
maximum five minutes). Failure rejects explicitly; call flush again to retry the
retained batch. No hidden timers, retry storm or global process-exit handler is
installed. Close refuses during a flush and otherwise retains all pending work.
Concurrent local processes use SQLite transactions; duplicate in-flight batches
retain the same request and are safe to retry. Each process still needs its own
run ID. A producer must preserve original event IDs when retrying an enqueue.

**The spool contains plaintext event content and metadata.** Use a local
fsync-capable filesystem, private ancestors, a 0700 directory and 0600 database.
Symlink files, hardlink aliases, unsafe owners/modes and unknown databases are
refused. The SDK does not encrypt the local filesystem or control machine backups;
configure those according to the content's retention policy. Apply redaction before
persistence. Enqueue and SQLite commits are synchronous and may block the Node
thread while storage syncs. No fsync durability promise applies to a filesystem or
hardware that fails to honor it.

The transport snapshots original input without getters/toJSON, preserves raw
UTF-8/BOM/NUL exactly, bounds response collection, requires clean EOF, rejects
redirects/cookies and redacts client/prepared/spool inspection. It does not echo
provider errors, raw credentials or input in failure messages.

## Verification

```sh
node --test sdk/typescript/test/*.test.js
```

The focused capture gate includes raw 16 MiB/NUL framing, immutable retry bytes,
invalid/truncated ACK retention, queue quotas/conflicts, metadata batch splitting,
offline reopen, producer-bound key replacement, private file checks and a real
HTTP child killed with SIGKILL after server acceptance. Restart recovers its exact
persisted request. This is a client process-crash check; it does not prove AWS
host/AZ recovery or representative throughput/storage economics.

Implementation references: [Node SQLite API](https://nodejs.org/docs/latest-v24.x/api/sqlite.html),
[Node FileHandle sync](https://nodejs.org/docs/latest-v24.x/api/fs.html#filehandlesync),
[SQLite synchronous modes](https://sqlite.org/pragma.html#pragma_synchronous).
