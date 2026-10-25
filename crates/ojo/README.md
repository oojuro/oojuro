# Minimal .ojo engine

A fresh implementation of the experimental [wire 0.1 specification](../../spec/0.1/FORMAT.md).
One synchronous Rust library. One file. Opaque byte keys and values. Four direct
runtime dependencies: SHA-256, CRC32, authenticated encryption and OS randomness.
It does not depend on the older Oojuro crates. Those remain available to existing consumers.

```rust
use ojo::{Change, Database};
# fn main() -> ojo::Result<()> {
# let root = std::env::temp_dir().join(format!("ojo-example-{:032x}",u128::from_le_bytes(ojo::request_id()?)));
# std::fs::create_dir(&root)?;
let key = ojo::generate_key()?; // retain securely outside the database
let path = std::path::Path::new("session.ojo");
# let path = root.join(path);
let mut db = Database::create(&path, Some(&key))?;
let request = ojo::request_id()?; // retain this ID until success is certain
db.commit(request, &[Change {
    key: b"2026-10-24T12:00:00Z/0001",
    value: Some(br#"{"tool":"read_file","result":"hello"}"#),
}])?;
assert_eq!(db.get(b"2026-10-24T12:00:00Z/0001")?.unwrap(),
    br#"{"tool":"read_file","result":"hello"}"#);
# drop(db);
# std::fs::remove_dir_all(root)?;
# Ok(()) }
```

Timestamps, tool names, schemas and session status belong to the caller. UTF-8
text is a normal byte value; binary attachments also round-trip exactly. No text
conversion, JSON parsing, network runtime, search/ranking, background worker or
agent integration runs inside this library.

## Guarantees

- A transaction contains 1–1,024 puts/deletes. All become visible together.
- Creation syncs the header and its parent directory. Every public commit calls
  `sync_all` before returning success; there is no unsafe/no-sync mode.
- Readers hold shared inode locks. A writer holds an exclusive lock for its
  lifetime. Opens refuse contention; readers and writers cannot coexist.
- Open verifies every complete transaction and rebuilds the in-memory key and
  retry indexes. Writable recovery truncates only a verified incomplete tail.
  Complete corruption is an error and never silently repaired.
- Keep a request ID across uncertain acknowledgements. An identical retry
  returns its original receipt, including after reopening; changed input conflicts.
  A write/sync error poisons the handle: reopen, recover and retry that ID.
  After open, the first retry of an existing transaction also reads that transaction
  to derive its retry fingerprint; later retries reuse that derived fingerprint.
- Open and scans verify complete transactions. Unencrypted raw point reads seek
  directly to the selected value and verify its SHA-256 against the trusted
  in-memory index built at commit/open. They detect changes to that value;
  unrelated bytes are checked on scan/reopen. Encrypted/compressed point reads
  verify and decode the containing transaction. Scans expose all versions in
  commit/operation order. Tombstones hide current values; they do not physically
  erase earlier content.
- Optional per-file XChaCha20-Poly1305 encryption authenticates the headers and
  body, with a fresh random nonce per transaction. Keys are caller-supplied;
  missing/wrong keys and plaintext downgrades are refused.

ACID here means atomic batches, validated file invariants, serial isolation via
locks, and durable acknowledgements on local filesystems honoring `fsync` and
inode locks. It does not provide SQL constraints, concurrent reader snapshots,
distributed transactions or replication. Checksums are not signatures.

## Compression and limits

Normal `commit` always writes raw bytes, even with the `archive` feature enabled.
The default build contains no compression library; compressed transactions return
`Unsupported`. Enable `archive` to read both wire codecs and call
`db.archive_to("session.archive.ojo")`. This creates a separate, atomically
published file, preserves encryption/history/retry IDs, and leaves the original
unchanged. The archive has a new file ID and physical checkpoints. Small or
incompressible bodies stay raw. Keeping both files consumes extra disk space.
Archive transactions are assembled in an unpublished temporary file, then all
contents are synced before its final name is published and the directory synced.
Public commits continue to sync individually. A failed final data sync prevents
archive publication.

The local/cloud policy is automatic archiving with an opt-out, after an explicitly
finished session has been idle for 24 hours. The caller in ojo-journal/ojo-cloud
owns that timer; resumed activity resets it. The storage engine has no session
state or scheduler. This crate supplies the archive operation, not that automation.

`scan_prefix(prefix, after, limit, callback)` visits only the latest live values
under a byte prefix, in bytewise key order, with an exclusive continuation key.
Pages contain at most 1,024 values; empty values remain visible and tombstones
are omitted. The callback borrows one selected value at a time and can return an
error to stop immediately. Adjacent selected values in an encoded transaction
share one verified decode within the call. No decoded values are retained between
calls. Raw values retain their selected-value SHA-256 checks.

The first prefix call builds an additional in-memory ordered index of live keys;
ordinary point reads do not allocate it. Later pages use bounded range selection,
and commits keep it current. Its key storage is additional to the ordinary index.
The continuation key is a range bound, not a frozen snapshot across writes or
reopens. Applications can use immutable monotonic keys and retained checkpoints
for resumable exports. The handle keeps its existing inode lock until dropped;
closing it before network work permits a writer, but reopening still validates
the complete history.

With `archive`, each handle lazily reuses independent Zstandard codec contexts.
Every transaction retains its own complete frame with the same format, level
and decoder limits. This reuses codec workspace, not source values, authority
decisions or an external dictionary. The default build still has no codec library.

Wire bounds are 64 KiB keys, 16 MiB values, 32 MiB decoded batches. This implementation
also refuses more than 10 million operations or 256 MiB cumulative key bytes;
these are index admission limits, not wire-format limits. Memory grows with unique
keys and request IDs, and reopen validates the complete history. A long-lived
handle avoids that reopen cost. No stable-format or universal performance claim
is made for this experimental implementation.

## Build, test and measure

```sh
cargo test --locked -p ojo
cargo test --locked -p ojo --features archive
cargo run --locked --release -p ojo --features bench --example acid_bench -- \
  ojo /persistent-disk/new-ojo-run 4096 64 1024
cargo run --locked --release -p ojo --features bench --example acid_bench -- \
  sqlite /persistent-disk/new-sqlite-run 4096 64 1024
```

Rust 1.89+; Linux/local filesystems. Only `archive` (Zstandard) and `bench`
(bundled SQLite) require a C compiler. The benchmark uses identical byte keys,
values and transaction sizes, verifies all returned bytes, and reports commit
latencies, throughput, warm reads, scan and reopen separately. SQLite uses WAL,
FULL synchronous, a prepared statement and a WITHOUT ROWID key table. It lacks
Ojo's persistent retry IDs and per-read value digest, so the operations
have matched durability but different validation/history contracts. The
`sqlite-rowid` arm also measures an integer primary key and a 64 MiB page cache
for applications that can use ordinal keys. Use persistent
disk; tmpfs numbers cannot establish durable device performance.

See the [measured results and limitations](../../docs/benchmarks/OJO_MINIMAL_ACID.md).
The [optimization qualification](../../docs/benchmarks/OJO_IO_OPTIMIZATION.md)
compares the same raw workload before/after, plus encrypted reads and archive
creation. It includes first-retry costs and cases that remain limited by disk sync.
