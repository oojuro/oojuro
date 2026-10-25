# Experimental compact disposable projection

This is an unqualified layout-2 implementation under development. It is not a
source database format, release guarantee, hosted performance result or new
format for `.juro` source files. Wire 0.1, Event profile 1, exact original reads
and existing layout-1 caches keep their current contracts.

`Builder::create` and `EventBuilder::create` still build layout 1. The additive
`create_compact` methods explicitly build a complete new layout-2 generation;
existing caches are not silently reinterpreted or migrated. Incremental append
inherits its prior generation's layout, retains unchanged source-prefix and
caller-authority requirements, and does not fuse per-source rankings. Both
layouts use the same global Plan/Stats/Model and selected-original validation.

Layout 2 has an independent authenticated `OJPROJ02` identity, bound to the same
namespace/writer/view fields. Its encrypted descriptor carries `format: 2`.
Layout-1 readers cannot authenticate that descriptor. New readers authenticate
only the two known identities and require the descriptor format to agree with
its authenticated identity. Wrong keys/bindings, unknown values, malformed
blocks and extra/truncated metadata refuse. The layout is disposable: unsupported
or corrupt caches require rebuilding from a freshly validated source. They never
become an authority, substitute for originals, or repair source bytes.

Term entries retain their existing authenticated term-hash/first-ordinal keys.
Each layout-2 block covers part of one aligned 256-ordinal interval. Sparse
blocks carry ordered offset/TF pairs; dense blocks carry exact u32 TF bitplanes;
uniform blocks carry one nonzero TF and a presence bitmap. A partial interval
can have additional nonoverlapping entries after append. Decoders validate
lengths, modes, strictly increasing ordinals, nonzero TF, row bounds and the key's
first ordinal. Raw and Zstd packing keep their existing bounded strict-frame
checks. No approximation, quantization or lossy text conversion is used.

Metadata uses bounded binary field encoding instead of JSON. Every existing
metadata field, optional-value distinction, original fingerprint and encrypted
text-blob reference is preserved; strings remain exact UTF-8 bytes. The codec
has an explicit marker, checked lengths/enums/booleans, and requires exact EOF.
The original Row metadata comparison and exact original-text hash still gate
materialization. Binary source events remain source-readable and unindexed by
the neutral Event adapter.

Numeric columns also derive live masks and exact integer length/scope bitplanes.
Their allocation is admitted within `Limits.metadata_bytes`, including column
and vector capacity, before use and included in `estimated_heap_bytes`. At most
65 bits per ordinal plus bounded vector overhead are needed for the mass planes;
real columns often need fewer planes. Query intersections provide the exact
integer collection/view statistics to the unchanged floating-point arithmetic.
A missing posting-cache lease falls back to the ordinary exhaustive scorer.

The existing 12 MiB posting/selection policy is retained and charged while any
reader holds a lease. Layout-2 leases store adaptive blocks instead of expanded
pairs; maximum block count/payload bounds admit allocation before loading. A
query-local score memo stores at most 1024 exact `(length, scope, every TF)`
shapes and is enabled only for at most 16 query terms. It computes each missing
shape with the original scorer; larger diversity uses exact scoring. Temporary
query allocations and process RSS remain additional to the retained index heap.

The first exploratory actual-file cohort at local revision `ab9d86b` included
compact term blocks without binary metadata or mass statistics. Its raw 100k
index changed from 110.44 to 104.97 MB and its Zstd index from 75.66 to 74.96 MB;
query latency was mixed. That is evidence that term packing alone does not solve
the total storage cost. Complete matched receipts, full gates and final
qualification remain required. This layout still duplicates original text,
retains the existing literal text fallback, and makes no general 100 ms, 250 ms,
1M concurrency or one-third-storage claim.

The second exploratory cohort at `02187c9` compares layout 1 and layout 2 in the
same frozen binary with identical actual source files and exact result checks.
At 100k rows, raw cache bytes are 110,442,233 versus 66,212,762; Zstd bytes are
75,658,372 versus 56,888,708. The stage-2 `common_triple` label actually queries `battery electrode transaction`;
`transaction` is absent in this fixture, so it is a pair plus one absent term,
not three matching terms. Ten-client forced-fresh pair/pair-with-absent-term p95s are
132.94/144.19 ms versus 83.13/79.78 ms raw, and 88.52/126.33 versus 66.27/82.62 ms
Zstd. Each query has 30 timed observations, after an initial warming query; final
rank-result reuse is bypassed. These are small local synthetic cohorts, with
retained OS caches and source callbacks serialized on one healthy handle.
At 1k rows the timings include mixed/adverse cases; dense phrase p95 remains
148.10 ms raw and 165.05 ms Zstd for layout 2. This is not a general latency
qualification. [All preliminary receipts](benchmarks/JURO_COMPACT_PROJECTION_DRAFT_2026_10_25.json)
retain both stages, every observation, exact signatures, binary hashes and peak
build allocation observations. A source file, other generations and build
scratch are additional to the reported retained index bytes.

An additional in-memory block summary is now being qualified. It retains at most
16 exact `(length,scope)` shapes per 256-ordinal block plus conservative maximum
time/minimum-label bounds, charged within the numeric metadata limit. More
heterogeneous blocks use no score bound. Every term TF bound is an overestimate;
score bounds evaluate concrete shapes because independently minimizing length
and scope is unsafe. Optimistic score/time/label tuples can prune only blocks
that cannot enter the current exact top-k. If a trial examines more than 16,384
rows, the complete scorer runs; no partial trial result is returned. Literals
retain the existing exact text check. The actual-file million-record cohort below covers this runtime; incremental
and diverse/dense qualification remains open.

The next `4e9c514` cohort alternates layouts for three observations per codec,
each with 30 ten-client forced-fresh requests per query. All exact signatures
match. At 100k actual rows the raw pair p95 ranges 107–138 ms for layout 1 versus
0.43–7.15 ms for layout 2; the actual three-matching-term query
`battery electrode original` ranges 134–137 versus 0.43–0.45 ms. Zstd pair ranges
90–144 versus 0.45–5.54 ms; the actual triple ranges 136–159 versus 0.49–11.20 ms.
The separate `pair_with_absent` query retains the earlier `transaction` case.
Future harness output includes the exact query text, so labels cannot obscure
this distinction. These homogeneous fixtures permit tight bounds. They do not
qualify heterogeneous data, literal queries, cold handles, a hosted workload or
million-record latency. All 1k mixed/adverse observations are retained too.

The actual-file 1M cohort at `4e9c514` now passes all paired signatures across
both layouts and encodings. It uses the same plaintext NOCOMPRESS source,
998,996 UTF-8 indexed rows (1,004 binary originals remain readable), actual
`Journal.get`, checkpoint guards, one CPU and a 2 GiB child address cap. Each
encoding has three alternating observations per layout and 30 timed ten-client
forced-fresh requests per query per observation, after an initial warming query.
The layout-1 cache is the retained complete cache from the earlier actual-file
cohort; no v1 build timing was repeated here.

| 1M retained ten-client query p95 | Layout 1 raw ms | Layout 2 raw ms | Layout 1 Zstd ms | Layout 2 Zstd ms |
| --- | ---: | ---: | ---: | ---: |
| Two matching terms | 1,024–1,048 | 26–29 | 1,183–1,211 | 24–32 |
| Three matching terms | 1,391–1,774 | 26–31 | 1,714–1,760 | 29–31 |
| Pair plus absent term | 1,047–1,078 | 28–30 | 1,196–1,206 | 26–29 |
| Single common term | 10–20 | 23–25 | 24–28 | 23–26 |
| Identifier anchor | 55–65 | 50–68 | 58–67 | 51–64 |

The single-common-term raw case regresses in these observations. All rows in
this synthetic fixture have closely related text/TF shapes; the multi-term
improvement does not qualify heterogeneous corpora or dense literal search.
A single-client cold source/index reopen plus pair query remains 1.10 seconds
raw and 1.06 seconds Zstd for layout 2 (1.21/1.13 seconds layout 1). OS caches
remain retained even for reopened handles. This is not a general 100 ms promise.

Retained raw cache bytes are 1,106,391,854 versus 663,059,132; Zstd bytes are
759,609,940 versus 570,925,451. Adding the 278,960,161-byte actual source gives
1,385,352,015 versus 942,019,293 bytes raw and 1,038,570,101 versus 849,885,612
bytes Zstd. Layout 2 therefore does not establish a one-third combined-size
claim. Previous generations and construction scratch are additional. New v2
builds take 135.85 seconds raw and 155.95 seconds Zstd; peak logical new-index
footprints are 1,953,315,379 /1,861,189,782 bytes, with observed allocated peaks
1,953,349,632 /1,861,226,496 bytes and 191 files. These observations are not a
filesystem quota. No v2 full-cold rebuild or encrypted 1M source cohort was run.

The retained v2 index heap estimate is 138.77 MB, including the full cache
policies, numeric columns, bitplanes and shape summaries; source handles and
query temporaries are additional. Build RSS peaks are 567,160 /567,484 KiB;
first retained-query observations peak at 550,660 /550,900 KiB. The complete
196-engine-test/eight-rank gate passes for this runtime with ten documented
helpers/opt-in tests ignored. This remains a draft until incremental, diverse
and dense qualification and final review. No package or hosted service changed.

After integrating main's bounded COW edits and original-read preflight, runtime
`8661394` passes 26 disk tests and a 56-observation actual append campaign. The
campaign covers 1k/100k plain sources, 100k encrypted sources and a 1M plain
source, both layouts and codecs, with identical durably retained engine intents.
Every exact original/checkpoint/signature comparison and thirteen complete
rebuild oracles pass. Each layout/codec/fixture has three +100 append observations.
The append timer still excludes source/index open, intent persistence and all
provider/application work.

Results include adverse cases: encrypted 100k Zstd layout-2 source+refresh ranges
175–189 ms versus 139–181 ms layout 1; 1M raw ranges 168–234 versus 198–217 ms;
1M Zstd ranges 242–289 versus 222–253 ms. The 250 ms target is not qualified by
this cohort. Layout-2 delta growth is 5.52 MB raw /5.46 MB Zstd at 1M, compared
with 6.30/5.59 MB layout 1. The one complete 1M compact-raw rebuild oracle takes
212.26 seconds outside the append timer. All results remain in the receipts.

The next local runtime avoids revisiting every numeric row during a small
append. It copies already-admitted mass planes and rebuilds only affected
1024-row numeric blocks, including their 256-row shape summaries. Retirements
clear old contributions; new numeric bits and appended mask words are admitted
before allocation. A focused multi-generation growth/retirement/new-bit test
matches full rebuild sums and bounds, and the raw/Zstd append/retire rank oracle
passes. Its performance is not yet qualified; the prior adverse observations
remain authoritative until matched runs complete.
