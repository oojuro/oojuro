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

Source candidate only until the qualification receipt is added. No owner
installation, production activation, main merge or package release is implied.
