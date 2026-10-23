# Native committed-receipt reads (opt-in)

This library-only milestone adds `oojuro_engine::receipts::ReceiptView` over the
independently opened CaptureLog. It does not change CaptureLog, append/ACK,
existing capture, lexical visibility or the default product. There is no second
journal, raw reparse, policy rerun, hosted selection or oracle dependency.

## Snapshot and continuation

`ReceiptView::open(log, Scope { writer, agent }, Range::default())` captures the
identity, sealed ciphertext incarnation and exact committed record-prefix stamp.
The writer and agent must match the log; this is local scope binding, not hosted
authentication. Every read validates the original prefix against the independently
supplied healthy CaptureLog. Later appends are allowed but invisible to this view;
a new view immediately includes an acknowledged append. Reopening the same
unchanged or extended authority preserves the view. An older authority, changed
ciphertext incarnation, independently created authority or diverged prefix is
refused. Compaction may change the incarnation and requires a fresh view.

`page(log, Limits { entries, bytes })` returns original receipts, the snapshot
watermark and an optional next cursor. `ReceiptView::resume(log, scope, range,
cursor)` resumes that exact snapshot even after append or process reopen. The
opaque cursor is AEAD sealed with the authority cipher and a distinct receipt
cursor AAD domain; it binds writer, agent, snapshot, ordinal range and next
position. Altered, foreign, unsupported, ahead and wrong-range cursors fail.
Reusing a cursor intentionally returns the same receipt page for retry; it is not
a single-use authorization token. Nonces may change the continuation token's
bytes. Following issued continuations covers the range without skips/duplicates.
There is no secret key getter or new persistent authority state.

Watermarks carry the ciphertext origin and an AEAD-encrypted existing prefix
digest, bound to identity/origin/counts by a separate receipt-prefix AAD domain.
The internal prefix hashes plaintext receipts/raw/context, so its digest is never
publicly exported: an unencrypted digest could confirm guesses about denied data.
These are not independent anti-rollback witnesses or deletion proofs.
Complete rollback of an authority and its usable key is outside this local API.

## What the timeline means

The primary range is zero-based receipt order, start inclusive and end exclusive.
Each stored or withheld emission occupies one ordinal. A blank, malformed or
parsed line with no emissions occupies one ordinal. Source registrations and a
raw-only unfinished tail have no timeline output; their committed records still
advance the watermark. Ordering is commit batch, line and emission order. Harness
event timestamps are returned unchanged and do not order the receipt timeline.
Withheld/blank/malformed outcomes have no retained event timestamp, so none is
inferred from raw bytes or a neighbouring event. Event IDs, session/parent fields
and source-generation tokens are original adapter/caller claims, not verified
causal identity.

Every receipt carries its batch ordinal, source-generation token, original line
span, emission ordinal, raw/parsed batch bounds, policy identifier, recorded raw-consent
label (not current authorization), adapter identifier and explicit receipt schema version 1. Stored events
retain their complete original ID, source spans, text/args and initial `indexed`
boolean, including stored-only events and original duplicates absent from search.
Withheld outcomes expose their retained bounded reason only; no denied payload is
reconstructed. Blank/malformed outcomes expose kind and bounds only.

No raw source bytes, descriptor bytes, parser context, arbitrary context-derived
fields, plaintext context digest or source pathname is exported. Exact raw
restoration remains a separate consent path. Original stored events and retained
policy/withholding labels remain sensitive authorized content; applications must
authorize their readers and avoid logging outputs/cursors.

## Other reads and bounds

- `get(log, id, bytes)` returns the original stored event receipt within the view's
  prefix/range, never a deduplicated search replacement. A withheld emission has
  no fabricated event ID.
- `stats(log, bytes)` counts original outcomes and the initial indexed flags over
  the selected range. It does not report current lexical hit count or visibility.
- `export(log, limits)` serializes the same bounded JSON page, including watermark
  and continuation. Page concatenation must use its entries rather than treating
  each envelope as a standalone new snapshot.
- Limits are 1..1000 entries and 1..16MiB for the exact encoded JSON envelope,
  including the encrypted cursor. Defaults are 100 entries and 1MiB. A page that
  cannot fit even its first receipt fails before returning anything; it never
  skips an oversized receipt. Retry with a larger explicit limit. All methods
  refuse invalid limits. Get/stats use the same explicit 16MiB encoded cap.
- Cursor input is bounded to 8192 lowercase hex bytes before decoding/decryption.
  Prefix replay remains CaptureLog's existing 100k-event/128MiB metadata bound.
  This is bounded logical output, not an exact RSS/latency guarantee. Reads walk
  retained receipt metadata in memory and do not provide a disk query engine.

## Scope relative to the foundation specification

This implements a truthful bounded part of SPEC sections 6/12 over the current
native receipt schema. It does not invent separate `valid_at`/`recorded_at`, keys,
key-version history, as-of visibility, timestamp filtering, retention/deletion,
audit receipts, learned outcomes, full history semantics, hosted authorization,
replicated ACK, all adapters or the complete section14/18 scale/performance gates.
The reference engine remains the product/differential oracle until those gates.

## Verification

Focused native tests exercise original indexed/stored-only/withheld and empty-line
provenance, privacy exclusions, cursor corruption/domain/range/scope refusal,
immutable append/reopen pagination without gaps/duplicates, independent and copied
origin forks, exact encoded byte limits, and native memory/persistent index ID and
score parity. Eleven new regressions pass. Final current-source compatibility gate:
43 capture tests and 49 engine tests pass, with the same three intentional engine
helpers ignored (the SIGKILL parent drivers run). Strict no-deps Clippy for engine
and capture lib/tests passes without new warning or waiver; scoped rustfmt and
`git diff --check` pass. Legacy test-only oracle dependencies still report their
pre-existing dead-code warnings, outside the no-deps lint scope.

Independent pre-publication review caught two draft defects: exporting the
plaintext-derived prefix digest and failing to count JSON `null` for absent get
results. The privacy regression failed on the initial draft, then passed with the
encrypted commitment; exact 3-byte refusal/4-byte null success is covered. The
reviewer rechecked both fixes without another blocker. Three new mechanical lint
warnings were corrected; no authority or scoring semantics changed.
No provider, AWS, real user data or production action is part of these fixtures.
