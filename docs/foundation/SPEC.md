# Oojuro engine specification (review draft 0.3, 2026-10-22)

Status: **draft for the owner's review; no launch or migration approval.** This specifies a proposed foundation.
The sealed raw journal, explicit file adapter and atomic Codex capture candidate are bounded review slices; the complete consolidated engine and hosted
primary-store contract remain unfinished. See REVIEW.md for current code and verification. The old engine (`oojuro/oojuro` at `3a17d1d`, called "the oracle" below) is the working reference
implementation and the source of every measured number quoted here.

How to read it. Statements marked **[exists]** describe behaviour the oracle already has and the new engine must keep.
Statements marked **[new]** describe the proposed contract rather than an acceptance claim; some now have bounded review
implementations listed in REVIEW.md, while the complete gates remain unfinished. **[open]** marks a
decision still to be taken (section 17). Numbers carry their source; none is a promise about the new engine until section 14
is passed.

Companions: [schematic](schematic.svg), [review and AWS migration gates](REVIEW.md).
`[exists]` means present or documented in the reference implementation; it is not a fresh test result.
All historical measurements below retain their original corpus/hardware scope. No benchmark was rerun for this document.
The format, wire API, adapter and ranker headings describe *proposed launch freezes*, not a current compatibility guarantee.

## Contents

1. Purpose and principles
2. Data model
3. Architecture
4. Storage and on-disk format (frozen at launch)
5. Write path
6. Read path
7. The ranker slot (frozen at launch)
8. Erasure and retention
9. Audit mode
10. Processes, concurrency, deployment shapes
11. Adapters (frozen at launch)
12. Wire API and local protocol (frozen at launch)
13. Versioning, compatibility, the freeze
14. Acceptance gates
15. What is ported, what is redesigned
16. Non-goals
17. Open decisions
18. Build plan

---

## 1. Purpose and principles

Oojuro is **the flight recorder for AI agents**: a lossless, compact, searchable, erasable journal that attaches to agents
(Claude Code, Codex, Temporal workers, any harness with a log, a proxy hook or OpenTelemetry) and records what they saw,
thought and did, so that other agents (and the same agent days later) and people can read it back. It is not a memory
product and not a message queue. Searching is one of its features.

Principles, each of which is a test in section 14:

1. **Lossless.** What the agent's harness wrote is kept exactly. The raw lines are the master copy; everything else
   (parsed records, indexes, columns, derived tables) is **derived and rebuildable** from it. Originals are never
   deleted automatically; only an explicit, verified prune removes them.
2. **Simple and horizontal.** A record is flat. Structure (trees, sessions, periods, anything analytic) is a query-time
   function over a few stored fields. Analysis never costs the write path: a feature that slows ingest or search is removed
   (the rule in the oracle's `DESIGN.md`). **[exists]**
3. **Erasable by construction.** Erasure is a first-class operation with a proof, not a delete flag. The unit of erasure is
   the **writer** (section 2). The default mechanism is destroying the writer's key. **[new]**
4. **Tamper-evident on request.** An optional audit mode makes the history append-only in a way a third party can verify,
   including that a deletion happened. **[new]**
5. **No model inside the database.** No embedding model, no per-write model call. Any smarter ranking is an optional
   ranker over derived tables (section 7).
6. **Deterministic reads.** The same store and the same query give the same ranked list, bit for bit, on every run and every
   machine (one tie rule). **[exists]**
7. **Local first, hosted when wanted.** The same engine runs embedded in a process, as a per-machine daemon, and as the
   multi-tenant hosted service. Private data stays on the machine unless forwarding is switched on. **[exists/new]**

## 2. Data model

### 2.1 Record **[exists, simplified]**

A record is one thing that happened. Stored fields (frozen):

| Field | Meaning |
|---|---|
| `writer` | memory owner and erasure/retention unit; a name of at most 256 bytes. Default `default`. Hosted: usually one per end user. |
| `agent_id` | one running agent instance (a session, a subagent). Says who. Optional parent `parent_id` names the instance that spawned it. |
| `valid_at` | when it happened (microseconds, UTC). |
| `recorded_at` | when the journal stored it. Monotonic per writer. |
| `kind` | `input`, `output`, `thinking`, `tool_call`, `tool_result`, `system`, `explicit`, `raw`. |
| `tool`, `status`, `exit` | typed fields inferred by the adapter or engine (tool name, ok/error, exit code); nullable. |
| `key` | optional; versions records so that the newest live version wins in search (`remember --key`). |
| `text` | UTF-8, up to 16 MiB per record through `remember`, 32 MiB frames; larger inputs are split by the capture layer. |
| `attrs` | small free-form map (at most 4 KiB), stored, not indexed. |
| `id` | stable record id, never reused (format in section 17, D1). |

Removed from the oracle's record: the 3,072-number `embedding`, the `verified` flag, the fixture `lexical_embedding`, and the
separate "metadata" string map that carried typed fields. They belong to the old memory-database design.

Structure is **inferred from text** where it can be (tool, status, exit). No user-defined schema. No SQL.

### 2.2 The two layers

| Layer | Content | Master? | Rebuildable | Backed up |
|---|---|---|---|---|
| **Raw** | every line the harness wrote, byte exact, in blocks (about 256 KB) per source file; `status` flags for no-newline tail and non-UTF-8 lines | yes | no | yes |
| **Parsed** | records as above, deduplicated, with the redacted text | no | from raw only where retained | yes |
| Index, columns, derived tables | postings, time/kind/tool columns, ranker tables | no | from parsed | **never** |

The lossless prototype round-tripped 2,024 of 2,024 files and 5,570,924,950 of 5,570,924,950 bytes exactly
(`LOSSLESS_EXPERIMENT.md`, branch `lossless-exp`). Raw blocks reach 14.5 % of the original JSONL; raw plus parsed 19.8 %.

Where the harness is `proxy` or `otel` (no file), the raw layer holds the request/response bytes the proxy saw, or the OTLP
message, in the same block format with a source tag.

Secrets: the proposed raw copy contains every secret the agent saw, so it is **encrypted at rest** (section 4.3) and **never sent to
the cloud**; the parsed layer holds redacted text (known secret formats and high-entropy tokens are masked before storing)
**[existing redaction; raw sealing is prototype work]**. The cloud receives only the redacted copy.
Consequently hosted redacted storage cannot regenerate the original harness files. Raw originals remain local until a separately
approved encrypted raw-upload design exists. Redaction is defense in depth and is not proof that arbitrary secrets were removed.

### 2.3 Writers, agents, projects

* **Project** = one journal = one trust boundary = one hosted API key space. Nothing in a request can reach another project.
* **Writer** = erasure and retention unit inside a project. Many agents write into one writer. Dedupe and dictionaries never
  cross writers.
* **Agent** = a running instance; query-time causal trees are proposed. Current hosted parent/source/tool links are
  recorded caller claims, not a verified causal graph; scoped keys enforce only their configured agent identity.
* **Human reviewer** = another registered account explicitly granted whole-project read-only trace access by the owner.
  Timeline, search, detail and export share the same redacted authorized records. No anonymous/public shares or credentials
  in share URLs; mutation, key, retention, billing and grant management remain owner-only. Grants and revocations belong in
  independently current authority and must survive old account-database restore. The working App implementation and strict
  authority-format migration are in [cloud PR #14](https://github.com/oojuro/cloud/pull/14); inactive v2 integration remains a gate.
* Reads that name no writer look at every writer the key may read.

Limits at launch (hosted): 10 projects per account, 20 keys per project, an account is one person; the current review slice allows at most 20 active human reviewers per project.

## 3. Architecture

```
 harness logs / proxy / OTel / SDK / CLI
                 |
          [ Adapters ]  (section 11)        frozen interface
                 |    redaction, identity, exclusion, resume cursors
          [ Ingest ] -> raw blocks + parsed records
                 |
   +-------------+--------------------------------------------+
   |  ENGINE (one crate: log, store, index, query, rank, erase, audit)
   |                                                          |
   |  Log     group-commit journal (shared or per-writer), AEAD frames, hash chain
   |  Store   frames, zstd + dictionary, CDC dedupe, tiers, raw blocks
   |  Index   segments (postings, rows, columns), merge, derived tables
   |  Query   plan -> lexical candidates -> RANKER SLOT -> pack -> excerpts
   |  Erase   key destruction, segment drop, rewrite fallback, proofs
   |  Audit   chain, checkpoints, signed receipts, verify/export
   +-------------+--------------------------------------------+
                 |
   [ Surfaces ]  library | CLI | MCP | local socket | HTTP (hosted)    frozen wire API
```

The picture with the data flow, the key hierarchy and the ranker stages is `schematic.svg`.

Crate layout of the new public repository (one engine crate, thin edges):

| Crate | Contents |
|---|---|
| `oojuro-engine` | modules `format`, `log`, `store`, `index`, `query`, `rank` (the slot and `lexical`), `erase`, `audit`. No I/O policy outside `log` and `store`. |
| `oojuro-capture` | adapter SDK, built-in adapters (Claude Code, Codex, pi, Temporal, OTel), redaction, resume state |
| `oojuro-proxy` | recording proxy for Anthropic- and OpenAI-compatible APIs |
| `oojuro` | library facade, CLI, MCP server, `oojurod` daemon, settings, `periods`, `--when` |
| `oojuro-ranker-*` | optional rankers (one crate each, separately deletable) |
| `oojuro-bench` | TraceBench, turn-loop, retrieval evals, differential tests against the oracle |

The hosted service (`oojuro/cloud`, private) depends on the public engine by pinned commit and adds tenancy, keys, billing,
retention scheduling, email and the dashboard. Nothing private lives in the public repo.

## 4. Storage and on-disk format (frozen at launch)

### 4.1 Directory **[new layout, ported mechanisms]**

```
<store>/
  FORMAT            "oojuro-store-1" and the minimum reader version
  LOCK              exclusive owner
  manifest          atomically replaced: writers, segment lists, chain heads, key-wrap ids
  keys/             wrapped writer keys (one file per writer), project key wrap
  w/<writer>/       one directory per writer (private-log mode)
    log-<gen>       journal generations (CRC frames, AEAD-sealed)
    seg-<n>.idx     index segments (derived, never backed up)
    seg-<n>.src     parsed originals in compressed runs (sealed)
    raw-<n>         raw blocks (sealed)
    chain           hash-chain records and checkpoints
  shared-log/       optional: one preallocated journal for many writers (local default)
  derived/<ranker>/ per-writer derived tables (never backed up)
  tombstones        durable deletions replayed on open
```

### 4.2 Frames and the journal **[exists, ported]**

* A **frame** is the unit of compression and integrity: `len u32 | crc u32 | payload`, payload = `codec | dict | first_seq |
  count | raw_len | meta_len | [digest] | packed body`, body = per-record meta then texts. Records of a frame have
  consecutive sequence numbers. The meta section comes first so an excerpt decodes only `meta_len + end` bytes. Codecs: raw,
  lz4, zstd. Cold frames carry a 16-byte BLAKE3 digest.
* **Dedupe** is chunk level (content-defined, about 1 KiB) plus record level (same text, tool and args within a writer; the
  key excludes source and run id, which dedupes 11.9 % of real records). A reference points at most one hop back.
* **Journal** frames: header 24 bytes with salted CRC, batches span frames flagged MORE, one `sync_data` per batch,
  leader/follower group commit. **Recovery rule:** at the first bad frame search the rest of the file for any later valid
  frame; if one exists the damage is in committed history and opening **fails without modifying the file**; otherwise the
  tail is torn and is truncated. A corrupted length must never discard a later committed frame (fault-injection gate).
* **Index segments** (version 3): header 192 B, rows packed in blocks of 128, 96-bit id fingerprints, Rice-coded postings
  with gamma term frequencies, front-coded dictionary in blocks of 32, CRC32 footer, written by a streaming writer so a
  multi-gigabyte merge holds no whole segment in memory. Version 3 cut the index 31.5 % on tier 1 and 24.6 % on a real
  sample.
* **Tiers and compaction**: hot tier fast level with the starter dictionary; merged runs and the archive tier use a stronger
  level and a dictionary trained on the run. 64 KiB frames for hot reads.

### 4.3 Sealing and the key hierarchy **[new; prototype in `lossless-exp` 00deb56]**

```
master key  (KMS in the cloud; a 0600 key file or the OS keychain locally)
   wraps  project key
             wraps  writer key (random 256 bits, one per writer)
                       seals  every frame, dictionary and raw block of that writer
```

* Cipher: XChaCha20-Poly1305 over the packed body, nonce 192 random bits, associated data = (format version, writer id,
  frame first_seq, codec). **Compress first, then encrypt**, so the ratio is kept.
* The writer key is wrapped by the project key and stored in `keys/`. **Destroying every recoverable copy of the writer key makes sealed content unreadable.** Deleting a live key-wrap file is
  insufficient when a backup contains that wrap and its wrapping key is still usable. The implementation must keep writer-key
  authority outside data backups, revoke/delete all recoverable wraps, clear caches, and demonstrate that an old backup cannot
  unwrap the key. A shared KMS master key alone does not provide per-writer revocation. This is a required gate, not a current guarantee.
* Derived layers (index, columns, ranker tables) hold terms in clear because they must be searchable. They are **local to the
  machine that serves queries, never backed up, never shipped, and deleted when their writer is erased**. Anything that can be
  rebuilt from sealed frames is rebuilt, not restored.
* Honest limit: freed blocks of deleted derived files can remain on a disk until reused. Hosted volumes are encrypted with a
  KMS key; the user's machine inherits whatever disk encryption it has. This is stated in the privacy documentation, not
  hidden.
* The recorder's local store seals the raw layer by default. Sealing the parsed layer locally is a setting.

### 4.4 Hash chain **[new]**

Per writer, each sealed frame appends a chain record `h_n = BLAKE3(h_(n-1) || frame_digest_n || seq_n || valid-time bounds)`
over the **ciphertext digest**, so the chain stays verifiable after the key is destroyed. See section 9.

## 5. Write path

1. **Capture** reads a harness source (tail a file, hook event, proxy exchange, OTLP span, SDK call) and produces raw bytes
   plus records with writer, agent and parent ids, kind and typed fields. Adapters apply `exclude` globs and the `record`
   switch before anything is stored; unreadable settings **fail closed** (the writers they cover stop recording). **[exists]**
2. **Redaction** masks known secret formats and high-entropy tokens in the parsed text before it is stored or forwarded. The
   forwarder redacts again before sending. Known gap in the oracle: a token directly after a JSON-escaped newline in a tool
   call is not masked locally (the cloud copy is clean); the new redactor must close it and the conformance tests include
   it.
3. **Append.** Records and the raw block land in the writer's journal as one atomic batch; the call returns after one
   `sync_data` shared by concurrent writers. The record is searchable before the call returns (read-your-writes). The write
   path does no analysis beyond tokenising for the postings.
4. **Dedupe and compaction** run in the background; flushes and merges never block appends.
5. **Idempotency [new].** Every write carries an optional idempotency key per writer (hosted API and forwarder) so a retry
   never duplicates; the window is per writer and persisted, large enough for a bulk import (the oracle's window of about 260 k
   keys is too small for a whole-journal import and is raised).
6. **Local default**: one shared preallocated log under per-writer stores (privacy and erasure stay per writer); **hosted
   default**: private per-writer logs. Measured on the oracle: erasing one of 2,000 writers 14 s shared vs 0.1 s private;
   ingest about 13 % slower private (`SHARED_LOG.md`).

Order guarantees: per writer, records are totally ordered by `recorded_at` and sequence; across writers no order is promised.

## 6. Read path

A search is the pipeline below. Every stage is deterministic. **[exists]** unless marked.

1. **Parse.** The query is a bag of normalised terms with integer counts; quoted phrases and identifiers are literals
   (`foo_bar` is tried whole and falls back to its parts only when the view holds no occurrence). Normalisation is the
   English Snowball 1.2.0 stemmer with a stop list. Queries up to 64 KiB by default (a setting); `max_query_terms` (off by
   default) keeps the rarest N words.
2. **Visibility.** Time bounds, kind/tool/writer filters, as-of view, key versions (newest live version wins), tombstones.
3. **Candidates.** Postings of the query terms in every visible segment; segments are scanned in parallel on
   `query_threads`.
4. **Lexical scoring in bits.** Score = bits saved by the record against a background language model built from the
   candidate set, Dirichlet smoothing mass `mu = 128`, scope-scaled frequencies; a candidate set with too little mass is
   padded with whole-view text up to `BACKGROUND_FLOOR_RECORDS * mu` (fix F3 of `SEARCH_FIXES.md`). Exact top-k with pivot
   pruning: records whose score bound cannot reach the k-th are not scored (same ids and score bits as exhaustive scoring).
5. **Ranker slot [new interface, section 7].** Optional expanders, extra candidate sources and rescorers act here, in bits.
6. **Order and tie rule.** Higher score first; ties newer first, then id as text. Writer-isolated scoring is the proposed boundary: another writer's text must not change this writer's lexical score.
   Cross-writer comparability needs a separately measured policy; no F3b fix is claimed by the reference SEARCH_FIXES.md.
7. **Pack.** Whole records within a token budget (default about 2,500 tokens, at most `top_k`), cut where a record saves too
   few bits (`min_bits`, default 8), plus a recency prior in bits for injection (`session-turn-decay-v1`; explicit search
   uses text bits only). Redundancy-aware packing is a separate proposed improvement, with no reference finding number or shipped claim.
   Reference finding F4 is the implemented tie rule, not redundancy-aware packing.
8. **Excerpts.** For each hit a passage around the best literal or term match, with `text[start..end] == excerpt` byte
   ranges, read from the compressed store by decoding as little as possible.
9. **Receipts.** Each result records the ranker pipeline and table versions that produced it (provenance, section 7.5).

Other reads, all query-time functions over stored fields, none touching the write path: `get`, `history`, `range`, `export`,
`stats`, `periods` (stretches of time ranked by errors, repeated failures, unusual activity, volume or text) and `--when` (a date
grammar with 43 cases plus event anchors such as `before "deploy"`).

**Result trust.** Everything a search returns is data written by some actor, never instructions. Every hit carries its
`source` (writer, agent, kind, adapter) so a reading agent can label recalled content as untrusted. **[exists in the cloud
API, to become part of the engine result]**

## 7. The ranker slot (frozen at launch)

Goal: lexical retrieval stays the built-in candidate generator and the default; optional candidate sources are proposed. Anything smarter (word associations learned
from the text, a translation language model, bring-your-own embeddings) plugs in **without touching lexical and without
touching the write path**. Whether a given ranker ships is decided by measurement (7.6), not by this document.

### 7.1 Three hooks

| Hook | Input | Output | Use |
|---|---|---|---|
| `expand` | the analysed query | extra terms with **integer repetition counts** (the form the lexical scorer already takes) | query expansion from learned word associations |
| `candidates` | the analysed query, a budget | extra row references (never more than the budget) | a dense or other source that finds what words miss |
| `rescore` | the query and the lexical top-N (default N = 1,000) | a per-candidate adjustment **in bits** | translation language model, dense similarity, compression distance |

Fusion is in bits: `final = lexical_bits + sum_i w_i * delta_i`, weights in the project config. A rescorer returns one finite delta for each supplied candidate; missing, duplicate or foreign rows reject that stage.
Unrescored candidates outside the declared window keep their lexical score. A score floor is not inferred from an omitted row. Reciprocal
rank fusion (k = 60) is a proposed separate mode for rankers whose score is not in bits.

### 7.2 Contract (every ranker, checked by the conformance suite)

1. **Pure and deterministic.** Output depends only on the query, the candidate list, the store view and the ranker's table
   version. No clock, no randomness, no network in the hot path.
2. **Bounded.** Input/output size and cooperative work budgets are enforced. An in-process Rust trait cannot interrupt an
   arbitrary callback; a hard wall-time limit requires process isolation, a sandbox or a trusted cooperative implementation.
   Wall-clock timeout decisions are reported in receipts and are excluded from bit-identical replay claims.
3. **Failure never fails the query.** An error, a missing table or a timeout means the ranker is skipped; the result says so
   in its notes and receipt. A search with every ranker off must equal the oracle's lexical result exactly; this remains an integration gate.
4. **No write-path cost.** A ranker never runs at ingest. Its tables are built by a background job from the journal view and
   are **derived and rebuildable**: deleting `derived/<ranker>/` loses nothing.
5. **Erasure-aware.** A table learned from a writer's text is per writer and is destroyed with the writer (section 8). A
   table learned from public text only (a "starter table") may be shared, and its training data is documented. **No ranker is
   ever trained on one customer's data and used for another's.**
6. **Versioned.** `id/version` for the ranker and for its table format; tables from an older version are rebuilt, not read.
7. **Switchable.** Off by default unless it passed 7.6. Per project and per writer: `[rank] pipeline = ["lexical",
   "assoc@1:w=1.0"]`.
8. **Separately deletable.** Each lives in its own crate and module and can be removed in one commit.

### 7.3 Interface sketch (Rust)

```rust
pub trait Ranker: Send + Sync {
    fn id(&self) -> RankerId;                       // "it.assoc/1"
    fn table(&self) -> Option<TableVersion>;        // None while no table is built
    fn expand(&self, q: &Analyzed, cx: &Cx) -> Option<Expansion> { None }
    fn candidates(&self, q: &Analyzed, cx: &Cx, budget: usize) -> Option<Vec<RowRef>> { None }
    fn rescore(&self, q: &Analyzed, top: &[Candidate], cx: &Cx) -> Option<Vec<Bits>> { None }
}
pub trait DerivedTable {
    fn build(&self, view: &JournalView, out: &mut TableWriter) -> Result<()>;   // background job
    fn update(&self, delta: &JournalView, t: &mut Table) -> Result<()>;         // optional incremental
    fn erase(&self, writer: &WriterId) -> Result<()>;                           // must leave nothing
}
```

`Cx` exposes read-only views: term statistics, row metadata, passage text for a row. A ranker cannot write to the store.

### 7.4 Planned rankers

| Id | Kind | State |
|---|---|---|
| `lexical` | the built-in scorer (section 6) | ships |
| `it.assoc` | `expand`: word associations (positive PMI over messages, truncated SVD, nearest vocabulary neighbours) | measured in D18 to D24; ships only if section 7.6 is passed |
| `it.translate` | `rescore`: translation language model `P(query word | message word)` fused in bits | same |
| `it.ncd` | `rescore`: conditional compression cost of a candidate given the query | same |
| `byo.embed` | `candidates` and `rescore`: the customer supplies vectors or their own embedding key; the engine stores no vectors unless asked | optional, after launch |

Evidence so far (internal, 90 labelled questions, evidence recall in an 8,000-token packed context): plain Oojuro 0.630,
Qwen3-Embedding-8B 0.781. The information-theory experiment (`LONGCTX_EVAL.md` D18 to D24, separate longctx branch) documented its
decision rule and later amendments. Amendment 2 discloses an evaluation-set smoke run viewed before further grid changes;
its scores are exploratory and require a fresh held-out confirmation. It has no verified final adoption verdict in this review; this slot exists so that its outcome, either way, changes a config line and not
the engine.

### 7.5 Provenance

Every result carries the ordered list of `ranker id@table version` that touched it and, per hit, the lexical bits and each
ranker's delta. This is what makes "why did it return this" answerable and what lets a future audit replay a search.

### 7.6 Adoption gate (fixed beforehand)

A ranker becomes a default only if, on a held-out set it did not tune on, it (a) improves the primary metric by at least 2 %
or meets the pre-registered rule for its experiment, (b) leaves every exactness gate of section 14 unchanged when switched
off, (c) stays inside its latency and state budgets, and (d) passes the conformance suite including erasure. Otherwise it stays
optional or is deleted.

## 8. Erasure and retention

Everything is append-only; **deleting is destroying a key, dropping segments, or, as an exception, rewriting**.

| Operation | Mechanism | Proof |
|---|---|---|
| Erase a writer (an end user, a project area) | destroy the writer key (all copies: live, wrapped, backup), delete its directory and derived tables, write a signed receipt | the sealed bytes cannot be decrypted; scan finds no plaintext; receipt in the chain; restore-based key-revocation test required |
| Retention (drop data older than N days) | drop whole segments/runs of the writer; the chain keeps their hashes | receipt with the range dropped |
| Erase one record, one agent, a leaked secret | physical rewrite of the affected writer's segments without those records (slower; rare) | decode every row and inspect every live file; sampling is diagnostic and cannot support `proven`; backup copies and shared live copies are reported separately |
| Erase an agent across writers | rewrite path per affected writer | as above |

* Erasure by key destruction is immediate by default (decision of 2026-10-22: no grace period).
* Backups are encrypted per writer and hold only sealed frames, chain and key wraps. Destroying all recoverable writer keys must make backup content unreadable; see 4.3. This is unproven until a restore
  from an old backup, with all still-valid master/project keys, demonstrably fails for the erased writer; the writer's name, tool names and event metadata may remain readable in
  backups for up to the backup retention (7 days) in the first hosted phase, and the engine work removes that residue by
  sealing names and metadata too (**[open]** D4).
* Tool names survive erasure in the oracle (an engine fix is queued); the new engine seals them with the frame.
* Derived tables are erased with the writer (7.2, rule 5).
* Required before recording end users of customers: writer-restricted keys (separate read and write writer lists, an
  `agent_id` stamp), audit of cross-writer reads and exports, provenance on results, recalled content labelled untrusted.
  These already exist in the hosted API and move into the engine's access model.

## 9. Audit mode **[new]**

Optional per project (`audit = on`). Off costs nothing.

* **Chain.** Per writer, hash-chained frames (4.4) over ciphertext digests.
* **Checkpoints.** Every N frames or T seconds a checkpoint over all chain heads is signed (Ed25519, a project signing key held
  separately from the data keys) and appended to a separate log.
* **Receipts.** Every erasure, retention drop and rewrite writes a signed receipt: scope, time, chain head before and after,
  count of records and bytes affected, how the key was destroyed. Receipts live in the chain, so a verifier sees both that
  data existed and that it was removed.
* **Commands.** `oojuro audit status | verify | export`. `verify` recomputes every chain and checks every checkpoint
  signature and receipt link; `export` writes a self-contained bundle for a third party (chain, checkpoints, receipts, public
  key; no content).
* **What it proves and what it does not.** It detects alteration relative to a trusted checkpoint. A hash chain alone cannot detect truncation of its final suffix;
  checkpoints need an independent durable witness/anchor. A signed deletion receipt proves the signer asserted a deletion,
  not that no key copy survives. Recovery and revocation tests establish the implementation's tested behavior. It does not prove that nobody ever read a record, nor that a copy was not made
  before deletion. The documentation says so.

## 10. Processes, concurrency, deployment shapes

* **One owning process per store** (`LOCK`; many logical writers inside it), many readers; all concurrency stays inside one process. The recorder daemon (`oojurod`)
  owns the store and serves other processes on a Unix socket (mode 600, same uid); queries cost about 10 ms per CLI call
  through it versus 0.27 s opening the journal directly (oracle measurement).
* **Embedded library**: `Journal::open`, no server, one folder per store, zero configuration, crash-safe.
* **Hosted**: one process per tenant group, one engine per project, per-writer private logs, KMS-wrapped master key, retention
  scheduler, encrypted 7-day backups. Reads scale with `query_threads` and per-store caches (N stores on N threads scaled 1.95x
  / 3.8x / 7.3x / 8.0x at 2 / 4 / 8 / 16 stores on tmpfs).
* **Resource rules**: ranker tables and background jobs run at lowest priority; merges never run on the append thread.
* **Crash safety**: recovery rule of 4.2; every file format has a CRC; opening never mutates committed history.

## 11. Adapters (frozen at launch)

An **adapter** turns one harness's activity into records. The contract:

| Requirement | Detail |
|---|---|
| Identity | map the harness's notion of session/instance to `writer`, `agent_id`, `parent_id` (subagents link to their parent) |
| Lossless source | hand the exact bytes it read to the raw layer, with `source`, path hash and sequence, so the harness's own files can be regenerated |
| Resume | a cursor persisted after every batch; after a crash it resumes without loss or duplicates |
| Kinds and typed fields | map to the kinds of 2.1; tool, status, exit where the harness exposes them |
| Policy | honour `record`, `pause`, `exclude`, `record_thinking`, `index_thinking`; unreadable policy fails closed |
| Redaction | call the shared redactor before text leaves the adapter |
| Idempotency | stable keys so a re-read never duplicates |
| Backpressure | bounded queues; resumable sources retry from durable cursor; non-resumable sources need durable spool or an explicit loss/failure signal |

Built-in adapters: **Claude Code** (transcript tailer, hooks with injection of the top results gated by `inject`, never
injecting what is still in context), **Codex** (rollout tailer), **pi**, **Temporal** (TypeScript package `@oojuro/temporal`:
interceptors, sinks, recall tools, claim-check codec for the 2 MB payload limit), **OTel** (GenAI spans), **proxy**
(Anthropic and OpenAI-compatible). A **conformance kit** ships with the SDK: replay a fixture log, kill the adapter at random
points, assert exactly-once, byte-exact regeneration, policy and redaction behaviour.

Injection is off by default until retrieval is proven offline on real data.

## 12. Wire API and local protocol (frozen at launch)

* **Hosted API**: JSON over HTTPS under `/v1`, additive changes only; the key decides the project; scopes `read`, `write`,
  `admin`; keys optionally limited by writer (read list, write list, `agent_id` stamp). The reference is the private cloud repo's
  `docs/API.md`; the endpoints (`records`, `remember`, `search`, `get`, `history`, `range`, `periods`, `export`, `stats`, `usage`,
  blobs, `forget`, writer and agent erasure, retention, audit log, key management, MCP) become the public API specification in
  the public repo before launch.
* **Idempotency** on every write; **per-record errors** in batch responses (the oracle's batch returns one status and the
  client bisects); gzip request bodies accepted; `when` echo carries resolved microsecond bounds; a write-only key can check
  its own project. (These are the gaps the forwarder work found.)
* **MCP**: `remember`, `search`, `get`, `forget`, `periods`, `when`; protocol 2025-06-18 and earlier.
* **Local socket**: one JSON request line, one answer line (export streams a line per record); peer uid must match.
* **CLI**: every command takes `--json`; times accept `now`, RFC 3339, `YYYY-MM-DD`, `@unix`, `1h`, `7d`.

## 13. Versioning, compatibility, the freeze

Frozen at the official launch, each with a version number and an upgrade path:

1. **On-disk format**: `FORMAT` string, frame and segment versions, key wrap and chain record layouts. A build reads the
   current and the previous format version; `oojuro upgrade` converts; raw plus parsed layers can always rebuild everything
   derived, so a derived-layer format change never loses data.
2. **Wire API**: `/v1` additive-only; a breaking change is `/v2` with both served.
3. **Adapter interface**: SDK version in every adapter; the engine accepts the current and previous.
4. **Ranker interface**: trait version; tables carry versions and are rebuilt, never migrated.

Before launch nothing is frozen and the formats may change freely; after launch the freeze applies. The launch gates
(lossless mode, tamper-evident audit, at least one adapter, final AWS numbers) come first.

## 14. Acceptance gates

The new engine may replace the oracle only when all of these hold. Baselines are the oracle at `3a17d1d`.

| Gate | Requirement | Baseline |
|---|---|---|
| Correctness | TraceBench `verify` passes in shared-log and private-log modes | 1,237 / 1,237 both |
| Differential search | the 30,905 tier-1 turn-loop queries return the same ranked ids and score bits as the oracle; SciFact, NFCorpus, FiQA (dev), LongMemEval (dev) and the agent-trace turn loop give equal metrics | identical across the perf round |
| Tests | full workspace tests, no new clippy warning, port of the oracle's 281 tests | 281 passed, 3 ignored |
| Ingest and per-turn loop | no worse than the oracle on TraceBench at equal hardware | shared log 3,261 turns/s, private 1,532; Postgres 59.9, Tantivy 16.9, SQLite 13.2, ClickHouse 4.0 (AWS benchmark, one run) |
| Size | parsed store no larger than the oracle's on the same input; lossless raw blocks within 15 % of original JSONL | store 7.2 % of raw on a 5.3 GB real import (385.7 MB); raw blocks 14.5 %, raw plus parsed 19.8 % |
| Round trip | every raw file regenerates byte for byte (every harness fixture, plus a real-log copy) | 2,024 / 2,024 files |
| Crash | kill -9 at every fsync boundary; torn-tail and mid-history damage cases behave as 4.2 | passes in the oracle's suite |
| Erasure | erase a writer by key destruction: unreadable in store and backup, no plaintext in any file, receipt verifies; rewrite path passes the file-wide byte scan | rewrite proof exists; key destruction new |
| Audit | `verify` detects any single-bit flip, deletion or reordering of frames, checkpoints and receipts (mutation tests) | new |
| Erase latency | one writer of 2,000 in hosted mode | 0.1 s private |
| Read latency | no regression on the search probe at p50 and p99; cold `get`, 1-hour `time_range` | oracle perf round 2 tables |
| Adapters | conformance kit green for every built-in adapter | new |
| Rankers | with every ranker off, results equal the lexical gate above | new |

## 15. What is ported, what is redesigned

| Area | Oracle | New engine |
|---|---|---|
| Group-commit journal, recovery rule, shared log | `oojuro-nine` `wal.rs`, `shared.rs` | **ported** into `log`, adding AEAD sealing and chain records |
| Frames, zstd + dictionary, CDC dedupe, tiers, raw blocks | `oojuro-store`, lossless-exp | **ported**, with sealing built in |
| Segments v3, Rice postings, merge | `segment.rs`, `merge.rs`, `rows.rs` | **ported** into `index` |
| Bits scoring, pivot pruning, tie rule, query terms, F1 to F4 fixes | `v4.rs` | **ported** into `query` behind the ranker slot; proposed writer-score isolation separately evaluated; implemented F4 tie rule retained |
| Record model, ids, metadata map, `Engine` trait | `oojuro-core` | **redesigned** (2.1, D1) |
| Three crates layered by history (nine, trace, oojuro) | | **redesigned** as modules of one engine crate |
| Directory layout, manifest, key wraps | | **redesigned** (4.1, 4.3) |
| Ranker slot | scorers compiled in, D-series prototypes | **new** (7) |
| Audit chain, receipts | | **new** (9) |
| Capture adapters, proxy, redaction, settings, MCP, CLI, `periods`, `when` | `oojuro-capture`, `oojuro-proxy`, `oojuro` | **adapted** to the new engine API; redactor gap closed |

The oracle stays as a differential-test target and as the dogfood engine until the new engine passes section 14.

## 16. Non-goals

SQL or a user-defined schema; embeddings or any model call in the write path; being a message queue or an event bus; a
stand-alone vector database; real-time stream processing; SSO and organization hierarchies (accounts remain individual; explicit owner grants allow registered-human read-only review); zero-knowledge hosting (the privacy promise is local-first, hardened hosted, and run-it-yourself with the Apache
engine; customer-held keys come after launch).

## 17. Open decisions

| # | Decision | Recommendation |
|---|---|---|
| D1 | Record id format: content-derived (oracle) or `(writer ordinal, sequence)` | preserve original id and full tie rule during the differential port; decide a new format only with an explicit compatibility/tie mapping |
| D2 | Where the writer key lives locally (key file 0600 or OS keychain) | key file by default, keychain optional |
| D3 | Seal the parsed layer locally by default | off locally, on hosted |
| D4 | Seal writer names and event metadata so backups hold no readable residue | yes, in the new engine |
| D5 | Hosted per-writer storage cost at thousands of writers (one directory and log per writer) | measure at 10,000 writers before launch; fall back to a shared sealed log with per-writer keys |
| D6 | Server license for single-tenant self-host (Apache or proprietary) | stay proprietary until the launch decision; engine is Apache-2.0 with a never-relicense pledge |
| D7 | Public name and layout of the repos after the old ones are archived | owner decision required; this review uses branches/PRs in the existing repository and does not rename, archive or rewrite history |
| D8 | Whether `it.assoc` ships | decided by 7.6 after D18 to D24 |

## 18. Build plan

Milestones, each ending with its gate of section 14 and a differential run against the oracle:

1. **Format and sealing** (`format`, `log`): frames, AEAD, key hierarchy, chain records, journal with recovery; fuzz and
   crash tests.
2. **Store and index** (`store`, `index`): runs, dedupe, raw blocks, segments, merge; size and ingest gates.
3. **Query** (`query`, `rank::lexical`): the pipeline of section 6 to bit-identical results with the oracle.
4. **Erase and audit** (`erase`, `audit`): key destruction, rewrite fallback, retention, receipts, `audit verify`.
5. **Ranker slot** (`rank`): hooks, conformance suite, first optional ranker if 7.6 allows.
6. **Edges**: adapters and SDK, proxy, CLI, MCP, daemon, forwarder, hosted wiring.
7. **Hardening**: 10 M and 100 M record tests, fault injection, security review, public-repo hygiene, launch.

The oracle keeps recording and forwarding for the dogfood until milestone 6 is green.
