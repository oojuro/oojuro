# Explicit direct Record commands

Linux `oojuro-native record-*` composes the current [Record SDK](NATIVE_RECORDS.md)
in a fresh, separate root. Existing [capture commands](NATIVE_CLI.md) retain their
v1 format and behavior. Nothing migrates a capture directory, chooses a default
store, discovers input, starts daemon/MCP, forwards data or calls a provider.

## Protected setup

```sh
oojuro-native record-setup --root ./record-review --writer synthetic-writer \
  --owner 7 --adapter reported-sdk --first-id 41
```

The parent must exist and be trusted. Setup requires a new root and creates it
at 0700 with a random 32-byte 0600 key and strict 0600 `record.json`. It opens the
real sealed `oojuro-record-2` journal and returns its initial protected watermark.
Existing roots/keys are never overwritten; capture and Record setup refuse each
other's roots. Interrupted setup retains private files for an explicit recovery
decision, with no replacement key generated.

Every command requires `--root`. Keys never come from argv/environment/stdout.
Owner UID, private modes, regular no-follow key/config/input files, config version
and immutable journal identity are checked. Parent directories remain trusted.
This is not a hostile-filesystem sandbox, key wrapping, external rollback witness
or hosted authorization. A missing journal refuses rather than creating a
successful empty view. An application with no newer known watermark cannot
detect a healthy older whole-directory restore.

`--owner` is the SDK's numeric owner partition, not the reported running agent.
`--first-id` defaults to 1. Writer, owner, allocator and adapter bind the namespace;
editing metadata cannot relabel existing records. Returned adapter is that
header-bound caller label, not harness attestation. Reported agent/parent, attrs
and recalled content are unverified data, never instructions.

## Explicit typed input and exact request identity

Write a **private 0600** JSON array. One complete Input looks like:

```json
[{"agent_id":"reported-instance","parent_id":null,"valid_at":10,"kind":"explicit","tool":null,"status":null,"exit":null,"key":"memory","text":"synthetic battery decision","attrs":{},"indexed":true}]
```

```sh
oojuro-native record-append --root ./record-review --input ./private-input.json \
  --request original-operation --allow-unredacted-records
```

This is direct caller-authorized typed storage, not harness raw capture. The flag
is required **before inspecting input**, including on retry. It acknowledges that
exact submitted text/attrs may contain unredacted secrets or thoughts. No redactor
or exclusion policy runs implicitly: apply your authorization/redaction policy
before submitting. Source files are never modified. The flag records a caller
decision, not independently verified human consent or revocation.

`key` (string or null) and `indexed` must be explicit. Unknown fields, malformed
JSON, unsupported kinds/status, invalid names, permissions or SDK bounds refuse.
Input is at most 32MiB serialized /1000 records; text at most 16MiB per record,
attrs 4KiB with nesting/work bounds. The bounded SDK namespace remains 100,000
records /128MiB serialized replay metadata, not an exact RSS budget.

Supply a stable request identifier and retry the **same typed input**. Never use
a credential as that identifier. No operation ID, automatic retry or re-redaction
is generated. Same request plus identical typed values returns original IDs,
engine-assigned `recorded_at` and exact receipt through process reopen. Changed
typed input conflicts. A late backdated `valid_at` need not become the key winner.
Store errors may be ambiguous after durable append: reopen/retry original intent;
a missing success response is not proof that nothing committed.

## Reads and independent time cutoffs

```sh
oojuro-native record-get --root ./record-review --id 41
oojuro-native record-get --root ./record-review --key memory --valid-at-us 25
oojuro-native record-history --root ./record-review --key memory --entries 100
oojuro-native record-range --root ./record-review --versions current
oojuro-native record-export --root ./record-review --versions all --bytes 1048576
oojuro-native record-search --root ./record-review --query battery --k 10
```

Get requires exactly one ID or key. ID lookup returns the original logical
version; key lookup selects the SDK's newest `(valid_at, recorded_at, sequence)`
version. History keeps all versions; range/export require `--versions current|all`.

Fresh reads accept exact UTC-microsecond SDK fields: `--from-us` is the lower
valid-time bound, `--valid-at-us` its upper cutoff, and `--known-at-us` the
independent recorded-time cutoff. Defaults are unbounded. These are not a date
grammar, deletion model or general transaction history. Key winners are selected
before lower bounds/metadata filters, so excluding a winner never revives an older
version.

Range/export/search accept `--kind`, `--tool`, `--status ok|error` and
`--reported-agent`. They filter typed data, not permissions. Attrs are not indexed;
duplicate text retains original IDs. `indexed:false` is stored-only, not deletion
or withholding: explicit time/key reads return it; nonempty content queries do not.

Page results contain a protected watermark and optional `next` token. Continue
with the dedicated command:

```sh
oojuro-native record-continue --root ./record-review --cursor TOKEN --entries 100
```

That token binds original scope, history/range mode, key, cutoffs, filters and
prefix. New key/time/filter/version selectors are rejected. It reads the same
snapshot after append/reopen; later appends stay invisible. Tampered/foreign/
unavailable-prefix tokens refuse. Tokens/content are sensitive output, not logs.

Read `--bytes` defaults to 1MiB, allowing 2..64MiB including final newline. Pages
permit 1..1000 entries and search 1..1000 hits. Whole serialized records, pages,
cursors and results must fit; an oversized first row is never silently skipped,
and failed output is never partially printed. Missing get still needs 5 bytes
for `null` plus newline. Errors are bounded codes without bodies, paths or keys.
Search rebuilds the real memory-only Record index each process. No persistent
Record cache, performance/scale improvement or automatic write-path projection
refresh is claimed.

## Verification boundary

Actual subprocess tests use only synthetic private roots/typed files: exact
ID/time retries through reopen, independent cutoffs, key history/duplicate text,
adapter metadata, typed filters/stored-only literals, protected continuation
across append, malformed/private-file/conflict refusal, no conversion/overwrite,
bounded output and sealed-at-rest scans. The PR records exact executed outcomes.
This does not establish full SPEC, raw+Record migration, automatic read-your-writes
facades, daemon, audit/erasure/retention, power loss, hosted/AZ or live acceptance.
