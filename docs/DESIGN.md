# Oojuro design

This is the technical design of the engine in this repository. Details live in the
documents it links to: [CAPTURE](CAPTURE.md) (what is recorded and how),
[FRONT_DOOR](FRONT_DOOR.md) (the library, CLI and MCP surface), [SHARED_LOG](SHARED_LOG.md)
(the write path) and [TRACEBENCH](TRACEBENCH.md) (the workload used to choose between designs).

## What it is

A journal for agents and humans, in the way a system log is part of an operating system: one background
daemon (`oojurod`) per machine **attaches** to agents and records what they do automatically. Nobody calls
the database to store anything. Everything is kept losslessly, compressed automatically, and searchable
inside an agent's turn. There is no model inside the database.

## Principle: simple and horizontal

The database stays as simple and horizontal as possible: just enough information for agents to know what
happened. A feature that slows down ingest or search is removed.

- A record is flat: **writer** (the memory owner and erasure domain), **agent id** (unique per agent
  instance: every session and every subagent), optional **parent id**, timestamps, kind, text. No other
  structure is stored.
- Trees, sessions, periods and any other analysis are **query-time functions** over those fields. They never
  add work, state or bytes to the write path.
- Every analysis feature lives behind a boundary that can be deleted in one commit. It ships only if ingest
  and search benchmarks are unchanged; otherwise it is removed.

## Capture boundary: the agent's perception

Everything the agent sees and thinks is logged. One store per agent holds its complete experience stream:

- everything that entered its context: user messages, system prompt, tool results, files it read, command
  output, fetched pages
- everything it produced: thinking (as far as the provider exposes it), tool calls, outputs

The writer is the agent, the observer. Human information is captured because it flows through the agent, so
there are no separate human capture sources. Because the model proxy forwards exactly what the agent sees,
and agents re-send their history on every call, storing each message once by hash is required, not optional.

## Record

A record is a **writer ID** (an agent or a human), **timestamps** (when it happened, when it was recorded),
and the **text**. There is no user-defined schema and no thread ID. Structure (kind: input, output,
thinking, tool call, tool result; tool name; paths; errors; exit status) is **inferred from the text** by
the engine.

## Stores

Journals currently use directories containing multiple files. The chosen extension for a future
standalone journal file is `.ojo`, for example `agent-memory.ojo`; the container is not implemented.
See the [format naming decision](OJO_FORMAT.md) for current storage and remaining design work.

- **Private store per writer.** Append-only, auto-compressed, holds everything that writer did. It is also
  the writer's erasure domain: dedupe and dictionaries never cross stores.
- **Shared memory: deferred.** A store that other people's agents can also read is not built; private stores
  only.
- **Erasure.** Erasing a writer, a key, or everything older than N days physically removes the bytes. Proof
  is required: erased text must be absent from every file.

## Capture (attach modes)

| Source | Captures | Needs |
|---|---|---|
| Log tailer | Claude Code JSONL, Codex rollout JSONL, appended as written | nothing |
| Claude Code hooks | prompt, tool call, tool result, stop; also **injects** the top 20 before each turn | one settings entry |
| Model API proxy | every request/response/tool call of any agent with a configurable base URL | base URL change |
| OpenTelemetry receiver | GenAI spans from instrumented frameworks | exporter endpoint |

**Redaction on ingest:** known secret formats and high-entropy tokens are masked before anything is stored.

## Explicit use (like SQLite)

Besides automatic capture, an agent or program can use the store directly, the way one uses SQLite. It is an
embedded library with no server, one folder per store, zero configuration and crash-safe, with many readers
and one writer. The surface is an API plus CLI plus MCP tools, and no SQL.

| Call | Meaning |
|---|---|
| `remember(text, key?)` | explicit write; with a key, the record is versioned and the newest wins in search |
| `search(query, k, filters)` | bits-ranked search; filters by time, kind and key |
| `get(id or key)` / `history(key)` | exact read / every version |
| `range(from, to)` | a time window |
| `forget(id / key / older-than)` | proven deletion |

Explicit writes are indexed like captured ones and carry kind `explicit`. Surfaces: a Rust library, a CLI
(`oojuro put/search/get/...`) and MCP tools (`remember`, `search`, `get`, `forget`).

## Read path

- **Search before each turn:** the word index supplies candidates, ranked by **bits saved** (query
  likelihood) plus a **recency prior in bits**. All results come from the writer's own store, followed by
  exact excerpts fetched from the compressed store.
- **Read by writer, time or key:** direct and exact, with no index involved.
- **No token window.** Records of any size (up to 32 MiB each, larger ones split automatically) are indexed
  whole, with no chunking. Queries are not truncated by default: the engine accepts 64 KiB today, and that
  is a setting, not an architectural limit. Long inputs are handled by information weighting (rare words
  save more bits).
- **Injection budget:** up to 20 excerpts, cut off where a record saves too few bits, within a token budget
  (about 2-3k tokens, configurable). A turn with nothing relevant gets fewer results, or none.

## Settings (all user-controlled)

`~/.oojuro/config.toml`; a per-project `.oojuro.toml` and per-agent sections (`[agents."claude-code:repo"]`)
override it. Recording and injection switch on and off independently.

| Key | Default | Meaning |
|---|---|---|
| `record` | true | capture what the agent sees, thinks and does |
| `inject` | false | search before each turn and add the results to context; off until retrieval is proven offline on real data |
| `top_k` | 20 | maximum results injected per turn |
| `budget_tokens` | 2500 | cap on injected text |
| `min_bits` | 8 | inject only records that save at least this many bits (fewer or none if nothing fits) |
| `recency` | 1.0 | recency weight in bits per turn (0 = off) |
| `record_thinking` | true | store thinking |
| `index_thinking` | true | thinking is also searchable by its own words |
| `exclude` | [] | path globs never recorded |

Commands: `oojuro pause|resume`, `oojuro inject on|off`, `oojuro forget --last 1h`. Explicit
`search(query, k)` calls take any k, regardless of the injection settings.

## Write path

1. Append into the writer's store. Durable at acknowledgement, with one sync per finished turn.
2. Searchable on the next turn.
3. Background compaction: zstd with a dictionary plus dedupe for the hot tier, and a stronger codec for the
   archive.

The shared write log is the default: one preallocated log under the private stores. Privacy and erasure stay
per store. [SHARED_LOG](SHARED_LOG.md) has the design and the measurements.

## Decisions

| Topic | Decision |
|---|---|
| Defaults | chosen by the measured rules (a held-out win of at least 2%, all gates, within budgets) |
| Platforms | Linux first; macOS and Windows later |
| Retention | forever; users forget anything with proven deletion |
| Recording proxy | Anthropic and OpenAI-compatible APIs |
| Starter dictionary | trained only on public agent datasets whose licenses allow redistribution (`crates/oojuro-store/dict/README.md`) |
| Thinking | stored and indexed |
| SQL | none: API, CLI and MCP only |
| Shared memory | deferred |

## Scope of this repository

Everything that runs in your process or on your own machine is here: the engine, the file format, the local
recorder and daemon, the CLI, the local MCP server, and the benchmark harnesses. Hosted services are not part
of this repository (see the README).

## Choosing algorithms by measurement

- **Compression:** a codec interface over store frames, evaluated on public agent traces and plain text.
  Families: LZ4, zstd (levels, dictionaries, long range), brotli, xz, PPMd, BWT, context mixing as the floor,
  FSST, dedupe, columnar JSON, log templates. Output: the ratio against decode-speed frontier per tier
  ([TRACK_COMPRESS](TRACK_COMPRESS.md)).
- **Retrieval:** lexical candidates feed pluggable scorers (BM25, Dirichlet and SPUD query likelihood,
  divergence from randomness, compression semantics, associations learned from the writer's own data,
  recency and usage probabilities, fusion in bits). Judged by public sets (turn loop, LongMemEval, BEIR) and
  by automatic labels from agent logs ([BENCH_HARNESS](BENCH_HARNESS.md), [SEARCH_FIXES](SEARCH_FIXES.md)).
- **Agent workload (TraceBench):** ingest, the per-turn loop, reads, size and purge, against Postgres,
  ClickHouse, SQLite and other engines, on public traces ([TRACEBENCH](TRACEBENCH.md)).
- **Traditional:** YCSB workloads A-F adapted to append-with-versions, bulk load, a concurrent-writer sweep
  (1-16), point reads, range scans, open and recovery time, disk and RSS.

## Privacy rules for measurements on real logs

Logs of a real developer machine are read locally only. Nothing from them is committed, uploaded or sent to
any API. Only aggregate numbers go into this repository.
