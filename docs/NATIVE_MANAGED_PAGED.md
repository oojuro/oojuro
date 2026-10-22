# Incremental managed paged layout (implementation in progress)

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

## Current evidence and remaining implementation

The source currently implements and verifies the bounded control-tail/root and
checkpoint ancestry foundation. Focused tests cover direct current open without
genesis, missing current refusal, old snapshot mode refusal, published-birth/sibling
and root namespace separation. Existing managed lifecycle tests still pass.

The DATA driver, original append receipts, streaming reader/visibility, and bounded
prune proof are not complete at this foundation checkpoint. Kernel pack/COW tests
do not establish a committed managed append or complete paged lifecycle. There is
no performance, 10M-row, or full SPEC acceptance claim.

A physical-root commitment is not proof of byte absence. Local retirement will
need a complete owned filesystem inventory including orphans and temporary files,
plus complete decoded and encoded scans before reporting local absence. Copied
keys, backups, filesystem freed blocks and external readers remain outside a local
scan. Immediate irreversible erasure of every backup remains unsupported by the
caller-retained key architecture; no final permanent erasure receipt is issued.
