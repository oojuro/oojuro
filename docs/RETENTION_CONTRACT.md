# Oojuro retention and deletion contract

Owner direction, October 22, 2026: follow Kafka, Redis and Temporal. This replaces the earlier unresolved choice for the standard hosted launch. It is an engineering contract to implement and verify, not a claim that the lifecycle service is complete.

## What the reference systems document

| System | Documented behavior | Oojuro design implication |
| --- | --- | --- |
| Kafka / Confluent | Per-topic retention and background compaction. Tombstones represent deletion; compaction timing is nondeterministic and maximum compaction lag is not a hard completion guarantee. [Compaction](https://docs.confluent.io/kafka/design/log_compaction.html), [topic configuration](https://docs.confluent.io/cloud/current/topics/manage.html) | Record deletion and physical reclamation have distinct progress; configure and monitor retention and cleanup. |
| Redis | UNLINK removes keys from the keyspace and reclaims memory asynchronously. RDB snapshots and AOF are separate persistence artifacts; the documentation describes recovering a flushed dataset from an unrewritten AOF. [UNLINK](https://redis.io/docs/latest/commands/unlink/), [persistence](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/) | Stop serving deleted data while tracking retained copies separately. A live delete is not an assertion that all historical copies vanished. |
| Redis Software / Cloud | Software exposes a separate backup-history retention setting; Cloud exports RDB backups to configured storage locations. [Software backup settings](https://redis.io/docs/latest/operate/rs/references/rest-api/objects/bdb/), [Cloud backups](https://redis.io/docs/latest/operate/rc/databases/back-up-data/) | Backups need explicit lifecycle configuration, inventory and cleanup evidence. Do not infer one universal backup retention period. |
| Temporal | Cloud namespace retention controls how long closed workflows are preserved. DeleteWorkflowExecution is explicitly asynchronous. Self-hosted archival can retain separate blob copies beyond primary persistence retention. [Cloud retention](https://docs.temporal.io/cloud/tcld/namespace#retention), [actual API contract](https://github.com/temporalio/api/blob/master/temporal/api/workflowservice/v1/service.proto), [self-hosted archival](https://docs.temporal.io/temporal-service/archival) | Separate operational history retention, explicit deletion progress and optional archive lifetime. This is not a statement that Temporal Cloud uses self-hosted archival. |

These products have different implementations and guarantees. The following is Oojuro's adaptation of the documented patterns.

## Contract to implement

1. **Current access:** acknowledge a delete or revoke only after its authoritative change is durably committed. Current reads, search, exports and key unwraps enforce that change. Active streams recheck current authority before further payload and clean completion; bytes already delivered cannot be recalled.
2. **Retention policy:** configure primary records, derived indexes, versions, archives and backups explicitly. Expose the applicable policy and cleanup progress. No numeric deadline is inferred from another vendor's defaults.
3. **Background purge:** remove eligible primary and derived data through bounded, idempotent, recoverable work. Failed or unknown work remains pending and visible; a configured eligibility time is not automatically a completed purge.
4. **Backups:** encrypted historical copies can remain until their documented backup lifecycle expires. Inventory retained copies and monitor cleanup; never describe an unexpired backup as destroyed. Customer-controlled exports are outside Oojuro's deletion authority and must be identified as such.
5. **Restore:** before restored data can serve, apply independently retained current deletion and authorization state. Missing, stale or unverified current control fails closed. Restore must not resurrect an acknowledged deletion or revoked grant.
6. **Honest status:** distinguish access revoked/deleted from cleanup pending and verified purge. Existing stronger cryptographic destruction proof remains valid only when independently established for its stated scope. A KMS schedule, NotFound response or outage is not final destruction proof.
7. **Evidence:** retain bounded metadata needed for authorization, deletion receipts, accounting and recovery under separate documented retention. Avoid retaining deleted payload in audit or error fields.

## Change to the earlier specification requirement

For the standard hosted launch, the owner-directed contract above replaces the requirement to prove immediate irreversible destruction of every backup wrap in SPEC §4.3/8. This does not weaken current access checks, justify skipping background purge or authorize falsely reporting permanent cryptographic destruction.

A separate immediate irreversible destruction tier is not promised. The historical frozen specification and original receipts remain reference evidence; active product requirements and readiness documents must point to this explicit amendment.

## Remaining implementation and acceptance

Actual lifecycle configuration/jobs, backup inventory/expiry, operator status, restore refusal and replica/stream revocation gates must be composed and verified. Component checks or a vendor comparison do not establish Oojuro's durability, availability or purge SLA. No production resource, key or user data was changed by this document.

AWS KMS-specific caveat: generated-key deletion has a cancellable 7–30 day wait, and scheduling alone does not immediately invalidate previously decrypted data keys. [AWS documentation](https://docs.aws.amazon.com/kms/latest/developerguide/deleting-keys.html).
