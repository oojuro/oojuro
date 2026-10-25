# Actual Event-file projection diagnostics

The opt-in `profile_juro_projection` example measures actual `.juro` files,
not regenerated source callbacks. Frozen candidate `7260a278638479a813d7ef70bcf8683e44ab7f06`
is compared with main `00ad5e10d454f76ec41ee83bdb76218a8194bfdb`, using the
same harness and actual source files. The [complete receipts](JURO_ACTUAL_FILE_PROJECTION_2026_10_25.json)
retain all 124 observations and 196 matching result signatures. Signature
comparison includes opaque cursor, engine timestamp, score bits, continuation,
excerpt boundaries and exact excerpt bytes.

The source is synthetic non-agent UTF-8 with CRLF, NUL and Unicode; every 997th
event is binary. `Journal.prepare` supplies engine time/nonces, and
`append_prepared_batch` durably writes up to 1,024 unchanged Event frames per
barrier. All source writes use NOCOMPRESS; source encryption is a separate
plain/encrypted dimension. Filesystem compression is disabled on these fixture
files (`lsattr` receipts). Index raw/Zstd encoding is an independent dimension.
These diagnostics add an `ojo` dev dependency only; the runtime engine graph and
minimal file engine remain independent.

`EventBuilder` streams actual `Journal.scan` pages. Every original, including
binary bytes, is checked against the synthetic fixture. Binary events remain
source-readable and are omitted from text results. Binding hashes the verified
file identity and checkpoint. Every selected callback calls `Journal.get` with
the actual 40-byte cursor, checks complete original bytes, and returns actual
engine acceptance time. The source checkpoint is checked before and after each
query and around index construction. Read-only source handles retain their
normal inode locks. No provider, network or hosted authorization is simulated.

All processes are pinned to one CPU; OS caches are retained. Each retained
client executes three rounds. Reopen/rebuild use one round. Every query first
obtains an exact reference signature, warming index pages/posting work. Retained
fresh ranking bypasses the final-result cache; it can reuse authenticated index
work. Retained ordinary ranking permits final-result reuse. One healthy source
handle serializes selected original reads through a mutex while index ranking
is concurrent. Timed queries include exact fixture validation and signature
construction.

| 100k events, ten retained clients, fresh final ranking | Raw main → candidate p95 ms | Zstd main → candidate p95 ms |
| --- | ---: | ---: |
| Common, plain source | 144.8 → 20.9 | 170.2 → 14.8 |
| Identifier anchor, plain source | 42.9 → 27.8 | 81.1 → 36.7 |
| Common, encrypted source | 148.2 → 18.7 | 176.5 → 19.8 |
| Identifier anchor, encrypted source | 41.3 → 27.4 | 91.2 → 30.4 |

Unique/missing queries are retained in the JSON, as are all 1k observations and
dense phrases at 1k. These improvements apply to this retained workload only.
The [distinct/dense index diagnostics](JURO_DISK_QUERY_HOTPATH.md) still fail a
general 100 ms target; these fixtures do not overturn those adverse results.

`reopen` times opening a new source and index, ranking, fetching selected exact
originals, and releasing those new handles. `rebuild` additionally constructs a
fresh index from the actual source before querying. Existing reference handles
remain resident for exact signature comparison, so RSS includes them. Neither
mode evicts OS caches or measures machine boot/storage-cold state.

| 100k events, common query | Raw candidate ms | Zstd candidate ms |
| --- | ---: | ---: |
| Ten concurrent reopened clients, plain source p95 | 1,274.9 | 1,183.5 |
| Ten concurrent reopened clients, encrypted source p95 | 3,591.1 | 3,516.5 |
| One full source reopen/index rebuild/query, plain source | 23,004.6 | 25,621.1 |
| One full source reopen/index rebuild/query, encrypted source | 23,494.2 | 26,027.6 |

Reopening replays source metadata and authenticates its committed prefix. The
100k encrypted source open alone is about 317–349 ms in these observations.
Repeatedly reopening per request does not meet a 100 ms target. At ten reopened
clients, peak process RSS reaches about 566 MiB; one retained source/index uses
about 57 MiB in these cohorts. Index admission still precharges all cache
capacities, about 36.4 MB at 100k. That estimate excludes source handles and
query/build work; it is not a process memory limit.

Storage is the authoritative file **plus** its disposable index. Here 100k
source events contain 8,195,029 indexed text bytes and 101 binary originals:

| Source encryption | Source bytes | Raw index bytes | Zstd index bytes | Source + raw | Source + Zstd |
| --- | ---: | ---: | ---: | ---: | ---: |
| Plain | 27,796,300 | 110,442,470 | 75,658,871 | 138,238,770 | 103,455,171 |
| Encrypted | 31,796,340 | 110,439,707 | 75,629,395 | 142,236,047 | 107,425,735 |

These totals do not support a “third of the space” claim. The index duplicates
source text and has per-row metadata/encrypted framing overhead. Initial index
construction takes 17.6–20.5 seconds at 100k; the source generation takes 1.50
seconds plain and 2.28 seconds encrypted with the stated group durability.
Those aggregate generation times are not individual append latency or a
250 ms incremental-refresh result. There is no competitor-storage, hosted,
production-concurrency or one-million-actual-source acceptance claim here.

Reproduce deliberately, using a fresh private scratch directory and serializing
builds/timings with the shared benchmark lock:

```sh
cargo build --locked --no-default-features --release -p oojuro-engine --example profile_juro_projection
profile_juro_projection source /private/new-fixture 100000 plain
profile_juro_projection build /private/new-fixture 100000 raw 1 1 plain
profile_juro_projection retained-fresh /private/new-fixture 100000 raw 10 3 plain
profile_juro_projection reopen /private/new-fixture 100000 raw 10 1 plain common
profile_juro_projection rebuild /private/new-fixture 100000 raw 1 1 plain common
```

The keys embedded in this synthetic diagnostic are public fixtures, never
production keys. Remove only the owned generated fixture/cache directories
after retaining receipts. Full source snapshots, captures and customer history
must never be committed to this repository.
