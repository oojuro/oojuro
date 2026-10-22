# Information theory research: evidence and resume boundary

Reviewed from the preserved research checkout at `b70efe9` plus its existing
uncommitted message-vector implementation and compression/report/cost helpers.
Those original files and artifacts were not changed. This public branch starts
from the public repository main and contains no imported research history,
conversation text, questions, per-question scores, learned tables, or vectors.

## What exists

The D18-D24 protocol and amended development grid exist in `docs/LONGCTX_EVAL.md`
in the preserved research checkout. Cached result artifacts exist for `own`,
`start`, `start@1M`, `start@100M`, `adapt`, and compression. Each contains 103
development questions and 90 evaluation questions, split 45 LongMemEval-M and
45 BEAM-1M. Every stored O8 recall matches the recomputed O8 recall, and references
and question membership agree across all six artifacts.

The scenario artifacts include settings from the expanded grid (600 dimensions,
40 expansion neighbors, or extended weights), so they differ from the disclosed
12-development-question smoke run. They have the final result schema, but no
immutable execution manifest proves which dirty source revision produced them.
Call these **cached post-amendment exploratory results**, not an audited final run.

Missing scenario results: `start@10M`, `ood`, and `pool`. There is no tracked
`bench/longctx/it_results.json` aggregate and no cached overlap table. A complete
D18-D24 report still needs the missing scenarios, overlap strata, table-build/state
costs, and the D23 systems evidence. This handoff does not certify those results.

## Evaluation exposure and decisions

D20 amendment 2 explicitly discloses that the same 90 evaluation questions were
scored during an earlier smoke run and that the numbers were seen before later
grids were extended. The amendment says the changes came from development
results; this handoff cannot independently prove that causal history. Reusing
these questions can measure the cached configurations, but cannot establish a
new untouched heldout result. There is no verified pristine heldout set in this
handoff; unused benchmark material has not been inspected or certified here.

The paired confidence intervals below quantify question-sampling uncertainty
within this exposed set. They do not remove test exposure, benchmark dependence,
or the starter corpus being in distribution. No adoption, superiority, or
production storage decision follows from these aggregates. Dense hybrid arms
still use the cached Qwen3-Embedding-8B references; their scores are not evidence
that embeddings have been replaced.

The headline arm is chosen by its saved development recall. Reporting the best
evaluation arm instead would select on the test set. An optional future clean
evaluation requires a frozen implementation/grid, an independently chosen unused
set, training-pool exclusion checks, and a recorded manifest before labels or
scores are inspected.

## Cached exploratory aggregates

O8 evidence recall: 0.629924; cached E8: 0.780678. Each row uses the saved development-selected headline, with 90 paired questions. No heldout/adoption verdict.

| Scenario | Headline | Recall | Arm minus O8 [95% CI] | E8 minus arm [95% CI] |
|---|---|---:|---|---|
| own | H1 | 0.746971 | +0.117047 [+0.067817, +0.170864] | +0.033707 [-0.034589, +0.105924] |
| start@1M | V | 0.684856 | +0.054932 [-0.020935, +0.132791] | +0.095822 [+0.021605, +0.173195] |
| start@100M | H1 | 0.689963 | +0.060040 [+0.014377, +0.109331] | +0.090715 [+0.023251, +0.161079] |
| start | V | 0.629944 | +0.000020 [-0.067645, +0.063857] | +0.150734 [+0.073676, +0.231485] |
| adapt | V | 0.678188 | +0.048264 [-0.016275, +0.115345] | +0.102490 [+0.038095, +0.171823] |
| compress | M3 | 0.671482 | +0.041559 [-0.042416, +0.124310] | +0.109196 [+0.027742, +0.195233] |

All-arm pooled and per-benchmark means, saved selections, and source artifact digests are in [`it_cached_exploratory.json`](../../bench/research/it_cached_exploratory.json).
The gaps and intervals above are descriptive exploratory evidence on the already exposed evaluation set.

## Safe aggregation

`scripts/research/it_summary.py` uses Python's standard library and reads only
existing result JSONs. It never imports the research helper modules, loads corpus
text or vectors, calls a provider, starts a service, or trains anything. It checks
the full development/evaluation counts, benchmark split, identical question sets
and references, valid recall ranges, and the saved headline selection. Missing
scenarios remain explicit and the adoption verdict is always null.

It emits aggregate arm means by benchmark and pooled, plus paired 95% bootstrap
intervals for the development-selected headline. Resampling uses 10,000 draws,
seed 20261021, question as unit, and Python `random`; draws therefore differ from
the old NumPy reporter even with the same numeric seed. Each sampled question
contributes its arm and both reference values together. Output never contains
question identifiers or individual scores. SHA-256 digests identify the exact
source result artifacts without publishing their contents.

Run on the CPU host against existing local artifacts, setting `RESEARCH_CHECKOUT`
to the preserved checkout. The destination must not already exist:

```bash
flock /tmp/sa11-verification.lock python3 scripts/research/it_summary.py \
  "$RESEARCH_CHECKOUT/.scratch/longctx/it/res" \
  --output /tmp/oojuro-it-exploratory.json
flock /tmp/sa11-verification.lock python3 -m unittest discover \
  -s scripts/research -p test_it_summary.py -v
```

The report is reproducible from those artifacts, not from a fresh checkout alone;
private/local benchmark inputs are deliberately absent from the public branch.

## Resume commands for the preserved experiment

These commands are documented only, and were **not run** for this handoff. They
can take significant CPU/memory and read the local benchmark corpus. Run them
only when that additional evaluation work is explicitly approved. They need the
preserved research Python environment and prebuilt local pools/spaces; do not
install packages, rebuild corpora, or fetch missing embeddings as a fallback.

```bash
cd "$RESEARCH_CHECKOUT/scripts/longctx"
# Development only; this does not issue evaluation scores.
flock /tmp/sa11-verification.lock env OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 \
  nice -n 19 python3 it_run.py run 'start@10M'
# After freezing the selection and recording the exposure/manifest boundary:
flock /tmp/sa11-verification.lock env OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 \
  nice -n 19 python3 it_run.py run 'start@10M' --eval
# Repeat those steps for ood and pool only if those scenarios remain required.
```

The original driver retunes before `--eval`; it does not load a frozen selection
manifest, and overwrites scenario files. Preserve prior files externally and
record source plus selected configurations before resuming. Development-only
mode still constructs evaluation question objects, though it does not score
them. The old `it_report.py` advertises `--res` but does not parse it, silently
skips missing scenarios, and computes overlap from source data when no cache
exists. Use the aggregate-only helper for this handoff. The original `best()`
tie break follows insertion order; because dimension/exponent precedes weight
in the V grid, it does not fully enforce the documented weight-before-size tie
rule. Fix and freeze that behavior before any future clean evaluation, with a
new source manifest; do not silently rerank the already observed results.

## What can ship for review today

This aggregate-only tool, synthetic regression tests, the six cached result
summaries, and the explicit incomplete/exposed status are reviewable now.
The research ranker, customer-derived learned tables, and deployment/storage
claims are outside this change. Retrieval quality does not prove durability,
tenant isolation, erasure, backup recovery, or AWS production readiness; those
remain separate gates for the Soft Axiom storage decision.
