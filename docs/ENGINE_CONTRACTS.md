# Experimental engine contracts

> **Research record.** This document comes from the research that preceded the
> v1 product. Only the Nine (`crates/oojuro-nine`) is part of this repository; the
> other engines named below (Flash, Tensor, Cortex, Pulse, Loom, Orbit, Forge,
> Horizon) and the historical Nine v1 are not, and the commands and crates that
> mention them do not exist here. The common
> contracts are published because they define what any retrieval claim has to prove.

This document states intended contracts and adoption tests. It is not a declaration
that every engine implements every property below. Read source, feature flags, and
test or benchmark results for the implementation actually being evaluated. The
workspace is a set of Rust research prototypes, not a production replacement for
Redis, PostgreSQL, S3, or a distributed database.

The current Nine is a model-free compressed timeline with time/keyword indexes,
conditional query code length, a bounded cache and assessed outcomes. Its current
contract is in [the engine README](../crates/oojuro-nine/README.md).
Detailed vector/summary/PreparedContext discussion below records historical
Nine v1, which is not part of this repository, and does not describe the current API.
Auxiliary queue/provider crates support that historical experiment.

## The decision matrix

| Engine | Use this when | Provides or aims to provide | Closest alternative | What must be proven to justify a separate engine |
| --- | --- | --- | --- | --- |
| **Flash** | An agent repeatedly needs a few hot facts and local query latency matters. | A simple durable retrieval path with bounded results and low overhead. | An in-memory map or Redis, with an ordinary persistent store and a small index. | Lower end-to-end latency at comparable recall, persistence, memory, and update cost; report tail latency and durable-write cost. |
| **Tensor** | A workload has enough eligible vectors or enough simultaneous queries to use a local GPU effectively. | GPU vector scoring with temporal eligibility; Rust CubeCL is the intended local Intel Arc path. | A CPU scan, an existing vector index, or an established GPU vector implementation. | Measured GPU execution, useful recall, and an end-to-end throughput or latency gain after transfers, filtering, build cost, and memory use. CPU fallback must be identified. |
| **Cortex** | The bottleneck is selecting evidence for an ambiguous request within a small context budget. | A model inside the retrieval planner that selects existing citations and can propose explicitly unverified memory. Native Rust Candle supplies the intended optional small-model path. | Ordinary retrieval plus an application reranker or external model call. | A real model improves evidence selection or downstream task success under matched time and context budgets; include model loading, inference cost, invalid output, and fallback rates. |
| **Pulse** | Facts change, corrections arrive late, and uncertainty or historical knowledge matters. | Time-aware assertions, evidence lineage, and explicit uncertainty alongside retained history. | An event log and temporal tables in a conventional database. | Reliable historical answers without future evidence leakage, plus a measurable advantage in query simplicity, cost, or error rate; uncertainty must have defined meaning. |
| **Loom** | Recurring tasks repeatedly reconstruct much of the same context. | Compressed storage and derived predictive memory views that can be refreshed from originals. | Materialized views, document compression, a summary cache, and conventional retrieval. | Lower total storage, context, or preparation cost at a declared task-quality loss bound; include refresh cost and difficult changes to the underlying facts. |
| **Orbit** | Order, time, and records being useful together may carry retrieval signal. | Position and temporal rotations plus a co-use graph; entropy describes routing dispersion. | Metadata-aware vector retrieval, a graph index, or a sequence-aware reranker. | Reproducible retrieval gains over each simpler component and their combination, including cold starts, changed workflows, and feedback bias. |
| **Forge** | An expensive computation or artifact may be reusable and its acceptance conditions can be checked. | Artifacts indexed by inputs and dependencies, with retained verification evidence and reuse decisions. | A content-addressed build cache or artifact store plus a dependency manifest. | More valid reuse or less verification work without accepting stale or incompatible artifacts; measure false reuse, invalidation, and the cost of checking eligibility. |
| **Horizon** | Decisions need versioned probabilities and later evaluation against outcomes. | Beliefs and forecast distributions with provenance, revision history, and outcome resolution. | A forecast table, model registry, and conventional analytics. | Better forecast management or decision results at acceptable cost; separately demonstrate calibration and proper scores on outcomes unavailable when forecasts were recorded. |
| **The Nine** | An agent needs fast local text history without a model provider. | Compressed exact originals, time/keyword indexes, conditional query code length, a bounded cache and assessed usefulness. | SQLite FTS5, PostgreSQL full-text search, ordinary hybrid retrieval, with equivalent caching and durability. | Complete-request speed and resource gains at matched evidence quality; verify actual token use and agent outcomes separately. |

The alternatives are baselines to test, not products claimed to be inferior. A
better library or database extension can satisfy a successful experiment's purpose.

## Common evidence contract

The shared interface carries `Record` values with immutable identifiers, a logical
key, text, `valid_at`, `recorded_at`, an optional embedding, a verification flag,
and metadata. Queries carry `valid_at`, `known_at`, a result-count limit, and a
text-byte budget. Search results carry original records, scores, optional
uncertainty values, routing entropy, and notes. Nine can return an explicitly
unverified derived summary; its exact source links and separate expansion API
distinguish that text from an original. Generic search must not erase that status.

The current shared helper selects the latest eligible record for each key from
records whose `valid_at <= query.valid_at` and `recorded_at <= query.known_at`.
It orders versions by valid time, recorded time, then insertion position. This is
a concrete point-in-time visibility rule; two timestamp fields alone do not
establish full bitemporal intervals, deletion semantics, branching histories, or
transaction isolation. An engine claiming stronger temporal behavior must define
and test it explicitly.

The default vector is a deterministic lexical projection with 3,072 dimensions.
It is a fixture mechanism, not a pretrained semantic encoder. Supplied embeddings
must contain 3,072 finite values. A semantic retrieval claim needs a named real
encoder, representative data, and a measured comparison.

The common packer limits the number of hits and the sum of returned text bytes.
That budget is not a token limit, a total serialized-response limit, or a limit on
all database and model work. Engine-specific model or computation budgets must be
reported separately.

`SearchResult::context_packet` separately budgets a serialized evidence array and
omits embeddings and unrestricted metadata. The Nine's `PreparedContext` is the
interface for its richer summary/raw-link contract; the generic core packet is
not a replacement for that provenance representation.

## Boundaries that must survive a smarter planner

- **Originals remain distinguishable from derivations.** Retain the source and
  derivation lineage needed to inspect or refresh a summary, forecast, or model
  proposal. A generated proposal must not silently become an observed fact.
- **Verification is evidence with a scope.** A boolean supplied by a caller does
  not prove truth. Identify the check, inputs, environment, dependencies, result,
  and conditions under which it remains applicable.
- **Time and access filtering precede model selection.** A model must not see
  records outside the effective scope or knowledge cutoff. The surrounding system
  must authenticate the caller and supply that scope. The generic `Engine` trait
  does not establish authorization.
- **Scores carry a meaning.** Label similarity, ordinal rank, estimated
  uncertainty, and forecast probability separately. None can be substituted for
  another without a defined model and validation.
- **Failure and fallback are visible.** Report an unavailable GPU, missing model,
  rejected generated output, stale view, or failed verification. Do not label a
  fixture or CPU fallback as successful native model or GPU execution.
- **A stored answer grants no authority.** Executing tools, approving spending,
  retrying external actions, and deciding whether a result meets the user's intent
  remain harness responsibilities.

These are required design boundaries for adoption. They are not a claim that a
particular prototype currently enforces every boundary.

## Scope and multi-agent integration

Use distinct namespaces for private agent state and shared project state. Local
directory namespaces and validated names prevent some accidental mixing; they are
not production identity, authorization, or a distributed-consistency protocol.
Shared use by several agents also does not imply that the current single-writer
journal supports concurrent distributed writers.

Promotion from private state to shared evidence must be explicit and attributable.
Specify which source records and derived artifacts are promoted, who authorized
the promotion, and the destination scope. A memory proposal, uncertain assertion,
or forecast retains that status after promotion. Sharing does not verify it.

Apply scope rules to indexes, co-use graphs, compressed views, model inputs,
caches, and training data as well as original records. Do not pool private data in
a shared model or compression dictionary merely because returned record IDs are
filtered. Cross-agent reuse in Forge additionally requires compatible dependency
and verification conditions; Horizon needs separate private branches and explicit
publication of shared forecasts.

The Nine defaults to one private logical scope per agent. Shared project scopes
are optional and require authorization by the data's owner or an explicit policy. Scope
eligibility applies to every episode edge, candidate page, result cache, feedback
event, and compression-derived view. An agent's participation in a project does
not authorize reading or promoting every other agent's private state.

## Historical Nine v1 API and integration contract

This is an attachable sidecar/library contract for an existing harness, PostgreSQL
application, or hosting arrangement. It does not transfer action authority or
workflow execution durability to the database. Harnesses and workflow systems such
as Temporal retain scheduling, retries, execution, and external outcome checks.
External synchronization and production authorization require their own designs.

1. **Keep the query simple.** `prepare_default(&Query)` accepts natural language or
   a supplied vector. `prepare(&Query, &PrepareOptions)` adds retrieval/packing
   budgets and encoder identity, not event-type filters. Episodes, parent IDs, and
   tool-call links are optional ingestion metadata. A declared causal link is
   provenance about a reported sequence, not proof of causation.
2. **Index selectively.** `Direct` indexes selected text and vectors. `RawOnly`
   and `NeedsSummary` retain exact originals without lexical, vector, corpus-statistic,
   or fallback participation. Tool results default to raw-only. A host explicitly
   selects summary work; queries do not generate summaries or train a model.
3. **Keep summaries attributable.** `append_summary` checks whole-source digests,
   UTF-8 source ranges, summary identities, and recorded vector/token metadata.
   Generated text remains unverified. Search returns summary text plus raw links;
   `expand_source_range` or `expand_summary` deliberately expands originals. Both
   clocks and source-version eligibility govern summary retrieval, and a newer
   eligible source invalidates its old summary.
4. **Bound retrieval and packing separately.** Historical BM25 and compatible
   vector ranks use reciprocal-rank fusion. The default candidate cap is 64 (maximum
   256), default vector-scoring cap 4,096, default prompt limit 16,384 bytes, and
   default context-record cap eight. Bounded vector probing is approximate below
   the eligible vector count. The Nine exposes a capped pool rather than cursor
   paging; StreamLoom has a separate passage/snapshot API. Candidate caps do not
   bound all scanning, replay, memory, or I/O.
5. **Preserve feedback authority.** Only eligible caller-labeled verified outcomes
   with a matching query digest and explicit per-source usefulness assessments
   adjust contextual rank. Reactions and unverified outcomes do not change rank.
   Negative outcomes remain inspectable; no feedback rewrites source text,
   verification flags, or LLM weights. Future feedback cannot change earlier views.
6. **Check reuse and corrections externally.** The initial integration goal is
   evidence and dependency-checked tool-result reuse, with final-answer caching
   excluded. The Nine currently has no response cache; Forge or the host supplies
   stronger artifact/dependency checks. A repeated-mistake intervention needs
   explicit failure preconditions and a linked verified correction whose current
   applicability the harness checks. A prior result never authorizes a side effect.

Entropy of the capped score distribution may trigger bounded expansion through
eligible indexed episode neighbors. The greedy context objective is actual
LZ4-with-size-prefix compressed JSON bytes plus a byte-equivalent penalty for
uncovered weighted features, subject to the uncompressed prompt-byte and record
limits. It may select an empty packet. The prompt itself remains uncompressed JSON.
These measured compression/coverage terms are surrogates, not semantic information,
optimal MDL coding, token costs, truth probabilities, or correctness certificates.

Supplied vectors require 3,072 finite values and compatible encoder identities.
Mixed known encoders suppress ambiguous dense routing unless one is selected;
lexical fixtures are not compared to supplied pretrained vectors. Summary vectors
require exactly one positive recorded token measure no greater than 8,172: an
exact validated count or a separately labeled conservative upper bound. Storage
does not implement a provider tokenizer. The optional Gemini client prefers exact
token counting and labels its conservative UTF-8-byte fallback separately. It
disables truncation and validates output dimensions before publication.

The native frame cap is 32 MiB including the event/summary envelope. All originals,
indexes, feedback and activity replay into memory; source retention alone does not
establish bounded-RAM operation. Larger documents require explicit splitting or
StreamLoom. There is no HTTP/MCP server or TypeScript/Python SDK in this workspace.

### Background and provider boundary

`oojuro-jobs` persists metadata-only jobs, digest identities, fenced leases,
bounded caller-driven retries, and supersession. Nothing runs automatically.
The lab's `ConsolidationPolicy` defaults to summary triggers at 10 user turns,
64 KiB pending source bytes, or 120 seconds. All thresholds are configurable in
Rust; the agent can trigger earlier work. Direct sources have an embedding-only
path and raw-only sources stay excluded even under force. Existing jobs do not
consume the per-call new-job cap.

The explicit worker calls GLM 5.3 Flash only for selected summaries, then optional
Gemini Embedding 2 for vectors; direct embedding needs no generative LLM. It checks
source versions before and after computation. Queue completion and external
publication are not one atomic transaction; leases do not prove exactly-once work.

The provider clients require one persisted shared guard capped at $5 for the
authorized experiment, reserve before sending, retain uncertain reservations, and
redact secrets in diagnostic output. No automatic paid retry occurs inside the
clients; a failed worker attempt can record a bounded retry for another caller
invocation. Separate ledgers are independent, and local reservations are not a
provider invoice or account-wide billing cap. Offline tests do not prove live
model availability or task quality. The provider and queue contracts are not part of this repository.

The initial gate is a realistic workload benchmark against the unchanged harness
and ordinary storage, plus the smaller-engine composition where applicable. Keep
model, tasks, candidate/context budgets, scopes, time cutoffs, and verification
requirements comparable. Measure total latency and cost, cited-evidence coverage,
valid versus stale reuse, and repeated failures under matched preconditions. Test
held-out episodes and dependency changes; do not let later outcomes teach earlier
queries. No repeated-mistake reduction or general accuracy improvement has yet
been demonstrated.

## Specific limits to make explicit

**Flash:** low latency is an empirical property. Report corpus size, query mix,
hardware, concurrency, durability settings, recall, and p50/p95/p99 latency. A
memory-only read comparison cannot establish the cost of durable insertion.

**Tensor:** eligibility must be identical on CPU and GPU. Compare returned records
as well as numeric tolerances, and identify the actual device used. A GPU kernel
or feature compiling does not demonstrate hardware execution or a speedup.

**Cortex:** enforce candidate membership, time, scope, and output limits outside
the model. Preserve exact cited originals; generated memory remains unverified.
Candle availability, weight loading, model execution, accepted output, and useful
selection are separate milestones. A deterministic fixture can test enforcement
but cannot establish inference quality. Candidate filtering can omit needed
evidence before the model runs, which must be included in evaluation.

**Pulse:** distinguish uncertainty about when an event happened, uncertainty about
whether an assertion is true, and uncertainty caused by missing evidence. State
how contradictions, corrections, and expiry behave. A score or entropy value is
not a probability of truth without an explicit calibrated model.

**Loom:** distinguish lossless byte compression from a lossy semantic view.
Lossless decompression can recover exact originals; a compact summary can omit
task-critical details. Keep views attributable, versioned, and refreshable, and
measure downstream errors as well as saved bytes or tokens.

**Orbit:** state the rotation transform and the meaning of position and elapsed
time. Check for aliasing and excessive sensitivity to small time changes. Co-use
is an observed correlation, not proof of usefulness or causality. Evaluate without
using future outcomes to construct the graph for earlier queries.

**Forge:** reuse is valid only within the recorded verification scope. Match the
inputs and relevant dependencies; define invalidation when code, environment,
policy, or source data changes. Artifact identity proves byte identity, not that
the artifact satisfies the current request. Retain failed checks as evidence too.

**Horizon:** identify the event, outcome space, time horizon, forecast as-of time,
source or model, and resolution rule. A revision must preserve the earlier belief.
Use time-separated evaluation with proper scores such as Brier or log score where
appropriate, calibration checks, and a simple baseline. Retrieval accuracy and a
forecast being durably stored do not demonstrate predictive skill.

## Adoption evidence

For each proposed benefit, retain a reproducible workload, an equally constrained
baseline, correctness and quality results, full cost measurements, and known
failure cases. Report sample size and uncertainty rather than universal speed or
future-performance claims. Separate unit tests, synthetic benchmarks, hardware
execution, and representative application trials in the results.

The shared journal uses an exclusive writer lock, framed entries, checksums, and
sync calls; its recovery path handles a partial final frame and rejects checksum
corruption. These mechanisms are a starting point for recovery tests. They do not
establish multi-record transactions, replication, distributed consensus, online
backup consistency, fault tolerance, or production durability under every failure.
State the tested failure model before claiming those guarantees.

Advance an engine when its distinct benefit is demonstrated. Merge or stop it
when a simpler component provides the same result. The desired outcome is a useful
evidence system for agents, not maintaining a fixed count of engines.
