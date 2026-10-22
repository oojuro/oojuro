# Native commercial candidate: cumulative review

Oojuro is a standalone commercial flight recorder, retrieval and storage product. Soft Axiom is the first intended customer. Self-recovery, authorized agent handoff and authorized human review are requirements. Local durability and hosted replicated authority remain separate implementations.

## Current assembled implementation

- Explicit consented Codex, Claude and Pi capture: byte-exact sealed raw source plus atomic parsed receipts, immutable policy/source generation, crash recovery and restart-safe withholding. Pi is explicit current-v3 capture; observed branch history is not a reconstruction of active model context.
- Explicit Temporal observation streams use the same durable raw/parsed authority. Awaited worker recording returns local ACK; workflow sinks remain bounded best-effort diagnostics. Claim-check codec and activity-side recall preserve original IDs, scope and pinned file revisions.
- Native lexical retrieval; encrypted capture and Record caches; scoped receipt timeline/get/export; protected Linux capture and direct-Record CLI. The explicit local read service opens existing authorities only, protects root/socket ancestry, and serves bounded same-user Record/Capture reads plus pinned read-only MCP.
- Durable Record IDs, first ingestion time, key/version/as-of semantics, exact retry identity, winner-before-filter scoring and source-adapter provenance. Stored-only punctuation literals remain excluded from content search.
- Authenticated paged Record reader/postings with disk prefix, request, ID, key-version and segment directories. The opt-in disk path avoids whole-population Record locator maps. Explicit PagedStore ownership now bounds source frame/file/dictionary populations and composes with those trees; the default old Store and current writers still retain material population costs. Source verification and file/update amplification remain explicit; this is not complete external-memory writer storage.
- Fresh managed lifecycle enrollment with separately retained signed control and independently current witness, immutable data/control binding, exact original retry lineage, typed retention/prune, full staged scan and old-generation/derived-file retirement. Operator CLI exposes logical denial, current status and bounded signed audit export/verification. Caller-owned keys/backups still prevent a permanent-destruction claim.
- Pinned Ed25519 audit/checkpoint/export wire with explicit externally configured scope, authority/public key and independently current witness. Signature verification alone proves neither freshness nor physical erasure.
- Optional direct-RecordLog and genuine managed RAM ranker hooks, explicit shared candidate-pool/F2 policy, guarded sealed derived tables and whole-record context packing. Managed tables use the actual current data key and control binding; every callback rechecks current authority. Default/off/all-skipped retrieval stays lexical; plugin callbacks have a cooperative budget, not hard preemption. Filtered implicit-key lookup matches the original direct authority after an independently reproduced correction.
- Inactive hosted JS/TypeScript SDK: frozen original commits/searches, explicit status-only search retries, exact blob revisions, bounded file manifests/parts/digests and explicit selected cleanup. Raw capture forwarding remains off.

## Current joined evidence

The assembled source passes **169 engine tests (9 ignored helpers), 15 Store library checks (2 ignored helpers), 8 pure ranker checks, 83 capture tests, 38 CLI/MCP/read-service checks, 59 storage SDK tests and 24 Temporal/native/codec/recall Node tests**. All three strict storage SDK declaration smokes and the actual Temporal 1.24 type contract pass. Strict engine/capture/ranker lib+test and CLI/test lint passes. Tests run serially at the Rust harness level to avoid unrelated crash-driver fork descriptors temporarily inheriting Store locks; meaningful concurrency inside individual tests remains active. The raw wrong-key test still requires the exact WrongKey result after that temporary lock exits.

The bounded Store replay change separately passes all **1047 exhaustive byte cuts** in eight disjoint shards, plus encrypted/run/non-tail suites. Its local CRC-only scan comparison measured about 2.4% overhead; this is not a full baseline or network throughput result. The cloud candidate separately exercises the actual file/codec/recall and protected audit-export/CLI paths; local native tests do not establish hosted fault-domain durability.

The current read-service join reran 169 engine, 83 capture and all 38 CLI/MCP/read-service checks, plus strict affected-package lint. Earlier Store/ranker/Node/type evidence is separate; unchanged suites were not rerun for the read-service-only join.

These are local synthetic correctness and subprocess results, not hosted failure drills, production-corpus relevance evidence or launch scale proof.

## Actual implementation still in progress

| Track | Remaining code |
| --- | --- |
| Storage/query scale | Incremental cold-open work, paged writer/control integration, hosted materialization and background compaction; hot replay/training buffers and disk Record directories are implemented, but current 100k/128MiB limits are not raised |
| Lifecycle | Incremental managed writer/prune and raw-capture prune/resume invalidation; the current managed format rewrites whole bounded snapshots and is not a scalable writer |
| Audit | Durable capture/write event producers/timers and paged lifelong audit history; managed retained status/export/CLI are implemented |
| Retrieval | Paged/streaming query integration for optional ranking/context; genuine managed RAM authority/data-key integration is implemented, while optional adoption and relevance gains remain unclaimed |
| Capture/API | Native OTel/proxy, managed service and remaining mutation/MCP tools, harness-receipt to versioned Record integration, current/previous reader compatibility and upgrade tooling; no fictional previous native version is invented |
| Commercial integration | New-authority customer project/writer enrollment, hosted Record parity and full customer setup/lifecycle; local actual hosted file/codec/recall/audit joins are separate verified cloud fixtures |

The separate cloud candidate owns current committed authority, grants/read permits, human review, durable account admission, isolated key-service transport and provider/IAM source. Local labels and reported agent/run/parent values never authorize sharing or establish causal truth.

The SPEC's immediate irreversible destruction of all backup wraps remains an architecture gap under caller-owned keys or cancellable KMS scheduling. Logical denial/local prune/pending permanent destruction must remain distinct. Real corpus quality, 10M/100M records, 10k writers, throughput/RSS/latency and declared process/host/AZ/regional failure objectives still require measured acceptance. No public Temporal/Kafka equivalence or SLA is claimed.

No provider resources, paid calls, live secrets, deployments or migrations occurred.

The full requirement-to-owner matrix, including actual missing code and external gates, is in [PRODUCT_COMPLETION.md](PRODUCT_COMPLETION.md).
