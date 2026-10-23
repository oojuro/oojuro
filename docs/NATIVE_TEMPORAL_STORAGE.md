# Temporal claim-check storage and recall (experimental)

Oojuro is independent agent storage and flight recording. These opt-in modules run outside
Workflow code; Soft Axiom is one consumer. They compose the actual prefix-v2 TypeScript client,
Blob file protocol, current permissions and metered search. They create no Temporal/AWS
resources and do not migrate account, wallet or workflow-state authorities.

## Claim-check codec

Import OojuroClaimCheckCodec from @oojuro/temporal/codec and install the same codec configuration
on every relevant Temporal Client and Worker through dataConverter.payloadCodecs. Never import
this module into a Workflow bundle or pass its client credentials/key as Workflow arguments.

Constructor options are client (the actual ClientOptions), writer, namespace, bindingKey (32
caller-protected durable bytes), consent: intentional-user-file, and optional thresholdBytes,
maxPayloadBytes and maxBatchBytes. This consent explicitly covers original Temporal payloads;
local raw harness consent does not grant any upload permission. The existing file SDK remains
the storage protocol. The binding key is not a server authorization grant or a decryption key.

The codec freezes the complete batch before the first upload. A canonical bounded binary
frame preserves each metadata value and data byte, including absent/null/empty payload states.
Map entries are canonicalized; original protobuf wire ordering or JavaScript prototypes are
not preserved. Threshold counts the complete canonical frame, so metadata participates in
offloading. Defaults are 256KiB threshold, 32MiB complete payload frame and 64MiB whole batch;
up to1024 items,128 metadata keys and64KiB aggregate metadata. Shared buffers are refused.
Intrinsic TypedArray access enforces real bounds before copying even with shadowed properties.

The content handle and upload ID are domain-separated HMACs of the configured project/writer/
namespace and the exact original canonical payload, using the caller-durable binding key.
The SDK 1.24 serialization context has no stable per-invocation/run identity; this codec does
not invent one. Its optional namespace is checked when present, and is never an authorization
source. Equal payloads within one configured scope intentionally share a handle, revealing
equality. No unkeyed payload digest is put in Workflow History.

Encode awaits every original part and manifest ACK before returning a reference. There are
no automatic codec retries. An unknown outcome is a static TemporalStorageError with
outcome=unknown. Recreate the codec with the same durable key/config and exact original
payloads to reproduce every upload identity. No RAM object is described as a durable journal.
Do not place nondeterministic encryption/compression before this codec if it changes the
payload bytes between attempts; changed bytes are a different storage intent.

The small History payload has encoding binary/oojuro-claim-check-v1 and a canonical reference
containing only version, keyed opaque handle, committed manifest revision, generation and MAC.
It contains no original metadata/data, unkeyed SHA, credentials, signed URL or arbitrary Context.

Decode authenticates all references against the configured project/writer/namespace before
any reads. Strict expectedRevision file reads pin the original manifest revision and every
original part revision, and require current authorization at initial manifest, each part and
final manifest read. Missing revision headers, same-byte replacement under a new incarnation,
changed pointer, revoked grants, tampering or incomplete EOF fail closed. File/full digest and
keyed original handle must agree before any payload is returned. Temporary bytes are wiped
without changing the returned owned payload. Caller output is not a delivery acknowledgement.

The durable binding key and Oojuro payload retention must outlive every Workflow History,
retry and replay that refers to them. Deletion or loss can make historical Workflows
unreplayable. No automatic expiration, cleanup, key rotation, public codec server or UI decode
endpoint is implemented here. This custom codec is not a claim to Temporal's separate preview
external-storage wire protocol.

## Authorized recall

@oojuro/temporal/recall exports OojuroRecall.prepare, createRecallActivity and a framework-neutral
createRecallTool. Credentials stay in the outside-workflow closure. The input requires a
caller-stable requestId and nonblank query; k/minGeneration are optional. The actual SDK
freezes the request ID/query/minimum and makes exactly one metered search attempt.
There is no blind retry or fresh-ID fallback. Repeating the same request after an unknown
outcome returns only the server's completion/status response, not a new search or old result
bytes. Callers retain the original logical-search identity and handle this uncertainty.

Results retain the server watermark, authorized opaque IDs and exact UTF8 excerpt ranges,
continuation, score bits, kind and reported valid time. A configurable whole ranked prefix
fits at most64KiB excerpt text and100 hits; omitted suffixes are explicit. No arbitrary full
trace, writer name, parser context or credentials are copied into the agent-facing result.
The tool schema is a neutral description, not an integration with a particular agent framework.
No retrieval-quality improvement or verified causal relationship is claimed.

## Verification boundaries

Synthetic FileSDK transport tests cover stable recreate/retry, changed bytes, >2MiB payloads,
opaque references, scope/MAC checks, original metadata/data, memory/accessor bounds, current
permission loss, exact revision pinning and refusal after same-byte replacement.
Recall tests exercise the actual SDK request/response validator, bounded UTF8 result prefix,
lost ACK and status-only retry, current denial, frozen intent and preflight refusal.
The complete owned run passed73 tests:59 TypeScript SDK tests (including strict revision
mode),9 codec cases and5 recall cases. Four isolated counterfactual variants each fail their
regression: mismatched header caps, ordinary TypedArray property reads, realm-specific shared
buffer checks and omitted part-revision pinning. Strict Temporal1.24 declaration composition
and the actual Temporal workflow bundler passed. These model tests are separate from the
real RustHTTP/SDK joined acceptance fixture, which is pending on this source pin.

Declarations compose with the already installed Temporal1.24.0 PayloadCodec,SerializationContext,
DataConverter and activity/worker types. No new package was installed. Source peer links only
reuse this checkout's actual TypeScript SDK. The package remains experimental/private;
publication, live Temporal Worker/Cloud replay, provider failure drills, retention guarantees
and launch compatibility are separate gates.

Official contracts checked October22,2026:
- https://docs.temporal.io/payload-codec
- https://temporal.io/changelog/external-storage-public-preview
- cached @temporalio/common1.24.0 PayloadCodec and SerializationContext declarations
