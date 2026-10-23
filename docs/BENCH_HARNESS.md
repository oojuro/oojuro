# Shared benchmark harness (`oojuro-bench`)

One harness for every retrieval and storage comparison (lexical, hybrid,
compress, tool-call, recency, sparse). It changes no engine code: the built-in
`nine` arm measures the `crates/oojuro-nine` of the checkout it is built from,
so the same harness compares branches of this repository.

Scorecard order: **1 gates** (byte-exact, no future leakage, crash recovery,
determinism) → **2 quality** → **3 storage** → **4 memory** → **5 speed**.

## Quick start

```bash
export OOJURO_DATA=~/.cache/oojuro-data                 # dataset cache (default); never committed
python3 -m venv $OOJURO_DATA/.venv && $OOJURO_DATA/.venv/bin/pip install -r scripts/bench/requirements.txt
cargo build --release -p oojuro-bench
B=target/release/oojuro-bench
P=$OOJURO_DATA/.venv/bin/python               # venv with pyarrow, pytrec_eval

# Fetching is idempotent: files already in the cache are only checked against their pinned sha256.
python3 -I scripts/bench/fetch.py beir-scifact beir-fiqa beir-arguana beir-scidocs beir-nfcorpus swe-agent longmemeval hermes-fc openthoughts openhands-sft
$P -I scripts/bench/prepare.py turnloop swe-agent
$P -I scripts/bench/prepare.py memqa longmemeval
$P -I scripts/bench/prepare.py --check                    # prepared files match bench/prepared.json

$B gates                                                   # all built-in arms, 3 runs
$B turnloop --set swe-agent --split dev --trajectories 50 --background 20000
$B beir --dataset beir-scifact --split dev
$B memqa --set longmemeval --split dev
$B report bench-results/*.json --baseline nine > report.md
```

Defaults: arms `native-record nine nine-v3 tantivy sqlite jsonl jsonl-zstd`, `--runs 3`, every run in a fresh child
process inside a `systemd-run --user --scope` with `MemoryMax=3G` and
`MemorySwapMax=0`, stores under `$TMPDIR/oojuro-bench/` (on disk, never /tmp),
scorecard in `bench-results/<suite>-<dataset>-<split>-<utc>.json`.
Timed suites take `$OOJURO_BENCH_LOCK` themselves for the whole run.
They print `waiting for bench lock` while another run holds it. The lock file is
`$OOJURO_BENCH_LOCK` (default `~/.cache/oojuro-bench.lock`).
`--no-lock` is for smoke tests only. The scorecard then says its timings are
not comparable.

Flags: `--arm [label=]spec` (repeat), `--runs N`, `--out FILE`, `--work DIR`,
`--data DIR` (default `$OOJURO_DATA`, else `~/.cache/oojuro-data`), `--mem-max 3G`,
`--in-process` (no isolation, no cgroup memory; quick iteration),
`--keep` (keep stores), `--no-lock`.

## Current native record arm and returned originals

`native-record` measures the current `oojuro-engine` public `RecordLog` and the
same `record::projection` API used by hosted captured-record search. It does not
use Nine or Trace as its query implementation. Every search materializes the
sealed log through the public paged view, resolves the latest imported version
per key at the requested two clocks, and builds a fresh native projection with
the actual source-origin/watermark binding. Its normal excerpt extraction is
included. There is no retained search index. This includes the public view's
pagination/materialization overhead, which is different from hosted blob reads.

The harness's arbitrary source IDs, keys, known-at timestamps and metadata are
stored as explicit **benchmark import attributes**. Native IDs and ingest clocks
remain genuinely assigned by RecordLog; imported known-at is never represented
as a native ingestion timestamp. Harness seconds convert to native microseconds,
with checked input bounds and exact round trips. This adapter tests the harness's imported
two-clock contract, not an independently attested source-clock claim. Text and
attributes must reopen exactly. A public fixed fixture key seals the local
Store; it supplies no production confidentiality/key-management claim.

RecordLog syncs each native append of at most 1,000 records. Larger harness
batches use multiple native commits and may recover partially before the harness
ACK. All acknowledged rows must survive SIGKILL. The native 100k-record/128 MiB
state and metadata/work bounds apply; refusals are recorded, never silently
truncated. The local Store's compression/encryption and metadata overhead differ
from hosted per-object Zstd/S3/KMS and from Trace's dictionary/run archive. Do not
use this arm's footprint, latency or failure domain for hosted pricing or SOTA.

BEIR and turnloop now time **search plus original get for every returned top-k
hit**, verify all those originals against the frozen input, and count returned
original bytes. `search_get_p50_us`/`search_get_p99_us` include both operations and
their protocol round trips; `turn_content_overhead_*` also includes durable
append. The older `search_*` metrics remain IDs-only service timings. Native
projection additionally computes excerpts as its real API requires. Ranking and
tokenization still differ across native/Nine and FTS5/Tantivy; quality metrics
and the exact returned-byte workload accompany speed. A result with a failed gate
or arm error cannot support a performance claim.

## AWS development comparison, 23 October 2026

Frozen source `27a1fb80231f38d5175ad75a99b6bed7880da4d6`, six arms and three
fresh processes/stores per workload ran on a dedicated 2-vCPU `m7a.large`, with
3 GiB cgroup memory and no swap. The native engine/projection source matches
the hosted `a4b2f9d` pin. This is a local API comparison on AWS, not hosted
HTTP/S3/KMS performance, heldout evaluation or a pricing/SOTA acceptance.

| Development workload | Native store MiB | Indexed JSONL+Zstd store MiB | Native search+originals p50 ms | JSONL+Zstd p50 ms |
| --- | ---: | ---: | ---: | ---: |
| SWE-agent, 1k background + 10 trajectories | 0.489 | 1.224 | 61.018 | 9.050 |
| SWE-agent, 4k background + 10 trajectories | 0.788 | 3.075 | 188.389 | 22.445 |
| SWE-agent, 20k background + 10 trajectories | 2.608 | 12.354 | 1015.470 | 102.725 |
| ArguAna, 8,674 documents, frozen dev queries | 3.055 | 9.734 | 785.199 | 110.821 |

Values are medians across three runs. Complete file footprints include payloads
and indexes; allocated sizes are retained separately. The indexed compressed
baseline uses **2.50–4.74× native's file space**, while native's search plus
original retrieval p50 is **6.74–9.89× slower**. It also returns different rankings:
ArguAna nDCG@10 is 0.5367 native versus 0.4368 JSONL+Zstd. SWE-agent labels are
lexically generated, not independent relevance judgments. The archived raw-text
denominator excludes synthetic canary text; complete store sizes include it.

At 20k background records, native durable batch append service time is
3.105 ms p50 / 3.498 ms p99, versus 6.702 / 15.846 ms for JSONL+Zstd.
Prefill is 31,216 versus 28,485 records/s. These are local single-host durability
measurements; the complete protocol/turn timings are retained in the scorecards.

All six arms and all 24 performance scorecards, including failures, are retained
in [the evidence directory](benchmarks/native-aws-2026-10-23/campaign.json).
The 20k-background Tantivy card fails repeated-ranking determinism and is excluded
from performance claims; byte preservation and visibility still pass. Nine's
retained-index arm reaches 1.769 ms p50 there, with a 15.555 MiB store and matching
native nDCG@10. It is a separate API/storage implementation; it does not establish
hosted native performance. Snapshot materialization and projection rebuild are
explicit next profiling targets.

Independent qualification verified 5,973 archived store files, 144 per-run
apparent/allocated footprint values, all 72 off-host store reopens with every
original field, and 24 ArguAna metrics against pytrec_eval. Fresh-process reopen
checks integrity, not cold OS-cache latency. A real Scheduler stop attributed in
CloudTrail preceded workloads. The exact private result archive was downloaded
and SHA-256 verified before the instance, disk, stack, roles and schedule were
deleted. Existing product/key/trial instances remained running. Full provenance,
input hashes, gates, measurements and cleanup evidence are in the directory.

## Indexed JSONL and JSONL + Zstd baselines

The `jsonl` and `jsonl-zstd` arms are built by `sqlite-arm`. Both retain the exact
same serialized `Doc` records as JSON lines, one immutable file per appended
batch. `jsonl-zstd` compresses the batch with Zstd level 3 (no dictionary or
cross-batch deduplication); the arm's hello records the actual codec version.
SQLite holds IDs, two-clock/revision metadata, frame offsets and a **contentless**
FTS5 index. Original text and attributes are in the payload frame, with no second
plain-text copy in the database. Search uses the same Porter/unicode61 tokens,
OR query, BM25 ordering and SQL latest-visible-revision filter as the `sqlite`
arm. It therefore performs indexed search rather than silently omitting that
work from a compression-only baseline. Ranking still differs from Nine.

A batch is acknowledged only after its payload file and payload directory have
been fsynced, followed by the index transaction's WAL `synchronous=FULL` commit.
After interruption, uncommitted payload frames are harmless orphans. They remain
on disk and **count** in the complete footprint; no automatic reclamation hides
them. Reopen retains the original codec and refuses a mismatched arm. Reads
verify the selected frame's BLAKE3 checksum, decode it once per get/range call,
and return byte-exact original text and attributes. There is no persistent
decoded-payload cache. Frame size follows the actual suite batch, so record that
batch configuration when reporting results.

The ordinary footprint recursively counts **all** payload files, SQLite index,
WAL/SHM, metadata and orphan frames, with both apparent and allocated bytes.
`stats` additionally separates payload bytes and referenced/orphan frame counts.
These are local, single-host baselines with no encryption, replication or backup;
they do not match the hosted AWS failure domain or full cost. This harness has no
delete/purge contract; deletion/retention comparisons need their separate gates.

```sh
cargo build --locked -p oojuro-bench --no-default-features --features sqlite-arm
B=target/debug/oojuro-bench
timeout --kill-after=5s 180 "$B" gates --arm jsonl --arm jsonl-zstd --arm sqlite --runs 1
# Timed suites use a release binary, the shared lock and identical frozen input.
# Pass the required arms explicitly when the binary omits other default features.
```

Run these gates before recording quality, speed or storage claims: exact text
before/after reopen, nine two-clock cases, immediate search freshness,
determinism, and three SIGKILL interruptions with acknowledged-write recovery.
The extra arm unit check covers attributes, codec mismatch, duplicate/atomic
index refusal, orphan accounting and payload corruption.

The [23 October 2026 correctness scorecard](benchmarks/jsonl-zstd-2026-10-23/gates.json)
records one run of all three baseline arms at source `81f11fb61eb5`: **21 gate
assertions pass**, including nine total SIGKILL interruptions and 600
acknowledged records checked per arm. The one extra arm unit test passes for
both codecs. Ordinary scoped Clippy succeeds with inherited diagnostics and
none in the new arm. [Provenance](benchmarks/jsonl-zstd-2026-10-23/provenance.json)
pins the source, debug binary and scorecard hashes. Inputs are synthetic; no
throughput, quality, representative storage or hosted AWS result is claimed.

## Plugging in an engine

There are three ways, all measured identically:

1. **This checkout's Nine.** `--arm nine` builds against
   `crates/oojuro-nine` of the checkout.
2. **A new arm file.** Add `crates/oojuro-bench/src/arms/<name>.rs` that
   implements `oojuro_bench::Arm` and register it in `arms::builtin`.
3. **Any executable** (Python, a binary from another branch, a server
   wrapper) that speaks the JSON-lines protocol below. Plug it in as
   `--arm mylabel=cmd:/abs/path/prog arg1 arg2`. Quote the whole `--arm`
   value; argv is split on whitespace.

Cross-branch comparison: build each branch into its own target dir, and one
driver runs them all:

```bash
$B turnloop --set swe-agent --split heldout \
  --arm main=nine \
  --arm other="cmd:/path/to/other-checkout/target/release/oojuro-bench serve --arm nine" \
  --arm tantivy=tantivy
```

### The `Arm` contract

```rust
pub trait Arm {
    fn info(&self) -> ArmInfo;                       // name, version, commit, durability, notes
    fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<()>;
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()>;
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>>;
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>>;
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>>;
    fn stats(&mut self) -> Result<serde_json::Value>;   // optional
    fn close(&mut self) -> Result<()>;
}
```

- `append_batch` returns only when the batch is **durable** (survives
  SIGKILL) and **visible** to the next `search`. One turn is one batch.
- `search` is as-of. It never returns a record with `valid_at > query.valid_at`
  or `recorded_at > query.known_at`. Per `key` (empty key = its own id) it
  returns at most the latest visible revision, ordered by
  `(valid_at, recorded_at, append order)`.
- `get` returns the exact original bytes.
- `open` on an existing directory recovers every acknowledged batch.
  `open` may be called again with another directory: memqa uses one fresh
  store per question.
- `query.context` carries `session`, `turn` and `task` in the turn loop, for
  recency and session priors. Labels never reach an arm.
- `config` comes from `--config key=value` (turnloop). An arm may read it,
  for example `thinking=store` for store-only thinking records.

### Protocol `oojuro-arm-v1` (JSON lines on stdin/stdout, in order)

| request | response fields |
|---|---|
| `{"op":"hello"}` | `info: {name, version, commit, durability, protocol, notes}` |
| `{"op":"open","dir":"/abs/dir","config":{}}` | – |
| `{"op":"append_batch","docs":[Doc]}` | – |
| `{"op":"search","query":{"text","valid_at","known_at","k","context":{}}}` | `hits: [{"id","score"}]` |
| `{"op":"get","ids":["…"]}` | `texts: [string or null]` |
| `{"op":"get_range","from":0,"to":9,"limit":100}` | `docs: [Doc]` |
| `{"op":"stats"}` | `stats: {…}` |
| `{"op":"close"}` | – |

Every response has `ok` (bool), `error` (when not ok) and `elapsed_us` (time
inside the arm, which is what latency tables report; round trip is reported
separately). `Doc = {"id","key"?,"text","valid_at","recorded_at","meta"?}`.
Times are integer UNIX seconds. `oojuro-bench serve --arm nine` is the
reference server.

## Suites

### `turnloop`: the primary workload

Replays agent trajectories turn by turn:

1. **Prefill.** Append `--background N` records from other trajectories, in
   batches of 1,000. Those trajectories never share an issue (`group`) with a
   measured one. When the file runs out, trajectories repeat with `~rK` id
   suffixes, so the store scales to millions of records. Two canary records
   per measured trajectory are also appended: one in the far future and one
   with `recorded_at` in the future. Their text is a strong match for that
   trajectory's queries.
2. **Replay.** For each of the first `--trajectories M` trajectories of the
   split, in frozen order:
   - Append its task record.
   - At each turn, search with the turn input (capped at 4 KiB, head + tail)
     as of the turn's time, with `k = 21`.
   - Drop the input's own record, score the top 20, and append that turn's
     `thinking`, `call` and `result` records as one batch.
3. **Measurements.**
   - Quality: nDCG@10/@20, recall@20, hit@20 and MRR@10 over labelled turns.
   - `freshness_self_hit@21`: how often the previous turn's result is
     findable by search at the next turn.
   - Per-turn overhead p50/p99 (search + append), plus each part.
   - Prefill time and the four gates.
4. **Gates.**
   - `no_future_leak`: no canary, future record or unknown id is ever returned.
   - `byte_exact`: every record of every measured trajectory reads back
     exactly, plus top-20 samples.
   - `freshness`: the previous turn's result reads back exactly at the next turn.
   - `determinism`: identical rankings across runs.

Offline tuning: `--pool 200 --dump-pools F` requests 200 (+1) candidates
per turn and writes `F.<label>`, one JSON line per labelled turn of run 0:
`{traj, step, at, session, turn, query_bytes, relevant, hits: [[id, score,
valid_at, session, turn, kind, path], ...]}`. Metrics still use the top
`--k`. Re-rankers and fusion weights can then be tuned on dev pools without
replaying, and the winner is confirmed with a real held-out run. Pool runs
time `k = pool`, so take speed numbers from a normal run.
`memqa --dump-pools F` writes `{qid, type, at, relevant, relevant_sessions,
hits: [[id, score, valid_at, session], ...]}` per question.

Data: `swe-agent` comes from shard 0 of nebius/SWE-agent-trajectories:
6,669 trajectories, 174,815 turns, 53,191 labelled and 524k records, a
0.7 GB prepared file. Set `OOJURO_SWE_SHARDS=2` when preparing to add
shard 1. `hermes-fc` (1,893 short function-calling conversations) has no
labelled turns. It is tool-call-shaped background and input for the
toolcall path.

**Relevance labels (`turnloop-labels-v1`, deterministic, `scripts/bench/prepare.py`).**
Labels at turn *i* come from what the agent does at turn *i*: its thinking and
its call, which the harness never shows the arm. Only earlier **tool results**
can be relevant, and the turn input's own record never is.

| rule | gain | an earlier result is relevant when… |
|---|---|---|
| R1a re-read | 2 | turn *i* runs a read command (`open cat search_file search_dir find_file grep ls find head tail`) whose whitespace-normalized text equals that earlier turn's call |
| R1b same path | 1 | turn *i* `open`s or `cat`s the same path as that earlier `open`/`cat` |
| R2 reference | 1 | turn *i*'s thinking+call contains a distinctive token that occurs in 1–3 earlier results; tokens are dotted/slashed names or ≥6-char identifiers with `_`, a digit or an inner capital; a token is skipped if it appears in the issue text, the system prompt or this turn's own input |

Records over 128 KiB are truncated with a visible marker (one record in shard 0).
These labels are lexical by construction, which favours lexical arms. Keep
that in mind when comparing dense arms. A labelled set from another source is
welcome as a second opinion.

### `beir`

The corpus is bulk-loaded at `valid_at = 0` and queried at `valid_at = 1`, so
a time prior must be neutral here.

- Scored: nDCG@10, recall@10, recall@100 and MRR@10, with trec_eval
  conventions: raw grades as gains, grades ≤ 0 non-relevant.
- Every labelled split query counts, and an empty result scores 0.
- Each query retrieves 101 and drops any hit whose id equals the query id
  (BEIR `ignore_identical_ids`; this matters for ArguAna).
- Doc text is `title + "\n" + text`.
- `--run-file F` writes TREC runs `F.<label>`.
  `scripts/bench/verify_metrics.py` recomputes them with pytrec_eval.
  On 2026-10-20 they agreed to 1e-9 on SciFact (3 arms, test), NFCorpus
  (graded, dev) and ArguAna (dev).
- Published Anserini BM25 flat and multifield rows from
  `bench/references.json` are attached to each scorecard as references.
  They cover the full test split.

### `memqa`

LongMemEval-S (cleaned, 500 questions; 479 have evidence turns).

- Each question gets a fresh store holding its own haystack: one record per
  chat turn, `"<role>: <content>"`, at `session date + turn index` seconds.
  The question is asked as of its own date.
- Retrieval only, with no reader LLM. Reported: turn nDCG@10, turn
  recall@10/20/50, and session recall_all@5/@10 and recall_any@5 (ranked
  turns mapped to sessions in rank order).
- Results are broken down by question type: `temporal-reasoning`,
  `knowledge-update`, `multi-session`, ….
- Questions are stored in a frozen hash order, so `--limit N` is a mixed
  sample.
- Thirteen haystacks repeat one session verbatim; only its first copy is kept.

### `gates`

These run on synthetic data, in 3 runs:

- `byte_exact`, plus after a clean reopen in a new process. The cases cover
  Unicode, CRLF/CR, a BOM, NUL, tabs, JSON, empty text, 300 KB and a 70 KB
  unbroken run.
- `get_range_order`.
- `as_of_visibility`: nine `(valid_at, known_at)` cases over a 3-revision key
  including a backfill, a future record and a late-recorded record.
- `freshness`: 20 append→search trials.
- `determinism`: two fresh builds and 60 queries.
- `crash_recovery`: SIGKILL with a 2,000-record batch in flight at 0, 5 and
  30 ms. Every acknowledged record must come back exact and searchable. The
  in-flight count shows whether batches are atomic.

## Splits (frozen; tune on dev only)

`bench/splits/<dataset>.json` holds the dev and held-out ID lists.

- Rule: an ID is dev iff
  `int(sha256("oojuro-split-v1:<dataset>:<id>")[:8], 16) % 3 == 0`.
- Units: BEIR test query, SWE-agent `instance_id` (every model's run of an
  issue stays together), LongMemEval `question_id`, hermes conversation.
- Turn loop: the split is by issue, so no trajectory or issue appears on
  both sides. `bench/splits/swe-agent.trajectories.json` lists the
  trajectory IDs of each side in replay order. `bench/splits/swe-agent.stats.json`
  gives each side's query-length distribution. Long-query tuning has a dev
  side: 11% of dev turn inputs hit the 4 KiB cap, and the dev p75 is 3.1 KB.
- `make_splits.py` never overwrites a split file.
- Report final claims on `heldout`. `--split test` (BEIR) exists only to
  compare with published full-test numbers.

## Datasets (cache `$OOJURO_DATA`, default `~/.cache/oojuro-data`; pinned in `bench/datasets.json`)

Every file is sha256-checked on fetch, and BEIR zips are also checked
against BEIR's published md5. No dataset text is committed: only scripts,
manifests, split IDs and aggregate JSON.

| set | source (pinned) | license | use |
|---|---|---|---|
| BEIR SciFact, FiQA, ArguAna, SciDocs, NFCorpus | UKP BEIR zips | per-dataset research licenses (SciFact CC BY-NC 2.0, SciDocs GPL-3.0; the BeIR card says CC BY-SA 4.0) | beir |
| LongMemEval-S cleaned | xiaowu0162/longmemeval-cleaned@98d7416 | MIT | memqa |
| LoCoMo | snap-research/locomo@3eb6f2c | **CC BY-NC 4.0, opt-in (`--accept-nc`), not fetched pending a license review** | memqa (planned) |
| SWE-agent trajectories | nebius/SWE-agent-trajectories@68195a1, shards 0–1 | CC BY 4.0 | turnloop |
| Hermes function calling v1 | NousResearch/hermes-function-calling-v1@dae3e1d | Apache-2.0 | turnloop background, toolcall |
| OpenThoughts-114k (shard 0) | open-thoughts/OpenThoughts-114k@bd093c3 | Apache-2.0 | reasoning traces (fetched; turnloop mix pending) |

Salesforce/xlam-function-calling-60k is gated (it needs an HF account to
accept its terms), so hermes is used instead.

## Run discipline

- Use at least 3 runs. Reports show median (min–max).
- Each run is a fresh process and a fresh store, so build time, cold-start
  memory and determinism are all measured per run.
- Each report states the hardware, isolation, bench-lock status, cache state
  and whether build time is included. Build/prefill is always reported
  separately, never inside query latency.
- Each report also states each arm's own durability. Arms differ, for
  example fsync per record (nine) versus one commit per batch (tantivy,
  sqlite).
- Memory: cgroup `memory.peak` and `memory.current`/`anon` after load, plus
  process VmRSS/VmHWM, each divided by the raw text bytes the arm received.
  Storage: apparent bytes on disk after `close`, divided by raw text bytes.

## Findings so far (2026-10-20, main @ 7b98903)

- Nine's appends are durable but **not batch-atomic**. A SIGKILL during a
  2,000-record batch left 1 and 14 of its records. SQLite and Tantivy left 0
  or all 2,000. One turn can survive as fragments.
- Nine fsyncs a query receipt on every search. In unlocked smoke runs, its
  SciFact search p50 was about 3.5 ms against about 1 ms for Tantivy.
- SciFact full test nDCG@10: nine .697, sqlite-fts5 .687, tantivy .687
  (Anserini BM25 flat .679). All match pytrec_eval.

## Not yet in v0

Not built yet:

- A Postgres FTS arm that checks its plan uses the GIN index.
- YCSB-style A–F point workloads (get/put/scan) as a secondary speed table.
- Verified published memory-QA rows (BM25, full context, Mem0, Zep, Letta),
  marked as LLM-judged.
- An OpenThoughts thinking-heavy background mix.
- LoCoMo, after the license review.
- An archive-tier (10–100 GB) scale plan.
