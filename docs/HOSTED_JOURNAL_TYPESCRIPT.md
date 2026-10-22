# Hosted Journal TypeScript client

The existing experimental OojuroClient has an explicit hosted flat-record surface.
It does not enroll a namespace, migrate legacy Trace records, capture local logs,
redact input, or verify caller-reported provenance. Use a separately authorized,
already-enrolled Journal root.

- journalRoot() returns the exact schema/project/namespace root.
- prepareJournalRecord({requestId, consent:"intentional-user-file",
  expectedRoot, writer, adapter, input}) freezes a native typed record.
  Required key:null means unkeyed; indexed:false remains stored-only.
- prepareJournalDelete({requestId, expectedRoot, writer, id}) deletes one ID.
- prepareJournalForget({requestId, expectedRoot, writer, key}) tombstones existing
  key versions. Future writes may reuse the key; this is logical deletion.
- journalPage({expectedRoot, query, cursor?, maxRecords?, maxBytes?}) returns a
  bounded page. The inner watermark pins the snapshot; the outer watermark is
  the current authenticated prefix and may be later. Reuse the exact root/query
  with its cursor. Later appends are excluded; changed authorization/deletion or
  changed root/query can refuse continuation.
- journalStats({expectedRoot, query}) returns counts and valid-time bounds.
- prepareJournalPeriods({requestId, expectedRoot, query, widthUs, maxPeriods,
  minGeneration?}) prepares a billed search over UTC epoch buckets.

## Identity, retries, and privacy

Prepared writes and periods requests own their encoded original body and minimum
generation. Every send() makes exactly one attempt. No identity is allocated or
changed automatically. After a lost/invalid ACK, retry the same prepared request,
or recreate it from retained original input and the same caller request ID.
A rejected retry cannot prove an earlier write did not commit. All write failures
remain outcome:"unknown". Page/stats failures are read-only (outcome:"none").
Periods retries use server status-only search_pending, search_completed, or
search_failed; they never recompute or return a past result.

Journal payloads must be plain own-data JSON objects/arrays. Accessors, custom
prototypes and function-valued toJSON are rejected without calling them. Snapshot
preflight bounds nodes/depth/keys and counts JSON escape/UTF8 bytes before encoding;
prototype-looking attribute names remain data. Existing transport configuration is
unchanged.

Private body/token data are absent from inspection/JSON representations and fixed
SDK errors. The caller is responsible for protecting its retained original input.
Server authorization and key-version selection are server responsibilities; shape
validation is not an independent proof of server honesty.

## Exact wire bounds

Native integer fields accept only exact JS safe integers. Larger i64/u64 values,
fractional/exponent source lexemes in integer fields, and malformed Unicode refuse
rather than silently round. General JSON attribute numbers may be fractional.

Current Journal JSON POSTs are at most **1 MiB encoded**, or a lower configured
client request limit; a server configuration can restrict them further. The
existing client response ceiling is at most **10 MiB**, or its lower configured
limit. Page maxBytes is 1024 through 10 MiB minus 1024; maxRecords is 1..1000.
The native 16 MiB text limit does not imply equivalent HTTP capacity, especially
after JSON escaping. This slice does not implement the separate framed transport.

Queries use explicit as_of:{from,valid_at,known_at}, nullable generation/writer,
and mode:{mode:"Range",versions:"current"|"all",filter:{...}},
{mode:"History",key}, or {mode:"Id",id}. History requires a writer.
Every response is fully consumed and validated before returning a completed value;
truncation, abort, schema mismatch, and bounds refuse without partial results.

## Verification

Offline Node 24 fixtures exercise original byte freezing, recreated retries,
unknown outcomes, cursor/schema identity, read-only failures, native integer
lexemes, consent, request/response budgets, malformed ACKs, and private-safe errors.
All pre-existing client/file suites and strict cached TypeScript checks also run.
Final evidence: 12 Journal cases and 59 pre-existing SDK cases pass, with strict
cached TypeScript fixtures. The actual-wire case consumes seven JSON operations
from test/support/journal-wire.json, captured by the cloud Rust test
journal_http_actual_wire_contract_fixture_for_scoped_sdk. It retains original
response bodies and actual content-type/generation headers; the separate eighth
descriptor operation is deliberately deferred. This is synthetic HTTP fixture
compatibility, not a Node-to-production network test. The own-data guard removal
negative control fails, and restoration passes.
No live provider, AWS, deployment, or production acceptance is implied.
