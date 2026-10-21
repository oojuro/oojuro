# Track: compression (oojuro-store)

Path: fewest bytes on disk and in RAM for agent text, fully lossless,
while per-turn writes and top-20 excerpt reads stay fast. Branch
`track/compress` (from main 7b98903). Started 2026-10-20.
Status: **milestone 1 in progress**. Tables marked *pending* are filled by
runs queued on the shared bench lock.

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

## Corpora (public; manifests with sha256 in `docs/benchmarks/compress/manifests/`)

| name | source (license) | records | raw MB | kinds by bytes |
|---|---|---|---|---|
| swe | nebius/SWE-agent-trajectories shard 0 (CC-BY-4.0) | 529,322 | 385.2 | tool_result 64%, thinking 14%, system 8%, user 7%, tool_call 7% |
| ohjson | nebius/SWE-rebench-openhands-trajectories, row group 0 prefix (CC-BY-4.0) | 371,691 | 537.0 | tool_result 63%, tool_call (OpenAI JSON) 20%, system 10% |
| othink | open-thoughts/OpenThoughts-114k shard 0 prefix (Apache-2.0) | 33,272 | 268.4 | thinking 82%, answer 9% |
| hermes | NousResearch/hermes-function-calling-v1, all files (Apache-2.0) | 64,758 | 50.7 | user 51%, system 28%, answer 12% |
| ohsft | SWE-Gym/OpenHands-SFT-Trajectories (MIT) | 23,554 | 31.5 | tool_result 70% |
| fiqa | BeIR/fiqa corpus (CC-BY-SA-4.0), non-agent control | 57,600 | 44.3 | doc |
| nfcorpus | BeIR/nfcorpus corpus, control (floors only) | 3,633 | 5.8 | doc |

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

## Scorecard

*pending: generated by `scripts/compress/scorecard.sh` + `scripts/compress/report.py`.*

## Matched baselines (same records, read back and BLAKE3-verified)

| system | swe | ohjson | othink | hermes | ohsft | notes |
|---|---|---|---|---|---|---|
| **current engine journal (NINET003)** | 0.9629 | 0.6340 | 0.5308 | 0.7941 | 0.7046 | 32 MiB prefix (the engine keeps everything in RAM); RSS after open 3.65 / 3.04 / 1.85 / 2.88 / 3.19 x raw; open 4.3 / 2.3 / 1.3 / 3.7 / 3.0 s |
| SQLite (one row per record, VACUUM) | 1.2325 | 1.1435 | 1.0806 | 1.1816 | 1.1214 | |
| Tantivy 0.27 doc store, lz4 | 0.3565 | 0.3289 | 0.4692 | 0.4713 | 0.3970 | |
| Tantivy 0.27 doc store, zstd 19 | 0.2273 | 0.2120 | *pending* | 0.2945 | 0.2489 | 16 KiB doc blocks |
| Tantivy 0.27 store + positional index | 0.5054 | 0.4409 | *pending* | 0.5918 | 0.5314 | |
| Postgres 17 TOAST pglz / lz4 | *pending* | 0.5665 / 0.6016 | *pending* | *pending* | *pending* | `pg_total_relation_size`; TOAST leaves values under ~2 KiB uncompressed |

Store on the same 32 MiB prefixes as the journal: *pending*.

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
are garbage-collected. Policy comparison (fixed 112 KiB vs adaptive 512 KiB
vs 1 MiB): *pending*.

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

## Codec leaderboard

*pending (whole-sample pass and external codecs running). Covers zstd 1–22
(+long), LZ4/HC, Snappy, libdeflate, brotli 1–11 (+large window), xz, bzip2,
PPMd7/8, tokenizer pre-pass, FSST, libbsc, zpaq, paq8px, cmix (prefix),
OpenZL, Re-Pair, and block-mode random-access variants.*

## System comparators (64 MiB samples, bytes/raw, texts read back exactly)

| system | variant | swe | ohjson | othink | hermes | ohsft | fiqa |
|---|---|---|---|---|---|---|---|
| CLP clp-s | level 19 | 0.0372 | 0.0868 | 0.1870 | 0.0683 | 0.0781 | 0.2851 |
| CLP clp-s | level 3 | 0.0606 | 0.1245 | 0.2682 | 0.0960 | 0.1303 | 0.3869 |
| VictoriaLogs | one stream | 0.0697 | 0.1354 | 0.2612 | 0.1606 | 0.1656 | 0.4119 |
| VictoriaLogs | stream per session | 0.2256 | 0.1981 | 0.3443 | 0.5751 | 0.2903 | 1.0047 |
| Loki | zstd chunks | 0.1650 | 0.2018 | 0.2742 | 0.2789 | 0.2165 | 0.4306 |
| Loki | snappy chunks | 0.3310 | 0.3312 | 0.4286 | 0.4685 | 0.3712 | 0.6646 |
| ClickHouse, OpenSearch, RocksDB | | *pending* | | | | | |

clp-s compresses each column over a whole archive, so it behaves like
whole-sample zstd 19 (swe 64 MiB: 0.0333) and has no per-record random access;
it is the comparator to match for the cold archive tier. Scripts:
`scripts/compress/systems/` (image tags and digests in its README).

## Adoption (integration branch, later)

- The engine replaces its `timeline.nine` journal with `oojuro_store::Store`
  for originals and keeps its own postings; originals leave RAM and excerpts
  come from `get_range`.
- IDs: the engine's 64-hex IDs become derived (hash of seq, time and text, as
  today) or move into `attrs`; the store does not need them.
- Old stores: a one-shot importer reads NINET003 through `oojuro-nine` and
  appends to a new store, checking BLAKE3. Needs a decision on migrate in
  place vs read side by side.

## Open decisions

1. zstd (C library via `zstd-sys`, BSD-3 / GPLv2 dual) becomes a hard
   dependency of the storage path. The workspace already builds C (SQLite,
   Tantivy's zstd).
2. Format v1 is a new on-disk format; NINET003 stays readable only through an
   importer or a dual reader.

## Next

- Scorecard on all corpora (queued on the lock); matched 32 MiB comparison with
  the journal; archive profile (256 KiB frames, 512 B chunks, 512 KiB dict,
  P-frames) as a second row.
- External codecs and system comparators (running).
- Archive tier with PPMd frames for store-only thinking.
