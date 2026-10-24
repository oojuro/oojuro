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

## Focused combined qualification

Actual task-local Rust/Cargo1.99.1 tested combined source
`f726bab51d982896a129d4bdf2fd2d3a2edc2fff`. This documentation descendant does
not change production source, manifests or lock. The initial JSON source proof
above records the earlier unbuilt stage; this section and the separate
[qualification receipt](combined-client-qualification.json) record the later run.

- Minimal policy passes; all7 refusal cases pass.
- `cargo test --offline --locked --no-default-features --lib --tests -p oojuro`:
  **96passed,0failed,0ignored**,19test binaries. This selects CLI/client
  consumers under the changed manifest/feature boundary; it does not count
  dependent-library unit suites as executed.
- `cargo build --offline --locked --no-default-features -p oojuro --bin oojuro`
  passes. The actual debug CLI is frozen locally:30,744,544bytes,
  SHA256`dc2d10bf7553da54beb43b585a3d73d7d3c8f0e2f13022896f449541db651b56`.
  It links libc/libm and the system loader; Rust dependencies and standard-codec
  native archives are linked into the binary. The executable is not published.
- Linux normal+build graph: engine49external+3local, Store34+1, rank0+1,
  explicit CLI151+9; all-platform engine62+3. These are distinct
  name/version/source identities, excluding dev dependencies, under separately
  selected roots. Package-set hashes and exact features/commands are archived.
  The policy-only metadata inventory intentionally omits external package detail;
  selected graph entries remain present.
- Warm Cargo artifacts, `-C target-cpu=x86-64`, Zig baseline wrappers, two build
  jobs, owned real mode0700 scratch removed after execution. This functional
  gate has no cold-cache or performance result. Actual rustc binary hash is
  `b53b780f77f952062f8ca8a3b70aef2390b1e45ad91939be94afdc9543026853`.

Raw synthetic/local evidence and the exact runner are in
[evidence-combined-client](evidence-combined-client). No credentials or real
history were used. SDK/setup/license trees are exactly80, so their prior
Node/TypeScript/offline-package/Python passes remain component evidence; those
suites were deliberately not repeated or relabeled as this combined run.
Dependency81's320Rust/12Python passes,12historical ignored cases and one bounded
fresh-artifact build pair remain its separate historical component evidence.
No further algorithm/recovery suite or benchmark was needed for this unchanged
library source. Remote Actions have not run; repository Actions remain disabled.

## Source-only consuming pin decision

Compared with native revision
`d72f879484655c758c474b43b7c38dc81b902c0d`, this composition has **identical**
engine, rank and Store production source trees, plus identical engine/rank
manifests. Exact shared tree identities are:

| Tree | Both revisions |
| --- | --- |
| `crates/oojuro-engine/src` | `90d34ff605f744c0040f9aa4fb2c1e4a2a0e2abf` |
| `crates/oojuro-rank/src` | `d92ba8efbfcdc6e915bd8c23f7bb0a9b9969fd57` |
| `crates/oojuro-store/src` | `8adb4ec9d63d27a1815ed2a3c9e90f46b498c959` |

The relevant library selection changes are only checked, safe block-only LZ4
features and Store reporting JSON behind `bench-tools`. No native data format,
crypto, append/recovery/index/search/ranking/cache algorithm or API is changed.
A consumer can therefore qualify a native-engine pin update separately.

Updating an older legacy client revision such as3a17d1d at the same time is a
broader migration. Legacy core/nine/trace/proxy source trees are unchanged, but
the front-door library now includes hosted clients and reqwest; exact decimal
`@unix` parsing rejects nonfinite, overflowing or inexact microsecond input;
the shared capture redactor additionally masks Oojuro credentials/escaped JSON
boundaries; and local native capture modules are added. Legacy trace uses Store's
run archive API, not `Store::open`: the archive schema/starter and public run
API remain compatible, while bounded dictionary sampling can change newly
compressed bytes. Store also gained opt-in sealed-frame/dictionary formats and
bounded replay/training since that older revision; plaintext defaults remain.
These differences should receive their own consumer compatibility gate.

Keeping an older legacy revision alongside a new native pin retains two Git
Store identities. Cargo features are additive, so the legacy source may enable
LZ4frame/twox-hash on the same registry crate. **The isolated engine49 graph is
not a whole consuming application count or RAM/latency/storage saving claim.**

This topic is a public draft for review. No main merge, binary/package release,
provider change or deployment is authorized. Separate hosted joined checks can
use the frozen CLI; their results are not claimed here.
