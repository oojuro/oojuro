> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/journal/blob/main/docs/NATIVE_PI_CAPTURE.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Explicit native Pi capture

oojuro_capture::local::LocalCapture::open_pi selects Pi session JSONL v3. It never
discovers sessions, opens referenced parent/full-output files, forwards raw data,
or changes the legacy daemon's format selection. Existing Codex/Claude adapter identities
and persisted receipts are unchanged; opening their authority as Pi is refused.

## Contract and evidence

The caller chooses one source generation, writer, agent and policy identity and authorizes
raw capture before source inspection. The writer is pi:<source parent directory name>.
Each bounded block uses the existing sealed atomic raw/parsed/context/allocator receipt.
No-newline tails keep their original policy identity; incomplete headers and UTF-8 sequences
can span blocks. Re-reading a committed source prefix does not allocate duplicate IDs.

Pi v3 headers, ISO entry times and millisecond message times are checked. User/assistant/
thinking/tool events are mapped; tool results require a retained matching call so an alternate
reported tool name cannot bypass its policy. Unresolved calls are bounded without eviction.
An empty tool result has a derived [empty tool result] marker and still closes its call.
Direct bash exits become typed exit/status fields; referenced full-output files stay unread.
Unknown entries/roles, malformed JSON and non-UTF-8 lines remain byte exact in encrypted raw.
Unsupported versions, duplicate unresolved calls and unbound tool results stop that candidate
before ACK. There is no automatic historical-session migration.

Branch entry IDs/parent IDs are retained as reported metadata. Compaction, branch summaries,
context edits and system prompt changes are stored as unindexed observed events. Old branch
records remain history. This is not active-context reconstruction, a verified causal graph,
or evidence that a forked session was spawned as a subagent. Header parent-session paths
remain raw data and confer no filesystem access. Caller IDs/names never become access grants.

Synthetic fixtures follow the official Pi [session format](https://pi.dev/docs/latest/session-format)
and [message definitions](https://pi.dev/docs/latest/message-types), checked October22,2026.
Focused regressions cover policy/restart, exact raw roundtrip, redaction, millisecond conversion,
call/result links, capacity, unknown/malformed lines, branch preservation, split input, consent,
format separation and actual SIGKILL before the capture caller receives ACK.

## Remaining composition

This is an opt-in embedded adapter and native CLI lane (setup --harness pi).
The actual CLI setup/capture/reopen/search/receipt/export/raw-restore fixture also covers Pi.
Automatic policy-controlled
daemon attachment, adapter version negotiation/conformance publication, injection, hosted
forwarding and full benchmark/adoption gates require separate composition. Raw remains local.
