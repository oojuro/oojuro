# Explicit native local read service

This opt-in Linux service exposes existing Record-v2 or Capture-v1 authority over
a protected same-user Unix socket. Existing legacy Journal/TraceDb daemon and
MCP paths do not implement this interface and are never a fallback. Managed-v3
is not an accepted namespace here; its operator and rank-view APIs are separate.

## Start and query

Initialize the desired namespace through the existing explicit native setup
command before serving it. The authority root must remain owned/private and its
existing key/config must match the authenticated original header.

~~~sh
oojuro-native serve --root /private/record-root --namespace record-v2 --socket /private/socket-dir/read.sock
printf '%s' '{"op":"search","query":"topic","k":10}' | oojuro-native read --root /private/record-root --namespace record-v2 --socket /private/socket-dir/read.sock
oojuro-native mcp --root /private/record-root --namespace record-v2 --socket /private/socket-dir/read.sock
~~~

The socket parent must already exist, be owned by the current effective user
and have mode0700. The socket is mode0600. Paths are explicit; there is no
discovery, anonymous link, HTTP listener or network binding. Symlink components,
ancestors owned by another non-root user, unsafe writable ancestors and an
existing socket/file are refused. Ancestors must be owned by root or the current
effective UID; a mode0755 foreign-owned directory is not trusted. The service
never automatically removes a pre-existing path. Normal SIGINT/SIGTERM shutdown
removes only its own unchanged socket inode. A killed process may leave a stale
socket; inspect ownership and process state before explicit operator cleanup.

Both server and client check Linux peer credentials against the effective UID.
This authenticates the local OS process/user, not an individual human, agent,
project membership or a hosted grant. Processes already trusted as this UID
can access its protected local files/keys; this service adds no cross-user
sharing authority. The server's protected config fixes writer/owner/adapter.
Wire requests cannot choose those identities, authority paths, keys or policies.

## No bootstrap on reads

RecordLog::open_existing and CaptureLog::open_existing perform an existing-file
preflight before entering a storage constructor. Required directory, manifest,
encryption marker, lock, original payload and Record format marker must exist
as regular files. A header-only explicitly initialized empty authority is valid.
An absent namespace or a recovered-empty missing header cannot be turned into
a fresh successful log.

Startup can recover torn or unpublished storage state, clean orphaned files and
perform existing storage housekeeping; startup is not a filesystem read-only
operation. It never silently bootstraps a missing authority. Request handlers
expose no append, capture, erase, setup, migration, raw restore or path operation.
The service owns the exclusive storage lock; external CLI append/capture cannot
run concurrently. It is a bounded read service, not the future combined native
recorder/watch/reader process.

## Wire contract

One UTF8 JSON request line and one JSON response line per connection:

~~~json
{"version":1,"namespace":"record-v2","request":{"op":"get","id":123}}
~~~

Success contains version1, the configured namespace, ok:true and the actual
native result. Failure contains only a fixed content-free code. The client
validates version/namespace and never reconnects or opens a legacy/direct reader
as a fallback. No payload, parser error text, filesystem path or key is logged.

Every request is at most64KiB. Complete socket output including its envelope
and newline is at most16MiB; requested bytes defaults to1MiB and reserves outer
envelope overhead before native reads. Oversized whole records/results refuse;
nothing is excerpted, truncated or represented as a successful partial object.
k and page entries are each at most1000. Cursor input is at most16KiB.
Original IDs and microsecond times retain exact u64/i64 integer values without
floating-point conversion. Clients must parse these JSON integers losslessly.

The server admits at most8 active peer workers. A busy authority returns a
fixed refusal instead of queueing unbounded work. Nonblocking connection/read/
write loops enforce one absolute5second IO deadline. Native engine computation
has its existing finite state/query bounds but cannot be hard-preempted by this
socket timeout; shutdown may wait for an in-progress bounded engine operation.
No latency, throughput, process-RSS or scale benchmark follows from these bounds.

## Record-v2 operations

- search: required query (empty string is a supported timeline query), optional
  k, from_us, valid_at_us, known_at_us, kind, status, tool, reported_agent, bytes.
- get: exactly one original id or logical key; optional time cuts and bytes.
- history: required key, optional time cuts, entries and bytes.
- range/export: optional versions current/all, time cuts, typed filters,
  entries and bytes. Output is the actual bounded original page and cursor.
- stats: optional versions, time cuts, filters and bytes; counts actual selected
  records, their initial indexed flags and stored-only flags, with real watermark.

Key winners and time/filter visibility come from the existing protected View.
Original full Record data, assigned ID/recorded_at, valid_at and reported
provenance remain unchanged. These labels are stored claims, not independently
verified truth, causal proof or instructions.

Record range/export continuation accepts only cursor, entries and bytes.
The encrypted cursor retains the exact original prefix, scope, time cuts,
filters, version selection and history/range mode. A history page continues
through range/export with that cursor; new filter or key assertions cannot
silently redefine it.

## Capture-v1 operations

search accepts required query plus k, from_us, to_us and bytes. get accepts an
original event ID plus optional start/end receipt ordinals, cursor and bytes.
range/export and stats accept those ordinal bounds/cursor and page limits as
appropriate. Capture cursor resume requires the original start/end selection.

Search/get/page results contain genuine original committed Receipt/Event shapes,
policy/source-generation metadata and recorded line spans. Blank/withheld/
malformed receipts retain their actual output variants. No raw source bytes,
source file descriptors/paths, parser contexts or fabricated flat Records cross
this API. Capture has no logical-key history model and advertises no history
operation. Stats indexed is the original boolean, not a claim of present
lexical dedupe visibility or retrieval quality.

## MCP

The sibling [NATIVE_READ_MCP.md](NATIVE_READ_MCP.md) documents the explicit
2025-06-18 stdio protocol shim. It calls only this protected socket and advertises
implemented read operations. It does not enable a model, install a hook or treat
returned records as instructions. Current newer MCP protocols are not claimed.

## Verification

The actual final composed gate passes **16 checks**: ten pure MCP protocol
fixtures, five binary/socket/MCP stories and one socket-ancestor ownership
policy regression. Package native-bin/integration
lint reports zero diagnostics in changed paths. Local documentation links and
diff whitespace checks pass. All data and keys in these tests are synthetic;
no user logs, live providers, credentials or infrastructure were accessed.

The actual stories cover legitimate initialized empty Record/Capture roots;
missing-authority refusal without recreating a journal; full u64 IDs through
the socket and actual stdio MCP; original Record IDs/ingest times; stored-only/
key/history visibility and protected continuation; scoped stats; original
Capture Receipt/Event shapes without raw-only marker, source path or descriptor
disclosure; authority file byte equality before/after requests; missing socket
without direct-open fallback; private/symlink/socket collision refusal; busy
peer admission; absolute partial-request IO deadline; and refusal of intermediate
root symlinks before either authority opens.

A semantic RED temporarily selected the previous auto-creating RecordLog::open
at daemon startup. The actual test failed because a missing journal and socket
were created. Restoring open_existing passed the same test and all14 checks.
This was a runtime assertion failure, not a compile failure.

Earlier composed fixture corrections retained real checks: the synthetic Claude
capture initially used a mismatched source writer and was correctly refused;
the fixture now follows its genuine harness binding. The actual MCP fixture
was corrected to the documented namespace/data envelope without changing
returned data. Empty-query protocol behavior is also covered.

The automated peer probes run as the current UID. A separate-UID live probe,
production deployment, concurrent recorder ownership, managed-v3 read-service
adapter, hosted transport, scale/performance and exact-RSS acceptance remain
outside this slice. No current/backup key-erasure claim is made.


Independent review found that the original mode-only ancestor check accepted a
foreign-owned mode0755 directory. Its owner could replace descendants despite
the final private socket parent. The path walk now requires each real metadata
owner to be root/current UID before evaluating writable/sticky permissions.
The actual production predicate fails the synthetic foreign-owner0755 case
under the previous check and passes after the ownership fix; root/current
private ancestors and root-owned sticky temporary directories remain valid.
All16 composed checks and changed-path lint pass after both path fixes. This regression
tests the metadata policy, not a claimed live separate-UID/chown acceptance run.

The same checked path walk now protects the new reader's authority-root config
and key startup, before opening authority files. It rejects intermediate
symlinks, foreign-owned ancestors, unsafe writable ancestors and a different
canonical root, then rechecks current-user mode0700 final ownership. Existing
default command loaders remain unchanged. An actual binary regression for both
namespaces failed when this startup check was removed: an intermediate alias
reached authority startup. Restoring it refuses the path without journal changes.
This is separate from the synthetic foreign-UID metadata-policy regression;
no root privileges, chown or live foreign-UID probe were used.
