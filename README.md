# Oojuro

The journal for AI agents: an embedded database that records agent activity,
losslessly compressed, and lets it search its own history in milliseconds. No LLM inside the database.

**Status:** pre-release. The native engine, CLI, SDKs and benchmark harnesses are published here; the
architecture is still being finalized, so formats and interfaces may change.

See the [open-source readiness assessment](docs/OPEN_SOURCE_READINESS.md) for the
current main/candidate boundary and remaining release gates, and
[capture transport](docs/CAPTURE_TRANSPORT.md) for optional compressed HTTPS uploads.

For local application records, start with the [local journal quickstart](docs/LOCAL_JOURNAL_QUICKSTART.md):
append typed JSONL, retry safely, search, retrieve original text and export bounded pages.
For Codex/Claude capture and shared hosted history, see [combined agent setup](docs/COMBINED_AGENT_SETUP.md).
**File extension:** `.ojo` is the chosen extension for a future standalone Oojuro journal file
(for example, `agent-memory.ojo`). Journals currently use directories containing multiple files;
standalone `.ojo` support is not implemented. See the [format naming decision](docs/OJO_FORMAT.md).

## License

Oojuro's original code is licensed under the **Apache License 2.0** (see `LICENSE` and `NOTICE`).
That covers the engine, file format, SDKs, CLI and hosted MCP client.

Third-party materials retain their own terms. In particular, the embedded starter dictionary
includes transformed CC BY 4.0, MIT and Apache-2.0 dataset material; it is not newly Apache-licensed
data. Preserve `NOTICE`, `licenses/` and the dictionary's provenance. Benchmark manifests point to
external datasets with separate licenses, including research-only inputs; those datasets are not
bundled with the runtime. See [publication boundaries](docs/PUBLICATION.md) and
[native distribution requirements](docs/NATIVE_DISTRIBUTION.md).

**Pledge:** the code in this repository will never be relicensed. What is open stays Apache-2.0.

How oojuro, Inc. makes money: the hosted Oojuro Cloud (durable journals for sandboxed agents, team journals,
compliance archive) is a separate proprietary service and is not part of this repository. Embedding Oojuro
in your own agents and products, commercial or not, is and stays free.

## Trademarks

"Oojuro" and its logos are trademarks of oojuro, Inc. The Apache-2.0 license grants rights to the
code, not to the name (see section 6 of the license). Hosted services built on this code must not be
called "Oojuro" or imply endorsement.

## Contributing

Contributions are accepted under Apache-2.0 (inbound = outbound) with a Developer Certificate of Origin
sign-off (`git commit -s`). There is no CLA.

## Build and test

You need a recent stable Rust toolchain (edition 2024) and a C compiler. Linux is the first-class platform.

```bash
cargo build --locked --release -p oojuro # builds `oojuro` (hosted client) and `oojuro-native` (local journal)
cargo test --locked --no-default-features --lib --tests -p oojuro-engine -p oojuro-rank -p oojuro-store -p oojuro-capture -p oojuro
```

Plain `cargo build` and `cargo test` select the engine/rank/store libraries.
Use the explicit application selection above and the native-engine gates in
CONTRIBUTING.md for complete runtime coverage.

These tests select the runtime packages and leave benchmark comparison features disabled.
See [CONTRIBUTING.md](CONTRIBUTING.md) for the explicit CI gates and topic-branch workflow.

Storage benchmark tools are explicit: `cargo build --release -p oojuro-store --features bench-tools`
for `store-bench` (`--example frame_probe` selects the probe); existing `codecs` and `baselines`
features also enable their JSON reporting dependency. The storage library does not require it.

The workspace crates:

| Crate | What it is |
|---|---|
| `oojuro` | native local CLI and scoped hosted capture/history/MCP clients |
| `oojuro-engine`, `oojuro-rank` | encrypted native authority, retrieval and ranking |
| `oojuro-capture` | attach-and-record ingestion of agent logs, with secret redaction |
| `oojuro-proxy` | recording proxy for Anthropic and OpenAI-compatible model APIs |
| `oojuro-trace` | historical comparison engine and test oracle |
| `oojuro-store` | append-only compressed store for agent text |
| `oojuro-nine` | ranking comparison engine and test oracle |
| `oojuro-core` | shared types and persistence primitives |
| `oojuro-bench`, `oojuro-tracebench` | benchmark harnesses (public datasets, competitor engines) |

Start with the [local journal quickstart](docs/LOCAL_JOURNAL_QUICKSTART.md) and
[combined agent setup](docs/COMBINED_AGENT_SETUP.md). See the [supported interface decision](docs/SUPPORTED_INTERFACES.md). The benchmark methodology and results are in
[docs/TRACEBENCH.md](docs/TRACEBENCH.md), [docs/BENCH_HARNESS.md](docs/BENCH_HARNESS.md) and
[docs/benchmarks/](docs/benchmarks/). See [CONTRIBUTING.md](CONTRIBUTING.md) to contribute.

For vulnerabilities, use [private security reporting](SECURITY.md). Source publication does not
announce an accepted native binary release or hosted deployment.
