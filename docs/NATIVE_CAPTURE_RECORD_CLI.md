# Explicit local Capture1 to managed Record CLI

The Linux command composes the existing protected Capture1 and managed loaders
with the [durable bridge](NATIVE_CAPTURE_RECORD_BRIDGE.md). It is opt-in; no
capture loop, daemon, legacy format, MessageCapture or cloud route selects it.

~~~sh
oojuro-native managed-ingest-capture --root /private/managed-operator --capture-root /private/capture-config --checkpoint /private/progress/capture-to-record --allow-capture-ingest --receipts 128 --input-bytes 33554432
~~~

Both namespaces must already have completed explicit setup. The source and
destination writer/owner must match their actual sealed/current authority.
The command never creates/enrolls a missing authority, key or witness. A valid
initialized empty Capture1 and managed namespace remain readable/ingestible.
Partial initialization requires explicit operator repair, not this command.

## Consent, paths and startup

The flag permits a local copy of already-authorized Stored parsed emissions.
It is checked before config/key/file inspection. It is not raw source recording,
raw export, cloud upload consent or a claim that arbitrary secrets were removed.
Raw source bytes/descriptors/parser context never appear in the report. Keys
remain in the existing private configuration; no key or credential enters argv.

Use absolute paths and an existing owned mode0700 progress directory. Its real
path must be ancestor-disjoint from source config/authority and target operator,
data, control and independently retained witness paths. Source and target roots
must also be disjoint. Symlink components, foreign-owned/unsafe writable
ancestors and an existing unsafe checkpoint file refuse before recovery
constructors. The checkpoint is the original sealed bridge format, not a new
sidecar receipt or second WAL.

Required source/control Store metadata, managed format/enrollment and witness
CURRENT must exist before constructors. The current data generation is checked
against actual retained control. Configured target origin/identity/key/signing
pins and current witness are reused, never inferred from a backup or a caller
writer flag. Source reads reuse the actual existing-only protected Owner loader,
including its existing bounded adapter/context/index validation. There is no
second forked authority loader.

Opening valid existing stores may perform their ordinary torn-tail recovery or
orphan cleanup. This is not filesystem read-only startup. There is no setup,
fresh header, implicit migration, source reparse or policy reinterpretation.

## Bounded report and retry

One invocation processes one bounded bridge page: default128 receipt entries,
maximum1000; default/max32MiB canonical encoded inputs. There is no watch loop.
The existing bounded JSON writer serializes the typed report directly within
one MiB, including newline. No unbounded intermediate JSON Value is allocated.

The report contains numeric scanned/withheld/no_output/stored/new/replayed/removed
counts, source-to-canonical ID/first-recorded_at mappings, receipt_only and more,
plus genuine current target binding. Field already_committed reports replay.
No record text, raw bytes, config path, key, parser error or private descriptor
is emitted. more=false describes only the checked prefix; later capture appends
can still be ingested using the same checkpoint.

An IO error or missing stdout acknowledgement can follow committed data and
saved progress. Retry the SAME roots and checkpoint. This resumes progress
without duplicating canonical rows; it does not promise replay of an entire
previous CLI report. Missing/rewound progress instead replays per-emission
original managed outcomes with the original canonical IDs and ingest times.
Pruned rows return receipt_only/removed rather than resurrecting, and current
writer denial refuses. Do not generate a new source identity to resolve an
uncertain acknowledgement.

The existing bridge's time, indexed/stored-only, original-provenance and canonical
duplicate semantics apply unchanged. This CLI does not enable unredacted direct
append, alter raw/policy receipts, invent key history or backdate recorded_at.

## Verification boundary

Synthetic actual subprocess stories exercise initialized empty then real native
capture/ingest, bounded continuation and later processes/get/status, preserved
full-width IDs/time/provenance/indexed flags, withheld text exclusion, original
retry, prune/receipt-only and denial; consent/no-bootstrap file absence;
private realpath/layout/scope/bounds refusal; broken stdout after commit; and
actual Capture compaction plus original-ID/clock replay. The focused offline run
passed all19 cases: five new CLI stories, nine managed CLI regressions and five
native read-service regressions. Owned binary/test Clippy diagnostics were zero.
The first run exposed an overstrict test expectation: a no-output receipt can
advance before a later oversized Stored emission refuses; the corrected fixture
checks that the refused emission itself advances neither checkpoint nor target.

No production/private source, live provider/AWS call, new infrastructure,
deployment, scale measurement or full-SPEC acceptance is claimed. The target
remains the bounded existing managed snapshot lane; paged target composition is
separate work.
