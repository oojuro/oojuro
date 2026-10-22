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
