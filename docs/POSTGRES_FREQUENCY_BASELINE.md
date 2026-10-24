# Explicit PostgreSQL frequency-ranked comparison

Source only; not compiled or measured yet.

`postgres` retains its existing English GIN and `ts_rank_cd` cover-density ranking. The new explicitly selected `postgres-frequency` arm uses PostgreSQL's standard `ts_rank` frequency ranking. Both require the existing optional `postgres-arm` feature on Linux, stay out of the default arm list and use the same owned server, indexes, original bytes, query terms, revision visibility, transaction durability and resource lifecycle. The chosen ranker appears in arm identity and server settings. No new dependency or engine/default feature change is introduced.

This is a different baseline, not a behavior-preserving optimization. Its scores and ordering can differ. A completed comparison must report relevance alongside latency, full returned originals, append/durability, total server memory and complete cluster storage. The original incomplete cover-density comparison remains evidence; no successful repetition, timeout extension or workload reduction is implied.

The reason to evaluate it is a separately fixed nine-case diagnostic on a copied PostgreSQL17.11 store. For the same three queries, candidate counting took75.941/71.593/77.218ms, cover-density search4389.801/6317.867/5674.434ms, and frequency search368.317/437.170/424.713ms. All nine statements completed, with current original inventories unchanged and all owned processes/cgroups gone. This is diagnostic evidence with per-node timing and differing cache/order effects, not a full benchmark or ranking-quality result.

The focused installed-server test exercises the same exact-originals, two-clock latest revision, deterministic ties, atomic failed batch and clean reopen story for each arm. It additionally asks the actual server for both ranking functions, verifies the selected output scores exactly and requires the fixture to distinguish the two functions. These tests are written but unrun. The existing crash/copy tests cover the unchanged storage lifecycle; new qualification will identify its actual source/artifact and executed checks before any comparison claims.

Next: fresh optional build and focused checks under the pinned PostgreSQL runtime, then a predeclared full same-work comparison preserving all201 timed queries and52 labelled rankings per run. No ranker adoption, main merge, release or deployment is authorized by these source changes.
