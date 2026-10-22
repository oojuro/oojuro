# Explicit Capture1 ingestion into paged managed Record

This local Linux lane composes the existing Capture1 authority with the actual
[managed paged driver](NATIVE_MANAGED_PAGED.md). It does not convert a snapshot,
join MessageCapture, change daemon/default commands or forward data to a cloud.
The original [snapshot CLI](NATIVE_CAPTURE_RECORD_CLI.md) remains unchanged.

## Fresh explicit enrollment

Use four absent, mutually disjoint absolute paths under existing private owned
parents. Setup refuses existing roots and does not repair or reset partial setup.

~~~sh
oojuro-native managed-paged-setup --root /private/paged-operator --data-root /private/paged-data --control-root /private/paged-control --witness-root /private/paged-witness --writer WRITER --owner 7 --adapter capture-paged-bridge-v1 --first-id 1000
~~~

The protected operator file is managed-paged.json, separate from managed.json.
Existing key/signer files and independently pinned origin/identity/public key
fields retain their exact meaning. Setup explicitly calls create_paged; reads
explicitly call open_paged. A copied/renamed config cannot change the actual
PAGED_CONTROL or DATA format. Neither loader falls back to another layout.

An independently fresh target origin is required. This command does not migrate
a snapshot's data, retained root, request history or checkpoint to paged storage.
Creating a second fresh target intentionally creates another canonical namespace;
the bridge never claims cross-target deduplication.

## Existing-only ingest and inspect

The source must already be initialized Capture1 with the SAME writer/owner.
The progress parent must already exist privately and be realpath
ancestor-disjoint from every source/operator/DATA/CONTROL/witness root.

~~~sh
oojuro-native managed-paged-ingest-capture --root /private/paged-operator --capture-root /private/capture-config --checkpoint /private/progress/capture-to-paged --allow-capture-ingest --receipts 128 --input-bytes 33554432
oojuro-native managed-paged-get --root /private/paged-operator --id 1000 --bytes 1048576
~~~

Consent precedes authority filesystem inspection. It permits copying only the
already-authorized Stored parsed emissions; it is not raw recording/export or
cloud consent. Withheld emissions remain withheld. Stored-only indexed=false,
duplicate originals, original text/typed fields and sealed policy provenance
are preserved. Original harness time is valid_at; first canonical recorded_at is
assigned exactly once by the actual paged append, never copied from a harness.

Protected canonical ancestors, ownership, modes and pairwise layout checks run
before recovery constructors. Source Store metadata, PAGED_CONTROL/LOCK,
DATA format/enrollment/lock and witness CURRENT/LOCK must exist first. The
actual driver then validates the pinned independently current signed checkpoint,
encrypted root, enrollment and configured scope. No snapshot MANIFEST or
g-generation Store layout is assumed for the paged target. Ingest never calls
create_paged or enrolls a missing target. Ordinary valid-store startup recovery
remains possible; this is not a promise of filesystem read-only startup.

## Durable retry and bounds

The [bridge protocol](NATIVE_CAPTURE_RECORD_BRIDGE.md) is unchanged. A private
concrete engine target enum shares its source mapping/request/checkpoint code.
The paged path uses actual scoped_binding before/after every append and progress
save, not a fabricated snapshot/rank view or caller-implemented authority.

The source-keyed stable MAC plus exact source generation/span/emission identifies
each request. Exact Input semantics remain the paged driver's keyed conflict
fingerprint. Original assigned IDs/first clocks come from the retained request
receipt; removed rows return receipt_only/removed without resurrection. The
original sealed checkpoint pins target origin/identity and a genuine immutable
source-prefix ancestor. A snapshot/other-target checkpoint refuses. Progress is
advisory: missing/rewound progress replays original operations; lost stdout may
follow committed data/saved progress, so retry resumes rather than replaying an
entire previous report. Do not invent a new source identity after an unknown ACK.

One invocation is at most1000 receipts and32MiB encoded Input data, with an
explicit oversized-first refusal and no progress for that emission. Reports are
numeric-only, serialized directly through the bounded one-MiB writer; get bounds
its complete original record. No text/key/raw/path/private descriptor is logged
by ingest. An oversized serialized request, including legal worst-escaped large
text above the current driver's32MiB window, remains explicitly unsupported.

The target uses actual persistent COW roots and bounded request/receipt reads.
It is opened once per command, never replayed/reopened per emission. Capture1
source loading and its existing bounded read index still use RAM. This is not a
whole-ingest constant-memory, paged search/rank,10M throughput or full-SPEC claim.

## Verification

Authored focused engine stories cover actual paged original append/read pages,
time cuts, restart and tombstones; compaction/conflicting source payload;
configured scope/current denial and cross-layout progress; unknown ACK/denial
between append and checkpoint; bounds/wrong target; and a real SIGKILL after
append before progress. Actual subprocess stories cover fresh explicit setup,
policy-bound Capture ingestion and get/restart/compaction, protected existing-only
refusal and layout separation, and closed stdout with unchanged retry outcomes.
The original snapshot bridge/CLI and managed CLI cases remain regression gates.
Actual results are recorded after the composed focused gate.

No provider/AWS call, deployment, private source, scale measurement, new training
or irreversible key destruction is claimed.
