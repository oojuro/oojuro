# The Nine: compressed text over time

The current Nine is a small, native Rust text database. It stores exact originals
in a compressed append log, indexes words and time, and ranks matching sources
using conditional query code length. It needs no SQL server, GPU, embeddings,
LLM, generated summary or model provider. It rejects supplied embeddings.

Authentication, authorized sharing, backup operations and usage metering are not
supplied by this library; they belong to the application around the engine.

## Small interface

```rust,no_run
use oojuro_core::Result;
use oojuro_nine::Nine;
use std::path::Path;

fn example() -> Result<()> {
    let mut db = Nine::open(Path::new("data/private-agent"))?;
    let id = db.put(100, "Tool output: migration v1 failed; v2 passed the check.")?;
    let found = db.find("migration failed", 0, 200, 50)?;
    // References contain source ID, time, original byte length and ranking score.
    // Expand the selected exact original deliberately:
    assert_eq!(found.matches[0].id, id);
    let original = db.read(&id).unwrap();
    assert!(original.contains("v2 passed"));
    Ok(())
}
```

`range(from, to, limit)` returns originals in time order. `record(id)` returns the
immutable source with its metadata. Result limits are bounded at 10,000; the
query-text limit is 64 KiB. Time values are caller-supplied signed integers; the
CLI uses Unix seconds. Put/read/find require no tables or event taxonomy.

The shared experiment interface also accepts a `Record` with an optional
revision key and two clocks, and a `Query` with valid-time and known-time cutoffs.
It selects the latest eligible revision. `put` uses a unique key for each source,
preserving a simple timeline. Explicit range reads retain all original versions.

`passage(id, query, max_bytes)` optionally selects a bounded **exact substring**
with start/end byte offsets in that original. `extract_passage` provides the same
reader for supplied text. This scan uses local query-feature rarity and coverage;
it generates no summary. Short excerpts can omit required evidence and are not
the default replacement for a full source. See the
[range experiment](../../docs/NINE_PASSAGE_EXPERIMENTS.md) for quote-level checks,
raw-tool timings, scan costs and failures. Source byte offsets differ from the
time interval accepted by `range`.

`expand_range(text, start, end, max_bytes)` expands an already known, valid UTF-8
byte range with nearby original context. It does not perform text retrieval.

## Optional shared context

`ContextPacket` stores identical selected text once when that representation
costs fewer JSON bytes. Every source keeps its own ID, revision key, both clocks,
verification flag and original byte range. Different text is never merged.

```rust,no_run
use oojuro_core::{EvidenceSnippet, Result};
use oojuro_nine::ContextPacket;

fn encode_selected(selected: Vec<EvidenceSnippet>) -> Result<Vec<u8>> {
    let mut packet = ContextPacket::new(50, 16_384)?;
    for source in selected {
        if !packet.push(source)? {
            return Err("selected evidence does not fit the context limits".into());
        }
    }
    packet.to_json()
}
```

The first limit counts distinct text blocks; `origin_count()` counts all source
origins. `decode_context` accepts shared or legacy flat JSON and reconstructs
the exact snippets in their admitted order, including interleaved sources.
Encoded and expanded JSON are each bounded at 32 MiB, with at most 10,000 origins.
Callers supply authorized, source-checked snippets; the codec does not retrieve
evidence or establish its truth. Existing retrieval selection stays unchanged.

An agent must understand the shared representation to use the smaller payload.
Expanding it into flat evidence before sending it to a model restores the
original size. The [measured experiment](../../docs/NINE_SHARED_CONTEXT_EXPERIMENT.md)
reports byte savings and codec costs; model tokens and answer accuracy remain
unmeasured.

## Lossless storage

For each original the encoder compares the actual native frame size of a literal
with text and full-record prefix/suffix deltas against a bounded set of recent originals. Each frame
uses LZ4 only if the compressed bytes are smaller. Delta chains are limited to
four links. A BLAKE3 digest of the complete original record is checked after reconstruction on open;
the journal checks each stored frame with CRC32. Unicode boundaries are checked.

An acknowledged append has synced its frame. The journal has one exclusive
writer, recovers an incomplete final frame and rejects checksum corruption.
These local checks do not prove replication, disaster recovery, power-failure
behavior on every filesystem or a production durability SLA. A native frame is
limited to 32 MiB including its serialized envelope; split larger sources.

Original text and derived indexes are currently memory resident after replay.
Open therefore scales with stored history and indexing work; this is not yet an
engine with a bounded disk buffer pool. There is no retention, vacuum, persistent
index checkpoint, compaction, snapshot export, replication or encryption layer.

## Statistical retrieval

Word queries use an OR candidate set. Time and revision eligibility apply
before statistics and scoring. The index includes ordinary words and complete
underscore identifiers. An exact identifier in a query is a literal when it
occurs in the eligible historical view: its postings anchor the candidate set,
and surrounding words refine scores inside that view. Multiple known identifiers
use the union of their postings. Explicit positive usefulness assessments may
still add a source. Otherwise the query falls back to its
component words. Plain word queries can still find components inside names.
This is deterministic lexical retrieval, with no semantic model.

Ordinary ASCII alphabetic words use fixed English Snowball normalization
(`rust-stemmers` 1.2.0); counts of merged forms are summed. Digits, complete
underscore identifiers and non-ASCII features stay literal after lowercasing.
Apply the same map to query words. This is an English-specific index choice,
not multilingual semantic equivalence. Exact originals remain unchanged.

A nonempty query entirely enclosed in double quotes requests a **case-sensitive
verbatim phrase**. Alphanumeric and underscore edges must be word boundaries,
so `"fn example"` does not match `fn example_extra`. A rare query-term posting
list supplies candidates, then exact original bytes and historical eligibility
are checked. Feedback cannot add a source that violates the phrase. Phrases
without indexed words scan the eligible timeline. `passage` uses the same phrase
constraint and returns no range when the phrase is absent or exceeds the cap.
This is an explicit query mode; Nine does not translate natural language into
declaration queries. See the [shared caller-policy experiment](../../docs/NINE_PHRASE_RETRIEVAL.md).

For the eligible candidate set C, write:

Here `w` denotes the normalized feature class, `tf` sums its original word-form
counts, `n(d)` is the total feature count, and `u(d)` is the distinct feature
count. Word identity is preserved in the original archive. Ordinary queries
separate repetition from scope: repeating an entire source changes its length
and term frequencies together, while leaving its vocabulary and scores intact.
This representation is motivated by the
[Pólya-urn document model](https://arxiv.org/html/1502.00804v2); Nine retains its
own fixed prior and query-addressed historical statistics.

```text
P0(w)   = 2^(-[8 * utf8_bytes(w) + elias_gamma_bits(utf8_bytes(w) + 1)])
df(w)   = number of sources in C containing w
PB(w)   = (df(w) + P0(w)) / (sum(d in C, u(d)) + 1)
a(w,d)  = u(d) * tf(w,d) / n(d)
P(w|d)  = (a(w,d) + 128 * PB(w)) / (u(d) + 128)
L(q|B)  = -sum(w in q, tf(w,q) * log2(PB(w)))
L(q|d)  = -sum(w in q, tf(w,q) * log2(P(w|d)))
score   = L(q|B) - L(q|d) + assessed_usefulness_bits(q,d)
```

The same score is evaluated sparsely, using the identity below with `mu = 128`:

```text
gain(q,d) = sum(w present in d, tf(w,q) * log2(1 + a(w,d)/(mu * PB(w))))
            - length(q) * log2(1 + u(d)/mu)
score     = gain(q,d) + assessed_usefulness_bits(q,d)
```

Empty sources contribute no term evidence and keep their eligible usefulness
prior. Literal phrases, recognized underscore identifiers, direct keys and
empty timeline requests retain the preceding token-mass path: background
counts sum raw frequencies over C, and source smoothing uses `tf` and `n`.
These addressed operations do not adopt ordinary-query repetition invariance.

Only nonzero postings need term-specific logarithms. Candidate eligibility is
checked once per source, background counts reuse the posting pass, and only the
requested result prefix is sorted. Per-record token maps are discarded after
indexing; postings retain the counts and each source retains one scope counter.
A dense-equation regression test checks
the sparse scores across historical views. Exact originals and the archive
format are unchanged; the derived identifier index is rebuilt on open.

The view C is query-addressed, so these are conditional retrieval costs rather
than an unconditional code for an unknown query. The literal prefix code supplies
a subprobability prior; unused mass is escape
mass. Query terms absent from all matching originals are omitted from relative
scoring. Generic English stopwords are removed unless that empties the query.
Candidate and query term iteration is deterministic. An unknown query returns no
arbitrary fallback. Empty text requests the eligible timeline; an exact existing
key or `key:NAME` resolves a key directly.

This is an adaptation of established smoothed query-likelihood retrieval. It is
not a new theorem, a learned semantic encoder, an answer generator or a proof that
compression measures truth. Storage codec selection minimizes measured frame
bytes, a narrow description-length objective; ranking does not perform a global
MDL optimization. Routing entropy is Shannon entropy of weights proportional to
`2^score` across candidates. It measures ranking ambiguity, not factual accuracy.

Time controls eligibility and physical locality. It does not guarantee that
entropy decreases as time passes; new or contradictory evidence can increase
ambiguity. Recent sources break ranking ties, rather than automatically
overriding more relevant older evidence.

For N sources, P visited postings and M matching candidates, ordinary insertion
indexes O(text length) tokens plus O(log N) timeline work. Late revisions can
shift a per-key version vector. Retrieval uses O(N) initialized temporary arrays,
two posting passes, sparse score updates and selection of the top K references;
only that prefix is sorted. It also includes per-key historical version checks
and exact-identifier backoff checks. No constant-time
semantic-search claim is made. Time ranges use O(log N + returned records).

## Reuse and assessed outcomes

A bounded cache retains up to 32 reference results with an 8 MiB estimate cap.
Its identity includes text, both time cutoffs, range, limit and byte budget. Every
source or outcome append invalidates it. Even a cached query syncs a receipt.
Sources remain exact; a cache hit does not automatically make them more correct.

`record_outcome(query, source, useful, at, verifier)` appends a distinct assessment.
For the exact query digest and eligible source, its bounded contribution is
`clamp(log2((positive+1)/(negative+1)), -8, 8)`. Positive assessments can route a
source lacking query words. This does not generalize to unseen paraphrases and
does not change originals, timestamps or verification flags. A verifier name is
provenance supplied by the caller; the host must authenticate and assess it.

Search returns references by default so a large original remains discoverable.
`Engine::search` is a compatibility adapter that returns complete originals under
a text-byte limit and skips originals that do not fit. For a small prompt budget,
use `find` and choose a deliberate expansion policy. The database does not claim
to losslessly turn arbitrary long text into short model context.

## Try it

```text
cargo test -p oojuro-nine
```

The `oojuro` command line ([FRONT_DOOR](../../docs/FRONT_DOOR.md)) is the user-facing way to
store and search; `examples/` holds small probes of this crate. The previous
model/vector/summary prototype is not part of this repository. Old stores are not
silently converted. The earlier negative benchmark remains valid for that version;
performance claims for the replacement require new evidence.

The [sparse scoring and identifier experiment](../../docs/NINE_RETRIEVAL_IMPROVEMENTS.md)
records the measured changes on frozen sources and separate public queries.
The [independent SciFact gate](../../docs/NINE_COUNT_EXPERIMENT.md) records a
separate externally labeled retrieval test and a rejected logarithmic-count
candidate. The compiled `RETRIEVAL_COUNT_MODEL` diagnostic identifies the
count model. The later
[scope-mass experiment](../../docs/NINE_SCOPE_MASS_EXPERIMENT.md) records the
current scope-scaled count model, preserved addressed paths and remaining gaps.

The [normalization and fresh FiQA experiment](../../docs/NINE_LEXICAL_NORMALIZATION.md)
records the current normalization and excerpt policies, both SQLite controls,
remaining quality gaps and replay patches. The reader prioritizes unstemmed
query-word matches and tries normalized matching only if the original pass has
no overlap. New query receipts identify the count and normalization models;
older receipts still load. Derived indexes rebuild on open, and no model call
or stored-original migration is required.
