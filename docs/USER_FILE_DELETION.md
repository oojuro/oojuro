# Explicit selected user-file revision deletion

This SDK slice removes a user-selected logical file revision, then optionally
cleans up exactly the original part revisions listed by that selected manifest.
It uses existing writer-scoped `DeleteBlob` CAS mutations and exact typed request
deduplication. It adds no HTTP routes, storage FORMAT, service, orphan scanner,
retention policy or cryptographic-erasure claim. Original upload/download APIs
retain their semantics and share one internal strict manifest validator.

The module is `sdk/typescript/file-deletion.js`, with adjacent declarations and
package subpath `./file-deletion`. This is an explicit bearer Blob operation.
Human redacted trace-review membership/cookies do not authorize deletion, and no
server-held human credential is exposed or accepted as a file-sharing policy.

## Select first, persist the original descriptor, then act

```js
import { planFileDeletion, prepareFileDeletion } from "./sdk/typescript/file-deletion.js";

// Read-only selection. Requires separate user permission to delete this file.
const descriptor = await planFileDeletion(client, {
  deleteId: durableCallerDeletionId,
  writer, name: selectedLogicalName,
  manifestRevision: selectedUploadReceipt.revision,
  minGeneration: selectedUploadReceipt.generation,
  consent: "delete-selected-user-file",
  signal,
});
// Application-owned durable storage; no credential is present in descriptor.
// Its logical name and selected manifest metadata remain private: do not log it.
await persistExactDeletionDescriptorPrivately(descriptor);
const operation = prepareFileDeletion(client, descriptor);
const logical = await operation.removeManifest({ signal });
// logical.logicalFileDeleted === true; parts remain until explicitly selected
// cleanup below. This does not mean a later replacement cannot exist.

// A separate explicit caller action; never automatic on manifest deletion.
const cleanup = await operation.cleanupParts({ signal });
// cleanup.cleanupComplete means matching original ACKs for every selected part.
```

`persistExactDeletionDescriptorPrivately` is application-owned, not a library
export. The descriptor stores only the exact selection: format, caller delete
ID, writer/name, selected manifest Blob revision, canonical manifest text plus
SHA-256, and explicit selected-file deletion consent. It contains part names,
lengths, digests and original part ACK revisions, but no file body or credential.
Treat this metadata as private and keep its durable integrity under application
control. Its digest detects accidental alteration; it is not a signature or
independent proof of the descriptor's authenticity.

The planner reads the current logical manifest, validates the exact bounded
schema/namespace, and performs no write. This bounded slice requires the caller's
original manifest revision from an upload receipt; the current Blob GET SDK
shape does not yet expose a revision. If that receipt is stale after replacement,
manifest CAS fails and cleanup remains blocked. A later revision-on-GET API may
supply that revision only after its response is validated and wired separately.

The caller supplies a fresh unique delete ID for the explicit selected deletion.
Do not overwrite, edit, re-plan or replace an unresolved original descriptor.
The prepared class snapshots it in RAM; safe inspection/JSON output hides its
private state and cannot serve as durable resume storage.

## Two explicit stages and their ACKs

`removeManifest` sends one original request whose only mutation is conditional
DeleteBlob of the selected logical manifest revision. A replacement that wins
first has a different revision and survives the CAS conflict. No part mutation
is sent by this stage, even when its ACK is received or lost.

`cleanupParts` is separately explicit and refuses to run until this prepared
instance has a validated matching ACK for its original manifest deletion. It
visits the selected ordered parts sequentially, using each selected original
Blob name/revision. Each part is one original-ID DeleteBlob CAS request. An
unknown, revoked, denied or conflicting attempt stops the pass immediately.
A changed part revision is preserved; there is no automatic fallback to its new
revision, no skip-as-success on absence, no replacement request ID and no cleanup
of unrelated uploads or unreferenced parts discovered elsewhere.

The deterministic manifest and part delete request namespaces include the caller
original delete ID **and selected canonical manifest SHA-256**. That binding is
necessary: a re-plan that reads a replacement cannot inherit a historical ACK
for an earlier selected manifest while cleaning the replacement's parts. Changing
the selection changes its identity and is not a retry of the original operation.
Only the original durably retained descriptor reproduces all original requests.

Progress distinguishes historical validated outcomes:

| Field | Meaning |
|---|---|
| `logicalFileDeleted` | This selected manifest revision has a matching delete ACK. A newer logical file may exist. |
| `cleanupComplete` | The explicit cleanup stage received matching original ACKs for every selected part revision. |
| `acknowledgedParts` | Count of selected part deletes acknowledged in this prepared instance. |
| `remainingParts`, `remainingPartIndices` | Selected parts still lacking matching ACKs, including unknown and unattempted ones. |
| `generation` | Maximum validated generation observed by these delete stages. |

Even an empty file keeps the stages explicit: manifest removal returns
cleanupComplete false; calling cleanupParts completes it without part writes.
An `OojuroFileDeletionError` includes bounded frozen progress and a fixed
code/status/outcome. It exposes no logical name, manifest, credential, response
body or raw cause. A pre-transport error has outcome none for that attempt; it
does not invalidate or release any prior unresolved original hold. The progress
ACK boundary remains authoritative for whether cleanup can start.

## Lost ACK and restart

If an attempt lacks its validated matching ACK, preserve the original descriptor
and stop. GET 404, an absent manifest/part, a denied retry or an unrelated newer
write cannot prove that original attempt was excluded and cannot free the hold.

After restart, recreate the prepared deletion from the exact durable descriptor,
then explicitly replay removeManifest first. The server can return the original
typed outcome even after that manifest is gone. Only that matching ACK unlocks
cleanup. Explicitly replay cleanup from part zero with its original IDs/revisions;
known original deletes deduplicate without deleting replacements at the same
names. If retry authority is missing, revoked or inconsistent, retain the hold
and report remaining work; never invent a success from absence or a new ID.

No automatic retry, persistence, identity allocation or housekeeping is hidden
inside either stage. Current authorization is checked by the existing API on
both manifest removal and each part cleanup request, including exact retries.
Commercial routing must use the mandatory UsageBridge; this SDK adds no alternate
write path. The parent joined metered HTTP/restart fixture remains its acceptance
gate. Local fake-fetch tests do not establish that composition or provider state.

## Race and lifecycle boundaries

Protocol replacements **must** use a fresh upload ID and part namespace. After
logical deletion, a fresh replacement can survive while the old selected parts
are cleaned; the new manifest and its parts are never named by old cleanup.
Original per-part CAS also preserves any direct revision replacement before its
selected delete commits. A replacement written after an original part delete
is preserved by exact old-request deduplication on retry.

A direct same-writer Blob client can intentionally publish a manifest referencing
old selected parts or mutate that data. Existing APIs do not atomically assert
logical-manifest absence while cleaning parts, enforce immutable part privileges,
or maintain a global reference graph. This helper does not protect arbitrary
manually shared references across manifests. Cleanup means the user's explicitly
selected old revisions, not a server-wide reachability or garbage-collection rule.

Logical DeleteBlob removes live references. Retained encrypted objects/backups,
physical reclamation, billing treatment, retention and cryptographic destruction
remain their existing separately authorized policies and writer/project lifecycle
operations. Neither logicalFileDeleted nor cleanupComplete is an erasure proof,
a zero-storage-cost proof or an assertion that no later data exists under a name.

## Offline verification

Tests model exact typed request persistence and current per-request permissions
with synthetic fake fetch only. They cover staged >2 MiB selection, original
manifest/part ACK loss, restart replay without a manifest, raced manifests and
part revisions, fresh replacement survival, absent retry receipts, revocation,
strict descriptor/manifest validation, private errors and empty files. A reviewed
historical-ACK/re-planned-replacement race fails on the unbound source and passes
after binding both request namespaces to the selected manifest digest. The final
Node24 gate passes 52 tests (39 existing upload/download/client and 13 deletion),
all three strict TypeScript declaration smoke checks pass, and independent source
review cleared the fix. The reviewer did not run or certify the joined HTTP gate.

```sh
flock /tmp/sa11-verification.lock timeout 180 node --test \
  sdk/typescript/test/client.test.js sdk/typescript/test/files.test.js \
  sdk/typescript/test/file-deletion.test.js
flock /tmp/sa11-verification.lock timeout 60 node "$EXISTING_TSC" \
  --noEmit --strict --target ES2024 --module NodeNext --moduleResolution NodeNext \
  --lib ES2024,DOM sdk/typescript/test/types.ts \
  sdk/typescript/test/files-types.ts sdk/typescript/test/file-deletion-types.ts
```

No provider/AWS operation, deployment, service change, Notion update or original
checkout edit is performed by this change. Actual metered HTTP/restart and live
hosted acceptance remain separate evidence.
