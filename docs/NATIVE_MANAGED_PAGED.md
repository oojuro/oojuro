# Incremental managed paged layout

This is a separate explicit fresh layout under the same retained control signer and
independent FileWitness authority as managed snapshots. Existing snapshot storage
and markers remain unchanged. A snapshot ManagedLog refuses a paged control.

## Publication and ancestry

Paged control stores bounded immutable signed Envelope objects, not a population
Store index. CURRENT directly pins the current envelope and encrypted root bundle.
Opening current state does not replay genesis. Historical checkpoints are addressed
by a CONTROL-key encrypted COW checkpoint tree. Each transition inserts the previous
checkpoint; the current digest comes from the independent witness, avoiding a
self-reference. The current signed root is the private bootstrap anchor for this
one index. Every other tree birth must match the exact published historical root
birth, including its unique epoch. Existence of an earlier parent alone does not
authorize an abandoned sibling candidate.

Root bundles are at most 64 KiB ciphertext. The small State anchor cross-binds
generation, origin, identity, CONTROL key version, immutable token, exact length,
ciphertext digest, and receipt details. Normal State/Envelope bounds remain intact.
The bundle contains only bounded roots/allocators, not all records/retries.
Its encrypted private intent key authenticates retry and logical receipt commitments.

## DATA protocol and current validation

The bounded control-tail/root and checkpoint ancestry foundation verifies direct
current open without genesis, missing current refusal, old snapshot mode refusal,
published-birth/sibling and root namespace separation.

The opt-in driver uses one bounded typed request, immutable compressed/encrypted
DATA packs, and COW Record/KeyVersion/Segment/Frame/File/Dictionary roots. Original
logical IDs and first engine ingestion times remain in Assigned rows, separately
from physical carrier sequences. Assigned JSON is split into at most 4 MiB carrier
chunks, with zero outer carrier times; extreme original valid times are retained
without overflowing physical codec deltas. Original request receipts remain in
CONTROL Request/Prefix trees, compactly storing consecutive allocated IDs/times.
Retry conflicts refuse; exact retries never allocate fresh IDs or resurrect erased
rows. Every candidate tail is decoded through the actual pack Reader and compared
with original typed IDs/times/Input before the independent checkpoint publication.
The private staged verification capability is not a serving API or an ACK.
Public operation entry/end and publication verify the full current root bundle and
enrollment. While that verified owner is immutably held, every metadata page,
record/file callback and deletion checks the independent exact CURRENT witness
and unchanged enrollment again. A shared 16-entry operation-local LRU stores only
exact published Birth proofs, keyed by full Birth/key class/current checkpoint.
Currentness is never cached; cache hits require a new witness check. Corrupt
metadata pages still pass their AEAD/reference checks on read. This avoids
reparsing the entire bundle at each internal check and makes no throughput claim.

The read API provides ordinal-ID pages, not event-time-sorted timeline output.
It checks configured writer/owner and pins AEAD cursors to the exact current
Binding/as-of/filter/versions. Key winner selection precedes lower-bound/filter
eligibility; stored-only winners remain stored-only and may be read through
canonical record APIs without exposing an older indexed version. The get_key API
returns the actual original winner. Examined rows/key probes, output entries and
exact encoded output are bounded. A changed current checkpoint invalidates old
cursors rather than silently mixing snapshots.

The driver removes the snapshot population caps from its paged Root counters:
records, retries, checkpoint history and physical inventory are disk trees with
checked u64 allocators; no whole-population row/retry map is opened. Per-request
1000-entry/32 MiB serialized windows and 64 MiB exact encoded read outputs remain.
This is a capacity boundary, not evidence of 10M throughput. In particular, legal
16 MiB text with worst-case JSON escaping exceeds the serialized request window
and is explicitly refused by the existing bounded serializer before publication.
The ASCII 16 MiB chunk fixture does not prove acceptance of every legal 16 MiB text.
A streaming serialization/export phase is still required to close that gap.

Prune builds a fresh DATA namespace solely from surviving
originals, not an in-place COW deletion leaving old payload pages. It streams
selected originals and scans every future-live row, actual DATA identity/layout
metadata and complete future-live file bytes with bounded per-row KMP proof.
A disk CONTROL file inventory binds exact paths, lengths and ciphertext digests.
Insufficient work or unknown state before publication yields no logical commit or
positive receipt. One retained checkpoint then publishes logical deletion and
pending cleanup. Different prune operations and appends remain fenced until the
same operation verifies immutable live files, retires every prior/orphan/partial
DATA namespace and signs its local completion. The reserved completion budget is
checked before logical publication. Crash retries use the retained exact inventory
and prior proof counts; they do not need to retain selected plaintext patterns.
Historical receipts are historical evidence and do not assert future durability
or absence after subsequent writes.

After retained writer denial, cleanup_paged_data can reopen only CONTROL/witness
and retire local DATA namespaces without obtaining a DATA key. The retained root
pins the enrolled data-root ciphertext digest. Every deletion is fenced by the
actual current denied authority and exclusive data owner. Remaining root enrollment
metadata is retained for refusal/audit; the API reports local payload namespace
removal and pending unsupported permanent erasure, not destroyed keys or backups.

Current source gates are reported per frozen checkpoint. The foundation at 5018e22
passed 31 managed cases (one subprocess helper ignored), including existing lifecycle
regressions. The final joined paged source gate passed 33 tests with one subprocess helper
ignored (30.80 seconds): actual pack/COW/control composition, original retries,
ordinal as-of pages, stored-only key winners, cursor/scope/byte/work refusals,
legal ASCII 16 MiB and explicit worst-escaping refusal, full end guards,
DATA-key-free denial cleanup and restored-backup refusal, eight KMP proof cases,
streamed signed removed-ID pages, and five actual SIGKILL publication/cleanup
boundaries. After the independent review corrections and real module reachability cleanup,
the complete managed filter passed 59 tests (two subprocess helpers ignored)
in 40.01 seconds. Engine library and tests strict clippy --no-deps -- -D warnings
passed in 1.47 seconds, with no engine warnings/suppressions; existing dependency
warnings are not an engine-wide dependency-clean claim. No performance, 10M-row,
full SPEC or provider acceptance is established.

A physical-root commitment is not proof of byte absence. Local retirement uses
the complete owned filesystem inventory including orphans and temporary files,
plus complete decoded and encoded scans before reporting local absence. Copied
keys, backups, filesystem freed blocks and external readers remain outside a local
scan. Immediate irreversible erasure of every backup remains unsupported by the
caller-retained key architecture; no final permanent erasure receipt is issued.

## Historical removed-ID proof without a population list

RetainedControl::pruned_ids reads only CONTROL, including after denial or removal
of DATA. Pages are pinned to one externally retained current floor. The numeric
start is an explicit inclusive ID range, not an authenticated opaque cursor.
Complete-operation verification starts at zero, follows every next value, and
feeds strictly ascending IDs into LifecycleReceipt::record_ids_verifier using
an externally pinned Ed25519 authority/key/scope. finish verifies the signed
count and exact original opaque-ID digest with constant population memory.
Missing, repeated, changed or reordered IDs fail; any push error poisons the
verifier. These are historical membership receipts, not fresh durability or
permanent-erasure proof. Page entries, encoded bytes and examined tombstones
have separate explicit bounds. An empty advancing page is possible when other
operations own the examined IDs.

## Observed failures and correction

The first actual prune run failed all five stories with Conflict and produced no
successful cleanup receipt. The generic metadata editor is insert-only; replacing
the pending-operation value attempted to mutate an immutable tree key. Completion
now has a distinct immutable key, retaining the original pending evidence.
The corrected focused run passed all five stories, including real SIGKILL at
staging, immutable control object, retained control publication, cleanup and ACK.
Final joined source evidence is reported separately.

A final-return source check also found prune's test ACK boundary after publication
without another current guard. It now verifies the full current authority before
returning the signed local receipt; a witness mutation at that boundary must fail.

## Joined capability boundary

The production driver currently uses no dictionaries and performs no dictionary
training/adoption or cross-pack dedupe. Lower immutable dictionary primitives
remain independently tested; they are not a claim that this driver adopts them.
Reference-only inventory and metadata diagnostics are unit-test helpers in this
join. Production cleanup uses the complete cipherless owned-namespace scanner,
including orphans and partial artifacts. No full metadata walk is added per append.
The obsolete duplicate test-only physical module is removed now that the real
production driver reaches the kernel.

Independent source review found read's final page serialization followed the
last current check. A deterministic final-boundary witness mutation reproduced
an actual returned stale page before the correction. The final current guard
now follows both cursor construction and exact encoded page validation, directly
before egress. The focused counterfactual failed with the guard absent.

Review also reproduced an uncharged-work failure: 32 empty owned namespaces were
removed and a cleanup receipt returned despite a one-row work limit. Each variable
namespace now prepays 20 work units for its bounded directory visits/removals,
with file actions charged separately. Prepublication retirement reserves the
same units plus the final live-only namespace check. Low-budget denied cleanup
refuses without completing; live prune with 128 empty orphans refuses before
logical publication and remains retryable with sufficient budget.

The final source contains no per-append population replay/metadata walk. Cleanup
work accounting includes empty namespaces, and final public page egress checks
current authority after its last serialization. The actual counterfactual read
and empty-namespace cases both failed before their corrections and pass afterward.
