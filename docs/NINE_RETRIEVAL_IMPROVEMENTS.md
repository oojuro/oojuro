# Sparse scoring and exact identifier retrieval

Experiment date: 2026-10-19. These changes address measured latency and relevance
failures in the [large-corpus experiment](NINE_SCALE_BENCHMARK.md). Nine still
requires no embedding model, LLM or external database. Original bytes and the
native archive format are unchanged.

## Frozen sources and complete query measurements

Every completed requery opens the same 18,495-record source store, checks every
SQLite record against the recovered native original, and checks the native
archive digest before and after the run. Query journals append normally. Each
timing includes uncached retrieval, a synced receipt, exact source expansion and
serialized evidence construction. Each arm has three timed repeats after a
warm-up. SQLite and Nine execution order alternates.

This is an in-process single-client Windows comparison on the earlier 150 MB
text corpus. It is not a hosted or out-of-RAM comparison, and no large-corpus
PostgreSQL, pgvector, Tantivy or Memvid rerun is included here. Storage write
performance was not rerun after these index changes.

## 1. Equivalent sparse scoring

For query counts c(w), source counts tf(w,d), source feature length n(d),
historical background PB(w) and mu=128, the previous score has this identity:

```text
L(q|B) - L(q|d)
  = sum(w present in d, c(w) * log2(1 + tf(w,d)/(mu * PB(w))))
    - length(q) * log2(1 + n(d)/mu)
```

The implementation initializes the shared length penalty once per candidate and
updates scores only from nonzero postings. It reuses posting-derived background
counts, checks historical eligibility once per addressed source, and sorts only
the requested result prefix. Per-record token maps are no longer retained after
indexing. This removes duplicated structures; an isolated memory reduction was
not measured.

A dense-equation regression test checks scores within 1e-10 across historical
views. The sparse-only run preserves all 29 old questions' top-512 lists and
evidence packets. It is a mathematical re-expression of the previous retrieval
model, not a new information-theory theorem.

| Historical query arm | Before p50 ms | Sparse p50 ms | Reference pairs | Mean evidence bytes |
| --- | ---: | ---: | ---: | ---: |
| Nine, k=8, 16 KiB | 69.918 | 9.962 | 18/21 | 5,561 |
| Nine, k=128, 1 MiB | 71.022 | 10.589 | 20/21 | 130,334 |
| SQLite FTS5, k=128, 1 MiB | 104.305 | 104.345 | 20/21 | 954,839 |

These are sequential runs on the same machine. The repeated SQLite measurement
helps identify system drift but does not turn this into a hardware-independent
speed guarantee. Current temporary source-addressed arrays cost O(N) to
initialize; source and derived indexes are still memory resident.

## 2. Exact names and a stronger SQLite baseline

The old tokenizer split `repair_shadow_page` into three unrelated words. The new
index retains the full identifier as well as those components. A query naming an
identifier with eligible historical occurrences uses those occurrences to anchor
its candidate view. Other query words refine relevance within that view. Multiple
known names use the union of their postings. If a whole name is absent in that
historical view, component-word retrieval remains available. (Since
2026-10-21, `SEARCH_FIXES.md` F2: when the anchored view holds fewer than k
records, the list continues past it with that component-word ranking instead
of ending.) Explicit positive
outcome assessments may still introduce a source.

The same conditional-code-length model ranks this view. Its source lengths
include the added identifier features, so this change can affect ordinary word
rankings as well. The index rebuilds from originals on open; old frames need no
conversion. Tests cover exact identifiers, component discovery, revisions,
reopen and a natural query prefix around the name.

SQLite receives a second external-content FTS5 index using an underscore-aware
tokenizer. It searches the exact names separately and combines those results
with the ordinary word index using reciprocal rank fusion:

```text
RRF(d) = sum(channel returning d, 1 / (60 + rank_channel(d)))
```

Each channel uses depth 512 with the same latest/historical predicates. Auxiliary
index construction happens before query timing. The source records stay the same;
the derived SQLite index is additional preprocessing and storage. No new write
or total storage comparison is inferred for that configuration.

### Rejected intermediate variant

Simply adding exact names to the flat query model did not solve the problem.
On the old six-declaration probe it fell to 0/6 at k=8 and 16 KiB, even though it
reached 5/6 at k=32 and 64 KiB. The stronger SQLite baseline reached 4/6 and 5/6
respectively. Generic surrounding words could still route unrelated sources.
That diagnostic led to identifier-anchored candidate views. Intermediate reports
are retained so the unsuccessful experiment is not erased.

## Additional public questions

The [public fixture](../benches/fixtures/nine-retrieval-public-v1.json) has 20
manually authored documentation questions and 40 deterministic function-location
queries, separate from the old 29-question probe. Every quoted reference was
matched to an exact existing passage before running the new index experiments.

Documentation questions concern Convex self-hosting, Redis TLS and LanceDB. Rust
names are globally unique declaration matches in the indexed Rust passages,
excluding the six old names and names beginning `test_`. They were ordered by
SHA-256 of `nine-public-unseen-v1:` plus the name; the first 40 were selected.
Thirty-eight names contain underscores. Questions use a common location template.
These are limited mechanical code probes, not evidence of general semantic QA.

The fixture was frozen before the first identifier experiment, but its aggregate
results informed this iteration. It is now a development/validation set rather
than an untouched final holdout.

At k=32 and a 64 KiB evidence cap, the sparse-only baseline retained 17/20 document
references and 18/40 declarations. Ordinary FTS5 retained 18/20 and 18/40. The
unsuccessful flat-identifier model retained 17/20 and 19/40; the stronger SQLite
baseline retained 18/20 and 38/40. Passing storage tests did not guarantee that
the first relevance change would help.

## Anchored results

The final anchored variant gives the following matched-cap results. SQLite here
means ordinary word FTS5 plus its new exact-identifier index and rank fusion.

| Probe and controls | Nine references | SQLite references | Nine p50 ms | SQLite p50 ms | Nine mean bytes | SQLite mean bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Historical evidence, k=128, 1 MiB | 20/21 | 20/21 | 11.200 | 98.753 | 131,497 | 954,839 |
| Old declarations, k=8, 16 KiB | 5/6 | 4/6 | 0.420 | 47.674 | 9,454 | 16,174 |
| Old declarations, k=128, 1 MiB | 6/6 | 6/6 | 0.589 | 50.501 | 103,236 | 967,791 |
| Large tool facts, k=8, 16 KiB | 9/9 | 9/9 | 0.362 | 43.710 | 9,539 | 16,246 |
| Additional documents, k=32, 64 KiB | 17/20 | 18/20 | 8.830 | 50.409 | 55,765 | 65,310 |
| Additional declarations, k=8, 16 KiB | 27/40 | 26/40 | 0.391 | 51.144 | 9,256 | 16,105 |
| Additional declarations, k=32, 64 KiB | 37/40 | 38/40 | 0.410 | 50.900 | 21,225 | 65,236 |

Anchoring repairs the tight-budget regression and improves the additional symbol
set substantially over the 18/40 sparse-only result. It also limits unrelated
context and scoring work. The large-tool questions explicitly include field
names such as `chunk_limit` and `resolved_epoch`; these are favorable literal
queries. Their sub-millisecond measurements must not be presented as the latency
for all natural-language queries on this corpus. General historical retrieval
still takes about 10–11 ms here.

At k=32/64 KiB, Nine still misses one additional document and one additional
declaration that the stronger SQLite baseline retains. Smaller returned text is
useful but does not excuse those quality deficits. The historical pilot also
still misses one reference pair at the widest tested count. Overall accuracy
superiority is unproven.

### Reserved declaration probes

After fixing the routing policy, a second disjoint set uses names 41–80 from the
same deterministic ordering. It excludes all 40 development names and all six
original names. Its [40 public queries](../benches/fixtures/nine-retrieval-reserved-v1.json)
were not used to change the policy before this run. Thirty-eight names contain
underscores. This is a reserved mechanical location check, not independently
authored semantic QA or an agent-task benchmark.

| Controls | Nine references / 40 | SQLite references / 40 | Nine p50 ms | SQLite p50 ms | Nine mean bytes | SQLite mean bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| k=8, 16 KiB | 25 | 27 | 0.407 | 51.309 | 10,886 | 16,184 |
| k=32, 64 KiB | 36 | 37 | 0.409 | 49.613 | 24,546 | 65,261 |

Nine's candidate pool contains 38/40 references, versus SQLite's 39/40. The
remaining candidate and tight-budget misses are real failures, not evidence
that a smaller context is automatically better. This check supports the local
latency improvement and narrower evidence packets; it contradicts a claim of
consistent accuracy superiority. No policy changes were made after inspecting
this reserved run in this milestone.

## Recorded aggregates

- [Sparse-only original probe](benchmarks/nine-sparse-requery-2026-10-19.json)
- [Additional public probe before identifier changes](benchmarks/nine-public-baseline-2026-10-19.json)
- [Rejected flat identifiers on the original probe](benchmarks/nine-flat-identifiers-2026-10-19.json)
- [Rejected flat identifiers on the additional probe](benchmarks/nine-public-flat-2026-10-19.json)
- [Final anchored original probe](benchmarks/nine-anchored-requery-2026-10-19.json)
- [Final anchored additional probe](benchmarks/nine-public-anchored-2026-10-19.json)
- [Final reserved probe](benchmarks/nine-reserved-2026-10-19.json)

The current source implements anchored retrieval. The unsuccessful intermediate
flat variant's reports are diagnostic records, not the behavior of the final
code. The sparse-only report predates the new identifier index.

## Verification and limits

The full all-features workspace suite passed 197 tests, zero failures, one GPU
test ignored. The compressed originals remained exact. A first requery matrix
completed its timings but report finalization attempted to read the still-locked
native archive on Windows. The checker now releases the database before its
independent archive digest read. The failed report attempt's private audits are
preserved; its incomplete aggregate is excluded from published results.

The seven completed matrices total 9,978 timed queries and 129,465 exact-record
cross-checks, with zero changed repeat packets. Archive digests and originals
are unchanged across the runs. These include development, rejected intermediate
and final variants; they are not 9,978 independent accuracy questions.

Evidence retention and serialized bytes are measured directly. Generated-answer
accuracy, actual model tokens, repeated agent-task success, cold caches,
concurrency and hosted network behavior remain unmeasured here. All added runs
use zero model/provider calls. No superiority claim over all database workloads
follows from these results.

## Reproduce against authorized frozen sources

```text
cargo test --workspace --all-features
cargo build --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/new-query-run --output results/new-query-run.json --repeats 3 --counts 8,32,128 --budgets 16384,65536,1048576 --sqlite-identifiers
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/new-public-run --output results/new-public-run.json --cases benches/fixtures/nine-retrieval-public-v1.json --repeats 3 --counts 8,32 --budgets 16384,65536 --sqlite-identifiers
oojuro-lab nine-scale-requery --source-run data/private/nine-scale-run3 --run-dir data/private/new-reserved-run --output results/new-reserved-run.json --cases benches/fixtures/nine-retrieval-reserved-v1.json --repeats 3 --counts 8,32 --budgets 16384,65536 --sqlite-identifiers
```

Fresh audit/report paths are required. Source-path identities, pinned public
snapshots and original byte offsets come from the large-corpus aggregate. The
approved private history and per-question private audits are not published.
Reproduction of the full augmented corpus requires access to those authorized
inputs. The public question definitions contain no private historical text.
