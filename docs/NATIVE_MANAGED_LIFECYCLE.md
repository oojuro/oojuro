# Explicit managed snapshot lifecycle

This is a fresh, opt-in direct-record namespace: `record::managed::ManagedLog`. Existing
Record2, CaptureLog, raw capture, paged readers and default write paths are unchanged.
There is no implicit import or migration. A managed namespace refuses a different format.
Capture raw sources and parser context are not opened or pruned by this API.

## Authority and actual erasure boundary

Create three independent directories: managed data, retained control, and retained witness.
`FileWitness::open` and `RetainedControl::open` own their exclusive locks; `ManagedLog::open`
takes the data lock. Control has a distinct encryption key and independently configured
Ed25519 signing key/authority. Produce the expected writer identity commitment with
`identity_binding(data_key, identity)`. None of these directories may overlap; data/control encryption-key reuse is rejected.

Control and witness must be outside data backups/restores; witness must also be excluded
from control rollback. The witness is an independent trusted filesystem authority, not
tamper-proof hardware. Restoring both authority and witness defeats this assumption.
Restoring old data, restoring control behind its retained witness, or removing the witness
from existing control fails closed. Missing current data never falls back to old generations.

Every managed read, binding/capability acquisition, append and prune checks current retained
control/witness. `RetainedControl::deny` commits logical denial even for an already-open
owner; subsequent operations fail. The mutex holds current-state validation and publication
together. Already delivered plaintext cannot be recalled.

**Immediate permanent crypto-erasure of every backup wrap remains an unmet architecture
contract.** Callers retain data keys and can clone FrameCipher; a retained root that unwraps
old wraps has the same problem. A test deliberately decrypts a copied backup through the
primitive after denial while managed reopen refuses it. No destroyed flag, KMS schedule,
disable, or missing-object status resolves this. `finalize_permanent_destruction` always
refuses. Writer status stays denied/pending permanent; partial prune reports
`PendingUnsupportedLocalKeyAuthority`. No permanent receipt is issued.

## Format, provenance and retries

Stable random origin and keyed writer identity are independent of physical generations.
Each generation is a sealed Store containing bounded snapshot chunks. Physical commitment
covers every named live Store file's bytes. Logical receipt lineage uses a separately
generated secret intent key inside the sealed snapshot: never a public payload/context hash.

Survivors retain sparse original IDs, engine ingestion times, valid times, initial indexed
flag, caller agent/parent labels, key, adapter and typed fields. Labels remain caller
provenance, not independently verified causal truth. IDs/times advance from original receipt
history including removed rows, so IDs are not recycled. Request tags/fingerprints and ACKs
survive without removed text. Changed retries conflict. Exact retries after pruning return
the original ACK with `receipt_only` and removed IDs: historical commit evidence, not a new
retention, projection or durability promise.

This first protocol writes O(live state) per mutation. It retains native 100,000 original-ID
and 128 MiB serialized-state ceilings. Pruning cannot discard immutable ACK history to reset
the lifetime ID bound. These are prototype state bounds, not exact RSS/throughput claims.
No 10M/full-scale, incremental-writer or paged-query acceptance is claimed. The next adapter
must share this control/lineage protocol rather than add a competing managed format.

## Publication and recovery

1. Validate current retained authority and bounded mutation.
2. Create a generation above every prior data/cache ordinal; write and sync.
3. Decode every carrier and compare the complete snapshot, including all survivors and
   retry/provenance metadata, with the intended snapshot. Recompute every-file commitment.
4. For prune, scan all surviving relevant decoded strings/attribute metadata and header
   labels, every staged Store file, and root marker/lock bytes. No short-needle exclusion,
   sampling, ignored open error or unknown-file skip establishes proof.
5. Append/sync signed sealed control, then atomically sync its independent current witness.
   Only that authority selects the generation.
6. Remove all prior owned data/cache generations, sync retirement, recheck exact current
   physical commitment, then publish the final signed local receipt.

Crash before publication leaves old authority. A signed durable control append interrupted
before witness advancement recovers only after signature, exact floor ancestry, identity
and transition validation; witness is then finished. After publication, reopen selects new
data. Pending cleanup blocks new appends; exact prune retries resume cleanup. SIGKILL tests
cover stage, control append before witness, published control, cleanup and final ACK.

Unknown files and symlinks cause refusal; they are not silently removed. Derived directories
accept coordinated rank registry names `RANK_TABLE`, `LOCK`, `tmp-<64 lowercase hex>.bin`.
Queries do not implicitly write caches. Every generation invalidates old bindings/cursors/
cache capabilities. Copied keys still decrypt copied caches outside managed authority.

## Selections, scans and public receipts

Selections cover IDs, matching caller agent labels, all versions of a key, or an original
**engine recorded-at** cutoff. Claimed valid/event time does not set retention. Capture raw,
source generations and parser-resume state remain a separate explicit prune stage.

Decoded shared and encoded-byte occurrences are separate. Surviving values, header labels
and fixed marker bytes prevent blanket byte absence. One-byte needles are scanned; ciphertext
coincidences are reported. Local absence requires zero occurrences and complete prior-generation
retirement. Scope is current tracked files, excluding plaintext in memory, freed disk blocks,
filesystem snapshots, external copies and backups.

Public `LifecycleReceipt` binds a typed content-free event and `ReceiptDetails` plus opaque
control-ciphertext commitment under a signed checkpoint. Details include exact opaque numeric
ID-set commitment, recorded-at cutoff and both scan counts. `verify` pins configured authority/
domain/key/project scope; `verify_record_ids` checks returned IDs. The ID commitment concerns
documented provenance IDs, never private text/context. Standalone JSON is bounded to 64 KiB
and rejects unknown fields. Signatures assert producer claims, not forensic key destruction.

A prune retried after later mutations is historical and cannot assert current byte absence.
Selected bytes mean encoded record JSON, not freed blocks or destroyed backup bytes. Current
authority/witness is required for freshness; a sender-supplied signature is insufficient.

## Remaining work

Capture/raw prune, provider destruction authority, production audit commands and timer
checkpoints, and paged/incremental managed writer remain separate requirements. Authenticated
rank-view/derived-sealing integration is coordinated with rank table ownership; an unrelated
RecordLog cannot stand in for managed authority. None is inferred complete from snapshot tests.

## Frozen local verification

The complete engine suite on runtime a5c6ed0 passed 103 tests; five existing/subprocess
helpers were ignored. Strict package library-and-test Clippy and diff checks passed.
Thirteen active managed tests include actual SIGKILL at five publication boundaries,
sparse original IDs/times/retry lineage, stale data/control and missing-witness refusal,
active-owner denial, honest primitive backup decryption, key separation, corrupt/foreign
data, unknown residue, shared one-byte strings, complete root-marker scanning, original
recorded-at retention, and independently verifiable public receipt metadata mutation.
This is local synthetic filesystem evidence, not live provider/backup-destruction acceptance.
