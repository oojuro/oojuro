# Activity search: finding moments by date and by how the activity looked

Status: design and measurements only (2026-10-21). No engine code yet; review is pending
before any is written. Prototypes: `scripts/activity/` (Python,
streaming, run with `python3 -I`). Every number below was measured locally on
a developer's live journal, read-only through the running recorder (`oojuro export
--from 30d`, `oojuro range`, `oojuro search`). Only aggregate numbers and
generic descriptions appear here; no transcript text, paths or snippets.

Answers (2026-10-21) that this design follows: four separate signals
(volume, novelty, errors, change vs normal); results are time periods ranked
by a signal, combinable with a text query and dates, each opening into its
records; one query model for agents (MCP) and humans (CLI, later the
dashboard); date phrases by rules, plus event-relative anchors found by
searching the journal. Added on 2026-10-21: work phases as a finite-state
machine decoded with Viterbi, compared against change-point baselines (§4).

## At a glance

| Signal | Definition (bits where possible) | Rank by | Measured on the top 20 (hand-checked) |
|---|---|---|---|
| Volume | activity records and tool calls per period | count | trivially right; finds fan-out peaks (about 20 parallel sessions) and multi-day coordinator sessions |
| Novelty | zstd conditional code length of each record given the writer's last 8 MiB, summed | most new bits | 20/20 real influx of new material, but 8/20 were sibling sessions of one fan-out, and the total tracks volume (ρ 0.85–0.92) |
| Repetition (the low end of novelty) | novelty bits per event, among periods with at least 20 events | fewest bits per event | 15/20 repetitive: 12 waiting or polling loops, 3 retry loops; 5 false positives (many tiny records) |
| Errors | binomial upper-tail surprisal of the period's error count against the writer's trailing 7-day error rate | most bits | 18/20 real error bursts; 2 weak (text rules firing on non-errors) |
| Change vs normal | sum of 5 rarity components (rate, tool mix, error rate, new tools/files, novelty density), each −log2 of its empirical tail among the writer's own windows of the last 7 days | most bits | per writer-hour: 9/10 plausible. Per session-window: 20/20 rare by construction, but 13/20 are the same routine pattern (a new subagent's opening read burst) |

Recommendations:
- **Novelty estimator: streaming zstd (`zs`).** All four estimators rank alike
  (ρ ≥ 0.97). It ties CDC for finding loops, it is the fastest, it needs no
  training, and it keeps no text-derived state on disk.
- **Phases (HMM + Viterbi): keep as an option, do not adopt for v1.**
  - Its phase boundaries explain the signals no better than equal-sized
    chunks with the same number of segments.
  - A plain optimal partitioning (PELT) beats it on that test.
  - The learned "stuck" state drifts toward fast web research.
  - Its labels are readable, and decoding is cheap and can run online. §4
    has the numbers.

## 1. Data

- **Export:** `oojuro export --from 30d`, compressed to a private local
  directory and deleted afterwards. 305,928 records, 2026-09-22 to
  2026-10-21, 68 writers, 1,328 sessions.
- **Activity records:** 206,111, which is everything except harness context
  (`system`, 99,817 records) and Oojuro's own echo (`oojuro=call|result|context`,
  161 records).
- **Concentration:** two writers hold 98% of the records, and one of them
  (every Claude Code session started in the home directory) holds 90%.
- **Warm-up:** windows before 2026-10-01 only feed baselines, because the
  estimators start without history. This left out 2% of activity.

Window kinds measured:

| kind | meaning | active windows (after warm-up) |
|---|---|---|
| `5m`, `1h` | one writer, clock-aligned, all its sessions together | 2,565 / 412 |
| `5m/s`, `1h/s` | the same, one session | 7,478 / 2,029 |
| `session` | one whole session (one transcript) | 1,316 |

## 2. Signal definitions

No ML model and no LLM; every quantity is a count, a code length or a tail
probability.

**What counts as activity.** An event is a record of kind `user_input`,
`thinking`, `assistant`, `tool_call`, `tool_result` or `explicit`, unless it
carries the echo mark. `system` records are harness context. They are left out
of every signal except errors, where a `system` record with `sub` =
`api_error`/`upstream_error` counts as a failure.

**Tool family** (rules, no model; `scripts/activity/common.py`):
- One of: read, edit, run, test, vcs, wait, delegate, ask, web, other.
  Non-tool events are user, say, think and note.
- **From tool names:** Read/Grep → read; Write/Edit → edit; MCP
  `get_*/list_*/search_*` → read; and so on.
- **From shell commands:** take the program of each simple command, skipping
  `cd`, assignments and wrappers like `nice` or `timeout`. The strongest
  family wins: test > run > vcs > edit > wait > read. For example,
  `cargo test` → test, `git push` → vcs, `sed -i` → edit, `sleep` → wait.
- A result takes its call's family.

### 2.1 Volume

`events(P)` is the number of activity records in period P; `calls(P)` is the
number of tool calls. Both are plain counts. Rate bits appear only inside
change vs normal.

### 2.2 Novelty

For each activity record r of writer W:
- `nov(r) = 8 · |zstd_W(r | h)|` bits.
- `zstd_W` is one streaming zstd context per writer: level 3, 8 MiB window,
  one block flushed per record.
- `h` is the writer's preceding activity text, so `nov(r)` is the code length
  of r given what the writer recently saw and did.

Per period:
- `novel_bits(P) = Σ nov(r)`.
- `bits_per_event(P) = novel_bits / events`. Low means repetitive.
- `density(P) = novel_bits / (8 · bytes)`.

Per-record bits are additive, so any period (window, session or phase) is a
sum. That is the property that makes rollups possible (§6). A loop is cheap at
once: the second identical call costs about 90 bits against about 1,000 for
the first.

Estimators compared (§3.3):

| id | estimator | state it keeps |
|---|---|---|
| `zs` | streaming zstd, as above | 8 MiB window per live writer, in memory only |
| `zd` | each record compressed alone with a 110 KiB zstd dictionary trained daily on the writer's previous 16 MiB of records | a dictionary per writer (text fragments) |
| `cdc` | bytes of content-defined chunks (gear hash, 256 B / ~1 KiB / 8 KiB) that the writer never stored before | a set of chunk hashes per writer |
| `ng` | adaptive hashed unigram+bigram token model with Dirichlet backoff (digits folded to one class), first 20k tokens of a record | 96 MiB of hashed counts |

### 2.3 Repetition (loops)

Repetition is not a fifth signal: it is the low end of novelty.
- Rank periods by fewest `bits_per_event` among those with at least 20
  events (5-minute windows) or 60 events (hours, sessions).
- Tested alternatives (§3.3): density, the share of new CDC bytes, and the
  ratio of recent to long-term redundancy. All three fail.

### 2.4 Errors

**Per result,** `err(r)` is 0 none, 1 error output, or 2 failed call.
- **Failed call (2)** comes from the harness: the stored status (Claude Code
  `is_error`), or a text marker where a format has no status. Markers include
  `Exit code N`, `<tool_use_error>`, a hook denial, an interrupt, Codex
  `Script failed|error` and an API error record.
- **Error output (1):** the first or last 2 KiB of a result shows a failure.
  Examples: a traceback, `error[E…]`, `FAIL`, `N failed`, `panicked at`,
  `command not found`, `No such file or directory`.

**Per period:**
- `errors(P)` is the number of results with `err > 0`.
- The error signal is the binomial upper-tail surprisal against the writer's
  own trailing rate:

```
err_bits(P) = −log2 P(X ≥ errors(P)),  X ~ Binomial(results(P), p0)
p0 = (errors_7d + 1) / (results_7d + 2)     (writer's trailing 7 days, same window kind)
```

More errors at a higher-than-usual rate rank higher. A window of 1 error in 1
call scores only about log2(1/p0), so tiny windows do not dominate.

### 2.5 Change vs normal

There are two steps: a raw statistic per dimension against the writer's
pooled baseline, then calibration of each statistic to its rarity among the
baseline windows.

**Raw statistics.**

| dimension | raw statistic of window w (n = events, n_e = min(n, 50)) |
|---|---|
| rate | events (time windows); events per minute (sessions, session windows) |
| mix | `n_e · Σ_f p̂_f log2(p̂_f / q_f) − (k−1)/2 · log2 n_e` (MDL excess code length of the tool-family mix; q = baseline mix, Dirichlet ½) |
| errors | `n_e · KL(errors/results ‖ p0)` when the rate exceeds p0 |
| new | `n_e · (ρ log2(ρ/ρ0) − (ρ − ρ0)/ln 2)`, where ρ is first-seen tools and files per event and ρ0 is the baseline rate |
| novelty | density (`novel_bits / 8·bytes`) |

**Calibration.**
- Each component is `u = −log2((#{baseline windows with stat ≥ x} + 1) / (N + 1))`.
  - The baseline is the writer's windows of the same kind in the trailing 7
    days.
  - For time windows, N counts the empty slots too (empty = 0).
  - Density is two-sided.
- `unusual_bits = Σ u` over the five components. It is at most about 5 ·
  log2(N+1), about 55 bits for 5-minute windows.
- The result lists each component, so it explains itself: "unusual: mix 6.5,
  new 8.5, rate 8.2".

Why both steps, as measured:
- **Without the exposure cap** (n_e), the mix and rate evidence grew with
  window size. Unusual then correlated 0.63–0.86 with volume and ranked big
  windows only.
- **Without calibration,** the mix evidence (up to about 300 bits)
  outweighed the bounded components (about 10 bits).
- **After both,** the components sit on one scale, with p99 between 6 and 9
  bits each.

## 3. Measurements

### 3.1 Distributions (p50 / p90 / p99 per window)

| kind | windows | events | novel bits | bits/event | errors | error bits | unusual bits |
|---|---|---|---|---|---|---|---|
| 5m | 2565 | 47 / 199 / 419 | 138k / 819k / 2,144k | 3.0k / 7.8k / 27k | 1 / 8 / 19 | 0 / 4.3 / 12.8 | 9.6 / 16.5 / 24.2 |
| 1h | 412 | 115 / 1,531 / 3,105 | 300k / 6,233k / 15,016k | 4.0k / 22k / 30k | 4 / 56 / 113 | 0 / 6.9 / 28.1 | 3.4 / 11.7 / 16.5 |
| 5m/s | 7478 | 21 / 61 / 106 | 58k / 250k / 750k | 2.8k / 7.8k / 25k | 0 / 3 / 7 | 0 / 3.5 / 10.3 | 5.3 / 10.2 / 16.3 |
| 1h/s | 2029 | 58 / 267 / 463 | 166k / 1,099k / 2,196k | 3.1k / 10k / 28k | 1 / 12 / 25 | 0 / 3.9 / 14.3 | 4.8 / 9.5 / 15.7 |
| session | 1316 | 21 / 352 / 1,466 | 30k / 1,526k / 4,365k | 3.5k / 15k / 29k | 0 / 15 / 54 | 0 / 3.1 / 14.2 | 3.8 / 9.2 / 16.4 |

**Error rate in this journal:** 8.4% of the 91,201 tool results are
flagged: 2.5% failed calls and 5.9% error output. Error bits are 0 for most
windows and above 10 bits for 1.1% of session 5-minute windows, 2.1% of
writer 5-minute windows and 6.1% of writer hours.

### 3.2 Redundant or complementary (Spearman ρ across windows)

| | events | novel bits | bits/event | error bits | unusual |
|---|---|---|---|---|---|
| novel bits (5m / 5m/s / session) | 0.92 / 0.85 / 0.91 | 1 | 0.48 / 0.62 / 0.53 | 0.20 / 0.16 / 0.48 | 0.41 / 0.46 / 0.48 |
| bits/event | 0.14 / 0.17 / 0.15 | | 1 | −0.11 / −0.08 / 0.00 | 0.01 / 0.07 / −0.04 |
| error bits | 0.26 / 0.25 / 0.56 | | | 1 | 0.57 / 0.47 / 0.40 |
| errors (count) | 0.78 / 0.53 / 0.89 | | | 0.69 / 0.85 / 0.73 | |

Reading:
- **Redundant:** total novelty is mostly volume, and an error count is mostly
  volume too.
- **Complementary:** novelty per event, the error surprisal and change vs
  normal are each largely independent of volume and of each other.
  - Change vs normal correlates 0.4–0.6 with error bits and volume, because
    error rate and event rate are two of its five components.
- **So the four signals are kept,** with novelty ranked both ways (most new
  bits; fewest bits per event) and errors ranked by surprisal, not count.

### 3.3 Novelty estimators: quality and cost

**Quality.**

| | `zs` stream | `zd` dictionary | `cdc` new chunks | `ng` n-gram |
|---|---|---|---|---|
| ρ with `zs`, window totals (5m / 5m/s / 1h / session) | 1 | 0.98 / 0.97 / 0.96 / 0.93 | 0.99 / 0.98 / 0.97 / 0.94 | 0.99 / 0.98 / 0.98 / 0.96 |
| top-20 novelty windows shared with `zs` (5m / 1h/s / session) | 20 | 12 / 10 / 19 | 15 / 14 / 18 | 15 / 12 / 18 |
| loops: exact-repeat share of calls in the 20 lowest bits/event windows, 5m/s (base 1.1%) | **31.7%** | 24.4% | **33.2%** | 23.7% |
| same, 50 lowest | 19.5% | 16.8% | 22.0% | 19.5% |

**Cost.**

| | `zs` stream | `zd` dictionary | `cdc` new chunks | `ng` n-gram |
|---|---|---|---|---|
| speed in the Python prototype (476 MB of activity text) | 98.7 MB/s | 110 MB/s compressing, plus 0.73 s per training (19 trainings) | 16.5 MB/s (pure-Python gear hash; FastCDC in Rust runs at GB/s) | 5.6 MB/s |
| persistent text-derived state | none (the window lives in RAM) | dictionaries hold text fragments | chunk hashes are a membership oracle for erased text | hashed token counts |
| erasure work | drop or re-prime the context; recompute the bits of the next 8 MiB | retrain the dictionaries | refcount and remove hashes | rebuild the tables |

Loop rankers tried on the exact-repeat label, top 20 at 5m/s:

| ranker | exact-repeat share |
|---|---|
| bits/event (`zs`) | 31.7% |
| bits/event × density | 22.3% |
| density | 1.8% |
| recent/long-term ratio (`zs/zd`) | 0.6% |
| new-CDC share | 0.8% |

Per-session windows find loops better than writer windows at 1 h (14.6% vs
7.7%), and about equally at 5 min (31.7% vs 32.2%). A whole session is too
coarse (4.2%).

**Recommendation: `zs`.**
- It ranks the same as the others.
- It finds loops as well as CDC and better than `zd` and `ng`.
- It is the fastest, needs no training, keeps nothing text-derived on disk,
  and is additive per record.
- CDC is the only close second, but it would need a refcounted chunk-hash set
  to be erasable.

### 3.4 Hand-checked precision, top 20 per signal

Each window was read locally; it is described here only in generic terms.

**Repetition** (5m/s, fewest bits per event): 15/20 repetitive.
- 12 waiting or polling loops:
  - browser automation cycling wait and screenshot, about 10 times each
  - CI or job polling with the same status command
  - waiting on a person to finish a step
- 3 retry loops: the same check or test command re-run 4–6 times.
- 5 false positives: windows of many tiny records (short assistant messages,
  small distinct reads), whose bits per event are low without any repetition.
- Almost none are a truly "stuck" agent. The signal finds repetition, and
  most repetition here is waiting.

**Errors** (5m/s, error bits): 18/20 real error bursts.
- Failing test runs in test-fix cycles, type errors, failed installs, a run
  of blocked commands.
- 2 weak: soft-error text rules matched build or format output that was not
  a failure.

**Novelty** (5m/s, most new bits): 20/20 real influx of new material.
- Mostly the first minutes of a subagent reading a codebase or a third-party
  library for a review, and bulk reads of fetched documentation.
- 8/20 were sibling sessions of one parallel fan-out in the same 5 minutes,
  so results need sibling folding (§5.3).

**Change vs normal:**
- **Session windows:** all 20 are rare for the writer, by construction.
  - 13 are the same routine pattern: a new subagent's opening burst of
    reads (high rate, all-read mix, many first-seen files).
  - 7 are distinct moments: a hotfix with failing tests and heavy
    edits, an investigation loop against an external service with errors, a
    file rewritten 12 times in a row, new scaffolding written from scratch,
    and a burst of web research.
- **Writer hours:** 9/10 plausible.
  - High-error hours of the second agent.
  - The journal's heaviest error hour (260 errors across 24 sessions).
  - 1 artifact: a 9-event hour. The fix is a minimum of 20 events, the same
    as repetition.

**Volume:**
- **Top hours:** fan-out peaks, with 10–24 concurrent sessions and
  2,700–3,600 events per hour.
- **Top sessions:** coordinator sessions lasting days (up to 17k events over
  5 days).

### 3.5 Speed and storage

**Feature pass** (Python, one thread, `nice 19`, the whole 30 days): 158 s.
- Rules (family, errors, repeats): 46 MB/s over all 702 MB of text.
- Estimators: per the table above.

**Window build and ranking:**
- Building all windows: 4.7 s.
- Scoring and ranking one writer's 30 days in Python:
  - 26.6 ms for 1,830 writer 5-minute windows
  - 88 ms for 6,005 session 5-minute windows
  - 5.5 ms for hours

**Storage,** measured by encoding the prototype's columns and rollups
(LEB128 + zstd):

| what | size |
|---|---|
| per-record novelty column (bytes of code length, varint) | 1.36 B per activity record |
| family + repeat + error level packed in one byte | 0.34 B per activity record |
| rollups, all five window kinds | 342 KiB for 30 days (23–35 B per window) |
| total | about 2.3 B per record, **about 0.3% of the journal** (664 B per record today) |

## 4. Phases: a finite-state machine decoded with Viterbi (option)

**Model.** A hidden Markov model over each session's activity records.
- **States (6):** explore, edit, run, debug, stuck, idle.
- **Observations:** five discrete features per record, with emissions
  factorized given the state:
  - family
  - error level
  - novelty bin (`zs` < 256, < 1024, < 8192, ≥ 8192 bits)
  - gap since the previous record (< 5 s, < 30 s, < 5 min, ≥ 5 min)
  - repeated-call flag
- **Costs are bits:** −log2 of transitions and emissions, plus a **switch
  cost** per phase change ("path cost per phase"). Viterbi returns the
  cheapest path.

**Learning.**
- Hand-set start: sticky transitions, and emissions that describe each phase.
- Then MAP Baum-Welch: 12 iterations, Dirichlet pseudo-counts of 2,000 per
  feature and state taken from the start values.
- Trained on sessions that started before 2026-10-15; scored on the 2026-10-15
  to 10-21 sessions.
- 706 sessions with at least 8 events, 204,904 events.
- `scripts/activity/phases.py`.

**Baselines:**
- **Optimal partitioning (PELT):**
  - with the KT code length per segment plus log2 T per boundary (pure MDL)
  - with a plug-in code length plus a penalty β per segment (BIC-like,
    β = 8/16/32)
- **Fixed windows:** 5 min and 1 h per session.
- **Equal-event chunks:** each session cut into the same number of equal
  chunks as the HMM found, which isolates the value of *where* the HMM puts
  its boundaries.

**Do phase boundaries beat fixed windows?** The test is the share of
per-event variance that segment means explain (R²) for the signals:
- log novelty bits
- error flag
- log gap (volume as pace)
- repeat flag

Mean over the four, held-out sessions:

| segmentation | held-out segments | mean R² |
|---|---|---|
| HMM, switch cost 32 bits | 1,311 | 0.072 |
| equal-event chunks, same count | 1,311 | 0.066 |
| PELT plug-in, β = 32 | 1,244 | 0.083 |
| fixed 5 min | 2,389 | 0.096 |
| HMM, switch cost 16 bits | 3,180 | 0.104 |
| equal-event chunks, same count | 3,180 | 0.099 |
| PELT plug-in, β = 16 | 3,760 | 0.149 |
| HMM, no switch cost | 16,338 | 0.251 |
| equal-event chunks, same count | 16,338 | 0.259 |
| PELT, pure MDL (KT + log2 T) | 600 | 0.046 |
| fixed 1 h | 618 | 0.044 |

- **At matched resolution,** HMM boundaries are no better than cutting the
  session into equal pieces (+0.005 to +0.006 at coarse resolution, −0.008
  at fine).
  - PELT with a plug-in cost wins clearly: 0.149 at 3,760 segments against
    the HMM's 0.104 at 3,180.
  - Fixed 5-minute windows land between the two.
- **Pure-MDL PELT** is far too coarse (about one segment per session). Each
  segment pays its own parameter cost (about 80 bits). HMM states share
  their parameters, which is why HMM phase switches are cheap.
- **Without a switch cost the HMM flickers:** the mean phase is about 4
  events. 16 bits per switch gives a median phase of 0.4–3.6 minutes for
  the working states.

**What the states hold** (switch cost 16):

| state | share of events | phases | median length | error rate | repeat rate of calls | dominant families |
|---|---|---|---|---|---|---|
| explore | 17% | 1,908 | 0.4 min | 1.2% | 0.1% | read |
| edit | 5% | 693 | 1.0 min | 4.4% | 4.8% | edit |
| run | 44% | 2,953 | 1.6 min | 3.1% | 0.5% | run |
| debug | 23% | 1,339 | 3.6 min | **7.6%** | 0.4% | test, run |
| stuck | 6.4% | 370 | 3.5 min | 2.9% | **9.1%** | wait, web, run |
| idle | 4% | 933 | < 0.1 min | 0.5% | 1.5% | delegate, user, say |

- **"Stuck"** holds 47.8% of all exact-repeat calls in 6.4% of the events
  (7.5× lift).
- **"Debug"** has 2.5× the error rate of "run".

**Read by hand:**
- **The 15 longest "stuck" phases:** 9 are real waiting, polling or repeat
  loops, one of them 2 hours of browser polling. 6 are fast web-research
  bursts, which is semantic drift.
- **Sensitivity to the start:** with the first hand-set start, which gave
  "stuck" a preference for web and wait families, only 2 of the 12 longest
  were loops.
- **Two build sessions read end to end:** the phases follow the real arc
  (read the brief → write the new code → test-and-fix cycles labelled
  debug). Family-rule mistakes (shell heredocs that edit files are labelled
  run) carry into the labels.

**Cost:**
- **Python:** Viterbi 5.5 µs per event; Baum-Welch 7 µs per event per
  iteration; PELT plug-in 13 µs per event at β = 16 (much slower at small β).
- **Viterbi in Rust:** 36 multiply-adds per event at S = 6, about 0.1 µs.
- **Online decoding:** fixed-lag Viterbi with lag L = 64 events.
  - Per open session: S path costs and an L×S back-pointer ring (about
    400 B).
  - Phases are final once L events past them; each phase is stored as a
    rollup row of about 30 B.
  - About one phase per 17 events at switch cost 16, so about 1.8 B per
    event.
- **Learned parameters:** about 200 numbers, no text. Refit periodically.

**Recommendation: keep as an option, do not adopt for v1.**
- **Against adopting now:**
  - Its boundaries do not beat equal chunks.
  - PELT does better on homogeneity, so a learned model buys labels, not
    better boundaries.
  - The labels drift with the start values.
  - "Stuck" overlaps what the repetition ranking already finds at 5-minute
    resolution.
- **For later:**
  - Readable labels: "a 40-minute debug phase on Tuesday".
  - Cheap online decoding.
  - Durations natural to the work.
- **v1 instead:** merge adjacent qualifying windows into one period (§5.3).
  This already yields "the 35-minute stretch of polling" without a model.
- **If adopted later:**
  - Results gain `--window phase` and a `--phase debug|stuck|…` filter.
  - Phases are stored as rollup rows decoded online at ingest.
  - The start values and switch cost become settings, re-measured against
    the repeat and error labels.

## 5. Query design

### 5.1 CLI

One new command, plus `--when` and `--session` on the existing ones:

```
oojuro periods [QUERY|-] [--by activity|novelty|repeat|errors|unusual]
               [--window 5m|1h|1d|session] [--per session|writer]
               [--when PHRASE] [--from T] [--to T] [--writer W]... [--kind K]...
               [-k N] [--samples N] [--json]
oojuro search|range|export ... [--when PHRASE] [--session S]
```

- **Ranking:**
  - `--by` picks the ranking. `repeat` is the low end of novelty.
  - With a `QUERY` and no `--by`, periods rank by the text bits of their hits
    ("periods about X").
  - With both, a period qualifies only if it holds a hit of at least
    `min_bits`; it is ranked by the signal, with text bits breaking ties.
- **Defaults:**
  - `--window`: 5m for `repeat` and `errors`, 1h otherwise. The measured
    precision is best there; one fixed default is an open question (§9).
  - `--per session`: a period belongs to one session, the sharper unit for
    loops and errors. `--per writer` sums all of a writer's sessions, for
    "busiest hour".
  - `-k 10`.
  - Minimum 20 events per period for `repeat` and `unusual`.
- **Dates:** `--when` takes date phrases and anchors (§5.4, §5.5). It is
  sugar for `--from/--to`, and the resolved interval is always echoed back.

### 5.2 MCP

**One new tool, `periods`:**
- Arguments: `query?`, `by`, `window`, `per`, `when?`, `from?`, `to?`,
  `writer?`, `kinds?`, `k` (1–50, default 10) and `samples` (0–5, default 3).
- Annotations: `readOnlyHint: true`.
- Description: "Find time periods of the journal by how the activity looked
  (activity, new information, repetition, errors, unusual for this agent), in
  bits; optionally only periods that mention QUERY; dates in plain words
  (`yesterday`, `last tuesday`, `before "deploy"`)."

**`search` gains** `when` and `session`, so that an agent can open a period
with the same tool it already knows.

### 5.3 Result shape and drill-in

```json
{
  "when": {"phrase": "after the first \"failed test\"", "from": "2026-10-14T09:12:03Z", "to": "2026-10-15T09:12:03Z",
           "anchor": {"id": 4211, "ts": "2026-10-14T09:12:03Z", "session": "9c1e…", "pick": "first",
                      "ambiguous": false, "occurrences": 3}},
  "by": "errors", "window": "5m", "per": "session", "baseline": "writer, trailing 7 days",
  "periods": [
    {"rank": 1, "writer": "agent:demo", "session": "9c1e…", "sessions": 1,
     "from": "2026-10-14T10:05:00Z", "to": "2026-10-14T10:20:00Z", "minutes": 15,
     "score_bits": 20.1,
     "events": 96, "calls": 44, "errors": 19, "error_bits": 20.1,
     "novel_bits": 121700, "bits_per_event": 1268, "unusual_bits": 15.6,
     "unusual": {"rate": 0.8, "mix": 4.1, "errors": 10.6, "new": 0.0, "novelty": 0.1},
     "families": {"test": 51, "edit": 20, "read": 12},
     "text_bits": 0.0,
     "samples": [{"id": 4302, "kind": "tool_result", "ts": "…", "excerpt": "…"}]}
  ]
}
```

- **Adjacent merge:** adjacent windows of the same session that both reach
  the top k merge into one period (max score, summed counts). Durations then
  follow the activity, for example the three-window error burst above.
- **Sibling fold:** periods of different sessions of the same writer in the
  same clock window fold into one result. It lists `sessions: N` and keeps
  the best. This answers the measured fan-out duplication; `--all` turns it
  off.
- **Samples:** the period's most characteristic records, with exact excerpts
  as `search` returns them.
  - `errors`: the failing results.
  - `repeat`: the most repeated call.
  - `novelty`: the records with the most new bits.
  - `unusual`: the records behind its largest component.
- **Drill-in:**
  - CLI: `oojuro range FROM TO --writer W --session S` lists every record.
  - Both surfaces: `get ID` reads one record; `search QUERY --from --to
    --session` searches inside the period.

### 5.4 Dates: phrases by rules

The grammar is implemented and tested in `scripts/activity/dates.py` (43/43
cases).
- **Time zone:** the caller's zone (CLI: local; MCP: the recorder's local
  zone unless a `tz` is passed).
- **Periods are half-open** `[from, to)`. A period that would end in the
  future ends now.

| form | meaning |
|---|---|
| `today`, `yesterday`, `day before yesterday`, `now` | calendar days (now = the last 15 minutes) |
| `tuesday` / `last tuesday` / `tuesday before last` | most recent on or before today / strictly before today / one week earlier |
| `this|last week`, `this|last month`, `this|last year`, `last hour` | calendar units; weeks start on Monday |
| `past 3 days`, `last 2 hours`, `the last few hours` | rolling, ending now (`few` = 3, `couple` = 2) |
| `3 days ago`, `2 hours ago`, `a week ago` | the calendar unit containing that moment |
| `oct 14`, `october 14th`, `14 oct`, `10/14`, `2026-10-14` | that day; without a year, the most recent past occurrence; `a/b` is month/day |
| `september`, `in october` | that month, most recent occurrence |
| `<day> morning|afternoon|evening|night`, `this morning`, `last night`, `tonight` | 06–12, 12–18, 18–24, 18–06 |
| `<day> at 3pm`, `<day> around 9am` | that clock hour / ±1 hour |
| `since X`, `after X`, `before X`, `until X`, `between X and Y`, `from X to Y` | bounds from the periods of X and Y |
| `7d`, `1h`, RFC 3339, `@unix` | the existing CLI forms (`7d` = since then) |

Anything that does not parse is an anchor if a relation word introduced it,
otherwise an error that lists the forms. Nothing is guessed.

### 5.5 Event-relative anchors

**Form:** `before|after|around|day of|hour of|during <anchor>`, or `between
<anchor> and <anchor>`.
- An anchor is a quoted search phrase with an optional ordinal:
  `the first "failed test"`, `last "migration"`, `top "oom killed"`.
- Or a record id: `before @4211`.
- An unquoted phrase works too (`before the deploy`): leading articles and
  ordinals are stripped.

**Resolution:**
1. **Search** the anchor phrase, with the same writer filter and outer date
   bounds as the query and k = 1000, time-ordered. The search leaves out the
   caller's own context window (`outside_context_of`).
   - Measured: a query phrase the agent had itself just typed resolved to
     its own session until excluded.
2. **Candidates:** hits saving at least `max(min_bits, ½ · best)` bits.
3. **Occurrences:** consecutive candidate hits of one session within 30
   minutes are one occurrence.
4. **Pick:** `first` is the earliest occurrence; `last` (the default) the
   latest; `top` the occurrence with the most bits. The anchor time is the
   pick's first hit, or its last hit for `after`.
5. **Period:**
   - `before`: [t − 24 h, t)
   - `after`: [t, t + 24 h)
   - `around`: ±1 h
   - `hour of`: ±30 min
   - `day of` / `the day`: the calendar day
   - `during`: the anchor's session, from first to last record
   - `--span` changes the 24 h.
6. **Ambiguity:**
   - The result always carries the anchor (id, time, session) and the number
     of occurrences.
   - It sets `ambiguous: true` with the top 5 occurrences (id, time, session,
     bits, a 160 B excerpt) when no ordinal was given and the occurrences
     fall on more than one day.

**Measured** on the live journal through the CLI (0.2–0.3 s per anchor with
k = 1000; an engine-side "latest relevant hit" should take under 20 ms):
- A common word ("deploy", "migration", "failed test") has 1,000+ relevant
  hits on 29–32 different days.
- With k = 50 sorted by score, "last" picked a weeks-old hit, so the time
  ordering of step 1 is required.

**How an agent should phrase it:**
- Quote a distinctive phrase, ideally one from a tool call rather than chat:
  `before "make release"` rather than `before the release`.
- Add an ordinal when it matters.
- When `ambiguous` comes back, re-ask with `@id` from the occurrences.

### 5.6 Examples

```
# Where did an agent repeat itself yesterday? (5-min windows per session, merged)
oojuro periods --by repeat --when yesterday

# Error bursts last week in periods that mention migrations
oojuro periods "migration" --by errors --when "last week"

# Hours unlike this agent's normal, since Monday
oojuro periods --by unusual --window 1h --per writer --writer codex:proj --when "since monday"

# What new material came in after the first failed test?
oojuro periods --by novelty --when 'after the first "failed test"' --samples 5
```

```json
{"name": "periods", "arguments": {"by": "errors", "when": "the day \"build broke\"", "k": 5}}
{"name": "search",  "arguments": {"query": "error", "session": "9c1e…", "from": "2026-10-14T10:05:00Z", "to": "2026-10-14T10:20:00Z"}}
```

## 6. Engine plan (superseded, never implemented: see §11)

### 6.1 At ingest, per activity record

The recorder's single writer computes these after redaction and the policy
check:

| column | how | stored |
|---|---|---|
| `nov` | the writer's streaming zstd context (level 3, 8 MiB window), one block flushed per record; bytes of output | varint stream in the sealed column blocks of `oojuro-trace` (`Col` + encoder); 1.36 B per record measured |
| `fam` (4 bits), `err` (2 bits), `rep` (1 bit) | family rules over tool name and input; harness status, else text markers; hash of (tool, input) against the session's last 32 calls | one byte stream; 0.34 B per record measured |

- **Contexts:** at most 6 live zstd contexts (LRU, about 10 MiB each). A cold
  writer is re-primed by compressing, and discarding the output of, its last
  8 MiB of live records; that is an estimated 30 ms (8 MiB at level 3, not
  measured).
- **Repeat ring:** the last 32 call hashes per open session, in memory only.
- **First-seen tools:** from the existing tool-id table.
- **First-seen files:** a per-writer map of path hash → number of live
  records naming the path. The count makes it erasable (§6.5).

### 6.2 Rollups

**Two keyed maps** of the same row type:
- `(writer, 5-min bucket)` and `(session, 5-min bucket)` → events, calls,
  results, errors by level, bytes, `nov` sum, first-seen tools and files,
  repeats, a sparse family histogram, and first and last id.
- They live beside the column table, are saved in its checkpoint
  (`cols.ckpt`), and are rebuilt from the columns on open. They are pure
  sums, so they need no durability of their own.

**Larger periods are sums at query time:**
- 1 h = 12 rows; 1 d = 288 rows.
- A session is the sum of its rows; one extra row per session holds its
  first and last times.
- Phases, if adopted later, add rows of `(session, phase)`.

**Query time:**
- Baselines (trailing 7 days, same window kind) and the rarity calibration
  are computed from the rows. Nothing about "normal" is stored, so it forgets
  by itself.
- A text query joins search hits (ids → bucket) to rows.

### 6.3 Costs and targets

- **Storage:** about 2.3 B per record in total (columns 1.7 B per activity
  record, rollups about 1.1 B per record). That is about 0.3% of the journal
  (§3.5).
- **Ingest:**
  - The rules cost about 1 µs per record in Rust.
  - zstd level 3 with a flush per record runs at C speed; the prototype
    measured 99 MB/s including Python overhead.
  - The measured rate is about 1 GB of text a month, so this adds seconds per
    month. A full re-import of today's journal adds about 10 s.
- **Memory:** about 60 MiB for 6 live contexts, within the recorder's
  `MemoryMax=1G`.
- **Query latency targets** (TraceBench: add a `periods` workload):
  - one writer, 30 days, no text (up to 8,640 rows): p99 < 10 ms. The Python
    prototype took 27 ms (writer windows) and 88 ms (session windows);
    Rust should be 10–30× faster.
  - every writer: < 50 ms
  - with a text query: search plus join, < 15 ms
  - anchor resolution, time-ordered relevant hits: < 20 ms

### 6.4 Erasure: rollups and derived state forget too

When `forget` removes records (after its existing tombstones and reclaim):
1. **Rollup rows** of the affected (writer, bucket) and (session, bucket)
   keys are recomputed from live columns. Rows left empty are deleted.
2. **`nov` of later records** was conditioned on the forgotten text. It is a
   number with no text, but it is still derived. For each forgotten run,
   recompute `nov` for the writer's live records in the following 8 MiB: re-prime
   from the live records before the run, then recompress. `--older-than`
   touches only the first 8 MiB after the cut.
3. **The live zstd context** of the writer is dropped and re-primed when it
   holds forgotten text.
4. **Path-hash counts** are decremented; entries that reach 0 are removed.
   Repeat rings of the affected sessions are cleared.
5. **Baselines and calibration** are computed at query time, so they forget
   with no extra step.
6. **Phases (if adopted):** affected sessions are decoded again. The model
   parameters are about 200 aggregate numbers, refit on schedule.
7. **The proof gains a derived-state check:** rollups equal a from-scratch
   rebuild, no context holds a needle, and no path hash survives without a
   live record.

### 6.5 Tests to write (synthetic data only)

- **Rules:** family per command (a table of synthetic shell lines and tool
  inputs), error level per format (Claude status, Codex text), the repeat
  ring.
- **Novelty:**
  - an identical record twice: the second costs at most 10% of the first
  - 50 identical calls: bits per event under 1/5 of a varied sequence
  - restart and re-prime: bits within a block's overhead of an uninterrupted
    stream
- **Rollups:** incremental equals batch over random synthetic journals;
  hours and days equal direct sums; per-session equals per-writer when there
  is one session.
- **Signals,** on a synthetic writer with a steady baseline and injected
  episodes:
  - a loop ranks first by `repeat`
  - an error burst ranks first by `errors`
  - a burst of new tools and files ranks top by `unusual`
  - a large new read ranks top by `novelty`
  - a big but ordinary window is *not* unusual (shape, not size)
- **Dates:** the 43 cases of `dates.py`, plus DST changes, time zones, year
  wrap and future dates.
- **Anchors:** first/last/top picks, occurrence clustering, the ambiguity
  flag, `@id`, and the anchor never resolving to the caller's own context.
- **Erasure:** forget an episode, then rollups, `nov`, contexts and path
  hashes equal a rebuild without it, `periods` no longer returns it, and the
  proof passes.
- **Surfaces:** CLI and MCP round trips for `periods` and `--when`;
  adjacent merge; sibling fold; a latency bench on a 30-day synthetic
  writer.

## 7. Recommended defaults

| setting | default | why (measured) |
|---|---|---|
| novelty estimator | `zs` (zstd level 3, 8 MiB window, flush per record) | same ranking as the alternatives; best-or-tied for loops; fastest; erasable |
| window | 5m for `repeat`/`errors`, 1h otherwise | loops 31.7% exact repeats at 5m vs 14.6% at 1h/s |
| unit | per session (`--per writer` on request) | sharper for loops and errors; writer windows mix parallel sessions |
| minimum events | 20 for `repeat` and `unusual` | removes the tiny-window artifacts seen in the checks |
| baseline | writer's own windows of the same kind, trailing 7 days | as defined in the design; calibrated per component |
| exposure cap in `unusual` | 50 events | without it, unusual tracked volume (ρ up to 0.86) |
| error rules | harness status first, then text markers | Codex has no status; 18/20 precision |
| anchor pick | latest occurrence, ambiguity flagged | frequent phrases span about 30 days |
| anchor span | 24 h before or after | |
| phases | off (option) | §4 |

## 8. Risks

- **Writer granularity:** one writer holds 90% of the records, because every
  session starts in the home directory. Its baseline pools coordinator and
  subagent work, so `unusual` flags every subagent's opening read burst.
- **Status is not exported:** `Record` lacks the stored tool status, so the
  prototype inferred all errors from text. The engine reads the column; Codex
  needs the text rules anyway, and those are heuristic.
- **Novelty totals are mostly volume.** Per-event novelty confuses loops with
  windows of many tiny records (5/20).
- **"Repeat" mostly finds waiting,** not stuck agents. Telling the two apart
  needs the error or progress context (§9).
- **Family rules** mislabel shell edits done through interpreters; the error
  propagates to mix and phases.
- **Anchors:** frequent phrases resolve to the latest mention, and an agent's
  own context matches its own anchor phrase unless excluded.
- **Codec drift:** `nov` depends on the zstd version and level. Store the
  codec id with the column, and recompute or accept drift on upgrade.
- **Time zones:** "yesterday" for an MCP caller depends on a zone the agent
  may not state.
- **Phases:** the learned states drift with their start values.

## 9. Open questions

1. **Period unit:** per session by default (sharper loops and errors), or
   per writer by default (the agent as a whole)?
2. **Window default:** per signal (5 min for repeat and errors, 1 h
   otherwise), or one fixed default for every signal?
3. **Novelty ranking:** total new bits (the most new material; tracks
   volume), or bits beyond the writer's typical bits per event?
4. **What `repeat` means:** any repetition, including waiting and polling
   (most of what it finds), or only repetition with errors or no progress
   (a separate `stuck`)?
5. **`unusual` baseline:** the writer's own windows (as specified; flags
   every subagent start-up), or split by role (coordinator sessions vs
   subagent sessions)?
6. **Phases:** ship fixed windows with adjacent-window merging now and keep
   the HMM as an option, or adopt the HMM now (`--window phase`, `--phase`)?
7. **Error output at ingest:** infer error output from text (needed for
   Codex; 2/20 weak hits), or harness status only?
8. **Anchors without an ordinal:** resolve to the latest occurrence and flag
   ambiguity, or refuse and return the occurrences?
9. **First-seen files in `unusual`:** keep them (refcounted path hashes per
   writer), or drop them and keep first-seen tools only?
10. **Time zone for MCP date phrases:** the recorder's local zone, or UTC
    unless the agent passes `tz`?

## 10. Decisions (2026-10-21)

| Question | Decision |
|---|---|
| Grouping | **Task tree by default**: a top-level session plus every subagent it spawned (recursively). Signals are detected inside each session (loops and error bursts live there); results are presented per task tree with the contributing sessions listed. `--per session` and `--per writer` switch the level. Why: a writer is one harness in one project folder, so on the measured machine one writer holds 84% of all records ("per agent" = everything), while per session floods results with parallel siblings. The first choice was "per session, siblings grouped", then "per agent" was preferred because of the many subagents; the task tree is both. |
| Novelty ranking | New bits **beyond the agent's usual** (not total new bits, which mirrors volume). |
| Loops (`repeat`) | **Stuck loops only** by default; `--include-waiting` adds waiting/polling. |
| Phases (HMM/Viterbi) | **Later, as an option**; keep the prototype. |
| Window size | 5 min for loops and errors, 1 h otherwise (default, not asked). |
| Baselines | Separate baselines for coordinator and subagent sessions (default, not asked). |
| Errors | Harness status, plus output-text detection where there is no status (Codex) (default, not asked). |
| Anchor without first/last | Latest occurrence; the answer says which (default, not asked). |
| Time zone | The person's local time zone from settings, else the machine's (default, not asked). |
| First-seen files in "change vs normal" | Kept, now that baselines are split (default, not asked). |

## 11. Implemented (2026-10-21)

Ryan's direction after §10: "the db as simple and horizontal as possible,
just enough information for agents to know what happened; if periods slows
down the performance, remove such functionalities" (`docs/DESIGN.md`,
"Principle"). So nothing of §6 was built: no per-record features at ingest,
no rollups, no compressor state. `periods` is a pure query-time function
over what every record already carries, in one file
(`crates/oojuro/src/periods.rs`) that can be deleted with its request,
command and tool (§11.8).

### 11.1 Identity: agent id and parent id

- **Agent id** is the existing session hash: one per transcript, so one per
  agent instance (every main session and every subagent). Records now show
  it as `agent_id` (the same 16 hex digits as `session`).
- **Parent id**: the instance that spawned this one.
  - Stored as a 12th field of the event metadata line, only when there is
    one: every other record is byte-identical to before (TraceBench data
    included).
  - In memory, one entry per session (`ColStore::parents`), saved as an
    optional trailing checkpoint section (older checkpoints load without it)
    and dropped with the session's last record at reclaim. There is no
    per-event column, so no existing read changes.
- **Where knowable:**
  - Claude Code subagents, from the path:
    `<project>/<session>/subagents/agent-*.jsonl` names
    `<project>/<session>.jsonl` (log tailer and hooks).
  - Codex subagents, from `session_meta.source.subagent.thread_spawn.parent_thread_id`,
    resolved by the capture owner to the parent rollout's session among the
    files it tails.
  - Proxy and appended records when the caller passes one
    (`DirectRec.parent_session`, `NewRecord.parent_session`).
  - Not knowable: the model API proxy (a subagent's calls carry the main
    session), OpenTelemetry (no standard attribute).
- **A task tree** is an agent id and its descendants, computed at query
  time.
- On the scratch copy (§11.7), 1,253 of 1,978 agent instances (63%) name a
  parent: 1,192 Claude Code subagent transcripts and 61 Codex subagent
  rollouts.

### 11.2 Signals from the columns

The column table holds, per record: valid time, kind, tool, status, exit
code, text length, agent id (parent through the session map) and the call a
result answers. `periods` reads nothing else, except the exact check of
repeat candidates.

| signal | as built | differs from the design |
|---|---|---|
| volume | activity records per window (no `system` context, no Oojuro tool calls or results) | echo is recognized by tool name only: a Bash-run `oojuro` command counts (the mark is in the metadata, not the columns); injected context is a `system` record anyway |
| errors | failed tool results (the harness status, or a nonzero exit code); binomial upper-tail bits against the writer's trailing 7-day rate, per role | no output-text rules (they need text). Codex outputs now get a status at capture, from what Codex itself reports (`Script failed` / `Script completed`, `Process exited with code N`, `Exit code: N`); the field existed. API-error `system` records are not counted (no column says so) |
| repeat | stuck loops (default): a call matching one of the instance's last 32 calls by tool and input length, whose result fails again; `--include-waiting` adds the matches that succeed (polling). Binomial bits against the trailing candidate rate | the design ranked low novelty per event; Ryan chose stuck loops only. The record dedupe hash cannot help: captured records carry their source position in their metadata, so they never dedupe and share no hash. The length rule alone is 70–80% exact in the top error and repeat windows (18% in fan-out hours: many same-length reads), so when ranking by `repeat` the candidates of the windows that can still reach the top are checked against the two calls' texts and only exact repeats count. The check stops as soon as the 3k-th checked score beats the next unchecked candidate (a checked score never exceeds its candidate score). Elsewhere `repeats_exact: false` marks candidate counts |
| unusual | five components, each `-log2` of its empirical tail among the writer's windows of the same role in the trailing 7 days: rate, tool mix (MDL bits, exposure capped at 50), error rate, tools absent from the trailing week, bytes per record (two-sided) | tool-name mix instead of families (families need the shell command); no first-seen files (paths need text); bytes per record instead of novelty density. Tails come from Fenwick counts over quantized values (2,048 log-spaced bins) |
| novelty | not available (§11.4) | |
| text | with a query and no signal: the bits of its search hits per window; with a signal, a query only filters | hits below the relevance floor of §11.5 are ignored |

- **Roles:** windows of top-level instances and of subagents have separate
  baselines (Ryan's default). `--per writer` uses one baseline per writer
  with empty slots counted as zero.
- **Windows:** clock-aligned in the person's zone, 5 min for `errors` and
  `repeat`, 1 h otherwise (`--window`, 1 min to 1 day).
- **Baseline span:** errors and repeat read 7 days before the period;
  unusual reads 14 (its baseline windows need their own 7-day baselines);
  volume and text read none.
- **Minimums:** `repeat` needs 2 repeats in a window, `unusual` 20 records.

### 11.3 Periods: grouping, merging, results

- **Grouping** (`--per`): `task` (default) groups the windows of an agent
  instance and all instances it spawned; `session` keeps one instance;
  `writer` sums all of a writer's instances per window.
- **Merging:** windows are taken best first. A window joins a period of its
  group that it touches when it scores at least half that period's best
  window; a weaker neighbour is left out, so continuous work does not chain
  into one long period. Otherwise it opens a period while fewer than `k`
  exist. A fan-out of parallel subagents folds into one task period.
- **Result** (`Periods`, JSON):
  - the resolved `when`
  - signal, window, baseline, period bounds, records read, elapsed ms
  - per period:
    - rank, task root (`agent_id`), writers, bounds, minutes, windows
    - `score` (bits; records for volume), counts (records, calls, results,
      errors), the per-signal bits, the unusual components, bytes
    - top tools
    - contributing instances with their parent ids (busiest first, at most 20)
    - sample record ids with why each was picked (`error`, `stuck`,
      `waiting`, `new_tool`, `first`, `hit`)
    - a `range` command to open the period

### 11.4 Novelty: dropped

"New bits beyond the agent's usual" needs each candidate window's text plus
a compressor primed with up to 8 MiB of the writer's preceding text. The
top 10 periods by volume on the scratch copy are 24 one-hour windows
holding 131.7 MB of text (5.5 MB per window). Novelty on demand for them
means decoding and compressing well over 100 MB per query: hundreds of
milliseconds against a 50 ms bar. `--by novelty` answers with this reason.

### 11.5 Dates (`--when`)

- **Implemented** in `crates/oojuro/src/when.rs` with the grammar of §5.4
  and §5.5. The 43 prototype cases pass, plus daylight saving, zones, year
  wrap, future dates and anchor picks.
- **Time zone:** the `timezone` setting (an IANA name), else the machine's.
- **Differences from the prototype:**
  - A whole phrase that is a period wins before relation words (the
    prototype read "the day before yesterday" as an anchor).
  - A bare clock (`9am`) is today.
  - **Relevance floor of anchors:** `max(min_bits, best / 2)`, or `best / 2`
    when even the best hit saves less than `min_bits`. A phrase found in a
    single record saves almost nothing in the engine (its background is then
    that record, `docs/SEARCH_FIXES.md`), and a distinctive phrase is exactly
    what an anchor should be.
- **Anchors:**
  - ids come from the ranked search without reading text, time-ordered.
  - Occurrences: one instance, gaps under 30 minutes.
  - `first`/`last`/`top`; the latest by default.
  - `ambiguous` with the top 5 occurrences when no ordinal was given and they
    fall on several days; `@id` names a record.
  - `--outside-context-of` leaves out the caller's own context window. An MCP
    caller cannot name its own session, so quote a distinctive phrase or use
    `@id`.
- **Where:**
  - CLI `search`, `range`, `export`: the meaning is echoed on stderr.
  - `periods`: in the result.
  - MCP `search` and `periods`: `when` in the result.

### 11.6 Erasure, restarts, deduplicated records

- **Erasure:** `periods` reads live columns only, so a forgotten record
  leaves every answer at once (its tombstone). Its session's parent link goes
  at the reclaim that follows. Test: forget a planted loop; `repeat` no longer
  returns it, the link is gone, and a reopened journal answers the same.
- **Restarts and crashes:** nothing derived is stored, so they are the
  column table's (checkpoint plus replay). The parent map rides in the
  checkpoint and is rebuilt from the records without one. Tests: reopen from
  a checkpoint, after a crash past it, with none, and from a checkpoint
  written before the section existed.
- **Deduplicated records:** every event, a deduplicated one included, has
  its own column slot with its own valid time, and `periods` counts each
  event at that time. The time of the row that stores the shared text (its
  first event, `docs/SEARCH_FIXES.md`) is never used.

### 11.7 Measurements

Scratch copy: every transcript on this machine (1,968 files, 5.5 GB)
imported with this build into a private store under `.scratch` (551,522
records, 74 writers, 401 MB; deleted afterwards). One writer holds 84% of
the records. Only counts and timings were kept; the hand check read
records locally.

**Latency** (one query, `elapsed_ms`, median of 5, bench lock held, load
about 9.5 on 16 threads, `--when "past 30 days"`, k = 10):

| scope | volume | errors | repeat (with the exact check) | unusual |
|---|---|---|---|---|
| the busiest writer (388k records read with the 7-day lead) | 13.8 ms | 28.0 ms | 24.1 ms | 30.0 ms |
| every writer | 15.0 ms | 25.6 ms | 29.4 ms | 30.0 ms |

- **Against the bars:** under the 50 ms bar, above the 10 ms target.
- **With a text part** (all writers):
  - An anchor (`--when 'after the first "migration"'`) adds 20–25 ms:
    1,000 ranked ids, no text, 46–58 ms with the errors ranking.
  - A text query (`periods "migration" --by errors`, 30 days) adds about
    30 ms (up to 10,000 hits): 58–62 ms.
  - Both are over the 50 ms bar, which was set for a column-only query.
- **Where the time goes:**
  - Decoding about 400 sealed column blocks (zstd plus 8 varint streams)
    takes about 10 ms.
  - The per-record bookkeeping (window, repeat ring) about as much.
  - Scoring 2–3 ms.
  - The scan reads each block once and never fills the shared column cache.

**Top 10 per signal, hand-checked** (generic):
- **repeat** (exact, stuck only): 4 periods in 30 days.
  - 3 are stuck loops: a disk check re-run while writes kept failing, a
    failing migration command retried, a failing edit retried.
  - 1 is a browser wait-and-screenshot loop with some failures.
  - The prototype's 12 waiting loops are gone by default and come back with
    `--include-waiting`.
- **errors:** 10/10 real failure bursts: a full disk across 4 agents,
  failing authenticated API calls, migrations, builds and tests, failing
  file reads. The prototype's 2/20 weak hits came from text rules, which are
  gone.
- **unusual:** no window is the routine "new subagent's opening read
  burst" (13/20 in the prototype, before the coordinator/subagent baseline
  split). The top 10 are distinct moments:
  - a production hotfix across 8 agents
  - a mass issue-tracker cleanup across 11 agents
  - cloud and browser operations
  - third-party code research
  - credentialed API work
- **volume:** fan-out peaks and long coordinator stretches, as in the
  prototype. Task periods group up to 11 instances (median 4 in the top 50).

**Storage:** the only new bytes are the parent field of subagent records
(`,<parent>`, at most 21 characters before compression) and 16 B per
spawned session in the column checkpoint.
- **Method:** the same frozen 400 MB of transcripts (5 sessions, 271 of
  them subagents, so nearly every record carries a parent: the worst case)
  imported by the base build and by this one.
- **Result:**
  - archive +57 KB (+0.20%)
  - column checkpoint +4.3 KB (the parents section)
  - index identical
  - durable bytes +0.14% (0.7 B per record)
- On the whole journal, where 63% of the instances are subagents, it is
  less.

**Write and read paths, before (d1de288) vs after:**
- **Setup:** TraceBench tier 1 (`base1m.tbe`), arm `oojuro`, both builds
  without the competitor arms, one bench-lock hold per run, 3 GiB scope.
- **Machine:** shared and very loaded during these runs. Only round 0 ran
  at load 13; rounds 1–2 at load averages of 40–280. So each suite is also
  given in alternating order (ABBA), where the order effect of a store just
  deleted by the previous run cancels out.

| suite | measure | before | after |
|---|---|---|---|
| ingest (6 ABBA pairs, load 16–18) | ops/s, median | 7,891 | 8,525 |
| | p50 / CPU seconds per run | 1.22 ms / 5.6 s | 1.15 ms / 5.6 s |
| turnloop (3 rounds) | turn p50 | 4.80 ms | 4.69 ms |
| | search top-20 p50 / append p50 | 2.12 / 2.55 ms | 2.08 / 2.46 ms |
| searchprobe, 1,000 turn-loop queries (3 rounds) | CPU p50 / p99 | 1.40 / 2.85 ms | 1.28 / 2.48 ms |
| reads (5 runs each, 2 in reverse order) | p50: get, get_range, session | 0.367, 0.310, 0.768 ms | 0.392, 0.336, 0.871 ms |
| | p50: time_range 1 h, filter 0–3 | 12.0; 0.74, 1.22, 0.059, 22.6 ms | 11.6; 0.70, 1.19, 0.059, 20.3 ms |
| | p50: count 0–3 | 1.08, 1.20, 0.063, 0.72 ms | 1.15, 1.17, 0.073, 0.48 ms |
| | p50: agg_session, agg_tools 1 d / 30 d, top_sessions, mixed 8 threads | 0.014, 1.20 / 1.65, 1.31, 0.20 ms | 0.015, 1.28 / 2.14, 1.36, 0.24 ms |
| same store, same 20k random ids, 4 alternating runs each | get, get_range 512 B, session p50 | 8.85 µs, 6.55 µs, 663 µs | 9.10 µs, 6.50 µs, 672 µs |

- **Spread:** the run-to-run spread of the reads suite is ±30–60% per
  operation (min–max in `.scratch/bench`). No operation differs beyond it,
  and the code under them did not change: `get`, `session`, filters,
  counts, aggregates and scans read the same column blocks through the
  same functions.
- **Two cases checked further:**
  - `agg_tools_30d` is +2% in the reverse-order pairs. Its +30% comes from
    the rounds where the new build always ran first.
  - The small get/session differences vanish on identical work against one
    store (last row).
- **Verify:** `tracebench verify` on smoke gives 1,237/1,237 for `oojuro`
  and `oojuro:log=private`.

### 11.8 Removing it

- **Delete:**
  - `crates/oojuro/src/periods.rs`
  - `Request::Periods` (`proto.rs`)
  - `Oojuro::periods` (`client.rs`)
  - `periods_cmd` and its usage line (`bin/oojuro.rs`)
  - the `periods` MCP tool (`mcp.rs`)
  - `tests/periods.rs`
- **Optionally:** `TraceDb::scan_agent`, `ActivityRow` and the column
  projector.
- **Kept:** agent and parent ids, `--when`, Codex statuses.
