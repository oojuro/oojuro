# Perf: store (`v1-perfstore`)

Goal: fewer bytes per raw byte for Oojuro's agent journal, losslessly, with
per-record random access still fast. Branch `v1-perfstore` from `v1`
(`0085d01`), 2026-10-20/21. Every idea: hypothesis, change, bytes/raw,
fetch latency, write cost, decision. Lossless gates for every kept change:
byte-exact readback (BLAKE3), crash recovery, erasure with proof
(`cargo test -p oojuro-store -p oojuro-trace`, `tracebench verify --arm
oojuro` on smoke: 1,237/1,237).

## Setup

- **Data.** TraceBench `base1m` (1,318,882 events, 933.7 MB raw text; public
  traces, see `docs/TRACEBENCH.md`). **proxy40**: a synthetic model-proxy
  capture built from base1m (`examples/proxy_stream.rs`): every turn re-sends
  every earlier event of its session (same text, tool and args) before the
  turn's new events; 1 in 40 sessions (463 sessions, 37,301 original events
  plus 1,020,799 re-sent copies = 1,058,100 events, 497.7 MB raw, 20.2x the
  24.6 MB of its originals). **devlogs**: a developer's own logs, read-only
  (`scripts/tracedb/logs_to_tbe.py`, a simplified mirror of the v1 capture
  parsers without redaction): 1,782 transcripts (Claude Code 1,683, Codex 99),
  72 writers, 5.23 GB of JSONL, 383,362 records, 740.3 MB of record text
  (tool_result 521 MB, tool_call 164 MB, system 21 MB, output 19 MB, user
  14 MB, thinking 1.5 MB). Built stores stay under `.scratch/` (git-excluded);
  only aggregates are written here.
- **Baseline.** "today" = branch `v1` at `0085d01` (binary built from it).
- **Store.** `oojuro-trace` arm, `tracebench size` (bulk load, then settle).
  *Flush tier*: per-agent 16 MiB segments, zstd 3 runs. *Archive tier*
  (`archive=on`): every agent store major-compacted into zstd 19 + trained
  dictionary runs (the steady state of a store that merges). Bytes/raw =
  files on disk / raw text bytes, split into **originals** (archive runs),
  **index** (segment files), **columns** (checkpoint). Sizes are exact.
- **Speed.** The machine runs another timed benchmark: every run is `nice 19`
  in a 3 GiB scope, no bench lock; latencies and throughputs are indicative
  and carry the 1-minute load average.

## Results

Bytes per raw byte, **archive tier** (originals / index / columns). "today"
and "final" are paired runs of the `v1` binary and this branch (all kept
changes: P1 record dedupe, P1b reference coalescing, P3 starter dictionary
policy, P5 Rice postings); the middle columns are single runs.

| stream | today | P1 | P1 + P5 | final | change |
|---|---|---|---|---|---:|
| base1m | 0.3807 (0.1420 / 0.2259 / 0.0128) | 0.3021 (0.1327 / 0.1545 / 0.0150) | 0.2764 (0.1328 / 0.1286 / 0.0150) | **0.2762** (0.1326 / 0.1286 / 0.0150) | -27.4% |
| proxy40 | 0.3296 (0.0402 / 0.2836 / 0.0058) | 0.0263 (0.0103 / 0.0095 / 0.0065) | 0.0247 (0.0103 / 0.0079 / 0.0065) | **0.0237** (0.0092 / 0.0079 / 0.0065) | 13.9x smaller |
| devlogs | 0.3954 (0.2189 / 0.1718 / 0.0046) | 0.3839 (0.2160 / 0.1629 / 0.0049) | 0.3350 (0.2164 / 0.1136 / 0.0049) | **0.3344** (0.2159 / 0.1136 / 0.0049) | -15.4% |

**Flush tier** (no major compaction; what a store holds before its first
merge, i.e. most small stores): base1m 0.4558 -> **0.3122** (originals
0.2108 -> 0.1646, index 0.2322 -> 0.1326); proxy40 0.3722 -> **0.0258**;
devlogs 0.5344 -> **0.4058** (originals 0.3454 -> 0.2782, index 0.1843 ->
0.1227).

Where base1m's final index goes (120 MB, 876,552 rows): fixed per-row tables
61% (rows 40 B, id and key fingerprints 20 + 20 B, time order 4 B), postings
29% (26.4M postings at 10.45 bits; LEB128 would be 17.8, the bound is 9.9),
term dictionary 10%.

**Fetch latency** (archive tier, one thread, after load; `tracebench fetch`:
2,000 random 512-byte excerpts, then 300 turn-loop reads = search top 20 for
the first 200 bytes of a random indexed event as of its time, then fetch
its 20 excerpts; ms, p50 / p99; load average before each run):

| stream | load avg today / final | excerpt 512 B | search top 20 | fetch 20 excerpts |
|---|---|---|---|---|
| base1m | 30.2 / 31.4 | 0.26 / 1.52 -> 0.22 / 0.92 | 5.2 / 24.6 -> 3.1 / 9.1 | 0.025 / 0.26 -> 0.026 / 0.24 |
| devlogs | 30.1 / 20.7 | 0.27 / 3.13 -> 0.22 / 0.84 | 43.7 / 309 -> 42.9 / 144 | 0.036 / 0.21 -> 0.035 / 0.20 |
| proxy40 | 17.8 / 19.7 | 0.10 / 0.37 -> 0.005 / 0.56 | 2.1 / 9.9 -> 0.25 / 2.8 | 0.031 / 0.14 -> 0.056 / 1.49 |

Reads of a repeat go straight to the row holding its text (no extra hop).
Search gets faster with fewer postings (repeats are not indexed). On proxy40
the top-20 tail grows (hits are first occurrences in older frames) but
stays under 1.5 ms.

**Write cost** (indicative): base1m bulk load 7.2k -> 10.2k events/s and
major compaction 104 -> 74 s (load avg 30 / 31); proxy40 load 21k -> 116k
events/s, compaction 34 -> 8 s; devlogs compaction 269 -> 118 s (load avg 30
/ 21). Durable per-turn appends (`tracebench ingest`, 16 writers, first
20,000 base1m batches): 3,163 -> 3,272 commits/s, p50 2.79 -> 2.70 ms, p99
15.4 -> 14.0 ms (load avg 20 / 24): the append path stays fsync-bound; the
added work is one BLAKE3 per event. Anonymous RSS after load is noisy
between runs (±30%); paired: base1m archive tier 356 -> 395 MB, devlogs 340 ->
236 MB, proxy40 273 -> 160 MB.

All gates pass on the final tree: `cargo test -p oojuro-store -p
oojuro-trace` (and `-p oojuro-nine`, 60 tests), `tracebench verify --arm
oojuro` on smoke 1,237/1,237 (also `archive=on` and `reclaim=rewrite`), on
proxy40 (also `archive=on`) and on base1m (tier 1, includes delete, purge and
reclaim with 44% of events stored by reference).

## Experiment log

**P1 — message-level dedupe at record granularity (kept).** Hypothesis:
agents repeat whole messages (system prompts, file views, `ls`, "File
updated."), and a model proxy re-sends the whole history every turn, so the
cheapest place to dedupe is the record, before the engine indexes it and
before chunk dedupe sees it. Measured potential first (`examples/dup_probe.rs`,
exact repeats of (tool, args, text) within one agent store):

| stream | repeated events | repeated raw bytes | biggest shares |
|---|---:|---:|---|
| base1m | 44.2% | 33.1% | system 96%, tool_call 43%, user 41%, tool_result 34% of their bytes |
| proxy40 | 98.5% | 96.4% | the re-sent history |
| devlogs | 9.1% | 5.3% | system 53%, tool_result 4.8% |

Change (`crates/oojuro-trace/src/dedupe.rs`, `lib.rs`): per agent store, a
128-bit BLAKE3 of (indexing class, tool, args, text) maps to the engine row
holding that text (a bounded two-generation window, default 256k texts,
rebuilt from replayed records after a restart). A repeat gets **no engine
record and no index entry**: its typed fields ride as one entry of the
batch's `B` list (ASCII, delta-coded) on the batch's first stored record (a
text-less bundle record when the whole batch repeats), and its column slot
points at the row holding the text (`src`). Column slots were decoupled from
engine rows for this (`src`/`host` streams, +0.002 bytes/raw of columns).
Reads go straight to `src` (no extra hop). Search finds a text once, through
its first occurrence, and reports the earliest live occurrence as of the
query time.

*Erasure, reference counted.* Reclaim keeps a text row while its own event
or any live repeat points at it. If its own event is deleted but repeats
live, the row becomes a **holder**: same text, tool and args, the deleted
event's fields removed (nine `rewrite_rows`: `erase_rows` plus metadata edits
in the same segment rewrite); search maps a holder hit to the earliest live
repeat. When the last reference dies, reclaim blanks the row and the segment
is rewritten, so the bytes leave every file. Dead repeats leave their host's
`B` list the same way. Gates (`tests/dedupe.rs`, erase and rewrite reclaim
modes): byte-exact reads (BLAKE3) of every event, reopen from checkpoint +
replay and from a full rebuild give identical answers, a deleted first
occurrence keeps its text for a live copy (and search hands over to it),
deleted unique text is absent from every decoded row and every file, and the
shared text is absent from both once its last copy is deleted. Status
revisions of a repeated tool call land in either order.

Bytes/raw (archive tier; originals / index / columns):

| stream | today | P1 | change |
|---|---|---|---|
| base1m | 0.3807 (0.1419 / 0.2259 / 0.0128) | 0.3021 (0.1327 / 0.1545 / 0.0150) | -20.6% |
| proxy40 | 0.3296 (0.0402 / 0.2836 / 0.0058) | 0.0263 (0.0103 / 0.0095 / 0.0065) | -92.0% (12.5x) |
| devlogs | 0.3948 (0.2183 / 0.1719 / 0.0046) | 0.3839 (0.2160 / 0.1629 / 0.0049) | -2.8% |

On proxy40 the store holds 25,259 engine records for 1,058,100 events; its
13.1 MB is 1.39x what base1m's settings would spend on the 24.6 MB of
original conversation (0.38 x 24.6 = 9.4 MB): re-sending costs ~4 bytes per
re-sent message instead of a re-indexed copy. Flush tier on base1m: 0.4558
-> 0.3572. Write cost: bulk load is faster when repeats skip the index
(proxy40 13.2 -> 3.5 s; base1m within load noise), at the price of one
BLAKE3 per event (outside the lock). Memory: the first version indexed every
repeat by source row in RAM (+180 MB RSS on base1m); the index is now built
per agent only when a search hit's own event is gone, and window keys are
16-byte arrays: base1m flush tier RSS 216 -> 242 MB with dedupe (window
sweep: 1M or 64k texts per agent lose nothing on base1m; 16k loses 3.4% of
repeats, +2.4% bytes). Caveats: dedupe depends on append order, so parallel
bulk loads differ by a few dozen repeats between runs; after a restart, a
text first stored before the restart is not deduplicated against until the
window has seen it again (at most one extra copy per text per restart).

**P1b — reference coalescing for long repeats (kept, small).** Hypothesis:
a large identical substring inside different records (a file body a tool
returns again with a different header) is cut into 1 KiB CDC chunks, each a
separate `(frame, offset, len)` reference. Change (`pack.rs`): a chunk whose
target bytes continue the previous reference's (same frame, adjacent
literal bytes) extends that reference. No format change; old readers read
new runs. Archive tier originals: base1m 0.1328 -> 0.1326 (-0.15%), devlogs
0.2164 -> 0.2156 (-0.4%), proxy request blobs (below) 6.03 -> 6.00 MB
(-0.5%). Kept because it is free and also shortens piece lists on reads.

**P1c — proxy capture as request bodies (measured).** If a proxy stored
each request body as one record instead of one record per message, record
dedupe cannot help (every body differs) and the repeats are large
substrings. `proxy_stream ... req`: per turn one stored-only record holding
the re-sent history (9,854 bodies, 472.7 MB) plus the turn's new events;
497.3 MB raw. Archive tier: today 0.0277 (originals 0.0126, index 0.0144);
P1 + P1b + P3 + P5 0.0226 (0.0121 / 0.0097). Chunk dedupe inside one merged
run already stores a body as references to earlier bodies (~0.27 KB of
originals per body beyond the conversation itself), so bodies are cheap
**once merged**; in 16 MiB flush runs they are not, and a body that is
indexed would be re-indexed in full every turn. So capture should store
messages (P1), or bodies stored-only plus messages indexed.

**P2 — template + variables for tool-call JSON (rejected).** Hypothesis
(CLP / OpenZL): splitting tool-call JSON into a template (keys and
punctuation) and per-field value streams lets zstd see similar values
together. Change (`examples/toolcall_template.rs`): per agent, tool calls cut
into 64 KiB frames (the random-access unit), each frame compressed at zstd
19 as rows (`tool, args, text` per call, as today) or as template ids + new
templates + value streams, lossless by construction (checked); dictionaries
trained per layout on the first 10% of frames. Bytes per raw tool-call byte:

| stream (tool-call share) | rows | per-field streams | one value stream | whole stream rows / columns |
|---|---:|---:|---:|---:|
| base1m (59.6 MB, 6.4% of raw) | 0.1130 | 0.1174 (+3.8%) | 0.1150 (+1.7%) | 0.0774 / 0.0778 |
| devlogs (163.6 MB, 22.1% of raw) | 0.2488 | 0.2497 (+0.4%) | 0.2495 (+0.3%) | 0.1741 / 0.1824 |

Columns lose: zstd 19 with a dictionary already models the repeated keys,
and splitting cuts the context between fields (in base1m the args repeat
the call text; in Claude Code inputs, `file_path` predicts `old_string`).
No change.

**P3 — dictionary policy: starter dictionary + retraining (kept).** Today a
flush run is zstd 3 with no dictionary (training one per flush stalled the
writer, T3) and only merged runs (>= 48 MiB) train their own. Most real
stores never merge: the developer's 72 writers average 10 MB of text, and a
first merge needs 8 x 16 MiB. Hypothesis: a dictionary shipped with the code
helps every small store at no per-run cost. Licenses of the public data on
this machine (`$OOJURO_DATA/*/LICENSE.note`, base1m manifest):

| dataset | license | permissive? |
|---|---|---|
| nebius/SWE-agent-trajectories | CC BY 4.0 | yes (attribution) |
| SWE-Gym/OpenHands-SFT-Trajectories | MIT | yes |
| NousResearch/hermes-function-calling-v1 | Apache-2.0 | yes |
| open-thoughts/OpenThoughts-114k | Apache-2.0 | yes |
| xiaowu0162/longmemeval-cleaned | MIT | yes (not used) |
| BeIR arguana, fiqa, nfcorpus | CC BY-SA 4.0 | no (share-alike) |
| BeIR scidocs | GPL-3.0 / CC BY-SA 4.0 | no |
| BeIR scifact | CC BY-NC 2.0 / CC BY-SA 4.0 | no |

Measured held out first (`examples/dict_probe.rs`: a 112 KiB dictionary
trained on the base1m sessions of one hash quarter; evaluated on the other
three quarters and on the developer's logs; rows shaped like engine records;
prefixes of each agent store; bytes/raw):

| eval | size | zstd 3 | zstd 3 + starter | zstd 19 + own | zstd 19 + starter |
|---|---|---:|---:|---:|---:|
| base1m held out | 1 MiB | 0.2184 | 0.1866 | 0.1815 | 0.1498 |
| | 4 MiB | 0.2082 | 0.1775 | 0.1632 | 0.1429 |
| | 16 MiB | 0.2023 | 0.1743 | 0.1475 | 0.1406 |
| | whole (<= 64 MiB) | 0.1954 | 0.1689 | 0.1362 | 0.1361 |
| devlogs | 1 MiB | 0.3457 | 0.3376 | 0.3046 | 0.2972 |
| | 4 MiB | 0.3036 | 0.2965 | 0.2568 | 0.2600 |
| | 16 MiB | 0.2970 | 0.2897 | 0.2313 | 0.2535 |
| | whole (<= 64 MiB) | 0.2754 | 0.2684 | 0.1929 | 0.2347 |

Change: run files can name a **shared dictionary** (`dict = 2` + its 16-byte
BLAKE3 id) instead of embedding one; such a run opens only with that
dictionary (`tests/run.rs`). `starter-v1.zdict` (112 KiB,
`crates/oojuro-store/dict/`, sources and attribution in its README) is
trained on the archive rows of a base1m store (permissive sources only).
Policy: flush runs use the starter; a heavy run under 4 MiB uses the starter
instead of training (own dictionaries lose below ~4 MiB on both datasets);
merged runs keep training their own (retraining at every merge, E12 budget
rule) because past 4 MiB an own dictionary wins out of domain (devlogs: 0.1929
vs 0.2347 at <= 64 MiB). The prepared dictionary is shared by every reader
and writer (one DDict, one CDict per level): without that, the developer's 72
stores held a dictionary copy per pooled decoder (RSS 193 -> 379 MB in a
pair of runs; 253 MB after the fix; RSS is noisy, see Results). End to end, flush tier, originals bytes/raw: devlogs 0.3367 -> 0.2782
(-17%, held out), base1m 0.1833 -> 0.1641 (-10.5%, in sample: the
dictionary saw these sessions). The domain gap is real: on public traces the
starter alone is worth 14%, on Claude Code / Codex records 2.5% in the probe
(but 17% end to end, where engine-record framing and many tiny runs
dominate). A starter trained on consenting users' own logs would be the
next step; that needs a product decision (only public permissive data is
allowed today). The bigger lever for small stores is compacting them at all
(devlogs, whole store: zstd 3 0.2754 -> zstd 19 + own 0.1929, -30%): see
Next.

**P4 — the developer's logs (measured).** 1,782 transcripts, 5.23 GB of JSONL,
of which 740.3 MB is record text (the rest is UI copies of tool results,
event and token counters, compaction history and JSON framing that capture
drops or does not store; `logs_to_tbe.py` prints the counts). Repeats are
rarer than in public traces (log tailing sees each message once): 9.1% of
records, 5.3% of bytes, mostly system prompts and repeated tool results.
Bytes/raw by kind (`examples/kind_probe.rs`: one run per kind and agent,
engine-record rows, repeats removed; the archive tier and the flush tier
with the starter):

| kind | raw MB | after record dedupe | zstd 19 + own dict | zstd 3 + starter |
|---|---:|---:|---:|---:|
| system | 21.3 | 9.9 MB | 0.1017 | 0.1176 |
| user | 13.6 | 12.4 MB | 0.1709 | 0.1883 |
| thinking | 1.5 | 1.5 MB | 0.3976 | 0.4430 |
| output | 19.2 | 18.9 MB | 0.3373 | 0.3965 |
| tool_call | 163.6 | 162.2 MB | 0.2449 | 0.3151 |
| tool_result | 521.0 | 496.1 MB | 0.2049 | 0.2624 |

Tool results and calls are 92% of the bytes and compress about 2x worse
than in the public traces (base1m, same probe: tool_result 0.0944,
tool_call 0.1270, thinking 0.1956, output 0.2080, user 0.1168, system
0.0086): they are code, diffs and file bodies written whole into tool inputs
(Write / Edit `content`), not repetitive command output. Whole-store
numbers and fetch latency are in the results table.

**P5 — index size: Rice-coded postings (kept).** Where the index bytes
went (`examples/index_probe.rs`, base1m archive tier before P1, 211 MB =
0.226 of raw): fixed per-row tables 52.5% (rows 40 B, id fingerprints 20 B,
key fingerprints 20 B, time order 4 B per row), postings 41.7% (40.5M
postings at 17.4 bits each: LEB128 (gap, tf)), dictionary 5.6%. Sizes of
other postings codings on the same lists: FOR-128 bit packing 13.9
bits/posting, Elias-Fano rows + Elias-gamma tf 10.5, bound 9.7 (log2
C(rows, df) + tf entropy). Change (`oojuro-nine/src/segment.rs`, segment
version 2): per posting, the row gap minus one Rice-coded with k =
floor(log2(rows / df)) (derived, nothing stored per list; the sequential
twin of Elias-Fano), then tf in Elias gamma (63% of tf are 1: one bit).
Version 1 segments still read. Index bytes/raw, archive tier: base1m 0.2259
-> 0.1864 without P1 (-17.5%), 0.1545 -> 0.1286 with P1; devlogs 0.1629 ->
0.1136 (-30%: its records are long, so postings dominate); proxy40 0.0095 ->
0.0079. After P1 + P5 on base1m the index is 0.129 of raw (120 MB): 876,552
rows x 84 B of fixed tables = 73.6 MB (61%), postings 34.4 MB (26.4M
postings at 10.45 bits; 88.1 MB before P1 + P5), term dictionary 11.8 MB.
The fixed tables are now the index (see Next).

**Not done: status revisions as entries (estimate).** In TraceBench a tool
call's final status is a separate text-less record (318,811 of base1m's
1.32M rows); each costs a full 84-byte index row. Riding in `B` lists like
repeats would save ~0.029 bytes/raw on base1m. Not built: v1 capture writes
the status on the tool result and emits no revision records, so the win
would be benchmark-only.

## Next (measured, not built here)

1. **Lean fixed index tables** (nine segment format; lexical / perfsearch
   domain). After P1 + P5 they are 61% of base1m's index: per row 40 B of
   row metadata, 20 B id fingerprint, 20 B key fingerprint (the key equals
   the id for every trace event except status revisions) and 4 B of time
   order (the identity for append-ordered stores). Bit-packing the row table
   and storing key and time entries only where they differ would take ~84 B
   to ~20 B per row: about -0.06 bytes/raw on base1m, the largest remaining
   index lever.
2. **Compact small stores when idle.** Most real stores never reach a merge
   (8 x 16 MiB); compacting a cold store at zstd 19 (starter below 4 MiB,
   own dictionary above) takes the developer's originals from 0.2754 to 0.1929
   (P3 probe, -30%). A daemon policy (idle time, minimum size) is the
   missing piece; `TraceDb::compact_archive` already does the work.
3. **Starter dictionary from consenting users' own logs** (a product decision):
   the public-data starter is worth 14% in its domain and 2.5% on Claude Code
   / Codex records in the probe.
4. **Status revisions as entries** (benchmark-only, -0.029 on base1m).

## Risks

- **Formats.** Index segments are version 2 (old binaries cannot read new
  segments; new binaries read both). Runs using the starter name it by
  BLAKE3 and need `starter-v1.zdict` forever (never edit the file; ship new
  versions beside it). Trace records gained `B` (repeat entries) and holder
  records (`h`); the column checkpoint is `OJCOLS03` (an older checkpoint is
  ignored and the columns are rebuilt from the records). Stores written by
  this branch are not readable by `v1`.
- **Search semantics.** Identical texts are one search document: a query
  returns the earliest live occurrence (as of the query time), not every
  copy, and ranks it by that occurrence's time. Right for an injection
  budget; a recency prior would see the first time a text appeared.
- **Shared code.** `nine::rewrite_rows` (merge.rs, v4.rs) and the postings
  coding (segment.rs) touch the engine the perfsearch branch is also
  changing; expect conflicts in `segment.rs` if that branch reworks postings
  (e.g. block-max skips would need skip data beside the Rice stream).
- **Dedupe exactness.** Repeats match on 128 bits of BLAKE3 without a byte
  compare (collision odds ~n^2 / 2^129). The window is in memory (~20 B per
  remembered text, 256k texts per store) and not persisted: after a restart
  a text is stored again once before it dedupes.
- **Memory.** The dedupe window and column `src`/`host` streams cost RAM:
  base1m flush tier 216 -> 242 MB in a paired run; other pairs vary by
  ±30% between runs (glibc arenas under parallel loads).
- **Licenses.** `starter-v1` contains substrings of CC BY 4.0 data
  (attribution in `dict/README.md`, to ship with binaries); the SWE-agent
  trajectories quote code from many repositories under their own licenses.
