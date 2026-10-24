> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/journal/blob/main/docs/FRONT_DOOR.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Front door: the journal used like SQLite

Code: `crates/oojuro` (library, `oojuro` CLI, MCP server, settings, and the
`oojurod` recorder, which moved here from `crates/oojuro-capture`). Product
model: `docs/DESIGN.md` ("Explicit use", "Settings"). Capture:
`docs/CAPTURE.md`.

## Surface

| Call | Library (`Oojuro` / `Journal`) | CLI | MCP tool |
|---|---|---|---|
| explicit write; a key keeps versions, newest wins | `remember(writer, text, key?, at?)` | `oojuro remember [--key K] TEXT\|-` | `remember` |
| bits-ranked search with exact excerpts and byte ranges | `search(SearchArgs)` | `oojuro search QUERY\|- [-k N] [--kind] [--key] [--writer] [--from] [--to] [--as-of]` | `search` |
| one record exactly, by id or by key (as of a time) | `get_id(id)`, `get_key(writer, key, as_of?)` | `oojuro get ID \| --key K [--as-of T]` | `get` |
| every version of a key | `history(writer, key)` | `oojuro history KEY` | |
| a time window (every version, oldest first) | `range(filters, limit)` | `oojuro range [FROM [TO]]` | |
| proven deletion | `forget(Target, proof)` | `oojuro forget --id \| --key \| --older-than \| --last` | `forget` |
| periods of activity ranked by errors, stuck loops, unusual, volume or text (query-time) | `periods::periods(Reader, PeriodsArgs)` | `oojuro periods [QUERY] [--by B] [--when P] [--per task\|session\|writer] [--window W] [--writer W]... [-k N] [--samples N] [--include-waiting]` | `periods` |
| what a date phrase means | `when::resolve_when` | `--when PHRASE` on `search`, `range`, `export`, `periods` | `when` on `search` and `periods` |
| counts per writer and kind, disk | `stats()` | `oojuro stats` | |
| everything as JSON lines | `export(filters, sink)` | `oojuro export > out.jsonl` | |
| recording switch | | `oojuro pause` / `resume` | |
| settings | `config::{Config, set_value}` | `oojuro inject on\|off`, `oojuro config get\|set\|path` | |

Every CLI command takes `--json`. Times are `now`, RFC 3339, `YYYY-MM-DD`,
`@unix`, or a duration ago (`1h`, `7d`). `--when` takes a date phrase or an
anchor (below). MCP: `oojuro mcp` (stdio, JSON-RPC,
protocol 2025-06-18 and earlier).

**Record.** A writer ID, a valid time (when it happened), a recorded time,
and the text. Structure is inferred: kind (`explicit` for `remember`; the
capture kinds otherwise), tool, sender, source. Search hits (`Ref`) carry
`id, writer, kind, key, ts, score_bits, start, end, len, excerpt` with
`text[start..end] == excerpt` (a passage around the best literal or term
match, `--excerpt-bytes`, default 320).

**Agent id and parent id.** Every agent instance (a session, or one
subagent) has its own id: records carry `agent_id` (the same 16 hex digits
as `session`) and `parent_id`, the instance that spawned it, when the
capture path knows it (Claude Code subagent transcripts, Codex subagent
rollouts, hooks; `docs/ACTIVITY_SEARCH.md` §11.1). The writer stays the
memory owner and erasure domain. Trees of instances are computed at query
time (`periods --per task`).

**Writers.** The CLI writes as `--writer`, else `$OOJURO_WRITER`, else
`human:<os user>`. The MCP server writes as `$OOJURO_WRITER`, else the
recorder's ID for the agent that started it (Claude Code sets `CLAUDECODE=1`:
`claude-code:<launch dir encoded>`), else `human:<user>`. Search reads every
writer of the journal unless filtered; MCP `forget` only touches its own
writer's records.

**Keys and as-of.** Explicit records live in the writer's private store,
indexed and redacted like captured ones (secrets are masked before storing;
`remember` reports how many). The versions of `(writer, key)` share a
session (64-bit hash of the pair) and `seq` is the version number. A
search drops a keyed record unless it is the newest live version (valid
time, then id) as of the view time; `get --key` returns that version and
`history` all of them. `--as-of T` is the view: only records valid by `T`
exist, and keys resolve to their version current at `T`. Forgetting the
newest version makes the previous one current again. Valid time is the
write time unless `--at` backdates it.

**No token window.** A query may be as long as `max_query_bytes` (default
64 KiB; the engine is opened with a 16 MiB cap and the setting is checked
per call), so a whole error log or file excerpt can be the query. One
record holds up to 16 MiB through `remember`.

## Reading while the recorder writes: option (b), the recorder serves

`oojurod tail` owns the store and serves other processes on a Unix socket
next to it (`~/.oojuro/oojurod.sock`, mode 600, home 700, and the peer's uid
must match). Each connection carries one JSON request line and gets one
answer line (`export` streams one line per record). Reads run on
per-connection threads through a `Reader` on the open store, in parallel
with ingest (the engine's reads are `&self`, appends are atomic batches,
and a hit whose typed fields are not yet visible is simply not returned).
Writes (`remember`, `forget`) are queued to the recorder's own loop, the
store's single writer, which answers them between file commits and while
it waits for the next poll. When no recorder answers on the socket, or it
serves a different store, the CLI, MCP server and library open the journal
themselves for that one call and close it again, so a recorder can start.

Why (b) and not (a), a read-only shared open:
- Writes from other processes must go through the single writer anyway, so
  (a) would need this socket (or another channel) as well; (b) is one
  mechanism instead of two.
- (a) means readers in other processes racing the writer's files: manifests
  replaced, segments merged and deleted, journals retired, and a tail of the
  journal being written. The engine's open path also repairs (removes
  unlisted segments, truncates torn journals), so a reader would need a new
  non-mutating open mode across the engine. (b) changes no file format and
  no locking; all concurrency stays inside one process, where it is already
  tested.
- Erasure: with (a), another process holding mmaps of old segments keeps
  forgotten bytes alive until it exits. With (b), `forget` is one process
  rewriting its own files and every reader sees the result at once.
- The socket also gives `oojuro stats` while the recorder runs (it failed
  before on the exclusive lock), and later the hooks/injection path a cheap
  per-turn query without opening 70 stores.

Costs of (b): queries need the recorder up (or no recorder at all); the
recorder's memory now also serves queries (systemd `MemoryMax=1G`); writes
wait for the current file commit (one chunk, up to 16 MiB of log, during a
large import).

Locking: `<store>/LOCK` (exclusive, `Capture::open`) makes one owner per
store; a direct call while a recorder runs fails with a clear message
instead of touching the store. The recorder refuses to start when another
recorder answers on its socket, replaces a stale socket, and on SIGTERM or
SIGINT finishes its pass, answers queued writes, flushes, saves its state
and removes the socket.

## Settings

`~/.oojuro/config.toml` (`$OOJURO_HOME`), the nearest `.oojuro.toml` at or
above the working directory, and `[agents."<writer>"]` sections in either
(names may use `*`, e.g. `[agents."codex:*"]`). Precedence, lowest first:
defaults, global top level, project top level, global agent sections (`*`
before exact), project agent sections. `exclude` is the union of every layer
(a project can add exclusions, never drop global ones); a relative pattern
with a `/` in a project file is relative to that project. `paused` (set by
`pause`/`resume`) is global only. `oojuro config set` validates types and
ranges and edits the file in place, keeping comments.

| Key | Default | Used by |
|---|---|---|
| `record` | true | recorder: a writer with `record = false` is not stored (not even registered) |
| `inject` | false | `oojuro hook UserPromptSubmit` (`docs/CAPTURE.md`, "Claude Code hooks"). Never injects what is still in the agent's context window: the prompt's session from its last compaction on, or all of it when never compacted (search filter `outside_context_of`) |
| `top_k`, `budget_tokens`, `min_bits` | 20, 2500, 8 | injection (explicit search takes any k) |
| `recency` | 1.0 | injection ranking: bits per turn of a recency prior added to the text bits (model `session-turn-decay-v1` of the recency track). A record of the current session `d` turns old loses `recency * d` bits; the session's task and other sessions' records count as older than its first turn; wall-clock age is never used. With the context window left out, this ranks the current session's pre-compaction records (newest first among near-ties) ahead of older sessions, while strong text evidence keeps its place (a heavy weight can outrank it); 0 = text bits only. `min_bits` stays a test of text bits. Explicit search has no turn context and ranks by text bits only |
| `record_thinking` / `index_thinking` | true / true | recorder: drop thinking / store it unindexed |
| `exclude` | [] | recorder: path globs never recorded (below) |
| `max_query_bytes` | 65536 | search (CLI, MCP, library callers pass it) |
| `timezone` | "" | `--when` phrases and day windows of `periods`: an IANA name; empty = the machine's zone |

The recorder resolves settings per record from its writer and the agent's
working directory (Claude Code `cwd`, Codex `session_meta.cwd`), re-reads
changed files once per pass (~0.5 s) and notices new project files within
5 s. **Pause** withholds what the recorder reads while paused: those lines
are consumed and never stored (explicit `remember` still works). **Exclude**
matches the transcript path and the file paths a tool call names
(`file_path`, `path`, `notebook_path`, `paths`, ...); a pattern without `/`
matches a name at any depth (`.env`, `*.pem`), `~/` is the home directory,
and a path is excluded when it or a parent directory matches. A withheld
tool call takes its result with it (also across restarts). Settings that
cannot be read (a file that does not parse, a bad type or pattern) stop
recording for the writers they cover and are reported once in the
recorder's log: the policy fails closed. Withheld records take no id and are
counted in the stats (`withheld by settings`).

## periods and --when

`periods` finds stretches of time by how the activity looked, as a pure
query-time function over the record columns (time, kind, tool, status, exit
code, text length, agent and parent ids): nothing is computed or stored at
ingest (`docs/ACTIVITY_SEARCH.md` §11).

- `--by errors`: failed tool calls, in bits of surprise against the writer's
  last 7 days.
- `--by repeat`: stuck loops, the same call failing again (exact, checked
  against the calls' texts); `--include-waiting` adds polling.
- `--by unusual`: rate, tool mix, error rate, new tools and record size
  against the writer's last 7 days.
- `--by volume`: most records.
- `--by text` (or a query alone): where a query's hits are.

Windows are 5 min for errors and repeat, 1 h otherwise (`--window`).
Coordinator and subagent instances have separate baselines. Periods group a
task tree (an instance and everything it spawned; `--per session|writer`
for the others). Each period has its bounds, scores, counts, top tools, the
contributing instances, sample record ids (open them with `get`), and a
`range` command to read it. The default period is the last 7 days.

```
oojuro periods --by repeat --when yesterday
oojuro periods "migration" --by errors --when "last week" --per session
oojuro periods --by unusual --writer codex:proj --when "since monday" --json
```

`--when` (also on `search`, `range`, `export`) takes, in the `timezone`
setting's zone (else the machine's):
- date phrases: `yesterday`, `last tuesday afternoon`, `past 3 days`,
  `oct 14`, `since monday`, `between monday and yesterday`, `7d`;
- anchors found by searching the journal in time order: `before "deploy"`,
  `after the first "failed test"`, `the day "build broke"`, `around @4211`,
  `during the last "release"`. The latest occurrence is used unless
  `first`/`top` is given, and the answer says which one, with the other
  occurrences when they fall on several days.

The resolved interval is echoed on stderr (CLI) or returned as `when` (MCP
`search` and `periods`). MCP: tool `periods` (read-only) with `by`, `query`,
`when`, `per`, `window`, `writer`, `include_waiting`, `k`, `samples`.

## forget

Tombstones (durable, replayed on open; a torn last line is ignored), then
the engine's reclaim rewrites every affected segment without the records,
then the proof: every row of the affected writer stores is decoded and
every file of the store read byte for byte, looking for the forgotten texts
(all of them, or an even sample of 64; raw scans use texts of 16+ bytes).
`proven` means no row that nothing live needs holds the text, and no file
contains a text that no live record holds (another record with the same
text, in any store, keeps it legitimately; the proof reports those rows
separately). `--last 1h` and `--older-than` without `--writer` cover every
writer; on a terminal the CLI asks first (`--yes` skips).

## Deploying

`oojurod` now builds from this crate: `cargo build --release -p oojuro
--bins` gives `oojurod` and `oojuro`. The running recorder (an older build)
does not serve the socket: until it is restarted from this build, `oojuro`
calls against `~/.oojuro/journal` fail with "held by another process"
(nothing is modified). Its store needs no migration (explicit records are a
new kind in the same format; the state file is unchanged; `<store>/LOCK` is
new).

Agent parents (2026-10-21) need no migration either: an older column
checkpoint loads (it has no parents section), records written before carry
no parent field and stay unlinked, and new records of subagents carry one.
Re-importing the transcripts into a fresh store links the old ones too.
`periods` and `--when` read only what the store already holds.

## Tests (`nice -n 19 cargo test -p oojuro -p oojuro-capture -p oojuro-trace`)

Synthetic data only. `oojuro`: API semantics (versions, as-of, filters,
excerpt ranges, long queries, redaction, range/export/stats), forget by id,
key, age and window with the proof checked again by grepping every file,
config precedence and comment-keeping edits, CLI round trips, MCP
initialize/list/call for every tool and JSON-RPC errors, and recorder
processes: 4 reader processes (search, range, get, stats in loops) against
an agent appending split lines and 3 processes writing 8 versions each
through `remember`, checking every text read back exactly against what was
written (no torn record), monotonic counts, read latency, and the final
contents; plus record/pause/exclude/thinking honored end to end; `periods`
on a synthetic 10-day writer with planted episodes (each first by its own
signal; stuck vs polling vs same-length false repeats; task trees; `when`
and anchors; forget and reopen), the date grammar (the prototype's 43 cases,
daylight saving, zones, year wrap, anchor picks), and `--when` / `periods`
through the CLI and MCP. Capture:
the policy hook (withheld records take no id; a withheld call's result is
withheld, also after a restart), explicit records in the id space and
crash recovery, one owner per store. Trace: delete by id, tombstone replay
with a torn line, filtered scored search, erasure scan.

Measured (debug build, loaded box): 1,178 reader process calls during the
concurrent test, slowest 57 ms. `tracebench verify --arm oojuro` on smoke:
1,237/1,237. The `oojuro` and `oojurod` suites also pass with the shared
write log as the engine default (v1 after 0ed4976).

On a private copy of the live journal (535,856 records, 74 writers, 355 MB,
shared log; release build, only counts and timings looked at, copy deleted
afterwards): direct access (open every store, search all writers, exit)
0.27-0.29 s per `oojuro search`; through a recorder serving the copy, 10 ms
per `oojuro search` process, 0.34 s for a 64 KB query, 20 ms per `remember`;
`forget --key` with its proof 0.7 s (506 files, 371 MB read).

## Limits and risks

- Version selection is done above the engine: superseded versions still
  count in the ranking's background statistics, and a key resolves through
  a 64-bit session hash (the stored key is checked on `get`/`history`, not
  on every search hit).
- As-of uses valid time only (records are recorded when they happen;
  backdated `--at` writes are visible "as of" their valid time).
- Exclusion sees paths in tool inputs and transcript paths, not paths
  inside shell commands (`cat ~/.env` via Bash is recorded).
- Pause covers what the recorder reads while paused (within one poll); a
  line written just before `pause` and read after is withheld too.
- Direct access opens every writer store per call (cheap for small
  journals; the recorder path avoids it).
- The proof's raw-file scan reads the whole store; on a large journal a
  bulk forget takes seconds to minutes (`--no-proof` skips it).
- The query socket trusts any process of the same uid (as the files do).
