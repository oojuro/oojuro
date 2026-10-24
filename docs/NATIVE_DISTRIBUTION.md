> Application native build/packaging scripts now live in [Oojuro Journal](https://github.com/oojuro/journal/tree/main/scripts).
> Trusted release handoff belongs to [o-release-operator](https://github.com/oojuro/o-release-operator).
> Historical source/package receipts below remain evidence for their exact revisions.

# Native release licensing

Scope: the `oojuro` and `oojurod` Linux x86-64 front doors, default features, locked production
and build dependencies. Competitor benchmark engines and optional codec features are not this
release. An Apache license field alone is not a binary notice bundle.

## Build and package

Build a clean reviewed commit in an isolated Ubuntu 22.04 builder with Rust 1.99.0 and a C
compiler, under the shared verification lock on Rocket. The Ubuntu baseline preserves the
existing E2B glibc floor. Do not publish the builder image or copy its system libraries.

```sh
CARGO_BUILD_JOBS=2 bash scripts/build_native.sh /fresh/output
```

Use binaries from that exact build, never a PATH executable or an unrelated target directory.
The packager records source revision, lock hash, Rust version, target, dependency inventory,
embedded-dictionary presence and file/binary SHA-256. It cannot prove that arbitrary binaries
passed to it were built from the named source: keep the clean build log and source checkout
identity alongside the manifest, and verify binary hashes before promotion.

Packaging requires Python 3.11+ and the Cargo registry archives from the build. Their SHA-256
must match Cargo.lock; license files are extracted from those verified archives, not a mutable
registry source directory.

The build script explicitly runs `cargo fetch --locked`
before building: Cargo's metadata and tree commands resolve workspace packages before filtering,
including benchmark archives a selected production build may not download. A disconnected
packaging host must receive this prepared cache. Fetching those sources neither compiles nor
redistributes the benchmark engines; only the selected normal/build graph is packaged.
Packaging then uses offline Cargo calls and immutable checksum-verified archive bytes.
Validation remains active under `python -O`/`PYTHONOPTIMIZE`; blank registry notices are refused.

Ship the entire output, including LICENSE, NOTICE, `licenses/`, Cargo.lock and manifest. The
packager retains all legal files found in the selected crates, including nested notices for
ring/BoringSSL/fiat, zstd, Unicode data, stemmers and webpki root certificates. It also preserves
the Rust library copyright inventory and supplied license texts. Missing licenses/notices fail.
The [supplements](../licenses/README.md) repair the missing alloc-stdlib crate notice and preserve
embedded dictionary attribution. Do not substitute one generic Apache file for these materials.

The resolved production/build declarations are permissive (MIT, BSD, Apache, ISC, Unlicense,
CC0, Unicode and CDLA alternatives); conjunctive grants such as ring's ISC material still apply.
No GPL/AGPL runtime crate is in this selected graph. This does not clear optional benchmark
features, future lockfiles, operating-system libraries or an entire container image.

## Verification before release

- Compare every packaged file with manifest hashes; reject missing or changed notices.
- Run front-door synthetic capture/search and Soft Axiom memory checkpoint/restore against the
  packaged executable on the supported OS. No private transcripts, credentials or real models.
- Inspect ELF `NEEDED`/symbol versions in the builder. Host glibc/libgcc remain separately
  licensed OS prerequisites; if copied into an image, preserve its distro notices and sources.
- Record build command, source commit, toolchain/container identity and binary hashes in the
  release handoff. Separate trusted publishing credentials from the build.

Native release automation and publication are owned by Ahikara; this change does not deploy or
publish binaries. A package inventory is evidence of the reviewed scope, not a legal warranty.
