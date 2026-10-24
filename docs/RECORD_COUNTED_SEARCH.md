# Count native Record search output without retaining a discarded encoding

## Change and actual callers

Runtime09b0fbd943625a031698fb52cd574b006b31786a changes exactly three size checks: ordinary `record::Index::search_view`, ranked fallback/base, and enabled ranked result. They use the existing `json_size` counting mode instead of constructing a discarded `json` Vec. Both modes call the same bounded writer and `serde_json::to_writer`; escaped values, exact byte limits, failure mapping and check order remain unchanged. The structured owned result and final required output encoding remain.

This is real production core API code used by opt-in `oojuro-native record-search` and `record-context`, plus `PersistentIndex` and ranked native API callers. It is not the benchmark projection adapter or hosted `record::projection::Index`, and does not change the legacy CLI. No new dependency, codec, format, append/recovery, authorization, cache or ranking change.

## Reproducible source and qualification

Control: accepted84 `7261a55d9230596470b2f64aeed4175fbe358aba`. Candidate runtime:09b0; test-only correction:`631c19cd1ecfe9ecc7ee99379c8ef9e5c49896ce`. No neutral traversal/offset/projection experiment is stacked. Both real `oojuro-native` binaries were built fresh (`fresh:false`) in separate targets with actual Rust1.99.1, x86_64-unknown-linux-gnu, explicit portable CC/CXX/linker and `RUSTFLAGS=-C target-cpu=x86-64`, no default features, DEBUG opt0/no debuginfo. Actual toolchain hashes/features/profile/source/ELF metadata are in the evidence receipts.

| Frozen CLI | SHA-256 |
|---|---|
| Control | `42d1d4ba03c921d584b7ad54de1365d3fabbe472067c0c859965a3965dc78eb7` |
| Candidate | `cd270843f75412ce77ac70ff13296d7aeb3a1ad4197e56a8e3051dafca8f2339` |

Six DISTINCT checks passed across two libtests: exact escaped large-original Search budgets; existing counted JSON bounds; snapshot/scope/cursor/output bounds; corrected ranked receipt/delta/full-original exact budgets; off/all-skipped ranked parity; actual native context whole-original/estimate behavior. Each passed1/failed0/ignored0. Three initial checks passed, then the new ranked test failed its incorrect expectation that disabled ranking emits stages. The test-only correction requires empty off receipts/deltas and no plugin calls, exact on-stage/delta count. Corrected ranked plus two previously unrun checks then passed. Preserve this original assertion failure; the normal CLIs were not rebuilt after a test-only change. No unrelated storage/crash matrix was repeated because those paths are byte-identical.

## Fixed allocation case and measured results

Thirty-two synthetic records, intentionally appended through the real baseline CLI in four8-row batches:31 originals contain `needle ` followed by131,072NUL bytes, and the final short original is `needle compact exact context`. Total original text4,063,477B. Before timing, one baseline Search with `needle compact`,k32 and32MiB output bound proved all32full original records/fields remain in Search, even though31large scores are negative. Its encoded output was24,391,653B; only a hash/count/length was retained. The short result is nonnegative and eligible at the unchanged minbits0.

Actual measured command: `record-context --query 'needle compact' --measure chars4-estimate --k 32 --top-k 1 --tokens 2500 --context-bytes 65536 --min-bits 0 --turn-start-us 9223372036854775807 --format claude-hook --bytes 65536` (options and values are separate argv entries in the archived source).

Four predeclared AB/BA/AB/BA rounds, eight fresh closed-store copies. One full32-record range page warms each arm identically; all32full original fields/IDs and semantic watermark fields are checked before/after. The actual core validates prefix AEAD; fresh randomized commitment bytes are not treated as deterministic. Exact encrypted-store inventories remain unchanged. Every arm emits the same nonempty922B hook output containing the one complete short original, SHA-256 `98d49580f145ebff62a8666bcdaf1f295bc901564c1c90d04bdfea5023f9e871`.

| Round/order | CLI maximum RSS KiB, before → after | Whole-scope charged peak B, before → after | Whole DEBUG CLI wall s, before → after |
|---|---:|---:|---:|
| 0 (AB) | 58,560 → 34,308 | 56,328,192 → 32,665,600 | 3.870764 → 3.871011 |
| 1 (BA) | 57,980 → 34,628 | 57,790,464 → 32,681,984 | 4.071710 → 3.871251 |
| 2 (AB) | 57,568 → 34,584 | 56,336,384 → 32,751,616 | 4.021395 → 3.870912 |
| 3 (BA) | 58,056 → 34,760 | 56,573,952 → 32,632,832 | 3.970921 → 3.821446 |

Median CLI peak RSS:58,018→34,606KiB,40.353% lower in THIS case; every paired peak was lower. This is maximum process RSS, not retained-heap accounting. Whole-scope charged peak median56,455,168→32,673,792B is a DIFFERENT measurement: helper+CLI cgroup charge, with shared-file charging differences. It is not RSS-equivalent and need not numerically exceed process RSS. Wall time is included for complete disclosure only; no release speed/latency claim follows from DEBUG timings.

Each measured sibling scope:3GiB/zero swap/512tasks/120s. Outer controller:6GiB/zero swap/512tasks/900s,780s work; independent reconciliation100s, CORE0/control-group kill/5s stop. Outer cap does not aggregate sibling scopes. Actual live unit properties were saved before assertions. Root independently rehashed both CLIs, all nine source/copy before/after inventories, exact output hashes and verified42observed process identities plus nine actual/named cgroups absent. No raw enumerated PID was signalled; failures retain their data.

## Earlier attempts retained

1. Root intentionally stopped context1 before any measured scope: repeated full-log get subprocesses projected verification beyond the fixed work budget. Worker receipt remains interrupted/running; independent cleanup is separate. Zero paired results.
2. Context2 failed before any measured scope by comparing randomized prefix commitment bytes. Actual core AEAD/prefix validation stayed unchanged; later oracle compares exact exposed semantic fields and bounds the fresh commitment. Zero paired results.
3. Context3's first baseline CLI completed but emitted empty additionalContext. Universal `needle` gives the four-token short record a negative score; the unchanged minbits0 properly excluded it. The nonempty oracle failed. One baseline measured scope, no candidate/complete pair, no performance claim. No empty context was accepted.
4. Distinct fixed context4 changed only query to `needle compact`, added the baseline all32-original Search preflight, retained texts/sample/caps/eight-copy schedule/nonempty output requirements, and completed all eight arms. No candidate outcome existed before correcting that fixture eligibility.

## Scope and evidence

This is a warm-OS, escape-heavy original→small-context DEBUG allocation diagnostic. The ordinary native context test separately qualifies common semantics; these measurements do not show ordinary small-result savings. Final `record-search` still needs a large output Vec and may reuse the freed validation allocation, so the same peak reduction is not promised for it. No release performance, tail/throughput, cold-start, larger-than-RAM, hosted, write/storage or AWS-cost gain is claimed. Source-only duplicate-original ownership findings are a separate future proposal, not bundled runtime changes.

[Compact evidence archive, manifest and redaction rules](benchmarks/record-counted-search-2026-10-24/README.md); [all eight metrics and limits](benchmarks/record-counted-search-2026-10-24/summary.json). Binaries, keys, fixture stores and large payloads stay outside Git. Local coordinator review of this archive is required before push; no main merge/package release/deployment is authorized.
