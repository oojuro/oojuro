# Bounded sorted copy-on-write edits

Repeated single-key edits rewrite a changed leaf and its ancestors after each
insertion. This candidate adds a bounded sorted edit operation and uses it for
disposable projection IDs, raw-key lookups, postings and numeric-row metadata.
It retains standard encrypted tree pages and existing source-authority defaults;
there is no source wire/profile or derived-layout version change.

Each edit buffer holds at most 64 entries and 1 MiB of payload. Recursive edits
reuse untouched children, write a touched node once for that batch, and split
by the existing page/count bounds. All inputs are bounded/validated; duplicate
or unsorted keys refuse. IDs consult both the persisted tree and the pending
predecessor label, preserving the exact existing total ordering and label gaps.
The ordinary single-edit source path remains unchanged.

Runtime `3147bc6ec7ea44d44c57f6d5ef9a27aacb6bf707` is compared with main
`2d8dfe73` using identical opt-in `profile_juro_append` harnesses and distinct
binary hashes. [Complete receipts](JURO_COW_BATCH_EDITS_2026_10_25.json) retain
the preliminary metadata-only variant and all final 1k/100k observations.
These are actual `.juro` source appends, actual original reads and actual
copy-on-write publication, with raw/Zstd derived indexes and a plain NOCOMPRESS
source. Each process uses one CPU and a 2 GiB virtual-memory cap; OS caches are
retained. Only synthetic non-agent records are used.

Engine-generated intents are persisted and synced once before creating fixture
copies. Both variants therefore append identical cursor/time/data records.
Source append success confirms its frames; the index uses the actual new
checkpoint after proving the old prefix. Every selected original is fetched
with `Journal.get` and checked exactly. Fresh full-index oracles pass for each
encoding/size, and all ID/time/score-bit/excerpt signatures match across variants
and rounds. All generated per-observation copies are removed afterward.

| Existing events, +100 | Baseline growth MB | Candidate growth MB | Baseline source+refresh ms | Candidate source+refresh ms |
| --- | ---: | ---: | ---: | ---: |
| 1k, raw index | 8.60 | 0.52 | 158–176 | 116–123 |
| 1k, Zstd index | 7.81 | 0.35 | 162–168 | 129–134 |
| 100k, raw index | 14.58 | 2.60 | 170–185 | 114–145 |
| 100k, Zstd index | 12.47 | 2.34 | 179–188 | 110–140 |

Ranges contain three observations per variant. The preliminary metadata-only
variant retained mixed/adverse latency results; batching all four trees reduces
both physical growth and observed refresh time in these small final cohorts.
This is an incremental storage improvement, not a claim about the original
index's full size or a universal speedup.

The source-plus-refresh timer starts with the already retained intents and a
healthy source/index handle. It excludes separately reported intent persistence,
source/index open, network/provider authorization and application policy. It
therefore does not establish a hosted or complete application 250 ms target.
One-million actual append qualification is still running separately; existing
distinct/dense query and cold-index limitations remain unchanged.

The final complete engine/rank gate passes 189 engine tests and eight rank
checks, with ten existing opt-in/helper cases ignored. It covers historical
source formats/authorities, exact score/originals, encryption, crash publication,
quota/prefix/refusal behavior and incremental append/retirement versus rebuild.
New tree tests cover sorted merges, splits, original-root readability, duplicate
and work limits, and large values. Strict runtime/example Clippy and changed-file
formatting pass. No package release or service activation follows from this PR.
