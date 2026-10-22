# Native commercial candidate: cumulative review

Oojuro is a standalone commercial flight recorder, retrieval and storage product. Soft Axiom is the first intended customer. Self-recovery, authorized agent handoff and authorized human review are requirements. Local durability and hosted replicated authority remain separate implementations.

## Current assembled implementation

- Explicit consented Codex, Claude and Pi capture: byte-exact sealed raw source plus atomic parsed receipts, immutable policy/source generation, crash recovery and restart-safe withholding. Pi is explicit current-v3 capture; observed branch history is not a reconstruction of active model context.
- Native lexical retrieval; encrypted capture and Record caches; scoped receipt timeline/get/export; protected Linux capture and direct-Record CLI.
- Durable Record IDs, first ingestion time, key/version/as-of semantics, exact retry identity, winner-before-filter scoring and source-adapter provenance. Stored-only punctuation literals remain excluded from content search.
- First paged Record reader/cache with authenticated source-prefix witnesses, bounded payload decoding and historical scoring parity. Metadata, key-winner rescans and source Store still retain material whole-population or quadratic costs; this slice is not complete external-memory storage.
- Pinned Ed25519 audit/checkpoint/export wire with explicit externally configured scope, authority/public key and independently current witness. Signature verification alone proves neither freshness nor physical erasure.
- Inactive hosted JS/TypeScript SDK: frozen original commits/searches, explicit status-only search retries, exact blob revisions, bounded file manifests/parts/digests and explicit selected cleanup. Raw capture forwarding remains off.

## Current joined evidence

The assembled source passes **101 engine tests (6 ignored helpers), 75 capture tests, 13 actual CLI subprocess stories, 55 SDK tests and all 3 strict TypeScript declaration smokes**. Strict engine/capture lib+test and CLI/test lint then passes after a test-only redundant-clone correction. Tests run serially at the harness level to avoid unrelated crash-driver fork descriptors temporarily inheriting Store locks; meaningful concurrency inside individual tests remains active. The raw wrong-key test still requires the exact WrongKey result after the inherited lock owner exits.

These are local synthetic correctness and subprocess results, not hosted failure drills, production-corpus relevance evidence or launch scale proof.

## Actual implementation still in progress

| Track | Remaining code |
| --- | --- |
| Storage/query scale | Bounded disk metadata/dedupe/visibility directories, paged writer/control integration, Store memory/training bounds and background compaction; current 100k/128MiB limits are not raised |
| Lifecycle | Independently retained current control, managed erasure/retention/rewrite, sparse original IDs/time/retry lineage, verified local prune and actual derived-table retirement |
| Audit | Durable event producers/timers, operational verification/export and paged lifelong audit history |
| Retrieval | Frozen ranker hooks/table lifecycle/receipts and whole-record token/min-bits context packing; optional ranker adoption remains off |
| Capture/API | Temporal, native OTel/proxy/daemon/MCP, harness-receipt to versioned Record integration, current+previous reader compatibility/upgrade tooling |
| Commercial integration | Actual SDK-to-hosted restart/review/audit/file flow, operator enrollment and full customer setup/lifecycle |

The separate cloud candidate owns current committed authority, grants/read permits, human review, durable account admission, isolated key-service transport and provider/IAM source. Local labels and reported agent/run/parent values never authorize sharing or establish causal truth.

The SPEC's immediate irreversible destruction of all backup wraps remains an architecture gap under caller-owned keys or cancellable KMS scheduling. Logical denial/local prune/pending permanent destruction must remain distinct. Real corpus quality, 10M/100M records, 10k writers, throughput/RSS/latency and declared process/host/AZ/regional failure objectives still require measured acceptance. No public Temporal/Kafka equivalence or SLA is claimed.

No provider resources, paid calls, live secrets, deployments or migrations occurred.
