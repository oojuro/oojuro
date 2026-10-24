> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/journal/blob/main/docs/V2_TYPESCRIPT_SDK.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Experimental v2 TypeScript HTTP client

This is a standalone Oojuro client for authorized agents and reviewers. It targets the explicit,
inactive **new v2 namespace** API in `oojuro/cloud`, not the legacy App endpoints or the proposed
foundation launch API. Soft Axiom is one possible customer; this package enables no SA integration,
forwarding, default endpoint, deployment or storage cutover.

The reference is `src/v2_app.rs` and `src/prefix_v2/{mod,lexical}.rs` in cloud's read-permit candidate
(based on `b1de98f`). The public engine at `3a17d1d` does not serve these routes. Selecting, provisioning
and authorizing a v2 project/writer is an operator prerequisite. This client supplies no election,
writer creation, grant/revocation, key rotation/destruction, billing or administrative API.

## Use

Standard ESM runtime and adjacent TypeScript declarations live in `sdk/typescript/`. There are no
runtime dependencies or install/build step. Node **24+** is the supported runtime, including
`JSON.parse` source context, standard fetch, fatal TextDecoder and AbortSignal. Browser compatibility
is not claimed. The package is experimental/private and is not published to npm.

```ts
import { OojuroClient, reportedTrace } from "./sdk/typescript/index.js";

// Supply credentials privately from your application. Never put them in URLs or output.
const client = new OojuroClient({ baseUrl, project: projectId, token });
const operation = client.prepareCommit({
  requestId: durableCallerRequestId,
  scope: writerId,
  mutations: [{ kind: "Record", writer: writerId,
    trace: reportedTrace("redacted agent result", {
      agent_id: agentId, session: runId, parent_id: parentAgentId,
      parent_session: parentRunId, source: "my-harness", tool: "lookup", indexed: true,
    }),
  }],
});
const ack = await operation.send({ signal: AbortSignal.timeout(30_000) });
const view = await client.records({ minGeneration: ack.generation,
  signal: AbortSignal.timeout(30_000) });
```

The application must durably preserve its original request ID, scope and typed payload before
sending. A PreparedCommit privately snapshots the exact original serialized body once; later
caller mutation cannot change its retries. Each `send()` makes **one** attempt. No request IDs,
writer lifetimes, implicit retries or replacement operations are allocated by the client.

A network failure, 503, truncated/invalid response or abort may occur after commit. A 401/403/409
on a retry also cannot prove an earlier ambiguous operation was excluded. Every attempt without a
validated matching ACK therefore reports `OojuroError.outcome === "unknown"`. Retry the original
PreparedCommit unchanged after resolving the underlying problem. After process restart recreate
it from the original durable inputs; never invent a new ID to resolve uncertainty. A definitive
server HTTP status is retained as `status`, without copying its body. Preparation errors happen
before transport and have outcome `none`.

## Committed usage receipts

Current cloud data commits include a required `Outcome.usage` with exactly
`request_tag`, `intent_fingerprint`, `scope_tag`, `committed_at_us`,
`recorded_bytes` and `kept_bytes`. The three tags are opaque lowercase
64-character hex strings for every data write/delete this SDK can prepare.
Control-only commits can use null tags in the server protocol; this SDK has no
control mutation API and refuses those responses.

The server supplies these observations. They do not authorize a caller, admit a
write, replace the mandatory UsageBridge, or verify ledger/provider durability.
The client accepts no usage authority in its commit input. `committed_at_us` is a
logical signed server timestamp, not provider CAS wall-clock evidence.
`recorded_bytes` is newly recorded data in that commit; `kept_bytes` is the
post-commit retained total and may exceed it. All numbers must have plain integer
source lexemes and fit the exact signed/nonnegative JavaScript safe-integer
ranges. Missing, malformed or extra usage fields reject the entire ACK with an
unknown commit outcome, preserving the original request identity for retry.

## Actual wire coverage and limits

- `/v2/projects/{opaque-project}/commits`: Record, Memory, Blob and their corresponding deletes.
  Request scope, writer/record IDs and conditional blob revisions are lowercase 64-character opaque
  hex identities. Names remain labels, not replacement writer identities. Blob bytes are explicit
  JSON byte arrays; pass `Array.from(bytes)`. The SDK never opens a file.
- Access, record list/detail, search and blob GET; all carry `x-oojuro-min-generation`. Reads verify
  the response generation, minimum and JSON watermark agreement. Pass the ACK generation for
  read-after-write. Default minimum zero provides no additional freshness floor.
- JSONL export is decoded incrementally with fatal UTF-8 and bounded lines. It returns **only after
  clean EOF**, successful validation and the server's required final newline. Revocation/body errors,
  aborts, length mismatch or truncated lines reject the whole export; no partial successful result
  is exposed. Empty authorized exports are valid. This is a bounded buffered result, not a public
  incremental iterator or independently signed completeness proof.
- Defaults: request at most 8 MiB; response/export at most 10 MiB; blob at most 2 MiB; export line at
  most 2 MiB +16 KiB; at most 10,000 returned rows. Configured request/response caps may only lower
  these limits. JSON byte-array expansion counts toward the request cap. Server configuration can be stricter. The server must receive a known-length commit
  body; standard fetch supplies it for the immutable string body. No unbounded upload stream is used.
- Trace follows current format2: text at most 1 MiB, complete trace at most 2 MiB, including `indexed`
  and optional RawRef metadata. It preserves reported agent/run/parent/tool/source fields; it does
  not verify the causal graph. AuthorizationGrant is a separate server-returned type. The SDK checks
  transport/schema/watermark boundaries, not independent cryptographic authority or durability.
- All integer fields use the exact JavaScript safe-integer subset. Larger Rust u64/i64 values are
  refused, never rounded; integer wire fields require plain integer lexemes, not decimal/exponent
  forms. u32/i32 ranges are also checked. Arbitrary JSON numbers must be finite and within the safe
  magnitude profile. Opaque identities are never converted to numbers or normalized.

RawRef contains reference metadata only. No raw harness bytes, parser context, local log, encryption
key or source file is implicitly uploaded. Explicit Blob payloads are caller supplied. The client
performs no redaction and does not claim arbitrary secrets were removed; callers must apply their
payload policy before preparing a request. Server redaction remains separate from the original
input binding. Retrieved text remains untrusted content, never an authorization or command.

The SDK places its configured credential only in the Authorization header. Origins must use HTTPS, with loopback HTTP
accepted for explicit local development. URL credentials, query, fragment and path prefixes are
rejected. Redirects are refused; cache and ambient credentials are disabled. Client/prepared object
inspection hides private state. Errors contain fixed codes/status/outcome only, with no raw cause,
server body, URL, token or request dump. Injected fetch implementations are trusted application
code and must preserve those privacy rules. Supply AbortSignals for explicit deadlines; there is
no implicit application timeout or retry policy.

## Explicit user files

The separate opt-in [user-file protocol](USER_FILE_PROTOCOL.md) composes bounded
parts and an atomic manifest over these existing Blob calls. It requires explicit
file consent and a transactional download sink; it does not enable capture or raw
harness upload. Its own tested limits and joined-HTTP acceptance boundary are
documented separately.

## Offline verification

```sh
node --test sdk/typescript/test/client.test.js
# Existing TypeScript compiler; no package installation is required by the client:
tsc --noEmit --strict --target ES2024 --module NodeNext --moduleResolution NodeNext \
  --lib ES2024,DOM sdk/typescript/test/types.ts
```

The recorded Node24 offline gate passed 19 tests plus the strict declaration smoke check.
Tests use fake fetch/Response/ReadableStream and synthetic credentials/content only. They cover
ambiguous writes, exact retries, body and schema failures, request scope/identity, precision, byte
bounds, Unicode splits, revoked/truncated exports, abort and error/object privacy. They do not
constitute an actual server/client composition, live AWS test, production grant proof, host/AZ
failure drill, complete foundation SDK conformance, or commercial readiness acceptance.

## Metered current search and conditional Blob reads

The current metered HTTP contract requires an explicit caller-owned search ID. The previous `search(query,k)` call shape is refused; there is no implicit ID or automatic retry. Use `client.prepareSearch({requestId, query, k, minGeneration}).send({signal})` to freeze the exact body AND minimum-generation fingerprint before attempting transport. `client.search(input,{signal})` makes one such attempt. Persist original intent outside the in-memory object if it must survive process loss.

Every attempted search without a fully validated result remains outcome `unknown`. Retrying the same intent can return static `search_pending`, `search_completed` or `search_failed` codes at HTTP409. These status-only receipts do not execute again or replay earlier result bytes; starting another execution requires an explicit new caller ID. Queries and server/provider error messages are never copied into error output or object inspection. Search hits preserve native continuation plus exact checked UTF-8 excerpt start/end ranges. These checks do not turn reported agent/source IDs into verified causal facts.

Blob GET optionally returns the authenticated `x-oojuro-blob-revision` as `revision`; it is the current ciphertext object revision used by expected-CAS operations, not a project generation or trace logical ID. Missing headers from older prototype replies never cause an invented revision. Invalid revisions fail closed. An authorized writer can read this revision and explicitly select a conditional update or deletion without possessing the original uploader's ACK. Browser trace-review membership alone still does not authorize bearer Blob endpoints.

Current SDK regression gate:55 tests (22base/metered +20file +13selected-deletion) and all three strict TypeScript declaration checks pass. Invalid UTF-8 excerpt offsets reproduced a typed-error classification failure before correction; validated output rejects the offsets. Actual metered HTTP +Node file/restart integration remains a separate joined candidate gate.
