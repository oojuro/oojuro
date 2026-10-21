# Track: compression (oojuro-store)

Path: fewest bytes on disk and in RAM for agent text, fully lossless,
while per-turn writes and top-20 excerpt reads stay fast. Branch
`track/compress` (from main 7b98903), started 2026-10-20; its measured
sections are merged here. The closing pass (2026-10-21) finished the
in-process leaderboard on every corpus plus a sample of a developer's own
journal (`devlogs`), ran the external codecs everywhere, evaluated a cold archive
tier and the search index, and ends with a summary: **what we use, what we
tried, what we rejected and why, what to adopt next** (last section).

How v1 uses this work today (`docs/PERF_STORE.md`): TraceDb keeps originals
in `oojuro_store::run` files: flush runs at zstd 3 with the shipped starter
dictionary, merged (archive-tier) runs at zstd 19 with a dictionary trained on
the run (≤ 512 KiB, ~1/256 of the text), 64 KiB frames, 1 KiB FastCDC dedupe
inside a run, plus record-level dedupe in TraceDb and Rice-coded postings in
the index. The `Store` API below (hot log + cold segments) is the track's
standalone store; its packer and reader are the ones run files use.

## What exists

`crates/oojuro-store` is a standalone store behind a narrow boundary the engine
can adopt without touching `oojuro-nine` internals:

| call | meaning |
|---|---|
| `Store::append(&[NewRecord])` | one turn batch, one frame, one `fdatasync`; returns the first sequence number |
| `get(seq)`, `get_range(seq, start, end)` | exact original bytes; decodes only what the range needs |
| `record(seq)` | text plus kind, two clocks (`valid_at`, `recorded_at`), key, attrs |
| `scan_time(from, to, f)` | records with `from <= valid_at < to`, in sequence order |
| `compact()` | rewrite the hot log into a cold segment (dedupe + dictionary + heavy level) |

Records carry a kind (system, user, thinking, tool_call, tool_result, answer,
doc), two clocks, an optional revision key and opaque attrs. IDs are dense
sequence numbers; the engine can keep deriving its own 64-hex IDs (the current
journal stores them plus an envelope per record, see E0).

### Format v1 (little-endian; `src/format.rs`, `src/store.rs`)

- **Frame** = `len u32 | crc32 | payload`; payload = varint head (codec, dict id,
  first seq, count, raw len, meta len, optional BLAKE3-128 digest) + body.
  Body = meta section (per record: kind/flags, lengths, zigzag time deltas,
  optional piece list), keys/attrs, then literal text. The meta comes first, so
  an excerpt decodes `meta_len + end` bytes, not the whole frame.
- **Hot tier**: `hot-<id>.ojl`, one zstd frame per turn batch (level 3; store
  dictionary once 4 MiB exist, refreshed at every compaction). Magicless zstd
  frames without size or dict-id fields (the head already has both).
- **Cold tier**: `cold-<id>.ojc`, frames of 64 KiB literal text at zstd 19 with
  a dictionary trained on the compacted data, plus a varint index and trailer
  for O(frames) open. Each frame carries a BLAKE3-128 of its body and is
  decoded and compared before it is written. Frames are compressed on 4
  threads; output is identical for any thread count.
- **Dedupe**: compaction cuts records with FastCDC (avg 1 KiB). A chunk already
  stored literally in an earlier frame (this compaction, or a 1 GiB window of
  the newest cold data) becomes a reference `(frame delta, literal offset,
  len)`. References only point at literal bytes, so a read is at most one hop.
  The chunk table is rebuilt from the window at compaction (128-bit BLAKE3
  keys) and never persisted.
- **MANIFEST** (atomic rename) lists cold segments, dictionaries and the first
  live hot log. Compaction writes and syncs the segment, publishes it, then
  deletes the hot logs it replaced. Orphans are removed on open.

### Gates (`crates/oojuro-store/tests/store.rs`, 10 tests, all passing)

| gate | evidence |
|---|---|
| byte-exact originals | every bench build reads every record back and compares BLAKE3 with the source; tests check text, kind, clocks, key and ranges after append, reopen, compaction and reopen |
| torn writes inside compressed frames | cut at **every byte** of the final compressed frame: recovers all committed records, appends continue |
| torn tails of zeros / garbage | truncated on open |
| corruption before the tail | open fails `Corrupt` (a resync scan tells a torn tail from damage) |
| header corruption | hot header, manifest, cold header, cold index, dictionary: open fails `Corrupt`; a flipped byte inside a cold frame fails that read |
| interrupted compaction | orphan segment/dictionary/manifest temp file removed; replaced hot logs that reappear are ignored |
| dedupe references | refs across frames and segments roundtrip after reopen; corrupting a referenced frame fails the referring read loudly |
| determinism | two builds of the same input are byte-identical, with 1 vs 4 compaction threads |
| old formats readable | v1 is this store's first version. The engine's NINET003 journal stays readable by `oojuro-nine` (untouched); adoption needs an importer or dual reader (see Adoption) |

## Corpora (public ones: manifests with sha256 in `docs/benchmarks/compress/manifests/`)

| name | source (license) | records | raw MB | kinds by bytes |
|---|---|---|---|---|
| swe | nebius/SWE-agent-trajectories shard 0 (CC-BY-4.0) | 529,322 | 385.2 | tool_result 64%, thinking 14%, system 8%, user 7%, tool_call 7% |
| ohjson | nebius/SWE-rebench-openhands-trajectories, row group 0 prefix (CC-BY-4.0) | 371,691 | 537.0 | tool_result 63%, tool_call (OpenAI JSON) 20%, system 10% |
| othink | open-thoughts/OpenThoughts-114k shard 0 prefix (Apache-2.0) | 33,272 | 268.4 | thinking 82%, answer 9% |
| hermes | NousResearch/hermes-function-calling-v1, all files (Apache-2.0) | 64,758 | 50.7 | user 51%, system 28%, answer 12% |
| ohsft | SWE-Gym/OpenHands-SFT-Trajectories (MIT) | 23,554 | 31.5 | tool_result 70% |
| fiqa | BeIR/fiqa corpus (CC-BY-SA-4.0), non-agent control | 57,600 | 44.3 | doc |
| nfcorpus | BeIR/nfcorpus corpus, control (floors only) | 3,633 | 5.8 | doc |
| devlogs | a developer's live journal: `oojuro export --from 60d` on 2026-10-21 (546,835 records, 1,135 MB of text, 1,938 sessions, 74 writers); the 64 MiB sample keeps 125 whole sessions chosen in a fixed hash order (`scripts/compress/devlogs_sample.py`), in append order. Private: kept under `.scratch/`, never committed | 29,237 | 67.9 | tool_result 49%, system 33%, tool_call 15%, answer 1.3%, user 1.1%, thinking 0.2% (journal: 50 / 32 / 15 / 1.7 / 1.4 / 0.1%) |

`xlam-function-calling-60k` is gated (login required); hermes (which includes
glaive function calling) substitutes. Records are what an agent appends: the
turn input, thinking text, the tool call, the tool result, the answer. A turn
batch = all records of one (session, turn). Clocks are synthetic
(`session * 100000 + turn`). `scripts/compress/build_corpus.py` rebuilds them.

Redundancy note: swe holds several model runs per SWE-bench instance, so
cross-session repeats are high (61% of bytes repeat at 1 KiB CDC chunks, 20%
within one session). That is realistic for a per-project store or a shared
archive, less so for one agent on unrelated tasks; E4 reports both.

Hardware: AMD Ryzen 7 8845HS (8 cores / 16 threads), NVMe ext4, Linux 7.0;
shared with other jobs (timed runs serialize on
`$OOJURO_BENCH_LOCK`).

## Reference floors (whole corpus, bytes / raw text bytes)

| corpus | zstd -19 --long=27 | xz -9 |
|---|---|---|
| swe | 0.0300 | 0.0288 |
| ohjson | 0.0772 | 0.0753 |
| othink | 0.1760 | 0.1674 |
| hermes | 0.0598 | 0.0590 |
| ohsft | 0.0780 | 0.0754 |
| fiqa | 0.2677 | 0.2596 |
| nfcorpus | 0.2357 | 0.2322 |

Single-stream floors with no random access.

## Scorecard (track `Store`, full corpora; `scripts/compress/scorecard.sh`)

Measured by the track on 2026-10-20 (results were in its scratch output but
had not reached this table). Each timed phase under the bench lock; one
or two reps; "—" = not recorded. fiqa was not run.

Default profile (64 KiB frames, ≤ 512 KiB adaptive dictionary):

| corpus | raw MB | compacted | zstd -19 --long=27 | xz -9 | append p50 / p99 µs (quiet) | append p50 / p99 µs (during compaction) | top-20 excerpt p50 / p99 µs | RSS MiB (RSS/raw) | open ms | compaction MB/s (4 threads) |
|---|---|---|---|---|---|---|---|---|---|---|
| swe | 385 | **0.0512** | 0.0300 | 0.0288 | 2,998 / 10,396 | 3,388 / 22,508 | 2,833 / 25,376 | 17.5 (0.048) | 19.6 | 6.2 |
| ohjson | 537 | **0.0985** | 0.0772 | 0.0753 | 3,520 / 19,897 | 5,170 / 21,680 | 9,136 / 41,943 | 19.6 (0.038) | 17.7 | 4.2 |
| othink | 268 | **0.1985** | 0.1760 | 0.1674 | 1,816 / 6,999 | 3,001 / 8,991 | 6,559 / 11,665 | 23.4 (0.092) | 4.1 | 3.3 |
| hermes | 51 | **0.0859** | 0.0598 | 0.0590 | 1,968 / 26,922 | — | 1,233 / 3,509 | 15.6 (0.322) | 6.2 | 22.7 |
| ohsft | 32 | **0.1214** | 0.0780 | 0.0754 | 1,740 / 2,032 | — | — | — | — | 22.3 |

Archive profile (256 KiB frames, ≤ 1 MiB dictionary):

| corpus | compacted | append p50 / p99 µs | top-20 excerpt p50 / p99 µs | RSS MiB | open ms | compaction MB/s |
|---|---|---|---|---|---|---|
| swe | **0.0426** | 2,408 / 5,826 | 4,722 / 22,778 | 19.9 | 21.8 | 12.8 |
| ohjson | **0.0889** | 2,374 / 6,024 | 10,251 / 90,505 | 25.2 | 13.0 | 9.8 |
| othink | **0.1937** | 3,009 / 9,036 | 5,734 / 23,995 | 21.1 | 3.3 | 4.2 |

The store stays 1.3–1.7x above the single-stream floors (zstd -19 --long,
xz -9) while serving any record's excerpt in a few ms and holding 2–9% of
raw in RAM. The leaderboard below repeats the comparison on equal 64 MiB
samples with v1's run format.

## Matched baselines (full corpora; the journal on 32 MiB prefixes; every row read back and BLAKE3-verified)

| system | swe | ohjson | othink | hermes | ohsft | fiqa | notes |
|---|---|---|---|---|---|---|---|
| **pre-v1 engine journal (NINET003)** | 0.9629 | 0.6340 | 0.5308 | 0.7941 | 0.7046 | 1.1603 | the engine kept everything in RAM; RSS after open 3.65 / 3.04 / 1.85 / 2.88 / 3.19 x raw; open 4.3 / 2.3 / 1.3 / 3.7 / 3.0 s |
| SQLite (one row per record, VACUUM) | 1.2325 | 1.1435 | 1.0806 | 1.1816 | 1.1214 | 1.2106 | |
| Tantivy 0.27 doc store, lz4 | 0.3565 | 0.3289 | 0.4692 | 0.4713 | 0.3970 | 0.6929 | |
| Tantivy 0.27 doc store, zstd 19 | 0.2273 | 0.2120 | 0.2635 | 0.2945 | 0.2489 | 0.4395 | 16 KiB doc blocks |
| Tantivy 0.27 store + positional index | 0.5054 | 0.4409 | 0.5637 | 0.5918 | 0.5314 | 0.8437 | the index alone: 0.28 / 0.23 / 0.30 / 0.30 / 0.28 / 0.41 |
| Postgres 17 TOAST pglz / lz4 | 0.8232 / 0.8739 | 0.5665 / 0.6016 | 0.4854 / 0.5807 | 0.9308 / 0.9675 | 0.6654 / 0.7106 | 1.0672 / 1.0877 | `pg_total_relation_size` (heap + TOAST + key index); bytea column (a text column rejects the NUL bytes in fiqa/ohsft); TOAST leaves values under ~2 KiB uncompressed |
| **oojuro-store** (track `Store`, default, compacted) | **0.0512** | **0.0985** | **0.1985** | **0.0859** | **0.1214** | — | full corpora (scorecard above) |
| **oojuro-store**, same 32 MiB prefixes as the journal | **0.0613** | **0.1124** | **0.2082** | **0.0571** | **0.1214** | — | 1 rep |

## Experiment log

Each entry: hypothesis, change, delta, decision. Exploratory sweeps use the
64 MiB samples (`manifests/s64.json`) and report ratio only unless marked as
timed under the lock. Rejected ideas stay as code paths in the experiment
binaries (`codecs`, `dedupe-exp`) rather than in the store.

**E0 — baseline (current journal).** JSON envelope per record (64-hex id,
digest strings, clocks, flags), LZ4 per frame, prefix/suffix delta vs the
previous two records: 0.53–0.96 bytes/raw on disk, 1.8–3.7x raw in RSS,
seconds to open 32 MiB.

**E1 — binary frames + zstd + dictionary + 64 KiB frames (no dedupe).**
Hypothesis: the envelope and per-record LZ4 dominate. ohsft, compacted store:
0.156 vs journal 0.705 (4.5x smaller). **Kept** (format v1).

**E2 — hot tier codec per turn batch** (ohsft, hot only, unlocked): zstd L1
0.272, L3 0.251 (82 µs/batch incl. compression), L9 0.226 (231 µs), L15 0.217
(962 µs, as slow as the fsync). **Kept L3**; L9 is an option. The hot tail is
bounded by the compaction interval (64 MiB), a small share of a 2 GB store.

**E3 — group frames by record kind, one dictionary per kind.** Hypothesis:
homogeneous frames compress better. ohsft 0.1235 vs append order 0.1189,
hermes 0.0888 vs 0.0842 (with dedupe). **Rejected**: append order keeps session
locality, which matters more than kind homogeneity.

**E4 — content-defined dedupe with one-hop references.** Dedupe potential
(FastCDC; share of bytes in chunks seen before):

| corpus | 256 B | 512 B | 1 KiB | 4 KiB | within-session @1 KiB |
|---|---|---|---|---|---|
| swe | 0.695 | 0.661 | 0.613 | 0.561 | 0.206 |
| ohjson | 0.296 | 0.253 | 0.219 | 0.159 | 0.041 |
| othink | 0.043 | 0.042 | 0.041 | 0.041 | 0.001 |
| hermes | 0.616 | 0.584 | 0.550 | 0.535 | 0.000 |
| ohsft | 0.415 | 0.384 | 0.358 | 0.316 | 0.042 |
| fiqa | 0.003 | 0.001 | 0.001 | 0.000 | 0.000 |

Frame ratio at 64 KiB / zstd 19 / 112 KiB dict (64 MiB samples), without →
with 1 KiB dedupe: swe 0.0830 → 0.0554 (−33%), ohjson 0.1160 → 0.1118 (−4%),
othink 0.2054 → 0.2050, hermes 0.1949 → 0.0844 (−57%), ohsft 0.1509 → 0.1199
(−21%), fiqa 0.3352 → 0.3349. About 1.0 frame decodes per excerpt fetch.
**Kept.** Persisting the chunk table would cost +0.005 (2 KiB chunks) to +0.026
(256 B); rebuilding it from the window at compaction costs nothing on disk.

**E5 — cold-tier parameter sweep** (64 MiB samples, ratio; base = 64 KiB
frames, 1 KiB chunks, zstd 19, 112 KiB dict):

| corpus | base | L9 | L15 | L22 | 16K | 32K | 128K | 256K | chunk 512 | 2048 | 4096 | no dict | 32K dict | 512K dict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| swe | 0.0554 | 0.0591 | 0.0557 | 0.0554 | 0.0671 | 0.0606 | 0.0514 | 0.0481 | 0.0537 | 0.0575 | 0.0587 | 0.0700 | 0.0605 | 0.0529 |
| ohjson | 0.1118 | 0.1178 | 0.1121 | 0.1113 | 0.1255 | 0.1180 | 0.1066 | 0.1030 | 0.1115 | 0.1122 | 0.1125 | 0.1351 | 0.1190 | 0.1072 |
| othink | 0.2050 | 0.2237 | 0.2063 | 0.2050 | 0.2089 | 0.2070 | 0.2042 | 0.2042 | 0.2051 | 0.2049 | 0.2049 | 0.2312 | 0.2136 | 0.2004 |
| hermes | 0.0844 | 0.0899 | 0.0849 | 0.0843 | 0.0934 | 0.0886 | 0.0812 | 0.0786 | 0.0842 | 0.0863 | 0.0897 | 0.0964 | 0.0878 | 0.0862 |
| ohsft | 0.1199 | 0.1270 | 0.1202 | 0.1194 | 0.1280 | 0.1237 | 0.1171 | 0.1145 | 0.1189 | 0.1220 | 0.1238 | 0.1422 | 0.1272 | 0.1174 |
| fiqa | 0.3349 | 0.3603 | 0.3367 | 0.3349 | 0.3388 | 0.3363 | 0.3343 | 0.3331 | 0.3348 | 0.3350 | 0.3351 | 0.3884 | 0.3547 | 0.3183 |

zstd 15 is within 0.5% of 19 at ~1.6x the compaction speed; L22 gains nothing;
256 KiB frames gain 4–13% on agent data but cost 3–4x decode per excerpt;
512 B chunks gain 1–3%; a 512 KiB dictionary gains 2–5% on the larger corpora
(and costs more than it saves on a 50 MB store). **Kept** 64 KiB / 1 KiB /
112 KiB as default; the larger settings go to an archive profile.

**E6 — P-frames: a frame may compress against the raw body of the earlier
I-frame it references most** (zstd prefix instead of the dictionary, kept when
≥ 3% smaller; P-frames are never prefixes, so a read decodes at most one extra
frame): swe 0.0554 → 0.0515 (−7.0%), hermes 0.0844 → 0.0818 (−3.1%), ohsft
0.1199 → 0.1177 (−1.8%), ohjson 0.1118 → 0.1099 (−1.7%), othink unchanged.
Decodes per fetch 1.0 → 1.1–1.3; top-20 p99 (unlocked) 2.1 → 2.6 ms on swe.
**Candidate for the archive profile**; not in format v1 (`dedupe-exp PREFIX=sim`).

**E7 — tokenizer pre-pass** (o200k BPE ids as varints, no model, then a codec;
whole 64 MiB sample):

| corpus | zstd 19 | tok + zstd 19 | xz 9e | tok + xz 9e | ppmd8 o8 | tok + ppmd8 o8 |
|---|---|---|---|---|---|---|
| ohsft | 0.0884 | 0.0854 | 0.0732 | 0.0787 | 0.0918 | 0.0755 |
| hermes | 0.0733 | 0.0600 | 0.0576 | 0.0562 | 0.0930 | 0.0553 |

Helps weak-context coders (zstd, PPMd) on JSON-heavy text, hurts xz on code
logs; decode drops ~6x (detokenize); depends on an external vocabulary file.
**Rejected for the store**; an archive idea (`codecs whole ... tok:<codec>`).

**E8 — compressed self-index** (fm-index 0.3 crate, 8 MiB prefix): plain
FM count-only 1.05x raw, with locate (sample 2^6) 1.10x; run-length FM
0.31–0.54x (count) / 0.36–0.58x (locate). Not competitive as storage with
zstd + dedupe (0.05–0.2) plus a separate index. **Rejected** for storage; a
run-length index may still interest the lexical track for substring search.

**E9 — JSON-unescape transform for tool calls** (nested `arguments` JSON
unescaped two levels where canonical re-serialization reproduces the bytes):
all 17,218 ohjson tool calls qualify; whole-sample zstd 19 0.0859 → 0.0842
(−2.0%). **Rejected for now** (small, kind-specific, adds format complexity);
revisit with the toolcall track's field structure
(`scripts/compress/unescape_probe.py`).

**E10 — strong text coder for store-only thinking** (othink, kind-grouped
frames, no dictionary, thinking share):

| frame | zstd 19 | xz 6 | ppmd8 o6 | ppmd8 o8 | fetch p50 / p99 (ppmd8 o8) |
|---|---|---|---|---|---|
| 256 KiB | 0.2230 | 0.2191 | 0.1860 | 0.1820 | 7.1 / 38.8 ms |
| 1 MiB | 0.2091 | 0.2051 | 0.1809 | 0.1756 | 30.5 / 217.9 ms |

PPMd o8 at 256 KiB is 13% smaller than the default (0.209) for thinking text,
with excerpt fetches 10–40x slower: a fit for **store-only** thinking (rarely
read), not for hits fetched every turn.

**E11 — P-frames inside the store** (format flag + one-hop prefix decode;
roles fixed before compression so frames still compress in parallel). With
the store's dictionaries the gain mostly vanished: swe 64 MiB, 112 KiB dict
0.0583 → 0.0571 (−2%); 512 KiB dict 0.0549 → 0.0547 (−0.4%). zstd cannot use
a prefix and a dictionary together, and the larger dictionary gains more
(−6%). **Rejected**; full feature kept as
`docs/benchmarks/compress/patches/e11-pframes-store.patch`.

**E12 — dictionary lifecycle.** Retraining at every 64 MiB compaction stored
7 dictionaries on swe (3.7% of stored bytes at 112 KiB, ~17% at 512 KiB).
Reusing the newest dictionary: swe default 0.0545 → 0.0553 (small dicts like
being fresh), swe archive (512 KiB) 0.0459 → 0.0430, ohjson 0.1060 → 0.1049.
**Kept** a budget rule: a dictionary is ~1/256 of the text it is trained on
(16 KiB–512 KiB), and is retrained once 512x its size of new text arrived, so
stored dictionaries stay near 0.2% of raw; dictionaries no live frame uses
are garbage-collected. Policy comparison, full corpora (store, 64 KiB frames; each store's
dictionary was trained on its first 64 MiB and reused for the rest, i.e. out
of sample): swe fixed 112 KiB 0.0557, adaptive ≤512 KiB 0.0537, ≤1 MiB
0.0526; ohjson 0.1051 vs 0.1007; hermes 0.0882 (112 KiB). Default set to
adaptive ≤512 KiB; the archive profile uses ≤1 MiB.

**E13 — background compaction.** v1 `compact()` held `&mut self`, stalling
appends for the whole rewrite (seconds to minutes at zstd 19). Now
`begin_compaction` (rotate + snapshot, quick) → `CompactionJob::run` (owned,
`Send`, any thread) → `finish_compaction` (publish + swap, quick); appends
continue during the job, including hot-log rotation (tested). Early unlocked
measurement on a loaded disk: append p50 4.0 ms quiet vs 7.0 ms while a
4-thread job runs (p99 33 vs 38 ms) — a running compaction's writes and CPU
roughly double median fsync latency; locked numbers and I/O throttling are
next. **Kept.**

**E14 — fail-stop after I/O errors.** A failed write or fdatasync may have
dropped dirty pages (fsyncgate), so the store refuses appends and compactions
until reopened; recovery keeps every acknowledged batch and drops only the
failed one (tested with an injected partial write). Appends are batch-atomic
(one frame per batch; a torn frame drops the whole batch), unlike the current
journal, which the shared harness found leaves fragments of a batch after
SIGKILL. **Kept.**

**E15 — large dictionaries, in-sample vs out-of-sample** (64 KiB frames,
512 B chunks, zstd 19; frames-only ratio, dictionary bytes excluded since a
large store amortizes them). In-sample on swe 64 MiB: 512 KiB 0.0442, 1 MiB
0.0398, 2 MiB 0.0349 — near whole-sample zstd 19 (0.0333). Out-of-sample
(train on the first part, measure the rest) the gain mostly disappears:
swe 64 MiB halves: none 0.0603, 112 KiB 0.0504, 512 KiB 0.0478; ohjson
0.1339 / 0.1113 / 0.1038; full swe (train on 128 MiB, measure 256 MiB):
512 KiB 0.0478, 1 MiB 0.0467 (−2.3%), 2 MiB 0.0455 (−4.8%). Large trained dictionaries memorize
their training text. **Kept** 512 KiB default / 1 MiB archive; the in-sample
2 MiB number is not a claim.

**E16 — shared read path and packer; immutable run files.** The frame read
path (partial decode, cache, one-hop references) moved to `reader.rs` and the
dedupe + parallel compress/verify packer to `pack.rs`; compaction now
streams its input twice (stride sample for the dictionary, then packing)
instead of holding a whole batch in memory. New `oojuro_store::run`:
`write_run(path, rows, opts)` / `RunReader::{open, get, get_range, scan}`, one
self-contained file per run (embedded dictionary, dedupe inside the run),
shaped like the lexical engine's `Archive`/`Run` boundary so that track can
adopt the codec for its immutable segments without either track editing the
other's internals. Tests: exact rows/ranges/scans, empty runs, identical bytes
for 1 vs 3 threads, truncation and in-frame corruption detected.

**E17 — compaction interval vs steady-state ratio** (ohsft, every batch
fsynced so background compaction keeps pace like real use, no final
compaction): compact every 4 MiB → 0.126 bytes/raw (≈ fully compacted
0.125), 16 MiB → 0.187, 64 MiB → 0.270 (nothing compacted yet). The hot
tail compresses ~2x worse than cold frames, so the engine should compact
every 4–8 MiB of hot data (`Store::hot_bytes()` exposes the trigger).

**E18 — search over compressed frames (archive tier)** — see the dedicated
section below.

**E19 — real deletion with proof.** `Store::erase(pred)` (a session, or a
time purge) rewrites the store into one new segment: erased records become
tombstones (stable sequence numbers; no text, key, attrs or times), dedupe
references are rebuilt among surviving records only, the dictionary is
retrained on surviving text only, the new state is published atomically, and
every replaced segment, hot log and dictionary is zero-filled, synced and
unlinked (also when recovering from a crash mid-erase). `Store::find_bytes`
scans raw file bytes, every decoded frame body and every dictionary. Test:
three sessions where two reference a large body first stored by the erased
session, two segments plus a hot tail; after erasing it, none of its 24-byte
windows that no surviving record shares occurs anywhere, before and after
reopen; surviving records stay byte-exact. Limits: erase is a full O(store)
rewrite today; copies outside the store's files (backups, page cache, SSD
cells not yet reclaimed after overwrite) are out of reach — per-domain
encryption keys (crypto-shredding) would cover those. **Kept.**

**E20 — erasure domains: dedupe only within a session.** If references never
cross a session, erasing a session can rewrite only its frames (or drop its
key). Cost (full corpora, store, attrs = session in both arms): ohsft 0.1214
→ 0.1484 (+22%), hermes 0.0866 → 0.1826 (+111%: every repeat there is a
shared system prompt or tool schema across conversations), swe 0.0513 →
0.0716 (+40%), ohjson 0.0987 → 0.1049 (+6%). **Option, off by default**
(`Options::domain_dedupe`); a product decision: per-session erasure
domains vs global dedupe with whole-store rewrites on erase. Middle ground,
domains of G consecutive sessions as an agent/tenant proxy (hermes / ohsft):
global 0.0866 / 0.1214; G=256 0.1219 / 0.1279; G=16 0.1745 / 0.1455; per
session 0.1826 / 0.1484. Crypto-shredding (one key per domain) also forbids a
shared dictionary (it would hold every domain's text): per session without a
dictionary 0.243 / 0.203, G=16 without 0.231 / 0.178 — 1.7–2.8x the global
store, plus ~28 bytes of nonce + tag per frame (< 1%). swe / ohjson: global
0.0513 / 0.0987; G=256 0.0517 / 0.1035; G=16 0.0560 / 0.1042; per session
0.0716 / 0.1049; per session without a dictionary 0.1296 / 0.1718; G=16
without 0.0790 / (not run). On these two, most redundancy sits inside a
group of related sessions, so domains of ~16–256 sessions keep most of the
global ratio; hermes (shared prompts across every conversation) is the case
that needs global dedupe.

**E21 — cold-start dictionary from other public agent text** (a dictionary
shipped with the store for small stores; licensing of training data is a
product decision). Frames-only ratio, 64 KiB frames, 112 KiB dictionary:
ohsft none 0.1422, trained on swe 0.1290, on ohjson 0.1260, self-trained
(in-sample) 0.1162; hermes 0.0964 / 0.0913 / 0.0906 / 0.0822. A foreign
agent-text dictionary recovers 40–60% of the self-trained gain and costs a
store nothing if it ships in the binary — useful until a store has its own
(≥ 4 MiB). **Candidate**, pending a licensing decision on the training text.

## Search over compressed frames (archive tier, E18)

Question: can the archive be searched while compressed, the way
vector search works over compressed vectors? Two arms share the tokenizer
(lowercase alphanumeric runs) and BM25 (k1 1.2, b 0.75); only the index
differs (`oojuro_store::search`, harness arms `archive-rec` /
`archive-coarse`, originals in oojuro-store for both):

* **record-level**: term -> (record, tf) postings;
* **coarse**: term -> (frame, quantized max BM25 weight in the frame). Query
  bounds per frame = sum of idf x max weight (scaled by any growth of the
  average length since indexing, so they stay valid). Frames are decoded and
  scored exactly in bound order until the k-th score beats the next bound —
  exact top-k (tested) — or until a frame cap (approximate).
  `nobound` scores every candidate frame: what a presence-only per-frame
  filter (Bloom / xor / binary fuse) must do even with no false positives.

**Quality and latency, harness turn loop** (swe-agent dev, 30 trajectories,
50k background records, 3 runs under the bench lock, machine load ~60; store
compacts every 512 KiB so most of each trajectory's own earlier results are
in cold frames when queried):

| arm | nDCG@20 | recall@20 | hit@20 | search p50 / p99 ms | gates |
|---|---|---|---|---|---|
| record-level, full turn input | 0.219 | 0.378 | 0.401 | 43 / 154 | all pass |
| coarse exact, full turn input | 0.219 | 0.378 | 0.401 | 330 / 846 | all pass |
| record-level, 32 highest-idf terms | 0.297 | 0.526 | 0.556 | 15 / 87 | all pass |
| coarse exact, 32 terms | 0.297 | 0.526 | 0.556 | 77 / 634 | all pass |
| coarse, 32 terms, ≤ 16 frames scored | 0.305 | 0.538 | 0.568 | 18.5 / 74 | determinism failed: background compactions finished at timing-dependent points |

After the fix (a compaction is finished at the next batch, so the hot/cold
boundary depends only on the data), all gates pass for every arm
(run without the lock: latency indicative only):

| arm (32-term queries) | nDCG@20 | recall@20 | search p50 ms (indicative) | gates |
|---|---|---|---|---|
| record-level | 0.297 | 0.526 | 7.0 | all pass |
| coarse, ≤ 8 frames scored | **0.313** | **0.554** | 4.1 | all pass |
| coarse, ≤ 16 frames | 0.309 | 0.539 | 8.2 | all pass |
| coarse, ≤ 32 frames | 0.305 | 0.539 | 15.6 | all pass |

Scoring only the best few frames by bound is *better* than exact BM25 here:
the frame bound acts as a cluster-level prior (frames that concentrate a
session's related records win; scattered distractors are never scored).
Caveats: swe dev split only, lexical labels, held-out not run yet; timed
numbers go to the bench box.

Store bytes on disk are the same for all arms (0.110–0.113 of raw; this
run's store uses an 8 MiB dedupe window for its many small compactions).

**Index size and unlabelled corpora** (ohsft, 31 MiB, 292 cold frames,
unlocked): encoded index bytes / raw — record-level 0.123, coarse 0.050
(−59%), 9-bit per-frame filters 0.018; store + index 0.267 vs 0.194.
Latency: record-level 14 ms, coarse exact 278 ms (195 of 292 frames scored),
coarse 32 terms 105 ms, filter emulation 364 ms. The four-corpus sweep got
as far as swe (full corpus, 64 KiB frames, 64 MiB frame cache, 199 turn
inputs, unlocked, so indicative): record-level p50 / p99 32 / 41 ms, coarse
exact 32 / 936 ms, presence-only (`nobound`) 678 / 1,306 ms; 32-term
queries: record-level 12.8 / 61.6 ms, coarse exact 7.2 / 177 ms, coarse
≤ 16 frames 7.3 / 12.4 ms (top-20 overlap with exact 0.94). ohjson, hermes,
othink and the frame-size sweep were not run; search over compressed frames
is not on v1's path (see the summary).

Reading:
1. Exact search over compressed frames works and keeps quality identical, but
   costs 5–8x the latency of a record-level index on long agent queries: frame
   bounds sum maxima from different records, so most candidate frames must be
   decoded, and decoding a frame resolves its dedupe references.
2. A frame cap brings latency to record-level speed; on this workload it even
   kept quality (0.305 vs 0.297 nDCG@20), but it is approximate, and the cap's
   behavior on larger archives is not measured yet.
3. Presence-only filters are 3x smaller than frame postings but cannot rank:
   with OR queries nearly every frame is a candidate.
4. Unexpected ranking result: reducing long turn inputs to their 32
   highest-idf terms raised nDCG@20 by 36% and recall@20 by 39% for plain
   BM25 (a lexical-track finding, passed on).
5. CLP-style search: clp-s searches its own archives in place (substring /
   wildcard over its template and variable dictionaries; match counts verified
   on 50/50 identifier queries); its latency was not measured (the shared
   lock never freed up for it; not repeated in the closing pass).

## Thinking tokens: full index vs store only

Stored (kind-grouped 64 KiB frames, zstd 19, per-kind dictionary), bytes per
raw thinking byte: othink 0.209, swe 0.089, ohjson 0.206, ohsft 0.191; with
PPMd archive frames (E10) ~0.18 on othink.

Index cost proxy, Tantivy 0.27 index only (no doc store), thinking records vs
everything else:

| corpus | thinking, positional | other kinds, positional | thinking, freqs only | other kinds, freqs only |
|---|---|---|---|---|
| othink | 0.2979 | 0.2808 | 0.0245 | 0.0887 |
| swe | 0.3248 | 0.2755 | 0.1646 | 0.1015 |
| ohjson | 0.3935 | 0.2267 | 0.2075 | 0.0862 |
| ohsft | 0.3637 | 0.2772 | 0.1902 | 0.0934 |

Reading: indexing thinking with positions costs 1.4–3.6x the bytes of storing
it; a freqs-only index costs 0.1–1.0x. Long thinking traces (othink, ~26 KB per
record) are far cheaper to index per byte than short per-turn thoughts
(swe/ohjson/ohsft, 250–480 B). These are proxies; the lexical engine's own
index sizes will differ.

In v1's own engine (nine) the question looks different: nine stores **no
positions** (postings are row + tf), so the "freqs-only" saving above is
already banked, and on the `devlogs` journal thinking is 0.2% of the index
(0.6% counting its rows). See the next section.

## Search index: where the bytes go and what would shrink it (2026-10-21)

The index is about a third of the `devlogs` live journal (~107 MB of 326 MB on
disk). The live store is off limits, so the journal was exported (`oojuro
export --from 60d`, 546,835 records, 1,135 MB of text) and replayed into a
fresh store with the recorder's own options and indexing rule
(`crates/oojuro/examples/replay_export.rs`: `store_options()`,
`kind::indexed`, records marked `oojuro` stored only, id order, one batch per
writer + session). For comparison: TraceBench base1m tier 1 (the search
probe's store, `searchprobe build --frac 0.1`). Sizes are exact; timed runs
held the bench lock in short holds at load 15–25 (indicative); quality runs
used `oojuro-bench --no-lock`, one run each (deterministic; a BEIR rerun
reproduced the numbers exactly). Tools: `crates/oojuro-nine/examples/index_probe.rs`
(section sizes, postings split, alternative encodings with a decode bench,
dictionary layouts, df and kind attribution), the `nine-maxdf` bench arm,
`searchprobe --max-df`. Nothing here changes the engine or its format.

**Rebuild vs live** (MB; `TraceDb::disk_breakdown`):

| state | index | archive | journal | columns | total |
|---|---:|---:|---:|---:|---:|
| rebuild, before settling merges | 116.9 | 246.6 | 67.1 | 3.4 | 434.0 |
| rebuild, after settling (6 merges) | 109.7 | 199.7 | 67.1 (33.6 after close) | 3.4 | 379.9 |
| live (measured on the running store) | ~107 | | | | 326 |

The index lands within 2.5% of live. The archive depends on merge history
(247 → 200 MB from settling alone), which explains most of the total gap.

**Finding: record dedupe never fires on the recorder path.** The dedupe key
is (indexing class, tool, args, text), and every captured record's args
carry its own source position and receive time (`src`, `rt`), so no two are
equal: 0 duplicates in 546,835 records. With `src`/`rt` left out of the key,
67,031 records (12.3%) dedupe: index 109.7 → 102.5 MB (−6.5%), archive
199.7 → 192.6 MB. (PERF_STORE's P1 numbers came from `logs_to_tbe.py`
traces, whose args carry no provenance; the live path never got the win.)

**Where the bytes go** (MB, % of index):

| part | devlogs (546,835 rows, 390,372 indexed; 39.9M postings; 1.11M terms) | base1m tier 1 (792,118 rows; 24.0M postings) |
|---|---:|---:|
| rows (40 B/row) | 21.87 (19.9%) | 31.68 (28.2%) |
| ids (20 B/row) | 10.94 (10.0%) | 15.84 (14.1%) |
| keys (20 B/row) | 10.94 (10.0%) | 15.84 (14.1%) |
| time order (4 B/row) | 2.19 (2.0%) | 3.17 (2.8%) |
| **fixed per-row tables** | **45.93 (41.9%)** | **66.54 (59.2%)** |
| postings: doc ids (Rice) | 34.81 (31.7%) | 23.62 (21.0%) |
| postings: tf (Elias gamma) | 11.67 (10.6%) | 6.50 (5.8%) |
| dictionary: term strings (front-coded) | 12.95 (11.8%) | 10.38 (9.2%) |
| dictionary: df / cf / postings length | 3.45 (3.1%) | 4.38 (3.9%) |
| dictionary block offsets + dindex | 0.39 | 0.49 |
| positions, skip tables on disk | none | none |
| columns (TraceDb checkpoint; not the index) | 3.4 | 12.6 |

Terms with df = 1 are 57% of the devlogs terms and 71% of its dictionary
bytes (hash-like identifiers). Share of postings in terms whose df exceeds
1 / 5 / 10 / 25% of rows: 76 / 49 / 30 / 11% (base1m 74 / 49 / 32 / 13%). By
kind (devlogs, rows included): tool_result 48.8%, tool_call 31.1%, system
12.0% (156k stored-only rows: rows but no postings), assistant 4.5%, user
2.3%, thinking 0.6%.

**(a) Doc-id encodings vs Rice** (lossless: rankings identical by
construction). Decode = the probe's own decoders over every real list, under
the lock (upper bounds for an engine implementation); latency change
estimated as postings per tier-1 query (52.7k at 200 B, 283k at 4 KB) x the
decode-time difference. Tier-1 baseline CPU p50 / p99: 200 B 1.15 / 2.83 ms,
4 KB 3.18 / 5.18 ms.

| encoding | devlogs MB (bits/posting) | index Δ devlogs / base1m | decode ns/posting | est. Δ p50 at 200 B / 4 KB |
|---|---:|---:|---:|---:|
| **Rice + gamma (today)** | **46.94 (9.42)** | — | 5.45 | — |
| entropy bound | 45.15 (9.06) | −1.6% / −1.4% | | |
| Elias-Fano + gamma | 49.50 (9.94) | +2.3% / +1.6% | 6.05 | +0.03 / +0.17 ms |
| partitioned EF (128) + gamma | 49.42 (9.92) | +2.3% / +1.9% | — | — |
| PForDelta-128 | 65.12 (13.07) | +16.6% / +10.7% | 1.52 | −0.21 / −1.11 ms |
| FOR-128 (bit packing) | 70.73 (14.20) | +21.7% / +13.4% | 1.23 | −0.22 / −1.20 ms |
| Rice doc ids + FOR tf | 57.54 | +9.7% / +4.8% | 3.38 | −0.11 / −0.59 ms |
| Roaring + gamma tf | 102.56 (20.58) | +50.7% / +41.8% | 9.44 | +0.21 / +1.13 ms |

Rice is within 4% of the entropy bound; every alternative is larger. FOR or
PFor would buy ~1 ms at 4 KB for +11–22% index bytes: a latency lever, not a
size lever. **Keep Rice.**

**(b) Positions.** None are stored. A quoted query takes candidates from its
terms' postings and confirms each candidate on its original text
(`phrase_start`), which is exact; there is no proximity scoring. Adding
positions would cost ~126 MB on the devlogs store (+115% of the index; base1m
+64 MB, +57%). **Not recommended**; nothing in v1 needs them.

**(c) Thinking records** (the design decision of 2026-10-20 indexes thinking):

| corpus | thinking share of index | thinking stored-only (measured) | drop tf for thinking (est.) |
|---|---:|---:|---:|
| devlogs | 0.2% (0.6% with rows) | −0.23 MB (est.) | −0.04 MB |
| othink sample (82% thinking) | 48.8% | 2.03 → 1.18 MB (−42%) | −0.41 MB |
| swe sample (14% thinking) | 11.4% | 7.60 → 5.81 MB (−23.5%)* | −0.22 MB |

\* includes 11,320 thinking repeats that dedupe only once thinking is
stored-only (TraceDb's dedupe classes split on the indexing flag).
Quality of stored-only thinking (turnloop swe-agent, 150 trajectories):
nDCG@10 dev .2627 → .2656, heldout .1590 → .1647, recall@20 +0.009 /
+0.012. The turnloop labels never mark thinking relevant, so this setup can
only reward unindexing; the cost of losing an agent's own reasoning from
search is unmeasured. Dropping tf for thinking: not measured, not
recommended. **Keep the decision**: on real journals thinking is 0.2% of the
index.

**(d) Term dictionary** (devlogs today 16.78 MB = 15.3% of the index):

| layout | devlogs MB | Δ devlogs / base1m |
|---|---:|---:|
| front-coded, 32-term blocks, df/cf/len inline (today) | 16.78 | — |
| 128-term blocks | 16.35 | −2.6% / −3.4% |
| drop `cf` (written, never read by search) | 15.63 | −6.9% / −9.7% |
| front-coded 128-term blocks + df gamma column + Elias-Fano offsets (no cf, no len) | 14.33 | −14.6% / −20.8% |
| FST (term → ordinal) + df column + offsets | 16.66 | −0.8% / −6.4% |

An FST loses to front coding here: the df = 1 hash-like terms share no
prefixes or suffixes. Lookup speed of larger blocks not timed.

**(e) Fixed per-row tables** (PERF_STORE "Next #1"; lossless):

| table | devlogs today → lean MB | base1m today → lean MB | how |
|---|---|---|---|
| rows | 21.87 → 5.35 | 31.68 → 8.40 | bit-pack each field per 128-row block (FOR-128); recorded_at equals valid_at in every row (store a flag) |
| ids | 10.94 → 0 | 15.84 → 0.10 | TraceDb ids are decimal and ascend with rows: binary-search the id column, list exceptions (64-bit fingerprint + row instead: 5.80 / 7.92) |
| keys | 10.94 → 0 | 15.84 → 5.74 | store only rows whose key differs from the id (devlogs 0; base1m 286,888 status revisions) |
| time | 2.19 → 0 | 3.17 → 0 | rebuild at open by sorting valid_at (or runs: 1.13 / 0.11) |
| **total** | **45.93 → 5.35 (−37.0% of index)** | **66.54 → 14.23 (−46.5%)** | |

With 64-bit id fingerprints instead of numeric ids and time as runs, the
devlogs saving is −30.6%. Read cost not timed (row metadata is read only for
finalists, PERF_SEARCH P3).

**(f) Stopping very frequent terms** (an index that drops terms above a df
threshold but keeps row lengths, simulated exactly on the query side by the
`nine-maxdf` arm; bytes are the dropped lists + dictionary entries):

| df above | devlogs bytes saved | tier-1 CPU p50, 200 B / 4 KB | turnloop nDCG@10 dev / heldout | turnloop recall@20 dev / heldout | SciFact nDCG@10 dev / heldout | NFCorpus | FiQA |
|---|---:|---:|---:|---:|---:|---:|---:|
| none | — | 1.15 / 3.18 ms | .2627 / .1590 | .3659 / .3391 | .677 / .707 | .293 / .339 | .288 / .235 |
| 25% | 3.5 MB (3.2%) | 0.40 / 2.50 ms | .2600 / .1578 | .3509 / .3349 | .674 / .708 | .292 / .335 | .287 / .237 |
| 10% | 10.9 MB (9.9%) | 0.21 / 1.21 ms | .2659 / .1631 | .3671 / .3459 | .658 / .675 | .288 / .330 | .256 / .214 |
| 5% | 18.5 MB (16.8%) | 0.15 / 0.75 ms | .2573 / .1713 | .3570 / .3673 | .609 / .652 | .273 / .318 | .210 / .163 |

(BEIR columns dev / heldout; base1m saves 2.3 / 6.3 / 10.3%.) 5% and 10% cost
up to 27% nDCG@10 on FiQA and 10% on SciFact: **rejected** as an index
change. 25% keeps BEIR within ±0.004 but costs turnloop recall@20 −0.015
on dev, for 3% of the bytes: **rejected** as a size lever. Its real value is
latency (2–3x faster tier-1 queries), which a query-side cutoff gets with no
format change; that changes rankings, so it is a ranking decision, measured here.

**What to adopt for the index, in order** (devlogs: 1.135 GB raw, index today
96.6 MB per GB of raw text):

| # | change | MB saved per GB raw | cost |
|---|---|---:|---|
| 1 | keys table only where key ≠ id | 9.6 | trivial, segment format bump |
| 2 | no id table for numeric ids (fingerprint fallback saves 4.5) | 9.6 | small |
| 3 | rows bit-packed per 128-row block, recorded_at flag | 14.6 | segment format v3 |
| 4 | time-order table rebuilt at open | 1.9 | a sort at open |
| 5 | dictionary: drop the unused `cf`, then df/offset columns with 128-term blocks | 2.2 | time lookups first |
| 6 | capture: dedupe key without `src`/`rt` (keep each repeat's own args in its `B` entry) | 6.3 index + 6.2 originals | capture change; erasure tests |

Items 1–5 are lossless and take the devlogs index from 109.7 to ~66.7 MB
(−39%); with 6, ~60 MB. Doc-id encodings, positions, thinking policy, FST and
term stopping are not worth adopting.

## Codec leaderboard (all corpora, 2026-10-21)

Every codec the track planned, on all six public samples plus the devlogs
journal sample, whole-sample and as random-access blocks; external codecs
(context mixing, BWT, OpenZL, Re-Pair) on every corpus; Oojuro's own run
format as its own rows. Runner: `scripts/compress/leaderboard2.sh` (in-process,
`codecs` binary), `scripts/compress/external/run_external.sh` (external),
tables from `scripts/compress/report2.py`; aggregates (no text) in
`docs/benchmarks/compress/leaderboard-v2.json`.

Method:
- **Samples**: the 64 MiB append-order samples (hermes, ohsft and fiqa are
  smaller than 64 MiB and run whole) and the devlogs sample. Every row decodes
  and compares every byte.
- **Sizes** come from an unlocked pass over the full samples (exact).
  **Speeds and fetch latencies** come from separate passes under the bench
  lock, one hold per codec (whole sample) or per (corpus, block size,
  dictionary) group: whole sample on othink and devlogs plus the track's
  locked swe and ohjson runs; blocks on 16 MiB prefixes of swe, othink and
  devlogs; run profiles on the full 64 MiB of the same three. Tables show the
  median over the timed corpora; single thread; the 1-minute load average
  (12–48 during this pass, from other sessions) is recorded with every row.
- **Fetch** = one cold 1 KiB excerpt at a random offset of a random record,
  200 x 20 per configuration, no decode cache. zstd, xz, brotli, bzip2 and
  PPMd stop decoding at the excerpt; LZ4, snappy, deflate and FSST decode
  the whole block.
- **Dictionary** ("+dict"): a 112 KiB zstd dictionary trained on the sample
  (in sample, as the store trains at compaction), counted in the size. LZ4
  uses its last 64 KiB, brotli its content as a custom dictionary (brotli's
  `-D`). deflate, xz, bzip2, snappy and PPMd are run without one (no
  dictionary in the libraries' APIs as used); FSST's symbol table is its own
  dictionary. Each block also pays 16 B of index.
- **Oojuro rows** (`codecs run`, one v1 run file per sample, read through
  `RunReader`): *flush* = what small stores hold (zstd 3, shipped starter
  dictionary), *default* = merged / archive-tier runs today (zstd 19,
  trained dictionary ≤ 512 KiB at ~1/256 of the text, 64 KiB frames, 1 KiB
  FastCDC dedupe), *archive* = the track's archive profile (256 KiB frames,
  512 B chunks, 512 KiB dictionary) without P-frames, which the run format
  does not have (simulated separately below). Run compress speed includes
  dedupe and dictionary training; the scan uses the 4 MiB frame cache
  TraceDb uses; fetches are cold.
- **Not run**: brotli + dictionary at quality 9/11 on ohjson and devlogs at
  4–16 KiB and per record (the Rust brotli 8.0.4 encoder panics with a custom
  dictionary on those inputs: index out of bounds in `brotli_bit_stream.rs`);
  paq8px and Re-Pair use 8 MiB prefixes (paq8px runs at ~5 KB/s). **cmix:
  skipped (2026-10-21): it needs ~32 GB of RAM and runs at
  ~2 KB/s, so it cannot be used in the product; paq8px already gives the
  floor.**

**Headline.**
- **Per record and 4 KiB blocks: zstd + trained dictionary wins on every
  corpus** (level 19/22; devlogs 0.277 per record, 0.263 at 4 KiB). Brotli 11
  with a custom dictionary comes within 0.1–7% where it ran, at 3–20x slower
  fetches and 4–25x slower compression. FSST per record stays at 0.54–0.68:
  its symbol table cannot see repeats across records.
- **16 KiB: zstd 22 + dictionary** on five corpora; brotli 11 + dictionary
  is 1% smaller on swe and ohsft at 0.7 MB/s compress and 3.5x the fetch
  time.
- **64 KiB: Oojuro's default run beats every codec on six of seven
  corpora**, because it dedupes 1 KiB chunks across the run before
  compressing: vs zstd 19 + dictionary at the same block size, swe −35%,
  hermes −57%, devlogs −27%, ohsft −20%, ohjson −6%, othink −2%; fiqa (BEIR
  prose, no repeats) is the exception, where PPMd o6 wins by 3%.
- **256 KiB: the archive profile** wins five of seven; PPMd o6 wins on
  othink and fiqa (prose and thinking) at ~30x the fetch time.
- **Whole sample (no random access)**: context mixing sets the floor (zpaq
  -m5 on all, paq8px on 8 MiB prefixes, devlogs 0.0845 / 0.0757); the best
  in-process codec is PPMd8 order 16 with 1 GiB of model on six corpora
  (devlogs 0.1004) and xz -9e on swe; BWT (libbsc -b64) beats xz -9e on six
  of seven (hermes +3%) at 20–40 MB/s each way.
- **Where we stand** (devlogs sample): flush runs 0.214, merged runs 0.171,
  archive profile 0.153; the same data as one stream is 0.124 at zstd 19,
  0.111 at xz -9e, 0.110 with bsc, 0.0845 with zpaq -m5. v1's random-access
  default costs 1.38x a whole-sample zstd 19 and 2.0x the context-mixing
  floor, and answers any 1 KiB excerpt in ~0.08 ms.

P-frames on top of the archive profile (`dedupe-exp PREFIX=sim`, a frame
may compress against the raw body of the earlier frame it references most):
swe −1.2%, hermes −1.5%, devlogs −1.3%, ohjson / othink / ohsft / fiqa 0.0%,
with 4–14% more frame decodes per fetch. With 512 KiB dictionaries they buy
almost nothing (E11 found the same): **rejected**.

### Best codec per block size and corpus (bytes / raw byte)

| block | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs |
|---|---|---|---|---|---|---|---|
| one record per block | 0.2236 zstd:19 +dict | 0.1795 zstd:19 +dict | 0.2146 zstd:19 +dict | 0.2795 zstd:19 +dict | 0.2079 zstd:19 +dict | 0.3858 zstd:19 +dict | 0.2771 zstd:19 +dict |
| 4 KiB | 0.1686 zstd:22 +dict | 0.1507 zstd:22 +dict | 0.2135 zstd:22 +dict | 0.2258 zstd:22 +dict | 0.1777 zstd:22 +dict | 0.3449 zstd:22 +dict | 0.2629 zstd:22 +dict |
| 16 KiB | 0.1201 brotli:11:24 +dict | 0.1310 zstd:22 +dict | 0.2107 zstd:22 +dict | 0.2083 zstd:22 +dict | 0.1596 brotli:11:24 +dict | 0.3342 zstd:22 +dict | 0.2507 zstd:22 +dict |
| 64 KiB | 0.0534 **oojuro** run, default profile | 0.1076 **oojuro** run, default profile | 0.2013 **oojuro** run, default profile | 0.0841 **oojuro** run, default profile | 0.1199 **oojuro** run, default profile | 0.3165 ppmd8:6:16 | 0.1708 **oojuro** run, default profile |
| 256 KiB | 0.0473 **oojuro** run, archive profile | 0.0991 **oojuro** run, archive profile | 0.1847 ppmd8:6:16 | 0.0788 **oojuro** run, archive profile | 0.1102 **oojuro** run, archive profile | 0.2796 ppmd8:6:16 | 0.1530 **oojuro** run, archive profile |

### Oojuro's own run format on the same samples (bytes / raw byte; one thread)

| profile | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | scan MB/s | 1 KiB fetch p50 / p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|
| flush | 0.0717 | 0.1439 | 0.2658 | 0.1076 | 0.1518 | 0.4154 | 0.2138 | 108 | 636 | 91.7 / 236 |
| default | 0.0534 | 0.1076 | 0.2013 | 0.0841 | 0.1199 | 0.3279 | 0.1708 | 3.28 | 588 | 83.2 / 216 |
| archive | 0.0473 | 0.0991 | 0.1985 | 0.0788 | 0.1102 | 0.3185 | 0.1530 | 2.96 | 383 | 240 / 624 |
| cold4 | 0.0408 | 0.0955 | 0.1967 | 0.0724 | 0.1059 | 0.3189 | 0.1385 | 6.05 | 14.7 | 1,509 / 3,219 |
| cold16 | 0.0404 | 0.0953 | 0.1968 | 0.0756 | 0.1037 | 0.3179 | 0.1354 | 6.06 | 5.74 | 5,710 / 14,152 |

### Whole-sample leaderboard (64 MiB samples; bytes / raw byte; best per corpus in bold)

Speeds: single-thread MB/s, median over the locked runs (swe and ohjson from the track's locked pass, othink and devlogs from this one); external codecs as measured by `run_external.sh` (unlocked, †).

| codec | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | decompress MB/s |
|---|---|---|---|---|---|---|---|---|---|
| ppmd8:16:1024 | 0.0351 | 0.0703 | 0.1494 | 0.0544 | 0.0700 | 0.2077 | 0.1004 | 4.89 | 5.00 |
| tok:ppmd8:8:256 | 0.0341 | 0.0754 | 0.1536 | 0.0553 | 0.0755 | 0.2109 | 0.1094 | 4.44 | 26.8 |
| ppmd8:12:1024 | 0.0426 | 0.0736 | 0.1514 | 0.0637 | 0.0756 | 0.2080 | 0.1080 | 26.7 | 25.1 |
| xz:9e | 0.0307 | 0.0781 | 0.1712 | 0.0576 | 0.0732 | 0.2596 | 0.1106 | 1.26 | 153 |
| brotli:11:30 | 0.0308 | 0.0778 | 0.1726 | 0.0583 | 0.0735 | 0.2606 | 0.1100 | 2.36 | 716 |
| tok:xz:9e | 0.0318 | 0.0844 | 0.1687 | 0.0562 | 0.0787 | 0.2483 | 0.1160 | 2.01 | 101 |
| xz:9 | 0.0315 | 0.0799 | 0.1714 | 0.0590 | 0.0754 | 0.2596 | 0.1113 | 8.59 | 613 |
| zstd:22:w27:ldm | 0.0321 | 0.0813 | 0.1743 | 0.0589 | 0.0770 | 0.2619 | 0.1138 | 2.99 | 1,383 |
| zstd:22 | 0.0321 | 0.0813 | 0.1743 | 0.0589 | 0.0771 | 0.2619 | 0.1138 | 1.67 | 883 |
| brotli:11:24 | 0.0312 | 0.0803 | 0.1753 | 0.0616 | 0.0745 | 0.2652 | 0.1159 | 0.631 | 341 |
| zstd:19:w27:ldm | 0.0328 | 0.0831 | 0.1781 | 0.0598 | 0.0781 | 0.2677 | 0.1160 | 2.41 | 750 |
| tok:zstd:19 | 0.0335 | 0.0895 | 0.1759 | 0.0600 | 0.0854 | 0.2557 | 0.1264 | 2.65 | 101 |
| ppmd7:8:256 | 0.0597 | 0.0862 | 0.1600 | 0.0928 | 0.0917 | 0.2115 | 0.1356 | 7.74 | 8.06 |
| ppmd8:8:256 | 0.0599 | 0.0862 | 0.1599 | 0.0930 | 0.0918 | 0.2114 | 0.1357 | 7.42 | 6.92 |
| xz:6 | 0.0323 | 0.0837 | 0.1769 | 0.0728 | 0.0855 | 0.2705 | 0.1208 | 10.3 | 837 |
| zstd:19 | 0.0333 | 0.0859 | 0.1799 | 0.0733 | 0.0884 | 0.2731 | 0.1243 | 3.79 | 901 |
| brotli:9:30 | 0.0358 | 0.0895 | 0.2041 | 0.0650 | 0.0860 | 0.3076 | 0.1250 | 22.9 | 658 |
| brotli:9:24 | 0.0358 | 0.0908 | 0.2042 | 0.0681 | 0.0868 | 0.3092 | 0.1291 | 4.40 | 341 |
| zstd:15 | 0.0363 | 0.0942 | 0.2004 | 0.0813 | 0.1040 | 0.2991 | 0.1377 | 5.85 | 1,399 |
| ppmd7:6:256 | 0.0777 | 0.1021 | 0.1712 | 0.1214 | 0.1114 | 0.2199 | 0.1647 | 43.6 | 36.7 |
| ppmd8:6:256 | 0.0778 | 0.1021 | 0.1713 | 0.1215 | 0.1115 | 0.2200 | 0.1648 | 35.3 | 37.3 |
| zstd:12 | 0.0376 | 0.0964 | 0.2090 | 0.0850 | 0.1078 | 0.3142 | 0.1447 | 107 | 1,602 |
| brotli:5:24 | 0.0393 | 0.0974 | 0.2187 | 0.0835 | 0.1026 | 0.3366 | 0.1464 | 157 | 1,049 |
| zstd:9 | 0.0389 | 0.0983 | 0.2146 | 0.0879 | 0.1107 | 0.3270 | 0.1474 | 84.6 | 1,050 |
| zstd:6 | 0.0421 | 0.1032 | 0.2235 | 0.1002 | 0.1232 | 0.3428 | 0.1643 | 309 | 1,702 |
| bzip2:9 | 0.0591 | 0.1114 | 0.1928 | 0.1558 | 0.1343 | 0.2858 | 0.2068 | 15.4 | 27.1 |
| zstd:3 | 0.0502 | 0.1182 | 0.2537 | 0.1229 | 0.1415 | 0.3672 | 0.1846 | 442 | 1,181 |
| zstd:1 | 0.0642 | 0.1402 | 0.2766 | 0.2035 | 0.1782 | 0.4239 | 0.2631 | 567 | 1,088 |
| deflate:12 | 0.1208 | 0.1647 | 0.2322 | 0.2249 | 0.1976 | 0.3688 | 0.2712 | 7.36 | 1,056 |
| deflate:6 | 0.1288 | 0.1724 | 0.2475 | 0.2365 | 0.2078 | 0.3875 | 0.2824 | 144 | 1,092 |
| lz4hc:12 | 0.1106 | 0.1934 | 0.2801 | 0.2599 | 0.2141 | 0.4356 | 0.3197 | 17.9 | 1,533 |
| lz4hc:9 | 0.1114 | 0.1946 | 0.2839 | 0.2622 | 0.2160 | 0.4399 | 0.3217 | 79.6 | 1,816 |
| deflate:1 | 0.1490 | 0.1901 | 0.2853 | 0.2634 | 0.2273 | 0.4191 | 0.3069 | 389 | 951 |
| zstd:-5 | 0.0943 | 0.2075 | 0.3864 | 0.3203 | 0.2700 | 0.6828 | 0.4111 | 1,300 | 1,590 |
| lz4 | 0.1712 | 0.2600 | 0.4064 | 0.3705 | 0.2986 | 0.6301 | 0.4380 | 475 | 1,266 |
| snappy | 0.2422 | 0.2867 | 0.4203 | 0.3980 | 0.3290 | 0.6193 | 0.4541 | 541 | 981 |
| brotli:1:24 | 0.3845 | 0.3136 | 0.4271 | 0.4238 | 0.3545 | 0.5267 | 0.4554 | 151 | 233 |
| paq8px v217 -8 (first 8 MiB) | 0.0181 | 0.0443 | 0.1093 | 0.0528 | 0.0355 | 0.1997 | 0.0757 | 0.005 † | 0.003 † |
| zpaq 7.15 -method 5 | **0.0230** | **0.0575** | **0.1277** | **0.0441** | **0.0546** | **0.2054** | **0.0845** | 0.208 † | 0.205 † |
| libbsc 3.3.12 -b64e2 | 0.0292 | 0.0720 | 0.1510 | 0.0596 | 0.0724 | 0.2057 | 0.1100 | 20.4 † | 30.1 † |
| libbsc 3.3.12 -b64 | 0.0295 | 0.0725 | 0.1518 | 0.0601 | 0.0730 | 0.2070 | 0.1108 | 22.2 † | 40.9 † |
| zpaq 7.15 -method 3 | 0.0374 | 0.0784 | 0.1550 | 0.0636 | 0.0778 | 0.2093 | 0.1201 | 3.28 † | 2.97 † |
| libbsc 3.3.12 -b16e2 | 0.0313 | 0.0778 | 0.1583 | 0.0715 | 0.0815 | 0.2179 | 0.1245 | 20.1 † | 25.8 † |
| libbsc 3.3.12 -b16 | 0.0316 | 0.0785 | 0.1592 | 0.0723 | 0.0823 | 0.2194 | 0.1254 | 24.6 † | 40.0 † |
| OpenZL 0.3 serial -l 7 | 0.0476 | 0.1038 | 0.2215 | 0.0947 | 0.1252 | 0.3390 | 0.1569 | 39.6 † | 290 † |
| OpenZL 0.3 serial -l 6 | 0.0501 | 0.1074 | 0.2284 | 0.1000 | 0.1309 | 0.3503 | 0.1672 | 45.0 † | 270 † |
| zpaq 7.15 -method 1 | 0.0494 | 0.1178 | 0.2523 | 0.0957 | 0.1255 | 0.3825 | 0.1693 | 23.9 † | 77.3 † |
| Re-Pair (Wan) -i (first 8 MiB) | 0.0660 | 0.1601 | 0.3609 | 0.1530 | 0.1336 | 0.4972 | 0.2328 | 1.20 † | 107 † |
| Re-Pair (Navarro, no entropy coder) (first 8 MiB) | 0.1352 | 0.2547 | 0.4814 | 0.2828 | 0.2469 | 0.6077 | 0.3938 | 3.34 † | 83.3 † |
| cmix v21 | skipped (2026-10-21): needs ~32 GB RAM and runs at ~2 KB/s; paq8px gives the floor | | | | | | | | |

† single unlocked run, indicative.

### Random access, one record per block (bytes / raw byte incl. dictionary and 16 B per block; 1 KiB excerpt fetch)

| codec | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | decompress MB/s | fetch p50 µs | fetch p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| zstd:19 +dict | **0.2236** | **0.1795** | **0.2146** | **0.2795** | **0.2079** | **0.3858** | **0.2771** | 5.21 | 660 | 0.962 | 37.7 |
| brotli:11:24 +dict | 0.2262 | 0.1835 | 0.2152 | 0.2877 | 0.2085 | 0.4128 | … | 0.218 | 113 | 18.4 | 118 |
| brotli:5:22 +dict | 0.2347 | 0.1886 | 0.2377 | 0.2966 | 0.2195 | 0.4245 | … | 3.37 | 112 | 21.0 | 107 |
| zstd:3 +dict | 0.2595 | 0.2042 | 0.2671 | 0.3170 | 0.2424 | 0.4305 | 0.3150 | 211 | 756 | 0.932 | 36.0 |
| zstd:3 | 0.4772 | 0.3367 | 0.3218 | 0.4988 | 0.3834 | 0.5858 | 0.3943 | 180 | 499 | 2.29 | 50.1 |
| lz4 +dict | 0.3894 | 0.3053 | 0.4056 | 0.4670 | 0.3550 | 0.6181 | 0.4845 | 141 | 1,248 | 0.541 | 23.1 |
| lz4 | 0.6373 | 0.4539 | 0.4952 | 0.6782 | 0.5282 | 0.8339 | 0.5551 | 455 | 1,701 | 0.671 | 16.6 |
| fsst | 0.6291 | 0.5410 | 0.5822 | 0.5936 | 0.6194 | 0.5692 | 0.6851 | 268 | 1,438 | 0.651 | 18.4 |

### Random access, 4 KiB blocks (bytes / raw byte incl. dictionary and 16 B per block; 1 KiB excerpt fetch)

| codec | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | decompress MB/s | fetch p50 µs | fetch p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| zstd:22 +dict | **0.1686** | **0.1507** | **0.2135** | **0.2258** | **0.1777** | **0.3449** | **0.2629** | 3.86 | 726 | 3.95 | 36.8 |
| zstd:19 +dict | 0.1687 | 0.1512 | 0.2135 | 0.2259 | 0.1784 | 0.3449 | 0.2629 | 4.19 | 871 | 3.97 | 47.9 |
| brotli:11:24 +dict | 0.1706 | … | 0.2145 | 0.2307 | 0.1779 | 0.3558 | … | 0.450 | 166 | 26.7 | 113 |
| zstd:9 +dict | 0.1785 | 0.1579 | 0.2314 | 0.2394 | 0.1873 | 0.3669 | 0.2751 | 35.8 | 775 | 4.56 | 54.8 |
| brotli:9:22 +dict | 0.1768 | … | 0.2264 | 0.2367 | 0.1867 | 0.3659 | … | 0.436 | 239 | 22.9 | 92.6 |
| brotli:5:22 +dict | 0.1825 | 0.1611 | 0.2370 | 0.2437 | 0.1907 | 0.3774 | 0.2775 | 11.6 | 210 | 23.5 | 107 |
| zstd:3 +dict | 0.2020 | 0.1764 | 0.2660 | 0.2635 | 0.2125 | 0.3918 | 0.3008 | 238 | 774 | 4.48 | 52.5 |
| brotli:11:24 | 0.2871 | 0.2228 | 0.2473 | 0.2907 | 0.2470 | 0.3730 | 0.3004 | 0.868 | 176 | 23.7 | 146 |
| zstd:1 +dict | 0.2496 | 0.2035 | 0.2918 | 0.3063 | 0.2376 | 0.4518 | 0.3481 | 326 | 796 | 4.18 | 46.3 |
| ppmd8:6:16 | 0.3128 | 0.2388 | 0.2331 | 0.3214 | 0.2614 | 0.4281 | 0.3078 | 7.00 | 6.27 | 755 | 2,101 |
| ppmd7:6:16 | 0.3130 | 0.2390 | 0.2337 | 0.3218 | 0.2619 | 0.4287 | 0.3081 | 7.74 | 7.33 | 803 | 1,681 |
| brotli:9:22 | 0.3293 | 0.2519 | 0.2771 | 0.3435 | 0.2814 | 0.4521 | 0.3363 | 1.32 | 151 | 27.3 | 190 |
| brotli:5:22 | 0.3305 | 0.2528 | 0.2818 | 0.3441 | 0.2825 | 0.4518 | 0.3376 | 15.0 | 145 | 32.9 | 196 |
| deflate:12 | 0.3445 | 0.2632 | 0.2813 | 0.3618 | 0.2929 | 0.4775 | 0.3455 | 7.99 | 578 | 7.00 | 46.5 |
| zstd:22 | 0.3517 | 0.2671 | 0.2819 | 0.3675 | 0.2964 | 0.4865 | 0.3479 | 3.75 | 561 | 5.60 | 38.2 |
| zstd:19 | 0.3517 | 0.2672 | 0.2819 | 0.3675 | 0.2964 | 0.4865 | 0.3480 | 4.26 | 741 | 5.52 | 37.5 |
| deflate:6 | 0.3512 | 0.2679 | 0.2932 | 0.3676 | 0.2996 | 0.4831 | 0.3537 | 87.5 | 593 | 6.68 | 46.6 |
| zstd:9 | 0.3576 | 0.2711 | 0.2929 | 0.3728 | 0.3018 | 0.4916 | 0.3551 | 46.7 | 672 | 6.15 | 49.0 |
| bzip2:9 | 0.3750 | 0.2884 | 0.2715 | 0.3848 | 0.3123 | 0.4938 | 0.3614 | 10.3 | 36.2 | 152 | 619 |
| xz:6 | 0.3702 | 0.2783 | 0.2866 | 0.3847 | 0.3066 | 0.5181 | 0.3537 | 3.22 | 109 | 38.9 | 228 |
| zstd:3 | 0.3702 | 0.2813 | 0.3189 | 0.3867 | 0.3165 | 0.5022 | 0.3707 | 176 | 596 | 6.46 | 47.4 |
| zstd:1 | 0.3797 | 0.2862 | 0.3338 | 0.3939 | 0.3217 | 0.5039 | 0.3822 | 227 | 466 | 6.32 | 44.8 |
| lz4 +dict | 0.3347 | 0.2791 | 0.4049 | 0.4176 | 0.3250 | 0.6032 | 0.4722 | 245 | 1,128 | 2.45 | 22.0 |
| lz4hc:12 | 0.4814 | 0.3625 | 0.3847 | 0.5118 | 0.4102 | 0.7122 | 0.4714 | 28.1 | 2,332 | 1.26 | 10.8 |
| snappy | 0.5104 | 0.3954 | 0.4669 | 0.5493 | 0.4376 | 0.7376 | 0.5250 | 497 | 1,351 | 2.20 | 24.6 |
| lz4 | 0.5275 | 0.3974 | 0.4923 | 0.5661 | 0.4578 | 0.7558 | 0.5334 | 524 | 1,788 | 1.83 | 19.5 |
| fsst | 0.6116 | 0.5330 | 0.5818 | 0.5769 | 0.6105 | 0.5528 | 0.6805 | 291 | 2,107 | 1.92 | 17.8 |

### Random access, 16 KiB blocks (bytes / raw byte incl. dictionary and 16 B per block; 1 KiB excerpt fetch)

| codec | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | decompress MB/s | fetch p50 µs | fetch p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| zstd:22 +dict | 0.1211 | **0.1310** | **0.2107** | **0.2083** | 0.1619 | **0.3342** | **0.2507** | 4.01 | 942 | 12.5 | 36.2 |
| zstd:19 +dict | 0.1212 | 0.1316 | 0.2107 | 0.2084 | 0.1625 | 0.3342 | 0.2508 | 4.67 | 958 | 12.3 | 40.3 |
| brotli:11:24 +dict | **0.1201** | … | 0.2109 | 0.2099 | **0.1596** | 0.3363 | … | 0.686 | 331 | 43.1 | 106 |
| brotli:5:22 +dict | 0.1312 | 0.1395 | 0.2339 | 0.2241 | 0.1733 | 0.3652 | … | 19.0 | 304 | 43.3 | 102 |
| zstd:9 +dict | 0.1286 | 0.1378 | 0.2287 | 0.2207 | 0.1709 | 0.3562 | 0.2625 | 34.7 | 824 | 14.4 | 44.0 |
| brotli:9:22 +dict | 0.1268 | … | 0.2233 | 0.2178 | 0.1696 | 0.3520 | … | 1.04 | 324 | 49.2 | 92.6 |
| zstd:3 +dict | 0.1482 | 0.1561 | 0.2635 | 0.2464 | 0.1957 | 0.3838 | 0.2889 | 239 | 956 | 15.8 | 37.0 |
| brotli:11:24 | 0.1960 | 0.1836 | 0.2409 | 0.2440 | 0.2128 | 0.3487 | 0.2819 | 1.06 | 277 | 59.8 | 150 |
| ppmd8:6:16 | 0.2140 | 0.1937 | 0.2240 | 0.2591 | 0.2184 | 0.3658 | 0.2849 | 10.2 | 10.6 | 1,164 | 2,011 |
| ppmd7:6:16 | 0.2143 | 0.1939 | 0.2245 | 0.2594 | 0.2189 | 0.3662 | 0.2851 | 9.73 | 10.6 | 1,079 | 2,295 |
| zstd:1 +dict | 0.1808 | 0.1787 | 0.2884 | 0.2825 | 0.2169 | 0.4315 | 0.3330 | 281 | 784 | 12.1 | 33.2 |
| brotli:9:22 | 0.2262 | 0.2092 | 0.2695 | 0.2869 | 0.2427 | 0.4184 | 0.3163 | 2.09 | 300 | 53.3 | 119 |
| brotli:5:22 | 0.2278 | 0.2103 | 0.2743 | 0.2879 | 0.2440 | 0.4188 | 0.3177 | 23.9 | 305 | 49.9 | 124 |
| zstd:22 | 0.2332 | 0.2165 | 0.2709 | 0.2966 | 0.2489 | 0.4292 | 0.3227 | 4.16 | 868 | 14.6 | 32.7 |
| zstd:19 | 0.2332 | 0.2166 | 0.2709 | 0.2967 | 0.2489 | 0.4292 | 0.3227 | 4.40 | 992 | 13.7 | 29.9 |
| deflate:12 | 0.2346 | 0.2169 | 0.2718 | 0.2967 | 0.2494 | 0.4287 | 0.3231 | 10.00 | 973 | 14.8 | 40.5 |
| xz:6 | 0.2339 | 0.2179 | 0.2717 | 0.3001 | 0.2488 | 0.4312 | 0.3215 | 3.94 | 121 | 122 | 220 |
| bzip2:9 | 0.2494 | 0.2289 | 0.2593 | 0.3022 | 0.2545 | 0.4122 | 0.3300 | 11.9 | 44.1 | 324 | 680 |
| zstd:9 | 0.2389 | 0.2208 | 0.2823 | 0.3027 | 0.2549 | 0.4380 | 0.3303 | 50.2 | 1,015 | 13.1 | 30.7 |
| deflate:6 | 0.2424 | 0.2226 | 0.2842 | 0.3041 | 0.2571 | 0.4399 | 0.3319 | 95.1 | 843 | 15.8 | 37.6 |
| zstd:3 | 0.2527 | 0.2332 | 0.3093 | 0.3198 | 0.2722 | 0.4566 | 0.3472 | 249 | 926 | 13.8 | 32.0 |
| zstd:1 | 0.2615 | 0.2386 | 0.3243 | 0.3270 | 0.2779 | 0.4572 | 0.3591 | 322 | 876 | 15.1 | 28.6 |
| lz4 +dict | 0.2513 | 0.2531 | 0.4029 | 0.3950 | 0.3022 | 0.6146 | 0.4573 | 326 | 1,512 | 8.72 | 22.4 |
| lz4hc:12 | 0.3196 | 0.2951 | 0.3686 | 0.4113 | 0.3447 | 0.6015 | 0.4365 | 25.6 | 3,171 | 3.30 | 10.4 |
| snappy | 0.3654 | 0.3363 | 0.4532 | 0.4638 | 0.3833 | 0.6672 | 0.4954 | 546 | 1,370 | 7.80 | 25.8 |
| lz4 | 0.3703 | 0.3373 | 0.4795 | 0.4773 | 0.4004 | 0.6845 | 0.5037 | 549 | 2,652 | 4.55 | 13.9 |
| fsst | 0.6080 | 0.5309 | 0.5811 | 0.5743 | 0.6085 | 0.5493 | 0.6792 | 316 | 1,997 | 5.36 | 15.0 |

### Random access, 64 KiB blocks (bytes / raw byte incl. dictionary and 16 B per block; 1 KiB excerpt fetch)

| codec | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | decompress MB/s | fetch p50 µs | fetch p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **oojuro** run, default profile | **0.0534** | **0.1076** | **0.2013** | **0.0841** | **0.1199** | 0.3279 | **0.1708** | 3.28 | 588 | 83.2 | 216 |
| brotli:11:24 +dict | 0.0810 | 0.1128 | 0.2039 | 0.1929 | 0.1452 | 0.3269 | 0.2306 | 1.15 | 394 | 162 | 225 |
| zstd:22 +dict | 0.0825 | 0.1143 | 0.2046 | 0.1931 | 0.1485 | 0.3301 | 0.2328 | 4.61 | 1,103 | 47.0 | 75.7 |
| zstd:19 +dict | 0.0826 | 0.1149 | 0.2046 | 0.1934 | 0.1492 | 0.3301 | 0.2329 | 7.77 | 1,120 | 46.4 | 64.9 |
| **oojuro** run, flush profile | 0.0717 | 0.1439 | 0.2658 | 0.1076 | 0.1518 | 0.4154 | 0.2138 | 108 | 636 | 91.7 | 236 |
| brotli:9:22 +dict | 0.0864 | 0.1195 | 0.2172 | 0.2030 | 0.1559 | 0.3480 | 0.2422 | 3.28 | 411 | 126 | 202 |
| zstd:9 +dict | 0.0884 | 0.1211 | 0.2231 | 0.2066 | 0.1578 | 0.3554 | 0.2453 | 51.3 | 1,156 | 46.9 | 72.7 |
| brotli:5:22 +dict | 0.0900 | 0.1230 | 0.2285 | 0.2102 | 0.1602 | 0.3637 | 0.2477 | 39.7 | 394 | 144 | 223 |
| ppmd8:6:16 | 0.1414 | 0.1567 | 0.2025 | 0.2102 | 0.1828 | **0.3165** | 0.2524 | 17.8 | 16.7 | 2,295 | 4,344 |
| ppmd7:6:16 | 0.1417 | 0.1569 | 0.2030 | 0.2104 | 0.1832 | 0.3169 | 0.2526 | 16.9 | 16.3 | 2,380 | 4,519 |
| brotli:11:24 | 0.1214 | 0.1538 | 0.2240 | 0.2099 | 0.1820 | 0.3339 | 0.2582 | 1.10 | 375 | 153 | 256 |
| zstd:3 +dict | 0.1035 | 0.1393 | 0.2583 | 0.2314 | 0.1813 | 0.3815 | 0.2710 | 263 | 1,013 | 54.3 | 77.8 |
| xz:6 | 0.1369 | 0.1711 | 0.2418 | 0.2386 | 0.1996 | 0.3782 | 0.2814 | 5.41 | 156 | 279 | 464 |
| bzip2:9 | 0.1567 | 0.1789 | 0.2321 | 0.2383 | 0.2071 | 0.3548 | 0.2884 | 18.0 | 57.9 | 961 | 1,432 |
| zstd:22 | 0.1396 | 0.1744 | 0.2442 | 0.2411 | 0.2059 | 0.3846 | 0.2872 | 3.58 | 1,151 | 48.3 | 89.5 |
| zstd:19 | 0.1397 | 0.1747 | 0.2442 | 0.2414 | 0.2061 | 0.3846 | 0.2873 | 3.83 | 1,184 | 49.2 | 84.6 |
| brotli:9:22 | 0.1388 | 0.1736 | 0.2481 | 0.2414 | 0.2060 | 0.3876 | 0.2875 | 3.69 | 333 | 170 | 290 |
| brotli:5:22 | 0.1405 | 0.1755 | 0.2540 | 0.2442 | 0.2079 | 0.3913 | 0.2899 | 32.0 | 329 | 173 | 275 |
| deflate:12 | 0.1531 | 0.1798 | 0.2492 | 0.2459 | 0.2142 | 0.3875 | 0.2915 | 6.67 | 1,064 | 49.1 | 75.5 |
| zstd:9 | 0.1453 | 0.1801 | 0.2570 | 0.2498 | 0.2131 | 0.3986 | 0.2964 | 71.7 | 1,240 | 43.3 | 72.8 |
| zstd:1 +dict | 0.1208 | 0.1589 | 0.2822 | 0.2637 | 0.2000 | 0.4313 | 0.3132 | 393 | 1,334 | 43.3 | 60.2 |
| deflate:6 | 0.1609 | 0.1869 | 0.2629 | 0.2561 | 0.2235 | 0.4033 | 0.3020 | 101 | 948 | 57.6 | 78.7 |
| zstd:3 | 0.1584 | 0.1953 | 0.2861 | 0.2692 | 0.2323 | 0.4172 | 0.3173 | 293 | 1,317 | 45.8 | 80.9 |
| zstd:1 | 0.1664 | 0.2040 | 0.3033 | 0.2825 | 0.2416 | 0.4414 | 0.3351 | 405 | 1,365 | 43.3 | 73.1 |
| lz4hc:12 | 0.1875 | 0.2316 | 0.3255 | 0.3187 | 0.2767 | 0.5012 | 0.3767 | 27.4 | 3,295 | 13.3 | 22.0 |
| lz4 +dict | 0.1778 | 0.2471 | 0.4003 | 0.3816 | 0.2901 | 0.6271 | 0.4442 | 421 | 1,731 | 32.9 | 41.8 |
| snappy | 0.2450 | 0.2878 | 0.4226 | 0.3992 | 0.3313 | 0.6197 | 0.4561 | 543 | 1,439 | 36.6 | 50.7 |
| lz4 | 0.2371 | 0.2877 | 0.4494 | 0.4124 | 0.3456 | 0.6458 | 0.4659 | 525 | 2,408 | 21.7 | 38.9 |
| fsst | 0.6071 | 0.5301 | 0.5806 | 0.5734 | 0.6077 | 0.5485 | 0.6785 | 310 | 2,600 | 20.9 | 31.1 |

### Random access, 256 KiB blocks (bytes / raw byte incl. dictionary and 16 B per block; 1 KiB excerpt fetch)

| codec | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s | decompress MB/s | fetch p50 µs | fetch p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **oojuro** run, archive profile | **0.0473** | **0.0991** | 0.1985 | **0.0788** | **0.1102** | 0.3185 | **0.1530** | 2.96 | 383 | 240 | 624 |
| brotli:11:24 +dict | 0.0564 | 0.1007 | 0.1990 | 0.1706 | 0.1323 | 0.3191 | 0.2044 | 1.00 | 360 | 625 | 920 |
| zstd:22 +dict | 0.0590 | 0.1044 | 0.2036 | 0.1743 | 0.1388 | 0.3291 | 0.2101 | 5.65 | 1,383 | 163 | 313 |
| zstd:19 +dict | 0.0591 | 0.1050 | 0.2036 | 0.1748 | 0.1395 | 0.3291 | 0.2102 | 5.43 | 1,322 | 169 | 322 |
| ppmd8:6:16 | 0.0975 | 0.1343 | **0.1847** | 0.1767 | 0.1479 | **0.2796** | 0.2192 | 23.8 | 21.4 | 6,637 | 14,782 |
| ppmd7:6:16 | 0.0976 | 0.1346 | 0.1849 | 0.1769 | 0.1481 | 0.2799 | 0.2194 | 21.2 | 17.4 | 7,036 | 16,069 |
| brotli:9:22 +dict | 0.0605 | 0.1076 | 0.2142 | 0.1811 | 0.1438 | 0.3429 | 0.2167 | 8.16 | 464 | 506 | 782 |
| brotli:11:24 | 0.0690 | 0.1321 | 0.2066 | 0.1799 | 0.1443 | 0.3203 | 0.2254 | 1.09 | 397 | 623 | 921 |
| zstd:9 +dict | 0.0628 | 0.1106 | 0.2221 | 0.1876 | 0.1474 | 0.3561 | 0.2231 | 62.1 | 1,398 | 158 | 327 |
| brotli:5:22 +dict | 0.0640 | 0.1120 | 0.2272 | 0.1906 | 0.1492 | 0.3633 | 0.2247 | 48.3 | 469 | 563 | 957 |
| xz:6 | 0.0747 | 0.1410 | 0.2127 | 0.1932 | 0.1517 | 0.3406 | 0.2362 | 5.45 | 188 | 859 | 1,638 |
| bzip2:9 | 0.0919 | 0.1468 | 0.2059 | 0.1929 | 0.1609 | 0.3128 | 0.2460 | 19.2 | 56.1 | 3,770 | 6,077 |
| zstd:22 | 0.0772 | 0.1451 | 0.2165 | 0.1961 | 0.1581 | 0.3464 | 0.2423 | 4.61 | 1,344 | 130 | 217 |
| zstd:19 | 0.0773 | 0.1453 | 0.2165 | 0.1964 | 0.1583 | 0.3464 | 0.2423 | 5.44 | 1,330 | 128 | 215 |
| brotli:9:22 | 0.0779 | 0.1477 | 0.2262 | 0.2015 | 0.1621 | 0.3600 | 0.2477 | 9.77 | 442 | 507 | 665 |
| zstd:9 | 0.0814 | 0.1520 | 0.2325 | 0.2073 | 0.1664 | 0.3676 | 0.2538 | 72.0 | 1,486 | 122 | 199 |
| brotli:5:22 | 0.0801 | 0.1508 | 0.2351 | 0.2080 | 0.1655 | 0.3715 | 0.2531 | 48.2 | 420 | 528 | 884 |
| zstd:3 +dict | 0.0749 | 0.1283 | 0.2576 | 0.2117 | 0.1697 | 0.3821 | 0.2475 | 295 | 1,384 | 141 | 260 |
| deflate:12 | 0.1281 | 0.1682 | 0.2361 | 0.2302 | 0.2016 | 0.3739 | 0.2770 | 8.52 | 1,151 | 171 | 250 |
| zstd:3 | 0.0921 | 0.1695 | 0.2664 | 0.2328 | 0.1872 | 0.4011 | 0.2786 | 290 | 1,329 | 136 | 226 |
| zstd:1 +dict | 0.0854 | 0.1529 | 0.2820 | 0.2470 | 0.1894 | 0.4324 | 0.2978 | 401 | 1,524 | 122 | 205 |
| deflate:6 | 0.1360 | 0.1758 | 0.2510 | 0.2413 | 0.2117 | 0.3915 | 0.2880 | 99.2 | 1,111 | 201 | 259 |
| zstd:1 | 0.0974 | 0.1778 | 0.2818 | 0.2434 | 0.1957 | 0.4183 | 0.2958 | 286 | 1,569 | 117 | 186 |
| lz4hc:12 | 0.1286 | 0.2023 | 0.2908 | 0.2741 | 0.2288 | 0.4519 | 0.3344 | 21.7 | 3,481 | 53.9 | 68.1 |
| lz4 +dict | 0.1721 | 0.2578 | 0.4058 | 0.3744 | 0.2988 | 0.6304 | 0.4413 | 499 | 1,798 | 124 | 163 |
| lz4 | 0.1850 | 0.2656 | 0.4114 | 0.3786 | 0.3082 | 0.6344 | 0.4465 | 535 | 3,002 | 73.6 | 103 |
| snappy | 0.2427 | 0.2872 | 0.4207 | 0.3969 | 0.3293 | 0.6195 | 0.4546 | 564 | 1,243 | 151 | 197 |
| fsst | 0.6070 | 0.5299 | 0.5804 | 0.5732 | 0.6075 | 0.5483 | 0.6783 | 329 | 2,404 | 83.4 | 128 |

## Cold archive tier (2026-10-21)

Question: for data that is old and rarely read, is a heavier codec at
archive block sizes (1–16 MiB) worth a separate tier next to the default
(merged runs: 64 KiB frames, zstd 19 + dictionary, 1 KiB dedupe)?

Method: each sample's first 64 MiB of text cut into independent 1, 4 and
16 MiB blocks (a read decodes one block). libbsc (BWT, `-e1` / `-e2`) and
zpaq (`-method 3` / `5`) run per block as their CLIs
(`scripts/compress/cold_tier.sh`): sizes and roundtrips for every block
(zpaq -m5 decodes the first block of each sample only), CPU from
`/usr/bin/time`; under the bench lock, one block per (corpus, size) is
encoded and decoded on swe, othink and devlogs for latency. PPMd o8, xz -9,
zstd 19/22 with a window covering the block, and brotli 11 run in process
(`leaderboard2.sh ratio-cold` / `time-cold`, 20 fetches per configuration on
the full 64 MiB). bsc and zpaq must decode a whole block per excerpt; the
in-process coders stop at the excerpt (half a block on average). Also
measured: v1's own run format with 4 / 16 MiB frames (`cold4`, `cold16`).

Cost: object storage lists at ~$0.023 per GB-month (S3 Standard), so every
0.01 of bytes/raw saved is **$0.23 per raw TB-month**. Today the `devlogs`
live journal is 326 MB on disk for 1,135 MB of text (0.287 bytes/raw, ~$6.6
per raw TB-month); its originals are 0.18–0.22 of raw (the replay in the index
section: 0.176 with merges settled, 0.217 before) and the default merged
runs reach 0.171 on the sample.

### Cold archive tier candidates (independent blocks of 1-16 MiB; bytes / raw byte)

Fetch = time to return one 1 KiB excerpt = decode its block (bsc and zpaq always decode the whole block; the in-process coders stop at the excerpt). External coders: one block encoded and decoded under the lock per (corpus, size), CPU MB/s from the size pass. Savings: S3 at $0.023 per GB-month, per TB of raw text kept, vs the default run profile on the same corpus (originals only; the index is unchanged).

| codec | block | swe | ohjson | othink | hermes | ohsft | fiqa | devlogs | compress MB/s (CPU) | decompress MB/s (CPU) | 1 KiB fetch p50 | saved $/TB-month (devlogs) | saved $/TB-month (median, agent corpora) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **oojuro run, default** | 64 KiB | 0.0534 | 0.1076 | 0.2013 | 0.0841 | 0.1199 | 0.3279 | 0.1708 | 3.28 | 588 | 83.2 µs | 0 | 0 |
| zpaq -m5 | 1 MiB | 0.0370 | 0.0807 | 0.1486 | 0.1114 | 0.0935 | 0.2461 | 0.1466 | 0.435 | 0.448 | 2,965 ms | +0.56 | +0.58 |
| ppmd8:8:256 | 1 MiB | 0.0634 | 0.1003 | 0.1705 | 0.1348 | 0.1182 | 0.2531 | 0.1754 | 11.8 | 11.5 | 24.6 ms | -0.11 | -0.03 |
| ppmd7:8:256 | 1 MiB | 0.0634 | 0.1004 | 0.1707 | 0.1348 | 0.1184 | 0.2536 | 0.1755 | 10.00 | 10.3 | 32.4 ms | -0.11 | -0.04 |
| libbsc -e2 | 1 MiB | 0.0471 | 0.1029 | 0.1769 | 0.1353 | 0.1189 | 0.2582 | 0.1796 | 31.5 | 36.5 | 47.2 ms | -0.20 | +0.07 |
| libbsc -e1 | 1 MiB | 0.0477 | 0.1041 | 0.1780 | 0.1362 | 0.1202 | 0.2602 | 0.1812 | 41.9 | 65.9 | 46.1 ms | -0.24 | +0.04 |
| brotli:11:24 | 1 MiB | 0.0445 | 0.0999 | 0.1957 | 0.1372 | 0.1225 | 0.3050 | 0.1826 | 1.11 | 577 | 1.01 ms | -0.27 | +0.03 |
| zpaq -m3 | 1 MiB | 0.0581 | 0.1121 | 0.1843 | 0.1448 | 0.1300 | 0.2639 | 0.1955 | 6.16 | 5.91 | 217 ms | -0.57 | -0.17 |
| xz:9 | 1 MiB | 0.0468 | 0.1040 | 0.1970 | 0.1425 | 0.1263 | 0.3130 | 0.1871 | 2.79 | 185 | 2.33 ms | -0.37 | -0.03 |
| zstd:22:w27 | 1 MiB | 0.0485 | 0.1074 | 0.2008 | 0.1445 | 0.1313 | 0.3176 | 0.1920 | 4.14 | 1,706 | 0.246 ms | -0.49 | -0.13 |
| zstd:19:w24 | 1 MiB | 0.0486 | 0.1076 | 0.2008 | 0.1449 | 0.1316 | 0.3176 | 0.1920 | 5.47 | 1,753 | 0.255 ms | -0.49 | -0.13 |
| zpaq -m5 | 4 MiB | 0.0280 | 0.0678 | 0.1385 | 0.0728 | 0.0760 | 0.2271 | 0.1124 | 0.456 | 0.359 | 9,398 ms | +1.34 | +0.96 |
| libbsc -e2 | 4 MiB | 0.0359 | 0.0862 | 0.1668 | 0.0928 | 0.0985 | 0.2352 | 0.1444 | 31.3 | 35.4 | 136 ms | +0.61 | +0.49 |
| libbsc -e1 | 4 MiB | 0.0363 | 0.0871 | 0.1678 | 0.0937 | 0.0996 | 0.2369 | 0.1456 | 33.2 | 38.9 | 99.1 ms | +0.58 | +0.47 |
| ppmd8:8:256 | 4 MiB | 0.0578 | 0.0904 | 0.1658 | 0.1072 | 0.1051 | 0.2340 | 0.1484 | 8.16 | 7.09 | 231 ms | +0.51 | +0.37 |
| ppmd7:8:256 | 4 MiB | 0.0577 | 0.0904 | 0.1660 | 0.1071 | 0.1051 | 0.2343 | 0.1484 | 15.5 | 15.3 | 155 ms | +0.52 | +0.37 |
| zpaq -m3 | 4 MiB | 0.0443 | 0.0953 | 0.1719 | 0.0998 | 0.1067 | 0.2392 | 0.1574 | 5.67 | 4.58 | 768 ms | +0.31 | +0.29 |
| brotli:11:24 | 4 MiB | 0.0358 | 0.0884 | 0.1870 | 0.0936 | 0.1033 | 0.2893 | 0.1426 | 0.587 | 427 | 5.02 ms | +0.65 | +0.39 |
| xz:9 | 4 MiB | 0.0369 | 0.0910 | 0.1867 | 0.0958 | 0.1058 | 0.2915 | 0.1447 | 2.88 | 147 | 13.1 ms | +0.60 | +0.36 |
| **oojuro run**, 4 MiB frames (dedupe + dict) | 4 MiB | 0.0408 | 0.0955 | 0.1967 | 0.0724 | 0.1059 | 0.3189 | 0.1385 | 6.05 | 14.7 | 1.51 ms | +0.74 | +0.28 |
| zstd:22:w27 | 4 MiB | 0.0381 | 0.0936 | 0.1902 | 0.0965 | 0.1093 | 0.2950 | 0.1485 | 4.40 | 1,570 | 1.97 ms | +0.51 | +0.29 |
| zstd:19:w24 | 4 MiB | 0.0382 | 0.0940 | 0.1902 | 0.0969 | 0.1097 | 0.2950 | 0.1486 | 5.64 | 1,941 | 1.39 ms | +0.51 | +0.28 |
| zpaq -m5 | 16 MiB | 0.0244 | 0.0616 | 0.1319 | 0.0545 | 0.0618 | 0.2141 | 0.0961 | 0.411 | 0.382 | 30,169 ms | +1.72 | +1.20 |
| libbsc -e2 | 16 MiB | 0.0313 | 0.0778 | 0.1583 | 0.0715 | 0.0815 | 0.2179 | 0.1240 | 34.6 | 37.1 | 485 ms | +1.08 | +0.78 |
| libbsc -e1 | 16 MiB | 0.0316 | 0.0785 | 0.1592 | 0.0723 | 0.0823 | 0.2194 | 0.1249 | 31.5 | 43.8 | 498 ms | +1.06 | +0.77 |
| zpaq -m3 | 16 MiB | 0.0391 | 0.0852 | 0.1626 | 0.0773 | 0.0877 | 0.2215 | 0.1367 | 5.84 | 4.73 | 3,061 ms | +0.78 | +0.63 |
| brotli:11:24 | 16 MiB | 0.0322 | 0.0827 | 0.1793 | 0.0717 | 0.0842 | 0.2740 | 0.1240 | 0.586 | 363 | 19.2 ms | +1.08 | +0.54 |
| xz:9 | 16 MiB | 0.0331 | 0.0850 | 0.1784 | 0.0729 | 0.0862 | 0.2740 | 0.1254 | 2.56 | 223 | 27.8 ms | +1.05 | +0.52 |
| ppmd8:8:256 | 16 MiB | 0.0577 | 0.0874 | 0.1624 | 0.0965 | 0.0960 | 0.2203 | 0.1398 | 11.7 | 9.32 | 506 ms | +0.71 | +0.51 |
| ppmd7:8:256 | 16 MiB | 0.0575 | 0.0873 | 0.1625 | 0.0964 | 0.0959 | 0.2205 | 0.1397 | 8.46 | 9.74 | 558 ms | +0.71 | +0.51 |
| zstd:22:w27 | 16 MiB | 0.0340 | 0.0869 | 0.1816 | 0.0728 | 0.0885 | 0.2767 | 0.1285 | 2.77 | 998 | 4.34 ms | +0.97 | +0.46 |
| zstd:19:w24 | 16 MiB | 0.0342 | 0.0874 | 0.1820 | 0.0743 | 0.0891 | 0.2774 | 0.1288 | 3.24 | 1,140 | 3.93 ms | +0.97 | +0.45 |
| **oojuro run**, 16 MiB frames (dedupe + dict) | 16 MiB | 0.0404 | 0.0953 | 0.1968 | 0.0756 | 0.1037 | 0.3179 | 0.1354 | 6.06 | 5.74 | 5.71 ms | +0.81 | +0.29 |

### Thinking text alone (records grouped by kind, no dictionary; bytes / raw thinking byte)

| corpus | block | ppmd8:8:256 | xz:9 | zstd:19:w24 |
|---|---|---|---|---|
| devlogs | 256 KiB | 0.2644 | 0.3210 | 0.3254 |
| devlogs | 1024 KiB | 0.2644 | 0.3210 | 0.3254 |
| devlogs | 4096 KiB | 0.2644 | 0.3210 | 0.3254 |
| devlogs | 16384 KiB | 0.2644 | 0.3210 | 0.3254 |
| othink | 256 KiB | 0.1820 | 0.2191 | 0.2230 |
| othink | 1024 KiB | 0.1756 | 0.2051 | 0.2091 |
| othink | 4096 KiB | 0.1716 | 0.1951 | 0.1989 |
| othink | 16384 KiB | 0.1688 | 0.1875 | 0.1913 |
| swe | 256 KiB | 0.0883 | 0.0982 | 0.0981 |
| swe | 1024 KiB | 0.0831 | 0.0860 | 0.0862 |
| swe | 4096 KiB | 0.0826 | 0.0795 | 0.0796 |
| swe | 16384 KiB | 0.0829 | 0.0761 | 0.0762 |

Reading (devlogs first, then the median over the six agent corpora):

1. **Most of the cold-tier gain needs no new codec.** Plain zstd 19 with a
   16 MiB window over 16 MiB blocks takes the devlogs sample from 0.171 to
   0.129 (−25%; agent corpora −10 to −36%), decodes at ~1.1 GB/s and returns
   a 1 KiB excerpt in ~4 ms. That is **$0.97 per raw TB-month** on devlogs
   ($0.45 median).
2. **BWT adds 4% more for 100x the read cost.** libbsc -e2 at 16 MiB: 0.124
   (−27%), $1.08 per TB-month (+$0.11 over zstd), but every excerpt decodes
   the whole block (~0.5 s at 16 MiB, 35–45 MB/s) and it is a new C++
   dependency.
   At 1 MiB it is worse than today's default on devlogs (0.180): small BWT
   blocks lose the cross-record redundancy dedupe and the dictionary catch.
3. **Context mixing is the floor, not a tier.** zpaq -m5 at 16 MiB: 0.096
   (−44%, $1.72 per TB-month) at 0.4 MB/s both ways: 30–50 s to serve one
   excerpt, and ~2,400 CPU-seconds to pack 1 GB (~$0.03 at $0.04 per
   vCPU-hour, repaid by the storage saving after ~16 months; zstd 19 at
   16 MiB repays its ~$0.003 per GB in ~3 months, bsc in under one).
   zpaq -m3 is beaten by bsc everywhere.
4. **PPMd o8 for thinking**: on thinking-only frames (othink) it is 12–18%
   smaller than zstd 19 at 256 KiB–16 MiB (table below), but thinking is
   0.1–0.2% of the devlogs journal and PPMd decodes at ~10–25 MB/s
   (0.15–0.55 s per excerpt at 4–16 MiB). On mixed
   agent text it trails BWT and zstd at 16 MiB (devlogs 0.140).
5. **Our own format, bigger frames.** v1's run format with 4 MiB frames
   (dedupe + dictionary) is the best 4 MiB option (devlogs 0.139, −19%,
   1.5 ms excerpts); at 16 MiB it loses to plain zstd (0.135 vs 0.129): at
   that window zstd finds repeats more cheaply than 1 KiB dedupe references.
   The reader's 4 MiB frame cache cannot hold such frames, so full scans
   drop to 6–15 MB/s: a cold profile needs a matching cache or streaming
   scan.

**Recommendation: no separate cold codec. Add, at most, a cold *profile*
of the existing format** (zstd 19, 16 MiB frames with a matching window,
no chunk dedupe, no dictionary) for merged runs nobody has read for N days:
−25% of originals on the devlogs journal, ~$1 per raw TB-month, ~4 ms
excerpts, same decoder. It is worth doing after the cheaper levers in the
summary (index tables, recorder dedupe, compacting idle stores), which save
as much with no read cost. libbsc would add ~$0.11 per TB-month for
100x the read CPU and a new dependency: not worth it at this saving. zpaq /
paq8px / cmix-class coders only make sense for a deep archive that is never
searched, and this store keeps text searchable.

## System comparators (64 MiB samples, bytes/raw; every row read back byte-exact)

| system | variant | swe | ohjson | othink | hermes | ohsft | fiqa |
|---|---|---|---|---|---|---|---|
| CLP clp-s | level 19 | 0.0372 | 0.0868 | 0.1870 | 0.0683 | 0.0781 | 0.2851 |
| CLP clp-s | level 3 | 0.0606 | 0.1245 | 0.2682 | 0.0960 | 0.1303 | 0.3869 |
| ClickHouse 26.9 MergeTree | ZSTD(19) | 0.0615 | 0.1146 | 0.2025 | 0.1599 | 0.1399 | 0.3388 |
| ClickHouse | ZSTD(3) | 0.0759 | 0.1384 | 0.2583 | 0.1941 | 0.1712 | 0.3956 |
| ClickHouse | LZ4 | 0.1880 | 0.2690 | 0.4095 | 0.3890 | 0.3094 | 0.6540 |
| VictoriaLogs 1.53 | one stream | 0.0697 | 0.1354 | 0.2612 | 0.1606 | 0.1656 | 0.4119 |
| VictoriaLogs | stream per session | 0.2256 | 0.1981 | 0.3443 | 0.5751 | 0.2903 | 1.0047 |
| RocksDB 9.8 | zstd 19 + dict, 64 KiB blocks | 0.0889 | 0.1175 | 0.2057 | 0.2033 | 0.1527 | 0.3392 |
| RocksDB | zstd 19 + dict, 16 KiB blocks | 0.1230 | 0.1332 | 0.2091 | 0.2147 | 0.1650 | 0.3447 |
| RocksDB | lz4, 16 KiB blocks | 0.3686 | 0.3336 | 0.4549 | 0.4738 | 0.4003 | 0.7045 |
| OpenSearch 2.19 | best_compression, text not indexed | 0.2195 | 0.2184 | 0.2621 | 0.3070 | 0.2567 | 0.4610 |
| OpenSearch | best_compression, text indexed | 0.4758 | 0.4256 | 0.5289 | 0.5839 | 0.4920 | 0.8318 |
| Loki 3.7 | zstd chunks | 0.1650 | 0.2018 | 0.2742 | 0.2789 | 0.2165 | 0.4306 |
| Loki | snappy chunks | 0.3310 | 0.3312 | 0.4286 | 0.4685 | 0.3712 | 0.6646 |
| ClickHouse | ZSTD(19), index_granularity 256 | 0.0958 | 0.1404 | 0.2033 | 0.1796 | 0.1602 | 0.3745 |
| **oojuro run (v1)** | default: merged runs, 64 KiB frames | 0.0534 | 0.1076 | 0.2013 | 0.0841 | 0.1199 | 0.3279 |
| **oojuro run (v1)** | archive profile, 256 KiB frames | **0.0473** | 0.0991 | 0.1985 | 0.0788 | 0.1102 | 0.3185 |
| zstd 19, 16 MiB blocks | the cold profile candidate (codec rows above) | **0.0342** | 0.0874 | **0.1820** | 0.0743 | 0.0891 | **0.2774** |

clp-s compresses each column over a whole archive (like whole-sample zstd 19,
swe 0.0333) and has no per-record random access; it is the comparator to
match for the cold archive tier. RocksDB stores only the text (seq as key);
ClickHouse counts all columns. RocksDB point reads (20 random records,
200 queries, under the lock, swe): zstd 19 + 64 KiB dict p50 2.19 ms / p99
11.0 ms; other systems' timed runs did not get the shared lock in time.
Scripts and pinned image digests: `scripts/compress/systems/`.

Against these systems (2026-10-21 rows): v1's default run is smaller than
every system that serves single records (RocksDB at 64 KiB blocks: 1.0–2.4x
v1's bytes; ClickHouse at index granularity 256: 1.0–2.1x) and is
beaten only by clp-s level 19, which compresses whole columns and cannot
serve one record without decoding its archive. The cold profile candidate
(zstd 19 over 16 MiB blocks) lands within −8% to +14% of clp-s level 19.

## Summary: what we use, what we tried, what we rejected, what to adopt next (2026-10-21)

Money below is per raw TB-month at the S3 Standard list price (~$0.023 per
GB-month): 0.01 bytes/raw = $0.23. The devlogs journal costs ~0.287
bytes/raw on disk today (~$6.6).

### What v1 uses

| layer | what | devlogs journal |
|---|---|---|
| originals, small stores | flush runs: zstd 3 + shipped starter dictionary, 64 KiB frames, 1 KiB FastCDC dedupe inside a run (P3) | 0.214 (sample) |
| originals, merged | zstd 19 + dictionary trained per run (≤ 512 KiB, ~1/256 of the text, E12), same frames and dedupe (E4, E16) | 0.171 (sample); 1.38x whole-sample zstd 19, 2.0x zpaq -m5 |
| record dedupe | one copy per repeated (tool, args, text) per agent store (P1, P1b) | **inert on the recorder path** (args carry `src`/`rt`); 12.3% of records would dedupe |
| index | nine segments: Rice postings + gamma tf (P5, within 4% of the entropy bound), front-coded dictionary, 84 B/row fixed tables, no positions | 96.6 MB per GB of text (fixed tables 42%, postings 42%, dictionary 15%) |
| safety | byte-exact BLAKE3 gates, batch-atomic appends, fail-stop after I/O errors, erase with proof (E14, E19) | |

### Tried and kept
Binary frames + zstd + dictionary (E1, 4.5x smaller than the old journal);
zstd 3 for the hot path (E2); 1 KiB content-defined dedupe with one-hop
references (E4: 0 to −57%); 64 KiB / 1 KiB / ≤ 512 KiB defaults (E5, E12);
background compaction and fail-stop (E13, E14); immutable run files that
the engine adopted (E16); erase with proof (E19); a shipped starter
dictionary from permissive public traces (E21 → P3); record-level dedupe
(P1, but see the finding above); Rice postings (P5). On the leaderboard,
v1's default run beats every codec at 64 KiB blocks on six of seven corpora
(−27% vs zstd 19 + dictionary on the devlogs sample).

### Tried and rejected (why)

| idea | result | why rejected |
|---|---|---|
| frames grouped by record kind (E3) | +4–5% bytes | append order keeps session locality |
| P-frames (E6, E11, and on the archive profile here) | 0 to −1.5% | the dictionary already captures it; +4–14% decodes per fetch |
| tokenizer pre-pass (E7) | −3 to −40% for zstd and PPMd, up to +7% for xz | 6x slower decode, needs a vocabulary file |
| FM-index as storage (E8) | 0.3–0.5x raw | 3–10x the store's bytes |
| JSON unescape (E9), template + variables for tool calls (P2) | −2% / +0.3–3.8% | small or negative, kind-specific format |
| very large dictionaries (E15) | in-sample gains vanish out of sample | they memorise their training text |
| search over compressed frames (E18) | exact but 5–8x slower; capped is approximate | record-level index is faster at equal quality |
| LZ4, snappy, deflate, lz4hc, FSST for originals | 1.5–7x the bytes of zstd + dictionary at every block size | speed we do not need (zstd fetches are 1–50 µs) |
| brotli 11 + dictionary | ≤ 1–2% smaller than zstd 22 + dictionary at 16–64 KiB, larger at 4 KiB and per record | 4–25x slower compression, 3–20x slower fetches; the Rust encoder panics on some inputs with a custom dictionary |
| xz, bzip2, PPMd for random access | lose to zstd + dictionary at ≤ 64 KiB on agent text | 5–50x slower fetches; PPMd wins only on prose/thinking at ≥ 64 KiB |
| OpenZL 0.3 (untrained) | within ±13% of zstd 6 | no gain over zstd without training |
| Re-Pair (Wan, Navarro) | ~2x zstd 19 (Wan, 8 MiB prefixes); Navarro ~4x | grammar compression without a strong entropy stage |
| context mixing: zpaq -m5, paq8px | the floor: devlogs 0.085 (zpaq), 0.076 (paq8px, 8 MiB) | 0.2–0.4 MB/s and ~5 KB/s: cannot serve excerpts |
| cmix | — | skipped (2026-10-21): ~32 GB of RAM, ~2 KB/s; paq8px gives the floor |
| cold tier with BWT / context mixing / PPMd | bsc −27%, zpaq -m5 −44% (devlogs, 16 MiB) | bsc adds 4% over plain 16 MiB zstd for 100x read CPU; zpaq takes 30–50 s per excerpt |
| index: Elias-Fano, PForDelta, Roaring doc ids; FST dictionary | doc ids +2% to +51% of the index; FST −1% of the dictionary vs −15% for front coding with columns | Rice is within 4% of the bound; hash-like terms share no prefixes |
| index: positions | +115% index | nothing needs them: phrases are verified on originals |
| index: stopping terms with df > 5–25% | −3 to −17% index | costs up to 27% nDCG@10 (FiQA) or 4% recall@20 (turnloop) |
| index: thinking stored-only / tf-free | −0.2% on the devlogs index | not worth overriding the 2026-10-20 decision |

Still open (measured, not decided): erasure domains (E20:
per-session domains cost +6% to +111%), a starter dictionary from
opt-in contributed logs (P3), a query-side frequent-term cutoff as a
latency lever (2–4x faster tier-1 queries, changes rankings), and the cold
profile below.

### Adopt next, in order

| # | change | devlogs journal effect | $/raw TB-month | cost and risk |
|---|---|---|---:|---|
| 1 | **Recorder dedupe key without `src`/`rt`** (keep each repeat's own args in its `B` entry) | 12.3% of records stored once: index −6.5%, originals −3.5% | ~0.29 | capture change; erasure and provenance tests |
| 2 | **Lean fixed index tables** (keys only where ≠ id, no id table for numeric ids, bit-packed rows, time order rebuilt at open) | index −37% | ~0.82 | segment format v3, read path for finalists |
| 3 | **Dictionary cleanup** (drop the unused `cf`, df/offset columns, 128-term blocks) | dictionary −15% (index −2.2%) | ~0.05 | time lookups first |
| 4 | **Compact idle small stores** (PERF_STORE Next #2): most stores never reach a merge | flush 0.214 → merged 0.171 on stores that never merge; settling the rebuild alone took originals 247 → 200 MB | up to ~0.99 | a daemon policy; `compact_archive` exists |
| 5 | **Archive profile for old merged runs** (256 KiB frames, 512 B chunks, 512 KiB dictionary; no P-frames) | originals −10% | ~0.41 | excerpt 0.08 → 0.24 ms |
| 6 | **Cold profile** (zstd 19, 16 MiB frames, no chunk dedupe or dictionary) for runs unread for N days, instead of 5 for those | originals −25% | ~0.97 | 4 ms excerpts; recompress CPU repaid in ~3 months; product decision |

Items 1–3 are lossless and leave search results unchanged; together they
take the devlogs index from 109.7 MB to ~60 MB. With 1–5, the devlogs journal
would go from ~0.287 to roughly 0.22 bytes/raw (~$5.0 per raw TB-month);
with 6 instead of 5, ~0.20. None of these needs a new codec or library.
