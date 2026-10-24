# Engine dependency policy and audit

The minimal production library is `oojuro-engine` → `oojuro-rank` +
`oojuro-store`. `oojuro-core`, `oojuro-nine` and `oojuro-trace` are separate
legacy/library stacks, not aliases for that graph. A workspace lockfile is not
the list of libraries in a product build.

The database implementation belongs here: formats, append/commit, recovery,
indexes, query execution/ranking, caches, compaction, deduplication references
and retention. Established cryptography and standard codec implementations are
deliberate dependencies. Do not replace them to lower a dependency count.

## Build boundaries

```sh
cargo build --locked --release                     # engine/rank/store libraries
cargo build --locked --release -p oojuro            # CLI/recorder, capture and proxy
cargo build --locked -p oojuro-store --features bench-tools --bin store-bench
cargo build --locked -p oojuro-store --features bench-tools --example frame_probe
cargo build --locked -p oojuro-store --features codecs,baselines
python3 scripts/dependency_audit.py --check --output work/dependency-audit.json
```

`--workspace`, explicitly selecting a benchmark package, or explicitly enabling
tool features is an intentional broader build. Feature unification is additive;
a combined application graph can enable more features than an isolated library.
Keep CI checks for the isolated graph as well as testing consuming applications.

`dependency-policy.json` contains the reviewed allowlist of external package
**names**, including transitive build/platform support. The direct allowlist is
eight foundational libraries and four explicit algorithm/platform exceptions.
The larger transitive set is not permission to add new direct dependencies.
`--engine-only --check` is a policy-only CI graph check: metadata uses
`--no-deps`, so its external package inventory is intentionally omitted and
reported as such. The full audit records the selected external metadata inventory
and refuses an unexplained coverage gap. Its metadata includes requested Store
comparison features and all platform predicates, matching the graph union rather
than silently omitting optional codecs or Windows/WASI support. Current core/workspace dependencies
have no Git-sourced external packages; source-qualified graph keys preserve Git
identity, and any unsupported inventory mapping must fail rather than disappear.

Changes to a dependency, enabled features, native build or lockfile require
review of purpose, scope, security, format/query compatibility and affected
tests. Prefer removing unused features or isolating tools over rewriting code.

No database engine, cloud/provider SDK, network runtime, benchmark database or
experimental comparison codec may enter the minimal graph. An unexpected
package or LZ4 frame wrapper causes the audit check to fail. The check preserves
safe/checked LZ4 block decoding. It also checks all Cargo target predicates;
cross-platform compilation is a separate qualification, not implied by a tree.

## Database ownership

| Responsibility | Implementation owned in this repository |
| --- | --- |
| Frame/run formats, metadata, checksums | `oojuro-store/src/format.rs`, `run.rs`, `store.rs` |
| Packed body, dedup reference encoding and validation | `oojuro-store/src/pack.rs`, `reader.rs` |
| Append, sealed authority, retry receipts and recovery | `store.rs`, `store/bounded.rs`, `oojuro-engine/src/capture_log.rs`, `record/authority.rs` |
| Lexical normalization, postings, disk index and merges | `oojuro-engine/src/lexical/`, including `segments/codec.rs` |
| Query execution, ranking and pagination | `oojuro-engine/src/lexical/`, `record/`, dependency-free `oojuro-rank` |
| Cache identity/authority and invalidation | `record/paged/cache/`, `record_cache.rs`, record/query view code |
| Compaction, retention, erasure and audit state | `oojuro-store/src/store.rs`, `oojuro-engine/src/record/managed/`, `audit.rs` |

## Direct production dependencies

| Dependency | Actual use | Decision |
| --- | --- | --- |
| `serde` | Versioned authority/receipt/index metadata DTOs throughout engine | Retain. Derivation and strict format handling are established infrastructure. |
| `serde_json` | Persisted authority metadata and engine wire representations | Retain in engine/legacy API; optional tool reporting in Store. No handwritten JSON parser. |
| `blake3` | Content hashes, commitments, dictionary IDs and key IDs in store/engine | Retain, `std,pure`, without default features. Do not rewrite cryptography. |
| `chacha20poly1305` | `oojuro-store/src/crypto.rs`: XChaCha20-Poly1305, random nonces/keys, authenticated headers | Retain RustCrypto, including OS randomness/zeroization dependencies. |
| `ring` | `audit.rs` Ed25519 signatures; `record/managed/control.rs` signed checkpoints | Retain. Managed state actually uses it, so making only `audit.rs` optional is insufficient. |
| `zstd` | Store packing, incremental decoding, dictionaries; trace column blocks | Retain standard Zstandard. Existing magicless format requires the `experimental` **API feature**. Dictionary training requires `zdict_builder`. These are not optional comparison codecs. |
| `lz4_flex` | Legacy block decompression in Store, block compression/decompression in legacy core/nine | Retain safe/checked block implementation. Remove unused `frame` wrapper and its `twox-hash` dependency. |
| `crc32fast` | Existing frame, index and dictionary accidental-corruption checksums | Retain; not a cryptographic substitute. Changing it would change formats. |
| `fastcdc` | `pack.rs` FastCDC v2020 boundaries before owned chunk/reference packing; capture uses it separately | Retain explicit algorithm exception. Dependency-free library; copying tables/algorithm is not a useful ownership gain. Own references, collision checks and lifecycle here. |
| `aho-corasick` | `record/managed.rs` multi-pattern occurrence verification | Retain established linear-time matching; replacing with repeated scans risks query/retention cost. |
| `rust-stemmers` | `lexical/text.rs` pinned English normalization; legacy nine normalization | Retain pinned `1.2.0`; changing stemming can change rankings and index/query compatibility. |
| `libc` | Unix no-follow opens, nonblocking locks; crash tests/process handling | Retain platform bindings. A future standard-library substitution must preserve locking/open semantics; libc also remains transitive. |

`ring` and RustCrypto provide different required primitives (signed witnesses vs
XChaCha20-Poly1305). Consolidating them by changing encryption/signature formats
is not justified. BLAKE3 is a different required content hash. Keep their
constant-time, randomness and zeroization support.

## Transitive production dependencies

The version-specific inventory records every package's description, source,
license, enabled features, immediate `used_by` parents, scopes, native `links`
metadata, build-script presence and disposition. These groups explain why they
are retained; they are not new database implementations.

| Packages | What they provide; decision |
| --- | --- |
| `aead`, `chacha20`, `cipher`, `crypto-common`, `inout`, `poly1305`, `universal-hash` | RustCrypto primitive composition and authenticated encryption; retain with the reviewed crypto library. |
| `rand_core`, `getrandom` | Crypto RNG interface and OS entropy; retain. No deterministic/randomness substitution. |
| `generic-array`, `typenum`, `version_check` | Fixed-size crypto buffers/types and compiler capability detection; retain transitively. |
| `subtle`, `constant_time_eq`, `zeroize`, `opaque-debug`, `untrusted` | Constant-time operations, secret cleanup and crypto input handling; retain. |
| `arrayvec`, `cfg-if`, both `cpufeatures` versions | Bounded BLAKE3 buffers, target selection and CPU capability detection; retain. Major versions are not interchangeable. |
| `serde_core`, `serde_derive`, `proc-macro2`, `quote`, `syn`, `unicode-ident` | Serialization support and compile-time derive/token parsing; retain. Proc macros are build tooling, not an embedded database. |
| `itoa`, `zmij`, `memchr` | JSON numeric formatting and byte scanning; retain where needed. `memchr` also supports Aho-Corasick. Removed from isolated Store with JSON reporting. |
| `zstd-safe`, `zstd-sys` | Rust wrapper and bundled native Zstandard; retain standard codec. |
| `cc`, `find-msvc-tools`, `jobserver`, `shlex`, `pkg-config` | Compiler discovery, bounded build parallelism and native library construction/probing; retain as build support. |
| `wasi`, `r-efi`, `windows-sys`, `windows-targets`, `windows_*` target packages | Conditional OS/build support for entropy, ring and compiler tools. Present in all-target audit; not all compiled into Linux production. Retain transitively. |

`twox-hash@2.1.5` was used only by the disabled LZ4 frame wrapper in the minimal
graph. It remains allowed in explicitly selected tools if another tool needs it;
it is not on the engine allowlist. Rust's standard library, libc ABI and compiler
runtime are platform dependencies outside Cargo's registry count.

## CLI, SDK, legacy libraries and integrations

These are separately audited, not included in the minimal engine budget.

| Scope | Main additional dependencies / disposition |
| --- | --- |
| `oojuro-core`, `oojuro-nine`, `oojuro-trace` | Legacy JSON/LZ4 log, mmap (`memmap2`), trace columns and legacy ranking; retain separately for compatibility. Do not silently migrate existing formats. |
| `oojuro-capture` | `regex`, `sha2`, `fastcdc`, libc and trace/engine libraries for ingestion/redaction/capture; retain in capture package. |
| `oojuro-proxy` | Hyper HTTP/body utilities, Tokio, rustls/ring/web PKI roots, bytes, pin projection, gzip/Brotli/Zstd HTTP decoding; retain in proxy package. No provider SDK in engine. |
| `oojuro` | `reqwest` blocking rustls HTTP, TOML settings, regex, `jiff` time handling, `prost` OTLP and `flate2`; retain at application boundary. Splitting the older CLI would be a larger migration, not part of these small reductions. |
| `sdk/typescript` | Inherited experimental client tree; outside the engine and this composition. Later session SDK/client proposals have separate qualification. |
| `sdk/temporal` | Explicit Temporal worker/workflow/common and Oojuro-client peer dependencies; retain in the Temporal integration. Never propagate Temporal into engine. |

## Benchmarks and experiments

`oojuro-bench` and `oojuro-tracebench` deliberately compare other systems. Their
SQLite, PostgreSQL, Tantivy, RocksDB, redb, fjall and optional Lance/Arrow/DataFusion
graphs are **not** engine requirements. Store's `baselines` feature adds comparison
databases; its `codecs` feature adds Brotli, liblzma, bzip2, libdeflater, Snappy,
native LZ4, PPMd, FSST, tiktoken and FM-index experiments. All stay explicit.

The workspace previously selected all those benchmark packages in a plain
`cargo build`; production `default-members` now prevent that selection.
The benchmark crates' own defaults remain compatible when explicitly selected.
Tests and benchmark examples may have dev dependencies; audit counts exclude
dev edges, and those dependencies never become a dependent application's
production requirements.

## Native code, CLI and provider boundaries

The isolated Linux engine normal/build graph retains bundled Zstandard
(`zstd-sys`, source Zstd1.5.7) and ring crypto native C/assembly. BLAKE3 selects
pure Rust. `cc`, build scripts and Cargo's `links` metadata are build support,
not proof that a native archive was linked. Record actual build outputs and
linked libraries separately when qualifying a distributable binary. No native
library or build script is replaced by this composition.

The `oojuro-native` command is a binary in package `oojuro`; selecting that
binary still compiles the package's declared CLI dependencies. Its Cargo build
graph is not the same as the standalone `oojuro-engine` library. This proposal
adds no engine feature and changes no native runtime flag. Trace/nine
`phase-profile` is diagnostic instrumentation, not a new production library.

Provider SDKs, hosted accounts/HTTP/TLS/database services and optional document
parsers belong outside the minimal engine. Their graphs and deployment
qualification are separate. No private hosted source or provider dependency is
included by this public engine proposal.

## Current composition and qualification

This proposal adapts public dependency draft66 onto qualified main/native
foundation75 at `ba144d90d396b680122f1e93c808c724844ef2c8`. Their common ancestor
is `f01345f92a2570ddce7a9577b7703fddfc8eaee7`; this is the same native engine,
not a substitution of a legacy library. It preserves current allocation
accounting, formats, native licensing/notices, publication rules and CI tests.
Older performance experiments and optional drafts65/68 are not merged.

Only feature/build selection changes: existing safe checked LZ4 blocks stay;
unused frame wrapping is disabled; Store JSON reporting becomes explicit
`bench-tools`; plain workspace builds select the three libraries. No storage,
append/recovery, query/ranking, compaction or encryption implementation changes.
The Store Rust library has no serde_json public API or source use; its reporting
binaries/example retain JSON with the documented feature. Existing callers of
`store-bench`/`frame_probe` must add `--features bench-tools`. `baselines` and
`codecs` enable that feature automatically.

See [composition measurements and pending gate plan](docs/dependencies/MAIN_NATIVE_DEPENDENCY_COMPOSITION.md).
Graph reductions are measured build dependencies, not latency/RAM/cost results.
The composition has passed its selected functional and fresh-artifact-cache
build gates; see [the exact current qualification](docs/dependencies/main-native-qualification.json).
One matched build sample was essentially unchanged, so no speed/RAM gain is claimed.
