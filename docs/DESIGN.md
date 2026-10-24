# Oojuro engine design

Oojuro is an embedded database engine for durable, compressed, searchable records.
Its scope is storage and retrieval. Applications decide which records to collect,
how to interpret their metadata and which actions or user experiences they support.

## Engine responsibilities

- Append exact caller-supplied records with durable acknowledgement and recovery.
- Store originals using lossless compression and bounded, validated encodings.
- Read original content and expose time, version and retrieval operations.
- Maintain dictionaries, indexes, caches and compaction as storage mechanisms.
- Preserve supported storage, encryption, integrity and retry contracts.

The engine does not discover agent logs, attach hooks, proxy model APIs, choose
agent/session policy, run agents, manage hosted accounts or bill customers.
Those are application and service responsibilities.

## Format

`.ojo` is the selected user-facing extension for a future standalone database
file. It is a naming decision; current stores still use multiple files inside
a directory. Existing `.ojl`, `.ojc` and `.ojd` components retain their formats.
A standalone container needs its own versioned reader/writer and recovery
contract; see [the format decision](OJO_FORMAT.md).

Existing record types retain their current agent-oriented field and enum names
for compatibility. Separating repository ownership does not rewrite stored data
or claim that all historical schemas are already application-neutral.

## Applications and services

[Oojuro Journal](https://github.com/oojuro/journal) owns the agent application:
capture adapters, agent/session conventions, recorder, proxy, application CLI,
MCP tools and SDKs. Its initial source is extracted from immutable public
revision `d1b24f7ed020365e09431aeaa8f4a7ff05e27dc3` and pins the engine libraries.

O-Cloud owns managed hosting and the `oojuro.com` website. The private release
operator coordinates signing, publication and promotion; reproducible build
scripts and tests stay with the corresponding source component. Each component
has its own revision and release schedule.

See [repository boundaries](REPOSITORY_BOUNDARIES.md), [dependency policy](../DEPENDENCIES.md),
[storage measurements](PERF_STORE.md), [search measurements](PERF_SEARCH.md) and
[benchmark methodology](BENCH_HARNESS.md). The previous agent-application design
is retained in [Journal](https://github.com/oojuro/journal/blob/main/docs/DESIGN.md).
