# Main-based native engine foundation proposal

## Exact source boundary

This is a local topic composition for review, not a main merge, release or deployed build.
The automatic merge required no conflict resolution. Its two parents are:

| Boundary | Commit |
| --- | --- |
| Public main publication/license baseline | `abbfc71465704fce546eb86c0af66ef301585c15` |
| Main-based CI proposal, first merge parent | `4493dc5ea3f83a3838074eae9c0945c314ec78f0` |
| Exact accounting runtime, second merge parent | `d72f879484655c758c474b43b7c38dc81b902c0d` |
| Local composition merge | `0efe987ae12fcc86f3dae490380e36bf993434c5` |

The runtime parent contains the deployed compact engine lineage through
`5197b8ca4dacecd1563b14c24e036eaf7cda2921`, followed by the allocation accounting API.
Accounting is a candidate addition; its presence does not mean it has been deployed.
At the merge, `Cargo.toml`, `Cargo.lock`, `crates/` and `sdk/` are byte-identical to
the exact runtime parent. Main publication/license files, packaging scripts and CI
are byte-identical to the first parent before this proposal's CI/documentation followup.
Frozen benchmark evidence remains unchanged, including three inherited trailing-blank-line
warnings from `git diff --check` across the complete merge.

No newer dependency-reduction PR66, allocation-trim PR65, mask PR68, integration CLI
PR70, MCP project-read PR72, attachment PR69 or continuing session-SDK topic changes
are merged here. These are separate candidates, not prerequisites implied by this merge.

The exact runtime ancestry already contains older client surfaces: `oojuro-native`,
`crates/oojuro/src/hosted.rs`, `sdk/typescript` and `sdk/temporal`. The two SDK packages
are private `0.0.0-experimental` packages requiring Node >=24. The TypeScript tree has
namespace, capture, files and file-deletion entry points; Temporal has workflow, codec
and recall entry points plus Temporal peer dependencies. Preserving these inherited
files is not qualification of their JavaScript, declarations or package payloads.
The later client/SDK topics must be composed separately and tested explicitly.

## Explicit CI graph

All commands use the locked graph, no default features and Rust 1.99.1. These Linux
jobs use no provider credentials or running comparison databases.

| Job | Packages and coverage |
| --- | --- |
| Legacy engine and shared storage | `oojuro-core`, `oojuro-store`, `oojuro-nine`: library and integration tests, including torn-tail recovery, deterministic bytes, compaction, deduplication, encrypted reopen/authentication and strict-sealing transitions |
| Native engine and ranking | `oojuro-engine`, `oojuro-rank`: library and integration tests, serial execution for native subprocess/lock fixtures; raw/parsed authority, lexical/projection memory accounting, scoped records, receipts, audit and rank conformance |
| Legacy and native capture, CLI and MCP | `oojuro-trace`, `oojuro-capture`, `oojuro-proxy`, `oojuro`: library and integration tests including inherited native CLI/record CLI tests, local adapters, hosted mocks, legacy daemon and MCP |

The runtime job additionally names `oojuro-trace --test compat` as a separate gate.
This intentionally repeats that small test target after the complete runtime suite,
so historical format acceptance is visible without an incomplete integration-target
allowlist. Its committed synthetic fixture was written by `df8fa85`: tests compare
original answers, append records, merge version-2/version-3 segments and compare
against a fresh store. The ignored fixture generator stays ignored. This does not
claim cross-version compatibility for every historical native binary or cloud format.
Native ignored subprocess helpers are invoked by their parent tests where applicable;
benchmark/measurement tests are not force-enabled with `--ignored`.

Source-only `cargo tree --locked --offline --no-default-features --edges normal,build,dev`
for these nine selected packages yields 168 unique formatted package entries on the
local Linux host. This is a source graph inspection, not a test pass, all-platform
inventory or count of minimal-engine production dependencies. It includes the
CLI/proxy and test graph. No rusqlite/libsqlite3-sys, Tantivy, Postgres, RocksDB,
DuckDB or optional experimental store codecs are selected. `brotli-decompressor`
is a normal proxy dependency for response decoding; it is not the optional store
Brotli comparison codec. Standard zstd (including native zstd-sys), LZ4, established
cryptography and existing runtime dependencies are preserved.

`oojuro-bench`, `oojuro-tracebench`, store `baselines` and store `codecs` are excluded.
The lockfile still records optional/workspace dependencies; their presence in the
lockfile is not evidence that the CI jobs build them. No `--workspace` or
`--all-features` job is introduced. No hosted Cargo pins change.

## Qualification status

The exact selected commands passed locally on frozen source
`5fbf73150036625136f9f2b89281491c031d2af7` (see the machine-readable receipt for the verified full SHA),
using task-local Rust 1.99.1 and the portable Zig C wrapper on local Linux. This is
not a GitHub-hosted Ubuntu VM result and establishes no remote status check.

| Gate | Passed | Failed | Existing ignored | Test binaries |
| --- | ---: | ---: | ---: | ---: |
| Legacy engine and shared storage | 141 | 0 | 1 | 8 |
| Native engine and rank | 175 | 0 | 10 | 3 |
| Capture, proxy, CLI and MCP | 215 | 0 | 3 | 29 |
| Explicit legacy-format repeat | 4 | 0 | 1 | 1 |

There are 531 distinct passing tests and 14 ignored entries in the three complete
suites. The explicit legacy-format gate repeats four passes and one ignored fixture
generator already counted in runtime: do not add that repeat to unique coverage.
The native ignored entries include subprocess-only helpers invoked by parent tests
and two opt-in measurements; the shared-storage ignored entry is a local scan
measurement. Runtime leaves its proxy/column speed measurements and fixture writer
ignored. No ignored measurement was enabled. SDK JavaScript/type/package checks and
later cloud composition require their own gates; CI73's earlier main-only results
are separate evidence.

The first runtime attempt used CI73's local short-path symlink workaround and
failed the inherited hosted private-file checks: those correctly reject symlink
ancestors. Mock HTTP accept threads then waited, so only that owned test process
was terminated. A focused reproduction returned `private_hosted_file_required`.
The complete runtime and explicit compatibility commands then passed with a short,
owned mode-0700 real directory under disk-backed `/var/tmp`, which was removed
when the tests finished. Storage/native had passed using the earlier scratch;
those paths did not exercise private credential loading. The actual workflow uses
a private real directory under `runner.temp`; no guard or test was weakened.
Diagnostic logs remain in the local qualification workspace. See
[the qualification receipt](MAIN_NATIVE_FOUNDATION_QUALIFICATION.json) for hashes.

Repository Actions remains disabled pending owner review. No admin setting, branch
rule, token permission, required-review count, package/release job or deployment is
changed. Public source topic branches/draft PRs are review artifacts; merging main,
publishing binaries/packages and deploying still require the owner's approval.
