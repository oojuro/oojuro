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
