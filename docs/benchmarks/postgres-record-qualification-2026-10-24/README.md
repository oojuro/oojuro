# Optional PostgreSQL Record arm: functional qualification

Measured 2026-10-24. **Functional evidence only; no comparative latency, RAM, storage-efficiency or pricing result.** This docs-only topic is isolated from the frozen 9e5 source worktree. It does not select a production engine or add PostgreSQL to minimal engine builds.

## What passed

The optional PostgreSQL arm and shared whole-cgroup benchmark lifecycle passed five focused checks, then the actual frozen CLI's seven standard correctness gates and two closed-cluster copy reopens:

- Bounded request limits and checked full file/directory inventory.
- Exact originals (NUL, empty, Unicode, CR/LF, whitespace, escaped JSON, long text), two-clock winner/visibility rules, full Doc/transaction atomicity and reopen.
- A preexisting partial cluster refuses reinitialization.
- Whole-cgroup termination removes a real different-session descendant; leader-only signals are insufficient for PostgreSQL.
- The actual CLI's seven gates passed: byte_exact, byte_exact_after_reopen, get_range_order, as_of_visibility, freshness, determinism, crash_recovery.
- Three real whole-PG cgroup SIGKILL trials at fixed 0/5/30 ms preserved all 600 acknowledged full Docs. All three unacknowledged 2,000-row batches recovered 0 rows; the new predicate requires PostgreSQL 0-or-all and independently checks cardinalities, full generated originals, no phantom/duplicate/truncated rows, earlier recovered originals and get/range consistency. No favorable kill-delay rerun was used.
- The CLOSED exact 12-Doc cluster and final CLOSED 600-Doc crash cluster were copied. Only copies were opened. Every Doc field and every text lookup matched mechanically reconstructed synthetic inputs; a missing-ID probe remained missing. Copy bytes/modes matched source before opening, and complete source inventories were unchanged afterward. This proves these local copies, not an off-host backup/restore protocol or cold-OS latency.

Actual copy Stats read PostgreSQL 17.11, fsync=on, synchronous_commit=on, full_page_writes=on, and the postmaster in its arm cgroup. Fixed settings include 32 MB shared_buffers,4 MB work_mem,32 MB maintenance_work_mem,10 connections,256 MB max_wal_size and autovacuum on. The complete same-image runtime preserves default JIT settings; execution of a JIT plan is not asserted.

## Process ownership and bounds

The fixed standard schedule opened 12 scopes; the two copies opened 2 more. All 14 launch records had finite 3 GiB/0swap/512-task/600 s scopes and 5 s stop bounds. The outer service had 6 GiB/0swap/512 tasks/1200 s/CORE0/KillModecontrol-group. The outer memory limit is not an aggregate cap over sibling scopes.

The outer controller was a verified child subreaper. It reaped only actual direct adopted children, excluding its live owned Popen leader. Before each arm exec, the strict launcher required the exact fixed crash-store's prior postmaster PID to disappear from /proc. Exactly three non-null PID barriers occurred at the three crash recoveries. There was no guessed PID signal, pidfile deletion, weakened PostgreSQL startup check or sleep used as proof. PostgreSQL's own lock-file PID liveness check remains unchanged.

The measured functional controller completed in 11.546 s. That includes setup/checks/copies/cleanup and is **not a database benchmark**. Independent observation verified all 14 scopes, the outer cgroup, 92 observed process identities and original parent identities absent. Stores and bounded logs remain deliberately retained for review; retention is distinct from live-process cleanup.

## Exact source/build/runtime provenance

- PostgreSQL benchmark arm source 16f42b82c9bac9f057efef5de8691059c901e263, based on public84.
- Test evidence cleanup 580f9c8016710e9d82e89ff675ef0e7fc84f32e1; frozen libtest SHA bf7db0e5d0e9aa3735d70d9521c6fbab66c70d629e0f77a858e368477584096c.
- Shared crash-oracle source 9e5d9214d4a017e8621197e757ec641421ca64c1 changes no database/engine/format code. Actual main SHA 8a5da7a6139ebc7b01975a057fc921b999720053af19e33bd85ea87374dae8ea;5,482,640B.
- Actual Rust/Cargo 1.99.1, portable x86-64, explicit canonical CC/CXX/linker, opt 3/non-test, fresh:false. Main built with no default features and only postgres-arm; no compiler or package install in the gate run.
- Official pinned image postgres@sha256:639ab7ceb90e13123085b741fb31ef493fba25463002f6da665352e7b534b652. Complete same-image PostgreSQL 17 binaries/modules/share runtime; runtime-v3 manifest original SHA 88acd85b9b3edba8ec5da5fc3b66f3371c947f211504fb3067f2a72214fcfa7c. Native library closure is recorded for all 4 selected executables and 79 modules, including English dictionary and JIT dependencies; host system libraries/timezone remain external. This is not a self-contained root filesystem.
- Final controller original SHA68e3a0132a3e17fff756681c3b5475f3c807dec648934ed22ad77f6464e2c6f6, strict launcher e135974ea955fd02831c5eb0ac9c8632fe56e06efe399562ccecc226535e88ad, dispatcher 189efb431caec4ddccdcd5a1b3ef0e983ff0f5a304e90bcf409f5505f35cd418, plan bd96547d09b858a76d26cf44d593e14935a0ce2da964afa87472011bad1ecabe.

## Retained failures and limits

The first focused attempt passed limits and inventory, then refused before cluster initialization because the extracted runtime lacked libpq.so.5. The second refused before initialization because a packaged configuration symlink's parent target was absent. Both receipts and failed-cluster evidence were retained. The complete same-image runtime was then assembled, including PostgreSQL share/bootstrap/dictionaries and LLVM/Z3 dependencies; the remaining three focused checks passed. The original two passing checks were not rerun. The later actual CLI compiled the stronger 9e5 crash predicate and qualified it separately.

Only synthetic local stores and trusted binaries were used. No network listener outside the owned Unix socket, real history, account credentials, AWS/provider request, hosted service, replication, encryption equivalence, larger-than-RAM behavior, merge, release or deployment was qualified. Native/SQLite/JSONL comparative results are separate campaigns. PostgreSQL's cluster floor and workload increment still require a measured matched experiment.

## Evidence archive

[manifest.json](manifest.json) records each original retained hash, its published path-redacted hash/size, and the deterministic compressed payload hash. The [qualification payload](qualification-payload.tar.gz) contains only27 flat regular metadata/source-snapshot files, not PostgreSQL data or executables. Decompression and every file hash were verified against the manifest; paths, modes, IDs and timestamps were checked before any publication.

Inside the archive, gates.json, qualification.json, independent-qualification.json and outside-cleanup.json retain the predicates. Separate focused/build/runtime receipts preserve their source boundaries and the two packaging failures. The complete source/copy file and directory inventories are metadata, not file contents. Controller snapshots end in.txt and have named host-path placeholders; they are inspection artifacts, not directly executable scripts. No binaries, native libraries, image layers, PGDATA, corpus, original-text output, private history or credentials are included. No tests were rerun to archive this evidence.

PostgreSQL17.11 lock-file behavior was checked against its [official source](https://github.com/postgres/postgres/blob/REL_17_11/src/backend/utils/init/miscinit.c#L1248-L1292).
