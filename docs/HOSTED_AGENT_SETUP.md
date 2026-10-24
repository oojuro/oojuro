> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/HOSTED_AGENT_SETUP.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Hosted agent capture — private preview

The existing `oojuro` CLI now supports browser-approved capture connections and
new-activity transcript capture. This candidate is private, not a published
package or a public release. Local recording/read commands keep their behavior.

## Model and consent

This existing private client explicitly connects to one selected journal and
reuses it across sessions. The corrected product default is one new journal per
new session; that setup is not implemented by this command. See the
[session setup contract](SESSION_JOURNAL_SETUP.md). Projects are optional
collections of complete retained journals. Capture keys cannot read/search or
administer an account. Project read keys are separate.

The Linux installer supports **Codex JSONL** and **Claude Code JSONL**. After
explicit consent it tails the selected transcript root, using the existing
parsers and known-format secret redactor. Existing bytes are excluded on first
initialization, including a partial last line. New sessions are discovered.
Messages, tool calls/results and visible reasoning are covered where recorded by
the supported parser. Encrypted reasoning, referenced tool-result side files,
non-transcript activity and unknown harness formats are not captured. A known
secret redactor is not a guarantee that arbitrary secrets are detected.

Stored evidence is the captured, redacted text. Images retain parser
placeholders; records do not recover an unredacted secret or an original binary
image. Source/agent labels are reported provenance, not process attestation.

## Connect

Start with an existing registered journal in the signed-in hosted account.
Automatic creation/provisioning of a new cloud journal remains a separate gate.
The Journals → Connections page supplies the two non-secret IDs below.

Build the pinned private candidate with `cargo build --locked --release -p
oojuro --bin oojuro`. Then run the installer with absolute paths:

```sh
python3 scripts/connect-agent.py \
  --binary /absolute/path/to/oojuro \
  --source SOURCE_ID --journal JOURNAL_ID --name 'My named agent' \
  --adapter codex \
  --directory /absolute/private/installation \
  --source-root /absolute/path/to/.codex/sessions \
  --dry-run
```

Review the displayed source scope. Run the same command with
`--accept-local-capture` in place of `--dry-run`. It prints a browser approval URL
and a fingerprint. Open it on your signed-in account and compare that fingerprint
with your terminal before approving. An approval expires after ten minutes.
A connecting machine generates its secret using the OS RNG and saves it in a
mode-0600 file. The URL/browser receive only its project-bound public BLAKE3
credential ID; possession of that public ID never grants capture access.
Owner session, CSRF, Origin, current source ownership, owner lease and witnessed
capture-key mutation still authorize approval. GET cannot create a grant.

The installer waits for discovery, initializes a protected durable spool, sends
a retry-safe **unindexed connection-check marker**, installs a per-user Linux
systemd service, and checks its active state. `--no-service` verifies connection
and delivery while leaving capture stopped. On macOS use the foreground CLI
below; the service installer does not claim macOS support.

A setup marker proves the connection can deliver, **not** that real agent
history was captured. Create new activity in your agent; verify the accepted
transcript counter and open its original record in an attached project. A local
queue, successful process launch or active service alone is not that proof.

The protected installation holds the exact executable, `connection.json`,
`setup-receipt.json` and `spool/`. Keep these files out of chat and repositories.
Capture is new-activity only; existing-history import is not implemented by this
command. The existing legacy capture service is not migrated or disabled.

## CLI and recovery

```text
oojuro cloud connect --connection /private/connection.json --source ID --journal ID --label NAME [--base-url URL]
oojuro cloud connection --connection /private/connection.json
oojuro cloud capture-status --connection /private/connection.json
oojuro cloud capture-check --connection /private/connection.json
oojuro cloud capture --connection /private/connection.json --state /private/spool --source-root /selected/transcripts --adapter codex|claude-code --accept-local-capture [--once]
```

- `connection` emits only the approval URL/fingerprint. `capture-status` checks
  current authorization, identity, expiry and limits. Neither returns a secret.
- Each accepted transcript line stages original event IDs, metadata and request
  together with its next cursor in one private, fsynced atomic snapshot **before**
  sending. Unknown, malformed, truncated or mismatched receipts retain that
  pending batch. Reopen replays the exact request/body; the server deduplicates.
- One file lock allows one writer per spool. The spool binds origin, journal,
  agent, producer, adapter and source root. Changing a project does not change
  capture identity. An authorized replacement key for the same producer may
  reuse the spool; never repoint it to a different journal/adapter.
- Bounds: 10,000 tracked files, 50,000 directory entries per scan, 16 MiB per
  complete line/text batch, 100 emitted events and 64 KiB metadata per batch,
  40 MiB retained snapshot. Oversize/invalid input stops with a visible static
  error and does not advance its durable cursor. There is no silent drop.
- Transcript paths must be owned regular files with no symlink ancestors or
  group/other write access. Truncation, inode replacement, or a changed trailing
  256-byte cursor boundary stops capture. This is an append-only source check,
  not full-file immutability proof. Keep original transcripts for recovery.
- A source line's timestamp/session and stable source coordinates provide event
  identity across restart. A run ID distinguishes transcript files; it does not
  prove causal order or authenticate the reporting process.
- Snapshot serialization and request framing temporarily copy bounded data.
  The 40 MiB snapshot limit is not a process-RAM cap. The service applies a
  separate 512 MiB memory limit and 50% CPU quota, with a 30-second restart delay.
- An abrupt crash during snapshot replacement can leave a private temporary file;
  reopening under the spool lock removes validated owned temporary copies.
- The connection check uses a stable event/request, timestamp and body derived
  from the retained connection. Retrying after an unknown result cannot make a
  second setup marker. Retain the connection file until its outcome is known.
- To stop capture, run the `stop_command` from the local receipt. Revoke the key
  in Journals → Connections to deny future appends. Local spool/transcripts are
  retained for explicit cleanup; stopping/revoking does not erase cloud history.

## Verification scope

Unit tests cover private connection files, fingerprint privacy, HTTPS/redirect
boundaries, single-writer/binding/source-change refusal, known-secret redaction,
Claude side-file exclusion, lost-receipt exact replay and new-activity defaults.
The cloud repository's disposable PostgreSQL/TLS fixture joins actual CLI and
installer processes with owner approval, discovery, automatic capture, restart,
project metadata and exact original readback. It also exercises outsider/CSRF/
expiry refusal and a repeated approval. Fixture users/data are synthetic; this
is not an owner's production-history acceptance.
