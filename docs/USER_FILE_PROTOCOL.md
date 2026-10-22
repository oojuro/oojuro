# Explicit user files over v2 Blob storage

This Node24 SDK slice stores an intentionally selected user file larger than the
existing 2 MiB Blob limit using bounded parts plus one conditional manifest Blob.
It composes the current encrypted writer-scoped Blob commit/read API; it adds no
HTTP routes, on-disk FORMAT, provider calls, service, capture hook or forwarding
policy. It does not upload raw harness logs or infer file consent from trace
capture/redaction. File transport requires explicit bearer Blob privileges.
Registered-human redacted trace review does not grant file access or expose the
server-held credential; no human file-sharing endpoint is added.

## Primary API

Runtime and declarations are `sdk/typescript/files.js` and `files.d.ts`, exposed
as package subpath `./files`. The module imports the existing `OojuroClient` and
uses its exact frozen `PreparedCommit` request bodies. There are no runtime
packages to install. Node24 is required; browser support is not claimed.

```js
import { OojuroClient } from "./sdk/typescript/index.js";
import { prepareFileUpload, downloadFileTo, FILE_PART_BYTES } from "./sdk/typescript/files.js";

const client = new OojuroClient({ baseUrl, project, token });
// Durably save this complete descriptor and exact original selected content
// before any request. Identities come from the caller, never from a retry.
const descriptor = {
  uploadId, writer, name: selectedLogicalName,
  expected: null, // Required: create-only, or the original manifest Blob revision.
  size: originalByteCount, sha256: originalFullSha256,
  consent: "intentional-user-file",
};
const operation = prepareFileUpload(client, descriptor);
const receipt = await operation.send(reopenExactOriginalFile({
  maxChunkBytes: FILE_PART_BYTES,
}), { signal });

// Application-owned transactional sink: write is tentative, commit publishes,
// abort discards tentative bytes. Neither the SDK nor sink cleans server parts.
await downloadFileTo(client, {
  writer, name: selectedLogicalName,
  minGeneration: receipt.generation, signal,
}, transactionalSink);
```

`reopenExactOriginalFile` and `transactionalSink` above are application-owned
interfaces, not library exports. Sources are async iterables of Uint8Array with
each chunk at most 262,144 bytes. A complete SHA-256 and exact byte count must be
known before sending; obtaining them and opening any user-selected file are
explicit caller work. The SDK never opens a local file or allocates a new ID.

`PreparedFileUpload` snapshots the descriptor primitives in RAM. It is not a
durable resume journal. Preserve the original descriptor, consent and exact
replayable bytes before the first attempt. Its safe inspection/JSON forms omit
private state; serialization of the object cannot recover that state.

## Protocol and atomic publication

Parts are exactly 256 KiB except the final nonempty part. There are at most 1,024
parts, giving a 256 MiB SDK bound. This is **not a server capacity promise**:
committed-prefix, usage-ledger, operation/state and deployment limits can refuse
an upload earlier. Nothing is silently truncated or cleaned to satisfy a limit.
Only the current bounded JSON Blob API is used; the JSON byte-array expansion
counts toward its request limit. No unbounded request/response or whole-file
buffer is introduced. One input chunk, one owned part, bounded wire buffers,
and at most 1,024 small manifest entries are retained by the SDK.

The caller's opaque upload ID fixes every request identity. Each part Blob name
also contains a SHA-256 binding of the full original writer/name/upload ID/CAS/
size/full digest/consent descriptor. Changing the original descriptor after an
ambiguous part ACK therefore conflicts with the original typed request even
when its initial bytes are identical. Every part is create-only (`expected:null`)
and has a distinct original request ID. A replay never replaces part bytes.

After every part has a validated matching ACK, the SDK verifies the complete
source length and full SHA-256 before submitting the logical file manifest.
(The last short part is sent only after that source verification.) One Blob
mutation atomically CASes the manifest at the hash-derived logical file name.
Required `expected:null` creates only; an intentional later version supplies
its previous upload receipt's manifest revision and a new explicitly persisted
upload identity. Existing blob GET does not expose its revision, so download
metadata is not advertised as an update-CAS receipt.

The manifest binds format, exact writer/logical name/upload identity, original
manifest request ID and expected revision, total bytes, full SHA-256, fixed part
size, and ordered part index/name/length/SHA-256/revision entries. Readers reject
unknown fields, duplicate/noncanonical JSON properties, noninteger lexemes,
foreign writers, duplicate/reordered parts and references outside the exact
descriptor-bound upload namespace. Parts cannot be arbitrary filesystem paths.
No filesystem path is ever evaluated by this protocol.

Part revisions are original receipt provenance only: Blob GET does not expose
or enforce those revisions. The writer can intentionally CAS/delete a Blob
using its ordinary privileges. This SDK treats part names as immutable by
convention and verifies every returned part's exact length and SHA-256; it does
not claim server-enforced immutable access or a separate file ACL.

## Lost ACKs, authorization and partial uploads

A send makes one forward pass with no automatic retries. A transport error,
invalid/truncated ACK, abort, or denied/conflicting retry after any attempted
write stops immediately with a fixed `OojuroError` whose outcome is `unknown`.
It sends no later part or manifest after an unknown ACK. Do not allocate a new
upload/request identity to resolve uncertainty. After fixing the underlying
condition, recreate the operation with its original durable descriptor and
replay the exact bytes from part zero. Existing typed request deduplication
returns the original outcomes, including when the manifest committed but its
ACK was lost. Changed replay bytes/descriptor conflict rather than overwrite.
Preparation failures occur before transport and have outcome `none`.

Every part and manifest is a separate request through the existing current-
authority submit/read path. Grant revocation can deny the next request. Server
read permits still guard each returned response body against current authority.
The SDK does not hold an immutable grant revision across requests: revoke then
regrant is fresh authorization for a later request. Upload authorization does
not follow from the presence of trace-review browser cookies or membership.

Commercial routing must send each part and manifest write through the mandatory
UsageBridge as ordinary metered Blob writes; this SDK adds no alternate billing
route. The separate joined fixture remains the acceptance gate for that composition. Partial uploads can
leave billable parts without a published file. There is no invented orphan
retention/deletion/GC policy, no abort-time server delete and no cleanup retry
identity. Lifecycle and erasure remain the existing writer/Blob operations and
separately authorized policy. Aborting releases client/iterator/response resources
and calls the local sink's abort; it does not erase remote objects.

## Verified download completion

`downloadFileTo` is the primary safe path. It reads and validates the manifest,
then reads each exact named part through the authenticated Blob API, carrying a
nondecreasing minimum generation. It verifies part length and digest **before**
calling sink.write. It checks full file length/digest after all parts, then
rereads the exact original manifest bytes with current authorization before
calling sink.commit. A changed logical pointer fails even if the replacement
points to identical file content. Any read, integrity, abort or sink error calls
sink.abort and never reports a completed file. The caller must implement actual
transactional sink semantics; the SDK cannot undo a sink that publishes in write.

`downloadFile` is an advanced API returning `{chunks,completion}`. Yielded chunks
are part-verified but remain tentative. Consume the iterator to EOF **and await
completion** before accepting any file. Early iterator return rejects completion;
ignoring completion cannot establish whole-file success. Source/sink iterators
must cooperate with the supplied abort signal and release their own resources.
No credential, private path/name, server body or raw cause is included in errors.

## Verification and remaining joined gate

Synthetic fake-fetch tests exercise >2 MiB exact reconstruction, replay after
part/final ACK loss, changed original descriptors, create/update CAS, partial
uploads without cleanup, per-step revocation, strict manifests, part/full digest
corruption, changed pointers, transactional sink abort and content-free errors.
The local Node24 gate passes 39 tests (19 existing and 20 file-protocol cases),
and the existing cached TypeScript compiler passes both strict declaration smoke
checks. A temporary copy with descriptor binding removed fails the changed-
descriptor-after-lost-ACK test; the real source passes that regression. Independent source review found and
cleared a Node Buffer aliasing issue and cancellation-before-first-next completion
hang. Reverting either fix in temporary copies makes its targeted regression
fail; the final source passes both. These models do not prove
actual HTTP runtime enforcement, ledger persistence, process restart or provider
durability. The separate joined actual-v2-HTTP + mandatory-UsageBridge fixture
must still demonstrate >2 MiB upload -> restart -> exact download, lost ACKs,
wrong-writer denial and no-bearer human-review-cookie denial.

```sh
flock /tmp/sa11-verification.lock timeout 180 node --test \
  sdk/typescript/test/client.test.js sdk/typescript/test/files.test.js
flock /tmp/sa11-verification.lock timeout 60 node "$EXISTING_TSC" \
  --noEmit --strict --target ES2024 --module NodeNext --moduleResolution NodeNext \
  --lib ES2024,DOM sdk/typescript/test/types.ts sdk/typescript/test/files-types.ts
```

No AWS/live/provider operation, deployment, Notion update or original-checkout
edit is part of this change. Passing these SDK tests does not establish the
remaining hosted durability, erasure, launch or primary-storage gates.
