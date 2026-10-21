# Search fixes F1 and F2 (outcome eval, 2026-10-21)

Two search bugs found by the outcome eval ("Product findings"), fixed on a
branch of the v1 line (base `5b9f72f`). Both reproduce on a copy of the eval's per-task store for
sympy-23413 (14,031-record public sympy writer), queried with the issue text
of an earlier task, `-k 100`.

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
`oojuro-trace` 11 (1 ignored), `oojuro-nine` 88, all passing. The 88
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

## Noticed, not changed

- Tiny anchored views score near 0 bits (the candidate-set background is
  the record itself). The injection hook drops hits under `min_bits` (8),
  and anchored hits with few occurrences can fall under it.
  `Background::View` would avoid it, but it changes rankings, so it needs
  the dev/held-out protocol.
- Ties: the engine orders equal scores by newest first, then the journal
  re-sorts them by id. With k smaller than a tie group, a smaller k can
  pick a different member than a larger k would put first (seen while
  writing the journal test; it predates this change).
- Deduplicated text: a row's time is its first stored event's time. A
  duplicate event older than its row (out-of-order timestamps) cannot be
  found in a window or view that ends before the row: that was already true
  for `as_of`, and for `to` while the stored event is live. Pushing `to`
  into the engine adds one case: the stored event deleted, its older
  duplicate inside the window (it was found before). Fixing it means
  giving a row the time of its earliest live event.
