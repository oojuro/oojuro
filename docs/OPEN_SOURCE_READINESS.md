# Open-source readiness — 24 October 2026

Assessment boundary: public `origin/main` at
`abbfc71465704fce546eb86c0af66ef301585c15`; local native/client candidate
`d1b24f7ed020365e09431aeaa8f4a7ff05e27dc3` from draft
[92](https://github.com/oojuro/oojuro/pull/92). The candidate contains 166 commits
not in that main snapshot. This transport proposal is stacked on that candidate;
neither its existence nor a local test pass merges or releases its parents.

The consolidation candidate composes those heads with the `.ojo` naming decision and
[retires legacy front doors](SUPPORTED_INTERFACES.md). It targets main directly in [PR 96](https://github.com/oojuro/oojuro/pull/96).
Historical compatibility is no longer a release requirement; current-format
corruption, interruption, retry and recovery guarantees remain required.

## Stage

Oojuro is a **pre-release source project with an experimental native engine and
client SDK**, suitable for deliberate evaluation. It is not an accepted stable
v1 binary/package distribution or a demonstrated general replacement for SQLite.

| Area | Evidence and boundary |
| --- | --- |
| Public source | Main states pre-release, publishes Apache-2.0 code with NOTICE/provenance, contribution guidance and private vulnerability reporting. Formats/interfaces may change. |
| Native local journal | The candidate supports encrypted typed appends, exact retry receipts, original reads, versions, time filters, search and paginated export. JSONL input is in draft 92. |
| Limits | The current direct Record namespace has one owning writer, 100,000 records and 128 MiB serialized replay metadata. SQL joins, arbitrary updates and multi-writer transactions are outside this API. |
| Hosted client | Session capture, durable local spool, separate project reads and explicit file helpers exist in candidate branches. Account/cloud activation and deployed-version compatibility require separate joined acceptance. |
| SDK publication | TypeScript/Temporal packages remain private experimental source packages. A local offline npm pack/install test is not package publication. |
| Automation | The read-only GitHub releases and Actions-workflows endpoints returned empty lists at this assessment, and `git ls-remote --tags` advertised no tags. Candidate workflows exist; main has no `.github/workflows/ci.yml`. Candidate contribution docs state Actions is disabled. |
| Performance | Workload-specific benchmark evidence exists. Compact storage does not establish faster queries or network delivery. Native, Trace/Nine and hosted paths are different implementations; do not interchange their measurements. |

The source snapshot and review boundaries are described in
[MAIN_NATIVE_FOUNDATION](MAIN_NATIVE_FOUNDATION.md),
[MAIN_CLIENT_SDK_COMPOSITION](MAIN_CLIENT_SDK_COMPOSITION.md),
[LOCAL_JOURNAL_QUICKSTART](LOCAL_JOURNAL_QUICKSTART.md),
[NATIVE_RECORDS](NATIVE_RECORDS.md) and [BENCH_HARNESS](BENCH_HARNESS.md).

## Next release gates

1. Review and land the intended native/client/dependency/CI composition so a new
   contributor builds the intended product from main. Explicitly retire or
   distinguish historical entry points and instructions.
2. Run the named runtime, CLI, SDK declaration and offline packaging gates on the
   exact release source in clean supported environments. Enable and observe the
   reviewed CI workflow through the repository owner; proposed YAML is not a
   successful remote run.
3. Freeze/version the supported disk and network formats; qualify interrupted-write recovery and current-format readers. Document
   supported platforms and unsupported workloads.
4. Produce reproducible, reviewed binary/package artifacts with the required
   notices and an explicit release/security/update policy.
5. Qualify the installed client against its matching hosted API: capture, lost
   acknowledgments, restart, project search, exact originals, revoke/delete and
   files. Cloud restore, availability, security review and capacity remain
   separate hosted-service gates.

The [capture transport](CAPTURE_TRANSPORT.md) change is a bounded client/protocol
improvement. Its HTTPS fixture results do not close these release gates.
