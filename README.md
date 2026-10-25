# Oojuro

A minimal Rust engine for durable `.ojo` files: opaque bytes, atomic batches,
exact reads, crash recovery and optional authenticated encryption. Normal writes
are uncompressed. The fresh implementation in [`crates/ojo`](crates/ojo/README.md)
is independent of the older storage/search crates.

**Status: experimental.** The standalone [wire 0.1 specification](spec/README.md)
and frozen interoperability vectors are versioned separately from the engine.
The default build selects only `ojo` and needs no C compiler. Existing libraries
remain available through explicit package selection for compatibility.

```sh
cargo build --locked
cargo test --locked -p ojo
cargo test --locked -p ojo --features archive
```

The generic [`ojo` file tools](crates/ojo-tools/README.md) inspect, verify, recover,
export and manipulate databases without any agent integration:

```sh
cargo install --locked --path crates/ojo-tools
ojo --help
```

The optional archive feature creates a separate compressed `.ojo` file while
preserving the original, its checkpoints and encryption. Session lifecycle and
24-hour idle scheduling belong to ojo-journal/ojo-cloud. See the
[engine API, guarantees and limits](crates/ojo/README.md).
See the [file-engine benchmarks and optimization results](docs/benchmarks/OJO_IO_OPTIMIZATION.md)
for measured workloads, SQLite comparisons and performance tradeoffs.

## One responsibility per repository

| Repository | Purpose |
| --- | --- |
| [oojuro](https://github.com/oojuro/oojuro) | Embedded database engine and `.ojo` format |
| [o-journal](https://github.com/oojuro/o-journal) | Agent capture, sessions, recorder, CLI/MCP integrations and client SDKs |
| [o-cloud](https://github.com/oojuro/o-cloud) | Managed hosting, sharing, accounts, billing and the `oojuro.com` website |
| [o-release-operator](https://github.com/oojuro/o-release-operator) | Trusted release coordination for this family |

For recording Codex/Claude activity or using the compatibility `oojuro`,
`oojurod` and `oojuro-native` application commands, use **Oojuro Journal**.
Engine libraries work locally without a cloud account. Soft Axiom is a separate
personal-agent application that consumes storage; its runtime and hosting live
in their own repositories. See [source boundaries](docs/REPOSITORY_BOUNDARIES.md).

## Build and test

The new `ojo` crate requires Rust 1.89+; Linux is the primary platform. The
optional archive/benchmark features and older libraries require a C compiler.
The following explicit checks cover the retained compatibility libraries:

```sh
cargo build --locked
cargo test --locked --no-default-features --lib --tests -p oojuro-core -p oojuro-store -p oojuro-nine
cargo test --locked --no-default-features --lib --tests -p oojuro-engine -p oojuro-rank -- --test-threads=1
cargo test --locked --no-default-features --lib --tests -p oojuro-trace
python3 scripts/dependency_audit.py --engine-only --check --output /tmp/oojuro-dependencies.json
```

The storage and retrieval source, data formats and algorithms are unchanged by
the repository split. Legacy `oojuro-core`, `oojuro-nine` and `oojuro-trace`
remain for compatibility and comparison; this reorganization does not rename
persisted fields or claim a newly generic binary format.

Benchmarks remain explicit opt-ins; see [CONTRIBUTING.md](CONTRIBUTING.md),
[dependency policy](DEPENDENCIES.md) and [benchmark methodology](docs/BENCH_HARNESS.md).
Current source pins and released installations remain on their exact revisions;
extracting the application does not update or deploy a cloud consumer.

## License

Original code is Apache-2.0; see [LICENSE](LICENSE) and [NOTICE](NOTICE).
Third-party materials retain their own licenses. Preserve the
[license inventory](licenses/README.md), dictionary provenance and required
attribution in source and native distributions. Original published source stays
Apache-2.0; moving source between repositories does not relicense it.

Oojuro Cloud is a separate proprietary hosted service. Embedding the engine in
commercial or noncommercial applications remains free under Apache-2.0.
Oojuro and its logos are trademarks of oojuro, Inc.; the code license does not
grant trademark rights. See [publication boundaries](docs/PUBLICATION.md),
[native distribution requirements](docs/NATIVE_DISTRIBUTION.md) and
[security reporting](SECURITY.md).
