# Native lexical review slice

This stacked milestone follows atomic Codex capture PR16. It is an opt-in native
in-memory index/query port, not completion of SPEC sections 14/18 or a production
switch. The old default recorder continues using its existing engine.

## Authority and API

`oojuro_engine::lexical::Index::rebuild(&CaptureLog)` derives postings and host rows
only from committed authorized parsed receipts. `refresh(&CaptureLog)` applies new
committed batches of that exact open authority; a process-local instance binding
refuses attaching an existing projection to another journal handle. Reopening the
same journal requires rebuilding. Failed projection invalidates the local capture
read path and reports `CommittedUnavailable` after an already committed append.
Invalid or uncertain authority commits cannot enter the derived view.

`Query` explicitly names writer, numeric owner partition (`agent`, the legacy
TraceDb owner stamp), text, inclusive valid-time bounds and top-k. Actual harness
instances remain the event's `session`/`parent_session` fields. Wrong owner/partition
fails closed. `search_where` applies a caller filter after lexical ranking, refilling
from the same time-scoped ranking exactly as the oracle's TraceDb does. This library
scope check does not implement human/project authentication or current hosted ACLs.

`LocalCapture::open(dir, key, options, policy)` now uses this native projection.
This deliberately changes the opt-in API from PR16: the tmpfs directory argument
is removed; `get` returns complete native `Event`, including policy eligibility and
source-line metadata; native `Hit` preserves event ID, exact bits, continuation,
valid timestamp and original indexed-host event ID (`row`). Caller provenance is
still the reported event's source generation, raw byte span and emission ordinal.
There is no TraceDb call, tmpfs requirement, derived disk file or second WAL in
this lane. Process memory may swap; this is not an OS memory erasure guarantee.

## Source mapping and preserved arithmetic

- `lexical/text.rs` ports oracle `nine/src/text.rs`: maximal Unicode alphanumeric
  tokens, preserved whole snake identifiers, lowercase, pinned English Snowball
  1.2.0, stop-word fallback, integer query counts, exact word-bounded phrases and
  Elias-gamma literal prior. `analyze` ports `segment.rs` token counts and scope.
- `lexical.rs` ports `v4.rs` rank/ScanContext semantics for one in-memory partition:
  canonical row and term order, identifier occurrence in the actual time view (F1),
  split-word continuation after anchored matches (F2), scope-scaled term frequency,
  Dirichlet mass 128 and candidate background padded to 64 records of mass (F3).
  Floating point expressions and addition order remain the oracle expressions.
- Exact pruning ports `log2_upper`, `best_indices`, exact seeds and the conservative
  reach guard from `v4.rs`. Tests compare both oracle and exhaustive native results.
  Full exact finalists sort by score descending, valid time descending, then full
  decimal event ID as text (F4), including equal eight-byte prefixes. This first
  bounded slice sorts finalists directly instead of porting disk/coarse pivot I/O.
- Trace's EventIn→Record mapping is preserved: key is event ID, `recorded_at` for
  query visibility equals event valid timestamp, and text indexes only when the
  initial receipt permits it and text is nonempty. Parsed `recorded_at` provenance
  in args is retained independently; changing its interpretation would change F1.
- `lexical/args.rs` ports exact trace dedupe src/rt span recognition and replacement,
  preserving the rest of serialized args byte for byte. Same indexing class/tool/
  normalized args/text share the original host; nonstandard indexing classes stay
  separate. Full equality verifies truncated-hash matches and collisions fail
  closed. At the authority's 100k-event bound, unique keys cannot reach the oracle's
  2^17 half-window rotation threshold; there is no new unbounded dedupe promise.

Stored-only events remain readable, withheld outputs get no ID, and rebuild never
reparses raw under current policy. Empty searches preserve oracle timeline/ranking
behavior, including eligible stored-only hosts; explicit keyed reads use decimal
record keys. A top-k of zero returns empty natively; the oracle TopK path panics for
zero and is outside the positive-k bitwise differential domain. No oracle source
was modified to conceal that boundary.

## Evidence and limits

All fixtures are synthetic. Differential tests cover lexical ID/score bits,
continuations, valid times, indexed-host IDs, time-window statistics, exact phrases,
Unicode/stemming/stop words, stored-only/empty records, cross-batch duplicate policy,
kind/ID postfilters, IDs 9/10 and long equal prefixes, small and above-floor mass,
1,200 candidates, pruning versus exhaustive scoring, native authority reopen and
rebuild. Existing local capture tests run against the native path, including actual
SIGKILL before ACK, withheld→relaxed restart, raw partial/malformed progress, denied
call state, missing sealed context/policy receipts and read blocking after failed
projection. Results are recorded in the PR after execution.

Engine production dependencies contain no nine/trace/oracle facade. Trace is an
engine **dev-dependency only** for differential testing. The existing capture crate
still depends on trace for its unchanged legacy lane; its opt-in local module does
not import it. Oracle fixtures use isolated tmpfs only for oracle files.

Bounded native authority limits remain 100k stored events and 128 MiB aggregate serialized replay metadata
(registrations, complete events including args, policy receipts, and sealed contexts;
raw bytes excluded),
plus existing raw/line/context/receipt bounds. This is an encoded-state bound,
not an exact process RSS bound. Reopen applies the same bound and refuses older
candidate logs that exceed it; no automatic truncation or migration occurs. Queries allow 64 KiB and up to 10k
requested hits. This is an in-memory derived index: no persistent segments/Rice
codec, parallel chunk scan/skip tables, compaction/merge, history/key versions,
learned outcomes, packing/excerpts, native erase/audit/retention, shared-log port,
all adapters or ranker integration is claimed. Full TraceBench/30,905-turn-loop and
external retrieval corpora, size/ingest/latency, 10M/100M, power loss and host/AZ
acceptance remain separate specification gates. No real logs, provider calls,
training, migration, default runtime selection, merge or deployment occurred.

The separate opt-in persisted derived slice is documented in [NATIVE_SEGMENTS.md](NATIVE_SEGMENTS.md). Its bounds and unfinished disk/scale gates remain explicit.
