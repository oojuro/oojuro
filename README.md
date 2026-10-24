# Oojuro

An embedded database engine for durable, compressed, searchable records.
Applications own the meaning of their records; Oojuro owns storage, exact reads,
time/version views, compression, recovery and retrieval. No model runs inside
the database.

**Status: pre-release.** `.ojo` is the selected extension for a future standalone
Oojuro database file, such as `memory.ojo`. Current stores use directories;
standalone `.ojo` read/write support is not implemented. See the
[format decision](docs/OJO_FORMAT.md).

## One responsibility per repository

| Repository | Purpose |
| --- | --- |
| [oojuro](https://github.com/oojuro/oojuro) | Embedded database engine and `.ojo` format |
| [journal](https://github.com/oojuro/journal) | Agent capture, sessions, recorder, CLI/MCP integrations and client SDKs |
| [o-cloud](https://github.com/oojuro/o-cloud) | Managed hosting, sharing, accounts, billing and the `oojuro.com` website |
| [o-release-operator](https://github.com/oojuro/o-release-operator) | Trusted release coordination for this family |

For recording Codex/Claude activity or using the compatibility `oojuro`,
`oojurod` and `oojuro-native` application commands, use **Oojuro Journal**.
Engine libraries work locally without a cloud account. Soft Axiom is a separate
personal-agent application that consumes storage; its runtime and hosting live
in their own repositories. See [source boundaries](docs/REPOSITORY_BOUNDARIES.md).

## Build and test

Use stable Rust supporting edition 2024 and a C compiler. Linux is the primary
platform. Default workspace members are the engine, rank and store libraries.

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
