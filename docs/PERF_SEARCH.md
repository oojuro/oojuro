# Search latency at scale (`v1-perfsearch`)

Question: how fast can per-turn search get at 1M+ records per store, for
queries from 200 B to 256 KB, without changing the ranking (SPUD-Dirichlet,
candidate-set background, mu 128)? Target: per-turn search p99 <= 20 ms at
1M+ records per store. Rule for every kept change: the top-20 ids and score
bits equal the reference on every tier-1 turn-loop query, or a held-out
quality comparison is shown. Every kept change passes the oojuro-nine,
oojuro-trace and oojuro-tracebench tests and `tracebench verify` on smoke
(1,237/1,237).

All timings here are indicative: the machine was shared (load 1.3 to 43
during this work; a timed benchmark ran alongside, so every probe ran at
`nice 19`).
Each number carries its load average. The final timings are rerun
on a quiet machine (commands at the end).

## Method

- **Probe** (`crates/oojuro-tracebench/src/bin/searchprobe.rs`). `build`
  bulk-loads a prefix of a tier through `TraceDb` (per agent, or every agent
  folded into one store with `--single`), flushes, settles merges and writes
  `probe.json`. `run` times `Nine::find(query, 0, as_of, 20)` on the agent's
  engine (what `TraceDb::search` calls) for the turn-loop queries that follow
  the loaded prefix, records per-query thread CPU time (load-robust), wall
  time, process CPU, minor page faults and work counters, and dumps top-k
  ids, score bits and entropy bits. `compare A B` checks two dumps line by
  line.
- **Stores.** Tier 1 per agent: the first 90% of base1m's batches (the
  turnloop suite's split), 16 agent stores, ~1.19M rows (900k events plus
  status revisions; largest store ~83k rows); after the merge, record
  dedupe leaves 792k rows (largest 53k). Large single store: tier 2
  (base1m twice, replica clocks shifted), all agents in one store, the
  first 1.70M rows (1.29M events), 5-7 segments, the largest 1.2M rows
  (after the merge built with `--no-dedupe`, see below).
- **Queries.** The turn-loop queries after the loaded prefix: batches whose
  first event is a tool result or user message, `as_of` = turn time - 1.
  `--size 0` is the turnloop suite's query (first 200 bytes of that event);
  `--size N` is exactly N bytes: that event, then the same agent's earlier
  events newest first (an agent's recent context as the query). Tier 1 has
  30,905 turn-loop queries (all used for bit-identity); latency runs use
  evenly spaced subsets.
- **Profile.** Cargo feature `phase-profile` (`oojuro-nine`, passed through
  `oojuro-trace` and `oojuro-tracebench`) adds per-phase thread CPU timers and
  work counters on the query path; without the feature they compile away.
  The baseline profile below was taken with an earlier wall-clock version
  of the same timers.

## Profile of the reference engine

Per query, single thread (wall, load 16-30; shares are what matter).

| phase | tier 1, 200 B | tier 1, 4 KB | tier 1, 64 KB | tier 1, 256 KB | 1.7M store, 200 B | 1.7M store, 64 KB |
|---|---:|---:|---:|---:|---:|---:|
| total | 5.1 ms | 10.9 ms | 44.5 ms | 75.4 ms | 523 ms | 4,983 ms |
| postings walk (decode, as-of/eligibility, slots, hit append) | 57% | 51% | 48% | 39% | 84% | 88% |
| candidate renumbering (sort to row order) | 12% | 2% | 2% | 2% | 4% | 1% |
| merge of chunk outputs (copies) | 4% | 3% | 6% | 5% | 7% | 2% |
| per-candidate term sort (anchored queries) | 0% | 5% | 19% | 20% | 0% | 6% |
| scoring (one log2 per hit) | 14% | 5% | 9% | 10% | 3% | 2% |
| background, penalty, scored list, entropy | 10% | 2% | 3% | 2% | 4% | 1% |
| top-k select and id reads (archive frame decode) | 11% | 24% | 4% | 3% | 2% | 0% |
| query parsing (split, lowercase, stem) | 1% | 2% | 5% | 12% | 0% | 0% |
| dictionary lookups | 3% | 5% | 6% | 8% | 0% | 0% |
| work: terms / candidates / postings | 19 / 15k / 118k | 175 / 5k / 481k | 835 / 13k / 1.1M | 1,938 / 15k / 1.5M | 19 / 285k / 2.7M | 866 / 295k / 27M |

Findings:

1. **A quadratic decode at scale.** Each segment is scanned in 64K-row
   chunks, and every chunk decoded each term's list from its start, skipping
   to the chunk. A 1.2M-row segment has 19 chunks, so the 1.7M store decoded
   31M postings to visit 2.7M (200 B) and 306M to visit 27M (64 KB). This
   alone is most of the 0.5-5 s per query at that scale; tier-1 stores
   (segments under 64K rows) never hit it.
2. **Long queries** are almost always "anchored" (a snake_case identifier
   is present): candidates come from identifier postings, then every other
   term's whole list is decoded to find hits on those candidates. Their
   cost is the posting volume of all query terms (1.1-1.5M postings at
   tier 1, 27-35M at 1.7M), plus a per-candidate re-sort of hits into term
   order (20% at 256 KB), plus parsing (12% at 256 KB).
3. **Allocation** shows as page faults: 1.5k-10k minor faults per query on
   the large store (6-40 MB of fresh hit and candidate vectors per query).
4. **Eligibility and as-of checks** cost one random 40-byte row-metadata
   read per candidate during the walk; in a turn-loop view every segment is
   entirely visible and no row has versions, so these reads decide nothing.
5. **Fixed costs** at 200 B: sorting candidates into row order (12%), the
   20 result ids (each an archive frame decode, 11%), the scored list and
   entropy over every candidate.

## Experiments

Indicative before/after in CPU time per query unless noted; load in
brackets. "Identical" means every one of the 30,905 tier-1 turn-loop queries
returns the same top-20 ids and score bits as the reference (and the same
routing-entropy bits unless stated).

### P1. Exact restructure of the scan (kept)

Hypothesis: the order v3 adds term contributions in (per candidate, by term
index) can be kept without sorting hits, and no chunk needs to decode a list
from its start. Change: hits are stored as one run per term per chunk, and
runs are merged by term index, so each candidate receives its terms in term
order with no per-candidate sort; postings cursors resume across chunks of a
segment (a worker that moves to a later chunk continues where it stopped);
dictionary lookups for a sorted query merge over the dictionary blocks;
`query_terms` counts words in a hash map and caches stems per thread
(equal to the old composition, tested). Result: identical; skipped postings
on the 1.7M store 31M -> 0 (200 B), 306M -> 0 (64 KB); 1.7M-store 200 B mean
530 ms wall -> 81 ms CPU [load 43].

### P2. Bitset row states, no metadata reads in visible segments, compact tail (kept)

Hypothesis: candidate slots, eligibility and the tail allocate and touch
far more memory than needed. Change: per-chunk row states are three 8 KiB
bitsets (candidate, evaluated, in view) instead of a 512 KiB stamp array;
candidates come out in row order by walking bits; rows of segments entirely
inside the view (span check) need only the version-group bit, not their
metadata; the tail keeps a score vector, finds the k-th best with a k-entry
heap and builds full records only for finalists; exp2 terms below -1080
(exactly 0) are skipped in the entropy sum; the length penalty is memoized
per distinct mass. Result: identical; tier-1 200 B p50 6.33 -> 3.50 ms, p99
11.6 -> 6.8 ms [29].

### P3. Branch-free loops, compact candidates, lazy metadata, parallel scoring (kept)

Change: in a fully visible chunk of a store without version groups (the
turn-loop case), the first pass makes every posting a candidate hit and the
second keeps hits by a candidate bit, both without branches; candidates are
rows plus (scope, length), hits name candidates by rank among the bits; row
metadata is read only for finalists; per-chunk scoring runs on the query
threads when the scan did. Result: identical with 1 and 4 query threads;
1.7M store 200 B mean 58 -> 48 ms [29]; tier-1 p50 3.14 ms [27-35].

### P4. Four LEB128 pairs per 8-byte load (kept; version 1 lists only after the merge)

Change: when the next 8 bytes have no continuation bit, decode four
one-byte (gap, tf) pairs at once. Result: identical; 1.7M-store scan time
-15% (200 B pass 1 7.4 -> 6.3 ms, pass 2 9.2 -> 7.7 ms) [33].

### P5. Per-segment cache of result ids (kept)

Change: a segment remembers the ids of rows it returned (1,024 rows; rows are
immutable and erasure writes new segments). Result: identical; top-k select
0.34 -> 0.25 ms on in-order tier-1 turns [28].

### P6. Routing entropy over the returned top k, with bound-based exact top-k (kept as the TraceDb setting)

Hypothesis: TraceDb returns only ids, so the routing entropy over every
candidate (which forces an exact score per candidate) is unused there.
Change: `Options::routing_entropy` = `All` (engine default, unchanged) or
`TopK`. With `TopK`, every candidate gets an upper bound (exact length
penalty, tf * scope / length <= tf, and a libm-free upper bound of log2 that
is above libm's by >= 1.2e-4 per term); the 64 best bounds are scored
exactly, which gives a k-th best exact score theta; every candidate whose
bound reaches theta is scored exactly; the top k are selected from those
with the unchanged selection code, so ids and score bits equal exhaustive
scoring. The entropy then covers the returned results (the same choice
the lexical track made, decision A). TraceDb sets `TopK` (option
`exact_entropy` restores `All`). Result: identical ids and scores on all
30,905 queries (entropy differs on 26,162 by design); a randomized test
compares `TopK` with `All` on stores with and without versions, outcomes,
views and k from 1 to 500 with 1 and 4 threads (a bound that is too low
fails it). Exactly scored candidates: 312 of 15k per tier-1 query, 2,238 of
258k on the 1.7M store. Tier-1 200 B p50 2.94 -> 2.02 ms, p99 5.6 -> 4.5 ms
[24-29]; 1.7M store 200 B mean 41 -> 31 ms, 64 KB 298 -> 269 ms [20].

Superseded first version: the bound used each candidate's exact
scope/length (one division and table read per hit) and was no faster than
exact scoring (hit pass 14.6 -> 17.0 ms on the 1.7M store): per-hit cost is
memory and indexing, not log2. Kept as
`benches/experiments/perfsearch-topk-exact-mass-bound-superseded.patch`.

### P7. Skip tables for long lists, candidate-driven second pass (kept)

Hypothesis: an anchored query's second pass decodes long common-word lists
to find a few candidates; a skip table would let it look candidates up.
Lists have no skip data on disk, but tables can be derived once and kept
(segments are immutable). Change: for lists of >= 1,024 postings, a table
of one entry per 128 postings (first row and decode state), built by one
decode on the list's second use (first-use building cost more than it saved
at tier 1, where most lists are searched once: p50 +6%); the second pass
looks candidates up through it when a chunk has fewer than 1/32 as many
candidates as the list has postings there; decodes that cannot resume start
at the right block. Result: identical (tests run both paths with tiny
blocks; a wrong tf in the lookup fails them); tier-1 200 B p50 1.37 -> 1.27
ms, p99 3.10 -> 2.89 ms; 1.7M store 200 B p50 13.4 -> 10.3 ms [6-8].

### Merge of v1 (Rice postings, record dedupe, starter dictionary)

Merged `v1` (with `v1-perfstore`) at `d598dfc`: Rice-coded postings
(segment version 2; version 1 still read), record-level dedupe and a
shipped starter dictionary. Resolution: the scan, the skip tables and the
candidate lookups decode through one cursor layer (`segment::List`,
`segment::Cursor`) for both versions, equal to `Postings` on every list
including truncated ones (test); P4's fast path stays for version 1 lists;
a test-only switch writes version 1 segments so a new test runs every scan
path on them against the v3 oracle.

The reference was rebuilt from the merged code (`v1` plus the probe's
prerequisites, `scripts/perfsearch/reference-probe.patch`), and so were the
stores: dedupe stores a repeated message once, so the tier-1 stores hold
792k rows (largest agent 53k) instead of 1.19M. On a replicated tier dedupe
folds the replica back (the tier-2 single store fell to 796k rows), so the
large store is built with `--no-dedupe` to keep 1.70M rows; search does not
depend on how the rows got there.

Before/after the merge for the same final engine: the Rice decode is now
on the hot path. On the 1.7M store (same rows both times) the final engine
went from 10.0 / 56.9 ms (p50 / p99, turnloop 200 B) and 178 / 209 ms
(64 KB) to 19.0 / 77.3 ms and 365 / 420 ms [load 3-9], about twice the
per-posting decode cost. Tier 1 is not comparable across the merge (dedupe
removed 40% of the postings): 1.71 / 3.35 ms before, 1.84 / 3.64 ms after.
P8-P11 recover it.

### P8. One-window Rice decode (kept)

Change: decode a whole posting (unary quotient, k remainder bits, gamma
tf) from one 8-byte window when it fits, else the bit reader as before.
Result: identical (random lists including truncated ends against
`Postings::Rice`; mutation-checked); merged tier-1 200 B p50 1.24 -> 1.00
ms, p99 2.96 -> 2.55 ms; 64 KB p50 12.5 -> 10.4 ms [5].

### P9. Result ids from the row's id prefix (kept)

Hypothesis: after P1-P8 the 20 result ids (one archive frame decode each
unless cached) are a quarter of a tier-1 query. Every row already stores
the first 8 bytes of its id. Change: an id of up to 8 bytes is taken from
that prefix when the segment's id table maps the prefix string's 128-bit
identity fingerprint to this very row; longer ids, ids ending in NUL bytes
or any mismatch read the original as before. Result: identical (and a test
of NUL-padded, 8-byte, longer and non-ASCII ids); merged tier-1 200 B p50
0.94 -> 0.80 ms, p99 2.43 -> 1.52 ms [4]. TraceDb ids are decimal event ids
(at most 8 bytes below 10^8 events), so its results never touch the archive.

### P10. Faster query parsing (kept)

Change: words are found by scanning ASCII bytes (chars decoded only for
non-ASCII), and counted in a multiply-rotate hash map instead of SipHash
(the stem cache too). Equal to the reference composition on randomized
text with Unicode letters and separators. Result: 256 KB parsing 2.4 ->
2.0 ms, query p50 12.6 -> 12.3 ms [1.3].

### P11. Query threads that scale (kept)

Hypothesis: the scan was already parallel over chunks, but scaled poorly
(1.7M store, 64 KB: 253 / 194 / 186 ms wall with 1 / 4 / 8 threads): a
worker that takes a chunk out of order cannot resume its lists, so it
decoded them from the start or from its previous chunk. Change: before a
parallel scan, long lists of multi-chunk segments get their skip tables
(built in parallel, kept), and a chunk starts at its block unless it
directly follows the worker's previous chunk. Result: identical with 1 and
8 threads (1.7M store, entropy bits too); 1.7M store wall p50 / p99 with
1 / 8 threads: 200 B 13.1 / 63.4 -> 2.1 / 24.1 ms, 64 KB 260 / 315 -> 50 /
76 ms [1.5] (table below).

### Ideas measured or analysed but not adopted

- **Dense-term bitsets cached across turns.** Measured potential on the
  1.7M store: terms with df >= rows/16 hold 87% of the postings of 200 B
  queries, 60% at 4 KB and 35% at 64 KB. Not built: every posting of such a
  term is still a hit that must be materialized for scoring, so a bitset
  only replaces the decode of part of the volume, at ~rows/8 bytes per term
  per segment of resident memory; the skip tables (P7) take the
  candidate-driven share at 32 B per 128 postings.
- **Skipping near-stopword postings with an exact bound.** Not possible
  under this model: the term's background probability needs its exact count
  among candidates (the intersection of its list with the candidate set),
  and the top-k scores need its tf in each finalist; both require reading
  the list (now through skip tables when the candidates are few).
- **Per-term contribution caching and incremental top-k across turns.** A
  term's contribution depends on its background probability, which depends
  on the whole candidate set (every query term) and on rows appended since
  the last turn, so contributions and top-k sets are not reusable exactly.
  What is reusable is cached: stems, skip tables, result ids.
- **Segment-level max-score bounds.** With the candidate background the
  bound of a term is only known after the union is complete, i.e. after
  the postings are read; P6 applies bounds after the scan, where they cut
  scoring, not reading. E12 (track/lexical) found that even with a
  precomputable background, 150+ term queries leave almost every term
  essential.
- **A query-length-adaptive model with a precomputable background.** Not
  run: it changes rankings, so it needs the dev/held-out protocol (turn-loop
  split and BEIR), and on its own it would not remove the posting volume of
  long queries (above). With skip tables in memory, a precomputable
  background plus block-max bounds is the next ranking experiment to try.
- **SIMD log2 / vectorized scoring.** Bit-identical scores require libm's
  log2 for every exactly scored hit; P6 already removes most of those calls
  where the entropy is not needed.

## Results

CPU time per query in ms, one query thread, p50 / p99. "final" is the
TraceDb setting (routing entropy of the top k); "final, exact entropy" is
the engine default (every candidate scored). The three arms of a row ran
alternately in one sweep (200 queries per tier-1 size, 100 at 16 KB, 50 at
64 KB and up; 40 / 20 on the 1.7M store). Exactness after the merge,
checked separately against the merged reference: all 30,905 tier-1
turn-loop queries; 200 tier-1 queries at each of 1, 4, 16, 64 and 256 KB;
on the 1.7M store 300 at 200 B, 40 at 4 KB, 30 at 64 KB and 10 at 256 KB;
with 1 and 8 query threads. Ids and score bits are equal in every case,
and in the engine-default mode the entropy bits too.

Before the merge (reference: the v1 engine at `0085d01` plus the probe;
final: `2c16b11`, P1-P7):

| store | query | terms | candidates | postings | reference p50 / p99 | final p50 / p99 | final, exact entropy p50 / p99 | load |
|---|---|---:|---:|---:|---:|---:|---:|---|
| tier 1 | turnloop 200 B | 19 | 15.0k | 0.12M | 3.43 / 7.67 | **1.71 / 3.35** | 1.90 / 4.13 | 4.26 |
| tier 1 | 200 B | 21 | 13.5k | 0.12M | 2.42 / 7.62 | **1.69 / 3.65** | 1.73 / 4.17 | 4.26-4.64 |
| tier 1 | 1 KB | 68 | 5.8k | 0.26M | 3.14 / 11.26 | **1.97 / 5.61** | 1.97 / 6.05 | 4.64 |
| tier 1 | 4 KB | 175 | 5.2k | 0.48M | 6.16 / 13.11 | **3.32 / 5.69** | 3.38 / 6.11 | 5.39 |
| tier 1 | 16 KB | 385 | 8.5k | 0.75M | 12.23 / 25.39 | **5.33 / 8.89** | 5.60 / 9.63 | 5.39-5.52 |
| tier 1 | 64 KB | 891 | 11.3k | 1.14M | 27.01 / 37.85 | **10.10 / 13.24** | 10.59 / 15.88 | 5.32-5.52 |
| tier 1 | 256 KB | 1908 | 14.6k | 1.52M | 44.91 / 61.06 | **15.82 / 18.72** | 16.52 / 20.07 | 5.32-6.09 |
| 1.7M rows | turnloop 200 B | 21 | 277.1k | 2.87M | 267.92 / 485.15 | **10.04 / 56.85** | 9.26 / 72.48 | 6.09-6.29-6.58 |
| 1.7M rows | 200 B | 22 | 265.6k | 2.91M | 259.08 / 478.34 | **10.95 / 56.10** | 12.06 / 72.07 | 5.34-5.63-6.29 |
| 1.7M rows | 1 KB | 71 | 95.3k | 6.18M | 454.98 / 917.76 | **16.90 / 110.21** | 16.57 / 151.42 | 4.71-5.34 |
| 1.7M rows | 4 KB | 177 | 106.1k | 11.31M | 853.01 / 1250.58 | **45.24 / 71.14** | 45.78 / 76.15 | 3.73-3.88-4.71 |
| 1.7M rows | 16 KB | 373 | 203.9k | 17.62M | 1487.73 / 2230.23 | **80.53 / 155.12** | 81.32 / 166.15 | 3.16-3.26-3.73 |
| 1.7M rows | 64 KB | 929 | 309.8k | 27.46M | 2688.38 / 3159.33 | **177.62 / 208.71** | 195.34 / 239.35 | 3.16-3.84-3.93 |
| 1.7M rows | 256 KB | 2220 | 336.9k | 34.50M | 3417.76 / 3793.97 | **249.65 / 333.48** | 271.75 / 332.03 | 3.93-5.83-6.25 |

After the merge (reference: merged `v1` at `d598dfc` plus the probe; final:
`9c28cc7`, P1-P9; P10 and P11 do not change these single-thread numbers
beyond P10's 2% at 256 KB):

| store | query | terms | candidates | postings | reference p50 / p99 | final p50 / p99 | final, exact entropy p50 / p99 | load |
|---|---|---:|---:|---:|---:|---:|---:|---|
| tier 1 | turnloop 200 B | 19 | 8.7k | 0.07M | 2.46 / 5.36 | **0.84 / 1.57** | 0.98 / 2.03 | 3.09 |
| tier 1 | 200 B | 21 | 7.8k | 0.07M | 2.33 / 5.05 | **0.77 / 1.67** | 0.80 / 2.16 | 3.09 |
| tier 1 | 1 KB | 68 | 3.3k | 0.16M | 2.79 / 8.31 | **1.31 / 3.12** | 1.35 / 3.75 | 3.00 |
| tier 1 | 4 KB | 175 | 3.0k | 0.30M | 5.04 / 10.11 | **2.65 / 4.12** | 2.85 / 4.54 | 3.00-3.08 |
| tier 1 | 16 KB | 385 | 4.7k | 0.46M | 9.26 / 17.95 | **4.65 / 7.44** | 4.77 / 7.60 | 3.08 |
| tier 1 | 64 KB | 891 | 6.2k | 0.72M | 18.23 / 24.84 | **8.07 / 11.16** | 8.72 / 11.73 | 3.00 |
| tier 1 | 256 KB | 1908 | 8.2k | 0.96M | 28.92 / 38.61 | **13.02 / 14.84** | 14.29 / 17.03 | 2.92-3.00 |
| 1.7M rows | turnloop 200 B | 21 | 277.1k | 2.87M | 326.29 / 540.24 | **13.34 / 63.19** | 14.15 / 85.08 | 2.86-2.92 |
| 1.7M rows | 200 B | 22 | 265.6k | 2.91M | 319.78 / 541.82 | **14.53 / 63.35** | 17.65 / 82.56 | 2.33-2.45-2.71 |
| 1.7M rows | 1 KB | 71 | 95.3k | 6.18M | 574.02 / 1056.53 | **23.48 / 132.81** | 23.64 / 166.34 | 1.88-2.33 |
| 1.7M rows | 4 KB | 177 | 106.1k | 11.31M | 1033.27 / 1533.59 | **74.61 / 110.66** | 82.11 / 119.51 | 1.81-1.84-2.17 |
| 1.7M rows | 16 KB | 373 | 203.9k | 17.62M | 1822.25 / 2585.80 | **136.32 / 216.38** | 141.02 / 234.02 | 2.56-3.03-3.12 |
| 1.7M rows | 64 KB | 929 | 309.8k | 27.46M | 3023.37 / 3453.86 | **264.95 / 302.93** | 269.31 / 338.46 | 1.82-1.89-3.03 |
| 1.7M rows | 256 KB | 2220 | 336.9k | 34.50M | 3752.97 / 4068.54 | **330.41 / 374.44** | 362.02 / 398.86 | 1.49-1.53-1.83 |

The tier-1 turnloop query (first 200 bytes of the turn's tool result or
user message) went from 2.46 / 5.36 ms to 0.84 / 1.57 ms after the merge.
Every query size at tier 1 is under 20 ms at p99 on one thread; the
largest, 256 KB, is 14.8 ms.

Query threads (wall time, HEAD, quiet machine; each run is a new process,
so its first queries also build skip tables):

| store | query | queries | 1 thread p50 / p99 | 4 threads p50 / p99 | 8 threads p50 / p99 | load |
|---|---|---:|---:|---:|---:|---|
| tier 1 | turnloop 200 B | 200 | 0.84 / 1.54 | - | 0.84 / 1.62 | 1.81 |
| tier 1 | 4 KB | 200 | 2.58 / 3.94 | - | 1.97 / 3.02 | 1.74-1.81 |
| tier 1 | 64 KB | 50 | 7.70 / 10.76 | - | 5.80 / 8.02 | 1.74 |
| tier 1 | 256 KB | 50 | 12.39 / 14.06 | - | 9.45 / 10.77 | 1.74 |
| 1.7M rows | turnloop 200 B | 40 | 13.12 / 63.42 | 2.43 / 31.88 | 2.05 / 24.08 | 1.48-1.52 |
| 1.7M rows | 1 KB | 40 | 23.59 / 128.46 | 6.12 / 55.79 | 4.13 / 42.92 | 1.48 |
| 1.7M rows | 4 KB | 40 | 73.77 / 108.11 | 23.49 / 45.47 | 14.27 / 26.89 | 1.33-1.36 |
| 1.7M rows | 16 KB | 20 | 129.63 / 211.93 | 44.02 / 79.67 | 25.87 / 48.36 | 1.33-1.70 |
| 1.7M rows | 64 KB | 20 | 260.14 / 315.18 | 91.26 / 126.76 | 50.24 / 76.42 | 1.67-1.73 |
| 1.7M rows | 256 KB | 20 | 316.48 / 365.30 | 118.11 / 173.72 | 66.63 / 104.65 | 1.62-1.81 |

Tier-1 queries under 200,000 postings (all 200 B ones) stay on one thread
by design (`PARALLEL_POSTINGS`). With 8 threads the 1.7M store answers
turn-loop queries in 2.1 ms at p50 and 24 ms at p99 (15.7 ms at p99 over
100 queries in an earlier run, load 2.5); long queries at that scale stay
well above 20 ms (64 KB: 50 / 76 ms; 256 KB: 67 / 105 ms).

## Where the time goes now and what remains

Final profile, merged tier-1 store, one thread (CPU, load 1.6-6):

| phase | 200 B | 4 KB | 64 KB | 256 KB |
|---|---:|---:|---:|---:|
| total | 1.23 ms | 3.6 ms | 9.7 ms | 12.5 ms |
| scan, first pass (candidates) | 23% | 3% | 1% | 1% |
| scan, second pass (anchored terms) | 9% | 52% | 54% | 49% |
| bounds and exact scoring (TopK) | 22% | 6% | 12% | 15% |
| query parsing | 1% | 2% | 9% | 16% |
| dictionary lookups | 5% | 8% | 9% | 10% |
| top-k select and ids | 29% before P9, ~2% after | 24% / ~1% | 8% / ~1% | 6% / ~1% |

(200 B to 64 KB: the P8 build at load 6; 256 KB: the P10 build at load
1.6; P9 removed the archive reads from the select phase.) Long queries are
now bound by decoding the postings of every query term (Rice, 2-6 ns per
posting) and touching each hit once or twice; the background model needs every term's count among the
candidates, so these lists cannot be skipped without changing the model.
On the 1.7M store one thread reads 2.9M postings for a 200 B query and 27M
for 64 KB: the floor is set by posting volume, and only query threads move
it (above).

What would go further, in order of expected gain:

1. **Query threads for TraceDb** (exact, measured above): `qthreads=4` or
   8 for stores past ~500k rows. TraceDb defaults to 1 (16 agents share the
   cores); the per-query threshold keeps small queries on one thread.
2. **Skip and block-max data on disk** (format change): per-block skip
   entries and per-block max tf in the segment (built at flush and merge),
   so lookups and bounds need no first decode and survive restarts.
3. **A query-length-adaptive model with a precomputable background**
   (ranking change, needs the dev/held-out protocol on the turn-loop split
   and BEIR): with block data on disk, the background would be known before
   the scan and block-max pruning could skip whole blocks. E12 suggests
   the gain is limited for 150+ term queries; worth one experiment.
4. Smaller: a persistent query thread pool (each parallel phase spawns
   scoped threads today, ~20-50 us per thread per phase), and the
   identifier backoff check reusing the sorted dictionary lookups.

## Commands for the final timed rerun

`scripts/perfsearch/rerun.sh` does everything below on a quiet machine
from the repository root at this branch's head; the reference is `v1` plus
`scripts/perfsearch/reference-probe.patch` (TraceDb accessors and the
probe, no engine change):

```bash
DATA=$TRACEDB_DATA ROUNDS=3 QTHREADS="1 8" \
  scripts/perfsearch/rerun.sh
python3 scripts/perfsearch/table.py .scratch/perfsearch/results/probe.jsonl
```

It (1) builds both engines, (2) builds the tier-1 per-agent stores and the
1.7M-row single store (`--no-dedupe`), (3) checks exactness on all 30,905
tier-1 turn-loop queries (`compare` must report identical lines with
`--entropy all`, and the same ids and scores in the TraceDb setting), (4)
runs the probe for every query size on both stores, arms alternating,
ROUNDS rounds, each run under the bench lock and a 3 GiB scope, and (5)
runs the `turnloop` suite for both engines. Single steps:

```bash
cargo build --release -p oojuro-tracebench
B=target/release W=.scratch/perfsearch/stores DATA=$TRACEDB_DATA
$B/searchprobe build --data $DATA/base1m.tbe --frac 0.1 --dir $W/t1
$B/searchprobe build --data $DATA/base1m.tbe --tier 2 --single --no-dedupe --records 1700000 --dir $W/single
$B/searchprobe run --dir $W/t1 --size 0 --dump final.txt            # all turn-loop queries
$B/searchprobe run --dir $W/single --size 65536 --queries 20 --qthreads 8 --out probe.jsonl
$REF/searchprobe run --dir $W/t1 --size 0 --dump ref.txt         # REF: v1 + reference-probe.patch
$B/searchprobe compare ref.txt final.txt
cargo build --release -p oojuro-tracebench --features phase-profile  # adds profile_per_query
```

## Later change: anchored lists continue (2026-10-21)

`SEARCH_FIXES.md` (F2): a query whose anchored view holds fewer than k
records now continues with its unanchored ranking. On all 30,905 tier-1
turn-loop queries, 25,950 stay bit-identical and 4,955 (that returned fewer
than 20) keep their list as an exact prefix, filled to 20
(`scripts/perfsearch/prefix_compare.py`). Turn-loop 200 B CPU p50 / p99
1.06 / 1.86 -> 1.20 / 2.35 ms (load 5-9), all of it in the continued
queries.

F3 (same doc): a candidate background below 64 x mu of mass is padded with
whole-view text. All 25,220 unpadded tier-1 turn-loop queries stay
bit-identical, and the 5,685 padded ones (18%) score on the normal scale.

## Risks

- **Routing entropy in TraceDb now covers the top k** (P6). Ids, scores and
  order are unchanged, but `Matches::routing_entropy_bits` from a TraceDb
  store differs from the engine default. TraceDb returns ids only and
  writes no receipts, so nothing reads it today; `exact_entropy=true`
  restores the old value. `Nine`'s default is unchanged.
- **Ids from the row prefix trust a 128-bit identity fingerprint** (P9):
  an id of up to 8 bytes is accepted when the segment's id table maps that
  string's fingerprint to the same row. A wrong id would need a 128-bit
  BLAKE3 collision between two ids of one segment.
- **Memory**: skip tables (32 B per 128 postings, 0.25 B per posting, of
  each list of >= 1,024 postings searched at least twice or scanned in
  parallel), id caches (<= 1,024 ids per segment), the per-thread stem
  cache (<= 65,536 words). None is bounded
  by a byte budget yet, and skip tables live as long as their segment.
- **First queries pay**: skip tables are built on a list's second use (and
  before parallel scans), so a cold store's first queries are slower than
  the steady state shown here; each probe run is a fresh process, so the
  tables include that cost.
- **Version 1 stores** take a separate decode path (LEB128 plus P4's fast
  path); tested against the v3 oracle with a test-only writer switch, but
  not timed on a real old store.
- **Timings are indicative**: shared machine, load 1.3-43 across this
  work, CPU time used to reduce noise. The query-thread numbers are wall
  time at load ~1.5 and depend on idle cores; with 16 agents searching at
  once the speedup will be smaller.
- **The 1.7M store is a replicated tier** with dedupe off: every text
  appears twice, which inflates candidate counts relative to a real 1.7M
  store of distinct records.
