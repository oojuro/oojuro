# Explicit local Capture1 to managed Record bridge

This library operation copies only already-authorized Stored emissions from an
opened Capture1 authority into an opened, independently current ManagedLog.
Capture retains its original sealed raw bytes, parser context and policy receipts.
The destination is real durable managed Record authority, not a query-time RAM
projection. The existing Capture, Record, managed and daemon formats are unchanged.
MessageCapture's separate pending format is unsupported.

## Explicit operation

The caller explicitly opens both authorities with their real configured keys,
writer/owner identity and retained control/witness. Then call:

~~~rust,ignore
use oojuro_engine::capture_record::{ingest, Limits};

let report = ingest(
    capture.authority()?,
    &mut managed_log,
    configured_scope,
    private_checkpoint_path,
    Limits::default(),
)?;
~~~

Opening authority, enrollment and local source consent remain caller duties.
The bridge discovers no source, creates no authority, infers no cloud consent,
uploads nothing and supplies no key getter. There is no new CLI/default daemon
selection in this slice. The public API is an explicit runnable library path.
Place the checkpoint in an existing current-user mode0700 directory OUTSIDE
all data/control/witness authority directories. It is a mode0600 sealed advisory
file; no plaintext payload, source path, descriptor or parser context is written.

Both source writer/owner and genuine managed scope must agree. The destination
capability checks actual retained authority before/after each append and before/
after checkpoint saving. A caller-provided label is not authentication. Current
denial, unavailable witness, source-prefix mismatch and wrong target refuse.

## Mapping and time

| Capture value | Canonical Record |
|---|---|
| source Event.text | unchanged authorized parsed text |
| Event.indexed | identical initial eligibility, including stored-only false |
| ts_us | valid_at, a reported original event time |
| kind0..6 | system, input, thinking, output, tool_call, tool_result, explicit |
| status0/2/3 | absent / ok / error; unsupported codes refuse |
| tool / exit_code | typed tool / exit |
| agent/session and parent_session | explicit capture agent/session labels; reported relationship |
| original ID and remaining Event metadata | sealed attrs.capture.reported, never substituted for canonical assigned ID |
| source generation, adapter identity, policy/raw-authorization labels | sealed attrs.capture provenance, never grants |
| args.rt or other harness clocks | reported metadata only |
| first managed append clock | canonical recorded_at, fixed once by actual destination authority |

Capture has no logical-key version model. The bridge sets key=None and never
invents memory history or overwrites explicitly keyed canonical notes. Original
IDs remain exact u64 provenance, while the destination allocates its own IDs.
A stored kind/status/time/provenance claim is data, not independently verified
truth, causality or instructions.

Withheld emissions, blank/malformed lines and parsed lines without output advance
progress without synthesizing records. The bridge never reparses retained raw to
recover excluded text. Original authorized metadata must fit the canonical4KiB
attrs bound; oversized metadata explicitly refuses rather than dropping fields.
Capture's derived lexical index suppresses some duplicate text/argument hosts
without changing the original Event.indexed flag. Canonical Record currently
keeps both original eligible Records. This bridge deliberately preserves each
original and its flag, so identical Capture-vs-Record search counts are NOT
claimed. Canonical IDs/order/score bits are tested before/after restart. The full
retrieval/deduplication product contract remains a separate acceptance gate.

The canonical16MiB text/32MiB batch limits remain enforced. A configured byte
budget smaller than the first legal Input refuses with no progress. Later budget
exhaustion returns a resumable report; no record or text is truncated/skipped.
The current Capture1 source itself has its existing smaller capture-batch bounds.

## Durable retry and checkpoint ordering

Each immutable source generation/line span/emission has one deterministic
request identity bound to an engine-private keyed MAC of the configured source
key, exact verified Capture1 header Identity and fixed framing domain. It is
stable across compaction; the disposable physical cache origin is never a retry
identity. The MAC is not exported in provenance or report output.

This is not globally unique fresh-store enrollment. Reusing the same key,
Identity and source generation means the SAME logical source. Changed payload
must conflict; independent sources require a distinct key/Identity/generation.
Assigned source ID and batch spans remain immutable Input provenance checked by
the exact typed Input fingerprint; they do not allocate a fresh retry identity.
No public plaintext-content fingerprint is published. ManagedLog separately
keys the request tag and exact typed Input fingerprint. A changed Input
under the same operation refuses.

For each Stored emission:

1. Recheck genuine current target scope/stable identity and source receipt.
2. Append through the existing managed commit protocol.
3. Recheck current target authority; retain actual assigned ID/recorded_at result.
4. After the bounded page, seal progress and atomically replace/fsync its file.
5. Recheck current target authority before returning success.

The destination commit and original retry receipt are the authority. The progress
file is not a second WAL, acknowledgement authority or fresh external witness.
A crash/unknown response after append but before checkpoint replays the same
operation and recovers the original canonical ID and recorded_at. A missing or
rewound checkpoint may replay from zero; this consumes bounded work again and
does not allocate duplicate rows. A pruned original returns receipt_only plus
removed IDs; it is never reinserted. Writer denial refuses even a replay.

A checkpoint binds exact keyed Capture source identity and an immutable ancestor
prefix, plus managed stable origin/identity. New source appends are permitted.
The live source head need not remain unchanged. Source rollback
below its checked prefix, another root, malformed ciphertext and wrong target
refuse. Checkpoint rollback alone is safe because the managed current authority
and original retry outcomes remain independent of that file.

An error can follow a committed append. Close/reopen/retry the identical source
operation; do not allocate a new source identity or reinterpret it as refusal.
A checkpoint write/sync failure leaves uncertain progress, never uncertain
authority. Abrupt process loss can leave an owned sealed temporary progress file;
it is not a success marker and may be explicitly removed after ownership/process
inspection. There is no automatic source or destination deletion.

## Bounds and report

Default operation:128 receipt entries and32MiB total canonical Input encoding;
maximum1000 entries. At most4096 cursor traversal steps process empty/raw-only
batches as bounded continuation work. The bridge holds at most the current
input plus bounded result mappings; it builds no corpus-sized deduplication map.
It uses batch/line/emission positions instead of rescanning every prior receipt.
Existing Capture replay and the current managed snapshot implementation still
retain bounded whole-state RAM/O(live-state) work; this adds no scale claim.

Report distinguishes scanned, withheld, no_output, stored, new,
already_committed, removed and exact source-to-canonical mappings with first
recorded_at/receipt_only. more describes this checked source prefix only;
more=false never means a live source cannot append later. Zero new rows can be
a successful idempotent replay. Progress is saved only after accepted outcomes.

## Verification boundaries

Synthetic focused tests cover actual LocalCapture -> managed commit -> reopen ->
query/get/known-time behavior, unchanged original metadata/IDs/text/policy and raw
bytes, redaction, withheld/stored-only/duplicate lexical eligibility, keyed memory
noninterference, bounded continuation, lost/rewound progress, a real SIGKILL after destination append and before progress,
pruned receipt-only replay, wrong target, corruption, appendable ancestor prefix,
permission/oversized-first refusal, kind/status mapping and denial between append
and checkpoint. The actual focused gate passes all11 cases (9 engine,2 LocalCapture integration),
with zero diagnostics in owned paths under package lib/tests and edge lint.

No live user source, cloud/provider call, backup crypto-erasure, live power-loss
or host/AZ durability test, scale measurement, automatic capture scheduling,
concurrent daemon recorder, MessageCapture support or complete SPEC acceptance
is claimed. The managed snapshot target remains the bounded existing lifecycle
lane; the separate paged/incremental writer must be joined and tested explicitly.

Self-review found and reproduced a compaction/retry defect before publication:
the disposable ciphertext cache origin could change during Store compaction,
causing a lost checkpoint to allocate duplicate canonical rows. The actual
compaction+reopen+lost-progress regression fails with that identity and passes
with the private stable source MAC. Reusing the same key/header/generation with
a changed payload refuses without a new canonical row. No on-disk Capture1
header, raw/policy receipt format, key getter or second WAL was introduced.
