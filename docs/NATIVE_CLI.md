# Explicit native CLI review slice

`oojuro-native` is a separate Linux executable. It does not change `oojuro`,
`oojurod`, their default stores, sockets, daemon, MCP or forwarding. Every command
requires `--root DIR`; nothing discovers a native home from the environment.
There is no migration or production selection. Build with:

```sh
cargo build -p oojuro --bin oojuro-native
```

## Setup and local ownership

Provide a private 0600 JSON policy file. All fields are required; unknown fields,
unknown version, invalid patterns or unreadable policy refuse capture.

```json
{"version":1,"record":true,"paused":false,"record_thinking":true,"index_thinking":true,"exclude":["*.pem",".env"],"denied_tools":[]}
```

```sh
oojuro-native setup --root ./native-review --harness claude-code \
  --writer claude-code:-synthetic --agent 7 --policy ./private-policy.json
```

The root must be new, with a trusted existing parent. Setup creates a 0700 root,
an OS-random 32-byte 0600 key, strict 0600 metadata and a copied 0600 policy file,
syncs files/directories, then opens the real sealed authority. Existing roots are
never overwritten. An interrupted setup retains its existing files/key for an
explicit recovery decision; setup does not silently replace that key.
Keys are never command arguments, environment values or printed output.
Wrong file type, final symlink, non-owner UID, non-private modes, wrong key or
mismatched writer/agent/harness/allocator refuse access. Parent directories remain
trusted; this is not a hostile-filesystem sandbox or a hosted authorization service.
The key and policy remain sensitive local files, protected by owner permissions;
this is not key wrapping, OS keychain, backup management or cryptographic erasure.

For Codex use `--harness codex` with its actual project writer; for Claude use
`claude-code` and its project writer. Writer/agent/first-ID metadata bind the
existing native identity exactly. The optional `--first-id` defaults to 1.

## Explicit source and raw authorization

```sh
oojuro-native capture --root ./native-review \
  --source ./fixture/.claude/projects/-synthetic/session.jsonl \
  --source-id 34343434343434343434343434343434 --allow-raw-source
```

The source-generation token is caller-chosen 16-byte lowercase hex. Reuse the
same token on retry; replacement, truncation or edited retained prefixes need an
explicit new generation. No path is auto-discovered. Missing raw consent refuses
before source inspection. Record-off/paused or unreadable policy also stops before
source access. Errors are bounded codes and do not echo paths, keys or payloads.

**Raw consent means every original byte may be retained, including excluded paths,
secrets and thoughts.** Thinking/path/tool exclusions govern parsed receipts only;
they do not authorize or sanitize raw retention. The source is never modified.
Source files must remain append-only during a pass. This explicit flag records
the caller's decision, not independently verified human authorization or revocation.
Claude secondary tool-result files remain unread, with explicit stub metadata.

The copied policy is parsed once per capture command. Its exact bytes receive a
content-derived immutable identity; even whitespace changes that identity. There
is no dynamic config discovery/reload. An unfinished source line must finish under
its original policy bytes. Exclude uses the existing path-glob/known tool-path
helpers; native snapshots require explicit paths rather than home expansion.
`denied_tools` matches exact parsed call names. Missing call IDs/inputs are refused
by the Claude adapter before acknowledgement. Reported roles and origins are data.

Each successful command reports the real committed generation's raw/parsed ends
and original event IDs. `committed_unavailable` reports the committed batch outcome
on stdout and exits 3; close/reopen/rebuild before reading or capturing more. It is
not a complete-file success or permission to allocate new IDs. Earlier blocks may
be committed when later source/policy/bound checks refuse; remaining source bytes
stay in the original file.

## Search and original receipts

```sh
oojuro-native search --root ./native-review --query battery --k 10
oojuro-native get --root ./native-review --id 1
oojuro-native timeline --root ./native-review --entries 100
oojuro-native export --root ./native-review --entries 100 --bytes 1048576
oojuro-native stats --root ./native-review
```

Outputs are JSON. Search returns actual native lexical IDs/scores and reported
event times; `get` returns the original receipt/event. Receipt reads use PR19's
committed-prefix snapshot, ordinal `--start/--end` range and authenticated
`--cursor` continuation. Appends after a snapshot are invisible to its cursor;
a new view includes them. Tampered, foreign or mismatched-range cursors refuse.
Withheld receipts never reconstruct denied text. No new timestamp, key-history,
audit or causal-truth model is inferred. See [receipt API](NATIVE_RECEIPT_API.md).

Read output limits are 2..16MiB including the final newline; default 1MiB. Pages
contain 1..1000 receipts. An oversized first receipt fails without partial stdout;
retry with an explicit larger byte cap. Search accepts 1..1000 hits and optional
`--from-us/--to-us` bounds over current reported event times. Cursors and authorized
record output are sensitive; applications must not treat them as log diagnostics.

`search --cache DIR` selects the existing encrypted native segment cache and
reports Loaded/Extended/Rebuilt. `cache-merge --cache DIR` invokes its real bounded
merge. The directory must be dedicated to the cache. Authority is independently
opened first, and stale/unhealthy state is never served as current. The command
can hold both bounded in-memory projections; no RSS, latency or scale improvement
is claimed. The cache is disposable; it does not acknowledge writes.

## Separate raw export consent

```sh
oojuro-native restore-raw --root ./native-review \
  --source-id 34343434343434343434343434343434 \
  --out ./selected-raw.jsonl --allow-raw-export
```

Without the flag no destination is created. Export creates a new 0600 file and
never overwrites a destination or follows its final symlink. It syncs the result
and parent directory before success. An I/O failure can leave a partial private
destination; no success is reported and retry requires another explicit path.

## Verification boundary

Real subprocess tests use only synthetic sources, keys, policies and private roots.
They exercise both adapters, process reopen, exact raw bytes, receipt pagination
across append, native cache use, failed consent, protected keys/metadata, wrong
key/scope, immutable partial-line policy, withheld-result recovery, output bounds,
cursor tampering and malformed-call privacy. Exact executed evidence is in the PR.
No real user log, provider, AWS, daemon, deployment or training action is exercised.
The foundation SPEC's full format/erase/audit/retention, scale/performance and
hosted/AZ gates remain separate. This is a reviewable runnable surface over the
current raw-authorized v1 libraries, not a completed commercial primary store.
