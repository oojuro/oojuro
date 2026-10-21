# Local PostgreSQL and pgvector comparison

Measured 2026-10-19. This is a small, single-client experiment on approved redacted
engineering history (the pilot corpus). Nine has faster exact reads and retrieval in this run, but
PostgreSQL has faster small writes and retains more reference evidence. **Overall
superiority is not established.** No hosted Supabase project was used.

[Aggregate report](benchmarks/nine-postgres-pilot-2026-10-19.json).
[Competitor scope and sources](NINE_COMPETITORS.md).

## Boundary

Both servers run in local Linux containers limited to two CPUs and 512 MiB RAM.
The Windows Rust driver uses one persistent loopback connection to each server.
Nine uses a length-prefixed JSON benchmark protocol; PostgreSQL uses its native
protocol and the official Rust pgvector binary encoding. PostgreSQL is 18.6 with
pgvector 0.8.7. Both index text and time. PostgreSQL also maintains native GIN
full-text and key/time indexes, with LZ4 enabled on its text column.

Each source write receives an individual durable acknowledgement: Nine syncs its
journal; PostgreSQL has fsync, synchronous commit and full-page writes enabled.
This does not establish equal transaction semantics, crash/power-loss recovery,
replication or concurrent performance. Nine keeps its source records and derived
indexes in RAM after replay; PostgreSQL supports a broader disk-backed database.

The real corpus has 51 conversation passages and 14 frozen, retrospective
questions. Their 28 reference excerpts form 21 question/source pairs, drawn from
only eight distinct source records. Coverage counts those pairs, so one source
needed by different questions is assessed separately. This is a very small
quality probe. The corpus excludes raw tool payloads. An additional 300 public deterministic
text fixtures resemble tool output at 1 KiB, 8 KiB and 64 KiB. They measure payload
size effects, not compression or accuracy on real raw tool histories.

## Individually acknowledged writes and exact reads

Median milliseconds, including client encoding, transport and the server's
acknowledgement or complete original record response. Writes have 51 or 100
observations per group; reads have five passes, checked against every original.
The aggregate includes p95 and raw observation counts.

| Payload | Nine write | PostgreSQL write | Nine exact read | PostgreSQL exact read |
| --- | ---: | ---: | ---: | ---: |
| Real conversation passages | 3.168 | 2.098 | 0.712 | 0.874 |
| Synthetic 1 KiB | 2.281 | 1.761 | 0.746 | 0.920 |
| Synthetic 8 KiB | 4.402 | 3.826 | 0.813 | 1.002 |
| Synthetic 64 KiB | 53.300 | 57.481 | 0.977 | 1.219 |

All 3,510 full-record read checks passed. These mixed results do not support a
claim that Nine has superior general read/write speed. The large-write results
also need a native Linux client control before interpreting their network tail.

## Complete retrieval

Both systems apply historical visibility and latest eligible revisions, collect
up to 64 candidates, hydrate exact originals within a 16 KiB serialized evidence
budget and persist a query receipt. No result cache is used here. A warm-up pass
is excluded. Each local arm has 420 timed queries; each fresh embedding arm has
14 actual encoding requests. All repeat packets were stable.

The dense arm uses frozen 3,072-dimensional Gemini document vectors, exact
pgvector cosine search and reciprocal rank fusion with PostgreSQL full-text
search. There is no ANN recall loss on this small corpus. Document encoding is
outside the bare text-write measurement. Query embedding is included only in
the explicitly labeled fresh rows.

| Retrieval arm | Sources allowed | p50 ms | p95 ms | Question/source pairs retained / 21 | Questions with all evidence / 14 | Mean context bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Nine, no models | 2 | 3.548 | 7.133 | 15 | 10 | 1,522 |
| PostgreSQL full-text | 2 | 4.287 | 8.276 | 17 | 10 | 1,970 |
| PostgreSQL hybrid, existing query vector | 2 | 4.580 | 9.211 | 18 | 11 | 1,924 |
| PostgreSQL hybrid + fresh Gemini query embedding | 2 | 417.893 | 530.395 | 18 | 11 | 1,924 |
| Nine, no models | 8 | 3.448 | 7.114 | 18 | 11 | 3,857 |
| PostgreSQL full-text | 8 | 4.417 | 9.542 | 21 | 14 | 5,668 |
| PostgreSQL hybrid, existing query vector | 8 | 4.818 | 9.149 | 21 | 14 | 5,461 |
| PostgreSQL hybrid + fresh Gemini query embedding | 8 | 434.524 | 500.317 | 21 | 14 | 5,461 |

Gemini latency materially changes the complete hybrid path. Nine avoids that
call, but it also misses evidence retained by both PostgreSQL baselines. A
shorter context is not a token-efficiency victory when useful evidence is lost.
PostgreSQL full-text also avoids embedding calls and retains all reference
sources at eight results here. Eliminating the encoder alone does not establish
a reason to adopt a new engine.
Context bytes are measured; actual answer-model input tokens, answer accuracy
and repeated agent-task outcomes are not measured in this run.

## Storage and diagnostic runs

The mixed 351-record Nine archive is 3,625,200 bytes versus 7,499,783 raw text
bytes. PostgreSQL's measured table plus indexes occupy 22,183,936 bytes. This is
not a total residency comparison: Nine's RAM index, PostgreSQL WAL, shared
memory, replication and backups are excluded. On the smaller real-only corpus,
Nine's archive previously exceeded the raw text size.

The [initial transport run](benchmarks/nine-postgres-pilot-2026-10-19-initial.json)
is retained. It measured approximately 44 ms Nine reads and 49 ms retrieval, and
67 ms PostgreSQL hybrid retrieval with text-encoded vectors. Nine then combined
its length header and payload into one write; PostgreSQL switched to the
[standard binary vector type](https://github.com/pgvector/pgvector-rust).
The rerun above uses both improvements. Those initial numbers describe the
earlier adapters and are not the final engine comparison.

Another attempt stopped on the populated-store guard before any benchmark source
writes. The corrected run used a fresh Nine container and a fresh PostgreSQL
schema. Database services remained local; 56 Gemini query requests were made
across the two completed runs under the existing shared provider budget. Nine
made zero embedding and zero LLM calls.

## Reproduction

This run used the research lab (`oojuro-lab`, not part of this repository), its
optional `competitors` feature built in release mode, and a benchmark-only Nine
network image. The setup is recorded here for the record; start a disposable
`pgvector/pgvector:0.8.7-pg18-bookworm` container with the limits and durability
settings above. Configure `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE` and
`PGPASSWORD` in the process environment without logging credentials. The driver
only accepts loopback and the dedicated `ninebench` user/database.

```text
cargo build --release -p oojuro-lab --features competitors
oojuro-lab nine-pg-bench --dataset data/private/pilot-benchmark-v1 --run-dir data/private/new-pg-run --output results/new-pg-run.json --repeats 30 --nine-address 127.0.0.1:55434 --live-query-embeddings
```

The final flag requires a locally configured Gemini key and the existing shared
budget ledger. Omitting it runs only the local/cached-vector arms. Use fresh
store/schema paths. The private dataset and per-question packets are not part of
the repository. No production database or hosted Supabase API is touched.
