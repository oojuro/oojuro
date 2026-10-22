# Explicit local managed lifecycle commands

The oojuro-native managed-* commands are a separate Linux operator lane. Capture1,
Record2, their configuration schemas, and the normal product remain unchanged.
No existing data is imported, migrated, or reenrolled.

## Fresh setup and trust

Setup requires four NEW, pairwise disjoint absolute directories. Their parents
must already exist. Existing or partial roots refuse; setup never resets them.

~~~sh
oojuro-native managed-setup --root /private/operator \
  --data-root /private/payload --control-root /retained/control \
  --witness-root /independent/witness \
  --writer example --owner 1 --adapter reported-sdk
~~~

The operator root and all three authority/data roots use owner-only 0700.
The managed.json file, independent data/control encryption keys and the newly
generated Ed25519 PKCS8 signing key use owner-only 0600. Keys are generated
internally; none are accepted as values on the command line, printed, or read
from environment variables. Current config pins the original origin, writer
identity, authority, public key and exact canonical roots. Symlinked/repointed
roots, wrong key/pin, missing current witness and cross-namespace opens refuse.

The operator metadata/key root is outside data backups. Retained control and its
independent witness must also be outside data restores; witness must remain
outside control rollback. These are filesystem/operator trust assumptions.
Restoring all authorities together defeats rollback protection. This launcher
does not provision tamper-proof hardware or a remote key service.

## Direct typed input

Append requires a private 0600 regular no-follow JSON file containing an array of
the existing strict record::Input objects. All fields, including nullable key
and indexed, are explicit. Caller agent/parent/adapter fields are reported
provenance, not independently verified facts.

~~~sh
oojuro-native managed-append --root /private/operator \
  --input /private/records.json --request ORIGINAL_REQUEST_ID \
  --allow-unredacted-records
~~~

Consent is checked before root, key or input inspection. This is unredacted caller
data, not automatic capture or a redaction guarantee. Never put secret payloads
in argv. Exact original requests replay their original IDs and ingestion times;
changed inputs conflict. After pruning, an exact append retry can return
receipt_only and removed IDs without restoring content.

The managed-open command validates current enrollment and returns its opaque
binding. The managed-get --id N [--bytes N] command returns one bounded current
retained record. Every open/read/write checks the current independent authority;
there is no bootstrap on a read.

## Prune and logical denial

The managed-prune --input /private/selection.json --operation ORIGINAL_OPERATION_ID
command consumes exactly one existing typed selection from a private file:

- {"Ids":[41,42]}
- {"Agent":"reported-instance"}
- {"Key":"memory-key"}
- {"RecordedBefore":123456789}

The cutoff uses original engine ingestion microseconds, not caller valid/event
time. Results preserve the engine's exact removed IDs, scan counts, prior-generation
retirement, historical/current distinction and signed receipt. Shared live values
can prevent a byte-absence result. An old receipt after later changes cannot
establish current absence.

The managed-deny command commits administrative logical denial and remains
retryable after denial. It does not delete caller key files or claim local payload
cleanup. The managed-status command also works when payload storage is unavailable and reports retained snapshot counts (evidence
counts, not visible live bytes after denial), exact current floor and actual local
cleanup status.

**Permanent erasure remains unsupported while caller-owned keys or backups can
still decrypt copies.** Neither logical denial, local pruning nor a signed claim
is proof that all recoverable keys were destroyed. No permanent-destruction
command or success receipt is fabricated.

## Content-free audit pages

The managed-export [--start 0 --entries 100 --bytes 1048576] command emits one
bounded JSON page of signed checkpoint/event/opaque ciphertext-commitment evidence.
No record text, raw control ciphertext, private keys or raw control state is exported.

A continuation supplies both returned next and the exact original current floor
JSON via --start N --floor JSON. A changed tip refuses; a numeric ordinal alone
cannot silently resume another snapshot. Each page permits 1..1000 entries and
at most the retained audit API's 16 MiB output limit.

To verify a complete chain, collect the original ordered JSON pages, one per line,
in a private file and run:

~~~sh
oojuro-native managed-verify --root /private/operator \
  --input /private/audit-pages.jsonl
~~~

Verification streams bounded pages, starts at genesis, checks every signature,
head, previous link, ordinal, configured identity and final independently current
floor, and rejects mutation, truncation, reordering or stale history. It never
trusts a key/floor bundled by a sender. This command requires the local current
authority; public receipt signatures can separately be checked through the core
Verifier with independently distributed trust pins. Signatures prove producer
claims, not forensic key destruction or that nobody copied plaintext.

Verification defaults to a 16 MiB total input work bound, configurable with
--max-input-bytes up to 256 MiB; each page remains at most 16 MiB. It refuses
larger work instead of allocating a lifelong history vector. Other command
outputs default to 1 MiB with explicit --bytes bounded by the Record API's 64 MiB maximum.
Errors are fixed codes without private input paths or payload text.

## Verification scope

Synthetic actual subprocess stories cover setup, reopen, original retry/pruning,
all selection types, audit pages/current verification, denial, consent ordering,
permissions, wrong keys, missing authority, payload-only backup restore refusal,
denied audit access without payload storage, and bounds. Final exact CLI source
passes all nine actual subprocess stories and has zero Clippy diagnostics in the
new managed module and tests. Earlier source passed all thirteen existing Capture
and Record CLI regression stories; those production paths are unchanged by the
final managed-only bound correction.

Removing the actual managed dispatch reproduces a real command refusal; restoring
it passes the command stories. Removing the setup config-size preflight reproduces
an acknowledged setup whose JSON exceeds the strict reopen limit; restored source
refuses before creating any root. The first ordinary-ASCII long-path fixture was
undersized; the final fixture uses valid Linux filename backslashes that expand in
JSON and demonstrates the actual bound. This is not capture/raw erasure, permanent backup-key
destruction, a daemon/MCP lifecycle endpoint, a scale benchmark or live-provider
acceptance.
