# The `.ojo` file extension

**Decision:** `.ojo` is the canonical extension for a future standalone Oojuro
database file. Use lowercase, for example `memory.ojo`. The product and
command names remain Oojuro and `oojuro`.

**Status:** experimental standalone wire 0.1 is implemented by the minimal
[`ojo` crate](../crates/ojo/README.md). The [specification](../spec/0.1/FORMAT.md)
defines direct writes, locks, synced commits, recovery, integrity, bounds,
encryption and exact retries. This is not a stable-format promise.

## Current storage

Oojuro currently opens a journal directory containing several files. The
[compressed store layout](../crates/oojuro-store/src/store.rs) includes:

| Name | Purpose |
|---|---|
| `MANIFEST` | Lists the live storage files and dictionaries |
| `hot-<id>.ojl` | Append log of compressed record batches |
| `cold-<id>.ojc` | Compacted compressed segments |
| `dict-<id>.ojd` | Compression dictionaries |
| `LOCK` | Coordinates ownership of the directory |

These files are components of a store. Renaming one to `.ojo` does not make it a
complete standalone database. Existing directory paths and component extensions
continue to work as before.

The [Journal application's JSONL export](https://github.com/oojuro/o-journal/blob/main/docs/FRONT_DOOR.md#surface) remains JSONL and should use
`.jsonl`. JSONL, a JSON export, or an arbitrary archive renamed to `.ojo` is not a
defined Oojuro container. The `~/.oojuro/` application directory and
`.oojuro.toml` configuration files retain their existing names.

## Standalone files

The standalone implementation writes one regular `.ojo` file, appending atomic
transactions without a sidecar WAL or persistent index. It is independent of the
historical directory libraries. Normal writes are raw. Optional archives are
separate compressed files with fresh identities; originals are preserved.

The specification is authoritative; see its frozen interoperability vectors and
`crates/ojo/tests` for compatibility, partial writes, truncation and corruption.
