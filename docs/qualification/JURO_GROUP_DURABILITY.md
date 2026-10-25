# Event group durability qualification

`Journal::append_prepared_batch` and the embedded collector share one final
file/publication barrier across up to1,024 retained intents and64MiB of data.
Every event retains its own existing profile frame, timestamp, nonce and cursor;
new writes remain raw even with archive support. No wire/profile/version or
frozen-vector change is required.

All input, exact existing retry originals, intra-group nonce conflicts and
aggregate new-event metadata admission are checked before writing. Failure after
writing starts poisons the handle. The final `confirm` covers an unproven replayed
prefix and pending parent-directory publication even for duplicate-only groups.
An already confirmed unchanged retry performs no additional sync.

This is not a rollback or atomic-visibility boundary. An interruption can leave
any complete prefix visible after recovery. Retain every intent and original;
retrying the full group preserves surviving cursor/time and appends only missing
events. An application can encode transaction markers and its visibility policy
inside its own data. The core does not interpret those markers.

## Correctness evidence

Default and archive core unit/ACID/wire/Event/doc tests pass, as do strict
all-target Clippy in both feature modes. New checks cover invalid later input,
changed timestamps under one nonce, count/aggregate-byte and near-capacity
refusal before writes, empty/binary/full16MiB originals, duplicate-only groups,
partial-group recovery, stable receipts and read-only refusal.

The opt-in actual Linux fsync-EIO runner passes20 cases across both feature
builds:12 previous file/directory/alias cases and8 group file/directory cases.
Group file traces `FFSS` show failed initial and duplicate-replay confirmations,
then successful retry and one final sync for two new events. Directory traces
`SSSFFS` include three healthy creation barriers, two refused confirmations and
one successful publication barrier; later healthy groups do not resync it.
No failure receives a receipt; exact replay retries preserve original bytes.

## Local write comparison

An optimized build measured100 x1KiB pre-prepared events on persistent Btrfs,
with NOCOMPRESS checked on every file and the shared build/timing lock held.
Three rounds alternate individual/group ordering for each encryption mode.
Median whole100-event elapsed times, including the required durability barrier:

| Mode | Individual durable appends | One durable group |
| --- | ---: | ---: |
| Plaintext | 837.67ms | 11.13ms |
| Authenticated encryption | 836.53ms | 9.36ms |

Preparation occurs before timing; exact readback and reopen validation occur
after it. All100 group receipts become successful together after the final
barrier; this is not a per-event latency measurement. Each mode keeps the same
file byte size between individual/group runs. Small local cohorts do not prove
SQLite superiority, compression performance, scale or hosted latency. All owned
benchmark datasets were removed after the process completed.

[Machine-readable commands, runs and source hashes](juro-group-durability-2026-10-25.json).

```sh
cargo test --locked -p ojo
cargo test --locked -p ojo --features archive
python3 scripts/verify-ojo-sync-retry.py
python3 scripts/verify-ojo-sync-retry.py --features archive
# For the ignored write comparison, provide an owned persistent directory with
# chattr +m before running the release group_retry comparison test.
```
