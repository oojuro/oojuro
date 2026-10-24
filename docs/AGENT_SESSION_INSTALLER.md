> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/AGENT_SESSION_INSTALLER.md).
> The content below describes the pre-split monorepo at its recorded revision.

# API-key session installer (review candidate)

This implements the CLI/installer step of the session-journal contract. The paired hosted integration-session API is a separate private candidate. Its synthetic joined CLI/HTTP/PostgreSQL journey now passes; production activation and actual owner agent/service acceptance remain unfinished.

## One local setup

For capture and optional prior-project MCP in one command, use the
[combined setup helper](COMBINED_AGENT_SETUP.md). The separate helper below
remains available for capture-only setup or finishing a partial installation.

With a trusted built `oojuro` CLI on PATH and this installer available, select the adapter and agent role:

```sh
python3 scripts/connect-agent.py --adapter codex --name 'Code reviewer' --accept-local-capture
# Or: --adapter claude-code
```

The installer detects that adapter's usual transcript root and uses a private per-adapter installation under `~/.local/state/oojuro/`. `--binary`, `--directory` and `--source-root` are explicit overrides. `--dry-run` shows the selected adapter/path and new-activity-only plan without reading a key or creating files. It preserves the agents' existing configuration; one per-user Linux service tails the selected transcript root across sessions. It does not create a service per journal.

Supply an integration API key through hidden terminal input, a protected mode-0600 plaintext `--api-key-file`, or an existing `OOJURO_API_KEY` environment variable. Never paste a key into agent chat, a prompt, URL or command argument. Environment/hidden input is copied through a private temporary file, removed after the protected connection is created, and is removed from CLI child/service environment. Connection and spool files remain private. Existing different binaries, service definitions, legacy connections or spool bindings are preserved and require explicit inspection rather than silent migration.

The advanced CLI entry is:

```sh
oojuro cloud connect-api --connection /private/connection.json \
  --api-key-file /private/integration-key --label 'Code reviewer'
```

This creates a distinct strict version-2 connection file. It needs no source/journal IDs, fingerprint or repeated browser approval. `cloud capture` retains explicit `--accept-local-capture`, adapter, source root and spool options. Existing version-1 fixed-journal connections, browser-approval commands and spool serialization remain compatible.

## Session identity and retry

Each actual provider session gets one logical journal by default. The client identifies Codex session metadata and Claude session IDs through bounded first-header inspection; UUID transcript filenames are the next fallback. Claude sidechain filenames distinguish sidechain sessions that report their parent's session ID. A bounded path fallback is pinned when provider identity is unavailable. It excludes inode identity, so restarting a process does not create a journal. A fallback cannot prove provider-session identity across arbitrary file moves; real metadata/UUID identity is preferred.

Run identity, selected label, original event batch and file cursor are durable before session creation or capture effects. The client posts the same run/label to `/api/capture/sessions` after an unknown outcome; a changed configured role does not rename a pending mapping. The returned logical journal, physical source, agent, run and exact same-origin relative events URL are validated and saved before sending. Every batch uses that exact journal route. The framed event format, event identity, original pending bytes, redaction and response validation are unchanged. New journals receive names such as `Code reviewer / codex / <session suffix>`; display names never select authority. Rename the journal in the account UI without changing local routing IDs.

Existing transcript bytes are excluded when the protected spool is initialized. Bounded header reads only identify the session and do not import those records. Referenced tool-result files are not opened. A new transcript file after initialization is processed using the existing supported parser; this is not authorization for arbitrary local-file reads or an old-history import.

## Verification and limits

The installer reports discovery/connection status separately from accepted transcript events. In session mode it does not publish a setup marker as evidence of real capture. `waiting for new transcript activity` means exactly that. A later capture report with accepted new transcript events is required before claiming real-history verification. The setup receipt includes `local_status_command`: `cloud capture-local-status --connection FILE --state DIR` reads atomically saved acknowledgement counters while the service holds its lock, after matching current discovery identity. It does not grant project/server-history reads. Linux user-service installation is the initial background mode; Unix CLI foreground capture remains explicit. Synthetic installer tests do not prove a real systemd/agent installation.

Capture authority does not grant reads. Optional project read credentials and [guided MCP setup](HOSTED_MCP.md#guided-project-setup) remain a separate grant until the one-setup bundle is completed. Reading prior sessions uses current project authority and attached journals. This step does not change file/attachment privileges, enable original-history import, publish a package, or deploy a hosted API.

### Synthetic joined acceptance

The unchanged frozen runtime `f6429cae9b6d4c594c39bd634ac210706aeb38b6`
(binary SHA256 `0e093079e4a7b8f9194a48ba857bffc0f60ff7a1d26604dec2d6c51cfa17b142`)
passes the actual hosted HTTP/native session/PostgreSQL journey in the paired
[o-cloud draft74](https://github.com/oojuro/o-cloud/pull/74), runtime
`d61cdc99919550c4c33e20c295b2ad2cef8fb0be`. The 14-test PostgreSQL/TLS/HTTP suite
includes one actual frozen CLI journey: old bytes excluded, two new synthetic
provider sessions create two journals, three transcript events accepted, fresh
process/spool reopen preserves the first journal, a separate project read key
finds and opens previous context exactly, and the capture key receives404 for
project reads. A strict acknowledgement extra-field mismatch found by this
journey was corrected in the hosted candidate; the frozen CLI remained unchanged.

This supplements the original 38 Rust and two installer subprocess tests. It
does not qualify a real Codex/Claude installation, systemd, owner history, AWS
activation/recovery, installed MCP client or SDK setup. Source/binary identity
and the historical local checks are preserved in
[the qualification report](session-installer-qualification-2026-10-23.json).

## Copy-paste agent prompt

> Set up Oojuro capture for this local Codex or Claude Code installation using the trusted CLI and session installer. Inspect the supported transcript location and show the dry-run plan first. Use one integration API key from a protected local file, existing environment, or hidden terminal input; never ask me to paste it into chat and never print it or put it on argv. Use my agent role as the base journal name. Enable capture for new activity only: each new provider session gets a new journal, while resume and retry keep the same mapping. Preserve existing configuration and old connections/spools; do not silently replace a different installation. Do not read referenced files or import history. Start the per-user service, then distinguish discovery/service status from an actual accepted new transcript event. Keep project read/MCP access separate unless I have granted it. Report what was verified and what is still waiting.
