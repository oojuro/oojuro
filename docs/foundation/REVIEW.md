# Foundation and Soft Axiom storage review

Review date: 2026-10-21 America/New_York. Reference engine: `3a17d1d`.
This is a review package, not a completed replacement engine, production readiness claim or cutover instruction.

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
| New consolidated engine | specification only; port and all section 14 gates still required |
| Information-theory methods | [PR #3](https://github.com/oojuro/oojuro/pull/3): reproducible aggregate of cached exploratory runs; exposed evaluation set, no clean adoption verdict |
| Hosted API | private `oojuro/cloud` service; existing EC2/EBS Terraform and encrypted S3 backup/restore scripts |

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

| Requirement | Present design / evidence | Gate before primary agent storage |
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
One Zone classes have a different failure boundary. These are service properties, not proof of the application's recovery behavior.

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
transaction. AWS provides [conditional creates and compare-and-swap writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html) through `If-None-Match` and `If-Match`. Applying them to the manifest protocol is a proposed design, not an implemented transaction. KMS authority, conflict handling and recovery still require implementation review and fault tests. This review did not query live AWS state.

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

- Primary scope: agent journal plus immutable payloads, or additional transactional state (which needs a separate design).
- Durability: EBS authority plus a declared nonzero disaster RPO, or S3-acknowledged commits (new protocol work).
- Reliability budget: target RPO/RTO, acceptable snapshot downtime, staging region and spending ceiling.
- Raw privacy: local originals only, or an explicit encrypted raw-upload mode with key authority and erasure requirements.
- Launch identity/licensing and ids remain owner decisions; this work does not archive repositories or relicense code.

These do not block the code/spec review. They do block claims of a production primary store or an AWS cutover.
