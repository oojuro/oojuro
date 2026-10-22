# Fresh managed paged streaming storage layout 2

This extends the same ManagedPagedLog/RetainedControl authority. It creates a
fresh explicit storage namespace; existing snapshot and paged layout 1 defaults
remain unchanged. There is no conversion, fallback or pending-receipt transfer.

## Refusal before control operations

Authenticated State.PagedAnchor contains optional layout {storage_layout:2,
minimum_reader:2}. Layout 1 omits the field byte-for-byte. Old Anchor decoders
deny unknown fields, so old control code cannot decode a layout-2 current State
before deny/current/status or publication. The new reader validates only absent
layout 1 or exact (2,2), in current-state validation, tail decoding and before
publication. Transitions cannot change mode. The bounded root bundle's format
must agree with the small anchor. Fresh create_paged_streaming pins this from
genesis; create_streaming/open_streaming require the distinct DATA marker.
Ordinary create/open require layout 1.

## Exact canonical admission and encoding

Input text remains at most 16 MiB raw UTF-8; all existing name, attr/node/depth
limits remain. At most 1000 Inputs are admitted. Layout-2 batch logical bytes
equal the canonical JSON array with every text replaced by the empty string,
plus the sum of raw UTF-8 text lengths. The total must fit 32 MiB. Thus names,
attrs, field names, quotes, array brackets and commas all consume admission.
This is an explicit layout-2 policy; layout 1 retains its existing encoded limit.

Real canonical serde_json bytes flow directly into keyed BLAKE3. The preimage is
identical to the original contiguous serializer, including negative-zero/number
spelling. No public plaintext digest is exported. JSON escaping expands at most
six times the raw text bytes; the real request stream is bounded by six times
the admitted batch bytes. Assigned uses the same JSON fields/order, with its
exact fixed wrapper length plus at most 20 decimal bytes each for ID and time.
Its encoded bound is derived from this overhead, not an allocated buffer.

A writer holds one 4 MiB carrier buffer. Eight carriers per new-format pack
bound uncompressed payload to 32 MiB plus fixed framing, below the unchanged
64 MiB pack limit. Actual lower compression uses RAW fallback if compression
does not shrink; compression still precedes contextual AEAD. Multiple packs
may hold one logical row. RowRef retains first physical sequence/chunks/bytes;
layout-2 Segment indexes first physical sequence to file ID. Pack/tree births,
full key version, current witness and staged typed readback remain unchanged.

## Incremental decoding and lifecycle

Verified parts implement Read with one carrier buffer; no escaped JSON is
concatenated. A first streaming lexical preflight bounds aggregate decoded
strings, node/token count, depth and primitive token lengths before serde can
allocate scratch/Value trees. It handles escapes and surrogate pairs without
charging six encoded bytes as six decoded bytes. A second serde Read pass
validates full JSON/UTF-8 and end-of-input; existing Input/id/time validation
follows. Every new part resolves the actual current catalog and contextual
AEAD; failed authority cannot reuse a cached part as a new chunk.

Append, candidate verification, reads and survivor pruning share this codec.
Lifecycle counters retain actual original encoded-byte accounting. Candidate
comparison uses private keyed canonical streaming digests, preserving numeric
representation without an encoded-row Vec. Logical IDs, first recorded_at,
retry preimages/ACKs and receipt-only removed outcomes remain original.

The public get/read encoded-JSON output limit is still 64 MiB. A legal raw
16 MiB text may require about 96 MiB of escaped JSON, so those output calls
can explicitly refuse it. This patch does not claim a bounded public large
export API, throughput, RSS, full-scale acceptance or permanent backup erasure.

## Evidence

Library check passed. Six initial focused cases passed, covering anchor
refusal before current/deny/publication, old schema/layout-1 bytes, exact budget,
canonical fingerprint/negative zero and chunked survivor rewrite.
The full 16 MiB NUL case passed in 86.46 seconds: actual multi-pack append,
internal incremental read, unchanged public-output refusal, reopen, prune,
signed original IDs, and exact original receipt-only retry.
The managed regression gate passed 66 tests (two intentional subprocess helpers
ignored) in 56.97 seconds; the separately proven large case was excluded.
After a lint-only named PackConfig change, seven affected codec cases passed
in 15.61 seconds and engine lib/tests strict lint (-D warnings, --no-deps)
passed in 3.90 seconds. Existing dependency-crate dead-code warnings remain
visible; there were no owned engine diagnostics. Independent source review
found no new concrete P1/P2 in the complete new codec/control/driver delta.
