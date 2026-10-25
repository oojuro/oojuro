# Disk-path diagnostic, 2026-10-24

This measures the retained Record2 directory/lexical consumer path, not the
minimal `crates/ojo` file engine. This is incomplete engineering evidence, not one-million-record acceptance,
a hosted result, an SLA, or an overall performance/storage win. The directory
API and the remaining scale work are described in
[RECORD_DISK_PATH.md](../../RECORD_DISK_PATH.md).

`metadata-abba.json` retains all 16 runs, including adverse tails. The source
control is `3de7696` and the metadata candidate is `79764ca`; independently built
binary SHA-256 values are in `metadata-binaries.json`. For each size (1k, 10k)
and phase mode, order is control, candidate, candidate, control. Every run uses
ten closed-loop clients, 500 one-ID and 500 twenty-ID reads, 60 searches and 50
single-row client writes, plus 1000-row initial ingest batches. Searches mix a
unique token, all-row common token, group token, common compound, absent token
and half-history valid-time cutoff. Known time is latest in this timing fixture;
two-clock/key/filter correctness is covered by the engine oracles.

The `isolated` JSON label means barriers separate read, search and write phases.
It does **not** mean an isolated machine. Root's two-job Cloud compile/test gates
overlapped the campaign. Host contention and phase/lock scheduling make unstable
tails unsuitable for attributing a general speedup. The `mixed` mode allows
clients to overlap phase boundaries. Read/write queueing is included; a write's
exact read/search visibility probes follow its searchable ACK outside the ACK
timer and can still affect later queued requests.

| Records / phase | Control search p95 (ms), AB / BA | Candidate search p95 (ms), AB / BA |
| --- | --- | --- |
| 1k / phase separated | 89.946 / 132.863 | 27.737 / 27.387 |
| 1k / mixed | 1734.520 / 1042.281 | 1510.962 / 974.911 |
| 10k / phase separated | 1164.695 / 497.329 | 229.684 / 613.984 |
| 10k / mixed | 2555.549 / 335.029 | 1458.560 / 1892.720 |

At 10k, candidate one-ID p95 spans 9.75–36.53 ms and twenty-ID p95 spans
72.49–263.54 ms. Queued single-write tails remain several seconds. Candidate
initial allocated storage is about 3.65 MB versus 2.13 MB for control; the sealed
visibility representation adds storage. Complete post-write file allocations
and process peak RSS are retained per run. They count COW/orphan/metadata usage,
not just payload bytes.

These initial runs verify every original text, ID, valid time and number
attribute before and after reopen; all 50 client receipts/original texts are
recovered after a second reopen. Comparator complete-Doc checks and native
complete-field/assigned-clock digests are strengthened in the subsequent grouped
harness. Individual search durations, operation distributions, max/p99 tails,
storage and peak RSS are retained here. The early harness did not export every
raw read/write sample; subsequent cohorts explicitly export all samples. No
missing samples have been reconstructed.

The group-commit follow-up is a separate change (`a013089`). It must compare
direct and grouped native modes separately, and give SQLite/raw JSONL/Zstd JSONL
the same bounded queue/count/byte/wait policy before comparing grouped results.

The first matched group smoke is retained in `group-smoke-1000.json` with binary
provenance in `group-binaries.json`. It uses a later source (`adec786`) and a
stronger complete-field/assigned-clock original digest; it is not a rerun of the
metadata AB/BA. It has one 1k, ten-client, phase-separated trial per arm/mode,
500 one-ID and 500 twenty-ID reads, 60 searches and 50 independent writes. Every
arm uses the same queue limit (10), submitted-Doc byte limit (32 MiB), collector
limit (10 requests) and collection wait (2 ms). Search original-text reads are
inside the search timer. The JSON retains every read/write latency, individual
search durations and queue group sizes/waits. Grouping changes request scheduling,
so direct and grouped distributions must be read separately.

| 1k records, phase separated | Direct write p95 (ms) | Grouped write p95 (ms) |
| --- | --- | --- |
| Record2, source and derived caches synced | 5487.7 | 331.1 |
| SQLite WAL, FULL, FTS5 | 744.6 | 26.7 |
| Raw JSONL and counted indexes | 1076.2 | 37.5 |
| JSONL + Zstd and counted indexes | 859.6 | 31.4 |

Record2 still misses the 250 ms write target in that smoke. Initial/post-write
allocation, complete reopen checks, peak RSS and raw tails remain in the JSON.
The native group sizes vary with its post-ACK read/search visibility probes;
all baseline groups happened to contain ten requests. This is equivalent queue
policy, not identical service demand: native records are encrypted and preserve
independent receipts, full fields and clocks; baseline timed reads materialize
Doc text and use their own lexical scores. It is not a general storage or
performance ranking. Original source hashes remain available on
`codex/record-disk-evidence-2026-10-24` after the topic's rebase onto current main.

`rebased-gates.json` records the exact post-rebase integration gate at `980c0f5`:
186 engine tests pass (11 explicit subprocess helpers remain ignored), 8 rank
tests pass, and core/store/Nine plus the independent minimal file engine's archive
and frozen-vector checks pass. It records correctness/build gates, not latency.
Subsequent documentation-only commits do not change those tested runtime bytes.

The fresh source-synced/cache-policy diagnostic is retained in
`cache-policy-phases.json` with exact source/binary provenance in
`cache-policy-binary.json`. It is a one-writer phase probe, ordered durable then
rebuildable at each size. Each trial makes 50 independent single-row requests
using groups of one or ten after 1000-row initial ingest batches. It is neither
an AB/BA nor a ten-client latency trial. Authoritative source/header/receipt sync
is always enabled; the explicit policy changes only disposable cache sync.

| 10k initial records | Median indexed ACK (ms) | Total cache-loss rebuild (ms) | Checkpointed allocation before loss (bytes) |
| --- | --- | --- | --- |
| Durable caches, groups of one | 143.912 | 985.513 | 7,802,880 |
| Rebuildable caches, groups of one | 10.365 | 399.594 | 7,798,784 |
| Durable caches, groups of ten | 144.252 | 1029.745 | 3,903,488 |
| Rebuildable caches, groups of ten | 9.443 | 385.432 | 3,907,584 |

The JSON separates authoritative Store append from locator and lexical
publication, and retains dirty/explicitly flushed apparent/allocated bytes,
all group phase samples, checkpoint durations and peak RSS. An explicit
checkpoint includes redundant clean-file syncs, so its duration is not exact
incremental writeback debt. Similar allocation across policies does not imply
that rebuildable caches eliminate storage work.

After every trial's indexed ACKs the diagnostic drops the writer, removes the
entire dedicated locator and lexical cache directories, and reopens with the
retained source watermark. It verifies all 1050/10050 complete records and
engine-clock digests, every client receipt and exact retry, current lexical
visibility, and an old pinned prefix. That functional visibility proof is
outside ACK and recovery timing. The recovery timer covers source verification
and locator/index reconstruction, not the later complete-original comparison,
visibility assertions or explicit cache checkpoint. Small fixture recovery
results do not qualify one-million-record recovery or production availability.

`cache-concurrent.jsonl` retains all 32 fresh frozen-binary trials, one JSON
object per trial, with source/binary provenance in `cache-concurrent-binary.json`.
These use the same complete-field/original/reopen oracles, ten closed-loop
clients, six query classes and direct/grouped queue policy as the group smoke.
There is one trial per size/phase/mode/arm cell, ordered native then SQLite then
raw JSONL then Zstd JSONL. This is not repeated or randomized competitor evidence.
All raw operation samples, search details, group scheduling, physical allocation,
peak RSS and before/after host pressure are retained. The lane excludes other
cooperating build/benchmark owners; it does not make the workstation isolated.

The native trial explicitly uses rebuildable derived caches. Source/header/receipt
durability and current validated searchable ACK remain enabled. The baseline
arms retain their own original stronger cache/index durability settings. This
policy difference is explicit; these are not equal cache-durability contracts or
a general performance ranking. The raw data includes direct writes as well as
grouped writes, and controls keep equivalent grouping/queue policy.

| 10k records, grouped | Phase | Read 1 p95 (ms) | Read 20 p95 (ms) | Search p95 (ms) | Indexed write p95 (ms) | Post-write allocation (bytes) |
| --- | --- | --- | --- | --- | --- | --- |
| Record2, rebuildable caches | separated | 7.617 | 54.897 | 122.090 | 98.991 | 4,247,552 |
| SQLite WAL/FULL/FTS5 | separated | 0.112 | 1.415 | 261.922 | 37.162 | 4,706,304 |
| Raw indexed JSONL | separated | 0.375 | 5.037 | 261.982 | 44.059 | 6,144,000 |
| Indexed JSONL + Zstd | separated | 0.281 | 6.976 | 239.853 | 36.285 | 4,100,096 |
| Record2, rebuildable caches | mixed | 9.529 | 61.505 | 133.628 | 124.769 | 4,665,344 |
| SQLite WAL/FULL/FTS5 | mixed | 0.180 | 1.945 | 267.240 | 32.737 | 4,706,304 |
| Raw indexed JSONL | mixed | 0.384 | 7.781 | 244.957 | 45.519 | 6,164,480 |
| Indexed JSONL + Zstd | mixed | 0.541 | 7.567 | 256.955 | 86.681 | 4,128,768 |

Native direct-write p95 still fails the 250 ms target: 1677.561 ms in separated
phases and 352.537 ms in mixed phases. Queueing remains inside the write timer.
The immediate post-ACK original/search visibility probes are outside that
request's ACK timer and can contend with later requests. A small one-writer
phase cost is therefore not a concurrent write-tail result.

The native grouped cells fit the numerical targets at 10k in this particular
small cohort. They do not establish the 1M/ten-client target, cold-open behavior,
sustained offered-load capacity, a Cloud result, or an SLA. Lexical work still
walks whole-project posting/visibility pages. Native and comparator scoring,
encryption, original field materialization and independent retry/receipt
contracts still differ as documented above.

`capacity-recovery.json` records a separate functional 100k → 1M probe from the
same frozen tested source (`capacity-recovery-binary.json`). It uses the explicit
rebuildable-cache policy with fully synced source and a trusted disk admission
policy, without changing any legacy in-memory guards. The synthetic corpus has
complete Record2 fields, unique text/hash data and no keyed revisions. Correct
key winners, both clocks, filters, forks and actual SIGKILL failure boundaries
remain separate unit/integration oracles, not million-row timing results.

| Initial records | Independent acknowledged rows | Full cache-loss rebuild (s) | Peak process RSS (KiB) | Allocation before loss (bytes) | Allocation after rebuild (bytes) |
| --- | --- | --- | --- | --- | --- |
| 100,000 | 50 | 4.159 | 18,800 | 42,201,088 | 32,677,888 |
| 1,000,000 | 50 | 43.003 | 40,768 | 465,743,872 | 327,143,424 |

All 100,050/1,000,050 complete originals and assigned-clock digests are compared
after total locator/lexical cache removal and reopen with the saved minimum
watermark. All 50 new request receipts and exact retries, current lexical
visibility and an old pinned prefix pass. Query visibility assertions use a
full-project query containing the new unique terms and are outside the recovery
and ACK timers; this is not a narrow-filter latency measurement. The one-writer
median ten-request group ACK was 11.798 ms at 100k and 15.550 ms at 1M, from only
five groups per size. It is not a concurrent tail result.

At 1M, explicitly checkpointing disposable caches before removal took 24.467 s,
and checkpointing the rebuilt cache took 10.149 s. Those durations include
redundant clean-file sync calls and are not exact writeback debt. Inventory
includes retained COW/orphan/metadata files; rebuilding removes obsolete cache
material, explaining much of the allocation difference. Total fixture creation,
ACK phases, complete original comparisons, checkpoints, reconstruction and
functional assertions took 122.536 s at 1M. This local cache-loss exercise is
not a regional disaster test or a Cloud recovery guarantee.

Reproduction after building `profile_record_write` in release mode:

```sh
profile_record_write NEW_PRIVATE_DIRECTORY 1000000 10 5 rebuildable
```

This establishes functional capacity for this bounded directory path/fixture.
The one-million-record ten-client read/search/write distributions and equivalent
full-scale SQLite/JSONL storage/performance controls remain unqualified.

The separate CRC follow-up changes only the local checksum implementation to a
constant 256-entry IEEE table. It preserves checksums, authority/index bytes and
corruption detection and adds a frozen bitwise-format oracle. The full gate at
`2415b8c` passes 187 engine tests (11 explicit helpers ignored) plus 8 rank tests;
all release profile examples build (`crc-gates.json`). It adds no dependency or
retained cache. Relative to the previously tested `980c0f5`, the only runtime
source change is `lexical/segments/codec.rs`.

`crc-query-abba.jsonl` and `crc-query-binaries.json` retain a source-separated
control/candidate/candidate/control warm-query profile at each of 1k and 10k.
Each process opens the same existing synthetic post-write fixture, verifies
source and caches before query timing, and makes five repetitions of seven query
classes with one client. All returned ID/fixture-number/score-bit signatures and
non-time work counters match. This is a small CPU-cost probe, not a ten-client
latency or cold-open result.

| 10,050 visible records | Bitwise control median (ms) | Table candidate median (ms) |
| --- | --- | --- |
| Unique token | 31.840 | 26.299 |
| All-row common token | 31.886 | 26.384 |
| Common compound | 32.587 | 26.914 |
| Missing term | 30.295 | 24.547 |
| Half-history cutoff | 34.617 | 29.090 |
| Typed source filter | 32.902 | 27.666 |
| Rare literal | 46.170 | 38.250 |

Medians pool the two exposures per binary (ten query samples per class). The
scorer still makes the same full-project visits and decodes the same bounded
segments/visibility pages. This improves checksum CPU work while leaving the
large-corpus query architecture and million-record concurrent target open.

The next fixed 100k ten-client cohort is retained completely in
`cache-concurrent-100k.jsonl` (16 trials) and
`cache-concurrent-100k-binary.json`. It uses the tested table-CRC source, the same
fixture generator/query mix and explicit native rebuildable-cache policy, the
same direct/grouped queues for all arms, and all earlier correctness/reopen
oracles. Each size/phase/mode/arm cell still has only one trial. These are the
checked local harness implementations, not general claims about SQLite, FTS5
or JSONL performance, and the cache durability/scoring/field contracts remain
different as described above. All raw results, including failed latency targets,
are retained; one JSON object per trial keeps the evidence machine-readable.

| 100k records, grouped | Phase | Read 1 p95 (ms) | Read 20 p95 (ms) | Search p95 (ms) | Indexed write p95 (ms) | Post-write allocation (bytes) |
| --- | --- | --- | --- | --- | --- | --- |
| Record2, rebuildable caches | separated | 7.084 | 94.304 | 539.532 | 493.751 | 43,429,888 |
| SQLite WAL/FULL/FTS5 | separated | 0.210 | 2.490 | 3151.628 | 39.507 | 42,594,304 |
| Raw indexed JSONL | separated | 0.881 | 10.784 | 3048.993 | 58.411 | 56,979,456 |
| Indexed JSONL + Zstd | separated | 1.087 | 13.968 | 3087.977 | 51.882 | 36,503,552 |
| Record2, rebuildable caches | mixed | 6.314 | 93.598 | 558.343 | 452.953 | 43,675,648 |
| SQLite WAL/FULL/FTS5 | mixed | 0.227 | 2.715 | 3158.152 | 39.325 | 42,577,920 |
| Raw indexed JSONL | mixed | 1.249 | 12.777 | 3109.706 | 139.322 | 56,991,744 |
| Indexed JSONL + Zstd | mixed | 0.915 | 13.612 | 3111.863 | 112.596 | 36,511,744 |

The native grouped search and write tails fail their 250 ms targets here. Native
direct-write p95 is 9605.011 ms with phases separated and 8617.044 ms mixed.
All sixteen trials pass their functional originals/receipt/reopen/visibility
checks. Peak native RSS is about 54–56 MiB in these concurrent trials; complete
process/resource/footprint data is retained for every arm. This is a measured
100k scale gap, not an accepted 1M concurrent path.

The required next query work is architectural: select terms/partitions and avoid
full-project metadata/posting walks while preserving exact two-clock/filter/key
winners, corpus/candidate mass and score bits. The current scorer depends on
candidate-union mass as well as corpus statistics, so a cached corpus row count
alone does not justify skipping the statistics pass. A routed persistent term
index with authenticated per-segment statistics is a plausible next step; broad
term pruning needs a separate exact proof, and arbitrary clocks/filters need
correct fallbacks. None of that unimplemented work is counted as a speedup.
