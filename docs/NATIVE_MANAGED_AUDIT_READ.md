# Current retained control reads

These methods do not open/create managed data, issue permanent destruction proof,
or expose the encrypted control carrier/plain state. They are a bounded capability
for the independently configured operator CLI, including after logical denial.

## Status

`RetainedControl::status()` rechecks the retained witness before/after returning
`ControlStatus`. Origin, keyed identity, logical Status, generation, retained snapshot
record/byte counts, cleanup-completed evidence and the exact current floor come from
the locally verified retained control owner. Counts after denial describe retained
evidence, never a claim that denied data remains live. Status is an authenticated local
authority report; its fields are not a new independently signed status wire.

## Export

`export_audit(expected_current, start, record::Limits)` returns `ControlAuditPage`.
`start` is the zero-based retained control ordinal, including checkpoint-only transitions,
not a record ID or a visible-event count. Each page has at most 1000 entries and at most
16MiB of exact encoded JSON; invalid/ahead positions and limits refuse before return.
The candidate buffer is at most one bounded page plus one bounded carrier, not lifelong
history. Appending/denial changes the exact current floor and refuses old continuations.

Each `ControlAuditEntry` contains only optional content-free AuditEvent, ReceiptDetails,
opaque ciphertext digest and signed checkpoint. `verify(verifier, expected_identity)`
pins the configured authority/domain/key/scope and checks the public-head commitment
for both event-bearing and event=None transitions. The latter require default details.
No signing key, ciphertext body, record content, parser context or private content hash
is exported.

The CLI must stream pages with one independently trusted current floor, verify every
signature/head, sequence and previous digest across page boundaries, and end at that
EXACT trusted floor. A signed bundle's own floor is not a freshness authority. Numeric
`next` is usable only with unchanged mandatory `expected_current`; callers may retry
a page, but must not skip, double-publish, splice different tips or treat partial output
as a verified complete chain. A configured origin/identity belongs to the same retained
owner; changing trust roots requires an explicit new namespace.

## Evidence

Three focused real managed/control tests and strict package lib+tests lint pass.
They cover denied status/cleanup versus retained counts, checkpoint-only and lifecycle
signature/head verification, streaming contiguous terminal witness, public metadata
mutation, wrong scope, exact encoded byte boundaries, invalid/ahead/oversized pages,
changed-tip continuation refusal and missing/corrupt current witness. This is local
synthetic filesystem evidence. Production CLI verification and its whole composition
are owned separately; no permanent backup-key destruction follows from these reads.
