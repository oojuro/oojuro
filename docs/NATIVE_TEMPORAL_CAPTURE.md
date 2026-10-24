> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/NATIVE_TEMPORAL_CAPTURE.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Explicit Temporal capture and SDK (experimental)

This is a native local recording lane, not a migration of Temporal history or workflow state.
The opt-in @oojuro/temporal package emits adapter observation envelopes into the actual
encrypted CaptureLog. It creates no plaintext spool, file-shaped WAL, provider request or
cloud forwarding. Existing Codex/Claude/Pi identities and legacy discovery stay separate.

## Native boundary

Create a private native root with setup --harness temporal, an explicit writer/agent and
the existing fail-closed policy snapshot. temporal-append accepts a bounded JSON request
only on stdin and requires --allow-raw-source before reading stdin. It carries a canonical
stream descriptor (namespace/workflow/run/caller generation), its SHA-256-derived 16-byte
source ID, an exact byte cursor and one bounded v1 JSONL event frame. A different source,
generation, cursor, sequence or reused event ID with changed bytes is refused.

Raw bytes, authorized parsed events, call/lookup policy state and allocator advance in one
sealed CaptureLog batch. Registration is opaque stream metadata, not invented filesystem
metadata. Original receipt lookup runs before refreshing or parsing the current policy.
An exact committed retry remains available when current policy is stopped/unreadable and
never reinterprets previously withheld text. A new frame still requires current enabled policy.
Each event has an immutable caller event ID; sequence is monotonic within this source.
Reported namespace/run/operation fields are provenance data, not authorization or verified causality.

temporal-position returns only current cursor/sequence and the original event's receipt
coordinates. The SDK uses those coordinates to reproduce the original sequence/cursor on
worker/task retry; native exact-byte comparison remains authoritative. The source descriptor
and the event's complete original intent, including timestamp and JSON field order, must stay
stable. Event ID reuse with changed payload refuses rather than allocating duplicate records.

The parser accepts typed input/output/thinking/system/tool-call/tool-result observations.
Calls retain bounded policy state, results must name the matching retained call/tool, and
withheld calls keep results withheld after policy relaxation and restart. Oojuro lookup
calls/results remain stored-only. Raw consent is independent of parsed record eligibility.
Original harness/service Event History bytes are not captured by this observation protocol:
the byte-exact raw source is the JSONL envelope the adapter emitted.

## Node and Temporal composition

NativeTemporalRecorder.prepare freezes a bounded JSON event and original native request.
PreparedTemporalEvent.send makes one attempt and validates the entire native ACK after clean
process exit. No retry is automatic. Unknown outcomes retain the original prepared request
through TemporalCaptureError.retry; errors contain static codes, never event/provider bodies.
After process death, reconstruct the same immutable input; position lookup supplies the original
cursor/sequence, and a changed intent still fails closed.

The local child uses argv only for the owned binary/root/command/consent. Event bytes are stdin;
stderr is discarded, stdout is bounded and checked, and an owned child is killed on deadline.
There is no shell invocation in the transport. The queue has explicit bounds; stats/diagnostic
callbacks report drops and failures, and callback errors cannot change an Activity outcome.

createWorkflowSinks sets callDuringReplay=false, but this is best-effort one-way telemetry.
The real Temporal 1.24 SDK explicitly says Workflow Task failure/timeout may call a sink again
even with that setting. Sinks have no Event History receipt/retry and cannot return an ACK to
a Workflow. The workflow entry point imports only workflow-safe SDK code, and
recordWorkflowEvent returns an offered/not-offered boolean, not a storage acknowledgement.

createActivityInterceptor is also a diagnostic observer: it queues start/result observations,
never waits for storage in the business path, and returns the original result or throws the
original error. It copies selected namespace/run/activity/attempt metadata, not task tokens,
headers or arbitrary Context fields. Its valid time is explicitly the current attempt's
scheduled timestamp, not a claimed completion timestamp. Unsupported JSON values create
observable diagnostic failures. An explicit awaited recorder.record call is the path for a
caller that actually needs a durable local recording ACK.

## Evidence and limits

Synthetic native tests exercise raw/parsed atomicity, exact-byte retry before changed policy,
cross-generation/cursor/sequence refusal, call-denial/lookup survival, projection failure and
actual SIGKILL before the caller receives ACK. Real Node-to-native tests exercise native setup,
capture/reopen/search/raw export, an actual dropped native ACK followed by exact retry with an
unreadable policy, changed-intent refusal, bounded queue reuse/drop, repeated sink invocation,
selected Activity metadata and unchanged business outcomes. No Temporal Cloud call is involved.

The full capture library passed 83 tests (including 8 Temporal tests); 13 existing native CLI
tests and 10 actual Node-to-native tests passed. The two observer review regressions each
failed in an isolated pre-fix copy and passed with the fixes. Owned capture Clippy passed
with warnings denied; existing dependency warnings remain. Independent source review found
no remaining concrete P1/P2 in this bounded slice.

The package declarations are checked against the already installed Temporal SDK 1.24.0.
This does not prove a live Temporal Worker, complete replay conformance, OTLP capture or cloud
acceptance. Raw is local; no original payload is silently uploaded.

This first slice inherits CaptureLog's 100k-event/128MiB parsed-metadata bounds. Event receipt
lookup scans retained batch metadata; it is not a 10M/100M or lifetime-scale claim. Raw size
and process memory remain separate limits. Claim-check codecs, authorized recall tools,
automatic forwarding, real Worker/replay acceptance and launch-version compatibility remain
separate work. No provider resources, tokens, charges, deployment or production cutover.

References checked October22,2026:
- https://docs.temporal.io/develop/typescript/platform/observability
- installed @temporalio/worker 1.24.0 lib/sinks.d.ts and lib/interceptors.d.ts
- installed @temporalio/activity 1.24.0 lib/index.d.ts
