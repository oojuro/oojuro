# TraceBench v1: the agent-trace database workload

TraceBench asks whether a database can hold everything an agent produces
(inputs, outputs, thinking, tool calls and their results) and serve the
operations agents and their operators run on it. It compares Oojuro with
Postgres 17, ClickHouse and SQLite on the same events, the same operations
and the same durability rule, and with competitor arms: the search/vector
engines Tantivy, LanceDB, Meilisearch and Qdrant, the log-search engines
Quickwit and OpenObserve, and the embedded key-value stores redb, fjall and
RocksDB. An operation an engine cannot do under the contract returns
`Unsupported` and every suite skips it with an `unsupported` label instead
of a number; nothing is faked. Code: `crates/oojuro-tracebench` (runner and
arms), `scripts/tracedb/` (data builder, driver). Results and analysis:
`docs/TRACK_TRACEDB.md`. Baseline and competitor settings:
`docs/TRACEBENCH_BASELINES.md`.

## 1. Events

One event = one thing an agent appended. Fields (every arm stores all of them,
typed where the engine has types):

| field | type | meaning |
|---|---|---|
| `id` | u64 | unique, dense in canonical append order |
| `session` | u64 | one task / run of an agent |
| `agent` | u32 | the agent (writer and search scope); 16 agents |
| `turn`, `seq` | u32 | turn within the session; append position within the session |
| `parent` | u64 or 0 | causal parent (call → thinking, result → call, …) |
| `kind` | enum | `system`, `user`, `thinking`, `output`, `tool_call`, `tool_result` |
| `tool` | string | tool name for calls and results ("" otherwise) |
| `args` | JSON | structured call arguments (derived from the call text) |
| `status`, `exit_code` | enum, i32 or null | `none`, `pending`, `ok`, `error`; tool exit code |
| `tokens_in`, `tokens_out` | u32 | model context and output tokens (estimated: ceil(bytes/4)) |
| `ts` | µs | event time (valid time) |
| `duration` | µs | tool run time (set by the status revision) |
| `text` | UTF-8 | the exact original; must read back byte-identical |

**Status revision**: a tool call is appended `pending`; when the tool finishes,
a revision event (`revises = call id`, empty text) carries the final status,
exit code and duration. It rides in the same turn batch as the tool's result.
Engines apply it natively (Postgres/SQLite `UPDATE`, ClickHouse per
`TRACEBENCH_BASELINES.md`, Oojuro an appended revision record). Revisions are
not events for reads: every read returns the call with its latest status.

**Turn batch**: everything one agent appends at the end of one turn (e.g. the
previous tool's result, its status revision, the new thinking and the new
tool call). One batch = one durable commit in every arm.

**Indexing rule**: `user`, `output`, `tool_call`, `tool_result` are indexed
for search; `thinking` and `system` are stored only (design decision 2026-10-20:
store-only thinking wins on next-turn retrieval).

## 2. Data

Built by `scripts/tracedb/build_events.py` from public trajectories in the
dataset cache (never committed; the manifest with sha256 is
`docs/benchmarks/tracedb/base1m.manifest.json`):

| source (license) | sessions | original events | raw MB |
|---|---:|---:|---:|
| nebius/SWE-agent-trajectories shards 0–1 (CC-BY-4.0) | 11,751 | 952,811 | 748.2 |
| SWE-Gym/OpenHands-SFT-Trajectories (MIT), real exit codes | 491 | 23,554 | 33.2 |
| NousResearch/hermes-function-calling-v1 func-calling (Apache-2.0) | 1,893 | 7,706 | 17.7 |
| open-thoughts/OpenThoughts-114k shard 0, first 4,000 (Apache-2.0) | 4,000 | 16,000 | 132.7 |
| **base (tier 1)** | **18,135** | **1,000,071** (+318,811 revisions = 1,318,882 events) | **933.7** |

By kind (bytes): tool_result 461 MB, thinking 205 MB, user 74 MB, system 68 MB,
output 64 MB, tool_call 60 MB, revisions 2 MB.

Derived fields (deterministic, documented in the builder):
- **Time.** Sessions are shuffled and dealt to 16 agents; each agent runs its
  sessions back to back with idle gaps sized so every agent spans 30 days from
  2026-09-01. A model step takes 400 ms + 15 ms per output token; a tool runs
  50 ms–25.6 s (log-uniform, hashed). Batches are ordered by append time, so
  agents interleave like a live fleet.
- **Tokens**: ceil(UTF-8 bytes / 4); `tokens_in` of a model step is the session
  context so far.
- **Exit codes**: OpenHands results carry real ones; SWE-agent results get 1
  when the observation matches an error pattern (traceback, `Error:`, command
  not found, …), else 0; Hermes results are 0.
- `args`: OpenHands `<parameter=…>` pairs, Hermes JSON calls, SWE-agent
  `{"argv": rest of the command line}` (≤ 256 chars per value).

To rebuild the files from a fresh clone (the sources are pinned by sha256 in
`bench/datasets.json`; the builder needs `pyarrow`: create the venv as in
`docs/BENCH_HARNESS.md`):

```bash
export OOJURO_DATA=~/.cache/oojuro-data
python3 -I scripts/bench/fetch.py swe-agent hermes-fc openthoughts openhands-sft
$OOJURO_DATA/.venv/bin/python -I scripts/tracedb/build_events.py .scratch/data smoke 20000     # 20,043 events, ~31 MB
$OOJURO_DATA/.venv/bin/python -I scripts/tracedb/build_events.py .scratch/data base1m 1000000  # tier-1 base, ~1 GB
```

The smoke file is byte-identical across rebuilds: its sha256 is in
`docs/benchmarks/tracedb/smoke.manifest.json`. The builder opens every source
file even for the smoke file, so fetch all four.

**Tiers.** Tier R = R replicas of the base merged by append time. Replica r
shifts session ids by r × 18,135 and its clock by a hashed offset below one
day, keeps the 16 agents (so each agent's store grows with the tier: tier 1
≈ 58 MB/agent, tier 10 ≈ 580 MB/agent, near the 2 GB hot-store size) and gets
new dense ids with remapped parents. Tier 1 ≈ 1M original events, tier 10 ≈ 10M,
tier 100 ≈ 100M (if feasible). **Replication repeats every text byte for byte,
which inflates what dedupe and long-window compression can find**: storage
claims use tier 1, and tier-10 storage is reported with and without dedupe.

## 3. Operations (the `TraceStore` contract, `crates/oojuro-tracebench/src/lib.rs`)

| op | semantics |
|---|---|
| `append(batch)` | durable on return (survives a crash); many concurrent writers |
| `load(batches)` | bulk path (many batches per commit allowed), durable after `finish_load` |
| `get(id)` | the event with latest status |
| `get_range(id, a, b)` | bytes a..b of the text (excerpt fetch) |
| `session(s)` | the whole session in `seq` order |
| `time_range(from, to, n)` | events with from ≤ ts < to, (ts, id) order, at most n |
| `filter(f, n)`, `count(f)` | by agent, kind, tool, latest status and time |
| `search(agent, q, as_of, k)` | top-k among the agent's indexed events with ts ≤ as_of; the arm's native ranking, OR semantics |
| `agg_session(s)` | events, tokens in/out, tool calls, tool errors, text bytes |
| `agg_tools(from, to)` | per tool: calls, errors, total duration |
| `top_sessions(from, to, n)` | sessions by tokens in the window |
| `delete_session(s)`, `purge_before(ts)` | invisible and durable on return |
| `reclaim()` | deletes become physical: no store file may hold deleted text |
| `export()` | every live event as JSON lines, (session, seq) order |

Every deterministic answer is checked against an in-memory oracle
(`tracebench verify`, untimed): 1,237 checks per arm on tier 1, including after
deletes, purge and reclaim. Search rankings differ by design and are not
compared; retrieval quality is measured on the shared harness (`track/bench`).

## 4. Mixes (suites of `tracebench`)

- **ingest** (ingest-heavy): W = 16 writers append turn batches durably into
  an empty store as fast as they can (closed loop); writer w owns the sessions
  with `session % W = w`. First 30,000 batches (~110k events) per run on the
  bench box (60,000 in early local runs).
- **turnloop** (the per-turn agent loop): bulk-load the first 97% of batches,
  then 16 writers replay the last 3% (~11k turns; 10% in early local runs): for each batch whose first event is a
  tool result or user message, `search(agent, first 200 bytes of it,
  as_of = turn time, 20)`, fetch a 512-byte excerpt of each hit, then append
  the batch durably and read its last event back (freshness gate).
- **reads** (dashboard/analytics): bulk-load all, then single-threaded
  get, get_range, session, agg_session (300 each), 1-hour scans (100),
  four filter shapes with counts, per-tool aggregates over a day and over 30
  days, top-10 sessions of a day, a mixed 8-thread run, and a full export.
- **purge** (retention, runs after reads on the same store): delete 100
  random sessions, purge everything older than 7 days, check invisibility,
  reclaim, and scan every store file for 20 needles (48-byte substrings of
  deleted texts that occur in no surviving text) before and after reclaim.
  Plaintext scans only prove something for engines that keep text
  uncompressed in some file (Postgres heap below the TOAST threshold, WAL,
  SQLite pages); compressed engines additionally prove removal structurally
  (rewritten files, reports per arm).

## 5. Rules

- One sync per finished turn: every `append` is durable on return. Arms that
  relax it (Postgres `synchronous_commit=off`) are labelled non-durable and
  never compared as durable.
- Each (arm, suite, run) runs in a fresh process inside a 3 GiB memory scope
  (`systemd-run --user --scope -p MemoryMax=3G -p MemorySwapMax=0`); docker
  arms get `--memory 3g`. Timed runs hold `$OOJURO_BENCH_LOCK`.
- ≥ 3 runs per arm; tables report the median and the min–max spread.
- Memory: embedded arms report anonymous RSS (`RssAnon`); docker arms report
  cgroup anon + shmem (shared buffers count, file cache does not).
- Disk: bytes of data, indexes and logs the engine needs; see per-arm notes
  for write-ahead logs retained for recovery.
- Hardware: AMD Ryzen 7 8845HS (8 cores / 16 threads), 27 GiB RAM (≈9 GiB
  free, swap full), Crucial CT1000E100SSD8 NVMe (DRAM-less), ext4, Linux 7.0.
  The machine is shared with other research sessions and runs at load 30–45;
  every timing carries `loadavg` before and after the run.

## 6. Traditional suite (ycsb)

YCSB's core workloads A–F, a bulk load, a writer sweep, open time and crash
recovery, run through the same `TraceStore` contract, so one command times
Oojuro, SQLite, Postgres and ClickHouse on equal terms and the numbers can
be read against standard YCSB results. Code: `crates/oojuro-tracebench/
src/ycsb.rs` (generators, unit tests), suite `ycsb` of `tracebench`.

### Records and operations

| YCSB | here |
|---|---|
| record, key | one `tool_call` event, `pending`; key k = id k + 1 for loaded keys; later keys get ids from the shared event-id counter |
| record layout | sessions of 1–100 consecutive keys (seeded), dealt round-robin to 16 agents; event time `2026-09-01 + 2 s × k`, so a time range is a key range |
| field values (default, `--ycsb-data real`) | text, tool and args of the tier's events in canonical order (revisions skipped; cycled if more records than events) |
| field values (`--ycsb-data synthetic`) | YCSB's record: 10 fields × 100 random printable bytes, as JSON in `args` (unindexed, like a YCSB table; `text` empty); ~1.1 KB |
| read | `get(id)` |
| update | an appended status revision of the record (status `ok`, exit code, `duration_us` = the revision's id): a new version. Arms apply it their own way: Postgres/SQLite `UPDATE` in place, ClickHouse a `call_status` row (argMax on read), Oojuro an appended revision record |
| insert | a new record (key = next key) |
| scan (E) | `time_range(ts(k), ts(k + len), len)` with len uniform 1–100 (default), or `session(session of k)` (`--ycsb-scan session`, 1–100 records) |
| read-modify-write (F) | `get`, then a revision computed from what was read (status flipped, exit code + 1) |

| workload | mix | keys |
|---|---|---|
| A | 50% read / 50% update | `--ycsb-dist` (zipfian default) |
| B | 95% read / 5% update | same |
| C | 100% read | same |
| D | 95% read latest / 5% insert | latest |
| E | 95% short scan / 5% insert | same as A |
| F | 50% read / 50% read-modify-write | same |

Request distributions follow YCSB's `CoreWorkload`: **zipfian** = YCSB's
ScrambledZipfianGenerator (θ = 0.99 over 10^10 items with the precomputed
ζ = 26.469, FNV-hashed into keys `0 .. records + 2 × expected inserts`;
draws beyond the newest acknowledged key are redrawn), **uniform** over the
keys present when the cell starts, **latest** (D) = SkewedLatest: newest
acknowledged key minus a zipfian draw, ζ extended incrementally as keys are
added. Inserted keys become readable once they and every earlier key are
acknowledged (YCSB's acknowledged counter). Every draw comes from a seeded
SplitMix64 (`--seed`), one stream per (cell, client thread).

### Phases (`--ycsb-phases load,open,run,writers,crash`, all by default)

1. **load**: `load` in chunks of 1,000 records from `--load-threads` (4)
   loaders, then `finish_load` (timed apart); 200 random rows checked.
2. **open**: drop the store, shut a server arm down cleanly
   (`<arm>.sh halt` = `docker stop`), then reopen it (`existing=on`: no
   schema, no reset) and read one record. Embedded arms: files stay in the
   OS cache (warm open); server arms: container start + startup + first
   query (the start script polls every 0.1 s). 200 rows checked after.
3. **run**: cells in order workload (A, B, C, F, D, E, like YCSB's
   recommended sequence, without a reload before E) × distribution × commit
   size (`--ycsb-batch 1,10`) × client threads (`--ycsb-threads
   1,2,4,8,16`), each a closed loop sharing `--ycsb-ops` operations (10,000)
   on the same, growing store. Batch 1: every update/insert is its own
   commit, durable on return. **Batch 10 (labelled `BATCHED` in
   `durability`)**: each client buffers 10 writes and commits them with one
   `append_many`, durable when it returns; a write's latency runs from its op
   start until that commit returns, and `commit` reports the commits
   themselves. C has no writes, so no batched cells.
4. **writers**: append-only sweep, `--ycsb-writers 1,2,4,8,16` threads, closed
   loop, one new record per durable commit, `--ycsb-writer-ops` commits per
   point (default `--ycsb-ops`): `commits_per_s` and latency per point.
5. **crash**: a child process (`tracebench ycsb-crash-child`) writes records
   into a fresh store with `--ycsb-crash-writers` (4) threads, one durable
   `append` per record, printing each id after its append returns. After
   `--ycsb-crash-acks` (2,000) acknowledgements the parent SIGKILLs it
   (server arms: `<arm>.sh kill` SIGKILLs the database container too), then
   times reopening the store until the first read, and reads back every
   acknowledged record byte for byte (`missing_acked`, `mismatched_acked`
   must be 0; `unacked_present` counts writes that committed but whose
   acknowledgement died with the process). The test is `valid` only if the
   writer was SIGKILLed mid-load after the full ack count; a writer that
   exited on its own or reached fewer acks within `--ycsb-crash-wait-s`
   (120 s) makes it invalid, which counts as a correctness failure.

`--ycsb-cell-seconds S` caps each cell and each writer point at S seconds
(the ops done are reported); use it for arms whose single durable write is
slow (ClickHouse acknowledges a durable insert after an async-insert flush,
50–200 ms) so a sweep stays bounded. The same cap for every arm keeps it fair:
throughput is a rate over the window either way.

### Output (one JSON line per run, `--out`)

`ycsb.config`, then per phase:

- `load`: records, raw bytes, `load_s`, `finish_s`, `records_per_s`,
  `mb_per_s`, `disk_bytes`, `bytes_per_raw`, `mem_peak`, `mem_bytes`.
- `open`: `close_s` (drop + clean shutdown), `open_s`, `first_get_ms`,
  `open_to_first_read_s` (the open time), rows checked.
- `cells[]`: workload, mix, dist, threads, batch, `durability`, `ops`,
  `wall_s`, `ops_per_s` (throughput: all operations / wall time); per op type
  (`read`, `update`, `insert`, `scan`, `read_modify_write`, `commit`) a latency
  summary: `n`, `p50_ms`, `p90_ms`, `p95_ms`, `p99_ms`, `max_ms`, `mean_ms`
  (scans: `items` = rows returned); `read_misses` (reads of an acknowledged
  key that found nothing; must be 0); `records_before/after`;
  `disk_bytes_before`, `disk_bytes` (after the cell); `mem_bytes` (after) and
  `mem_peak` (sampled every 100 ms); `loadavg_before`, `loadavg_after`.
- `writers[]`: `writers`, `commits_per_s`, the latency summary, disk, memory,
  loadavg.
- `crash`: `valid` (+ `invalid_reasons`), `acked_at_kill`, `acked`,
  `killed_after_s`, `child_status`, `open_s`,
  `first_get_ms`, `reopen_s` (recovery time = open + first read),
  `verify_s`, `missing_acked`, `mismatched_acked`, `unacked_present`,
  `disk_bytes`.
- `correctness`: total checks, failures and read misses of the run.

Disk and memory mean what they mean for the other suites (§5): embedded
arms report the process's `RssAnon` (which includes the runner's own key
tables, and the oracle copy in check mode), docker arms the container's
anon + shmem; Postgres `disk_bytes` excludes WAL.

### Correctness (`--ycsb-check`)

Feeds every write to an in-memory oracle (`arms::oracle::OwnedOracle`, also
available as `--arm oracle`) and compares every read, read-modify-write read
and scan result with it, then every key's row and 400 sampled scans at the
end (`final_check`). Workload cells run with one client thread so the op
order (and so every expected answer) is fixed by the seed; the writer sweep
keeps its threads (inserts commute). Check-mode timings are not results.

### Commands

Smoke (correctness, small data; any machine state):

```bash
B=target/release/tracebench; S=.scratch/data/smoke.tbe
for arm in oracle sqlite oojuro tantivy redb fjall rocksdb; do
  $B ycsb --arm $arm --data $S --work .scratch/work --out .scratch/results/ycsb-smoke.jsonl \
    --ycsb-ops 1000 --ycsb-writer-ops 300 --ycsb-crash-acks 500 --ycsb-check
done
```

Full timed sweep (tier 1, real records; `run.sh` takes the bench lock and
the 3 GiB scope; server arms reset their container themselves, so never two
`pg` or two `ch` runs at once):

```bash
for arm in oojuro sqlite pg ch; do
  scripts/tracedb/run.sh $arm ycsb 3 1 --ycsb-ops 10000 --ycsb-cell-seconds 120 --ycsb-crash-wait-s 600
done
# classic YCSB records (1M x ~1.1 KB synthetic):
for arm in oojuro sqlite pg ch; do
  scripts/tracedb/run.sh $arm ycsb 3 1 --ycsb-data synthetic --ycsb-records 1000000 \
    --ycsb-ops 10000 --ycsb-cell-seconds 120 --ycsb-crash-wait-s 600
done
# variants: --ycsb-dist zipfian,uniform   --ycsb-scan session   --ycsb-batch 1
# competitor arms (docs/TRACEBENCH_BASELINES.md, "Competitor arms"): tantivy
# lance meili qdrant quickwit openobserve redb fjall rocksdb, same flags with
# --ycsb-crash-wait-s 1200 (quickwit acknowledges a durable write after ~2 s,
# so 2,000 acknowledgements from 4 writers take ~1,000 s)
```

Useful flags: `--ycsb-records N` (default: every event of the tier; 1M
synthetic), `--ycsb-workloads abcfde`, `--ycsb-threads 1,2,4,8,16`,
`--ycsb-batch 1,10`, `--ycsb-writers 1,2,4,8,16`, `--ycsb-writer-ops N`,
`--ycsb-crash-acks N`, `--ycsb-crash-writers N`, `--ycsb-crash-wait-s S`,
`--seed`, `--load-threads`.

### How to read it against YCSB and update-in-place databases

- **An update is an append, not an overwrite.** Oojuro (and ClickHouse's
  `call_status`) store every revision; Postgres and SQLite update the row in
  place (Postgres still writes a new heap tuple and WAL). An update writes a
  ~50-byte status revision, not YCSB's 100-byte field, and reads must fold in
  the latest revision. Disk grows with updates on the append-only arms until
  compaction; compare `disk_bytes` growth per cell with that in mind.
- **Every record carries the trace schema's indexes.** Each insert also
  maintains the session, time and kind indexes and (real text, `tool_call`
  kind) the full-text index of every arm, which a YCSB key-value table does
  not have. Synthetic records keep their payload in unindexed `args`, the
  closest to YCSB.
- **Batched commits are per agent store in Oojuro.** A 10-write
  `append_many` touching k agents is k journal commits there (one store per
  agent, no cross-agent log); one transaction in SQLite and Postgres, one
  insert in ClickHouse. Postgres updates a commit's rows in id order so
  concurrent batched commits cannot deadlock; a deadlock victim (40P01)
  would be retried once.
- **Durability**: one commit per write means an fsync'd commit on every arm
  (Postgres `synchronous_commit=on`, SQLite `synchronous=FULL`, ClickHouse
  fsync on insert, Oojuro a journal sync); YCSB clients often run with
  relaxed durability, so expect lower write throughput than published YCSB
  numbers for the same engines.
- **Crash test**: a SIGKILL of the process (and container) leaves the OS
  page cache intact, so it proves recovery of acknowledged writes and the
  recovery time, not fsync ordering against power loss.
- **Real payloads repeat** when `--ycsb-records` exceeds the tier's events
  (the source cycles), which helps dedupe; `config.distinct_payloads` says
  how many distinct payloads there are.
