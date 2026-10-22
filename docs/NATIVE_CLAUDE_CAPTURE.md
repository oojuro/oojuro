# Explicit native Claude Code capture

This review slice adds `oojuro_capture::local::LocalCapture::open_claude` on Linux.
It is an opt-in library entry point for one explicitly selected Claude Code JSONL
source generation. No CLI, daemon, discovery, forwarding or hosted route selects it.
`LocalCapture::open` retains its existing Codex adapter and identity unchanged.

## One authority and explicit consent

The new entry point reuses the same native `CaptureLog`, sealed Store append,
source descriptor validation, raw/parsed cursors, ID allocator, policy receipts
and in-memory native lexical projection described in [atomic Codex capture](CODEX_ATOMIC_CAPTURE.md).
There is no second WAL or oracle/trace query fallback. Search rebuilds exclusively
from committed authorized parsed receipts; it does not reinterpret raw bytes with
today's policy. All initial and historical contexts require the complete strict
schema, including empty call/lookup state, and reject unknown fields.

Claude's adapter identity is
`claude-buffer-1/no-sidefiles-1/redact-escaped-1/context-1`. Opening a Codex journal
with the Claude entry point, or the reverse, fails. There is no automatic migration.
Writer, numeric agent and first-ID bindings retain the existing exact checks.

The raw consent callback runs before metadata inspection or opening the selected
source, including absent files. It must resolve an explicit caller grant and fail
closed. Raw permission covers every original byte, including secrets and thoughts;
per-record thinking/index/redaction settings do not authorize raw retention. The
caller separately authorizes raw export and supplies the ownership key. The
callback's authorization is for one open pass, not instantaneous revocation.
Parent directories remain a trusted caller boundary; this is not a hostile-filesystem sandbox.

## Claude parser and complete resume state

The existing Claude parser is used directly: assistant text/thinking/tool blocks,
user prompts/tool results, attachment rendering, compaction/system entries,
noise/unknown/malformed lines, timestamps, verbatim tool inputs and shared redaction.
Session/subagent identity is derived from the selected path. `origin`, role,
parent session, model, status and other metadata are **reported provenance**;
they do not prove human identity, authorization or the truth of a tool result.

Resume seals the full owned parser state: writer/session/parent, sequence/turn,
last timestamp, cwd, sidechain/origin and inherited state, calls and Oojuro lookup
markers. The secondary-file root is permanently absent under this adapter identity.
A withheld call retains ID zero; its later result stays withheld across restart
and policy relaxation. Stored-only text remains unindexed after rebuild.

Before the parser runs, Claude-specific preflight simulates call/lookup changes
over the complete content-block sequence on cloned state. Duplicate unresolved IDs
and capacity overflow refuse the candidate batch before cache eviction or commit.
A tool call with missing/empty/nonstring ID or missing input is refused before commit: the legacy
emitter otherwise drops missing-input calls before exclusion policy sees the tool,
and an untrackable call cannot bind a later denied result. This is
an explicit malformed-valid-JSON restriction, with no source/cursor/ID progress.
Null/string/array inputs still produce a policy-visible record and their denied
results remain withheld across restart; no tool-input type is treated as authority.
Matched emitted completions retire calls; empty results may retire a lookup marker
but do not fabricate a stored completion. More than 64 total completed calls are
supported; at most 64 calls and 64 lookup markers may remain unresolved at once.

## Referenced tool-output files remain unread

Claude transcripts can contain a persisted-output stub pointing at a secondary
tool-results file. Consent to the selected JSONL does **not** authorize that file.
This lane never canonicalizes, stats, opens or reads the referenced path. It keeps
the original model-visible stub, labelled `persisted=unread`; it does not claim to
have captured the complete external tool body or that the referenced file is missing.
The original reference remains in the byte-exact main-file raw export.

Default legacy Claude capture still uses its existing project-root-bounded
lookup behavior. Capturing secondary raw files with independent explicit consent,
descriptors, generations, receipts and stable linkage is separate future work.

## Bounds and failure behavior

The existing native limits remain: 256 KiB new raw per batch, 1 MiB per complete or
partial line, 128 KiB context, 8 MiB serialized parsed receipt, 100,000 stored events
and 128 MiB aggregate replay metadata. Capacity refusal acknowledges no raw/cursor/ID
progress for that candidate; earlier blocks may already be committed. Original
files and the unacknowledged suffix remain untouched. There is no silent eviction,
cursor skip or automatic reinterpretation. Smaller caller-selected raw chunks can
resolve a batch-size limit. Unfinished lines remain bound to their starting policy ID.

Source replacement, edited retained prefixes and truncation require a new explicit
generation. Non-UTF8, malformed, blank and partial raw bytes round-trip unchanged.
Source files must be append-only during an open pass. The authority commits before
projection; `CommittedUnavailable` preserves the committed outcome and requires
rebuild/reopen. SIGKILL after authority or projection before ACK recovers the same IDs.

## Review evidence and remaining acceptance

All fixtures are synthetic. Tests compare the native records with the existing
Claude parser where no secondary-file read is involved; cover consent, policy
relaxation, sidefile open/access traps, source bytes, strict context, partial Unicode,
malformed lines, native search, duplicate/capacity refusal, sequential completion,
subagent provenance, cross-harness refusal and actual process SIGKILL/reopen.
Exact executed counts and scoped lint results are recorded in the draft PR.

This adds one bounded local adapter, not the complete consolidated engine or all
Claude sidefile semantics. No real private transcript, provider, training or AWS
operation is exercised. Native persistent segments, scale/performance/size gates,
erasure/audit/key lifecycle, hosted authorization and host/AZ durability remain
separate SPEC acceptance. Existing source files and production recorder defaults
are not migrated or replaced.
