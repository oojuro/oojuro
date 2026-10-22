# Managed native RAM rank pipeline

This opt-in library slice joins the existing shared RAM ranker pipeline and
sealed table lifecycle to actual managed-v3 authority. It does not add a managed
CLI/daemon, enable a plugin, train a model, adopt a ranker or change the paged
scorer. Optional rankers remain disabled by default.

## Authentic current view

Call ManagedLog::with_rank_view with the enrolled writer/owner, AsOf and Filter.
Only that owner constructs ManagedRankView. The callback holds current control
and independently retained-witness checks through projection and delivery.
ManagedRankView::search_ranked takes query text, k, byte bound and an explicit
Pipeline; scope/time/filter come from the authentic view, not caller claims.

The managed view shares the ordinary key-winner kernel. All versions participate
in time/winner selection before lower-time, filter or indexed exclusion, so a
newer stored-only or filtered winner never revives an older matching version.
The private lexical index retains all surviving original versions, and logical
key lookup uses genuine latest time-visible winners before lower-time or filter
exclusion. An excluded real key therefore prevents an implicit-key query from
falling back to unrelated visible content. The effective scoring/hydration mask
still uses the selected view, including stored-only winners for existing empty
timeline and direct-key semantics. Hidden index/locator rows never enter plugin
context or table builds; those receive only the public indexed eligible slice. Empty genuine views retain the enrolled
scope even when there is no first row from which to infer an identity.

Both direct and managed wrappers call the same post-visibility RAM hook/scoring
body and common canonical lexical scorer. The direct fallback still calls its
existing Index::search_view. Managed fallback calls the same lexical scorer over
the authentic selected originals. All-off/all-skipped behavior, F2 grouping,
expanded/common F3 model, tail merge and bounded receipts follow
[NATIVE_RANK_PIPELINE.md](NATIVE_RANK_PIPELINE.md). No alternate copied ranker
algorithm or fake RecordLog/Watermark appears in this join.

The managed result contains actual managed Binding, receipt and original hits.
That binding preserves stable origin, identity, generation, physical digest,
logical receipt head and retained control floor. It is a real managed result,
not a converted legacy prefix witness. Whole output including the binding and
receipt must fit the supplied byte limit.

## Every callback and table use is fenced

ManagedRankView::validate_current checks the held owner's independently retained
witness and current data authority. Each optional callback validates its actual
table/view binding immediately before dispatch and afterward. The rescore
adapter checks again at the actual rescore_context boundary, including after
intermediate trait identity/work declarations. A change during callback one
prevents callback two from receiving old context/table data and refuses final
delivery. Errors do not become a new authority or a successful empty result.

These remain trusted in-process cooperative Rust callbacks. A callback can hold
the owning process or independently retain plaintext it already received; the
API is not hard preemption, network isolation or retroactive memory revocation.

## Actual managed data-DEK tables

rank_tables::managed::{build,load,remove_all} require the genuine held view.
Build uses view.records(), a bounded Writer and WorkBudget. The same registered
file lock/read/temp/fsync/rename/reap/remove helpers serve both authority lanes.
Managed metadata contains exact actual Binding plus the private-keyed stamp of
binding/time/filter and stable implementation/table labels. Random stored
content versions identify builds without publishing a low-entropy table hash.

The complete managed registry is sealed/opened through the actual managed
data-DEK capability under purpose rank-tables-v1. Scope, generation, current
control, time and filter are part of the view's authenticated encryption AAD.
There is no key getter, control-key cache encryption, public plaintext sidecar,
caller-provided seal callback or fake legacy binding. Copying ciphertext to
another key/identity/view does not authenticate it.

Only explicit build creates a generation-derived directory and LOCK.
Missing table loads and ordinary queries create neither. Registry, file and
work bounds are shared with the direct table implementation. Rebuilding a
missing/corrupt/stale disposable table is explicit; query never silently rebuilds.
A loaded table retained across append, prune, scope/filter change or generation
advance is not usable in a new view. Wrong legacy/managed table variants refuse
cross-lane use.

The existing managed lifecycle registry owns RANK_TABLE, LOCK and opaque
tmp-64lowerhex.bin. Verified pruning retires prior-generation derived directories
before a local-live-pruned receipt. Explicit remove refuses foreign or symlink
entries; it never recursively follows arbitrary paths. Writer denial fences
new queries and table use even if a caller still holds a Loaded object.

This is logical current-authority refusal and owned-file removal. It does not
erase plaintext already returned or copied elsewhere, destroy retained local
keys, prove freed-block absence or make old ciphertext backups undecryptable.

## Verification

Actual synthetic managed pipeline/table coverage passes all eight new cases.
The final focused gate passes **16 ranking checks** (eight managed and eight
direct), with zero diagnostics in changed paths under package lib/tests lint.
The prior warm composed gate also passed eight pure ranker conformance checks
and 21 shared lexical checks (two pre-existing lexical ignores).

An earlier broader Record pass completed 98 checks and five pre-existing
ignores, with one new assertion failure: shared reported metadata correctly
prevented the stronger local-live-absence claim. The assertion was corrected
to the actual required prior-generation derived-directory retirement, then
passed in the focused suite. No lifecycle implementation was weakened.

The stale-loaded regression has a semantic RED/GREEN: temporarily removing
the exact managed binding/stamp validation let the retained pre-append table
be accepted rather than skipped; the assertion failed without a compile error.
Restoring validation passed the same case and all 15 ranking checks at that checkpoint. Another
actual joined case modifies the retained witness during callback one and proves
callback two is never invoked and final output is refused.

Tests compare managed all-off/all-skipped original IDs/order/f64 score bits/F2
with an independently opened direct RecordLog lexical reader, including empty
and direct-key stored-only behavior. They exercise actual managed expand/
candidate/rescore stages, exact common-model score equivalence, original IDs/
ingest times, authentic Binding-only output, no query-created directory,
encrypted reopen, distinct data key/time/filter refusal, retained Loaded across
append/prune/denial, corruption/explicit rebuild and registered remove refusal.

The final source preserves the original direct disabled branch without cloning
all visible log records. There is no full cumulative candidate, managed CLI,
paged rank integration, large-corpus/scale, quality adoption or live deployment
verdict in this slice.


Independent review found an implicit-key lookup parity edge after the first
freeze. Its actual RED compared a real direct reader returning no hits with the
managed reader incorrectly returning a visible text record when the real key
winner was filtered out. The authentic view now provides full original lexical
documents and pre-filter latest key winners; the wrapper uses both without
giving hidden rows to plugins. Agent-filter and lower-time exclusions now pass
against the independent direct baseline. Final follow-up: all 16 ranking and
21 lexical checks pass, with zero diagnostics in changed paths. This correction
requires the corresponding authentic-view capability follow-up; it is not a
claimed key reconstructed from caller data.
