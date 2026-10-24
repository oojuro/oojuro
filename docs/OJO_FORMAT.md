# The `.ojo` file extension

**Decision:** `.ojo` is the canonical extension for a future standalone Oojuro
database file. Use lowercase, for example `memory.ojo`. The product and
command names remain Oojuro and `oojuro`.

**Status:** the name is adopted; the standalone container is not implemented.
This document records the naming decision, not a released binary format or a
promise of compatibility with future versions.

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

## Standalone container work

A future implementation will use `.ojo` for its user-facing database files.
Before adding commands that produce or consume those files, its format and
reader/writer contract must define:

- Whether a file is a portable snapshot or a database that supports direct
  writes, including any required companion files.
- A recognizable header, an explicit format version, and rejection of
  unsupported versions independently of the filename.
- The contents required to reopen a complete database, including dictionaries,
  metadata, and the treatment of rebuildable indexes.
- Integrity checks, resource bounds, and behavior on truncated or corrupt input.
- Snapshot consistency and safe publication of a completed file; locking and
  recovery rules if direct writes are supported.
- Encryption and key handling when the source journal is encrypted.

That implementation needs synthetic round-trip and corruption tests. Adopting
the extension does not select snapshot versus direct-write semantics, freeze a
header or version, change the current storage layout, or add `.ojo` CLI support.
