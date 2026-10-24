> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/COMBINED_AGENT_SETUP.md).
> The content below describes the pre-split monorepo at its recorded revision.

# One agent setup command (review candidate)

Use one command for capture and optional project-history MCP. This composes the
existing [capture installer](AGENT_SESSION_INSTALLER.md) and
[project MCP helper](HOSTED_MCP.md#guided-project-setup); it adds no credential
format, authority, package dependency or configuration writer.

With a trusted CLI and this checkout, first review the plan:

```sh
python3 scripts/setup-agent.py --adapter codex --name 'Code reviewer' \
  --project PROJECT_ID --dry-run
```

Then run the same command with the two keys already saved in private files:

```sh
python3 scripts/setup-agent.py --adapter codex --name 'Code reviewer' \
  --project PROJECT_ID --api-key-file /private/integration-key \
  --read-key-file /private/project-read-key --accept-local-capture
```

Use `--adapter claude-code` for Claude Code. Files must satisfy the existing
helpers' mode-0600/ownership/path checks. Omit `--project` for capture only.
Existing environment keys or interactive hidden input also work; each helper
receives only its own Oojuro key environment variable, and CLI/service/agent
children receive neither. Never paste keys into chat or command arguments.
`--binary`, `--capture-directory`, `--project-directory`, `--source-root` and
`--no-service` are optional explicit overrides. Linux background capture retains
the existing installer platform limits.

Both helper dry runs validate before setup starts, without reading keys or
changing configuration. Then capture setup runs, followed by project setup.
New activity only is captured: new provider sessions create new journals;
resume/retry retains the original journal. The project read key separately grants
access to all currently attached journals, including previous sessions. The
command never attaches journals, creates a read grant, imports old activity,
reads referenced files or treats a capture key as a read key.

This is sequential setup, not an atomic transaction. If project access or MCP
registration fails, successful capture setup remains in place and may be running.
The final status identifies the failed phase. Inspect the helper's capture status
and finish `connect-project.py` separately. Existing MCP entries refuse, including
an identical one; do not rerun the entire bundle as a replacement operation.
Connection/spool/configuration safety and unknown-outcome rules remain those of
the two helpers. Capture's delivered-event count and a real client `records`/`get`
call are still needed to verify activity and prior-history access. Discovery,
service activation and MCP registration are reported separately.

## Existing journal policy

A fresh capture installation keeps the default: one new journal for each new
provider session. Reusing an existing capture directory retains its saved policy.
The wrapper adds no policy flags. To choose an existing same-integration journal,
stop capture and use the existing client command on that installation's protected
connection:

```sh
oojuro cloud capture-policy --connection /private/capture/connection.json \
  --reuse-journal JOURNAL_ID
# Restore new-session defaults for future unseen runs:
oojuro cloud capture-policy --connection /private/capture/connection.json \
  --new-journal-per-session
```

Restart capture using the same installation and spool to load the choice. Queued
and resumed runs retain their original targets and pending bytes. A journal ID is
a selector, not authorization; the service checks the same integration lineage,
writer, source and account before binding a new run. No refusal falls back to a
different journal. Capture policy changes do not grant project reads, attach
journals to projects, rename journals or import history. See
[continuing journal setup](CONTINUING_JOURNAL_SETUP.md) for the existing CLI,
installer and SDK choices.

## Copy-paste prompt

> Set up Oojuro with scripts/setup-agent.py from the trusted client checkout.
> Select this agent's adapter and my agent-role name. Use my project ID if I
> want prior-project history. Show the dry-run plan first. Use the capture
> integration key and separate project read key from protected local files or
> hidden input; keep both out of chat, argv, logs and agent configuration.
> After my local capture consent, enable new-activity-only capture and optional
> project MCP. Preserve existing connections, spools and MCP entries; stop on
> conflicts. Report partial setup explicitly. Verify accepted new transcript
> activity separately from service status, and verify prior context with an
> actual records/get call after reconnecting the client. Do not import history
> or open referenced local files.

Three subprocess boundary tests pass, covering both adapters and partial/failure
paths with the actual helper scripts and synthetic CLI/agent boundaries.
[Qualification receipt](combined-agent-setup-qualification-2026-10-24.json) records
the exact source and limitations. Actual combined CLI/HTTP/hosted acceptance, owner
installation, service delivery and model-driven tools remain unverified. No main
merge, package release or production activation is implied.
