# Capture layer (v1): attach and record agent logs

Code: `crates/oojuro-capture` (the ingest library); the `oojurod` binary is
built from `crates/oojuro`, where it also serves the journal to other
processes and honors the settings (`docs/FRONT_DOOR.md`). Product model:
`docs/DESIGN.md`.

The agent never calls the database. `oojurod` reads the transcripts agents
already write, turns every entry into a v1 record, masks secrets **before**
anything is stored, and appends into the writer's private store.

```text
oojurod import [--store DIR] [DIR...]   one shot: everything written so far
oojurod tail   [--store DIR] [DIR...]   follow: new files and appended lines; serve the journal on <store>/../oojurod.sock
                                        (and run the model API proxy when [proxy] enabled = true)
oojurod proxy  [--listen ADDR] [--writer NAME] [--anthropic-upstream URL] [--openai-upstream URL]
                                        the recording model API proxy on its own (see "Model API proxy")
oojurod otel   [--otel-listen ADDR]     OpenTelemetry receiver only (no logs read); same socket
oojuro hook EVENT                       Claude Code hook (stdin JSON); `oojuro hooks print` shows the settings entry
oojurod stats  [--store DIR]            records per writer and kind, disk
oojurod audit  [--sample N] [DIR...]    redaction false-positive audit (stores nothing)
oojurod compact [--store DIR]           major compaction (heavy zstd + dictionary)
oojurod verify  [--store DIR]           placeholders per kind; a second redaction pass must find nothing
```

DIRs default to `~/.claude/projects` and `~/.codex/sessions`. Logs are only
ever opened read-only.

Four attach modes feed the same records (`docs/DESIGN.md`, "Capture"):
the log tailer (below), Claude Code hooks and the OpenTelemetry receiver
(sections at the end), and the model API proxy (separate). Records that come
from no log carry `via` (`hook`, `otel`) in their metadata.

## Record mapping

A record is a writer ID, timestamps (valid time from the entry's
`timestamp`; recorded time `rt` at ingest) and the verbatim text. Everything
else is inferred and kept as derived metadata.

**Writer ID = the agent that saw or produced the text** (design decision
2026-10-20): `claude-code:<project>` / `codex:<project>`. `<project>` is
Claude Code's own project key, the launch directory with every character
outside `[A-Za-z0-9]` replaced by `-`; Codex sessions use their `cwd` encoded
the same way, so one directory gives the same project for both agents. User
prompts are records of the agent's own store with kind `user_input` and the
sender in metadata (`from`: `human`, `parent-agent`, `coordinator`, `peer`,
`agent:<path>`, `caller`, `inherited`).

Alternatives considered:

| scheme | for | against |
|---|---|---|
| per project (chosen) | stable, readable, project-scoped search and erasure | coarse when every session starts in `~` (see below) |
| per session (`claude-code:<session-id>`) | finest erasure | ~1,700 stores, one sync each; dedupe and dictionaries cannot span sessions (resumes and system prompts repeat across them) |
| per product (`claude-code`) | best dedupe, simplest | erasure all-or-nothing, no project scoping |
| per event cwd | follows `cd` into worktrees | unstable: 12% of transcripts change cwd, ~380 distinct cwds in a third of the files |
| `human:<user>` for prompts | person as a writer | superseded: the agent's store holds its whole experience stream |

Measured caveat: the measured developer launches almost every Claude Code session from
`~`, so `claude-code:-home-<user>` holds ~97% of the Claude bytes. A better
`<project>` for that case is the session's dominant git worktree; left open.

**Session / turn / seq:** one transcript file = one session (hash of its
path; a subagent sidechain file is its own session). `turn` counts the user
inputs so far, `seq` the records of the file. A tool result links to its call
(`parent`) and inherits the call's tool name.

**Kinds** (`oojuro_trace` codes): `system` (harness context, stored only),
`user_input`, `thinking`, `assistant`, `tool_call`, `tool_result` (all
indexed; thinking is indexed by design decision 2026-10-20), and `explicit`
(written through the front door with `oojuro remember`, never parsed from a
log).

**Settings** (`docs/FRONT_DOOR.md`, "Settings"): a recording policy decides
per parsed record, before it gets an id, whether it is stored and indexed:
`oojuro pause`, `record = false` for a writer, `record_thinking` /
`index_thinking`, and `exclude` globs on the transcript path and on file
paths named in tool calls (a withheld call's result is withheld too).
Withheld records are counted by reason in the stats.

### Claude Code (`<project>/<session>.jsonl`, `<project>/<session>/subagents/agent-*.jsonl`)

| entry | record |
|---|---|
| `assistant` text / thinking / tool_use | `assistant` / `thinking` / `tool_call` (input JSON verbatim, key order kept) |
| `user` string or text blocks | `user_input` (sender from `origin.kind`; a subagent's prompt comes from the parent agent) |
| `user` `isMeta`, `isCompactSummary`, tag-wrapped (`<task-notification>`, `<local-command-stdout>`, `<system-reminder>`, ...) | `system` (`<command-name>` and `<bash-input>` are typed by a person: `user_input`) |
| `user` tool_result blocks | `tool_result` (status from `is_error`) |
| `user` tool_result that is a `<persisted-output>` stub (oversized output; the model saw a 2 KB preview and the path) | `tool_result` with the full output read from `<project>/<session>/tool-results/<id>.txt` (`persisted=full`, cut at 8 MiB); the stub as it is when the file is gone or outside this project's `tool-results/` (`persisted=missing`) |
| `attachment` with `rendered` | `system`: exactly what entered the model's context (`queued_command` from a person: `user_input`) |
| `attachment` `prompt_snapshot` | `system` (the system prompt and tool definitions) |
| `attachment` without `rendered` (hook bookkeeping, tool records, ...) | noise: never reached the model |
| `system` with `content` | `system` |
| `toolUseResult` | not stored: the UI's structured copy of the tool result (counted as re-sent) |
| `queue-operation`, `last-prompt`, titles, `pr-link`, `mode`, file-history snapshots, `bridge-session`, `atis-latch`, `cost-state` | noise (counted) |
| anything else | unknown (counted, skipped) |

### Codex (`YYYY/MM/DD/rollout-*.jsonl`)

| entry | record |
|---|---|
| `session_meta` | sets writer (cwd), who writes user messages (interactive person / parent agent / programmatic caller), inherited-history cut; `base_instructions` -> `system` |
| `response_item` `message` | user -> `user_input` (tag-wrapped context -> `system`), developer -> `system`, assistant -> `assistant` (`phase` in metadata) |
| `response_item` `reasoning` | `thinking` from the summary text; encrypted-only reasoning is noise (not text) |
| `function_call` / `custom_tool_call` and outputs | `tool_call` (arguments verbatim) / `tool_result` |
| `agent_message` | `user_input` from `agent:<author>` |
| `compacted` | `system` (the summary; the replacement history repeats earlier records: counted as re-sent) |
| `world_state`, `realtime_item` transcript segments | `system` / voice `user_input` and `assistant` |
| `event_msg/*`, `token_usage_record`, `turn_context`, `inter_agent_communication_metadata` | noise (UI views and counters) |

Lines of a subagent rollout below `subagent_history_start_ordinal` are the
parent's history and carry `inh=1`.

Images are replaced by `[image:<media type>:<bytes>B:<hash8>]`.

Saved tool outputs are read only from the transcript's own project folder,
after symlinks are resolved, and only `.txt` files under `tool-results/`
(saved images there stay placeholders). On this machine (2026-10-21): 640
stubs, 519 read in full (27.1 MB), 121 files already cleaned up. Findability
of words that appear only past the preview (40 queries of three such words, a
top-5 hit holding all three): 30/40 with the full outputs, 28/40 with
previews only; most of that text had reached the journal anyway because the
agent read the saved file afterwards.

## Redaction on ingest

Every text goes through `redact::Redactor` before it leaves the parser. Each
secret becomes `[REDACTED:<kind>:<first 8 hex of sha256(secret)>]`, so the same
secret stays linkable across records without being stored. Rules run in
order, specific formats first, and never re-match an earlier placeholder:

`pem_private_key`, `jwt`, `aws_key_id` (AKIA/ASIA), `github` (`ghp_`/`gho_`/
`ghu_`/`ghs_`/`ghr_`/`github_pat_`), `anthropic` (`sk-ant-`), `openrouter`,
`openai` (`sk-`/`sk-proj-`, plus terminal-wrapped key tails by the `T3BlbkFJ`
marker), `slack` (`xox[abprs]-`), `stripe` (`sk_`/`rk_` live/test, `whsec_`),
`google_api` (`AIza`), vendor prefixes (`supabase` `sbp_`/`sb_secret_`,
`sentry`, `linear`, `huggingface`, `npm`, `gitlab`, `e2b`, `fireworks`, `xai`,
`pinecone`), `aws_secret`, `url_credentials` (`scheme://user:PASS@host`),
`url_signature` (presigned-URL signatures and session tokens), `bearer`,
`assignment` (`password=`, `"token": "..."`, `API_KEY=`, `--password x`), and
`high_entropy` (random-looking tokens next to key/secret/token/password words,
or right after another placeholder: the rest of a wrapped secret).

The generic rules are tuned against false positives found on real logs: code
references (`process.env.X!`, `config?.accessToken`), env var names in empty
listings, ARNs and secret names, versions, grep line references, UUID
references, typed ids (`prj_`, `we_`, GraphQL node ids, Twilio SIDs), paths,
hashes and commit SHAs, public keys, placeholders and well-known local
defaults (`postgres:postgres@127.0.0.1`).

`oojurod audit` samples hits per kind and prints each as length, entropy,
character-class shape and surrounding text with every secret masked, never
the secret itself, so the false-positive rate can be judged by hand.

### Measured on the developer's logs (2026-10-20, 5.2 GB)

Hits per kind on the full import: `assignment` 1,224, `jwt` 1,137, `supabase`
389, `github` 250, `url_signature` 161, `aws_key_id` 148, `high_entropy` 39,
`stripe` 20, `pem_private_key` 8, `e2b` 5, `bearer` 4, `openai` 3,
`url_credentials` 1, `openrouter` 1, `aws_secret` 1 (3,391 in all).

False positives, by a manual check of `oojurod audit` samples (40 per kind,
all hits when fewer), with three rounds of fixes in between:

| kind | judged | not a credential (FP) | note |
|---|---|---|---|
| `high_entropy` | all 40 | 3 (1 fixed since) | first version: 1,055 hits, mostly paths and ids |
| `assignment` | 40 of 1,232 | 2 (1 fixed since) | 24 of 40 are the Supabase CLI's public local-default S3 key pair, 13 test-fixture passwords |
| `url_credentials` | all 15 | 15 (all fixed since: 1 hit left) | regex patterns and `{pw}` templates |
| `bearer` | all 4 | 0 | |
| format kinds (`jwt`, `github`, `supabase`, `aws_key_id`, `url_signature`, `stripe`, `pem`, ...) | 40 per kind | 0 format errors | many are public local defaults (Supabase demo JWTs and keys) or docs examples (Stripe `sk_test_`/`whsec_`) |

Distinct secrets behind the placeholders (by hash): `github` 250 hits are one
token, `supabase` 390 are two keys, `jwt` 1,141 are 44, `assignment` 1,254 are
141, `url_signature` 163 are 80, `aws_key_id` 148 are 16. `oojurod verify`
runs the redactor again over every stored record: 0 new hits.

So the masked text is very rarely ordinary text (estimated under 5% of hits),
but a large share of hits are credentials that are public by design (local
stack defaults). They stay masked: the rule cannot know.

A wrapped secret is the main leak risk found: a TUI table split one OpenAI
key over three lines; the head and the marker line were caught by format
rules and the third piece only by the placeholder-context rule.

## Import and tail

One pass lists every `*.jsonl`, registers new files, and for each file reads
the complete lines past its committed offset. Commit unit: one chunk of one
file (16 MB of lines by default); its records get the next ids grouped by
writer and each group is one durable append. Then the file's offset and parse
context (`seq`, `turn`, open tool calls, Codex session origin) advance.

Crash safety: `capture-state.json` (atomic replace) always describes
committed data. Committed records always form a prefix of the id range, and
each record stores its source position (`src` = file number : line offset :
index in line). On open, ids past the saved `next_id` are probed; their
positions are skipped when their lines are read again. So a crash between an
append and a state save neither duplicates nor loses records (tested), and
`kill -9` during `tail` is safe. A line without its newline yet is left for
the next pass; a file whose inode changes or that shrinks starts over as a
new file. New files and writers are saved before any record cites them.

Tests (`nice -n 19 cargo test -p oojuro-capture`, synthetic data only): parser
mapping for both formats, redaction formats and false-positive cases, a tail
that follows a file written in half-lines, a restart that re-reads nothing,
and a crash after the appends but before the state save that neither
duplicates nor loses (ids, `seq`, `turn` and call links continue). Demo: a
`tail` killed with `kill -9` mid-follow and restarted ends with exactly the
18 records of 6 synthetic turns.

Storage target today: `oojuro_trace::TraceDb`, one agent store per writer;
derived metadata rides in `args` as JSON
(`{"src":..,"rt":..,"from":..,"sub":..,"model":..}`). A dedicated v1 record
API replaces this later.

## Model API proxy

Code: `crates/oojuro-proxy` (forwarding, parsing, dedupe), wired into
`oojurod` in `crates/oojuro` (`proxy_sink.rs`, `[proxy]` in `config.rs`).

The third attach mode: an agent whose model base URL can be changed points
it at a local proxy. Every call is forwarded unchanged and the proxy
journals what the agent saw and produced. Unlike the log tailer it sees
exactly what reached the model (system prompt, tool definitions, thinking
text as far as the provider exposes it), for any agent, with no log format
to know.

### Running it

- **Inside the recorder (one daemon):** `[proxy] enabled = true` in
  `~/.oojuro/config.toml`, then `oojurod tail` also listens for model calls
  and appends their records itself (the store's single writer). A bad
  `[proxy]` table or a busy port is logged and the recorder runs without
  the proxy.
- **On its own:** `oojurod proxy [--listen ADDR] [--writer NAME]
  [--anthropic-upstream URL] [--openai-upstream URL]` (flags override the
  first listener of `[proxy]`). When a recorder serves the journal it
  sends its batches over that recorder's socket (`ingest` op); otherwise it
  owns the journal itself and serves the socket, without tailing logs.

It listens on loopback only (127.0.0.1 or ::1; anything else is refused).

```toml
[proxy]
enabled = true                       # run inside `oojurod tail`
listen = "127.0.0.1:8790"            # default; also "localhost:8790" or "8790"
writer = ""                          # optional writer for calls without x-oojuro-writer
anthropic_upstream = "https://api.anthropic.com"   # default
openai_upstream = "https://api.openai.com"         # default; any OpenAI-compatible base
record_tailed = false                # true: also record agents whose transcripts are tailed (see below)
# seen_per_writer = 100000, queue_jobs = 1024, queue_mb = 512, max_body_mb = 64

[[proxy.listeners]]                  # optional: more listeners, own writer/upstreams
listen = "127.0.0.1:8791"
writer = "openrouter-bot"
openai_upstream = "https://openrouter.ai/api"
```

`[proxy]` is global only (in a project `.oojuro.toml` it is ignored with a
warning). Unknown keys and bad values are errors.

### Pointing agents at it

The incoming path is appended to the upstream base URL, so a client's base
URL keeps its `/v1`:

| Agent | Setting |
|---|---|
| Claude Code | `ANTHROPIC_BASE_URL=http://127.0.0.1:8790`; optionally `ANTHROPIC_CUSTOM_HEADERS="x-oojuro-writer: <writer>"` |
| Anthropic SDKs | `ANTHROPIC_BASE_URL=http://127.0.0.1:8790` (or `base_url=`) |
| OpenAI SDKs (Python, Node) | `OPENAI_BASE_URL=http://127.0.0.1:8790/v1` (or `base_url=`) |
| Codex (API key) | `~/.codex/config.toml`: `model_provider = "oojuro"` and `[model_providers.oojuro]` with `base_url = "http://127.0.0.1:8790/v1"`, `wire_api = "responses"`, `env_key = "OPENAI_API_KEY"`, optionally `http_headers = { "x-oojuro-writer" = "<writer>" }` |
| Another OpenAI-compatible provider | `openai_upstream` = its base without `/v1` (e.g. `https://openrouter.ai/api`), client at `http://127.0.0.1:8790/v1` |
| Anything else | any HTTP client: `curl http://127.0.0.1:8790/v1/messages ...` |

With the default `oojurod tail` (which follows `~/.claude/projects` and
`~/.codex/sessions`), Claude Code's and Codex's calls are forwarded but
not recorded: their transcripts are (see "The transcript recorder wins").
The proxy records them when no recorder follows their directories, or
with `record_tailed = true`.

Credentials pass through untouched (API keys, OAuth bearer tokens), so a
subscription login works the same as an API key. Codex signed in with a
ChatGPT account talks to a different endpoint and needs an API-key provider
as above.

### Routes

| Path (POST) | API | Recorded |
|---|---|---|
| ends in `/messages` | Anthropic Messages, plain or streamed (SSE) | yes |
| ends in `/chat/completions` | OpenAI Chat Completions, plain or streamed | yes |
| ends in `/responses` | OpenAI Responses, plain or streamed | yes |
| anything else (`/v1/models`, `/v1/messages/count_tokens`, GET, ...) | to the Anthropic upstream when `anthropic-version` is sent or the path has `/messages`, else the OpenAI one | no (passed through) |

### Passthrough

- Request and response bodies are forwarded byte for byte, streamed as they
  arrive (each body is teed: frames go on at once and a shared copy of the
  bytes is kept for the journal, up to `max_body_mb`; larger bodies are
  forwarded in full but not journaled, counted `dropped_too_large`).
- End-to-end headers are forwarded unchanged both ways, credentials
  included, encodings included (`Content-Encoding` is undone only for the
  journal). Removed: hop-by-hop headers (`Connection` and the ones it
  names, `Keep-Alive`, `Transfer-Encoding`, `TE`, `Trailer`, `Upgrade`,
  `Proxy-*`), `Expect`, `Host` (set from the upstream URL) and the proxy's
  own `x-oojuro-*` headers.
- Upstream is HTTP/1.1 with TLS (rustls, webpki roots), pooled connections,
  `TCP_NODELAY` on both sides. An unreachable upstream gets the agent a 502
  with an Anthropic-style JSON error; an agent that hangs up mid-stream
  drops the upstream stream too, as a direct connection would.
- Journaling is off the hot path: when the response ends (or breaks off,
  or the agent hangs up), the copy goes to a bounded queue with a
  non-blocking send (`queue_jobs` calls, `queue_mb` of bodies). One worker
  thread decodes, parses, deduplicates, redacts and writes. A full queue,
  a slow journal or a journal that is down never delays a call: the copy
  is dropped and counted (`dropped_queue_full`, `dropped_journal`). The
  recorder logs the counters once a minute when they change.

### Writer

1. header `x-oojuro-writer` (1-256 bytes, no control characters);
2. else the listener's `writer`;
3. else `proxy:<user-agent product>`, lowercase: `proxy:claude-cli` (Claude
   Code), `proxy:openai` (OpenAI Python SDK), `proxy:codex_cli_rs`,
   `proxy:unknown` without a user agent.

Optional headers: `x-oojuro-session` (a conversation id, see below) and
`x-oojuro-cwd` (the agent's working directory, so per-project
`.oojuro.toml` settings apply). All `x-oojuro-*` headers are stripped
before forwarding.

### The transcript recorder wins

Decision (2026-10-21): when an agent is both tailed and proxied, the
transcript recorder wins. The proxy records only agents whose transcripts
no tailer reads; it forwards the others' calls unchanged and counts them
(`skipped_tailed`), so nothing is journaled twice under two writers.

- **Which agent made a call:** each capture format declares, next to its
  transcript parser, how its agent shows itself on the wire
  (`oojuro_capture::{claude,codex}::WIRE`, matched by
  `Format::of_client`): Claude Code by the user-agent product `claude-cli`
  (`claude-cli/2.1.3 (external, cli)`, also `sdk-ts`) or `x-app: cli`;
  Codex by a user-agent product starting with `codex` (`codex_cli_rs`,
  `codex-tui`, `codex_exec`, `codex_vscode`) or such an `originator`
  header. Anything else (SDKs, other agents, curl) is recorded.
- **Which formats are tailed:** the recorder decides from the directories
  it follows: a format is tailed when its transcript root
  (`.claude/projects`, `.codex/sessions`, declared next to the parser) is
  one of them, lies above one (`~/.claude/projects/-h-repo`) or below one
  (`~/.claude`, `~`), or when the recorder has registered a transcript of
  that format under one of them. The default `oojurod tail` (both default
  directories) tails both. The recorder's hello says so (`"tailed":
  ["claude-code", "codex"]`, next to its `watch` list).
- **Inside `oojurod tail`:** the proxy gets the recorder's tailed formats
  at start. **`oojurod proxy` with a recorder:** it reads them from the
  recorder's hello (from `watch` for an older recorder) and asks again
  every 5 s, so a recorder restarted with other directories is followed;
  while no recorder answers, the last answer stands. **`oojurod proxy`
  with no recorder** (it owns the journal and tails nothing), or a
  recorder not following an agent's directory: that agent is recorded as
  before.
- `x-oojuro-writer` does not override this: a tailed agent's call is not
  recorded under any writer.
- `[proxy] record_tailed = true` turns it off: every agent is recorded
  (then an agent that is also tailed is journaled twice, under its
  transcript writer and its proxy writer).

### What is stored

Records of the capture kinds, in the writer's private store, with
`"via":"proxy"` instead of a source position:

| Source | Record |
|---|---|
| Anthropic `system` (string or each text block), Responses `instructions`, chat `system`/`developer` messages, Responses `developer`/`system` messages | `system` (`sub`: `system_prompt`, `instructions`, `system`, `developer`) |
| `tools` (the definitions, JSON as sent) | `system` (`sub: tools`) |
| user text | `user_input`; tag-wrapped harness context (`<system-reminder>`, `<environment_context>`, ...) is `system` with the tag as `sub`, as in the tailer |
| images, documents, audio, files | `user_input` placeholder `[image:<media>:<bytes>B:<hash8>]` (URLs as `[image:url:...]`) |
| assistant text (history and responses), refusals | `assistant` |
| thinking text (Anthropic `thinking`, chat `reasoning_content`/`reasoning`, Responses reasoning summaries and `reasoning_text`) | `thinking`; redacted thinking, signatures and encrypted reasoning are not text (not stored) |
| tool calls (`tool_use`, server/MCP tool use, chat `tool_calls`, Responses `function_call`, `custom_tool_call`, `*_call`) | `tool_call`: tool name, call id, input JSON verbatim as the model produced it |
| tool results (`tool_result`, chat `tool` messages, `*_call_output`) | `tool_result` (`is_error` gives the status), linked to its call (`parent`, tool name) |
| HTTP status 400 and above, `error` stream events, `response.failed`, an unreachable upstream | `system` (`sub: upstream_error`, status error, `http`: the code), text `HTTP <code>: <body>` (cut at 64 KiB); always stored |

Metadata (`args`): `via`, `rt`, `api` (`anthropic.messages`,
`openai.chat`, `openai.responses`), `model` (the response's, else the
request's), and on the first output record of a call `usage` (`in`, `out`,
`cache_read`, `cache_write`, `reasoning`, as the API reports them; also in
`tokens_in`/`tokens_out`), `stop` (stop reason), `rid` (upstream request
id). Outputs of a stream that broke off carry `partial`
(`client_disconnect` or `upstream_error`): what arrived is stored. Valid
time: the request's arrival for inputs, the response's end for outputs
(with the call's duration). `turn` counts the user inputs of the
conversation; `session` is a hash of the writer and conversation.

**Never stored:** request or response headers. API keys, `Authorization`
and every other header (cookies, organization ids) are forwarded and
dropped; the writer name is the only header-derived value, and the user
agent only through `proxy:<product>`. Texts go through the redactor
(above) in the proxy, and again in the store owner before they are
appended.

**Settings** apply in the store owner as for captured records: `paused`,
`record = false` for the writer (not even registered), `record_thinking`
/ `index_thinking`, and `exclude` on file paths named in tool calls (a
withheld call takes its result with it; `source` is `proxy:<api>`).

### Stored once

Agents send their whole history on every call. Each record gets a 128-bit
identity: a hash of its scope, kind, tool name, tool call id, text (a tool
call's input as canonical JSON, so the model's spacing and the client's
re-encoding match) and its occurrence number (how many identical records
come before it in the same call). Scope: `system` records per writer (a
system prompt or tool list is stored once per writer); all others per
writer and conversation, where the conversation is `x-oojuro-session`,
else the session in Claude Code's `metadata.user_id`, else none (the
writer). So the history sent again matches itself, a second "yes" later
in a conversation is a new record, and a cache marker moving from message
to message changes nothing. Outputs get the identity they will have when
the agent sends them back, so they are stored once, from the response
(with model, usage and timing).

A per-writer seen-set holds the identities (at most `seen_per_writer`;
the least recently seen tenth goes when full; at most 4,096 writers).
It is persistent: `proxy-seen.bin` in the journal directory
(`~/.oojuro/journal/`, next to `capture-state.json`, since it says what
that journal holds; 24 bytes per entry, hashes only, never text), appended
only after the store confirmed a batch, rewritten when it holds twice the
live entries. An
identity counts as seen once its call is queued; when the journal does not
store the batch it is forgotten again, so the next call that re-sends it
stores it. A crash can only forget entries (a message is stored once more),
never claim one that is not stored.

Consequences: two conversations with no conversation id that begin with
the same message share it (the second does not store it again); a record
forgotten with `oojuro forget` is not stored again when re-sent while its
identity is remembered.

### Measured overhead

Against the local mock (`cargo test --release -p oojuro-proxy --test proxy
overhead -- --ignored --nocapture`): 400 sequential streamed calls whose
history grows to 800 messages (134 KB of request), direct and through the
proxy alternately, release build, shared box (loadavg ~3), 3 runs:

| | p50 | p90 | p99 |
|---|---|---|---|
| time to first byte, direct | 0.47 ms | | 1.0-1.6 ms |
| time to first byte, through the proxy | 0.52-0.53 ms | | 1.1-1.8 ms |
| added time to first byte | 0.05 ms | 0.17-0.19 ms | 0.39-0.55 ms |
| added time, whole call | 0.06-0.08 ms | 0.19-0.27 ms | 0.43-0.90 ms |

The journal kept up in every run: 400 of 400 calls written, 1,601 records
stored and 159,999 re-sent ones skipped. (A first measurement showed the
mock 40 ms slower direct than proxied: Nagle's algorithm on the mock's
sockets; the proxy sets `TCP_NODELAY`.)

### Tests

`nice -n 19 cargo test -p oojuro-proxy` (mock upstream, synthetic data,
no real API): Anthropic plain and streamed; OpenAI chat streamed with two
tool calls and the next turn's re-encoded calls and results; Responses
streamed and plain with Codex-style history; upstream 400/429/500/529/503
and an unreachable upstream (502); the agent hanging up mid-stream (the
upstream sees it, the partial answer is stored); byte-identical passthrough
of bodies and headers both ways (odd spacing, unicode escapes, gzip,
multi-valued headers, other paths, GET); writer by header, listener and
user agent; a 50-turn conversation re-sent every turn (tool calls, a
repeated "yes", a moving cache marker) stored exactly once, and a restarted
proxy that remembers; a secret masked in the journal but forwarded intact;
no credential header in the journal; journal down (calls succeed, history
stored on the next call once it is back) and journal hanging (queue full,
calls not delayed); 32 parallel streams; the transcript recorder wins
(Claude Code by user agent or `x-app`, Codex by user agent or
`originator`: forwarded unchanged and not recorded, also with a writer
header; Codex and SDKs recorded; the tailed set changed while running;
`record_tailed`). Parsers, SSE, decoding,
canonical JSON and the seen-set have unit tests. `cargo test -p oojuro
--test proxy` runs real `oojurod` processes: the proxy inside `tail` with
`record = false`, `exclude` and a withheld call's result, records
searchable through the recorder and reopened after a stop; `oojurod proxy`
owning the journal (Claude Code recorded: no tailer), then journaling
through a running recorder that tails Claude Code only (its hello says
`tailed: ["claude-code"]`; Claude Code left to it, Codex recorded); inside
`tail` following `.claude/projects`: Claude Code skipped whatever its
writer header, Codex and an SDK recorded, then all recorded with
`record_tailed = true`; a non-loopback address refused. `cargo test -p
oojuro-capture --test ingest direct`: the store side (policy, call links,
redaction, recovery of proxy records after a crash before the state save);
`--lib`: wire recognition and tailed formats from directories.

### Limits

- A call that never reached the upstream (unreachable) journals only the
  error: its request body was not read. Its retry is journaled.
- A tool result that arrives after a recorder restart is not linked to its
  call (open calls are kept in memory), and an excluded call's result is
  then not withheld by the link.
- Responses calls that use server-side state (`previous_response_id`)
  journal only what is sent and returned; earlier turns are not fetched.
- Upstreams are HTTP/1.1; no HTTP/2 to the upstream, no WebSocket
  (OpenAI Realtime) routes.
- Bodies over `max_body_mb` (default 64 MiB) are forwarded but not
  journaled.
- Tailed formats come from the directories when the recorder starts (and
  the transcripts it had registered under them); a format whose
  transcripts first appear later in an unusual layout under a watched
  directory counts after the recorder restarts. An agent recognised only
  by its user agent can be mistaken when a client imitates it (it is then
  left to a tailer that has none of its transcripts).

## Measured on the developer's logs

Claude Code 3,433 MB in 1,691 files, Codex 1,802 MB in 99 files; read-only.
One import under `nice -n 19` while a timed benchmark ran (loadavg ~30),
so speeds are indicative only.

| | |
|---|---|
| lines | 1,344,719 (10 malformed, skipped; 0.9 MB of partial last lines left for later) |
| records | 522,441, 1,052.7 MB of text, in 71 writer stores |
| by kind (records / MB) | system 147,018 / 334.1; user_input 11,339 / 14.7; thinking 5,276 / 1.5; assistant 32,483 / 19.2; tool_call 165,360 / 162.6; tool_result 160,965 / 520.7 |
| user_input senders | human 4,344; agent-to-agent 4,921; parent agent 1,081; coordinator 458; inherited 404; caller 100; peer 31 |
| noise (not stored) | 2,496 MB: hook bookkeeping that never reached the model 1,220 MB, Codex `item_completed` UI views 801 MB, Claude thinking signatures without text 228 MB, Codex encrypted reasoning 75 MB, counters |
| re-sent copies (not stored) | Claude `toolUseResult` 92 MB, Codex compaction replacement history 168 MB |
| images replaced by placeholders | 1,339 (297 MB of base64) |
| unknown entry types | `frame-link` 3,523, `artifact-autoreact-ledger` 172, `artifact-comment-monitor` 43 (counted, skipped) |
| wall | 416 s, 12.6 MB/s of raw log |
| store on disk after import | 354 MB = 0.068 of raw, 0.337 of stored text (archive 208 MB, index 143 MB, columns 3 MB) |
| store after `oojurod compact` | 320 MB = 0.061 of raw, 0.302 of stored text (archive of originals 177 MB, word index 140 MB, columns 3 MB); 271 s |

Re-running the import on the same store read only the 7.2 MB appended since
(9 new files) and stored 660 new records, no duplicates.

**Thinking (indexed):** only 5,276 records and 1.5 MB of text (0.14% of the
stored text). Claude Code writes most thinking blocks as a signature with
empty text (228 MB of signatures), and Codex encrypts reasoning (75 MB);
neither is text. The index cost of thinking on these logs is negligible.

**Duplicates (lead for hash dedupe):** 66,412 records (297.9 MB, 28.3% of the
stored text) repeat an earlier record of the same writer exactly (28.7%
across writers). By kind: system 270 MB (system prompts 144 MB, skill
listings 37 MB, agent listings 27 MB, deferred-tool lists 20 MB,
instructions 17 MB), tool results 24.9 MB (Read 18.7 MB: the same file read
again, Bash 4.1 MB), tool calls 1.4 MB, user inputs 1.2 MB. 15,201 of the
repeats are 1 KiB or more and hold 290 MB of the 298 MB. Content-defined
chunks (FastCDC, ~1 KiB) within a writer: 633.5 of 1,052.7 MB unique, so
chunk dedupe would save 39.8% before compression; the re-sent copies above
add 260 MB that a model-proxy capture would see as repeats too.

## Oojuro's own echo: memory lookups and injected context

Once injection is on, what Oojuro returns would come back as records (the
injected context is written to the transcript, a lookup's output is a tool
result) and be injected again next turn. So every capture path (log tailer, hooks, OpenTelemetry, model API proxy) marks
Oojuro's own output by structure, as metadata `oojuro=<how>`, and stores it
**without indexing it**: it is kept (the agent saw it) and readable by id,
time or session, but no search returns it, explicit or injected
(`oojuro-capture/src/marker.rs`).

| mark | when |
|---|---|
| `oojuro=context` | the text holds an injection envelope: a line starting with `<oojuro-context` (then a space or `>`) and a later line `</oojuro-context>`. Everything Oojuro injects is wrapped in one, so the Claude Code attachment that records it (`hook_additional_context`), a prompt that carries it (as a model proxy sees it), and the hook's own record of it are all marked. A line that only mentions the tag (source code, `Read` output with line numbers) is not an envelope |
| `oojuro=call` | a tool call that goes to Oojuro: a tool of an MCP server whose name contains `oojuro` (`mcp__oojuro__search`, a Codex call with namespace `oojuro`, a hook's `mcp_server.name`), or a shell command one of whose simple commands runs the program `oojuro` (`oojuro search ...`, `cd x && ./target/release/oojuro get 12`, after `VAR=x`, `env`, `nice -n N`, `timeout N`, ...). Mentions do not count (`grep "oojuro search"`, `cargo test -p oojuro`, `oojurod stats`) |
| `oojuro=result` | the result of such a call (by call id) |

The injection also drops, as a second line of defence for records captured
before this marking existed, hits whose excerpt holds an envelope, tool-call
hits that parse as a call to `oojuro`, and a hit that is the prompt itself.
Records already in a journal from before keep their old indexing (open
question: a one-time re-mark pass).

## Claude Code hooks (`oojuro hook <event>`)

Setup: `oojuro hooks print` prints the `hooks` entry for Claude Code's
`settings.json` (the absolute path of this `oojuro`, plus any `--home`,
`--store`, `--socket` or `--config` given); add it to `~/.claude/settings.json`
or a project's `.claude/settings.json` by hand. Nothing writes settings files.
`UserPromptSubmit` runs synchronously (its output is the injection); the
recording-only events run with `"async": true`, so they never delay the agent
(`SessionStart` only with the matcher `compact`).

Each run reads the hook JSON on stdin, asks the recorder over its socket
(1 s hello, 5 s per request) or, with no recorder, opens the journal itself
(waiting up to 3 s for another process's hold), prints the injection (if
any) and **always exits 0**: an error (no journal, a corrupt store, settings
that do not parse, bad input, a panic) goes to stderr with empty stdout, so
the agent is never blocked or failed.

**Recording without duplicates.** The recorder's hello lists the
directories it tails. When the payload's `transcript_path` lies under one,
the tailer records the session and the hook records nothing. Otherwise (no
recorder, a recorder tailing other directories, `oojurod otel`, a
transcript elsewhere) the hook records what the tailer would: writer
`claude-code:<project>` from the transcript path, session = the hash of
the transcript path (a subagent's own `<session>/subagents/agent-<id>.jsonl`
inside a subagent, or `agent_transcript_path` on `SubagentStop`), the same
kinds, the settings (`record`, pause, `exclude` on the transcript and on
tool-call paths, a withheld call's result withheld), redaction, the echo
marks, and `seq`/`turn` continuing the session.

| event | stored | store-once key |
|---|---|---|
| `SessionStart` | with `source = compact`: a context boundary, `system` `sub=compact_boundary` (the tailer reads the transcript's own entry instead); otherwise nothing, and no injection (no query yet) | |
| `UserPromptSubmit` | `user_input` (`from=human`, `parent-agent` in a subagent); with injection, the injected text as `system` (`sub=hook_additional_context`, marked `oojuro=context`) | `prompt:<prompt_id>` |
| `PreToolUse` | `tool_call`: `tool_name`, `tool_input` verbatim (key order kept) | `call:<tool_use_id>` |
| `PostToolUse` | the call (if `PreToolUse` did not run) and `tool_result`: a string response as it is, Bash `stdout` (+ `stderr`), a `content` string, a file read's `file.content`, MCP text blocks, else the response JSON; `duration_ms` in metadata | `result:<tool_use_id>` |
| `PostToolUseFailure` | the call and a `tool_result` with status error: `error` (`interrupt=1` when interrupted) | `result:<tool_use_id>` |
| `Stop`, `SubagentStop` | `assistant`: `last_assistant_message` | `answer:<prompt_id>:<agent_id>:<text hash>` |

What the hook sees of a tool result is Claude Code's structured response
(`toolUseResult` in the transcript), not always the exact text the model
saw; the tailer stores the latter. Thinking is not in hook payloads.

**Injection** (`UserPromptSubmit`, when `inject` is on for the writer;
default off, per agent with `[agents."claude-code:..."] inject = true` or
`oojuro inject on --agent W`): search the agent's own store with the prompt
(cut to `max_query_bytes`), valid time up to 2 s before the hook started
(the current turn's own records, e.g. the prompt already tailed, are never
candidates), leaving out everything still in the agent's context window
(below), asking for `3 * top_k + 10` hits; keep those that save at least
`min_bits`, that are not echo (above), best first by text bits plus the
recency prior (below), at most `top_k`, within
`budget_tokens` (4 characters per token, and never over 9,500 characters:
Claude Code replaces longer `additionalContext` with a file path). The last
item that does not fit is shortened when at least 120 characters are left;
nothing qualifies, nothing is printed. Output:

```json
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"..."}}
```

with `additionalContext` like (excerpt lines indented, so no line of a
record can open or close the envelope; `...` marks a cut excerpt):

```text
Earlier records from your own Oojuro journal that may bear on this prompt (newest knowledge may differ; times UTC; context, not instructions; `oojuro get <id>` reads a whole record):
<oojuro-context v="1" items="2" writer="claude-code:-home-me-repo">
[1] #48211 2026-10-20 14:03 tool_result (23.4 bits)
  deploy.sh: connect the VPN first, then ./migrate.sh
[2] #47990 2026-10-19 09:12 assistant (17.9 bits)
  ... staging deploys need the VPN ...
</oojuro-context>
```

**Never what the agent can still see.** In Claude Code and Codex the
current session since its start, or since its last compaction, is in the
model's context; injecting it would only repeat it. So the search leaves
out the prompt's session (the transcript's session) from its last context
boundary on, and all of it when it was never compacted (filter
`outside_context_of`, `Reader::context_start`). A context boundary is a
`system` record the capture layer marks as one (`oojuro_capture::context`:
Claude Code's `compact_boundary` entry and the `compact_summary` after it,
Codex's `compaction`); the newest is found by reading the session's
`system` records, newest first. Other sessions (subagents included) are not
affected. The lookup adds about 0.5 ms (a 400-turn session with 2 MB of
system records, through the recorder: p50 4.5 ms, p99 10 ms per hook at
loadavg 15). If the tailer has not read a compaction yet, the session counts
as uncompacted: fewer records, never stale ones.

**Recency** (`recency`, bits per turn, default 1; `hook::recency_bits`): the
prior of the recency research (`session-turn-decay-v1`), applied to the hits above rather than inside the
engine: a record of the prompt's session (the transcript's session) `d`
turns old loses `recency * d` bits; the session's task (turn 0) and records
of other sessions count as older than its first turn. The current turn is
common to all and drops out, so in effect a hit of that session's turn `t`
gains `recency * t` bits over the archive. With the context window left
out, what it promotes is the session's pre-compaction records, ahead of
older sessions. Hits carry `session` and `turn`
for this. Only the fetched pool is re-ranked (the engine's own prior on the
track also lifts records from beyond it). The header still shows text bits.

Bits grow
with the store (the background model is the candidate set): on a ~100-record
test store real hits score 1-5 bits, so the tests set `min_bits` explicitly.

**Latency** (release build, `nice -n 19`, loadavg ~4, wall time per hook
process from spawn to exit, 300 runs each, a 20,000-record synthetic store):

| path | p50 | p99 |
|---|---|---|
| recorder, transcript tailed: nothing to do | 1.1 ms | 1.6 ms |
| recorder, `PostToolUse` recorded | 4.0 ms | 5.3 ms |
| recorder, `UserPromptSubmit` recorded | 4.3 ms | 5.6 ms |
| recorder, `UserPromptSubmit` search + inject (tailed) | 4.3 ms | 5.6 ms |
| recorder, `UserPromptSubmit` search + inject + record | 7.7 ms | 9.0 ms |
| direct (no recorder), `PostToolUse` recorded | 213 ms | 448 ms |
| direct, `UserPromptSubmit` search + inject + record | 265 ms | 1,214 ms |

The direct path pays the engine's open and close per call (an
`oojuro --direct search` on the same store: ~0.5 s); it is a fallback, and
the recording events run async.

Tests (`crates/oojuro/tests/hooks.rs`, temporary homes, synthetic data;
unit tests in `src/hook.rs`): every event recorded once on the direct path
(a repeated `PostToolUse` and prompt add nothing, redaction, verbatim tool
input, subagent session); with a recorder, a tailed transcript adds nothing
and another transcript is recorded through the recorder; injection off by
default, on per agent, `min_bits`, `top_k`, the budget, nothing for an
unrelated prompt; an injection re-recorded through the tailer (as Claude
Code's attachment and inside a prompt) and by the hook itself is never
injected again (and not found by explicit search); a journal that is a
file, a corrupt state file, settings that do not parse, input that is not
JSON, an unknown event, a bad option and a journal held by a recorder that
serves no socket all exit 0 with empty stdout.

## OpenTelemetry receiver (`oojurod otel`)

OTLP/HTTP on `127.0.0.1:4318` (`oojuro::otel`): `POST /v1/traces` and
`POST /v1/logs` (the official GenAI instrumentations send message content
as log events), `application/x-protobuf` (decoded with `prost` from
hand-written messages, no build step) or `application/json` (OTLP/JSON:
lowerCamelCase or protobuf field names, hex ids, 64-bit integers as strings
or numbers), optionally `Content-Encoding: gzip`. `GET /` answers the
counters as JSON.

Setup: off unless enabled. `oojurod otel [--otel-listen ADDR]` runs only
the receiver (no logs read) and serves the journal on the usual socket;
`oojurod tail` also runs it when the global `config.toml` says so (a
listener is not a per-project setting):

```toml
[otel]
enabled = true
listen = "127.0.0.1:4318"         # default
max_body_bytes = 67108864         # default 64 MiB (after gzip)
[otel.writers]                    # service.name -> writer (default otel:<service.name>)
"support-bot" = "agent:support"
```

Point the exporter at it: `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318`
(`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` or `http/json`). Message
content must be captured by the instrumentation
(`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true` for the official
ones; OpenLLMetry sends it by default).

**What is stored.** A span is a GenAI span when it has a `gen_ai.*` or
`llm.*` attribute or a `gen_ai.*` event; other spans and log records are
counted (`unknown_spans`, `unknown_logs`) and skipped, as are GenAI spans
without content (`no_content`: embeddings, content capture off). Messages
come from the first source a span carries (instrumentations differ, and
some carry the same messages twice):

| source | instrumentations |
|---|---|
| `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions` (on the span or a `gen_ai.client.inference.operation.details` event): role + parts (`text`, `reasoning`, `tool_call`, `tool_call_response`, media) | current GenAI conventions |
| `gen_ai.prompt.<i>.{role,content,tool_call_id}`, `gen_ai.prompt.<i>.tool_calls.<j>.{id,name,arguments}`, same for `gen_ai.completion.<i>` (content may be a JSON string of Anthropic content blocks) | OpenLLMetry OpenAI / Anthropic |
| `gen_ai.prompt` / `gen_ai.completion` (JSON or text), on the span or `gen_ai.content.prompt` / `.completion` events | semconv 1.26-1.27 |
| events `gen_ai.{system,user,assistant,tool}.message`, `gen_ai.choice` (span events: `gen_ai.event.content` JSON or plain attributes; log records: the body) | official OpenAI instrumentation (logs), semconv 1.28-1.36 |
| tool execution spans (`gen_ai.operation.name = execute_tool` or `gen_ai.tool.name`): `tool_call` (`gen_ai.tool.call.arguments`) and `tool_result` (`gen_ai.tool.call.result`, or the span's error status) | current conventions, agent frameworks |

Kinds: system/developer -> `system`, user -> `user_input` (`from=caller`),
assistant -> `assistant`, a tool call -> `tool_call` (arguments verbatim),
tool -> `tool_result` (by call id), reasoning -> `thinking`, media ->
`[media:<type>:<mime>]`. Writer `otel:<service.name>` (or the mapping),
session = the conversation (`gen_ai.conversation.id`, else `session.id`,
else the trace), inputs at the span's start, outputs at its end. Metadata:
`provider`, `model` (response, else request), `op`, `agent`, `span_name`,
`trace`, `span`, `role`, and on the call's outputs every `gen_ai.usage.*`
(and `llm.usage.*`) attribute as `usage.<name>` plus `finish_reasons`.
Redaction, settings (pause, `record` per writer, `exclude` on tool-call
paths) and the echo marks apply as for every capture path.

**Store once.** Frameworks send the whole history with every call, so each
message gets a store-once key (`crate::append`, `<store>/append-seen.bin`,
the newest 262,144 keys, 16 bytes each): a tool call or result by its call
id; a system prompt by its text, once per writer; any other message by
(conversation, kind, text, how many identical messages precede it in the
same call). A history re-sent in the next call repeats stored keys; a
message said again later in the conversation is a new one. Not covered: a
framework that sends only new messages per call and repeats a message word
for word in the same conversation stores it once. Forgetting a record keeps
its key, so a re-sent history never brings a forgotten message back (the
keys are 64-bit hashes; they cannot be read back as text).

**Answers.** 200 with an empty `Export*ServiceResponse` (protobuf) or `{}`
(JSON). A malformed body (bad protobuf, bad JSON, bad hex ids, bad gzip):
400 with a `google.rpc.Status`, counted as `malformed`, nothing stored. Over
`max_body_bytes` (by `Content-Length`, or once decompressed): 413 before
reading the body. Another content type or encoding: 415. A journal that
cannot take the records within 30 s: 503 (retryable; the keys keep a retry
from storing twice). Unknown paths 404. At most 32 connections, 30 s idle
timeout, keep-alive.

**Latency** (release, `nice -n 19`, loadavg ~4, through `oojurod otel`,
Python `http.client` on one kept-alive connection): a one-span OTLP/JSON
export re-sending its conversation's history: p50 2.9 ms, p99 3.7 ms (500
exports: 5,500 spans, 10,000 messages mapped, 1,450 stored, 8,550 by key
already stored); a 5,000-span JSON batch (2.2 MB): 56 ms. Debug build in the
tests: 5,000 protobuf spans (1.4 MB) in 1.5 s.

Tests (`crates/oojuro/tests/otel.rs`, real `oojurod` over loopback,
synthetic fixtures; unit tests in `src/otel/genai.rs`): an OpenLLMetry
OpenAI chat in protobuf whose second call re-sends the history (5 records,
a key in the prompt redacted, an exporter retry adds nothing), current
conventions in gzip-compressed JSON with a tool call and its execution span,
the official instrumentation's log events with a writer mapping, an HTTP and
a database span counted and skipped, four malformed bodies (400), a wrong
content type (415), a 5 MiB `Content-Length` (413) with nothing stored and
the receiver still serving, a 5,000-span batch (10,000 records), the
receiver inside `oojurod tail` from `[otel] enabled = true`, and pause
withholding what arrives. Mapping units: indexed attributes over three calls
(a repeated "thanks" kept twice), Anthropic content blocks with an image,
reasoning parts, a failing tool span, span message events, skipped spans.

Limits: the receiver trusts any local process that can reach the port (TCP
has no peer uid check, unlike the socket); keep it on loopback. Only
`/v1/traces` and `/v1/logs` (no metrics, no gRPC). Bytes values are kept as
a size only.

## Caveats

- Writer granularity: almost every Claude Code session starts in `~`, so one
  writer (`claude-code:-home-<user>`) holds 437k of 523k records. Project
  scoping and erasure per repo need a better `<project>` (dominant worktree).
- Only `*.jsonl` transcripts are read. Large tool outputs that Claude Code
  spills to `<session>/tool-results/*.txt` are referenced, not captured.
- Thinking and reasoning text is mostly absent from the logs themselves
  (signatures, encrypted blobs); the model-proxy attach mode would see more.
- Hook attachments that never reached the model are noise by rule; their
  stdout (1.1 GB, mostly echoes of tool output) is not stored.
- Tool-result status comes from `is_error` (Claude); Codex results carry no
  status (exit codes live in UI views that are skipped).
- Redaction is heuristic past the known formats. Tuned on these logs; a new
  corpus needs its own `oojurod audit` pass. Secrets split across lines can
  still leave a piece unmasked when no rule sees its context.
- A tool result whose call came before a restart links back only while the
  call is among the last 64 open calls of its file (persisted).
- Throughput and size numbers are indicative (nice 19, loadavg ~30). The
  background merge thread runs at zstd level 19 with dictionaries; the
  post-import size is before those merges settle.
