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

## Qualified gates

Tested source: `5e150c97308dd38d1aa169ff8269942cea97947b`. Later qualification
commits add receipts/docs only. All database implementation files and protected
native licensing/publication/packaging files remain identical to foundation75.

| Gate | Passed | Failed | Existing ignored | Total command time |
| --- | ---: | ---: | ---: | ---: |
| Shared storage and legacy engine | 141 | 0 | 1 | 235.457 s |
| Serial native engine/rank | 175 | 0 | 10 | 41.766 s |
| Frozen legacy trace reopen/append/merge | 4 | 0 | 1 | 9.009 s |
| Dependency policy refusal cases | 7 | 0 | 0 | 0.164 s |
| Native packaging/license safeguards | 5 | 0 | 0 | 0.166 s |

The current minimal graph policy check also passes. Explicit `bench-tools`
compilation checks both store-bench and frame_probe (2.619s). The selected metadata
confirms baselines/codecs imply JSON reporting. The exhaustive compressed-frame
cut-recovery case passed; ignored cases, including intentional subprocess helpers
and measurements, are recorded individually. No new ignored case or failed gate
is hidden. The earlier34-entry metadata inventory gap and its correction remain
recorded without changing any graph package-set hash.

## One matched fresh-artifact-cache build pair

Both arms use actual task-local Rust/Cargo1.99.1 (rustc58cf8f9c3, LLVM23.1.1),
release library profile, explicit x86_64-unknown-linux-gnu target,
`RUSTFLAGS=-C target-cpu=x86-64`, jobs2, and identical Zig0.15.2 baseline C/C++
wrappers. Each Cargo target **and native Zig artifact cache** starts empty.
The existing wrapper hardcodes a shared Zig cache, so the measurement copies the
same wrapper with only its cache location parameterized per arm. Registry source/
index data and OS page cache remain warm; this is not a cold-machine benchmark.
The functional test target is explicitly warm and separate from this pair.

| Build | Elapsed | GNUtime maximum single-process RSS | Compiled packages |
| --- | ---: | ---: | ---: |
| Before foundation75 | 30.075 s | 540,588 KiB | 53 (50 external +3workspace) |
| After composition | 29.975 s | 538,756 KiB | 52 (49 external +3workspace) |

This one ordered pair is essentially unchanged. It supports the removed build
input (`twox-hash`), not a percentage speed/RAM claim. GNUtime RSS is not aggregate
process-tree memory or runtime database RAM. The engine rlib sizes are11,515,260
and11,515,144 bytes; compiler artifacts differ by path/metadata and are not a
released executable-size result. The freshly built ring main/test-support and
Zstandard native archives are byte-identical across arms. No BLAKE3 native archive
is produced under pure selection.

The exact executed native test binary is frozen locally with SHA-256
`aa9e21ff53749e9ede270e8579377409b8549ae8c106f27ab0f909f74f79026c`.
It dynamically links libc plus the ELF loader; static native algorithms are
accounted separately. The after engine library SHA-256 is
`2787224fbfb3f9b379c44d0dd973aafa17ef441dda71f058d243cf1eeaba1b3f`.
These local artifacts are not a package/binary publication.

[Exact commands, compiler/cache provenance, ignored cases and artifacts](main-native-qualification.json),
[raw gate receipt hash manifest](evidence-main-native/manifest.json), and
[full selected graph/comparator receipt](main-native-composition.json) retain
all evidence. During preparation a global rustup query unexpectedly installed
missing1.99.1; it completed before qualification and did not change the default
stable selection. Both actual arms use the existing task-local compiler and
record its matching binary hash. No further installation/deletion was attempted.

The short owned real /var/tmp synthetic fixture directory was removed after all
gates. No merge, release, package publication or deployment is authorized by
this proposal. Runtime query/append/RAM behavior, all historical native formats,
cross-platform compilation and hosted services remain separate qualifications.
