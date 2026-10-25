# General Event database foundation

The experimental `ojo::events` API stores engine acceptance time and exact data.
The engine generates timestamps, cursors and retry bookkeeping. Agent/provider/
session conventions belong in the application's data. This source addition is
separate from the published `ojo-v0.1.0` KV preview; that immutable release does
not expose the Event API.

```rust
use ojo::events::{Collector, Journal};
use std::time::Duration;

# fn example(path: &std::path::Path) -> ojo::Result<()> {
let mut journal = Journal::create(path, None)?;
let receipt = journal.append(b"sensor=alpha temperature=21.5\n")?;
let original = journal.get(receipt.cursor)?.unwrap();
assert_eq!(original.data, b"sensor=alpha temperature=21.5\n");

let collector = Collector::new(journal);
let next = collector.append(b"sensor=alpha temperature=21.8\n")?;
let (events, page) = collector.follow(
    Some(receipt.cursor), 100, Duration::from_secs(1),
)?;
assert_eq!(events[0].cursor, next.cursor);
assert_eq!(page.cursor, Some(next.cursor));
# Ok(())
# }
```

`append(data)` uses the engine clock and acknowledges after file sync. Its
`accepted_at` is signed UTC Unix microseconds. Clocks may repeat or move backward;
`scan` and `follow` always use append positions. `scan_time` reads [from,to)
ordered by `(accepted_at, append_position)`. A cursor is opaque retrieval
metadata, not a required logical field or a caller-selected key.

For a write whose acknowledgement may be lost, call `prepare(data)`, retain
its opaque `AppendIntent::to_bytes()` alongside the exact data, then call
`append_prepared(&intent,data)`. Preparation assigns the engine acceptance time
but is not durable acknowledgement. After an uncertain I/O error, drop the
poisoned handle, reopen the same journal, restore the retained intent and retry.
An already committed request returns the original cursor/time without an
additional event; a changed original conflicts. Independent identical appends
remain distinct. Retry state is engine bookkeeping, not an application schema.

The [separately versioned profile specification](../spec/events/1/FORMAT.md)
defines the marker, exact bytes, cursors and optional retry-intent encoding.
Existing KV files are refused before any writable recovery. There is no silent
conversion of old records into events with fabricated historical acceptance
times. Applications migrating old data must preserve a recovery artifact and
label new acceptance times as migration acceptance, while retaining original
source times in their own data.

The healthy collector owns one underlying handle. Local append/read/follow calls
share it; followers release its mutex while waiting. Wire 0.1 still excludes
external read handles while a writer is open. Cross-process applications must
route through their owning process. At most 1,024 records and 64 MiB of originals
leave a collector follow call; individual data is at most 16 MiB. Callback scans
stream originals one at a time. A lazy timestamp index contains metadata only.
Reopen still verifies the complete history and retains in-memory bookkeeping;
these limits are not million-record qualification.

The complete product scope also includes indexed lexical/full-text search.
The Event storage/follow foundation alone does not establish that capability.
The neutral engine disk projection, Event search adapter, generic command-line
exposure, scale measurements and consumer adoption are tracked by
[issue #106](https://github.com/oojuro/oojuro/issues/106). No legacy search or KV
benchmark may stand in for Event-path evidence.

The [generic file tool](../crates/ojo-tools/README.md) also exposes `init`,
`append`, `read`, `scan` and a bounded `follow` page. Existing KV commands retain
their meanings. CLI follow drops read handles between snapshots; a persistent
writer uses the embedded collector or an application-owned service instead.

## Independent non-agent example

```sh
cargo run --locked -p ojo --example sensor_journal -- new-sensors.juro
python3 examples/read_events.py new-sensors.juro
python3 scripts/test_event_reader.py
```

Both programs expose the same timestamps, exact binary data and portable
cursors. The Python reader imports no engine code or application schema and
explicitly supports only raw plaintext files within its documented resource
budget. Rust encrypted and compressed behavior has separate tests. This is
experimental compatibility evidence, not a stable-format or hosted-product claim.

`event_bench` separately measures durable single-event writes, exact warm gets,
verified scans, reopen and archive creation. SQLite uses a timestamp/BLOB table,
WAL and FULL synchronous with a 64-MiB page cache. That SQLite arm does not
retain persistent retry identities or perform Ojo's per-read original checksum;
the result therefore compares a documented baseline with different guarantees.
It measures neither full-text search nor concurrent hosted operations. Always
run on persistent storage with filesystem compression disclosed.

The [initial Event measurements](benchmarks/OJO_EVENT_FOUNDATION.md) publish
all twelve small raw/encrypted/SQLite runs, archived-original checks and their
limits. They include slower Event reopen and mixed encrypted-read results;
none is a full-text-search or hosted scale claim.
