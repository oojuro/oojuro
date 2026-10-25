# Durable acknowledgement after replay

A complete transaction may remain readable after its `fsync` failed. Validating
that frame after reopening proves its bytes and grammar, but does not establish
that the failed barrier reached stable storage. Previously an exact retry could
return its original receipt immediately after validation, converting that
ambiguous outcome into a new acknowledgement without a successful barrier.

Each handle now tracks the transaction prefix covered by a successful sync.
Creation confirms the empty prefix; successful commits and writable incomplete-
tail recovery confirm their complete prefix. Ordinary replay starts unconfirmed.
An exact durable retry confirms the current prefix before returning its original
receipt. Sync failure poisons the handle. Repeats on an unchanged healthy handle
reuse its successful barrier and perform no extra sync or append.

`Database::confirm()` and `events::Journal::confirm()` expose this barrier for
applications that reconstruct accepted batch identities and acknowledge them
without calling the engine's commit method. They require a writable healthy
handle and return the current physical checkpoint, without generating a new
record, request ID, acceptance timestamp or cursor. Read-only verification is
not a durable acknowledgement. Applications that bypass commit for a duplicate
must call this API before acknowledging an unconfirmed replay.

Specification document release 0.1.0-draft.2 clarifies the same requirement.
Wire bytes, accepted transactions, retry identities, file IDs and frozen vectors
are unchanged. First confirmation after replay may add one file sync. Existing
performance results from older pinned revisions do not include that barrier.
The published v0.1.0 binary predates this fix; package promotion is a separate gate.

## Qualification

The opt-in Linux runner compiles a test-only C `fsync` interposer. It matches the
exact owned fixture inode, including a descriptor originally opened through the
Event creation temporary name. While a private failure flag exists it returns
real syscall error `EIO` for that database; every other sync uses the kernel's
actual `SYS_fsync`. The interposer is never linked into product binaries.

```sh
python3 scripts/verify-ojo-sync-retry.py
python3 scripts/verify-ojo-sync-retry.py --features archive
```

The baseline at `102bad6eb5311676de7f2e8433400271cac0944f` failed the meaningful
regression assertion: an unconfirmed replay acknowledged its exact duplicate
while the sync failure remained active. The fixed source passes plaintext and
encrypted cases under both raw and archive-enabled builds. Each trace is
`FFFSSS`:

1. The append writes a complete valid frame, then its sync fails and poisons.
2. Reopen verifies that frame; its exact retry's sync still fails and poisons.
3. Reopen and explicit application confirmation also fail and poison.
4. Removing the failure lets confirmation sync once without changing any bytes.
5. Another reopen and exact retry sync once, preserving the original cursor/time.
6. Writable torn-tail recovery syncs the recovered prefix once.

Read-only confirmation refuses. Freshly created and already confirmed handles,
healthy repeated retries and retries after synced recovery add no extra calls.
The original file bytes remain identical after accepted retry/confirmation.
Owned fault flags, shared libraries and synthetic datasets are removed by the
runner after the actual child process terminates. This exercises syscall failure
and acknowledgement behavior, not a physical device power-cut experiment.

Default gates pass 7 unit, 17 ACID, 2 wire compatibility, 1 independent Event
vector and 1 doc test. Archive gates pass 9 unit, 19 ACID, the same 2+1
compatibility tests and 1 doc test. The C fault case is intentionally ignored
by ordinary Cargo runs and explicitly executed in the four runner cases above;
normal Rust tests retain their compiler requirements. Strict Clippy all targets
with archive enabled passes. The format remains experimental.
