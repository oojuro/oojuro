# Opt-in continuing journal setup candidate

Status: source candidate awaiting functional qualification and matching hosted
reuse capability. Nothing is deployed or released. The default is still a new
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

## Source checks awaiting execution

Added source tests cover strict low-level target identity, lost setup/event ACKs,
version2 pending-default preservation across a version3 upgrade, offline reopen,
saved/default policy changes affecting only new runs, local actual CLI private
configuration, installer preservation and declaration surfaces. The hosted native
and HTTP/PostgreSQL authority gates are a separate candidate. No composed gate
has run at source freeze, and no production/owner-history/performance acceptance
is claimed.
