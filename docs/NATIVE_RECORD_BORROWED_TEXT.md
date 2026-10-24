# Native Record original-text ownership

Draft ownership change, with **no meaningful peak-RSS improvement** in the qualified CLI case. Runtime `d6b1f94a0b39a529ceeff1dc7d198d4e0132315e` is based on the counted-output topic/PR86 (`319174d`, runtime `09b0fbd`). No dependency, default feature, public API, format, scorer arithmetic, authority or storage change.

## Change and proof boundaries

RecordLog owns the original Assigned.input.text. Returned public Record/Search hits still own complete original text and metadata. Native Index rebuild and PersistentIndex load/extend stop copying originals into their internal lexical Event carriers. Tokenization, event IDs/times/indexed flags, length/scope/postings and ordering stay unchanged.

A private object-safe OriginalTextSource resolves a short-lived row plus expected original ID through the current RecordLog. Existing current watermark, scope, as-of and visibility checks still precede scoring. Both literal checks and native cache hydration fail closed if the source is absent, the row is missing or its ID differs. This is not a persistent callback, pointer vector, full-text cache or authorization cache.

Capture constructors/segment hydration retain text. Projection/paged builders also retain their sole original carrier; projection clears its input Row text after construction. Native cache hydration checks the actual original's empty/nonempty state, row identity, term counts/length/scope and indexed eligibility. Empty carrier Strings are never evidence of original emptiness. Persistent format/AAD/manifest/fallback behavior is unchanged.

Three new tests cover nonzero first ID9; large escaped/Unicode, empty/punctuation/stored-only/key-replaced originals; two-clock/filter/query variants; full Records and score bits against a retained constructor before/after reopen; PersistentIndex rebuild/load/tail/merge/load; and missing/wrong hydration source refusal. Native carrier text length AND capacity are zero. That proves removed owned String payload in this type, **not measured steady-state RSS**, allocator overhead or a process memory cap.

## Build and correctness history

The first canonical attempt at `7f7730c` failed with six lifetime errors before producing any CLI/libtest or running a selector. It is retained with its raw errors and independent cleanup. The private accessor was then changed from an invariant Fn alias to OriginalTextSource::text(&self,row,expected_id), returning a borrow tied to its source. No ownership mode, score, format or validation was weakened.

A separately approved continuation built fresh normal engine, actual native CLI and engine libtest with actual Rust1.99.1, portable x86-64 CC/linker, DEBUGopt0, features[], offline/locked. All16 exact selected tests passed1/0/0 each: the3new cases plus retained native/Capture oracle, cache, projection and ranking consumers. The successful build's92 observed identities, exact cgroup and scratch were independently absent; the failed build's137 identities/cgroup/scratch were also checked. No broad unchanged storage suite was repeated.

Frozen CLI: `71754eef61c9fbdcc36ede784c873ffe15a96350b44c413d4d4bfba0240ce8c6` (16,576,632B). Frozen libtest: `6425f8e7e9bb33444f1a8a80d98798499d976b2c8e031cf1e2c51c25065ce2fa`.

## Actual CLI allocation result: neutral peak memory

Control is the already qualified counted-output CLI `cd270843...` / runtime `09b0fbd`, so counted serialization is in BOTH arms. Four fixed AB/BA/AB/BA rounds, eight independent closed-store copies, same32 synthetic originals (31 large NUL-containing originals plus one short original), query `needle compact`, k32, one full-page warming operation and one small nonempty context per invocation. Full baseline Search proves all32 full originals; every source/copy verifies all32 Record fields and semantic prefix fields with bounded fresh seals. The public CLI validates randomized AEAD commitments; their fresh bytes are not compared. Within this campaign all eight outputs are exactly922B and byte-identical. All source/copy inventories stayed unchanged. Independent40 observed identities and nine cgroups were absent.

| Round | Order | CLI peak RSS KiB before → after | Complete CLI seconds before → after |
| --- | --- | ---: | ---: |
| 0 | before/after | 33,452 → 33,936 | 3.771714 → 3.771763 |
| 1 | after/before | 33,900 → 33,900 | 3.821231 → 3.771523 |
| 2 | before/after | 34,036 → 33,816 | 3.874271 → 3.770321 |
| 3 | after/before | 34,052 → 33,764 | 3.821520 → 3.770432 |

Median peak RSS33,968→33,858KiB is about0.324% lower and not a meaningful peak-memory gain. Round0 is adverse and round1 equal. Helper+CLI charged cgroup median32,571,392→32,696,320B is slightly higher and is separate accounting, not RSS-equivalent. Elapsed medians3.82138→3.77098s do not establish a release-speed improvement.

The actual CLI first replays RecordLog, then constructs Index and owned Search hits. Earlier decode/canonical buffers can determine process high-water before this carrier exists. Long-lived API users remove the duplicated type payload, but this campaign did not measure their retained RSS. No release, ordinary small-result, cold-OS, larger-than-RAM, hosted, storage or cost gain follows. Public Search ownership is preserved; no borrowed-context API is added.

[Safe reproducible evidence](benchmarks/native-record-borrowed-text-2026-10-24/README.md) includes all8 metric rows,16 raw terminal test results, first compile failure, controller/source/artifact/toolchain pins and independent cleanup. Binaries, keys, stores and original/context payload stdout are excluded. No merge, release or deployment is authorized by this draft.
