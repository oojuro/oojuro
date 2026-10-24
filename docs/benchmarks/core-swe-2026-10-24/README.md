# Core read/write, storage and RAM comparison

Measured 2026-10-24. One local warm development workload; no hosted or pricing claim.

**Current Oojuro used 6.11% less peak RSS (10.79 MiB) than the older native control, with search plus originals 5.37% slower.** Durable turn-batch append was similar. The RAM saving did not remove Oojuro's large memory difference from the disk-backed FTS baselines.

## Results

Median of four run medians. Each arm ran the same 201 turn queries four times (804 successful timed queries per arm), through four fixed Latin-order rotations. Times are milliseconds; disk/RAM are MiB (2^20 bytes).

| Arm | Durable append p50 ms | Search + originals p50 ms | All-file disk MiB | Peak RSS MiB | nDCG@20 | Recall@20 |
|---|---:|---:|---:|---:|---:|---:|
| Oojuro current f726 | 1.38 | 149.62 | 2.61 | 165.90 | 0.368 | 0.593 |
| Oojuro control 7633 | 1.38 | 141.99 | 2.61 | 176.69 | 0.368 | 0.593 |
| SQLite FTS5 | 1.37 | 83.87 | 26.95 | 23.14 | 0.288 | 0.402 |
| JSONL + Zstd + FTS5 | 2.91 | 83.64 | 12.35 | 34.89 | 0.288 | 0.402 |

The native current/control labelled query pools, scores, originals and quality were identical. SQLite and JSONL-Zstd labelled pools were also identical to each other. Native and FTS ranking differ: native returned 4,944,282 original bytes per run; both FTS baselines returned 8,855,107. Quality uses only **52 unique retrospectively labelled queries**, repeated across runs. These differences prevent an overall efficiency or equivalent-relevance claim.

The current/control search+original run medians were 148.18–149.98 ms versus 138.92–144.11 ms. Peak RSS ranges were 165.83–165.92 MiB versus 176.66–177.01 MiB. All four values and every measured field are retained in the JSON companion.

## What was verified

- Same public SWE dev selection: first 10 trajectories, 20,000 background records, k20, batch1000, same clocks/canaries, no tuning, fresh stores, 3GiB/no swap per arm.
- 20,623 complete records per store; all 16 copied stores reopened and every Doc field matched the reconstructed input. No extra/missing originals or invisible hits.
- 28 existing correctness checks across four arms, including 12 deliberate SIGKILL cases; 2,400 acknowledged records survived exactly.
- Complete closed-store apparent and allocated footprints matched the scorecards. The independent full text denominator is 14,716,773 bytes, including canaries; the harness's 14,714,211-byte denominator excludes their 2,562 bytes.
- Four rounds completed without retries, retuning, fallback or excluded arms. Measured controller and verification finished in 456.014s.
- All 64 owned scopes and original process identities (including the earlier gates), actual observed outer/first-arm cgroup paths, and the controller PID independently absent afterward. Stores and inputs remain retained.

## Durability and scope

JSONL-Zstd is an indexed compressed baseline: Zstd3 immutable batch files plus contentless FTS5, payload and directory fsync before FULL-WAL commit; all payload/index/WAL/SHM/orphan files count. SQLite also uses FULL-WAL durability. Native uses the default encrypted sealed local Store, public fixture key, Zstd3/dictionary compression, and a RAM projection; native excerpts and fresh view materialization are inside search. The plaintext FTS baselines provide different security/features. Single-host ACK survival is matched; replication/backups and failure domains are not qualified.

This is warm local engine/API evidence. Post-prefill 'steady' RAM is sampled **before native search projection creation**; peak RSS above covers replay and temporary allocations. It is not precise loaded-index residency. Copied-store reopen checks prove integrity, not cold-OS latency. The harness emits empirical p99 fields, but no production tail claim is made from about 201 calls/run. No larger-than-RAM result: native still bounds retained state at 100,000 records /128MiB. PostgreSQL has no current RecordLog comparator arm; older Trace/Nine comparisons are separate. No AWS infrastructure, IAM, deployment, package release or source algorithm changed for this experiment.

## Reproducibility

Current source: f726bab51d982896a129d4bdf2fd2d3a2edc2fff ([draft82](https://github.com/oojuro/oojuro/pull/82)); control: 7633eefe7503e355635522dd3e8f67fce3b143d7. Benchmark/bench/scripts trees identical, so no suite rebase or rewrite. Explicit benchmark build: no default features, sqlite-arm only; actual Rust/Cargo1.99.1, portable baseline x86-64, same CC/CXX/linker, opt3/non-test, fresh:false exact lib/bin source artifacts. Current binary SHA256 a4b861d7049c6125f8b42b59fba98fa19775a44fbdbc79c06f452cb6d3086a51; control f35af93ca0167707dcaa77d7b17dd79814294f9b8e4264bf66a9571047d718a3.

Retained public prepared dataset SHA2563106438b52feadab2bcf3c0e7ff7fb40ac8625b3969312e8cf64d4c3782089c2 reverified. Frozen controller f2e23a7891b922dd7f76612acd0fa6b78167db27600e53ad976cf3d1ebd917cf; strict launcher c432d1874483cfe1086e666dcad1597eb0eaaa1b141e96a31da0df248c5ddc68. One benign scope smoke passed; an immediate state-observer assertion failed and was reconciled independently without repeating it. That evidence limit and historical failures remain recorded.

The next core optimization should target retained decoded state and repeated query materialization, while preserving exact originals, ranking and durable recovery. This experiment does not authorize a runtime change.

## Archived evidence

This docs-only topic is stacked on draft82. Runtime, benchmark source, Cargo manifests, workflows, notices and licenses are unchanged. No new test run is needed for archiving the already qualified results.

The four scorecards, correctness card, exact commands, selected build receipts and independent cleanup proof are included here. Artifact-manifest.json distinguishes the SHA256 of each retained original from its published path-redacted snapshot. Host-specific workspace, home, temporary and cgroup path strings were replaced with named placeholders, and source snapshots have trailing blank lines normalized; numeric results and source/compiler/artifact identities remain unchanged. Controller/build/launcher source snapshots end in .txt and preserve the reviewed logic for inspection; they are not drop-in scripts from this documentation directory. Substitute local paths and recreate the documented work layout to reproduce. The exact original scripts and compiler JSON remain retained locally under the recorded hashes.

No binaries, corpus records, original-text stores, query-pool datasets, private agent history, live provider data, credentials or keys are included. Pool equality is attested by the verified receipt; derived pool file hashes are retained locally. The one scope-smoke observer refusal is included, with its unrecorded initial query-state limitation and independent subsequent cleanup. No favorable rerun was performed.
