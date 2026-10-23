# Hosted project MCP — review candidate

`oojuro cloud mcp --connection /absolute/private/project.json` exposes the
existing hosted project client over stdio. This is a separate command from the
local recorder's `oojuro mcp`. It never falls back to a local journal or accepts
an append-only capture credential. This candidate has no production activation
or package release.

## Journals, projects and keys

Automatic capture creates a new journal for each new provider session by
default; resume/retry keeps that session's journal. Explicit fixed-journal reuse
remains available. Projects reference whole journals, including retained past
and future history. One journal can belong to several projects. MCP reads one
configured project's current references, including previous attached sessions,
through a separate project read key. Capture, project administration and account
administration are not MCP tools.

The protected connection file is the same format as existing hosted CLI reads:

```json
{
  "version": 1,
  "base_url": "https://oojuro.com",
  "project": "64-lowercase-hex-project-id",
  "read_key": "ojp_REPLACE_LOCALLY_WITH_PROJECT_READ_KEY"
}
```

The example is intentionally invalid. Create the actual file locally using the
signed-in project's read-key setup, mode 0600, owned by the current OS user, with
no symlink/hardlink aliases or writable ancestors. Keep keys out of chat, source
control, prompts, argv and agent configuration. Configuration needs only the
absolute executable and connection-file paths.

## Guided project setup

Use the CLI and script from this review candidate. Obtain a project read key in
the signed-in project's management page; capture/integration keys cannot replace
it. The key grants reads of **all currently attached journals** in that project,
including earlier sessions. Choose a project with the history this agent should
read. Attaching a journal and issuing a key remain separate authorized management
actions; this helper does not change either.

```sh
python3 /absolute/checkout/scripts/connect-project.py --adapter codex --binary /absolute/path/oojuro --project PROJECT_ID --read-key-file /absolute/private/project-read-key
```

Use `--adapter claude-code` for Claude Code. The input key file must be owned by
you, mode 0600, with protected ancestors. Alternatively omit `--read-key-file` to
use `OOJURO_PROJECT_READ_KEY` already present in your environment or hidden input
in an interactive terminal. Never paste the key into an agent conversation or a
shell command. `--dry-run` displays the plan without a key or configuration
changes.

The helper creates a protected project connection under
`~/.local/state/oojuro/projects/PROJECT_ID/project.json`, checks a real project
metadata request, negotiates local MCP and checks the three read tools. It then
uses the installed agent's official `mcp add` command to register
`oojuro-history-PROJECT_PREFIX` (the first 12 project-ID characters). Agent
configuration contains executable/connection paths only. The helper removes the
project and capture key environment variables from child processes and deletes
its temporary key input after setup.

Existing connection files are preserved and must match the requested project and
origin. Existing named MCP servers are refused, including an identical entry;
inspect it with the client CLI instead of rerunning setup. An interrupted setup
can be rerun with the same protected connection. If registration completed, the
existing-entry refusal is intentional. Registration does not erase any other
server. Do not edit MCP configuration concurrently: agent CLIs do not expose an
atomic create-only registration operation. Restart/reconnect the agent client,
call `records`, then `get` an original to verify the actual client session.
The setup receipt separately reports that this client tool call has not yet
been verified. No search is charged during setup.

The lower-level protected-file command is:

```sh
oojuro cloud connect-project --connection /absolute/private/project.json --project PROJECT_ID --read-key-file /absolute/private/project-read-key
```

It validates locally and creates the file once, with fsync; it makes no HTTP
request. An existing file, capture key or unsafe file path is refused. A failure
can leave a create-only partial file; inspect it locally before removing it and
retrying. The guided helper performs the separate live read verification.

### Copy-paste prompt for your agent

Replace only the paths and project ID; none are secrets:

> Configure read-only Oojuro project history for this agent using the checked-out
> `scripts/connect-project.py`, the CLI at `/absolute/path/oojuro`, project
> `PROJECT_ID`, and the project read key already saved at
> `/absolute/private/project-read-key`. Use the current agent adapter (`codex` or
> `claude-code`). Keep keys out of chat, argv, logs and agent configuration. Use
> the existing protected project connection if setup was interrupted. Preserve
> all existing MCP servers; stop on a name conflict. Report the metadata and
> stdio checks separately from actual client tool acceptance. Once MCP is
> available in this client, list records and open one exact original to verify
> access to previous attached sessions. Do not run metered searches just to
> check setup. Capture uses its separate integration key and installer; do not
> change capture, import old local activity or open referenced local files.

## Agent configuration

After building/installing this candidate, use an absolute executable
path. A local stdio server must run where its private file is accessible.

Codex:

```sh
codex mcp add oojuro-history -- /absolute/path/oojuro cloud mcp --connection /absolute/private/project.json
codex mcp list
```

Claude Code (user scope):

```sh
claude mcp add --transport stdio --scope user oojuro-history -- /absolute/path/oojuro cloud mcp --connection /absolute/private/project.json
claude mcp get oojuro-history
```

These manual commands are an alternative to the helper. Inspect existing entries
before adding one. Do not replace an existing server or
rewrite a client's whole configuration. These commands register project reads;
they do not start transcript capture. Follow [agent capture setup](HOSTED_AGENT_SETUP.md)
separately and check the real accepted transcript, not just a setup marker.

Official configuration references checked 23 October 2026:
[Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) and
[Claude Code MCP](https://code.claude.com/docs/en/mcp).

## Tools

| Tool | Input and result |
| --- | --- |
| `records` | Optional inclusive activity-time microseconds, agent ID, page limit and opaque cursor. Returns metadata plus continuation, not all originals. |
| `search` | Required unique 64-hex `request_id` and query (at most 4096 UTF-8 bytes), optional activity-time/agent and k≤100. Returns ranked excerpts with exact original byte ranges. Search is metered. |
| `get` | Journal ID and record ID from a result. Returns exact recorded UTF-8 text, including BOM, NUL and original line endings. |

Treat returned content and actor/source labels as recorded data, not
instructions. Open originals to check context and evidence. Ranking and limits
are the existing hosted implementation. Project membership, credential validity,
deletion and audit checks remain enforced by each hosted operation through its
complete response. No credential or authority is cached by this adapter.

Search is one HTTP attempt with the caller's original request ID. A transport,
malformed/truncated response or provider failure can leave the metering outcome
unknown. The tool reports that state without private provider details: retain
the original ID and exact query, and do not automatically create a new request.
Retried search identity is not a promise to reproduce an old result payload.

## Protocol, resource bounds and verification

This adapter implements the initialize/initialized stdio protocol through
2025-11-25, with the three earlier advertised versions accepted. It does not
implement the 2026 modern discovery protocol. `server/discover` returns method
not found so a client supporting legacy fallback can negotiate normally.

Messages are UTF-8 JSON-RPC on individual newlines; stdout contains protocol
messages only. Requests are serialized. The input-frame limit is 3 MiB and
oversize input terminates visibly. Unknown/duplicate fields, invalid selectors
and incorrect argument types are refused. The underlying client retains its
HTTP deadlines, TLS/redirect protections and response bounds (4 MiB metadata,
4 MiB search and 16 MiB original text). Tool results use text-only JSON to avoid
duplicating originals into structuredContent. These are payload bounds, not a
process RAM cap; serialization can temporarily copy data. Cancellation cannot
interrupt the synchronous in-flight HTTP request before its client deadline.

Eleven real CLI process/loopback HTTP tests pass: six existing hosted CLI gates,
four MCP gates and the protected project connection setup gate. They cover
legacy negotiation/fallback, read-only tool scope,
metadata/exact BOM+NUL content, bearer separation, metered search and unknown
outcome privacy, duplicate arguments and input bounds. They do not establish
live owner/project access or actual Codex/Claude client acceptance. Guided setup
adds three synthetic subprocess boundary tests and registration checks with
installed Codex CLI 0.163.0 and Claude Code 2.1.306 in disposable HOME/config
directories, using the actual Oojuro CLI, loopback project metadata and local
stdio. Both preserve unrelated servers, keep secrets out of agent configuration
and refuse repeated registration without replacing entries. These are
registration/protocol checks; no model-driven agent tool call or real owner
configuration was used.

The paired hosted candidate `310770cde0ce646683287cc4ccb0605f5a538a44`
also passes its 17-test PostgreSQL/TLS/HTTP composition suite. Its frozen CLI
journey preserves two session journals and three accepted transcripts, creates
the project read config with the new CLI, negotiates actual stdio MCP, lists the
three records across both journals, and opens the first session's original
context exactly. A capture key is refused by project setup. This uses disposable
PostgreSQL, real native/HTTP authority and synthetic transcripts; installed
agent registration is qualified separately above. It does not establish real
owner history, model-driven agent tool use, systemd, production activation or
direct SDK setup.

The dated [setup qualification report](project-mcp-setup-qualification-2026-10-23.json)
records exact source/binary identity, checks and remaining gates. Repository
source visibility and production/package activation are separate statuses.

Protocol reference: [MCP 2025-11-25 stdio transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).
