# Structural paged Record reads: first bounded slice

This is an opt-in native source change toward foundation SPEC sections4/6/14,
not the complete10M/100M engine. It changes retention structure rather than
raising existing limits. PR26 and the original RecordLog/memory/PersistentIndex
APIs remain separate. This namespace still honors Record2's100k records/128MiB
serialized state and existing segment/manifest caps. No migration occurs.

## Source-backed capacity audit (base b33cad3)

| Layer | Actual current allocation and consequence |
| --- | --- |
| RecordLog authority.rs73,306-365 | all Assigned/Input/text/attrs in Vec, all original receipt outcomes and prefix witnesses in maps/vectors; startup replays every bounded batch |
| record/persistent.rs332-420 | creates carriers for the entire population (cloning text), then hydrates every posting segment into one RAM Index |
| record/view.rs213-272 | whole key-winner map/unkeyed vector, visible-ID set, then complete selected vector and sort before pagination |
| lexical.rs402-574 | complete candidate set/rows, per-term hit lists, gains/bounds/exact vectors; F3 needs statistics from the entire visible view |
| Store store.rs200-218/328-418 | all FrameRefs, dictionary bytes and file handles retained; default decoded cache4MiB |
| Store store.rs532-552 | reads each whole hot file (default rotation64MiB), verifies/decodes each frame on open |
| Store store.rs705-714 | first dictionary training collects current hot texts; normally starts at4MiB plus an append, but no explicit sample-work byte budget |
| Store store.rs912-978/1098-1180 | compaction streams frames/rows but clones hot/window frame locators, retains dictionary sample/entry vectors and Packer chunk table |
| Store reader.rs220-248 | random reads use a bounded decoded-frame cache; get_range limits returned text, but encryption/decompression still processes a bounded whole frame as needed |

A bigger EC2 instance does not remove these population/frame-count allocations.
No numbers here are an RSS measurement or a performance result.

For the subsequent opt-in disk metadata/tree implementation, see
[NATIVE_RECORD_TREES.md](NATIVE_RECORD_TREES.md). The allocation descriptions
below describe this first slice and its original default APIs.

## New owning reader

`record::paged::Reader::open(existing_dir,key,identity,minimum_watermark)` opens
the existing sealed Store directly, verifies exact Record2 header/identity,
sequential original IDs/recorded_at, strict typed intent fingerprint, complete
batch receipt and rolling sealed prefix one batch at a time. Payloads are dropped
between batches. It retains O(batch-count) numeric/opaque request/prefix locators,
not the Assigned/text/attrs population. The startup duplicate-request set is also
O(batch metadata), transient. These are explicit intermediate bounds, NOT constant
memory or the final scale architecture. Store retains its own frame/dictionary/
file metadata and a1MiB decoded cache for this reader.

The reader is an **owning/recovering Store open**, not a filesystem read-only
open: Store may lock, clean orphans, repair a torn tail or create a hot file.
It has no application append API, no ID/time allocator, no clock and no new ACK.
`receipt(request)` returns the verified ORIGINAL stored Commit, including its
original sealed watermark; it never fabricates another allocation result.

A supplied known minimum witness rejects shortened/equal-count divergent history.
Without an independently known later watermark, a wholly restored healthy old
journal remains undetectable. Sealed prefix digests stay in RAM or encrypted
commitments, never plaintext sidecars. Shared wire structs/functions only gain
internal visibility; old Record2 serialization/replay semantics are unchanged.

Reader and old RecordLog cannot coexist as separate Store owners. The tested
workflow closes the old writer after ACK, opens Reader, then closes Reader before
another old-writer append and reopens it for cache tail refresh. A shared paged
writer/serving owner remains critical-path code, not a production concurrency
claim.

## Reads and snapshot paging

`Reader::view(scope,AsOf)` captures a protected original prefix. `get_id`,
`get_key`, `history`, `range` and `continue_page` retain original typed provenance,
adapter, IDs, valid_at and engine recorded_at. Key winner selection considers
valid/known time before lower time and typed filters, matching existing View.
Pages select only bounded IDs in a heap, fetch payloads only for the selected
page and enforce the complete serialized budget. Sealed continuations bind exact
snapshot, scope, cutoffs, selection/filter and last original ordering tuple; no
whole selection vector is retained or sorted. An oversized next record is not
skipped. Historical and All reads preserve every original version/duplicate ID.

Until the on-disk key/version tree is added, key winner checks stream original
records. Many distinct keyed records can make this **O(N squared)** work. That is
an explicit intermediate performance gap; no fast-large-store result is claimed.
Queries currently also stream typed Record envelopes, rather than separate
persisted columns. A batch is <=1000 rows/32MiB serialized; a Store frame can be
up to64MiB. Nested winner checks can hold an outer and inner bounded batch,
canonical verification buffers and bounded decoded frames simultaneously. Bounds
refer to representation windows, not exact Rust RSS or only a single row.

## Disk posting reader and exact query

`record::paged::Index::open/refresh/merge/search/search_view` uses a NEW explicit
`PAGED_RECORD_INDEX` encrypted manifest and `native-record-paged/.../v1` AAD
namespace. It does not reinterpret PR26 cache directories. It retains reference
metadata and decrypts one <=8192-row/16MiB binary posting segment at a time; it
never hydrates one all-population lexical Index or text carrier vector.

The query uses bounded passes: determine visible identifier occurrence and
query-term DFs; accumulate exact candidate/whole-visible-view integer statistics;
then score exhaustively with original canonical term order, Dirichlet128/F3
padding and f64 operations. Top-k uses a bounded heap with score, newer valid time,
then full original decimal Record ID ordering. F2 anchored results precede marked
continuation. Direct actual-key reads keep their original zero lexical score,
and stored-only punctuation/literal queries keep the eligibility guard. Visibility
and known-time/key winner selection happen BEFORE candidates and background
statistics. There is no approximate per-segment top-k or post-top-k filtering.
For historical snapshots, literal rarest-term selection deliberately uses DF
from the whole current manifest, exactly as the existing native Index does even
when passed an old View. This is only a candidate-scan optimization: snapshot
visibility, identifier occurrence and F3 background statistics remain restricted
to the historical prefix. A regression appends rows that flip the rarest term,
adds a later key version and identifier occurrences, then compares exact native
old-View IDs/bits/continuation before and after paged refresh/merge.

The separate externally supplied opaque-ID projection is untouched by this
Record2/u64 storage path; no conversion or reassignment of those IDs is implied.

Original records/payloads for final hits are fetched by batch locators, with
bounded retained output. Index loss/corruption/unsupported/ahead/fork causes
rebuild only from the independently validated supplied Reader; a failed/stale
live handle refuses. The authority's original Commit remains unchanged on cache
publication failure. Names, attrs, payloads, terms and temporary files have no
plaintext disk sidecars. Platform RAM/swap/key security remains external.

## Streaming builder and coalescing merge

Build/refresh tokenizes a bounded <=1024-row/16MiB-text window and writes genuine
native binary segments. It never creates an all-record lexical Index. A single
pathological row/candidate outside codec accounting refuses without changing
authority. Existing per-segment16MiB and aggregate128MiB decoded/encoded accounting,
1024references and1MiB manifest are enforced. These caps are NOT raised.

Merge holds at most a bounded incoming segment plus carry segment, coalesces
contiguous whole segment runs when row/accounting bounds permit, and emits new
immutable encrypted segments. It does not materialize all segments. Temp+fsync+
rename+directory fsync precedes atomic encrypted manifest replacement and orphan
retirement. No second WAL exists. This is bounded coalescing, not the SPEC's final
multi-gigabyte external streaming compactor or mmap query reader.

## Next assigned structural code gates

1. Replace Reader's O(batch) locator/request/prefix metadata and cache reference
   vector with authenticated encrypted paged ID/sequence/request/witness trees.
2. Add a sorted paged key/version index and bounded external-sort builder, replacing
   repeated full winner scans; keep known_at/winner-before-filter/F3 exactness.
3. Integrate a shared paged writer/serving owner and exact retry lookup, preserving
   journal-only ACK; coordinate a SINGLE managed future format with erase/audit.
   Record2 remains capped/unchanged. Its first-ciphertext origin is invalidated by
   physical compaction, so future rewrite/compaction requires explicit stable
   origin/lineage semantics, not an implicit reinterpretation.
4. Bound/map Store frame/file/dictionary metadata, hot scanning, dictionary sample
   and dedupe-compaction working sets. Then execute the actual10M/100M and complete
   correctness/size/latency/privacy/erase/audit gates on declared hardware.

These remain implementation work, not a deployment checklist or a claim that the
first paged read slice completed SPEC. No AWS/provider/Notion operation, real data
import or original-store modification was performed; tests use synthetic stores.

## Evidence

Initial seven focused regressions passed (11.87s compile,10.39s runtime), covering
native bitwise parity below/above F3 floor and1200 candidates, IDs/times/provenance,
known/key visibility, duplicate text, literal/identifier continuation, original
receipt/get/history/range/cursors, restart/tail, fork/corruption/bounds/privacy.
The full current-source engine gate at bf1804d passed **81 tests,0 failed,
5 ignored helpers** (1.27s compile,12.02s runtime), including the nine new active
paged regressions and actual SIGKILL at all five new cache publication boundaries.
Strict owned engine lib/tests clippy initially found two style diagnostics; only
nested-if and fixed-chunk syntax were changed, then strict lint passed. The final
historical future-DF test passed separately (1.34s compile,0.17s runtime); it did
not require rerunning unrelated behavior. Final strict lint including that test
passed0.60s. This is not a claimed fresh82-test aggregate run.

Root source review cleared the first read/cache runtime; the follow-ups add
explicit proof cases, documentation and mechanical lint corrections. Tests used
only synthetic stores and this worktree's isolated target, acquiring the shared
verification lock outside each bounded actual step. Ignored local evidence logs
are `.scratch/paged-first-test.txt`, `paged-full-test.txt`,
`paged-historical-test.txt` and `paged-lint-historical.txt`. No large-store/RSS,
provider/AZ, production concurrency or full-SPEC acceptance was measured.
