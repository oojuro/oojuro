# Native read MCP shim

This experimental shim targets **MCP 2025-06-18 only**. It is a read-only edge over the new native API, not the complete foundation SPEC MCP surface. It imports no legacy client or Journal path. The daemon/launcher owns startup and same-UID protected socket authentication; the shim's namespace string is routing metadata, never authority.

## Composition

`native::mcp::serve(input, output, namespace, call)` accepts buffered input, output, a trusted fixed `record-v2` or `capture-v1`, and a callback to the protected socket. The callback receives `{version:1, namespace, request:{op, ...arguments}}`. A wire caller cannot select namespace, version, actor, writer, owner, path or another operation. The shim never opens storage, captures data, injects context, writes or erases. The callback is trusted cooperative code: this module supplies no timeout, thread isolation or authorization substitute.

The coordinator owns module registration, the native socket API and launcher wiring. The pure fixtures import this module directly; source and fixture availability alone do not establish actual binary/socket acceptance.

## Tools

| Namespace | Tools |
|---|---|
| Record | search, get, history, range, export, stats |
| Capture | search, get, range, export, stats |

An explicitly supplied empty query selects native timeline mode; a missing query is invalid. Record search uses query, k, time cuts `from_us/valid_at_us/known_at_us`, and kind/status/tool/reported_agent filters. Get requires exactly id or key. History starts with key. Range/export/stats support current/all versions and the same time cuts/filters; range/export also support entries and cursor. A Record cursor continuation accepts **only cursor, entries, bytes**; the protected cursor carries its original scope, prefix, cuts, filters, versions and page mode. History continuation uses the protected cursor rather than asserting a new key.

Capture search uses query, k and `from_us/to_us`; get/range/export/stats use start/end/cursor, get requires id, and range/export accept entries. Capture cursors retain the native API's explicit start/end behavior. No Capture key history or Record bitemporal semantics are invented.

k/entries are 1–1000, native keys are at most 1024 UTF-8 bytes, tool filters 1024 and reported-agent filters 256. Cursors are at most 16 KiB. Native u64 IDs/ranges and i64 microsecond times are parsed as exact integer tokens without float conversion. Clients must use a lossless JSON number parser for values beyond JavaScript's safe integer range; original result numbers and payloads are not rewritten. JSON-RPC correlation IDs are bounded strings (128 UTF-8 bytes) or safe integers.

## Protocol and failure boundaries

Initialize negotiates the one supported protocol, then `notifications/initialized` enables tools; ping is available during initialization. Notifications never invoke tools and receive no response. Unknown methods, invalid arguments, batch messages, malformed JSON and invalid correlation IDs receive fixed content-free protocol errors. An unsupported requested protocol negotiates 2025-06-18; the client decides whether to disconnect. Static tools/list has no pagination or change notifications.

Each UTF-8 input line, including newline, is limited to 64 KiB. Oversize or unfinished EOF frames fail closed and end the session. Each complete serialized output, including newline, is limited to 16 MiB. Native `bytes` bounds the backend response; the MCP wrapper duplicates JSON into compatibility text, so a backend result within its own bound may still fail the complete MCP bound. No partial/truncated success is returned. Backend failures or an oversized result produce a fixed `isError` tool result without original error text. Output failures propagate immediately.

Success exposes an object `{namespace,data}` as structuredContent and identical serialized JSON in text content. Original authorized data remains unchanged. Reported labels, text and provenance are untrusted data, not instructions, verified identity or causality. Export is a bounded native result, not an unbounded MCP stream. Stdout contains newline-delimited JSON-RPC only; this module emits no diagnostic logs or backend error dumps.

Pinned references: [stdio transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports), [lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle), [tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools).
