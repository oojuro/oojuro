# Opt-in continuing journal setup candidate

Status: implemented and qualified locally with the matching hosted protocol.
Nothing is deployed or released. The default is still a new
journal per new provider run, with its original journal retained on resume.

## Select a continuing journal explicitly

The target must already belong to this same capture integration lineage, native
writer and source/account. Its opaque ID selects the requested destination; it
is not a grant. A name, project membership/read key or another integration's
journal cannot authorize capture into it. Related key replacement follows the
existing native lineage rules. Arbitrary cross-integration reuse is unsupported.

For a fresh protected connection:

```bash
oojuro cloud connect-api --connection /private/connection.json \
  --api-key-file /private/integration-key --label 'Code reviewer' \
  --reuse-journal JOURNAL_ID
```

To change only future unseen runs on an existing integration connection:

```bash
oojuro cloud capture-policy --connection /private/connection.json --reuse-journal JOURNAL_ID
# Restore the default for future unseen runs:
oojuro cloud capture-policy --connection /private/connection.json --new-journal-per-session
```

This atomically changes a private local file; output explicitly says access has
not been verified. It makes no service call. Stop/restart an existing capture
process to load the changed connection. Already queued and resumed runs retain
their original requested target and accepted route. Pending originals/request
IDs never move when settings change. Authorization is checked by the service
before provisioning a new run; failure retains pending input and never falls
back to a new journal. Capture still grants no history reads.

The installer exposes the same mutually exclusive `--reuse-journal ID` and
`--new-journal-per-session` choices. Its dry run shows the selection. With neither
option, it retains an existing saved policy and uses the default on first setup.
Existing connection/binary protections and local capture consent remain. An
explicit policy change does not replace the integration key or queued history.
Legacy fixed-journal setup does not accept these integration-session options.

## Application SDK

```js
const spool = await CaptureSpool.open({ directory, client, reuseJournal: journalId });
// New events still need their real stable run/event IDs. Do not fake one run
// across independent provider sessions to obtain a continuing journal.
spool.enqueue(applicationEvent);
await spool.flush();
spool.close();
```

On reopen, `reuseJournal: undefined` (or omission) retains the saved policy;
`reuseJournal: null` selects the new-session default for unseen runs. An explicit
ID chooses a continuing target for unseen runs. The choice is copied into each
new run's durable intent before network effects. Existing run intents do not
change. The low-level API accepts
`client.prepareSession({runId, label, reuseJournal: journalId})`; it freezes that
choice and refuses a response that names a different journal.

The normal session SQLite spool remains schema2 until the first explicit reuse
opt-in. That opt-in transactionally adds a per-run target and saved policy column
and marks schema3. Existing rows become NULL/default, including unresolved setup
and event requests. Old SDKs refuse schema3 safely. Clearing the policy does not
downgrade the spool or discard retained targets. Legacy schema1 spools do not
migrate into session capture. CLI per-run JSON omits the new field for old/default
intents; older CLIs refuse states/connections containing the explicit new field.
No database engine storage format or runtime dependency is added.

Reuse changes no journal name/project membership and copies/imports no history.
Its existing project readers see retained past/future history under current
membership, with a separate current read key. Run IDs, writer-qualified tags,
original evidence, request identities and exact accepted routes remain distinct.
Current backend/session bounds still apply; reuse does not make them unlimited.

## Focused local qualification

Exact client source `da76c40b039f4df9b24d9e7937709339ef2ddc5f` passed:

- 10 hosted-capture unit tests and 12 actual CLI tests.
- 2 installer boundary tests.
- 114 Node runtime tests, with no failures or skips, including an actual SQLite
  trigger-induced migration failure/rollback and successful atomic retry.
- 8 strict declaration smoke files and all 5 offline installed package export surfaces.

Actual task-local Rust1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`),
Node24.20.0 and the locked declaration checker were used. The initial binary-only
unit selector matched zero; it is not counted. The corrected library selector
passed 10, and both logs are retained. Engine, storage and ranking gates were not
repeated because their source and dependency graph are unchanged.

The separately frozen actual debug CLI SHA is
`d2a6b7adcb36d6d3541eb0df986ad6133f2bb9b26e230ff56bd6aa72fdb1bd82`.
Separate hosted synthetic HTTP/TLS-SCRAM PostgreSQL journeys qualified the actual
SDK and this same CLI/MCP executable, alongside previous fixed client artifacts.
They exercised reuse, restoration of future-session defaults, resume of previously
reused runs, exact project context and current read-authority/refusal guards.
These are local protocol checks, not an actual owner-agent registration or import.

Raw local client receipts are in `docs/evidence/journal-reuse-client/`.
No provider, production, performance, real-owner history, main merge or package
release acceptance is claimed.

## Topic composition provenance

The qualification-only parent topic was merged locally before opening this draft.
The source/manifests/scripts trees are byte-identical before and after composition;
`topic-merge-proof.json` records the Git object IDs. Raw logs remain unchanged,
including final blank lines. This is a topic merge, not a main merge, release or
deployment. No unchanged functional gate was rerun for documentation composition.

The [combined setup command](COMBINED_AGENT_SETUP.md) composes the existing
capture and optional project-read helpers with separate grants. It retains an
existing installation's journal policy; change that policy through the existing
client/helper options and restart capture with the same spool. Source composition
and the pending combined-wrapper qualification are recorded separately; prior
component passes do not imply this new composed wrapper has been executed.
