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

## Build and test

You need a recent stable Rust toolchain (the workspace uses edition 2024) and a C compiler (the storage path
builds zstd and SQLite). Linux is the first-class platform.

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

The benchmark harnesses (`oojuro-bench`, `oojuro-tracebench`) build many competitor engines and fetch public
datasets; see `docs/BENCH_HARNESS.md` and `docs/TRACEBENCH.md`. They are not needed to work on the engine.

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
