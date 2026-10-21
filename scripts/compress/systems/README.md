# System comparators (bytes on disk for the s64 agent-text corpora)

Matched storage-system baselines for the compression track. Every script reads
`.scratch/data/s64/<corpus>.jsonl` (`raw` = size of the sibling `<corpus>.txt`), stores
seq/session/turn/kind/valid_at/text, measures bytes on disk, reads texts back and
compares sha256, and appends one JSON object per (system, variant, corpus) to
`.scratch/out/sys-comparators.jsonl`. No dataset text is embedded in the scripts.

| script | system | image / package | digest / version |
|---|---|---|---|
| `clickhouse_run.py` | ClickHouse MergeTree | `clickhouse/clickhouse-server:26.9.23.15` | `sha256:d40ed32b22730ef2c1ba995e8affd28cf324b47399323f8b4c43f2cdcb7c35b4` |
| `victorialogs_run.py` | VictoriaLogs single node | `victoriametrics/victoria-logs:v1.53.0` | `sha256:251121fa882af99b95ba0c230a4a2f412ea602d2698c64a96c58dc9842bb755d` |
| `opensearch_run.py` | OpenSearch single node | `opensearchproject/opensearch:2.19.6` | `sha256:be67c71b8d0e5edf1e9825145501c0136c3b8221206e1ce782ce17b04d0280b5` |
| `clp_run.py` | CLP clp-s | `ghcr.io/y-scope/clp/clp-core-x86-ubuntu-jammy:main` (only tag published; pinned by digest) | `sha256:b2d550bf9ff60e431118658cc4916455bd7ce8da3adc27b164f388e3f92d48de` (label revision `e5351214fced30b3e817f761cbf75210968f962a`, created 2026-10-19) |
| `loki_run.py` (optional) | Grafana Loki single binary | `grafana/loki:3.7.8` | `sha256:1107dd5274e0ada47e42472b7a7e71f3b2a2fe878878108f3e2f9e51528f0193` |
| `clp_search_run.py` | clp-s search over level-19 archives (swe, ohjson) | same CLP image | same digest |
| `rocksdb_run.py` | RocksDB 9.8.4 | PyPI `rocksdict==0.3.29` (bundles RocksDB 9.8.4, read from its LOG) | - |

`common.py` holds the shared loader, sha256 verification, the fixed random samples
(`random.Random("verify-<corpus>")`, `random.Random("read-<corpus>")`), the bench lock and
the output writer.

## Run

`run_all.sh` runs everything in order: size-only builds first (no bench lock, at most two at a
time), then the timed point-read phases one system at a time. `clickhouse_run.py`,
`opensearch_run.py` and `rocksdb_run.py` take `--build-only` (stage rows in
`.scratch/sys/<system>[-state]/pending.json`, keep the container/store) and `--resume` (reuse
the staged rows and live store, then run the read loops, emit, clean up). Corpus names may be
passed as positional args. `STREAM=none victorialogs_run.py` runs the single-stream contrast.

Containers are named `oojuro-compress-<system>` (helpers `-rm`/`-du`), capped
`--memory 2g --cpus 2`, ports bound to 127.0.0.1, data dirs under `.scratch/sys/<system>/`,
removed when each script finishes.

## Method notes

- Builds are size-only steps and run outside the bench lock, so `load_s` is null in every row.
- Point reads (ClickHouse, OpenSearch, RocksDB): 200 queries x 20 random seqs, client-side
  wall time per 20-record query, one `flock(2)` hold of `$OOJURO_BENCH_LOCK` (the
  same lock `flock(1)` takes) around each 200-query loop only, then a 15 s idle gap before
  the next hold so other sessions' waiters get the lock. ClickHouse = one
  `SELECT seq, text WHERE seq IN (...)` over keep-alive HTTP; OpenSearch = one `_mget` of 20
  ids; RocksDB = one in-process `multi_get` of 20 keys.
- Verification: every record for `ohsft` (and every record for VictoriaLogs and CLP, whose
  read-back returns everything anyway), a fixed 1000-record sample for the other corpora.
- ClickHouse: `variant` lz4 / zstd3 / zstd19 use default `index_granularity` (8192 rows,
  10 MiB adaptive); `zstd19-g256` sets `index_granularity = 256` as a point-read contrast.
  `disk_bytes` = all columns' active parts; the text column alone is in `extra.text_col_compressed`.
- RocksDB stores only the text value (seq is the key); the other four fields are not stored.
- OpenSearch store stats are cached (`index.store.stats_refresh_interval`, 10 s); the size is
  read only once `pri.store.size` equals `du -sb` of the shard's Lucene dir and is stable.
- VictoriaLogs: `stream=session` (as specified) and `stream=none` (no `_stream_fields`, one
  stream) contrast. `_time` = 2026-01-01T00:00:00Z + seq ms (valid_at is synthetic and up to 5.8e9,
  outside the ingest window as seconds); `-retentionPeriod=100y`.
- Loki: one stream `{corpus=...}`, the five other fields as structured metadata, line = text,
  ts = 2026-01-01Z + seq ms; `chunk_encoding` snappy (default) and zstd; size = chunks dir
  (chunk objects + shipped tsdb index) after `/flush` and a graceful stop; WAL reported in notes.
- CLP: `clp-s c --compression-level 3|19`, size = `du -sb` of the archives dir, verified with
  `clp-s x --ordered` (all records).
- Timed point reads were run only for corpus `swe` (`READ_CORPORA`, default `swe`), and only
  RocksDB got them: on 2026-10-20 the shared bench lock had one ~80-min holder and ~18-36
  queued jobs, so the OpenSearch and ClickHouse swe loops and the clp-s search passes were
  not run (rows say so in `notes`; `SKIP_READS` / `SKIP_TIMING` emit staged rows without timing).
  Re-run later with `clickhouse_run.py --build-only` then `--resume` (same for OpenSearch)
  and `clp_search_run.py`.
- clp-s search: `clp-s s -i --count <archive> 'text: *<token>*'` via `docker exec` into one
  long-running container; queries from `.scratch/out/identifier-queries.json`; match counts are
  checked against a Python case-insensitive substring count per record.
