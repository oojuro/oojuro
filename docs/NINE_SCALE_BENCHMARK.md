# Large tool payloads, text corpus and context budgets

Experiment date: 2026-10-19. Both the small-pilot budget controls and the full
scale/query matrix completed. Nine's engine still uses no
embedding model or LLM. The core ranking algorithm is unchanged.

This records the original scale run. Later scoring and identifier changes have
[separate measurements](NINE_RETRIEVAL_IMPROVEMENTS.md); the tables below are
preserved as the earlier result.

[Small-pilot aggregate](benchmarks/nine-small-budget-2026-10-19.json).
[Large-corpus aggregate](benchmarks/nine-scale-2026-10-19.json).

**Result:** returning more evidence fixes the small pilot, and linked passages
make the huge tool text usable within a small budget. Increasing the return count
alone does not recover every reference on the larger corpus. Nine is faster in
these measured retrieval paths; code-declaration accuracy is weaker than FTS5.

## Increasing the return count on the frozen pilot

The original 14-question engineering-history pilot has 21 question/source pairs, drawn
from eight distinct source records and 28 supporting quotes. It is reused and
retrospective. A new control varies returned count and serialized evidence budget
while both engines search a pool of up to 512 candidates. No result cache is used.

| Engine and return count | Evidence cap | Reference pairs retained / 21 | Questions retaining all evidence / 14 | Mean evidence bytes | Local p50 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Nine, 8 | 16 KiB | 18 | 11 | 3,857 | 0.137 |
| Nine, 16 | 16 KiB | 19 | 12 | 8,030 | 0.203 |
| Nine, 32 | 16 KiB | 21 | 14 | 10,641 | 0.152 |
| Nine, 32 | 64 KiB | 21 | 14 | 11,270 | 0.156 |
| SQLite FTS5, 8 | 16 KiB | 21 | 14 | 5,798 | 0.581 |

Each arm has 70 timed queries after warm-up. Increasing Nine's return count
does recover all reference evidence here, even under the original byte cap.
It sends roughly 1.8 times SQLite's evidence bytes at matched coverage. This
fixes this pilot's retention deficit, but does not establish lower answer-token
usage or better generated-answer accuracy. Source coverage is not an answer score.

SQLite uses one persistent FTS5 index with historical/latest eligibility
predicates; its collection statistics are global. The earlier comparison used
per-question historical views. Its exact rankings and timings are therefore a
separate protocol. Neither in-process result represents hosted request latency.

## Huge raw input/output storage

The stress fixtures are deterministic public synthetic data: a roughly 8 MiB
tool input, an 8 MiB output, a revision of that output and an 8 MiB opaque
alphanumeric result. The revision changes three distant values. They are
stored as individual raw records before any passage copies are constructed.

| Payload | Original text bytes | Nine archive increment bytes |
| --- | ---: | ---: |
| Structured input | 8,388,672 | 1,428,894 |
| Structured output | 8,388,676 | 1,415,876 |
| Revised output | 8,388,676 | 707,342 |
| Opaque result | 8,388,608 | 8,388,840 |

LZ4 plus bounded deltas reduces these structured payloads substantially. The
opaque result does not shrink. Three distant edits still cost about 707 KB:
the current delta has one changed middle, rather than an arbitrary set of
small patches. These are measured codec limits, not universal compression ratios.

All nine raw-tool references enter the candidate pool. Whole records fit none
of the 16–256 KiB evidence budgets. A 32 MiB budget retains all nine references
but returns about 16 MB of evidence per query. Disk compression does not make
the expanded text fit into a practical model context.

The next stage retains these exact raw records and adds caller-created passages
with parent IDs and exact UTF-8 byte ranges. Both Nine and SQLite receive the
same passage construction; the experiment does not claim Nine uniquely invented
chunking or that automatic passage indexing is already part of its native API.

## Corpus size and verification

The completed load has 18,495 records and 149,793,732 stored text bytes. That
includes 91,048,493 bytes from 6,508 materialized tracked UTF-8 files in pinned
PostgreSQL, Redis, Convex and LanceDB checkouts, plus the synthetic raw tools,
their linked passage copies and 51 approved redacted pilot passages.

The logical original payloads total 124,627,708 bytes; the extra 25,166,024
bytes are linked tool-passage copies. Public files are split into approximately
8 KiB passages without overlap, preserving complete byte coverage. No model is
used for splitting or indexing. Public distractors use synthetic timestamp zero
for this experiment, not a verified historical availability date.

Both engines passed all 36,990 exact-record read checks, and complete public
files were reconstructed and hash-checked. Nine's source archive is 67,955,792
bytes; SQLite's source table plus FTS index is 201,732,096 bytes. Neither figure
is a total residency comparison: Nine's RAM index and separate query journals
are excluded. The observed process RAM includes both engines and the driver.
An independent native-store reopen subsequently validated the compressed frames,
original-record digests and all 18,495 sources, rebuilding 4,217,393 postings.

## Larger-corpus retrieval results

The matrix has 29 questions: the reused 14-question pilot probe, nine
synthetic large-tool/time-revision questions and six mechanical unique Rust
declaration lookups. Candidate depth is 512; returned counts range from two to
128 and evidence budgets from 16 KiB to 1 MiB. Each arm has three timed repeats
after warm-up. The 48 arms supply 4,176 timed queries, plus 144 timed raw-stage
queries. The small-pilot control supplies another 3,360 timed queries. There
were zero changed repeat packets in all completed matrices.

### Pilot evidence with the public corpus as distractors

| Engine | Return count | Evidence cap | Reference pairs retained / 21 | Questions with all evidence / 14 | Mean evidence bytes | Local p50 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Nine | 8 | 16 KiB | 18 | 11 | 5,561 | 69.918 |
| SQLite FTS5 | 8 | 16 KiB | 17 | 10 | 16,202 | 96.379 |
| Nine | 32 | 64 KiB | 19 | 12 | 27,630 | 71.777 |
| SQLite FTS5 | 32 | 64 KiB | 18 | 11 | 65,304 | 101.595 |
| Nine | 128 | 1 MiB | 20 | 13 | 130,334 | 71.022 |
| SQLite FTS5 | 128 | 1 MiB | 20 | 13 | 954,839 | 104.305 |

All 21 reference pairs are present within each engine's 512-candidate pool.
The return count and byte cap still determine what reaches the context. Neither
engine reaches 21/21 in this matrix. At matched 20/21 coverage in the widest
listed setting, Nine returns much less evidence text and has lower local latency.
That is a measured result on this augmented pilot, not proof of answer-token
efficiency or general agent accuracy. Small-pilot sub-millisecond results must
not be presented as these larger-corpus latencies.

### Large tools after linked passage construction

With two returned sources and a 64 KiB cap, Nine retains all nine tool references
using 10,745 mean evidence bytes at 24.828 ms median. SQLite's two-source,
16 KiB arm also retains all nine, using 14,424 bytes at 43.847 ms median. The
aggregate includes matched caps and every other tested count. These contexts
include exact passages and raw-parent links rather than the full 8 MiB originals.

The raw-only and full-corpus stages have different corpus sizes and are not an
isolated passage-index latency A/B test. They do establish the original-size
constraint and successful retrieval of the same facts from exact linked ranges.

### Code-declaration probe

| Engine | Return count | Evidence cap | Expected declarations retained / 6 | Declarations in candidate pool / 6 | Mean evidence bytes | Local p50 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Nine | 8 | 16 KiB | 1 | 5 | 10,061 | 8.659 |
| SQLite FTS5 | 8 | 16 KiB | 2 | 6 | 16,220 | 43.289 |
| Nine | 128 | 1 MiB | 3 | 5 | 330,836 | 11.498 |
| SQLite FTS5 | 128 | 1 MiB | 5 | 6 | 966,717 | 50.773 |

This exposes a relevance problem that more output does not fully solve within
these tested limits. Current tokenization splits identifiers with underscores
into ordinary words; function names such as `log_udf_executor_result` lose their
whole-identifier signal. A unique declaration can also have a common name such
as `resolution`. These are mechanisms to investigate, not established root-cause
claims for every missed case. Phrase/identifier/declaration-aware retrieval needs
its own controlled ablation and stronger baseline before claiming a fix.

## Write behavior at scale

Median individually acknowledged append times:

| Payload group | Nine ms | SQLite FTS5 ms | Observations per engine |
| --- | ---: | ---: | ---: |
| Four roughly 8 MiB raw records | 231.129 | 114.423 | 4 |
| Linked tool passages | 0.668 | 1.306 | 3,093 |
| Public source passages | 0.729 | 1.485 | 15,347 |

Nine is faster on the passage writes here, while SQLite is faster on the giant
raw records. Four large records are insufficient for a general large-write
distribution. Codec compression, storage footprint and append latency are
different outcomes.

## Corrections and limitations

Two initial attempts stopped before the corpus query matrix: one encountered
unmaterialized tracked files in a sparse checkout; another exposed an interior
UTF-8 boundary error in the new benchmark splitter. Materialized-file filtering
and an adversarial three-byte-glyph regression test address those harness errors.
Completed raw-stage measurements from the stopped attempts remain private.

The first context builder serialized multi-megabyte records before rejecting
them for a small budget. Both engines now expose source byte counts, allowing
oversized bodies to be skipped before loading and serializing them. This is a
benchmark adapter improvement; it is not a change to Nine's relevance formula.

This is a single-client, in-process Windows experiment, outside the earlier
512 MiB Linux-container setup. It does not measure hosted Supabase, PostgreSQL
at this larger scale, out-of-RAM behavior, concurrency, generated answers,
actual model tokens or repeated agent tasks. Large-tool facts are synthetic,
and code-search questions are mechanical lookups restricted to unique Rust
declaration names. These results need a larger independently authored task set
before making a general accuracy claim.

## Reproduction and next gate

```text
cargo build --release -p oojuro-lab --features competitors
oojuro-lab nine-scale-bench --dataset data/private/pilot-benchmark-v1 --public-root ../../work/upstream --run-dir data/private/new-scale-run --output results/new-scale-run.json --repeats 3
oojuro-lab nine-scale-bench --dataset data/private/pilot-benchmark-v1 --public-root ../../work/upstream --run-dir data/private/new-small-control --output results/new-small-control.json --repeats 5 --small-only
cargo run --release -p oojuro-nine --example verify_store -- data/private/new-scale-run/nine 18495
```

The expected source count applies to the pinned materialized checkouts in the
aggregate. New source snapshots, sparse-checkout selections or checkout newline
settings can change chunk counts and byte offsets. The private historical corpus
and per-question audits are not committed. No provider credentials are required.

The next quality gate should combine improved lexical/identifier ranking and
context selection with unseen queries, then verify actual agent answers and
repeated task outcomes. Larger return counts are useful, but do not establish
accurate interpretation or mistake prevention by themselves.
