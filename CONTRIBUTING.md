# Contributing to Oojuro

Thank you for helping. This project is licensed under the **Apache License 2.0** (see `LICENSE` and
`NOTICE`). Contributions are accepted under the same license: what you contribute is licensed to the project
and everyone else exactly as the rest of the code is (inbound = outbound).

There is **no CLA** to sign. Instead, every commit carries a Developer Certificate of Origin sign-off.

## Sign your commits (DCO)

Add a `Signed-off-by` line to each commit with `git commit -s`:

```text
Signed-off-by: Your Name <you@example.com>
```

The name and email must be yours and match the commit author. By signing off you certify the Developer
Certificate of Origin, version 1.1:

```text
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

Forgot to sign? Amend the last commit with `git commit --amend -s`, or sign a series with
`git rebase --signoff main`.

## Branches and pull requests

Create a topic branch from current `main`; keep unrelated changes in separate branches. An isolated worktree is useful when another change is in progress:

```bash
git fetch origin main
git worktree add -b your-topic ../oojuro-your-topic origin/main
```

Open a draft pull request while the change is being developed. Include what changed, the exact tests run and any remaining limits. Use `git commit -s` for each commit. Maintainers may review their own work; a draft PR does not authorize a main-branch merge, binary/package release or deployment.

## Build and test

You need stable Rust (edition 2024) and a C compiler for the standard zstd codec. Linux is the first-class platform. CI pins Rust 1.99.1 on disposable GitHub-hosted Ubuntu 24.04 runners.

Plain `cargo build --locked` and `cargo test --locked` select only `oojuro-engine`, `oojuro-rank` and `oojuro-store`. CLI/runtime applications still require an explicit `-p` selection. The dependency policy gate checks these libraries independently, including conditional platform dependencies; see [DEPENDENCIES.md](DEPENDENCIES.md). Store JSON-reporting tools require `--features bench-tools`; `baselines` and `codecs` enable it automatically. No public library API is removed.

The normal checks select runtime packages explicitly, without benchmark comparison features:

```bash
cargo test --locked --no-default-features --lib --tests -p oojuro-core -p oojuro-store -p oojuro-nine
cargo test --locked --no-default-features --lib --tests -p oojuro-engine -p oojuro-rank -- --test-threads=1
cargo test --locked --no-default-features --lib --tests -p oojuro-trace -p oojuro-capture -p oojuro-proxy -p oojuro
```

The native command runs authority, retrieval, allocation accounting and rank conformance tests serially, including subprocess crash/lock fixtures. The third command includes real legacy and native CLI, daemon, mock proxy and stdio MCP integration tests. They use synthetic data and local test servers. Give fixtures a short private real-directory `TMPDIR` with enough disk space and no symlink ancestors (the hosted credential guard rejects them); the daemon rejects socket paths longer than 103 bytes. CI creates private scratch under `runner.temp`. CI explicitly repeats `cargo test --locked --no-default-features -p oojuro-trace --test compat` as a named historical-format gate. It opens the frozen `df8fa85` fixture, compares its original answers, then appends and merges old/new segments. The shared storage suite includes encrypted/plaintext transitions, strict sealing, corruption and torn-tail recovery. These checks do not establish compatibility for every historical native binary.

This native-lineage proposal inherits older experimental `sdk/typescript` and `sdk/temporal` trees. These Rust jobs do not test, typecheck or package those SDKs. Later CLI/session, MCP read, attachment and session-SDK topic branches are outside this proposal; see [the exact source boundary](docs/MAIN_NATIVE_FOUNDATION.md). Add checks for a new runtime crate or SDK in the same PR that introduces it; this explicit package list does not discover new packages automatically.

The benchmark harnesses (`oojuro-bench`, `oojuro-tracebench`) build competitor engines and fetch public datasets. `oojuro-store`'s `baselines` and `codecs` features are also comparison tools. They are not part of normal CI. Run them deliberately for the change being measured; see `docs/BENCH_HARNESS.md` and `docs/TRACEBENCH.md`. Do not use `--workspace` or `--all-features` as a substitute for the runtime checks above.

GitHub Actions is currently disabled at repository level. This PR proposes the workflow; an owner must review and enable Actions before remote check results exist. The workflow does not configure branch protection, require another reviewer, publish packages or deploy services. Keep trusted signing and release credentials outside PR jobs; see [publication boundaries](docs/PUBLICATION.md).

## What a good change looks like

- **Small and focused.** One idea per pull request, with tests. Bug fixes come with a test that fails without
  the fix.
- **Simple and horizontal.** The record model stays flat (see `docs/DESIGN.md`). Analysis features live behind
  a boundary that can be deleted in one commit.
- **Measured.** A change that touches the write path, the index or search should include before and after
  numbers from the benchmark that covers it (`docs/TRACEBENCH.md`, `docs/PERF_SEARCH.md`, `docs/PERF_STORE.md`).
  A feature that slows ingest or search is not accepted.
- **Lossless and provable.** Stored text must read back byte for byte, and erased text must be absent from
  every file. Do not weaken those checks.
- **No real data.** Never commit transcripts, logs or datasets, and never commit anything from a real person's
  agent history. Tests use synthetic text. Benchmarks use public datasets that are fetched on demand.
- **No secrets.** Real tokens, keys and credentials never go into the repository. Tests use invented values
  or build them at run time (see `crates/oojuro-capture/src/redact.rs` for how).

## Reporting bugs and security issues

Open a GitHub issue for bugs. For a security problem, please use GitHub's private vulnerability reporting
("Security" tab of the repository) instead of a public issue.

## Trademarks

"Oojuro" and its logos are trademarks of oojuro, Inc. The license covers the code, not the name (see the
README).
