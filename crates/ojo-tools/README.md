# ojo file tools

`ojo` creates, inspects, verifies and manipulates standalone `.juro` databases.
It uses the same Rust engine and independently versioned wire 0.1 as applications.
Agent capture, session conventions and integrations live in ojo-journal.

`.juro` is the canonical new extension. Existing `.ojo` files and explicit
caller paths continue to work; the tool neither renames nor silently converts
them. File headers and versions determine compatibility, not suffixes. The
immutable v0.1.0 release predates this convention and keeps its original name.

The current source adds generic timestamp-and-data journals. These commands
postdate the immutable v0.1.0 preview; build from this source until a newer
qualified package is published.

```sh
ojo keygen private.key
ojo init sensors.juro --key-file private.key
printf 'sensor=alpha temperature=21.5\n' > sample.bin
ojo append sensors.juro sample.bin --intent sample.intent --key-file private.key
ojo scan sensors.juro --limit 100 --key-file private.key
```

`append` requires only the data: the engine assigns its UTC acceptance time and
opaque cursor. Read exact bytes with `ojo read sensors.juro CURSOR_HEX`, adding
the key option for encrypted files. `--output NEW_FILE` refuses replacement.
Data may be a file, empty bytes, binary bytes, or `-` for stdin; it is never
implicitly decoded or rewritten.

`--intent` is optional engine retry bookkeeping. For writes whose acknowledgement
may be lost, choose a new private intent file per logical append. The tool
durably stores its engine-issued intent before committing. Reuse that exact
file with the same data after uncertainty to obtain the original cursor/time
without duplication. Changed data or a foreign journal is refused. Existing
intent/data validation runs on a read-only snapshot before writable tail recovery.
Distinct appends without a retained intent remain distinct even for identical data.

`scan` emits one JSON object with exact `data_hex` originals and a `page` object.
Resume using `page.cursor` with `--after`; `page.has_more` describes that snapshot.
Optional `--from`/`--to` use signed UTC Unix microseconds and a [from,to) time
range. Timestamp ordering is separate from append ordering.

`follow` returns one bounded append-order page, waiting up to `--wait-ms` (default
1,000; maximum 60,000). Resume with its cursor even after an empty page. Each
snapshot closes before waiting or writing stdout, so another short-lived CLI
append can proceed. A persistent external writer still excludes these readers
under wire 0.1. Follow validates on reopen and has no large-history latency
claim; embedded concurrent applications should use the healthy Collector API.
Pages contain at most 1,024 records and 64 MiB of original data; output encodes
binary data as hex. An incomplete tail is reported separately in page metadata.

`init` uses the explicit Event profile. Existing `create`, `put`, `get`, `delete`
and `dump` retain their KV compatibility semantics; Event commands refuse KV
files without reinterpretation. Generic export/archive work on either profile.
See [Event API and profile](../../docs/EVENT_DATABASE.md).

## Install from source

```sh
cargo install --locked --path crates/ojo-tools
# Raw files only, without Zstandard or a C compiler:
cargo install --locked --path crates/ojo-tools --no-default-features
```

The published preview can also be installed by HTTPS or from its immutable
source tag:

```sh
curl -fsSL https://raw.githubusercontent.com/oojuro/oojuro/ojo-v0.1.0/scripts/install-ojo.sh | sh
cargo install --locked --git https://github.com/oojuro/oojuro \
  --tag ojo-v0.1.0 --bin ojo ojo-tools
```

Homebrew uses this repository's formula and the same checksummed binary package:

```sh
brew tap oojuro/engine https://github.com/oojuro/oojuro
brew install oojuro/engine/ojo
```

Rust 1.89+; Linux is the qualified platform. Archive support is enabled for the
normal tool build and needs a C compiler. The default engine library remains
raw-only and retains its four direct runtime dependencies.

Versioned binaries are packaged with their full notices, dependency inventory,
source revision and SHA-256 checksums. `scripts/install-ojo.sh` installs those
release packages into `~/.local/bin` by default. `OJO_INSTALL_DIR` selects another
binary directory and `OJO_VERSION` selects a published version. The Linux x86-64
package requires glibc 2.34 or newer. The installer refuses unsupported platforms;
source installation remains available where the engine is supported.

## Preserve original bytes

```sh
ojo keygen private.key
ojo create session.juro --key-file private.key
printf 'hello\r\n' > original.bin
request_id=$(ojo request-id)
ojo put session.juro 6d657373616765 original.bin \
  --request "$request_id" --key-file private.key
ojo get session.juro 6d657373616765 --key-file private.key --output restored.bin
cmp original.bin restored.bin
ojo verify session.juro --key-file private.key
```

Keys are hexadecimal, so arbitrary binary keys do not depend on terminal text
encoding. `get` writes exact original bytes to stdout or a new output file.
`put` accepts a file or `-` for stdin, up to the 16 MiB wire value limit.
An empty file stores an empty value. `delete` writes a tombstone, preserving
history. Keys remain subject to the engine's wire and index admission limits.

Retain the request ID until success is certain. Repeating the exact write with
that ID returns its original receipt; changing its input conflicts. Values and
request IDs are validated before opening the database for writing. Writable
opens may recover an incomplete tail and report the recovered byte count.
Every successful new commit is flushed before its receipt is printed.

Generated keys are separate private 32-byte files, created without overwriting
anything. Unix key-file group/other access is refused. Key contents never appear
in status JSON; keep the key separately from any exported database.

## Inspect, recover and move

```sh
ojo inspect session.juro --key-file private.key
ojo dump session.juro --key-file private.key > history.jsonl
ojo export session.juro portable-copy.juro --key-file private.key
ojo archive session.juro compressed-copy.juro --key-file private.key
```

`inspect` validates the committed history and reports its checkpoint, historical
operation/value-byte counts and any incomplete trailing bytes. `verify` refuses
an incomplete tail. `recover` opens exclusively and truncates only an incomplete
trailing transaction; complete corruption is an error. No command silently
repairs complete corruption.

`dump` is a readable JSONL view of **all historical operations**. Binary keys and
values use hex; a tombstone has `value_hex: null`, while an empty value has `""`.
This is a diagnostic view, not a replacement serialization of transaction IDs.

`export` creates a complete binary copy, validates its exact source checkpoint,
syncs it and atomically publishes a new destination. It retains file identity,
all original bytes, encryption and retry IDs. A copied `.juro` can be opened
directly by another compatible reader; no import conversion is needed.
`archive` creates a separate compressed file with a fresh file identity and
preserves the source unchanged. Neither operation overwrites an existing
destination. Checkpoints from the original do not identify the archive.

The specification remains experimental. See [engine guarantees](../ojo/README.md)
and the [standalone format specification](../../spec/0.1/FORMAT.md).
