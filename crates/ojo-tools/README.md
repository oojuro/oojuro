# ojo file tools

`ojo` creates, inspects, verifies and manipulates standalone `.ojo` databases.
It uses the same Rust engine and independently versioned wire 0.1 as applications.
Agent capture, session conventions and integrations live in ojo-journal.

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
ojo create session.ojo --key-file private.key
printf 'hello\r\n' > original.bin
request_id=$(ojo request-id)
ojo put session.ojo 6d657373616765 original.bin \
  --request "$request_id" --key-file private.key
ojo get session.ojo 6d657373616765 --key-file private.key --output restored.bin
cmp original.bin restored.bin
ojo verify session.ojo --key-file private.key
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
ojo inspect session.ojo --key-file private.key
ojo dump session.ojo --key-file private.key > history.jsonl
ojo export session.ojo portable-copy.ojo --key-file private.key
ojo archive session.ojo compressed-copy.ojo --key-file private.key
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
all original bytes, encryption and retry IDs. A copied `.ojo` can be opened
directly by another compatible reader; no import conversion is needed.
`archive` creates a separate compressed file with a fresh file identity and
preserves the source unchanged. Neither operation overwrites an existing
destination. Checkpoints from the original do not identify the archive.

The specification remains experimental. See [engine guarantees](../ojo/README.md)
and the [standalone format specification](../../spec/0.1/FORMAT.md).
