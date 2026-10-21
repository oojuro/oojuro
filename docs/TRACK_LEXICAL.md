# Lexical track log (`track/lexical`)

Running log for the non-hybrid engine core (`crates/oojuro-nine`). Each
experiment records hypothesis, change, scorecard delta and keep/reject.
Rejected code is kept as a patch under `benches/experiments/`.

Scope (narrowed 2026-10-20): per-turn group
commit, zero-lag fresh segment plus immutable segments, concurrent readers
without per-search fsync, query cost (sparse accumulation, top-k pruning),
compact postings and memory, data larger than RAM for the archive tier, the
ranking-math background fix, and the thinking-token indexing test. Heavy
compression belongs to `track/compress`; this track keeps a narrow storage
boundary (append batch, get, get range, scan by time) that its codec can
implement. Final comparisons use the shared `track/bench` harness.

## Gates (never traded for a metric)

1. Byte-exact roundtrip of every original, checked with BLAKE3.
2. No future leakage in as-of queries (two clocks: `valid_at`, `recorded_at`).
3. Crash recovery: torn tail and header corruption.
4. Deterministic results.
5. Old-format stores stay readable (versioned magic plus reader/migration).

## Frozen evaluation splits (fixed before any tuning)

Tuning (dev) data: SciFact `train` (809 queries), FiQA `dev` (500),
NFCorpus `dev` (324). Held out, never used to choose parameters: the `test`
split of SciFact (300), FiQA (648), ArguAna (1,406), SciDocs (1,000) and
NFCorpus (323). ArguAna and SciDocs have no dev split and are held out
entirely. BEIR archives were fetched from the official UKP mirror; all five
MD5s match the BEIR dataset list (scifact `5f7d1de6…`, fiqa `17918ed2…`,
arguana `8ad3e3c2…`, scidocs `38121350…`, nfcorpus `a89dba18…`).

Scoring: `scripts/lexical_beir_eval.py` (pytrec_eval-terrier). Documents are
`title + "\n" + text`. Results whose id equals the query id are dropped
before scoring (BEIR `ignore_identical_ids`). Unlike BEIR's averaging, every
labelled query counts in the denominator, including queries with no results
and ArguAna's five queries whose positives are missing from the corpus.

## Review of the `perf/*` branches (read-only)

All ten branches fork from `9280e4a`; `main` (`7b98903`) already carries
the memoized two-clock visibility from `f35a4ab`.

| Branch | Change | Verdict for this track |
| --- | --- | --- |
| stream-replay (`ce80df4`, `0b14579`) | `Journal::open_visit` streams query receipts | Sound. The v4 replay streams every journal. |
| stream-source-replay (`c20e6fc`) | Streams source frames straight into the index | Sound; same idea, superseded by segment open without replay. |
| owned-term-move (`87a2fb8`) | Moves owned terms into postings (no clone) | Trivially sound; subsumed. |
| compact-postings (`5a45a2f`) | `Vec::with_capacity(1)` for singleton lists | Sound micro-fix; subsumed by encoded postings. |
| resident-posting-layout (`cef0a9e`) | `(u32,u32)` postings with `usize` fallback; HPC census | Census is the key finding: per-record `Record`, metadata `BTreeMap` nodes and `usize` postings dominate RSS, not text. Motivates moving originals and metadata out of RAM. |
| sparse-query-scratch (`bc23285`) | HashMap scratch instead of O(N) arrays | Right goal; the v4 query path accumulates per segment, document-at-a-time, with no O(N) arrays and no hash per posting. |
| prepared-journal-frame (`6970c84`) | Reuses the selected payload, avoiding a second serialize+compress | Sound; obsolete once frames are binary. |
| journal-preallocation-proposal (`f360632`) | Proposes extent reservation | Not adopted; group commit removes the per-record sync it targets. |
| ingest-profile (`e6a81f2`, `d678be0`) | Attributes 93.3% of insert time to `sync_data` | Key evidence for group commit. |
| fresh-beir | BEIR harness/reference scripts | Read for methodology; the shared harness replaces it. |

## Experiment log

(entries appended below, newest last)

### E1. v4 core: group-commit journal, fresh + immutable segments (kept)

Hypothesis: one sync per batch, a searchable in-memory fresh segment and
mmapped immutable segments keep every gate while removing per-record
sync cost, per-search sync, whole-store replay and resident originals.

Change: `Nine` is the v4 engine (`src/v4.rs`, `wal.rs`, `segment.rs`,
`archive.rs`, `codec.rs`); v3 stays as `legacy::V3` (reader, migration,
oracle). Read APIs return owned values.

Gates: v4 equals the v3 oracle bit for bit (ids, scores, background bits,
conditional bits; entropy within 1e-9) on randomized multi-segment
workloads with versions, as-of views, lower bounds, literals, identifiers,
key addressing, ties and feedback, live and after reopen. One deliberate
deviation: a bare query equal to a key that only exists after the as-of
time no longer switches to key addressing (v3 leaked future writes into
past results). Journal tests cover torn tails at every byte, zero-filled
tails, header/length/flag/sequence corruption (error, file untouched),
atomic multi-frame batches and poisoning. v3 stores migrate without
modifying their files. BEIR runs are byte-identical to main.

Bug found on the way (also on main): `oojuro_core::entropy` produced NaN
when candidate scores span more than ~1074 bits; v3 then wrote a `null`
receipt and the store could no longer be opened. Fixed (p = 0 terms
contribute 0); legacy null receipts are readable. Fixed on a branch; main untouched.

### E2. Ranking background over the whole eligible view (dev selection)

Hypothesis: v3 normalizes the background by the candidate set, so the
effective smoothing is mu * view/candidates (136,015 vs 128 on a rare
query in the review). Using every eligible record of the historical view
(still no future rows) is the standard Dirichlet model and should rank
better, and it is a prerequisite for top-k pruning.

Change: `Options { background: View, mu }`. Exact view mass via segment
time spans plus superseded-version subtraction; verified against a dense
oracle across as-of views, lower bounds, versions and segments.

Dev nDCG@10 (tuning splits only; test untouched):

| Config | SciFact train | FiQA dev | NFCorpus dev | Mean |
| --- | ---: | ---: | ---: | ---: |
| candidates, mu 128 (v3) | 0.6981 | 0.2733 | 0.2867 | 0.4194 |
| view, mu 32 | 0.6749 | 0.2518 | 0.2802 | 0.4023 |
| view, mu 64 | 0.6833 | 0.2638 | 0.2858 | 0.4110 |
| view, mu 128 | 0.6941 | 0.2718 | 0.2864 | 0.4174 |
| view, mu 256 | 0.7014 | 0.2707 | 0.2896 | 0.4206 |
| view, mu 512 | 0.7056 | 0.2683 | 0.2900 | 0.4213 |
| view, mu 1000 | 0.7023 | 0.2544 | 0.2900 | 0.4156 |
| view, mu 2000 | 0.6969 | 0.2403 | 0.2893 | 0.4088 |
| view, mu 4000 | 0.6885 | 0.2215 | 0.2879 | 0.3993 |

Selection rule (fixed before any test-split run): the config with the
best mean dev nDCG@10, view mu 512. The held-out comparison is
candidates/128 vs view/512 on all five test splits.

Held-out test (pytrec_eval; all labelled queries counted):

| Test split | candidates/128 (v3) nDCG@10 | view/512 nDCG@10 | v3 R@100 | view R@100 |
| --- | ---: | ---: | ---: | ---: |
| SciFact | 0.6974 | 0.6945 | 0.9092 | 0.9199 |
| FiQA | 0.2530 | 0.2529 | 0.5441 | 0.5533 |
| ArguAna | 0.5453 | 0.5102 | 0.9595 | 0.9758 |
| SciDocs | 0.1539 | 0.1556 | 0.3614 | 0.3665 |
| NFCorpus | 0.3244 | 0.3276 | 0.2445 | 0.2464 |
| Mean | 0.3948 | 0.3882 | 0.6037 | 0.6124 |

Decision: **rejected as default** (held-out mean nDCG@10 -1.7%, driven by
ArguAna -6.4%); kept as an option. Recall@100 improves on all five.
Interpretation, not used for selection: v3's normalizer is a
query-adaptive smoothing mass, mu * view / candidates. Long queries
(ArguAna) cover almost every document, so v3 behaves like mu 128 there;
short dev queries cover little, so their effective mu is much larger. A
single mu tuned on short-query dev sets cannot reproduce that. The dev
splits contain no long queries, so any query-length-dependent model needs
a long-query dev set (the agent turn-loop labels from track/bench) before
it can be selected without touching ArguAna.

Consequence for query cost: the v3 normalizer needs the full union of the
query's posting lists, so the default model cannot use top-k pruning;
pruning applies to the view model only until a view-family model wins on
held-out data.
