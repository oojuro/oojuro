# Foundation and Soft Axiom storage review

Review date: 2026-10-22 America/New_York. Reference engine: `3a17d1d`.
Oojuro is a standalone commercial agent flight recorder, with a public local engine and separate hosted service.
Soft Axiom is its first customer. Agents review their own history, other agents read only authorized shared history,
and people inspect/export traces through authenticated review surfaces. This is a review package, not a completed
replacement engine, production readiness claim or cutover instruction.

## What exists and what this finishes

The current engine is a functioning local compressed journal with lexical retrieval. The design draft left by the prior
session is now reconciled against source documentation, paired with a schematic, and bounded by the migration gates below.
The draft is preserved in its original checkout; no stores, services or customer data were modified for this review.

| Component | Evidence and state |
|---|---|
| Lexical baseline | [search fixes](../SEARCH_FIXES.md), [performance round 2](../PERF_ROUND2.md); reference measurements, not rerun here |
| Byte-exact raw capture | `lossless-exp`, commit `c7bae21`, `docs/LOSSLESS_EXPERIMENT.md`; prototype examples, not the default daemon |
| Store sealing | [PR #4](https://github.com/oojuro/oojuro/pull/4): preserved sealing implementation plus regression review; 27 store tests and application compilation pass; low-level prototype, no capture integration |
| Ranker seam | [PR #2](https://github.com/oojuro/oojuro/pull/2): separately reviewed standalone crate, 8 synthetic conformance tests; no shipped semantic ranker or live query integration |
| New consolidated engine | [PR #5](https://github.com/oojuro/oojuro/pull/5): sealed raw journal; [PR #15](https://github.com/oojuro/oojuro/pull/15): explicit file adapter; [PR #16](https://github.com/oojuro/oojuro/pull/16): atomic Codex raw/parsed capture; [PR #17](https://github.com/oojuro/oojuro/pull/17): ready bounded native lexical port. Persistent segments/merge, complete format/audit/erase/adapters and section14 gates remain unfinished. |
| Information-theory methods | [PR #3](https://github.com/oojuro/oojuro/pull/3): reproducible aggregate of cached exploratory runs; exposed evaluation set, no clean adoption verdict |
| Hosted API | [cloud PR #1](https://github.com/oojuro/cloud/pull/1): durable control commits, tenant-bound restore and atomic blob authorization; 86 tests pass, 2 intentional load tests ignored |

Historical figures in SPEC.md are scoped research observations. The 14.5% raw ratio is not an upper bound for arbitrary
inputs; incompressible/encrypted bytes may not compress. The new format has not been measured.
The cached own-history hybrid reaches recall 0.747 versus lexical 0.630 and Qwen3 0.781; this is exploratory evidence, not a held-out adoption result. The shared starter headline has essentially no gain. See PR #3 for uncertainty, missing scenarios and reproducibility.

The reference labels F1 (time view), F2 (anchored continuation), F3 (background floor), F4 (tie rule) have been checked against
SEARCH_FIXES.md. Proposed writer-score isolation and redundancy-aware packing are separate changes requiring their own evals.

## AWS can host Oojuro, with a bounded role

The repository already describes one EC2 host running the service with retained EBS data and hourly encrypted S3 backups.
That is an AWS hosting design. It is not a completed live deployment verification, a distributed database, or S3-authoritative
storage. No new AWS resources, costs, secret reads, or live cutover are part of this work.

The useful first role for Soft Axiom is agent transcripts, explicit memory notes and immutable file/tool/artifact payloads referenced
by stable ids. Postgres remains the account/wallet authority and Temporal remains the workflow durability authority.
Accounts, permissions, billing, workflow scheduling and transactional application state keep their own explicit
systems of record until a separate correctness contract and migration are approved. An append-only journal cannot implement
arbitrary multi-record application transactions merely because it can store JSON.

The following table describes the legacy hosting baseline; the inactive protocol and composed candidates are distinguished in the readiness matrix below.

| Requirement | Legacy design / evidence | Gate before primary agent storage |
|---|---|---|
| Acknowledgement | local fsync journal plus SQLite/blob stores; S3 backup is asynchronous | kill/power-loss tests prove all acknowledged records, idempotency state and metadata recover consistently |
| Multi-agent writes | one owning service process serializes engine mutation | concurrent retry tests, stable per-writer ordering, documented read-after-write scope |
| Multi-host writers | no distributed lease/consensus | retain one owner; prevent a second owner from opening shared/restored volume; durable fencing before any failover |
| S3 authority | hourly backup only | choose declared RPO/RTO for EBS authority, or build object commit protocol before claiming S3 authority |
| Losslessness | hosted copy is redacted parsed text; raw stays local | retain harness originals; byte-exact cloud claims need separately approved encrypted raw transport/storage |
| Isolation | project API key + restricted writers, filesystem separation | adversarial auth tests for every read/mutation/export; backup identity and restore paths bound to project/writer |
| Encryption/erasure | backup encryption prototype; store sealing separately reviewed | test revocation using an OLD backup with every still-usable wrapping key; include metadata/index/cache residue |
| Recovery | service-stopping encrypted snapshot/restore scripts | isolated restore, replay counts/digests, corruption and wrong-key rejection, measured restore time |
| Availability | one EC2 process + backup downtime | agree outage/maintenance budget; bounded graceful restart and health checks; independent alerting |
| Migration | no production migration performed | shadow copies, reconciliation, explicit owner acceptance, reversible read switch |

### Confirmed product and durability target

The owner confirmed authority scope: **agent transcripts, memory and files only**. Accounts, wallets and workflow state
remain in their existing systems. The requested service target is comparable to Temporal/Kafka for acknowledged writes
surviving process, host and Availability Zone loss. Regional disaster recovery is a separate objective.

[Temporal Cloud documents synchronous storage across three AZs before acknowledging a write](https://docs.temporal.io/evaluate/cloud).
[Kafka's replication design](https://kafka.apache.org/43/design/design/) requires the appropriate in-sync replica policy as well
as `acks=all`; an `acks=all` setting by itself can acknowledge a single remaining replica. Refusing writes with insufficient
replication and fencing stale leaders are part of the target. These references establish the requested failure contract;
Oojuro has not implemented equivalent hosted replication.

The existing hourly backup path cannot meet that target. The hosted committed-prefix prototype is a separate implementation
track: immutable durable transaction/blob objects, one conditional authoritative head, owner epochs, exact request results,
key/deletion state in the same committed prefix, and watermarked derived reads. Local precommit appends must not be searchable.
A test filesystem backend can establish protocol behavior under its declared contract; it cannot certify AWS multi-AZ durability.
Kafka/MSK as a replicated journal remains an alternative to implementing the conditional-object protocol. No provider deployment
or billable infrastructure choice has been made by this review.

### Failure domains and the acknowledgement choice

| Failure | Existing design boundary | Evidence still required |
|---|---|---|
| Process SIGKILL | local journal fsync and durable SQLite/blob commits should recover acknowledged state | executable restart gate; not a disk power-cut test |
| EC2 host replacement | retained usable EBS can be reattached in its own AZ | fence old owner, repair/mount/check/replay, measured restore time |
| EBS volume loss or AZ loss | restore last completed encrypted S3 backup; later acknowledged writes can be lost | monitor last successful backup, include key authority and control state, test complete restore |
| Region loss | current same-region backup design does not establish independent regional recovery | separate region/key recovery design and explicit owner approval |
| Credential/key compromise or deletion | encrypted data alone does not protect an available decrypt key or revoked key copies | scoped IAM, tested key revocation, access audit, independently recoverable control/key policy |

AWS documents [EBS attachment within the same AZ](https://docs.aws.amazon.com/ebs/latest/userguide/ebs-attaching-volume.html).
[S3 Standard stores across at least three AZs within a region](https://docs.aws.amazon.com/AmazonS3/latest/userguide/DataDurability.html);
One Zone classes have a different failure boundary. The current [S3 FAQ](https://aws.amazon.com/s3/faqs/) says S3 verifies correct redundant storage across devices before returning a successful upload. Its current wording does not explicitly promise waiting for all three AZs before acknowledgement in the same terms as Temporal Cloud. Treat the current documented S3 Standard durability/AZ-loss design as a provider foundation; stronger equivalence requires confirmation plus application acceptance. These service properties do not prove the application's recovery behavior.

The hourly schedule is not a hard one-hour maximum loss bound: failed backups can make the last successful recovery point older.
A shorter asynchronous schedule still has a nonzero loss window. If the owner requires no acknowledged-data loss on volume/AZ failure,
acknowledgement must wait for the required independent durable authority; the current backup path cannot supply that guarantee.

### If S3 is to hold the authoritative journal

Implement and test a distinct commit protocol: immutable per-project/writer objects, content digest and format version,
unique generation/sequence bounds, then one authoritative manifest/commit pointer advanced conditionally under a fenced
single owner. A success response follows durable object and commit-marker acknowledgement. Persist request idempotency
with the commit, so retry after any ambiguous response returns the original result. Query replicas may cache only committed
generations and must rebuild derived indexes from that manifest. A stale cache must expose its watermark or wait for the
caller's acknowledged watermark. A local fsync followed by later S3 upload does not meet this contract.

Test process death after each upload/manifest step, duplicate requests, competing owners, stale manifests, missing/corrupt
objects, erased-writer recovery, and snapshots during writes. Do not assume S3 alone supplies writer ordering or a database
transaction. AWS provides [conditional creates and compare-and-swap writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html) through `If-None-Match` and `If-Match`. [Cloud PR #7](https://github.com/oojuro/cloud/pull/7) implements a bounded experimental protocol with an independently current head and a local test backend. It does not include an S3 adapter or production serving integration. KMS authority, wrapped-DEK lifecycle, deployment IAM/fencing and host/AZ recovery still require implementation and acceptance. This review did not query live AWS state.

## Implementation continuation

[Engine PR #5](https://github.com/oojuro/oojuro/pull/5) implements the first sealed byte-exact raw-source journal slice,
with cursors reconstructed from its existing Store journal. Nine active raw tests and four strict-sealing regressions pass,
including the independently reviewed failed-initial-directory-sync retry gap; application compatibility and scoped strict clippy pass.
[Engine PR #15](https://github.com/oojuro/oojuro/pull/15) adds an explicit per-source-policy file adapter with sealed provenance,
full retained-prefix validation and caller-owned generation identity. All five file tests and scoped strict clippy pass,
including an independently reviewed final-component symlink-swap race fixed by atomic no-follow open. Both engine slices are ready for review.
Cloud PRs [#2](https://github.com/oojuro/cloud/pull/2), [#3](https://github.com/oojuro/cloud/pull/3) and
[#4](https://github.com/oojuro/cloud/pull/4) continue exact retry authority, conditional blob revisions and current deletion authority.
[Cloud PR #6](https://github.com/oojuro/cloud/pull/6) composes exact retry state and deletion authority so an old archive cannot
recreate erased records on retry. Its full current-source suite passes 117 tests with two deliberate load tests ignored, including archives before
project-journal creation and after the append, key-save-failure status, startup freshness and restart. [Cloud PR #7](https://github.com/oojuro/cloud/pull/7) adds the isolated
committed-prefix protocol prototype: twelve tests plus a bounded local CPU/crypto/fsync measurement pass. Warm record/memory/one-blob
batches measured approximately 165–177 commits/s on the development filesystem; this is not S3 latency or an AZ survival result.
[Cloud PR #9](https://github.com/oojuro/cloud/pull/9) supplies inactive shared real lexical materialization over committed rows,
with six tests and explicit per-writer tmpfs caches. [Cloud PR #10](https://github.com/oojuro/cloud/pull/10) supplies an inactive
S3 Standard CLI adapter with seven fake-transport/CLI tests. Each remains governed by the unimplemented key/control/runtime gates below.

[Cloud PR #8](https://github.com/oojuro/cloud/pull/8) implements authenticated human timeline/search/provenance detail/export;
its 43 tests pass. [Cloud PR #11](https://github.com/oojuro/cloud/pull/11) is the ready runnable hosted review slice,
combining storage repairs, agent handoff/provenance and human review. Evidence is deliberately separate: the storage base passed
117 tests (two opt-in load tests ignored), the first product composition passed 47 checks, and the final authentication composition
passed 23 affected checks. These are not a claimed fresh aggregate/full-suite run.

[Cloud PR #13](https://github.com/oojuro/cloud/pull/13) persists current-authority revoked-key intent before control-DB mutation,
replays it before loading keys, rechecks cached authentication, and enforces a service/offline-admin lifecycle lock. Browser
sessions and magic links are invalidated on boot/restore; service restart requires new sign-in/link. Its 58 unique focused checks
pass. The credential-aware authority requires strict format 3 with no implicit legacy migration; unknown historical revocations,
incomplete retry history and earliest nonvalidating binaries are not represented as protected by a version field.

[Cloud PR #12](https://github.com/oojuro/cloud/pull/12) is the separately ready inactive S3-adapter/shared-lexical composition.
Its cross-component synthetic story covers real lexical IDs/scores, record/memory/blob, exact recovered retries and revoked-key
refusal through the actual adapter with fake S3 transport. It does not switch the runnable product to S3 or prove live provider durability. None is a declaration that the new core or commercial hosted product is finished.

## Composed review entrypoints and readiness

These levels are separate; passing a component's unit tests does not complete the
commercial product or the consolidated engine specification.

| Level | Review entrypoint and current evidence | Remaining acceptance |
|---|---|---|
| Runnable App candidate | [Cloud PR #14](https://github.com/oojuro/cloud/pull/14), ready: storage safety fixes, scoped agent handoff, other registered-human read-only trace review. Complete current-source suite **131 passed, 0 failed, 2 intentional load tests ignored**; independent source review cleared the HTTP runtime blocking issue. Runtime source `81fa312`; later commit only records evidence. | Local durability/offsite-pending ACK is still distinct from replicated hosted ACK. Deliberate authority format4 migration, deployment review and actual customer acceptance remain. |
| Local core candidate | [Engine PR #16](https://github.com/oojuro/oojuro/pull/16), ready at `45b6b07`: atomic sealed raw bytes + parsed/policy/context/ID outcomes, explicit Codex source adapter, real tmpfs lexical projection. Final **43 capture + 18 engine tests pass**, one pre-existing helper ignored; strict scoped lib/tests clippy passes. Independent source review cleared complete resume-state, unresolved-call/lookup and duplicate-ID policy fixes. Earlier nine legacy-ingest tests pass separately. | This is a Linux local-durability candidate with explicit unresolved-context bounds; it is not the consolidated query/index port, all-adapter support, hosted key authority or multi-AZ ACK implementation. |
| Native lexical candidate | [Engine PR #17](https://github.com/oojuro/oojuro/pull/17), ready at `1afbc51`: native postings/query over committed receipts, no nine/trace production dependency in engine and no oracle/tmpfs projection in opt-in LocalCapture. **43 capture +24 engine tests pass**, one existing helper ignored; scoped strict lint passes. Above-floor/pruning/long-ID bitwise differential fixtures and complete replay-metadata capacity refusal/reopen are covered. | In-memory derived view only. Persistent native segments/codec/merge are being implemented separately; full section14 corpus/size/performance and remaining SPEC work are not complete. |
| Inactive v2 key-control candidate | [Cloud PR #15](https://github.com/oojuro/cloud/pull/15), ready: independently current control prefix, separate writer/project DEKs, exact original outcomes, logical tombstones before terminal-scope destruction. **22 lifecycle regressions +1 actual-S3Backend/fake-transport/real-lexical composition story pass**. | Test key service is volatile/synthetic. Durable production scope-key/authentication/proof adapters, immutable key provisioning, recovery and irreversible destruction remain code work. Local timings/operation counts are not S3 benchmarks. |
| Explicit v2 App draft | [Cloud PR #16](https://github.com/oojuro/cloud/pull/16): actual API/browser and registered-human review, scoped redacted exports, current-prefix permissions, old-DB recovery and lifecycle transitions. Actual HTTP story and seven paused-body checks covered; root review added per-frame authorization after return. | Draft correction of API-host redirect, original typed retry binding before redaction and oversized provenance rendering is underway. Current exact-Mark guard conservatively aborts on unrelated ingest; transaction-bound grant identity and snapshot read permits remain commercial concurrency work. Default CLI does not select v2. |
| AWS adapter/IaC source in progress | Separate isolated branches implement an inert-constructor SecretsManager root loader (explicit fullARN/version/AWSCURRENT fetch, no local fallback or automatic election) and new storage/IAM/KMS/empty-root Terraform with no secret values. | Offline gates pending at this update. Real per-scope key provider, root transition/enrollment, practical v2 grant/account administration, billing reconciliation, runtime/CLI composition and production topology are still implementation tasks. No AWS API/resource operation occurred. |
| AWS provider acceptance | Prior adapter and composition use actual adapter code with fake transport; no live AWS operations were performed. | Independently current anti-rollback authority; provider key-service durability and irreversible destruction; IAM/obsolete binary exclusion; S3 semantics/faults; process/host/AZ drills, throughput/latency/cost and region DR evidence. |
| Complete specification | Section18 milestones below are the intended build order; the oracle remains the working product engine. | Full new format/log/chain, store/index/query differential port, erase/audit/retention, ranker integration, all edges and scale/security/launch gates remain incomplete. |

PR14 extends PR11/PR13/PR6 and is the current composed working-product review.
Its required live authority **format4** supersedes the credential-only format3
boundary described for historical PR13 above. Snapshot container format3 is a
separate version. Older live authorities need explicit trustworthy migration or
a fresh namespace; no missing review history becomes an empty confirmed ACL.

### Concrete next specification milestones

1. Finish the consolidated format/log and integrity-chain recovery, using the proven Store
   append boundary and preserving exact raw/parsed policy receipts.
2. Implement persistent native index segments/codec/merge and measure size/ingest
   at equal hardware. PR17 already supplies a bounded in-memory lexical/dedupe port
   with bitwise oracle fixtures; complete the declared differential corpus and
   persistence/crash/rebuild gates. Frozen PR16 retains its oracle bridge; PR17
   explicitly removes that bridge from its opt-in lane.
3. Implement native erase/retention and verifiable audit receipts, with old
   backup/key/cache recovery tests. Connect the already separate ranker seam
   only after the lexical gate and held-out adoption requirements pass.
4. Complete the remaining adapters, local protocol/CLI/MCP/daemon/forwarder and
   hosted wiring. A single explicit Codex adapter does not fulfill every edge.
5. Run the section14 correctness/performance/size/round-trip/audit gates and
   section18 10M/100M record, security and launch acceptance. Historic reference
   measurements are not measurements of the new candidate.

## Current blockers for primary agent storage

The initial cloud review found deletion resurrection from old archives, asynchronous remote key cleanup, bounded retry-key
retention with possible acknowledgement after retry-state failure, and mutable blob lost updates. PRs #2/#3/#4/#6 now implement
reviewable repairs with passing component/composed gates. PR #13 and product PR #11 add tested current-key revocation replay and ephemeral browser credential rollback protection. Hosted deletion freshness at startup and
conditional publisher updates are interim protection, not a continuously fenced replicated authority. An obsolete publisher
with broad overwrite/version-delete IAM can bypass new-script protections until the reviewed policy and credential boundary
are deployed and verified. Logical record/writer restore revocation does not cryptographically erase retained backup bytes
while their project DEK remains usable.

The commercial primary-store blockers include unfinished production key/auth-root
transition/provider/destruction code, practical grant/account enrollment, durable
billing reconciliation, runtime/CLI wiring and topology, and the remaining native
format/index/merge/erasure/audit/retention/adapters milestones. The S3 adapter and v2
App draft are concrete source progress, not a completed commercial primary store.
Excluding obsolete binaries/endpoints and legacy storage credentials, validating
real IAM/provider semantics, and independent process/host/AZ/region failure drills
remain separate deployment acceptance. Test-backend success substitutes for none
of those code or external evidence requirements. See cloud PR #1's
`docs/PRIMARY_STORAGE_REVIEW.md` for the original baseline and the follow-up PRs for current source/evidence.

## Executable gates delivered for review

Cloud PR #1 adds a synthetic real-process gate: a writer-restricted agent writes a transcript, explicit memory note and exact binary blob;
the process is killed with SIGKILL; reopening must preserve record ids/blob bytes, reject a forbidden writer and deduplicate retries.
An encrypted isolated snapshot restore must pass those checks again. The full cloud suite passes 86 tests; two opt-in load tests remain
ignored. The tenant-remapping and missing-control-database restore regressions both failed before their fixes. These are process/snapshot
correctness checks, not power-loss, regional disaster, deployment or SLA proof.

PR #2 passes eight ranker conformance tests and strict clippy; PR #3 passes six synthetic aggregation tests; PR #4 passes 27 store tests
and application/engine compilation. Full benchmark-workspace compilation is blocked by absent libclang/llvm-config, and optional encryption
performance is unmeasured. GitHub exposes no configured status checks for engine PRs #1 through #4; these results come from Rocket2 local runs.

## Review and migration sequence

1. Review the spec, sealing regressions, cloud safety fixes and standalone ranker seam as independent changes. Preserve
   lexical results with the ranker disabled; do not enable an experimental method from a promising smoke result.
2. Port the proven store/log/index/query implementation in milestones from SPEC.md section 18. Run synthetic lossless,
   recovery, erasure and isolation gates before importing sensitive data. No rewrite of current journals in place.
3. In a separately approved staging account/stack, demonstrate acknowledged write -> crash -> recovery and backup ->
   independent restore. Record storage sizes, billing estimates, concurrent latency, peak memory, RPO and RTO.
4. Add a Soft Axiom adapter behind an explicit flag: idempotent per-agent event ids, project/writer scope, immutable blobs,
   and provenance. Mirroring must not become a dependency of agent success. Preserve current primary stores and originals.
5. Reconcile counts, stable ids, text/blob digests, deletions and permissions over a defined shadow period. Report gaps
   instead of accepting a search hit as proof of durable storage. Verify retention and deletion on both sides.
6. Owner reviews evidence and authorizes any cutover. Switch bounded reads first; stop and revert reads on missing data,
   permission disagreement, durability failure or unacceptable latency. Keep a restore/export path throughout migration.

## Decisions needed before live hosting or cutover

- Confirmed: standalone product; agent transcripts/memory/files are the customer payloads. Other transactional state remains external.
- Confirmed target: acknowledged data survives process/host/AZ failure; region loss is separate. Provider/protocol implementation and independent acceptance evidence remain required.
- Live staging decisions remaining: region, spending ceiling, concrete availability/recovery-time objective and failure drill authorization.
- Raw privacy: local originals only, or an explicit encrypted raw-upload mode with key authority and erasure requirements.
- Launch identity/licensing and ids remain owner decisions; this work does not archive repositories or relicense code.

These do not block the code/spec review. They do block claims of a production primary store or an AWS cutover.
