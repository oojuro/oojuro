# Client/setup and dependency policy: source composition

This isolated topic merges current core80 `c426a324e64e2127194cccb88f41cd655a3c4bf8`
and qualified dependency81 `180271257297306cb97ec0fa307e661316cf1783` at
`2252394f1b7b440a59b8d9268ecf84313314052f`. Both complete histories and DCO
sign-offs are preserved. No textual conflicts occurred. Cargo.lock, CI and
CONTRIBUTING auto-merged; each result was checked deliberately against both
parents. No old experimental performance branch or private hosted code is added.

## Exact preservation proof

- All core80 engine/rank, Store implementation, legacy/capture/proxy/CLI trees,
  current SDK, locked development tooling and capture/project/combined setup
  scripts/tests are byte-identical. Native notices/licenses/publication and
  packaging safeguards are also unchanged.
- Root/Store manifests and policy/checker/refusal-test blobs are exactly81.
- Cargo.lock has the same package identity/version/source set as both parents.
  Relative80, its only change is removal of the unused LZ4 frame→twox-hash edge.
  Relative81, its only change is preservation of80's CLI base640.22.1 edge.
  CLI base64 and serde_json/raw_value declarations remain exactly80.
- Removing the two new dependency-policy steps from composed CI reproduces80's
  workflow exactly, including Rust/runtime/installer/Node/declaration/offline
  package gates and pinned actions/toolchains.

[Machine-readable tree/blob, lock and CI proof](COMBINED_CLIENT_DEPENDENCY_SOURCE.json)
records exact identities. The minimum library graph is preserved by its identical
manifests/source and isolated roots; a combined graph check has not yet run.

## Qualification remains separate

Dependency81's tested source5e150c9 and its320Rust/12Python passes,12historical
ignored cases and fresh-cache build pair remain exact component evidence.
Core80 carries separate client/SDK/reuse/setup qualification. Neither is a test
pass for this new combined tree. No combined build/test, remote push or draft PR
has occurred. No performance result is inferred from composition.

After the root releases CPU, the proposed focused gates are the current minimal
policy/refusal checks, CLI package `cargo test --lib --tests -p oojuro`, all three
protected setup wrapper tests, current Node runtime/declaration checks and
local offline package exports. The JSON proof records exact commands/environment.
They cover consumer compatibility with the changed feature/default-selection
boundary; unchanged engine-library/torn-tail suites and cold benchmarks need no
repeat for this source-only merge. Do not run them during the separate paired
performance window.

No merge, binary/package release, provider change or deployment is authorized.
