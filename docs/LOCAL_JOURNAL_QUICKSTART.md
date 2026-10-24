# Store and search a local agent journal

The native Record CLI stores typed agent records in an encrypted local journal.
It provides durable append, exact retry receipts, original-text reads, key
versions, time filters and lexical search. This is the local path for applications
currently keeping agent records in JSONL files or SQLite tables. It runs without
an account, cloud connection, daemon or model.

This source candidate is pre-release and Linux only. Build from this checkout
with stable Rust (edition 2024) and a C compiler:

```sh
cargo build --locked --release --no-default-features -p oojuro --bin oojuro-native
```

## Append JSONL and retrieve the originals

Run in a trusted directory. The sample writes synthetic data only. `umask` keeps
input/output files private; setup creates a new private root and key. Keep the
root, its key and journal together when making a quiescent local copy.

```sh
umask 077
mkdir journal-demo
cd journal-demo
OOJURO_NATIVE=../target/release/oojuro-native
"$OOJURO_NATIVE" record-setup --root ./journal \
  --writer my-app --owner 1 --adapter my-app-v1

cat > batch-001.jsonl <<'JSONL'
{"agent_id":"assistant-1","parent_id":null,"valid_at":10,"kind":"explicit","tool":null,"status":null,"exit":null,"key":"decision","text":"Use SQLite for the billing tables.","attrs":{"source_id":"decision-1"},"indexed":true}
{"agent_id":"assistant-2","parent_id":null,"valid_at":20,"kind":"explicit","tool":null,"status":null,"exit":null,"key":null,"text":"Battery tests pass after replacing the cable.","attrs":{"source_id":"note-2"},"indexed":true}
JSONL

"$OOJURO_NATIVE" record-append --root ./journal --input ./batch-001.jsonl \
  --input-format jsonl --request batch-001 --allow-unredacted-records
"$OOJURO_NATIVE" record-search --root ./journal --query battery --k 10
"$OOJURO_NATIVE" record-get --root ./journal --id 2
"$OOJURO_NATIVE" record-get --root ./journal --key decision
"$OOJURO_NATIVE" record-export --root ./journal --versions all > export-page.json
```

Use an absolute `OOJURO_NATIVE` path if running outside the checkout. The append
receipt contains assigned IDs and engine timestamps. Run the identical append
again: it returns that same receipt without adding records. Reusing `batch-001`
with changed values refuses. If an append loses its response, retry the original
file, request and batch boundaries after reopening; an error alone does not
establish that the write failed to commit.

The raw-retention flag acknowledges that submitted fields may contain unredacted
data. Apply your application's recording and redaction policy before submission.
The CLI reads only the explicitly selected private file; it does not import
existing agent history or open file paths mentioned in record text.

## Map an existing log or SQLite table

| Existing value | Typed Input field |
| --- | --- |
| Original text | `text` (UTF-8 string) |
| Event timestamp | `valid_at` (integer UTC microseconds) |
| Reported agent/session | `agent_id`, optional `parent_id` |
| Versioned logical key | `key`; use explicit `null` for independent events |
| Source row ID and application metadata | `attrs` (JSON object) |
| Record classification | `kind`; use `explicit` for application notes |
| Eligible for content search | `indexed` (explicit boolean) |

Emit one complete Input per line as shown above, or a JSON array with the default
input format. Optional tool/status/exit fields may be `null`. SQLite callers
should select the intended columns in a read transaction and explicitly convert
their units and nulls before writing the input file. Original source IDs can be
retained in `attrs`; Oojuro assigns its own IDs and ingestion clock. There is no
automatic SQLite schema conversion or preservation of an old database's row IDs
as Oojuro IDs. Keep each batch at most 1,000 records / 32 MiB serialized, and
retain its original request ID and typed inputs for retries.

`record-export` returns one bounded JSON page with a watermark and optional
`next`. A non-null token means more data remains. Pass it unchanged to
`record-continue --root ./journal --cursor TOKEN`, saving each page until `next`
is null. Continuations keep the same snapshot across later appends. Treat pages
and cursor tokens as private data. This export includes assigned provenance and
timestamps; it is not an append Input file or a byte-for-byte JSONL backup.

Use the [explicit file adapter](../crates/oojuro-engine/README.md#explicit-file-adapter-unix)
when the original file bytes, including encoding and whitespace, must also be
retained. That is a separate raw journal and recording-policy decision.

## Choose the right boundary

This bounded pre-release namespace has one owning writer, at most 100,000 records
and 128 MiB serialized replay metadata. Search reconstructs its in-memory index
per CLI process. SQL joins, relational constraints, arbitrary updates and
multi-writer transactions remain application/database responsibilities. The
current evidence does not establish universal speed or memory improvements over
SQLite or indexed JSONL; see the [benchmark methodology](BENCH_HARNESS.md)
and the benchmark reports in this repository.

The [Record API contract](NATIVE_RECORDS.md) documents atomic batches, clocks,
recovery, encrypted storage and snapshot limits. For hosted capture and shared
project-history reads, use [combined agent setup](COMBINED_AGENT_SETUP.md) with
the matching hosted service. A local Record root is not automatically uploaded
or converted into a hosted capture journal.
