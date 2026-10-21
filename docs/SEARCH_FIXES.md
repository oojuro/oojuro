# Search fixes F1-F4 (outcome eval, 2026-10-21)

Two search bugs found by the outcome eval ("Product findings"), fixed on a
branch of the v1 line (base `5b9f72f`), plus two follow-ups from this work's
own findings: F3, small candidate sets scored near 0 bits, and F4, one tie
rule (sections below). Both bugs reproduce on a copy of the eval's per-task
store for sympy-23413 (14,031-record public sympy writer), queried with the
issue text of an earlier task, `-k 100`.

| query (store: sympy-23413 task store) | before | after |
|---|---:|---:|
| sympy-16792 issue, no filter | 1 hit | 100 (issue record first) |
| same, `autofunc_result` written as `autofunc result` | 100 | 100 (same list as the row above) |
| sympy-14976 issue, `--to` 1 s before the issue | 0 | 100 (= `--as-of` at that time, ids and bits) |
| sympy-14976 issue, `--as-of` 1 s before the issue | 100 | 100 (unchanged) |
| sympy-14976 issue, `--to` / `--as-of` the issue time | 1 / 1 | 100 / 100 (equal) |

## F2: a near-verbatim record starves the list

**Cause: identifier anchoring, not the ranking formula.** A query naming
an identifier (a word with `_`) that occurs in its historical view ranks
only the records containing it: the union of those identifiers' postings
is the whole candidate set ("anchored retrieval",
`NINE_RETRIEVAL_IMPROVEMENTS.md` section 2). The only identifier in
sympy-16792's issue is `autofunc_result`, and in the store it occurs in one
record: the issue itself. So the candidate set is that record, and the list
ends after it. Before that record existed the identifier had no occurrence,
so the query fell back to its words and got 100 hits. Adding or removing
half of the text changes nothing while the identifier stays in. In live
injection the recorded prompt is exactly such a record: any identifier the
user types anchors the search to records naming it, and the prompt itself is
then dropped as still in context.

The record's 0.73 bits (787 bits when the query is read as words) is a
side effect: the background is estimated from the candidate set, and a
candidate set of one record is that record, so it saves almost nothing.
The other suspects were ruled out. `rank` has no minimum-score filter.
The exact top-k (P6) prunes only below the k-th best of at least k
candidates, and its test compares it with exhaustive scoring. Skip tables
change how lists are read, not the candidate set.

**Fix (`crates/oojuro-nine`, v4 and the v3 oracle).** When the anchored
view holds fewer than k records, the list continues past it with the
query's unanchored ranking and skips rows already listed. The unanchored
ranking reads every identifier as its component words, exactly the ranking
the query gets when no identifier occurs. These matches carry
`SourceMatch::continued` (serialized only when true), and
`TimelineMetrics::continued_matches` counts them. Their `score_bits` and the
routing entropy come from the unanchored ranking. The anchored part keeps
its order and bits, so:

- a list whose anchored view has at least k records is unchanged;
- a shorter list is a prefix of a longer one (for any k);
- quoted phrases and key addressing are still filters (the caller asked
  for them), and are not continued.

Nothing tests for near-duplicates. The near-verbatim record ranks first
because it is anchored: it names the query's identifiers.

`TraceDb::search_where` returns `SearchHit { id, bits, continued }`.
`Journal::search` merges writers by bits, but a store's continued hits never
move above that store's anchored hits. When nothing is continued, the merge
gives exactly the old sort (a randomized unit test with ties checks this).

## F1: `search --to` ranked records it then dropped

**Cause.** `Journal::search` gave the engine the view `[from, as_of]` and
applied `to` only in the filter closure. The engine's statistics and its
identifier anchoring therefore saw records newer than `to`. For
sympy-14976, `_lambdifygenerated` occurs only in records after the issue,
so the anchored view held only records that the filter then dropped. The
engine had returned fewer than `want`, so the retry loop stopped with 0
hits. The loop also stopped at 10,000 candidates. A filter the engine cannot
evaluate (kinds, keys, `outside_context_of`) that rejects more than 10,000
better matches ended the search early.

**Fix.** `[from, to]` (with `to = min(to, as_of)`) is the engine's view, so
`--to T` searches what the window holds, as `--as-of T` does. Rows are
immutable with `recorded_at = valid_at`; for them the two are identical,
and a key's newest version is still taken as of `as_of`, in the filter.
`TraceDb::search_where` asks the engine for 4x more until it has k hits or
the engine returns fewer than asked (it has no more). There is no cap, and
`Nine::find` takes limits above the public 10,000-result `Query` cap.

## Tests (written first; each failed before its fix)

- `oojuro-nine` `an_anchored_view_smaller_than_k_continues_with_the_unanchored_ranking`:
  a long query whose identifier occurs only in a near-verbatim record, for
  k from 1 to 500. Checks: full lists; that record first; after it the
  unanchored ranking's ids and bits; prefix across k; unchanged before the
  record existed; an identifier with 4 occurrences unchanged for k <= 4;
  v3 oracle equal. Before: 1 result at k=2.
- `oojuro-trace` `filters_outside_the_engine_never_end_the_search_early`:
  the only events a `keep` filter accepts rank below 10,045 better ones.
  Before: 0 hits.
- `oojuro` `search_windows_are_engine_views_and_lists_are_never_cut_short`:
  `--to` before and at the issue equals `--as-of` (ids and bits), the issue
  first, lists filled. Before: 0 hits. Plus `journal::tests::merging_sorted_lists_*`.
- Changed: v3's `normalized_words_preserve_literal_identifiers_originals_and_time`
  asserted that `running_workers` at k=50 returns only `exact`, which is
  the cut-off. It now asserts `exact` first, then the continued word
  ranking (and only `exact` at k=1).

Suites: `oojuro` 45, `oojuro-capture` 29, `oojuro-proxy` 22 (1 ignored),
`oojuro-trace` 11 (1 ignored), `oojuro-nine` 88, all passing (after F3 and
F4: `oojuro` 47 and `oojuro-nine` 89; the others unchanged). The 88
include the randomized v4-vs-v3 oracle, TopK-vs-exhaustive and
version-1-segment tests, which now exercise the continuation.
`tracebench verify` on smoke: 1,237/1,237
for `oojuro` and `oojuro:log=private`.

## Ranking where the bug does not apply

Exactness (`searchprobe`, tier-1 per-agent stores built from base1m as in
`PERF_SEARCH.md`, k=20, base 5b9f72f vs fix,
`scripts/perfsearch/prefix_compare.py`):

| queries | identical (ids, bits, entropy) | continued | other |
|---|---:|---:|---:|
| all 30,905 turn-loop (200 B), TraceDb setting | 25,950 | 4,955 | 0 |
| same, `--entropy all` | 25,950 | 4,955 | 0 |
| 200 at 1 / 4 / 16 KB | 171 / 190 / 197 | 29 / 10 / 3 | 0 |
| 200 at 64 / 256 KB | 200 / 200 | 0 | 0 |

Every changed query returned fewer than 20 results before (4,955 of 30,905
turn-loop queries, 16%, were cut short). Its old list is now an exact
prefix (ids and bits) of the new one, filled to 20 (60,107 added results).
Every query with 20 results before is bit-identical, entropy included.

Quality (shared harness `oojuro-bench`, dev splits, arm `nine`. Base was
run as `cmd:` with the base binary; both arms print commit 5ad3dad because
the commit is read from git at build time):

| suite | metric | base | fix |
|---|---|---:|---:|
| BEIR SciFact dev (93 q) | nDCG@10 / R@100 | 0.677 / 0.894 | 0.677 / 0.894 |
| BEIR NFCorpus dev (102 q) | nDCG@10 / R@100 | 0.293 / 0.243 | 0.293 / 0.243 |
| BEIR FiQA dev (219 q) | nDCG@10 / R@100 | 0.288 / 0.581 | 0.288 / 0.581 |
| LongMemEval dev | turn nDCG@10 / R@50 | 0.696 / 0.885 | 0.696 / 0.885 |
| turnloop swe-agent dev (50 traj., 20k background, 239 labelled turns) | nDCG@10 | 0.235 | 0.246 |
| | nDCG@20 | 0.267 | 0.284 |
| | recall@20 | 0.477 | 0.522 |
| | hit@20 | 0.544 | 0.586 |
| | MRR@10 | 0.219 | 0.224 |

Natural-language sets are unchanged (their queries rarely name
identifiers). On code trajectories every gain comes from lists that used to
be cut short. Earlier results keep their places, so no per-query metric can
drop. All gates pass (byte-exact, no future leak, freshness, determinism).
The identifier-location probes of `NINE_RETRIEVAL_IMPROVEMENTS.md` (private
corpus) were not rerun. Their references sit in the anchored part, which
keeps its order; a continuation only adds text after it.

## Latency (tier 1, one query thread, CPU ms p50 / p99, median of 3 rounds)

Run under `flock $OOJURO_BENCH_LOCK`, `nice 19`, 3 GiB scope,
arms alternating. Load 4.7 to 8.9 (shared machine).

| query | base | fix |
|---|---:|---:|
| turn-loop 200 B (1,000 q) | 1.06 / 1.86 | 1.20 / 2.35 |
| 200 B | 0.83 / 2.02 | 1.18 / 2.38 |
| 1 KB | 1.32 / 3.58 | 1.65 / 4.81 |
| 4 KB (200 q) | 3.12 / 5.70 | 3.07 / 8.06 |
| 16 KB | 5.80 / 9.69 | 5.79 / 10.61 |
| 64 KB (100 q) | 9.16 / 13.60 | 9.29 / 14.75 |
| 256 KB | 15.24 / 21.94 | 14.86 / 20.68 |

The cost is in the queries that used to be cut short. Split per query
(turn-loop 200 B, 3 rounds):

- the 864 unchanged queries per round: 1.15 / 2.17 to 1.17 / 2.39;
- the 136 continued ones: 0.28 / 0.83 to 1.65 / 3.02. They used to rank
  one candidate; now they also rank the query as words, about the cost of a
  query that never anchored.

## Reproducing

Aggregates are in `benchmarks/search-fixes-2026-10-21/`. Builds: the fix at
this branch, and the base from `git archive 5b9f72f` in its own
`CARGO_TARGET_DIR`. A shared target dir silently reuses the fix's artifacts
for an archived copy, because path crates hash by workspace-relative path.

```bash
B=.scratch/fix; R=.scratch/base; W=.scratch/perfsearch; DATA=$TRACEDB_DATA
$B/searchprobe build --data $DATA/base1m.tbe --frac 0.1 --dir $W/stores/t1
$R/searchprobe run --dir $W/stores/t1 --size 0 --dump base-0.txt   # and $B/... --dump fix-0.txt
python3 -I scripts/perfsearch/prefix_compare.py base-0.txt fix-0.txt
flock $OOJURO_BENCH_LOCK $B/searchprobe run --dir $W/stores/t1 --size 0 --queries 1000 --out probe.jsonl
$B/oojuro-bench turnloop --set swe-agent --split dev --trajectories 50 --background 20000 --runs 1 \
  --arm "base=cmd:$PWD/$R/oojuro-bench serve --arm nine" --arm fix=nine
$B/tracebench verify --arm oojuro:log=private --data $DATA/smoke.tbe
```

## F3: small candidate sets scored near 0 bits (follow-up)

**Cause.** The default background is estimated from the candidate set:
PB(w) = (count of w among candidates + P0(w)) / (candidate mass + 1). In
the research this acts as query-length-adaptive smoothing:
`TRACK_LEXICAL.md` E2 shows that the term gains see mu * view mass /
candidate mass. A whole-view background with one mu won the short dev
queries but lost held-out ArguAna (long queries), so it was rejected as
the default. The adaptivity has no lower limit on the candidate mass,
though. When the candidates are a few records, the background is mostly
the records being scored, and they save almost nothing. With one
candidate, each present term's gain log2(1 + tf/(mu PB)) equals that
term's share of the length penalty, so the score is about 0 bits however
well it matches. This happens for an identifier with few occurrences
(anchored), a quoted phrase, or rare words. The injection hook then drops
exactly the best hit (`min_bits` 8).

**Fix (`crates/oojuro-nine`, v4 and the v3 oracle).** The candidate
background is never estimated from less than
`BACKGROUND_FLOOR_RECORDS` (64) x mu of mass (8,192 scope or length units
at mu 128). A smaller candidate set is padded up to that mass with
whole-view text, in proportion to the view's own statistics:

```text
r      = 64 * mu - M_C                      (only when M_C < 64 * mu)
PB(w)  = (c_C(w) + r * c_V(w) / M_V + P0(w)) / (M_C + r + 1)
```

`c_C` and `M_C` are the candidate counts and mass the background always
used. `c_V` and `M_V` are the same counts over the query's view: the same
two clocks, lower bound and current revisions, so a later row changes
nothing. Fully visible segments read them from their dictionaries; other
segments decode the term's postings. Candidate sets at or above the floor
use the old expression, bit for bit. Terms count in the penalty exactly as
before. The floor is continuous (r goes to 0 at the floor). It caps the
adaptive smoothing at mu_eff of about M_V / 64 instead of letting it grow
without bound. The 64 was fixed before any eval ran and was not swept.
`TimelineMetrics::background_pad` reports r.

**Tests.** `a_small_candidate_set_is_scored_against_a_padded_background`
covers a single anchored candidate, a single rare-word candidate, no
future rows, and no padding above the floor, against the v3 oracle. The
single anchored candidate scores 44.7 bits (52.9 for the same record read
as words). With the floor at 0 the test fails at 0.12 bits. The v3 dense
query-likelihood oracle and the hand-computed probabilities now compute
the padding independently, over historical views.

**Exactness** (tier-1, base = v1 engine; `scripts/perfsearch/pad_compare.py`):

| queries | unpadded: identical | padded | padded, same top hit | padded, same ids |
|---|---:|---:|---:|---:|
| 30,905 turn-loop (both entropy settings) | 25,220 / 25,220 | 5,685 | 5,128 | 2,611 |
| 200 at 1 / 4 / 16 / 64 / 256 KB | 162 / 188 / 194 / 198 / 200, all identical | 38 / 12 / 6 / 2 / 0 | 36 / 12 / 6 / 2 / - | 14 / 4 / 2 / 0 / - |

**Score scale** (top hit of each turn-loop query, bits):

| | p10 | p50 | p90 | at least 8 bits |
|---|---:|---:|---:|---:|
| unpadded (unchanged) | 16.8 | 42.4 | 84.5 | 97.5% |
| padded, before | 0.0 | 8.1 | 29.7 | 50.4% |
| padded, after | 4.8 | 21.8 | 47.6 | 86.1% |

On the outcome-eval store (sympy-23413), the sympy-16792 near-verbatim
issue goes from 0.7 to 521.6 bits (787.3 when the query is read as words),
and the sympy-14976 issue at `--to` its time from 0.5 to 270.3. The
continued hits after them are unchanged.

**Quality** (v1 engine vs floor, dev splits, all metrics at full precision):

| eval | v1 | floor |
|---|---|---|
| BEIR SciFact (93 q) | nDCG@10 0.67671, R@100 0.89355 | equal on all 4 metrics |
| BEIR FiQA (219 q) | nDCG@10 0.28791, R@100 0.58092 | equal on all 4 metrics |
| BEIR NFCorpus (102 q) | nDCG@10 0.29322, R@10 0.15505 | nDCG@10 0.29253, R@10 0.15488; MRR, R@100 equal |
| LongMemEval | turn nDCG@10 0.69620, session R_all@5 0.85887 | 0.69692, 0.86207; others equal |
| turnloop swe-agent (239 turns) | nDCG@10 0.24636, nDCG@20 0.28356, R@20 0.52176, hit@20 0.58577, MRR@10 0.22424 | 0.26071, 0.29477, 0.53431, 0.59833, 0.23228 |

NFCorpus is the one drop, and it comes only from padded queries: 18 of
its 102 dev queries are 1-2 word queries with 2 to 53 candidates. Six
moved. `sweeteners` (34 candidates) fell 0.852 to 0.789, and `anisakis`,
`serotonin` and `BPH` fell 0.003 to 0.012. `magnesium` and `cinnamon`
rose. For a one-word query the floor changes the tf-versus-length balance
inside the small set, so ranks shift either way. Normal sets are
bit-identical everywhere.

**Latency** (tier 1, one query thread, CPU ms p50 / p99, median of 3
rounds, arms alternating under the bench lock). Other sessions saturated
the machine during this run (load 9 to 274), so these are indicative
only:

| query | v1 | floor |
|---|---:|---:|
| turn-loop 200 B (1,000 q) | 1.41 / 3.25 | 1.27 / 2.83 |
| 200 B | 1.27 / 2.92 | 1.77 / 3.42 (one round at load 45) |
| 1 KB | 2.39 / 6.92 | 2.47 / 7.23 |
| 4 KB (200 q) | 4.53 / 9.83 | 4.17 / 9.46 |
| 16 KB | 6.06 / 11.86 | 6.21 / 10.85 |
| 64 KB (100 q) | 9.94 / 15.72 | 10.29 / 15.65 |
| 256 KB | 14.73 / 19.53 | 15.18 / 21.56 |

Split per turn-loop query: the 834 unpadded queries per round go from 1.44
/ 3.34 to 1.24 / 2.74, and the 166 padded ones from 1.75 / 4.32 to 1.55 /
3.09. Both groups move by the same 12%, which is load drift, so the
padding has no cost visible at this noise level. With every segment
visible, the padding adds an O(segments) view mass and one dictionary
lookup per retained term and segment. Only historical views decode
postings. `tracebench verify` on smoke: 1,237/1,237 for `oojuro` and
`oojuro:log=private`.

## F4: one tie rule (follow-up)

Equal scores rank newer first, then by id ascending as text (decimal ids
compare as strings, "10" < "9"). This is the engine's own order
(`select_top`). `SearchHit` carries the matched row's time and id, and
`SearchHit::rank_cmp` is the rule. The journal merges the stores' lists in
that order and no longer re-sorts them (it used to re-sort ties by numeric
id). So within a store, and across stores, a smaller k is always a prefix
of a larger one. The injection hook breaks its ties the same way.
`tied_scores_rank_the_same_for_every_k_and_across_writers` checks 24
exactly tied records over two writers, with repeated times and ids that
cross digit counts, for every k. It fails on the old merge. A randomized
merge test and a decimal-text order test (20,000 pairs against
`to_string`) cover the rest.

## Noticed, not changed

- Deduplicated text: a row's time is its first stored event's time. A
  duplicate event older than its row (out-of-order timestamps) cannot be
  found in a window or view that ends before the row: that was already true
  for `as_of`, and for `to` while the stored event is live. Pushing `to`
  into the engine adds one case: the stored event deleted, its older
  duplicate inside the window (it was found before). Fixing it means
  giving a row the time of its earliest live event.
