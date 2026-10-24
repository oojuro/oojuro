# Continuing journal and combined setup source composition

Status: source prepared; not newly tested, released or deployed.

This public topic starts from qualified client79 head `6704a35` and locally merges
qualified setup78 head `ef0d33627f361279d0e72e1d9b0c55502d3a4591`.
The merge was automatic (`ac49dd2`). All Rust crates, SDK and Cargo manifests/lock
are Git-object-identical to client79. CI still declares Rust1.99.1; neither source
has a rust-toolchain file. Its frozen actual CLI artifact remains SHA
`d2a6b7adcb36d6d3541eb0df986ad6133f2bb9b26e230ff56bd6aa72fdb1bd82`,
qualified from runtime source `da76c40`.

The capture/project helpers remain exact client79 files; the setup wrapper and
its boundary tests remain exact setup78 files. The CI merge adds only the existing
`python3 scripts/test-setup-agent.py` command to the existing unprivileged Python
installer step. It adds no flags, features, runtime dependencies, permissions,
Actions administration, package publication, provider or deployment operation.
`REUSE_SETUP_SOURCE_COMPOSITION.json` records the exact source trees and helper
hashes. Frozen earlier evidence files are unchanged.

Fresh capture still creates a new journal for each new provider session. Existing
installation policy is retained. Explicit opt-in reuse and restoration of defaults
use the already implemented client/helper choices; capture must restart to load
local policy changes while retaining the same spool. Pending/resumed targets do
not move. Project reads keep their separate protected grant and current attached
history scope. The wrapper copies no history, attaches no project references and
creates no additional authority.

## Qualification boundary

Client79's 22 focused Rust, 114 Node, 2 installer, strict declaration and offline
export checks are component evidence for the unchanged runtime. Setup78's
3 Python boundary tests and the separately fixed original-wrapper HTTP/PG journey
are earlier component evidence. They are not a fresh execution of this composition.
A new fixed wrapper journey must pin this scripts tree (newer capture helper),
the existing setup script and the frozen d2a6 client, while preserving the old
entrypoint's source/pin meaning. No core engine/SDK rebuild or broad suite repeat
is proposed. Agent registration can remain an isolated task shim and must not be
reported as a real client/model tool invocation.

No private hosted topic is merged here. Reuse, wrapper, provider/dependency and
admission candidates remain independently reviewable until their explicit hosted
composition is qualified. This remains a draft topic; main merge, release and
production activation require owner approval.
