# Main/native dependency composition

## Source and scope

Base: public native foundation75 `ba144d90d396b680122f1e93c808c724844ef2c8`
(runtime qualified at5fbf731). Dependency draft66 shares ancestorf01345f.
Runtime reductions adapt its two reviewed manifest changes; no engine/library
implementation file is replaced. The only cherry-pick conflict was README prose,
resolved by preserving current CI/publication text and adding the explicit tool
feature. Current NOTICE, licenses, SECURITY.md, publication/native packaging
scripts and existing CI tests stay intact. No optional performance branch65/68
is composed. Every composition commit is DCO signed.

## Exact selected graphs

Locked Cargo graph, target `x86_64-unknown-linux-gnu`, separate selection per row.
Counts below are **external distinct package/version/source identities** over
normal **and build** edges, with dev edges excluded. Local workspace packages
are recorded separately in the machine-readable comparisons. Normal-only and
dev-inclusive closures are also captured: build-only is the union minus the
normal closure, and added test/dev packages are dev-inclusive minus production.
These are closure differences, not a claim that a package serves only one role.

| Selection | Before | After |
| --- | ---: | ---: |
| `-p oojuro-engine` library | 50 | 49 |
| `-p oojuro-store` library, no tools | 40 | 34 |
| `-p oojuro-rank` library | 0 | 0 |
| `-p oojuro-core` legacy library | 22 | 21 |
| `-p oojuro-nine` legacy library | 25 | 24 |
| `-p oojuro-trace` legacy library | 49 | 48 |
| `-p oojuro-capture` | 57 | 56 |
| `-p oojuro-proxy` | 95 | 94 |
| `-p oojuro` CLI/package, including native command | 152 | 151 |
| `-p oojuro-bench` explicit benchmark | 154 | 153 |
| `-p oojuro-tracebench` explicit benchmark | 224 | 224 |
| Store `--features baselines,codecs` | 218 | 217 |
| Engine, all target predicates (not cross-compilation) | 63 | 62 |

A plain workspace selection changes302→49 because `default-members` now chooses
engine/rank/store. That is a change of selected packages; the isolated engine
closure changes50→49. The current foundation CLI is152→151 external identities;
a number from another package union or newer client proposal is not comparable.
No release binary size, linkage, runtime latency, RSS or production cost gain is
claimed from these graph counts.

LZ4 remains exact locked0.11.6, from the established `lz4_flex::block` API.
Before features: `checked-decode,default,frame,safe-decode,safe-encode,std`.
After: `checked-decode,safe-decode,safe-encode,std`. Only the unused frame wrapper
and its twox-hash edge disappear. All production LZ4 call sites use block APIs;
block algorithms/format and safe checked flags remain. Existing twox-hash stays
in the workspace lock for explicit other tools. Zstandard's existing
`experimental,zdict_builder` API features remain required for magicless frames
and dictionary training; they are not experimental comparison codecs.

Store removes its unconditional serde_json edge. It only uses JSON in
`store-bench`, `frame_probe`, `codecs`, `baselines` and `dedupe-exp` reporting;
its public library functions/types and file formats do not change. Explicit
`bench-tools` restores reporting, and the two existing comparison features imply
it. CLI and engine still retain their own required JSON implementations.

## Reproduction

```sh
CARGO_NET_OFFLINE=true python3 scripts/dependency_audit.py --root /path/to/base75 --edge-breakdown --output work/before.json
CARGO_NET_OFFLINE=true python3 scripts/dependency_audit.py --edge-breakdown --check --output work/after.json
python3 scripts/dependency_audit.py --engine-only --check --output work/minimal.json
```

The full inventory covers all336 external packages in the selected graph union, including
optional Store codecs and all-platform support; no unexplained omissions remain.
The initial default-feature/Linux metadata query omitted34 such entries and was
corrected without changing any graph package-set hash. `--engine-only` is an
explicit policy-only mode using `--no-deps`; it reports external inventory as
intentionally omitted. No actual selected dependency is Git-sourced, and a full
audit refuses unsupported inventory mapping rather than silently dropping it.

The current build/install documentation uses explicit CLI/package selections.
README and CONTRIBUTING also state that plain `cargo build`/`cargo test` select
the three default libraries; historical broad benchmark commands remain labeled
as broader experimental checks. There is no bare documented install command
that implies the CLI is a new default.

Machine-readable comparator inventories are archived in this directory. The
receipt records exact source/tree identities, commands, hashes, selected feature
roots and local/external closure differences. Cargo tree/metadata do not have an
optimization profile. An all-target graph is not evidence of cross-platform
compilation. Required-feature target declarations, normal/build/dev edge modes
and exact toolchain are explicit in the receipt.

## Targeted gates still pending

No functional tests or compilation have run for this new composition. Existing
qualification on another source is historical, not a pass for this tree.
After the shared CPU window is released, qualify this frozen source with:

1. Minimal policy gate; negative checks for provider/benchmark dependency,
   unexpected internal benchmark crate, frame re-enable, Store JSON leak and
   wrong default selection. Keep the same gate in the storage CI job with the
   pinned installed toolchain; repository Actions remains disabled.
2. Current shared storage/legacy engine suite and serial native engine/rank
   suite, plus exact frozen legacy trace compatibility fixture (reopen, compare
   original payload/answers, append and merge). Preserve ignored counts and any
   failures. Exercise both safe LZ4 blocks and encrypted/plaintext/sealed/torn-tail
   paths through established tests; no replacement codec implementation.
3. Explicit `bench-tools` build/check of store-bench and frame_probe. Verify
   baselines/codecs auto-enable JSON in metadata; no unrelated competitor or
   experimental-codec full build is required by this manifest-only change.
4. Existing native packaging/license tests: selected-graph archive/notices,
   locked checksum verification and release cache preparation must stay valid.
5. One bounded matched cold standalone engine-library build before/after using
   separate targets, same actual compiler/profile/target/CC/flags, recording
   elapsed time, peak process RSS, artifacts and linked/native archives. Treat
   this as build evidence; do not infer query/append performance. Stop optional
   measurement once the remaining build/format risks are resolved.

No merge, release, package publication or deployment is authorized by this
proposal. The public API remains available; the intentional tool feature and
plain-build selection changes are the user-visible compatibility notes.
