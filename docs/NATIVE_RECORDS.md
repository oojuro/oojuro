# Direct native Record/time namespace (opt-in)

This implements a bounded direct SDK slice of foundation SPEC sections 2, 6 and 12. It is not a claim of full SPEC completion or a migration of harness raw+parsed capture. The existing v1 CaptureLog, receipt API, capture adapters and encrypted segment cache are unchanged. Store is the only durable authority; there is no new WAL, sidecar or dual write.

## Open and append

`oojuro_engine::record::RecordLog` (also reexported from `capture_log`) opens an explicit fresh `RECORD_FORMAT = oojuro-record-2` directory using the existing sealed Store. An existing nonempty directory without that exact marker is rejected before Store open. It never infers or converts a v1 capture directory. A directory has one owning writer and numeric owner partition from the existing Identity; `agent_id` and `parent_id` on each Input are reported running-instance claims, separate from that owner. Adapter labels are caller declarations, not verified provenance or causal links.

`append(request, &[Input]) -> Commit` requires a nonempty exact retry key and explicitly supplied typed inputs: Kind, nullable Status/tool/exit/parent/key, agent identity, attrs, valid_at, text and initial indexed eligibility. No harness parser or implicit redactor runs on this direct SDK path. Callers must apply their authorized input/redaction policy before submitting. Encryption at rest is not a secret classifier.

- Input `valid_at` is caller-supplied event time in UTC microseconds. It does not set the engine clock.
- `recorded_at` is assigned by the engine at the first successful journal append: `max(now_utc_micros, prior_recorded_at + 1)`, strictly increasing within this writer namespace, including a batch and clock rollback. It is not manufactured from Event.ts_us or from the outer Store carrier, whose time fields remain zero because it carries an envelope.
- One sealed Store append atomically persists typed content, original IDs, engine times, original intent fingerprint, allocation outcome and exact ACK watermark. ID/time overflow fails before mutation. IDs are contiguous from Identity.first_id and are never reused within this authority.
- Retry identity and original typed intent use separate keyed BLAKE3 domains. Original agent/attrs/key/valid_at/indexed/kind/tool/status/exit/text all participate. Reordered JSON map syntax is not another typed value. Same request+same typed values returns the persisted original IDs, times and exact receipt before consulting the clock; changed values conflict. There is no eviction within the bounded namespace.
- Once Store append is attempted, any ambiguous error poisons live access until reopen. Replay validates strict envelope versions, complete assignments, sequential IDs/times, keyed intent, origin/scope and sealed prefix commitments before publishing state. A durable append followed by failure or process death can recover the original outcome on retry.

## Read API and semantics

`View::open(&log, Scope { writer, owner }, AsOf { from, valid_at, known_at })` captures an immutable protected prefix. All times are UTC microseconds; the default cutoffs are unbounded. Every operation independently validates that prefix and scope against the supplied current RecordLog. Later append is allowed but hidden from the old view. A shortened or divergent prefix cannot satisfy an already-known watermark. No external current-head authority is provided: an application that knows no later watermark cannot detect restoration of a healthy old entire directory.

- `get_id` returns an original logical version if it is in the snapshot and time range, including initially unindexed records. It does not substitute a duplicate host.
- `get_key` selects the newest eligible version by `(valid_at, recorded_at, original sequence)`, with both `valid_at <= cutoff` and `recorded_at <= known_at`. A late backdated write does not automatically win. `key = null` is unversioned and never collapses another record.
- `history` retains all original versions; `range` and `export` explicitly select Current or All. Timeline order is ascending `(valid_at, recorded_at, id)`; search uses the separate F4 score/time/text-ID order.
- Lower time bounds and typed filters apply **after** selecting key winners, preventing an excluded newest version from resurrecting an older one. Kind, tool, status and reported agent are typed filters. Attrs are retained JSON data and never indexed.
- Pages include the watermark and a separately domain-sealed continuation. It binds writer/owner, origin/prefix, time bounds, read mode, filters and next position. Resuming after append/reopen reads the same old snapshot. Unknown, altered, foreign or truncated-prefix cursors fail closed. Cursor replay returns the same rows; opaque next-token bytes may vary.
- Limits cover the serialized envelope, full records and cursor, including escaped Unicode/control bytes. A first record that does not fit returns Bounds; it is never silently skipped. A missing get result still requires four bytes for JSON null. Search also requires an explicit max_bytes and never silently omits a result to satisfy it.

## Native search projection

`record::Index::rebuild(&log)` and `refresh(&log)` tokenize only original committed typed records and use the existing native scorer. Every logical ID survives even if text duplicates another record. An internal lexical carrier supplies ID/valid time/text only; it is never exported as harness provenance. A failed refresh invalidates that projection; an advanced log requires refresh rather than serving a stale result. Refresh cannot rebind an existing index to a foreign or rolled-back prefix. Reads include a protected originating watermark.

Visibility (snapshot, known time, key winner, lower time and typed filter) is applied before identifier anchoring, candidate selection, F3 statistics and pruning. Shared scorer arithmetic and term iteration are unchanged for legacy calls. Search preserves F1 time visibility, F2 identifier continuation, F3 whole-visible-view background padding, and F4 score descending, valid_at descending, full decimal ID text ties. Time/key reads can retrieve stored-only records; their text has no postings and does not affect lexical statistics. Initial indexed=false is not a deletion or withholding protocol.

The new Record index is **memory-only**, fully rebuilt on refresh/reopen, with no new segment cache, delta merge, multi-writer query, ranker slot, excerpt/packing receipt, retention/tombstone or audit-proof integration. Existing persistent segments remain v1 capture projections. Append ACK certifies journal durability, not an automatically refreshed external Index: callers must refresh before search, which otherwise fails closed. Harness raw+Record atomic migration, automatic read-your-writes facade, persistent Record segments, bulk scale/performance and complete SPEC acceptance remain future slices.

## Bounds and recovery evidence

- Writer/reported agent IDs: 256 bytes; tool/logical key: 1024 bytes. Invalid/control-containing names are rejected.
- Text: 16 MiB; serialized attrs: 4 KiB with bounded JSON nesting/work. One batch: up to 1000 records, 32 MiB serialized. Namespace: up to 100,000 records / 128 MiB serialized replay metadata. These bound logical state, not exact RSS; indexing, serialization and Store buffers add memory.
- Output: at most 1000 entries per page / 64 MiB exact serialized envelope, explicit search k <= 10,000 and text <= 64 KiB. Page selection accounts for final cursor overhead without repeatedly serializing the full growing page.

Synthetic tests execute actual Store append -> fsynced commit -> process SIGKILL immediately before live replay/ACK and after ACK -> reopen -> exact retry -> original get/history/as-of/export/search. Separate regressions exercise malformed but sealed envelopes, original intent conflict, clock/ID overflow, clock rollback, failed projection, same-origin rollback with a known later watermark, foreign scope/fork/cursor, output budgets, sealed-at-rest contents and unchanged v1 namespaces. Dev-only direct Nine Record differentials compare IDs/order/f64 bits/time/continuation across key versions, independent known time, backdated writes, duplicate text, long decimal ties, 1200 candidates and low/high background mass.

These tests prove bounded local process-crash/replay behavior and the stated synthetic differential cases. They do not prove power-loss durability, multi-host/AZ replication, AWS operations, cross-machine floating-point equality, provider erasure, authorization or semantic retrieval quality. No runtime/normal dependency on Nine, Trace or the old fixture Record was added.
