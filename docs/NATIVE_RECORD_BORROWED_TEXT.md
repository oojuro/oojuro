# Native Record index borrows validated originals: source proposal

SOURCE ONLY, uncompiled/unrun. Separate topic based on qualified counted-output319174/PR86 (runtime09b0/test631). No dependency or feature/default change. No performance claim and no change to the published86 evidence/source.

## Exact ownership change

RecordLog retains Assigned.input.text; returned Record/Search still own full original text/metadata. Native Index rebuild and PersistentIndex load/extend no longer ALSO copy that text into the internal lexical Event carrier. The native constructor tokenizes the identical borrowed text and retains identical event ID/time/indexed/length/scope/postings. A private retain_text flag distinguishes this mode from genuine empty original text. No public API type/lifetime changes.

Existing CaptureLog constructors/cache hydration and projection/paged builders retain text as before. Projection clears input Row.text after lexical construction and needs its one retained carrier for refresh equality/excerpts; it is not changed to borrowed mode. Its visibility adds only None for the optional accessor. Generic record_text refuses borrowed mode instead of returning a fabricated empty original. Capture segment load explicitly keeps retain_text=true.

## Borrowed reads and fail-closed boundaries

Native Index/ranked search first perform their existing current mark, scope, View/as-of, visibility and key checks in the same order. Only then a short-lived callback in RecordVisibility can resolve a lexical ordinal plus EXPECTED original ID to the current RecordLog text. Checked get+exact ID, no first-ID=0/1 assumption, arithmetic, unchecked indexing, new fulltext/pointer Vec or persistent callback. The callback is never retained by Index, PersistentIndex, a receipt or an authorization object.

Only the two existing literal checks use it: ordinary eligibility (including quoted punctuation/no-postings scans) and optional ranked extra-ID eligibility. Pure lexical token scoring needs only existing posting metadata. Missing accessor/wrong row or ID returns an error; it is never treated as empty text/nonmatch. Small private rank/continued Result propagation carries that failure through the already-Result public search APIs. Shared candidate order, background statistics, scores, pruning, literal word boundaries and continued behavior are unchanged.

Persistent cache hydration gets the true original via the same checked accessor at its sole native caller. It still checks row ID/fingerprint, term frequency/length/scope sums and indexed/EMPTY-original zero-posting invariants. Empty carrier Strings are not used as evidence of original emptiness. Disk rows/segments/manifest/AAD/formats and fallback rebuild behavior remain unchanged; snapshots never stored original text. No Store/authority/append/recovery implementation changes.

## Three new source tests (not executed)

1. Actual native RecordLog, nonzero firstID9, >100kB escaped/Unicode originals, empty/punctuation/stored-only/key replacement/two-clock cuts; borrowed index vs retained-constructor reference compares full owned Records, exact score bits, IDs/row/continuation, normal/literal/identifier/key/empty queries and filtered results before and after log reopen. Explicit phrase/punctuation/Unicode assertions prove returned originals. Assert native carrier text length AND capacity0, retained reference length exactly source text and capacity>=length. This is known retained String payload, not process RSS or allocator overhead.
2. Genuine PersistentIndex rebuilt→Loaded→tail refresh→merge→Loaded with empty/punctuation/large originals; require Loaded rather than silently falling back, compare exact full-original/score parity throughout and assert zero retained carrier text capacity.
3. Native codec hydration: genuine retained snapshot loads into textless carrier, requires exact row identity; forged posting on truly empty original refuses. Actual quoted query without original accessor refuses, correct checked accessor resolves the expected nonzero ID. No weakened cache or empty-string fallback.

## Bounded qualification plan for separate approval

ONE canonical actual Rust1.99.1/portable x86-64/no-default fresh non-test engine lib and DEBUG native CLI+engine libtest build. Match PR86 compiler/CC/linker/RUSTFLAGS/debugopt0. Frozen candidate source/artifacts/fresh:false/feature[]/profile/ELF/hash proof and independent exact PID/Invocation/cgroup/TMP cleanup. No build dispatched by this document.

Then exact16 selected engine tests: the3new above; native direct Nine bitwise versions/background; native stored-only literal/identifier; snapshot/scope/output; escaped Search wire bounds; retained Persistent reopen/tail/merge; Capture bitwise phrases/dedupe/pruning oracle and above-floor pruning oracle; projection opaque-ID scores, refresh growth/replacement/removal, exact UTF8 passages; ranked off/all-skipped, expansion/extras/hidden refusal and exact wire receipts. No storage crash/codec matrix ritual: authority/storage/recovery/formats are byte-identical, but these retained shared-scorer consumers are meaningful because private Result/accessor plumbing is shared. Exact selectors must match nonzero1/0/0; root reviews executable driver and CPU window before dispatch.

## Later fixed allocation comparison, not authorized here

Control should be the EXISTING qualified PR86 runtime09b0 DEBUG CLI cd270843..., so JSON-counting savings are already present in BOTH arms. Fresh candidate only. Reuse the exact qualified context4 fixed32texts/query needle compact/k32/eightcopy AB/BA/AB/BA/one-page warming/whole32field+watermark/source/copy inventories/exact922B output/caps. New candidate artifact/hash is NOT_FROZEN; no workload controller changes or run yet. Root reviews concrete source/commands before any campaign. All outcomes and prior failures remain retained; no smaller substitute/favorable rerun.

A retained-payload length/capacity assertion establishes removed owned String storage in this type. It does not establish process peak, steady RSS, speed, release behavior, hosted savings, a larger-than-RAM engine or a cost reduction. Source log decoding, postings, owned Search hits and final context/output still allocate. CLI DEBUG peak may be controlled by those other phases; no gain is assumed. Any separate phase diagnostic would require another explicit scope review, not be silently added to the fixed campaign.
