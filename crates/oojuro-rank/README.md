# Optional ranker foundation

`oojuro-rank` is an isolated, dependency-free executable contract for the **rescore**
hook in the draft engine specification, section 7. It is not wired into Nine, the CLI,
the cloud service or the write path. No retrieval behavior changes by adding this crate.
No association, translation, compression-distance or embedding ranker ships here.

The caller supplies an analyzed query and one writer's already-authorized, visible
lexical top-N (at most 1,000 by default). The slice is the entire rescore window;
the caller owns any lexical tail and must define its merge/floor behavior before
integration. Candidates carry only IDs, writer, time and lexical bits. A ranker gets
that slice and returns one delta per row in the same order. It cannot add a row.
The pipeline checks writer scope, unique IDs, finite scores, dimensions, finite
weights and finite sums, then applies each stage transactionally. Every enabled
stage has an identity/table receipt and a reason if skipped. Hit deltas line up with
receipt order. Successful fusion sorts by bits descending, time descending, ID as
text ascending. Disabled stages are never invoked; with no successful stage, the
original lexical order and score bit patterns are preserved exactly.

A missing table, returned error, exhausted work budget or invalid output skips that
stage. The budget limits declared work units and input dimensions. Work units must
be documented and charged by trusted ranker code. This **does not enforce wall-time,
memory, network isolation or panic containment**; an in-process trait cannot stop an
uncooperative implementation. Rankers must return errors instead of panicking.
Purity and determinism remain implementation requirements, demonstrated for synthetic
fixtures by conformance tests rather than guaranteed for arbitrary implementations.
The receipt records ranker and table versions; view/query identity and persisted
replay provenance are responsibilities of the future engine integration.

Before integrating or enabling any real ranker:

- Supply the engine's authorization/as-of/tombstone checks before the hook. Writer
  equality here is a second guard, not an authorization system.
- Define multi-writer fusion without shared private statistics or tables. Build
  tables per writer, version them, and prove erasure destroys each derived artifact.
- Add expansion, alternate candidates, score-floor behavior and lexical-tail handling
  only with their exactness tests. This foundation deliberately requires dense deltas.
- Establish enforceable time/memory budgets or explicitly trust the ranker. Add
  engine-level disabled/oracle parity and immutable write-path checks.
- Pass the held-out quality and latency adoption gate. This crate provides no evidence
  of improved retrieval relevance, AWS readiness or suitability as primary storage.

Run `cargo test -p oojuro-rank` and `cargo clippy -p oojuro-rank --all-targets -- -D warnings`.
Tests use synthetic IDs and terms only, with no logs or private text.
