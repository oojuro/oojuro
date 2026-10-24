> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/PROJECT_READ_SDK.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Shared-project read SDK

Private candidate in the existing `sdk/typescript` package. Nothing is published.
`OojuroProjectClient` uses an `ojp_` read key issued at **Projects → API keys**.
It reads only that project's current referenced journals. Capture uses the
separate append-only `OojuroCaptureClient` and `ojc_` key; the older `OojuroClient`
uses its separate `ojv2_` protocol. None of these clients creates a customer source.

```js
import { OojuroProjectClient } from "./sdk/typescript/index.js";

const reader = new OojuroProjectClient({
  baseUrl: "https://oojuro.com", // Target domain; deployment acceptance is still pending.
  project: process.env.OOJURO_PROJECT_ID,
  token: process.env.OOJURO_PROJECT_READ_KEY,
});
const selection = { from: Date.now() * 1000 - 3600_000_000, limit: 100 };
let cursor = null;
let snapshot;
do {
  const page = await reader.records({ ...selection, cursor }, {
    signal: AbortSignal.timeout(60_000),
  });
  snapshot ??= page.snapshot;
  if (page.snapshot !== snapshot) throw Error("Project snapshot changed");
  for (const row of page.records) {
    // Metadata has no text field. Fetch only content your application needs.
    const text = await reader.content({ journal: row.journal, recordId: row.record.id }, {
      signal: AbortSignal.timeout(60_000),
    });
    // Use text according to your application's data/retention policy.
  }
  cursor = page.next;
} while (cursor !== null);
```

## Selection and continuation

`records` accepts optional inclusive `from`/`through` activity times in exact JS
safe-integer microseconds, opaque persistent `agentId`, `limit` (1–1,000; default
100) and `cursor`. Omitting time bounds means unbounded time. Integers outside
JS's exact subset are refused rather than rounded. This is a metadata/time/agent
query, not full-text search.

The client sends form POSTs so cursors never appear in request URLs. Continue
using the original selection and read key; only the page count may change.
The opaque `snapshot` represents a vector of committed source snapshots, not an
atomic transaction or global generation across journals. Later appends are
excluded from continuation; start a fresh query to see them. Server cursors expire
after one hour. Key replacement/revocation, reference/deletion changes or an
unavailable source may refuse continuation. Start a fresh query after resolving
the denial; no automatic reset or retry hides the changed view.

Rows retain journal and record identity. Capture-source descriptors optionally
include `capture: {event_id, run_id}`. Metadata validates complete schemas,
identities, bounds, duplicate keys, exact integer lexemes and selected time/agent.
Legacy descriptors without capture metadata remain valid. No invented native
watermark header is required by this common project protocol.

## Full-text search

```js
import { randomBytes } from "node:crypto";
const request = {
  requestId: randomBytes(32).toString("hex"),
  query: "connection timeout",
  from: Date.now() * 1000 - 3600_000_000,
  agentId: process.env.OOJURO_AGENT_ID,
  k: 20,
};
// Retain this exact request if its outcome is uncertain.
const result = await reader.search(request, { signal: AbortSignal.timeout(60_000) });
for (const hit of result.hits) {
  // Excerpt offsets are UTF-8 bytes in the exact original text, not JS characters.
  console.log(hit.journal, hit.record.id, hit.excerpt);
}
```

Search accepts a 64-character lowercase hex `requestId`, a nonblank query of at
most 4,096 UTF-8 bytes, inclusive time/agent selectors and `k` 1–100 (default 20).
It uses latest visible keyed versions; an unindexed winner does not make an old
indexed version searchable again. Results carry the original descriptor,
`score_bits`, `continued`, exact `excerpt_start`/`excerpt_end` byte offsets and
excerpt. Native per-writer continuation order is preserved when merging heads
across referenced journals; scores are not calibrated across writers. There is
no search cursor or invented atomic cross-journal watermark.

Search is a **metered operation**. Keep the original request ID and exact input
after `outcome: "unknown"`; repeating that request returns retained status only
(HTTP 409), never another execution or a result replay. A new ID explicitly
requests another search. The client never retries automatically. Once dispatch
starts, transport, invalid/partial response and HTTP failures conservatively
report unknown metering outcome. Query and request ID are in the form body,
never the URL; raw query text is not retained in the usage or read-audit ledger.
All results require clean EOF and the same current directory/source guards.

The initial server projection is bounded to 4,000 selected indexed records and
8 MiB of original text across the project, 128 writers, at most 100 hits and a
4 MiB complete response. A broader request returns 413 and must narrow its time
or agent selection. It reads current selected source text to build a disposable
native projection; no scalable persistent index or throughput claim is made.

## Content, errors and limits

`content` returns exact UTF-8, including a leading BOM and embedded NUL. Metadata
is bounded at 4 MiB, content at 16 MiB and form requests/cursors at 2 MiB. Both
methods return results only after clean EOF; partial reads, invalid encoding,
oversized bodies, changed declared lengths and stream errors reject the whole
result. A metadata page is a snapshot; separate content reads use current
authority and may fail after a detach/delete/revoke. The server controls current
authorization through response frames and EOF. Previously delivered bytes cannot
be recalled.

HTTPS is required except explicit loopback HTTP. Requests omit cookies, reject
redirects and avoid caching/referrers. Only the scoped bearer header carries the
read credential; inspection/JSON/error messages do not reveal it or provider
response bodies. There is no automatic retry or hidden request timer. Pass an
AbortSignal to bound requests. Limits describe accepted sizes, not throughput or
AWS recovery guarantees. All source, authority and audit costs still count in
benchmarks and pricing.

## Checks

`node --test sdk/typescript/test/*.test.js` covers request isolation, cursor POST,
credential separation, malformed/ambiguous metadata, exact 16 MiB text, missing
EOF, cancellation, redaction and legacy descriptors. The cloud joined acceptance
uses this SDK against its actual HTTP/PostgreSQL server to read captured records,
continue a stable snapshot and verify read-key replacement/revocation. Production
source provisioning, growing-history search, installed packages and AWS failure/benchmark
acceptance remain separate launch gates.

The same project routes are available in the existing [Oojuro CLI](HOSTED_CLI.md).
