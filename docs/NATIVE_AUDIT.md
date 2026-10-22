# Opt-in native audit signing wire

This milestone supplies content-free Ed25519 checkpoint, event-chain, verification and JSON
export APIs in `oojuro_engine::audit`. It does not enable audit on existing stores or add
signing cost to their default write/query path. It is a shared cryptographic wire, not the
complete SPEC 9 storage integration or the audit CLI.

## Trust and authority

Create a separate project signing key outside data backups. `Signer::from_pkcs8` accepts
caller-owned PKCS8; it never returns the private key. `Verifier::new` requires the expected
authority, domain and public key from an independent trusted configuration. The public key
included in an export is for distribution; it cannot authorize itself. Native lifecycle and
hosted read-audit use separate key authorities and domains. This API does not persist keys,
perform rotation, call providers, or prove that a key has no copies.

Signatures use ring Ed25519 over a fixed binary domain-separated preimage, with fixed-width
little-endian integers and opaque 32-byte commitments. JSON ordering is not signed.
A signed checkpoint includes schema, domain, authority, scope, checkpoint ordinal, event count,
previous signed-checkpoint digest, chain head and the signer's claimed time. The time is not an
independent timestamp. Checkpoint ordinals advance even if timer checkpoints have no new events.

## Current witness and forks

`verify_checkpoint` verifies the pinned signer and an explicit expected scope.
`verify_chain` checks every signature/link and requires an externally trusted **current**
checkpoint. An optional retained floor must occur exactly in the verified ancestry; a larger
ordinal on another branch cannot satisfy it. A range may begin at that exact floor; otherwise
it must begin at genesis. A floor alone proves no rewind behind that checkpoint, not freshness
after it. Obtaining the current witness from the same untrusted/restored bundle defeats this
protection. Authority unavailability must be handled fail-closed by the production owner.

Native `AuditExport::verify` recomputes every event and signed checkpoint from genesis,
checks every checkpoint against the actual event prefix, and refuses uncheckpointed tails.
Export encode/decode is bounded to 100,000 entries/checkpoints and 16 MiB. Unknown fields,
domains and versions fail closed on verification. Producers must authenticate actors/scopes and
supply accurate ciphertext before/after commitments and counts. IDs must be opaque authority
identifiers; hashing a denied/private plaintext value is forbidden. Arbitrary content, raw
descriptors, parser context and plaintext-derived fingerprints are absent from this schema.

The hosted read-audit owner can use the same `CheckpointBody`, `Signer` and `Verifier` for
its own encrypted event-chain head. It must verify that chain and authenticated scope itself;
the native clear content-free `AuditEntry` hash is not its encrypted-chain format. Durable
publication before sensitive egress, retained CAS heads and signing-root loading remain the
hosted owner's responsibility.

## Deletion and remaining implementation

Lifecycle event kinds distinguish logical denial pending permanent destruction from verified
live rewrite/retention. There is deliberately no permanent-destruction event: today's native
caller-held `FrameCipher` key and cloned ciphers can decrypt retained copies. A root that can
unwrap old backup wraps cannot establish immediate permanent destruction of those wraps.
KMS scheduled deletion/disable or a missing object is not permanent-destruction evidence.
A signature proves a signer asserted something, not that all key copies were destroyed.

Managed lifecycle control, retained tombstones, complete live-file rewrite/scans, explicit raw
pruning, timer/append checkpoint scheduling, production status/verify/export commands and
backup recovery/destruction authority remain separate implementation stages. No local-file,
freed-block or backup erasure acceptance is claimed by these signing APIs.

## Verification

On this candidate, the offline engine suite passed 89 tests (4 existing helpers ignored),
including nine new audit tests. Package-only strict Clippy and diff checks pass. The new
tests mutate every signed field, replace the export key, test an alternate longer signed
fork against an independently retained floor, truncate the suffix against a current witness,
check every event/checkpoint link, exercise hosted opaque ciphertext heads and timer-only
checkpoints, and reject oversized/unknown-field exports. The first compiler invocation
needed the existing cached C toolchain on PATH. Whole-dependency strict lint additionally
finds an unchanged Store iterator-style warning; package-only lint isolates this slice.
