# Repeated-term evidence and independent retrieval evaluation

Experiment protocol recorded on 2026-10-19 before the candidate was evaluated.
**Decision: reject the logarithmic-count candidate.** The delivered engine keeps
raw counts. A new independent retrieval gate and stronger SQLite control remain.
The starting engine is commit `155aa7d`, which uses raw token counts and
Dirichlet-smoothed query likelihood. Its unchanged document questions retain
16/20 required references at 16 KiB versus SQLite's 18/20. Inspection of the
private audits found the missing history/document sources inside the candidate
pool but below the returned evidence. Some high-ranked results have strongly
overlapping boilerplate. This is a ranking problem in those cases.

## One candidate, fixed before evaluation

The proposed index replaces each document feature's frequency `tf` with its
binary width:

```text
c(w,d) = 0                               if tf(w,d) = 0
c(w,d) = 1 + floor(log2(tf(w,d)))         otherwise
n(d)   = sum(w, c(w,d))
PB(w)  = (sum(d in C, c(w,d)) + P0(w)) / (sum(d in C, n(d)) + 1)
P(w|d) = (c(w,d) + 128 * PB(w)) / (n(d) + 128)
score  = sum(w in q, tf(w,q) * log2(P(w|d) / PB(w))) + assessed_usefulness_bits
```

Each doubling of a word's frequency contributes one additional unit. This is a
logarithmic feature-count model, not a new theorem or a claim that log weighting
measures relevance. It still defines normalized document/background feature
models with an escape mass. The existing literal-byte prior, time filtering,
sparse algebra and explicit outcome contribution remain unchanged. Original
text, LZ4/delta storage and archive formats remain unchanged. Derived postings
are rebuilt on open, and caches are local to the running implementation.

The hypothesis is that repetition should contribute less independent relevance
evidence. The test may reject it. The smoothing constant stays 128; no sweep of
weights, smoothing constants or per-question rules is planned. The default
must not be promoted based on gains that hide an existing accuracy regression.

## Development controls

Reuse the frozen 129 questions and 150 MB corpus, the stronger SQLite native
snippet/SQL-expansion control, 2 KiB excerpts, k=32, and 16/64 KiB total evidence.
The same explicit Rust declaration policy goes to both engines. Measure complete
reference retention, bytes and full latency separately. Raw-only huge tool
facts and exact historical/recovery tests remain required checks. Candidate
quality is assessed here before inspecting its independent test aggregate.

## Independent test

The public [BEIR SciFact fixture](https://github.com/beir-cellar/beir/wiki/Datasets-available)
contains 5,183 abstracts and 300 labeled test claims, with 339 positive
query/document pairs. The canonical archive matches the published download
digest. Data stay outside Git; aggregate reports contain source-file digests.
The original [SciFact project](https://github.com/allenai/scifact) attributes
claims/annotations under CC BY 4.0 and abstracts under ODC-By 1.0.

The Rust lab stores each title, newline and abstract as one exact original in
both databases. The retriever receives only query text, a common timestamp and
fixed limits. External labels are used only by the evaluator. No model, encoder,
query rewrite based on relevance labels, provider call or answer generation is
used. Corpus and query IDs are disjoint and every labeled source is verified.

Report macro nDCG@10, MRR@10, recall@10/32 and relevant-document retention in
the returned context. Binary nDCG discounts each relevant result at zero-based
rank r by `1/log2(r+2)`, normalized by the ideal relevant ranking. These labels
certify document relevance rather than supporting sentence retention or claim
truth. A correct document ID can still accompany an insufficient excerpt.

Each query ranks up to 512 references and returns up to 32 excerpts under
16 KiB, with a 2 KiB per-source ceiling. All tokenization, ranking, exact source
expansion, JSON serialization and synced receipts are timed. There are three
timed repeats after warm-up, alternating engine order. Source loading, index
build/replay and exact-source checks are outside query time. This is an
in-process, warm, single-client experiment with Nine's decoded index in RAM.

Both SQLite's ordinary Unicode FTS5 index and a stronger Porter-stemmed index
are explicit controls; native snippets expand inside SQLite in both. SQLite's
[Porter tokenizer](https://sqlite.org/fts5.html#porter_tokenizer) is an established
stemmer. Nine's candidate adds no stemming, semantic encoder or external model.
Canonical relevance metrics follow the [BEIR evaluation definitions](https://github.com/beir-cellar/beir/blob/main/beir/retrieval/evaluation.py).

SQLite uses OR expressions over the shared core tokenizer's words; Nine also
removes its fixed query stopwords. These are the actual current pipelines, not
identically normalized text processors or a claim of optimally tuned SQLite.
Query preprocessing needs a separate control before a general speed comparison.

This test will become inspected evaluation data after the run. Repeatedly
tuning to it would require a fresh independent gate. It is outside the agent
history domain and cannot replace actual agent-outcome measurements.

## Completed development result

The candidate did not change complete-reference retention in any group at
either cap. This includes 19/21 historical pairs, 16/20 document pairs at 16 KiB
and 18/20 at 64 KiB, all 86 explicit declaration queries and all nine large-tool
facts. The SQLite arm's evidence was also unchanged. Original and case digests
matched the earlier control; repeated packets were stable and there were no
full-body SQL fallbacks.

At 64 KiB, Nine's mean history context grew from 24,101 to 29,738 bytes and
document context from 41,943 to 49,534 bytes at the same coverage. The candidate
did not solve the diagnosed ranking misses or improve evidence efficiency.

## Independent SciFact result

The fixed candidate was evaluated once against the stronger Porter control,
after its development result was inspected. No candidate parameters or rules
were changed after the protocol was recorded.

| Engine/model | nDCG@10 | MRR@10 | Recall@10 | Recall@32 | Context document recall | p50 ms | Mean evidence bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Nine, raw counts, first control | 0.6536 | 0.6206 | 0.7774 | 0.8357 | 0.7799 | 2.462 | 16,334 |
| SQLite Unicode FTS5, same run | 0.6695 | 0.6354 | 0.7989 | 0.8554 | 0.7923 | 27.543 | 16,335 |
| Nine, raw counts, Porter control | 0.6536 | 0.6206 | 0.7774 | 0.8357 | 0.7799 | 2.520 | 16,334 |
| SQLite Porter FTS5, same run | 0.6874 | 0.6513 | 0.8245 | 0.8896 | 0.8253 | 25.545 | 16,336 |
| Nine, logarithmic counts | 0.6511 | 0.6178 | 0.7758 | 0.8357 | 0.7924 | 3.031 | 16,335 |
| SQLite Porter FTS5, candidate run | 0.6874 | 0.6513 | 0.8245 | 0.8896 | 0.8253 | 28.122 | 16,336 |

The candidate's context document recall rises slightly while its nDCG, MRR and
top-ten recall decline. Source presence alone would conceal that ranking
regression and says nothing about whether its snippet retains a rationale.
The tiny negative nDCG change is not presented as a statistical significance
claim; the candidate lacks evidence of the required improvement. The original
model is retained. Time comparisons are paired within each run; the earlier
observed desktop timing variation prevents attributing differences between
separate runs solely to this model change.

The plain versus Porter SQLite controls have the same ranker and source corpus.
Their quality difference makes lexical normalization a concrete next candidate
to investigate. It does not prove that adding a stemmer to Nine will close the
gap. This inspected test now needs a fresh independent gate before further
model selection can claim held-out validation.

## Saved implementation and verification

The candidate is preserved as an [experiment patch](../benches/experiments/binary-width-counts.patch)
against commit `155aa7d`; it is not applied in the delivered engine. Reverse
applicability was checked against the exact candidate before reverting. The
archived zero-context form also passed forward/reverse checks on copies of
that source base; applying it requires `git apply --unidiff-zero`. The
library exposes a compiled `RETRIEVAL_COUNT_MODEL` diagnostic so new benchmark
reports identify the actual count model. No runtime model-selection framework
or alternative index format was added.

The new `nine-beir-bench` command is Rust, with no model/provider dependency in
its execution path. Its binary nDCG/MRR/recall test checks a hand-computable
ranking. An independent standard-library calculation matched all 1,800 query
packets from the first three SciFact matrices. This checks metric arithmetic,
not the correctness or completeness of the external relevance labels.

After reverting, the full all-features suite passed **204 tests, zero failures,
one GPU test ignored**. The SQLite test additionally proves that a query for
`run` does not match `running` in the plain index, does match in the Porter
index, and still returns an exact original snippet without a full-body fallback.
The source archive format and every original remain unchanged. The core plus
reader and the whole shared helper file total 1,613 code lines; the additional
evaluator belongs to the experiment lab.

The rebuilt delivered executable was then rerun against the frozen Porter
control. Its raw-count model label was verified, both 300-query audit files were
identical to the earlier raw-count/Porter control, and all dataset/archive
digests matched. This final run measures **2.555 ms** median for Nine and
**26.576 ms** for SQLite, with unchanged nDCG values of **0.6536** and **0.6874**.
It is a release verification, not 300 new independent claims.

Across the four public matrices there are 7,200 timed queries and 20,732 recorded
exact-source cross-checks. The development matrix adds 1,548 timed queries and
18,495 checks. All matrices have stable repeated packets, unchanged original
archives and zero full-body SQL fallbacks. Independent metric arithmetic was
also checked for the delivered run, for 2,400 query packets across both engines
and all four public matrices.

### Aggregates

- [Raw-count Nine versus Unicode SQLite](benchmarks/nine-scifact-raw-2026-10-19.json)
- [Raw-count Nine versus Porter SQLite](benchmarks/nine-scifact-porter-2026-10-19.json)
- [Rejected candidate on the existing corpus](benchmarks/nine-log-count-development-2026-10-19.json)
- [Rejected candidate on SciFact](benchmarks/nine-scifact-log-count-2026-10-19.json)
- [Rebuilt delivered model versus Porter SQLite](benchmarks/nine-scifact-delivered-2026-10-19.json)

## Reproduction

Download the canonical `scifact.zip` from the URL in the BEIR dataset page,
verify the published checksum and extract it to `data/public/scifact`. On this
Windows host the system HTTPS client was used after Python's local certificate
chain failed; certificate verification was kept enabled. Data and per-query
audits remain outside Git. The benchmark requires the binary-positive qrels
schema and disjoint query/document IDs; this is not an evaluator for arbitrary
graded BEIR tasks.

```text
cargo test --workspace --all-features
cargo build --release -p oojuro-lab --features competitors
oojuro-lab nine-beir-bench --dataset data/public/scifact --dataset-name SciFact --run-dir data/private/new-beir --output results/new-beir.json --repeats 3
oojuro-lab nine-beir-bench --dataset data/public/scifact --dataset-name SciFact --source-run data/private/new-beir --run-dir data/private/new-beir-porter --output results/new-beir-porter.json --repeats 3 --sqlite-porter
```

Fresh run/report paths are enforced. Original records are checked against the
dataset and between engines before timing, and the native archive is hashed
after close before/after queries. To reproduce the rejected candidate, use its
recorded source base plus the archived patch and this evaluator; do not apply
an old experiment patch blindly to a newer engine.

The subsequent [normalization experiment](NINE_LEXICAL_NORMALIZATION.md) records
a later delivered model and fresh FiQA gate. Its evaluator also handles valid
query/document ID overlap by excluding identical IDs before context packing,
and provides a shared query-stopword control. The disjoint-ID requirement and
raw-word default described above are historical to this experiment's snapshot.
