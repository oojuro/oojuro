# Oojuro

A minimal Rust engine for durable `.juro` files: opaque bytes, atomic batches,
exact reads, crash recovery and optional authenticated encryption. Normal writes
are uncompressed. The fresh implementation in [`crates/ojo`](crates/ojo/README.md)
is independent of the older storage/search crates.

New files use `.juro`. Existing `.ojo` files and explicit paths remain supported
without renaming or conversion; the header and version determine compatibility.
Oojuro and its CLI/API names are unchanged. See the
[filename decision](https://github.com/oojuro/oojuro/issues/110).

The additive experimental [Event API](docs/EVENT_DATABASE.md) accepts just data;
the engine supplies UTC acceptance time and an append cursor. It adds exact
original reads, append/time scans and an embedded live collector under an
explicit [Event profile](spec/events/1/FORMAT.md). Existing KV files retain
their meaning. The generic CLI can build disposable encrypted text indexes and
search selected originals from their exact file snapshot. Scale qualification
remains in progress. The v0.2.0 experimental preview includes these APIs and
commands; the immutable v0.1.0 preview remains the earlier KV file-tool release.

## Use a local journal

Build the current source with Rust 1.89+ and a C compiler:

```sh
cargo install --locked --path crates/ojo-tools
ojo init activity.juro
printf 'sensor alpha calibration completed\n' | ojo append activity.juro -
ojo scan activity.juro --text
ojo keygen search.key
ojo query activity.juro calibration --index-key search.key
```

Append another event and repeat `query`: its private encrypted cache refreshes
from the verified append prefix. Follow events in a terminal with
`ojo follow activity.juro --watch --text`. These convenience options require
the current source; the immutable v0.2.0 preview has the earlier explicit
`index`/`search` and bounded `follow` commands.

For encrypted sources, create a separate key with `ojo keygen journal.key` and
add `--key-file journal.key` to each command accessing that file. For safe
retries after an uncertain acknowledgement, retain a new `--intent FILE` for
each logical append and retry with the same intent and exact bytes. Read exact
originals with `ojo read FILE CURSOR_HEX`. See the [local guide](docs/LOCAL_QUICKSTART.md)
for time ranges, encryption, cache limits and recovery.

**Project focus:** Oojuro remains an experimental open source embedded database.
Managed Oojuro Cloud hosting and development are suspended while the company
focuses on Soft Axiom. Local files and tools require no cloud account.

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

The [v0.2.0 preview release](https://github.com/oojuro/oojuro/releases/tag/ojo-v0.2.0)
also provides a checksummed Linux x86-64 package. See the
[curl, Cargo and Homebrew installation options](crates/ojo-tools/README.md#install-from-source).

The optional archive feature creates a separate compressed `.juro` file while
preserving the original, its checkpoints and encryption. Session lifecycle and
24-hour idle scheduling belong to ojo-journal/ojo-cloud. See the
[engine API, guarantees and limits](crates/ojo/README.md).
See the [file-engine benchmarks and optimization results](docs/benchmarks/OJO_IO_OPTIMIZATION.md)
for measured workloads, SQLite comparisons and performance tradeoffs.

## One responsibility per repository

| Repository | Purpose |
| --- | --- |
| [oojuro](https://github.com/oojuro/oojuro) | Embedded database engine and `.juro` format |
| [o-journal](https://github.com/oojuro/o-journal) | Agent capture, sessions, recorder, CLI/MCP integrations and client SDKs |
| [o-cloud](https://github.com/oojuro/o-cloud) | Suspended managed hosting and website; private service source |
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
