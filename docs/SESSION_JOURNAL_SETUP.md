# Session journal setup contract

Owner correction, 23 October 2026: **new session → new journal by default**.
This supersedes the earlier assumption that every session of a named agent goes
into one continuing journal. The following is the target contract, not a claim
that the current private fixed-journal client implements it.

## User experience

- One API key and one connect command, or a prompt that lets Codex/Claude Code
  install and configure the integration. Accept keys in hidden terminal input
  or an existing protected file, never in agent chat, command arguments or URLs.
- Detect supported integrations and preserve their existing configuration.
  Enable automatic capture and verify an actual accepted transcript event.
- Each new session creates a named journal automatically. Suggested names may
  include the agent role and session date; users can rename them.
- A user may explicitly choose an existing journal and save persistent reuse as
  an integration preference. An exact journal ID is the saved routing authority;
  an ambiguous display name is not. Renaming does not redirect future writes.
- Journals are independent of projects. A project may reference several complete
  journals, and one journal may appear in several projects without payload copies.
- The normal path does not require manual source/journal IDs, fingerprints or
  repeated browser approval. Project read/search authority remains scoped.

## Identity and delivery

Use the supported agent's stable session ID. Resume, process restart, delivery
retry and compaction within that session continue the same journal. A genuinely
new session gets a new one. Server provisioning is idempotent for the authorized
account/integration/session mapping, including concurrent installers.

The integration's setup credential may create permitted journals and delegate
append-only capture; it is not a storage-root, IAM or unrestricted project-read
credential. Revoking an integration must deny its delegated capture too. Persist
the session-to-journal mapping and pending original request before uncertain
network effects, so retry never silently writes a session into another journal.

Preserve original event identity, redaction, complete-response validation and
durable pending batches. The initial setup still excludes existing transcript
bytes. It may inspect bounded session metadata to identify the active session;
that does not authorize importing old transcript history.

Logical session journals must share physical infrastructure efficiently. Do not
create a KMS/secret/service/instance set per session or expose another journal
through a shared physical store. API/session mapping, native read selection,
sharing, revocation and stream EOF must all enforce the logical journal scope.

## Existing private clients and remaining gates

The current `cloud connect` command and `scripts/connect-agent.py` require an
existing journal and append future sessions there. They are explicit reuse
clients, not the new default setup. Keep existing credentials, journals, spool
and history intact; do not silently split or import their old records.

Before calling the new setup ready, verify two real new sessions create separate
journals, resume/retry preserves identity, explicit reuse works, duplicate names
are safe, rename preserves IDs/sharing, integration revocation stops delegated
capture, and a project cannot read an unshared sibling in the same physical
store. Exercise actual installed Codex/Claude configuration, MCP and direct SDK.

Engine/CLI/SDK/integration behavior belongs in `oojuro/oojuro`; account API keys,
provisioning authority, hosted logical-journal directory, UI, billing and AWS
operations belong in private `oojuro/o-cloud`.
