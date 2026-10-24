# Opt-in atomic Codex capture

This is a local library lane for agents, authorized agents and people using
Oojuro. No daemon, CLI, HTTP, cloud or existing recorder selects it. It consumes
only an explicitly selected Codex JSONL source generation, in one writer journal.
Existing capture and `oojuro-engine::raw::RawJournal` formats remain separate.

## Authority and recovery

`oojuro-engine::capture_log::CaptureLog` uses the existing strict sealed Store,
not a new WAL. A dedicated format header binds writer, legacy numeric agent,
initial event ID and adapter revision. Source registrations seal their original
path/inode descriptor and initial parser context. Each bounded capture batch is
one Store append/frame containing all of:

- New byte-exact raw bytes, including malformed, blank and unterminated data.
- Initial redacted parsed outputs with the complete owned EventIn fields:
  index eligibility, parent session, revisions, tool links, times and original IDs.
- Per-line source-generation/byte-span/emission receipts, including explicit
  stored/indexed or withheld outcomes and the policy/raw-authorization identifiers.
- Versioned complete post-parser context, next event ID and the committed outcome, all sealed with
  the same caller-supplied key. There is no authoritative plaintext cursor sidecar.

Raw progress covers all retained bytes. Parsed progress covers complete processed
lines, including zero-output and all-withheld batches. A partial line remains a
range of sealed raw until its newline arrives. IDs are assigned only to stored
outputs and survive restart, changed raw chunking and cache reconstruction.
Store sequence numbers never become public event IDs.

`oojuro-capture::local::LocalCapture` uses the existing Codex buffer parser,
shared redactor and call-link semantics. Source descriptors and every retained
byte are revalidated before new suffixes are committed. Replacement, truncation
or edited prefixes require an explicit new generation ID. Original files are
never deleted or rewritten by this lane. Capturing a whole file is not atomic:
separate acknowledged blocks form a durable prefix. A source must be append-only
during its open pass; arbitrary concurrent edits are not a consistent snapshot.

The native authority commits before lexical projection. Successful capture means
its authorized parsed outputs are searchable. If projection fails after commit,
`CommittedUnavailable(outcome)` reports that fact and reads/new capture stop until
rebuild or reopen. Retrying the same retained source returns its existing IDs,
not another allocation. An uncertain authority write poisons the handle; reopening
recovers either the last complete sealed frame or its preceding valid prefix.

## Consent and policy

The raw consent callback runs before source metadata inspection or opening, even
for an absent file. It must resolve an actual explicit grant and fail closed for
unreadable/unknown policy. The receipt's authorization identifier labels the
caller's decision; it is not a cryptographic access-control proof. The callback's
authorization lasts for that open pass, not an instantaneous revocation protocol.
Final symlinks and nonregular replacements are refused. Parent directories must
be in the caller's trusted boundary; this is not a hostile-filesystem sandbox.

Raw permission covers every original byte, including thoughts and secrets.
Per-record `record_thinking`, redaction or index settings do not authorize raw
retention. A separate explicit decision is required for raw export. The caller's
provided writer/key is the ownership boundary; the library is not an account or
shared-human authorization service.

Default rebuild reads only the committed authorized parsed receipts. It never
reparses raw with current defaults or reconstructs decisions from EventOut,
legacy export or reclaim. Changing the current policy affects only newly consumed
complete lines. Policy IDs must identify immutable caller-supplied policy snapshots;
the caller must change the ID when its policy semantics change. An incomplete raw
line is bound to the policy identity under which it began: changing that identity
refuses further capture until the original policy finishes it or an explicit
reprocess facility is implemented. Previously withheld thought/tool records stay withheld; stored-only text
stays unindexed. A result of a withheld call remains withheld after restart even
if current policy permits it. Missing policy/context/output receipts or a different
format, key, writer/allocator binding or adapter revision fails closed. The local
context schema requires every field and rejects unknown fields on source registration
and every historical batch, independently of legacy FileCtx serde defaults.

Reinterpreting old raw under a new policy/parser is a separate explicitly
authorized operation and is not implemented here. Claude's parser can read
secondary persisted tool-result files, so it is deliberately absent from this
lane; those files need their own consent and provenance before adding that adapter.

Parsed tool and metadata strings receive the shared redactor at the output boundary,
as well as the parser-redacted text. The shared redactor now recognizes JSON-escaped control boundaries before known
credential formats. Matching uses an equal-byte-length whitespace view; masking
uses the original bytes, preserving JSON escapes, Unicode, offsets and placeholder
hashes. Pattern redaction is not a guarantee that all possible secrets are detected.

## Actual lexical baseline and cache boundary

Each owner journal projects into `oojuro_engine::lexical::Index`, using the native in-memory lexical port, per-writer term statistics and exact
original IDs/valid times.
Results retain baseline ordering and score bits, including identifier continuations
and decimal-string ties (`10` before `9`). Provenance follows the reported event's
own source, not a dedupe host's row. This is a bounded native index/query port;
persistent segments, scale acceptance and semantic superiority are not claimed.

This stacked revision changes the opt-in `LocalCapture::open` API: the former
`ram_root` argument is removed, `get` returns the complete native `capture_log::Event`,
and hit types belong to the native lexical module. No unused cache argument is
accepted. The derived postings and host rows live only in memory; no derived files
are written. Dropping the projection loses no authority and rebuild reads only the
sealed initial parsed receipts. Process memory can swap; OS confidentiality and
resource review remain necessary. Legacy recorder/capture paths remain unchanged.

See [native lexical port](NATIVE_LEXICAL.md) for exact source mapping, oracle
differential scope, pruning, dedupe and remaining persistence/scale gates.

## Bounds and remaining gates

Per append: at most 256 KiB new raw, 1 MiB per complete/partial line, 128 KiB sealed
context and 8 MiB serialized parsed receipt. At most 64 unresolved calls and 64
unresolved Oojuro lookup markers are retained. A new call that could evict unresolved
state refuses the candidate batch before commit, including empty-body lookup calls
that emit no parsed record. Reusing an unresolved nonempty call ID is also refused
before parser mutation, so a newer allowed call cannot shadow an older denied call.
Matched completions retire state and permit subsequent ID reuse, so this does not bound
the total calls in a session. No raw/cursor/allocator progress is acknowledged for
the refused batch; original suffix bytes remain available for an explicit decision.
The current projection limits are
100,000 stored events and 128 MiB aggregate serialized replay metadata
(registrations, complete events including args, policy receipts, and sealed contexts;
raw bytes excluded). Source/descriptor bounds also apply.
A limit error does not advance that batch or discard its provenance. Earlier
blocks may already be committed; original files and remaining suffixes stay intact.
Many tiny lines can hit the serialized-receipt bound before the raw-byte bound;
retrying with a smaller configured raw chunk is supported without losing progress.
Inputs that exceed a bound require an explicit operator/configuration decision,
not silent dropping or cursor skipping. The full-file report returns the generation's
committed event IDs, so retries are stable.

The caller supplies the key each open; no key file, key wrapping, OS keychain,
erasure, sharing, production routing or cloud forwarding is added. This does not
prove host/power-loss/AZ durability, native launch acceptance, multiwriter migration,
full adapter coverage or the specification's group-commit/one-total-fsync performance.
The existing lexical cache may do its own disposable I/O in addition to the native
sealed commit. Existing journals are not automatically migrated.

Synthetic tests cover native torn/uncertain frames, missing receipts, SIGKILL
between authority/projection/ACK, policy relaxation across restart, cache deletion,
byte-exact non-UTF8/partial recovery, rotation, cross-block calls and bitwise
oracle comparisons including IDs 9/10 and identifier continuation. No real agent
transcripts, provider calls or training data are used.
