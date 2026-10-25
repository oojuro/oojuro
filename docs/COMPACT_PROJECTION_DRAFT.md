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
retain the existing exact text check. This optimization is not yet qualified by
million-record receipts.

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
