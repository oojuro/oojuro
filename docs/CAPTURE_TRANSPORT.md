# Hosted capture transport

Capture uploads use verified HTTPS outside loopback, append-only capture
credentials, fixed-origin requests, and explicit request/event identities. Cloud
may decrypt authorized content for indexing and search. HTTPS plus encrypted
storage does not promise that the service operator cannot decrypt journals.

## Optional gzip

Identity encoding remains the default. Compression saves bandwidth but can cost
more CPU and increase latency on a fast connection. Enable it deliberately for
the workload and network being measured:

```js
const capture = new OojuroCaptureClient({
  baseUrl, token, compression: "auto",
});
await capture.discover();
await capture.prepareBatch({ requestId, events, session }).send();
```

The Rust hosted capture client opts in with
`OOJURO_CAPTURE_COMPRESSION=gzip`; unset or `identity` retains the existing path.
Set this in the capture process's environment when launching the foreground
capture command or configuring its service. Other values refuse configuration.
This variable changes transport only, never recording consent or retained data.

Successful authenticated `GET /api/capture` may return
`X-Oojuro-Capture-Encoding: gzip`. Its JSON schema is unchanged. A client enables
gzip only after the complete bounded discovery response and normal identity,
scope, expiry and limits checks succeed. Missing/combined capabilities keep
identity; a later failed discovery clears the remembered capability. Older
clients ignore the additive response header. A new client talking to an older
server stays on identity. Browsers must be permitted to read that response
header by their existing origin/CORS policy; an inaccessible header safely
leaves compression disabled. This change adds no CORS permissions.

An eligible batch has at least 1,024 framed bytes and saves more than 64 bytes.
The body is one complete gzip member containing the existing capture frame:
big-endian u32 metadata length, JSON metadata, then exact UTF-8 text. POST retains
`application/vnd.oojuro.capture.v1`, declares `Content-Encoding: gzip`, and gives
the compressed `Content-Length`. The SDK uses the platform CompressionStream;
without it, identity remains available. No new SDK runtime dependency is added.
Session creation stays identity JSON; responses stay bounded identity JSON.

Both encoded and decoded request bodies retain the 16 MiB + 64 KiB + 4-byte
maximum. The Cloud implementation must authenticate before collecting the body,
reserve its existing working-memory allowance, and decode on its bounded blocking
worker. It must reject damaged, truncated, concatenated, trailing, unsupported or
over-expanding inputs before append. Compression is never permission to raise
the text, metadata, record-count or concurrency limits.

## Retries and streaming

Compression changes representation, not the canonical request or event. Retain
the original request ID, event IDs, scope and logical values after an unknown
acknowledgment. Do not allocate a new identity or automatically retry with another
encoding. Explicit replay of the original values across identity/gzip must
return the original receipt and must not create duplicate stored events.

The existing clients reuse their HTTP connection pools. A capture batch remains
bounded and buffered, with one complete acknowledgment after the batch; this is
not an unbounded bidirectional stream or acknowledgment of individual network
chunks. Existing export streaming still requires clean EOF. Disconnect, abort,
revocation or a partial response must not become completed delivery.

## Reproduce the transport measurement

`scripts/bench-capture-transport.mjs` compares explicit identity and auto modes
with identical logical inputs, alternating order, three warmups and bounded
samples. It uses the actual SDK over verified loopback HTTPS, checks every exact
original, and records payload bytes, latency and TLS connection reuse.

Create an isolated synthetic localhost certificate/key with your test tooling,
then run in a process that trusts only the added fixture certificate:

```sh
NODE_EXTRA_CA_CERTS=/private/fixture/server.crt \
  node scripts/bench-capture-transport.mjs \
  /private/fixture/server.crt /private/fixture/server.key 12
```

The certificate must cover `localhost`; verification is never disabled. The
server binds loopback only. This is an in-memory transport fixture: it does not
measure Cloud durability, WAN bandwidth, tenant capacity, KMS, S3 or PostgreSQL.
Report the slower cases as well as byte savings. Default compression must not be
enabled based on the compression ratio alone.

`node scripts/bench-capture-transport.mjs --fixture` emits a small synthetic
gzip/identity wire pair from the actual SDK, with its source hash. The Cloud
decoder test consumes this fixture for cross-language compatibility.

The [local qualification receipt](capture-transport-qualification-2026-10-24.json)
records the selected checks, source hashes and limits. The complete
[HTTPS measurement](benchmarks/capture-transport-2026-10-24/README.md) includes
slower compression cases and the untrusted-certificate negative control.
