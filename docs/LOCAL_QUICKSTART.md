# Use Oojuro locally

Oojuro is an experimental embedded journal database. An event contains an
engine-assigned UTC acceptance timestamp and exact bytes. No agent schema or
cloud account is required. Agent capture and session integrations belong to
[Oojuro Journal](https://github.com/oojuro/o-journal). Managed Cloud hosting
and development are suspended.

## Install

From a checkout of the current source:

```sh
cargo install --locked --path crates/ojo-tools
ojo --version
```

Linux is the primary platform. Rust 1.89+ and a C compiler are required for the
normal CLI. Use `--no-default-features` for raw-file tools without search/archive
or a C compiler. The [published v0.2.0 preview](https://github.com/oojuro/oojuro/releases/tag/ojo-v0.2.0)
has prebuilt Linux installation options and explicit `index`/`search`; managed
`query`, readable output and continuous `--watch` require this newer source.

## Write and inspect

```sh
ojo init sensors.juro
printf 'sensor alpha calibration completed\n' | ojo append sensors.juro -
printf 'sensor beta calibration failed\n' | ojo append sensors.juro -
ojo scan sensors.juro --text
```

Readable output shows UTC ISO timestamps and JSON-escaped text. Each event is
one line even when it contains newlines, NULs or terminal control codes.
Non-UTF-8 originals appear as `hex:...`. The database preserves every input
byte; it does not add or remove a newline. Empty and binary events are valid.
A single event can contain up to 16 MiB.

Omit `--text` for a JSON page containing `accepted_at` (signed UTC Unix
microseconds), opaque cursor and `data_hex`. Resume with `--after CURSOR_HEX`.
`--limit` is 1–1,024, default 100; pages also cap originals at 64 MiB.
`scan --from MICROS --to MICROS` uses a half-open time range and time order.
Append order stays independent of the clock.

Read an exact original using a cursor from `append`, `scan` or a search hit:

```sh
ojo read sensors.juro CURSOR_HEX --output original.bin
```

Without `--output`, bytes go to stdout. Output files must be new.

## Retry an uncertain append

```sh
printf 'sensor gamma calibration completed\n' > event.bin
ojo append sensors.juro event.bin --intent gamma-001.intent
```

Choose a new intent file for each logical event. The CLI durably saves the
engine-issued intent before committing. If the process ends before the
acknowledgement arrives, rerun that exact command with the same bytes and
intent. A successful retry returns the original cursor and timestamp without
adding a duplicate. Changed bytes or another journal are refused. Without a
retained intent, another append is distinct even for equal data.

## Search as the journal grows

```sh
ojo keygen search.key
ojo query sensors.juro calibration --index-key search.key
printf 'sensor delta calibration completed\n' | ojo append sensors.juro -
ojo query sensors.juro calibration --index-key search.key
```

`query` builds a private encrypted cache at `sensors.juro.cache`, then reuses it
or incrementally adds verified new events. Ranked UTF-8 text hits contain exact
excerpts, source cursors and timestamps. Binary originals remain readable.
`--from MICROS`, `--to MICROS` and `--limit N` constrain results. Search does not
interpret JSON fields or agent metadata.

Use `--cache PRIVATE_DIR` to choose a different cache. Keep source files, source
encryption keys and the index key outside it. This is managed derived storage:
successful queries remove obsolete owned cache generations. Do not store other
files inside it. A new generation temporarily coexists with the old one;
immutable packs are hard-linked on the same filesystem. The small plaintext
receipt contains file/checkpoint identifiers, not original text or keys.
Text and index contents are encrypted.

The cache is bound to a verified source prefix. Replaced, truncated, divergent
or corrupt sources are refused; a compressed archive needs a new cache. An
exact exported copy can reuse it. Wrong keys, exhausted budgets, concurrent
queries using the same cache and failed refreshes return errors without stale
or partial results. A generation and its checkpoint are published together.
Interrupted temporary generations can remain until a successful query prunes
them. A damaged cache can be removed and rebuilt; source originals never
depend on it.

`--compression raw|zstd` selects new cache data encoding; default is raw.
`--max-index-bytes N` defaults to an 8-GiB logical per-generation build budget.
`--max-original-bytes N` defaults to 64 MiB of original/cache-text query work.
The cache retains its original per-query ceiling: later queries can lower it,
while raising it requires a fresh cache. At most 1,024 hits and a 1-MiB result
payload are admitted. Logical budgets are not hard RSS or filesystem quotas;
source metadata, retained generations and temporary work also consume resources.
A million-record cache can be substantial: see the [measurements](benchmarks/JURO_ACTUAL_FILE_MILLION.md).

Each invocation verifies the full journal and holds a read lock during
building/searching. A competing append refuses contention and must retry after
the reader closes. Large histories may take seconds to reopen and minutes to
build. Long-lived embedded applications can retain an owning Collector and
index handles. For explicit immutable snapshots, `index`/`search` remain available.

## Follow new events

```sh
ojo follow sensors.juro --watch --text
```

This prints existing events, then continues until interrupted. Add `--after
CURSOR_HEX` to resume at an append position. Without `--text`, `--watch` emits
one bounded JSON page per line, including empty heartbeat pages retaining the
last cursor. `--wait-ms` sets the heartbeat wait, default 1,000 ms; continuous
mode accepts 25–60,000 ms. Omitting `--watch` preserves single-page follow.
Snapshots close before output or waits, permitting short CLI appends between
polls. A persistent external writer excludes CLI readers; concurrent embedded
applications should use the Collector API.

## Encryption, recovery and copies

Create a source key with `ojo keygen journal.key` and add `--key-file journal.key`
to commands accessing that encrypted source. Keys contain exactly 32 raw bytes
and must have private Unix permissions. Source and search keys are separate.

`ojo verify sensors.juro` reports history verification and incomplete tail bytes.
Read/query commands refuse incomplete tails. After inspection, `ojo recover
sensors.juro` truncates only an incomplete trailing write; complete corruption
is refused. Add the key option when encrypted. Keep recovery copies of important
files and their separately stored keys.

`ojo export sensors.juro copy.juro` makes a validated byte-for-byte copy.
`ojo archive sensors.juro compressed.juro` creates a separate compressed file;
the original stays unchanged and logical event cursors are preserved. Existing
`.ojo` paths work without conversion. Format/profile versions determine
compatibility. There is no stable-format promise yet.
