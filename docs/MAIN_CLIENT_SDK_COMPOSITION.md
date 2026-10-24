> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/journal/blob/main/docs/MAIN_CLIENT_SDK_COMPOSITION.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Main-based client and session SDK composition

## Source boundary

This topic follows the main-native foundation proposal (public PR75, stacked on
CI73). It composes source for review; it does not merge main, release packages,
activate a hosted feature or change private hosted Cargo pins.

| Boundary | Exact source |
| --- | --- |
| Qualified main-native foundation | `5e4b257b72adb8a1c98bbba1229d6573a82ce93d` (runtime tested at `5fbf731`) |
| Reviewed session SDK74 head | `99818c3e2c8125d3f8243b36ba880abf31c5004e` |
| Session SDK runtime within that head | `e42553d885f76552928e447ce4999efcb98d982b` |
| Local two-parent merge | `1639e11c8ac8bee5b872a6b5cf2f420213a5ce3c` |

The automatic merge had no conflicts. Relative to the foundation, the inherited
runtime diff adds capture API-key setup, per-session routing/spool, CLI installer,
project-read CLI/MCP, attachment helpers and SDK tests/docs across 40 paths.
Native engine/rank/store sources remain unchanged from accounting `d72f879`.
Main publication/license files and packaging scripts are preserved. The one-line
Cargo.lock overlap is an additional CLI dependency edge to **already locked**
`base64 0.22.1`; it adds no new locked package. CLI serde_json additionally enables
its existing `raw_value` feature for strict wire parsing.

SDK74 includes CLI70, MCP72 and the attachment change already incorporated at
`4833a052c341645639622ec910367d66b4b0942f`. `git cherry` marks attachment69 runtime
`df110aa885bbe6a53728225173df0fe29660df80` patch-equivalent to an ancestor of SDK74.
It is not cherry-picked a second time. Dependency66, allocation-trim65, mask68 and
other later performance experiments remain outside this composition.

## Reproducible checks added with the source

The existing pinned Rust1.99.1 native/shared-storage/runtime/legacy-format commands
remain. Runtime also runs both standard-library Python installer test scripts
against synthetic binaries/settings, without changing actual agent configuration.

A separate disposable Ubuntu Node job pins Node **24.20.0** and
`actions/setup-node` **v7.1.0** commit
`949feb2413d6458794dcd2491c4babbbce0c15c1`. Its action manifest was inspected from
the official repository; automatic package-manager caching is explicitly disabled.
Checkout remains pinned/read-only, and the job has no registry-auth setup, production
secrets, artifact upload, signing, publishing or deployment step.

The SDK package has no runtime or development dependencies and no committed npm
lock. To make declaration checking reproducible without changing that package,
`scripts/ci-sdk` has one development-only dependency, TypeScript **5.9.3**, and its
npm v3 lock with exact registry URL and integrity. `npm ci --ignore-scripts` installs
only that tool. No Temporal peer packages or new application dependencies are added.

The Node job runs every `sdk/typescript/test/*.test.js`, checks every adjacent
`test/*.ts` with strict NodeNext/ES2023 TypeScript options, then runs
`scripts/test-sdk-package.py`. The package gate creates a local tarball, verifies
all five runtime/declaration entry points and shared file modules, installs that
tarball offline with scripts disabled, imports all exported surfaces from the
installed package, and asserts the manifest remains private with no runtime
dependencies. It makes no service call and publishes nothing. Temporary package
artifacts are removed. This does not test the inherited Temporal peer integration.

## Qualification status

The exact selected CI commands passed on source `b6c029917d2e43e23506ca5c136941d30a6c33cd`.
Local Linux used pinned Rust1.99.1 and the portable Zig C wrapper, Node24.20.0,
npm11.19.0 and locked TypeScript5.9.3. This is not a GitHub-hosted VM result.

| Gate | Passed | Failed | Existing ignored | Test binaries/files |
| --- | ---: | ---: | ---: | ---: |
| Legacy engine/shared storage | 141 | 0 | 1 | 8 binaries |
| Native engine/rank | 175 | 0 | 10 | 3 binaries |
| Capture/proxy/CLI/MCP | 230 | 0 | 3 | 29 binaries |
| Explicit legacy compatibility repeat | 4 | 0 | 1 | 1 binary |
| Agent/project installer scripts | 2 + 3 | 0 | 0 | 2 scripts |
| Node client/session/attachment runtime | 111 | 0 | 0 | 9 files |
| Strict TypeScript declaration smoke | passed | 0 | 0 | 8 files |
| Local offline package | passed | 0 | 0 | 5 export surfaces |

The complete Rust suites contain **546 distinct passes and 14 existing ignored
entries**. The explicit legacy-format step repeats four passes and one ignored
fixture generator already counted in runtime. Node reports no skipped/cancelled
tests. The Node development install and package gate both passed; there was no
publication or live service call. The selected Linux normal/build/dev Rust graph
has 168 unique formatted package entries and no selected comparison databases or
optional experimental store codecs. Workflow actionlint and new-source whitespace
checks pass.

All gate processes exited zero. The local orchestration script's final `rmdir`
then refused the nonempty owned scratch directory because existing fixtures and
Node's compile cache retained files. Only that owned real private directory was
inspected and removed. The cleanup receipt and every gate log are retained locally;
no source guard/test was changed or repeated for cleanup. The
[machine-readable receipt](MAIN_CLIENT_SDK_QUALIFICATION.json) records commands,
counts and evidence hashes.

This qualification covers local/mock/offline source composition. The earlier
SDK74 actual hosted fixture qualifies its pinned earlier SDK/source, not a new
joined test of this main-based composition. Real owner instrumentation/history,
Temporal peer integration, hosted/AWS recovery, performance and package release
remain separate gates. Actions remains disabled pending owner review; no remote
CI result, main merge, hosted pin change, release or deployment is claimed.

Actual local compiler selection is separately verified: the task-local
`RUSTUP_HOME` and `cargo +1.99.1` commands selected rustc 1.99.1 commit
58cf8f9c35b355199727627d07ae34a4e2846d67. The target's existing `.rustc_info.json`
records that compiler; `RUSTC`, wrapper and toolchain override variables were
unset. The receipt includes resolved paths, verbose command provenance and the
raw proof hash. This docs-only proof collection reran no gate and is distinct
from performance fixtures using system stable 1.99.0.
