# Checked exact-record lookup

Oojuro is a generic embedded database. Record append and replay require IDs
contiguous from the header-bound `first_id`. Use checked subtraction/conversion,
verify the selected row ID, then preserve scope, validated snapshot prefix,
two-clock visibility and the exact output-byte budget. Exact reads no longer
scan the complete record vector. Public `record::Index` also materializes ranked
originals through that lookup, without building a full ID-to-row tree per query.

No storage format, encryption, append/acknowledgement path, ranker, public library
signature, dependency or row/memory limit changes. Agent integration remains in
o-journal; this change adds no semantic/vector/LLM/hybrid retrieval. A standalone
`.ojo` file container is not implemented by this change.

## Exact engine-main comparison

Control: engine-only main `5a7c31e`. Candidate runtime: `c19e806`. Fedora
Rust/Cargo 1.99.0, optimized generic x86-64. Each arm was built into a separate
clean target directory; the normal artifact hashes differ. Two fixed AB/BA pairs
use fresh 100,000-record synthetic stores. Each run times 1,000 single-original
and 1,000 twenty-original calls, with fresh scope-checked views. The lexical
query series uses a cached public `record::Index`, 200 unique-token searches/run,
and fresh authority/view checks. These are complete public API calls, without a
cloud network or application serialization.

| Public API p95, microseconds | AB control → candidate | BA control → candidate |
| --- | ---: | ---: |
| One exact original | 129.008 → 4.515 | 127.591 → 4.510 |
| Twenty exact originals | 1540.777 → 50.485 | 1553.167 → 51.409 |
| Cached lexical unique-token search | 6847.552 → 5832.812 | 6822.832 → 5614.763 |

All four runs checked 100,000 original texts, IDs, valid times and fixture
attributes before and after clean reopen; their original-text digests and
ranked ID/score-bit signatures agree. The **103 Record tests passed**, with six
existing ignored measurement/helper cases. Coverage includes non-default and
near-u64::MAX IDs, snapshot-prefix boundaries, two clocks, foreign scope,
exact byte budgets, original metadata, reopen, corruption and existing SIGKILL
recovery fixtures. This campaign is not a new 100k-record crash experiment.

Append was not optimized. Its 100 batches of 1,000 records/run have adverse
p95 observations: **21.530 → 22.444 ms** in AB and **21.979 → 22.868 ms** in BA.
All distributions are retained; no write-performance gain is claimed. Closed
allocated storage is **4,579,328 bytes** in each control and **4,583,424 bytes**
in each candidate. Small apparent-byte differences in encrypted/clocked fixtures
can cross allocation blocks. There is no compression/storage-saving claim.
The index is in memory, not a persistent disk index.

## Separate existing-arm comparison

A preceding diagnostic on the independently reviewed borrowed-projection source
`9a6dc2e` versus lookup source `f93bcd9` ran native-record, SQLite WAL/FULL+FTS5,
indexed raw JSONL and indexed JSONL+Zstd on the same 100k synthetic corpus. Every
arm verified all acknowledged complete Docs/originals on reopen. It is retained
as context with its own source IDs, full file inventories, binary hashes and
scope; those numbers are not attributed to the new engine-main composition.

That native-record arm deliberately resolves every fresh imported row before
refreshing a hosted-style pure projection. Its short unique-token search series
stays around 0.3 seconds, with mixed before/after tails, despite faster exact
reads. SQLite and the indexed JSONL arms query retained FTS5 indexes. This is a
real difference in performed work, not a reason to label every embedded
`record::Index` query as a 0.3-second operation. Native encryption and in-memory
indexes also differ from the plaintext baselines' on-disk indexes. The complete
file totals and RAM/query-work tradeoff must accompany any later comparison.

## Limits and reproduction

Warm, serial, fits-RAM, generated unique-token data on a shared host. AB/BA order
and system load are recorded; OS caches were not dropped and there is no cgroup
isolation. The lexical series is not a representative ranking-quality/mixed-query
benchmark. None of these results establish million-record capacity, ten-client
throughput, cold I/O, hosted p95, generic SQL compatibility, pricing or an SLA.
The 100k-record/128MiB guards remain; [the scale path](RECORD_SCALE_PATH.md) is a
separate persistent/paged indexing milestone.

Use the explicit `profile_record_lookup` example only on a new synthetic fixture
directory. It uses a public fixture key. Build each source into its own target
and freeze the executable SHA-256 before running the fixed coordinator. Inputs
are generated; no user history, credentials, corpora, stores or binaries are
committed. Each profile process has a deadline and is reaped.

```sh
CARGO_TARGET_DIR=/private/control-target cargo build --locked --release -p oojuro-engine --example profile_record_lookup
# Repeat from the candidate checkout with a different CARGO_TARGET_DIR.
python3 docs/benchmarks/record-id-lookup-main-2026-10-24/run_public_profiles.py \
  --control /private/control-profile --candidate /private/candidate-profile \
  --profile-source crates/oojuro-engine/examples/profile_record_lookup.rs \
  --out /private/new-comparison --records 100000 --searches 200
```

[Primary main-based results](benchmarks/record-id-lookup-main-2026-10-24/public-api-comparison.json),
[separate earlier arm results](benchmarks/record-id-lookup-main-2026-10-24/separate-arm-diagnostic.json).
