# Confirm publication after an uncertain creation

A database creation whose final parent-directory sync fails may leave a complete
valid file visible under its final name. Reopening that file proves its bytes,
but a later successful file-only append can still depend on an unconfirmed
directory entry. The regression reproduces this on baseline
`94202dba0f7ae3ab44aac6f12416f80f2dfc2bd4`.

Writable opens now retain the actual file's parent-directory descriptor until
their first durable commit, explicit confirmation or tail-recovery barrier.
Opening through an existing symlink resolves that actual parent and verifies its
named dev/ino matches the already opened file. No caller path is renamed.
Read-only opens retain their existing requirements and avoid parent access.

The first acknowledgement confirms both file bytes and parent publication.
Either failure refuses success and poisons the handle; failed recovery returns
no handle. Success releases the directory descriptor. Subsequent healthy writes
and retries add no directory sync. Writable opens need access to the containing
directory. Callers still own publication of directory trees or aliases they
create outside this file API. The generic `recover` tool explicitly confirms
even a clean visible prefix before reporting success.

The existing file-prefix fault gate remains intact. The extended Linux runner
matches only an owned file or directory inode and otherwise invokes the kernel's
actual `SYS_fsync`. A private flag injects `EIO`. Run:

```sh
python3 scripts/verify-ojo-sync-retry.py
python3 scripts/verify-ojo-sync-retry.py --features archive
```

Each build executes six cases: file failure in raw/encrypted legacy `.ojo`
fixtures, plus directory failure in raw/encrypted canonical `.juro` fixtures
through direct and existing symlink paths. The file cases retain `FFFSSS`.
Directory cases produce `SFFFFS`:

1. Initial temporary-name publication sync succeeds.
2. Final-name publication sync fails after the complete profile is written.
3. Cleanup-directory sync also fails; the destination remains visible.
4. A reopened new append fails its directory barrier instead of acknowledging.
5. Reopened exact retry also refuses while that failure remains.
6. Removing the failure allows confirmation, then the original cursor/time is
   returned without a duplicate or byte rewrite. Later healthy operations add
   no directory sync, and explicit original/alias paths stay intact.

The old source fails at the new-ACK assertion. All twelve explicit fault cases
pass on the fixed source. Normal default tests pass 7 unit, 17 ACID, 2 wire,
1 Event-vector and 1 doc test; archive passes 9 unit, 19 ACID, the same 2+1
compatibility tests and 1 doc test. The two C fault helpers are deliberately
ignored in normal Cargo runs and executed explicitly by the runner. Eight CLI
tests and strict Clippy all targets pass. Wire bytes/versions, magic, keys,
profile markers and frozen vectors are unchanged.

The runner removes its owned shared libraries, flags, aliases and datasets only
after the actual test child terminates. This tests syscall-failure handling,
not physical device power loss or arbitrary ancestor renaming. Source changes
are separate from immutable package publication and live service activation.
