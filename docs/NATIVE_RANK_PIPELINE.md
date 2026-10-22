# Native RAM rank pipeline and sealed tables

This opt-in direct RecordLog lane joins the previously isolated oojuro-rank
contract to the actual native RAM Record Index. It does not enable a plugin or
adopt an experimental ranker. The default pipeline is disabled. No model,
training, network or table work occurs in append or ordinary lexical search.

## Query contract

record::Index::search_ranked takes the independently opened log, protected View,
ordinary Query, owning LifecycleGuard and explicit Pipeline. The guard is checked
before and after the entire operation; loaded table bindings are checked before
each callback. A guard error or changed generation refuses delivery. A Binding
struct itself is not authentication, authorization or current authority.

The current View chooses original IDs by prefix, known/valid time, logical key
winner and typed filter before any plugin sees rows. Only initially indexed
eligible originals enter optional read-only Context; stored-only text does not.
Extra IDs must be canonical original decimal IDs from that same eligible slice.
Duplicate, foreign, superseded, time-hidden or stored-only extras reject that hook.

Hooks are explicitly ordered: expand, candidates, then rescore. Each enabled
hook has a stable implementation/table identity, weight and bounded receipt.
Disabled hooks are never invoked. Missing/wrong/stale tables, empty hooks, bad
identities/weights/order, over-budget work, invalid terms/IDs, missing/non-finite
deltas or changing callback identity skip that hook transactionally. Prior valid
hooks remain. Expansion terms are normalized as individual terms, then passed as
bounded analyzed term/count pairs; they never become a reconstructed query
string that could reinterpret quotes, key directives or identifiers.
Expansion/candidate hooks skip literal, direct-key, numeric-ID and empty queries.

Successful expansion/candidate hooks select expanded-pool-v1: the common scorer
uses one canonical query/candidate-pool/F3 model for every candidate in an F2
group. Extra rows never receive a candidate-only recomputed score. Rescore-only
uses original-pool-v1, preserving the original lexical background. The original
RAM score body is shared; all-off ordinary search still passes no optional plan.
No writer text outside the effective view changes this writer's model.

Both policies explicitly retain f2-original-v1: original-query anchors precede
continued rows. Expansion cannot create a new primary anchor. Rescore deltas
never silently move a continued hit ahead of an original anchor. Within groups,
higher final bits, newer valid time and full decimal ID text break ties.
Rescoring sees at most the configured top-N (maximum 1000). The engine retains
that window plus the first requested-k unrescored lexical contenders; farther
unrescored tail rows cannot outrank those k unchanged contenders. It merges the
window with that complete possible final-k tail before truncating. A negative
window delta can therefore reveal an outside-window winner.

With no successful hook, the engine calls the original search_view again and
preserves original IDs, order, continued flags and f64 score bits exactly.
The new result additionally records bounded ordered hook/implementation/table
versions, skips, configured weights, per-hit raw deltas and the policy ID.
Receipts contain no private query, query hash, table bytes or provenance claim.
The new complete result, including receipt overhead, must fit Query.max_bytes.

## Callback and resource boundary

These are trusted in-process Rust implementations. They receive read-only
source/table views, no writer/provider/authority handles, and a cooperative
WorkBudget charged before work. The engine checks declared work before dispatch
and rejects exhausted budgets even if the callback ignores an error. Per-hook
work is capped at 1M units, 8 stages, query terms256/64KiB, extras1000 and rescore
window1000. Existing log/index state bounds still apply. Output/cardinality and
finite arithmetic checks are enforced before applying a hook.

A Rust trait cannot prevent a malicious implementation from using other process
state, making network calls, allocating memory or looping forever. The contract
forbids network, clock, randomness and writes in these callbacks; this is not a
sandbox or hard wall-time guarantee. No shipped plugin has received a quality
adoption verdict. Public starter tables, trained algorithms, full corpus
metrics/performance and plugin adoption require their separate SPEC gates.

## Explicit derived-table lifecycle

rank_tables::build is explicit caller/background work. It receives only original
selected indexed rows, a bounded Writer and cooperative budget. Search never
builds or updates tables. rank_tables::load opens existing files only and creates
no directory, lock or table. Missing/corrupt/stale tables are optional-query skips.

One RANK_TABLE registry and LOCK live under the owning lifecycle's supplied
generation-derived directory; temporary names contain random opaque IDs only.
At most8 tables,2MiB payload/table and 16MiB complete serialized registry are
accepted. The registry binds format, actual log origin/identity, exact
authenticated prefix witness, AsOf/filter, supplied authenticated opaque lifecycle
binding and implementation/table labels. Each explicit table build also receives
a stored random opaque content-version ID; query receipts use that same ID across
loads, and a rebuild receives a new version even when an implementation label is
reused. The ID exposes no low-entropy content hash. RecordLog's supplied FrameCipher
seals all metadata/payload bytes in a separate native-rank-tables/v1 AAD domain
before any write. No raw key getter or new key authority exists. No low-entropy
public text/table hash is used as a filename or plaintext sidecar.

Publication is private temp create/write/fsync, guard recheck, atomic rename,
directory fsync and guard recheck. Only the registry selects table versions.
Explicit rebuild may replace disposable missing/corrupt/old-generation data
after independently validating the supplied log/view/guard. remove_all removes
only registered regular files, refuses unknown/symlink entries and never
recursively follows arbitrary paths. Owner generation retirement must include
RANK_TABLE/LOCK/tmp-64lowerhex.bin before reporting local pruning complete.

This first implementation uses actual direct RecordLog sealing and prefix
verification. Fresh managed-v3 is a different format with no RecordLog backing.
Its required with_rank_view adapter, held independent current-control guard,
selected-original projection and actual managed data-DEK seal/open capability
are separate code being composed with that owner. Supplying an arbitrary Binding
does not complete that join. No managed-v3 erasure or old-backup crypto-erasure
acceptance is claimed by this direct lane. Plaintext exists in RAM; retained old
keys/copied ciphertext outside the guarded API are not made unreadable by a
logical cache removal.

## Explicit native context command

record-context is opt-in caller work, not automatic turn injection. See
[NATIVE_RECORD_CONTEXT.md](NATIVE_RECORD_CONTEXT.md) for whole-record packing,
declared trusted token measurement, provenance and echo/time exclusion.

~~~sh
oojuro-native record-context --root /private/record-root --query "topic"   --measure chars4-estimate --turn-start-us 1792627200000000   --tokens 2500 --context-bytes 65536 --top-k 20 --bytes 131072
~~~

The CLI requires an explicit chars4-estimate selection, labels its Unicode
characters/4 measure as an estimate and emits hard_provider_token_cap:false.
The command clamps valid_at and known_at to strictly before the explicit turn
start BEFORE key-winner selection and scoring. Later corrections cannot hide
a prior-turn key version or enter its F3 background. The complete actual JSON
context is measured; complete output UTF-8 bytes are
always bounded. This estimate is not a provider tokenizer or hard token limit.
The library accepts caller-supplied declared exact/conservative measures.
--format claude-hook explicitly renders a UserPromptSubmit additionalContext
envelope; it installs no hook, changes no injection setting and invokes no model.
Legacy hook/storage selection remains unchanged.

## Verification

Current final-source gate passed **75 active checks**: 8 pure ranker conformance,
45 Record (including 7 rank/table and 11 context cases), 21 shared lexical
and one actual native record-context binary story. Three existing engine
tests were ignored.
The native binary story covers explicit measurement refusal, whole original
content, untrusted-data labeling, estimate disclosure, hook envelope and byte
refusal. Additional tests cover all-off/all-skipped exact IDs/bits/F2, malformed/
non-finite/budget failures, view/key/stored-only extra refusal, outside-window
winners, shared expanded-model bit equivalence, sealed table privacy, stale
versions/views/generations, mid-query fencing, corruption/rebuild and registered
deletion refusal. Ordinary scoped engine/ranker and native-CLI lint has zero owned diagnostics
(one existing engine-dependency and 32 existing CLI/dependency diagnostics
remain elsewhere). No full workspace/corpus/scale, paged rank integration,
live provider, hosted availability or adopted retrieval-gain claim is made.

The F2 regression failed semantically when the previous optional anchor rule was
restored and passed again with the original-anchor fix. The actual CLI
historical-key case failed before moving the cutoff into query visibility and
passed after the correction. Neither red run was a compile failure. The
distinct-key table-refusal case was checked separately after the full gate.
These are local synthetic correctness results, not performance measurements.
