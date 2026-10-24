# Native Record append preparation lifetimes

Draft runtime `cd46c17f36b176d75a0587df035a4266abe6fbb0`, based on the separate borrowed-text ownership topic `d6b1f94`. Exactly two explicit drops in RecordLog::append release temporary ownership earlier:

- Drop the unsigned canonical JSON Vec after chain() returns its owned digest, before watermark construction and signed-batch serialization.
- Drop the owned Batch after its complete bounded encoded JSON is produced, before capacity checks, Store append and replay decode another Batch.

Batch/Input contain ordinary owned data, with no custom destructor. The caller's borrowed inputs stay intact. Serialization, fingerprint/digest, ID/clock assignment, capacity/error order, append sync, fail/poison hooks, replay validation and original ACK lookup are unchanged. Replay/open code, formats, crypto, public API, features and dependencies are unchanged. An encoded serialization failure still drops ownership through the original error path.

## Historical limitation retained

The separate23October `d204189` experiment dropped canonical Vecs in append AND replay. It reduced synthetic large-write RSS, but repeated normal read/search and large-write latency were mixed/adverse, and it was rejected as a default recommendation. This is a different two-line candidate: append canonical plus owned Batch; replay is unchanged. That historical report is included in this archive. The base's borrowed-text actual CLI peak result was neutral; it is not presented as a performance gain.

## Actual production-path qualification

Fresh actual normal engine, native CLI and engine libtest: actual Rust1.99.1, portable x86-64 CC/linker, DEBUGopt0/features[], offline/locked. Six exact existing API selectors passed1/0/0 each: original ACK/reopen/typed retry; full typed intent/complete JSON/bounds; overflow and no-clock retry; actual SIGKILL after Store/before ACK and after ACK; sealed incomplete/extended receipt refusal; and durable-boundary panic/poison/rollback. No test merely checks that drop statements exist. Independent32 observed identities/cgroup/scratch were absent. This is not a rerun of every unchanged storage suite.

Frozen CLI `1aaf67d50329c2be09f742cd79584750a9b9e0c89e96a4a808b407323a8c10e8` (16,577,032B); libtest `b7c512ce20d57712f78a246f579bc0cd70564a49b23bf7bca4985b8a21064e60`.

## Fixed DEBUG append allocation case

Four fixed AB/BA/AB/BA rounds/eight fresh stores. Both arms use the same32-record corpus and four batches of8:31 large131,072-NUL originals and one short original. Control is the already qualified `d6b1f94` CLI `71754eef...`; only the two drops differ in this comparison. Each measured actual record-append process includes ordinary log reopen, input parse, durable append and replay. There are32 measured invocations; all per-batch raw RSS/CPU/elapsed metrics are retained.

Outside timing, every store verifies32 full Record fields against fixed inputs and its original ACK-assigned clocks, four exact original ACK retries without changing the closed store, and a separate closed copy reopened with all32 fields equal. Independently assigned clocks and ciphertext/nonces are not required to match across different stores. Complete file/directory apparent/allocated inventories and before/after hashes are retained; template stays unchanged. Independent119 unique observed identities and33 actual/named cgroups were absent.

| Round | Order | Maximum CLI RSS across four appends, KiB before → after | Sum of four CLI elapsed seconds before → after |
| --- | --- | ---: | ---: |
| 0 | before/after | 55,824 → 47,224 | 13.085073 → 13.183387 |
| 1 | after/before | 56,152 → 49,212 | 13.132015 → 13.130251 |
| 2 | before/after | 56,296 → 47,312 | 13.181185 → 13.132130 |
| 3 | after/before | 56,688 → 47,516 | 13.131101 → 13.133513 |

Median of the four per-arm maximum RSS values56,224→47,414KiB is15.669% lower in this exact DEBUG escape-heavy append case. All four pairs use less peak RSS. Elapsed medians13.131558→13.132822s are effectively neutral/slightly worse (+0.0096%); two paired totals are adverse. There is no latency-improvement or tail claim.

Helper+CLI charged cgroup medians54,827,008→46,020,608B are separate accounting, not RSS-equivalent. An arm maximum is the maximum of four separate CLI processes, not in-process steady heap; elapsed is the sum of their complete invocations. Shared/page-cache charging and allocator behavior remain limitations. This does not establish release performance, ordinary small-batch behavior, general search/hosted/storage/cost improvements or a global memory cap. No favorable rerun, changed corpus or fallback occurred.

[Safe evidence](benchmarks/native-record-append-lifetimes-2026-10-24/README.md) includes every32-batch metric, all paired outcomes, source/build/toolchain pins, six raw test terminals, raw ACK metadata, full closed inventories and independent proofs. No ELF, key, store, original input/context payload, corpus or private hosted code is published. This draft authorizes no merge/release/deployment.
