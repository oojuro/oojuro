//! A local compressed timeline. No model, vector projection or network client.
//! Text retrieval uses keyword postings and conditional query code length.
use serde::{Deserialize, Serialize};

pub const RETRIEVAL_MODEL: &str = "dirichlet-scope-128-v1";
pub const RETRIEVAL_COUNT_MODEL: &str = "scope-scaled-raw-frequency-v1";
pub const RETRIEVAL_NORMALIZATION: &str = "english-snowball-1.2.0-v1";
pub const PASSAGE_MODEL: &str = "literal-first-snowball-backoff-v1";
pub use text::QUERY_STOP_WORDS;
pub(crate) use text::{literal_query, normalize_word, phrase_start, query_terms, query_words};
mod passage;
pub use passage::{TextSlice, expand_range, extract_passage};
mod context;
pub use context::{ContextPacket, decode_context};
pub mod archive;
mod codec;
pub mod profile;
mod segment;
mod shared;
mod text;
mod v3;
mod v4;
mod wal;
pub use profile::{reset as profile_reset, snapshot as profile_snapshot};
pub use shared::{LogBinding, SharedLog, SharedLogOptions, SharedLogStats};
pub use v4::{Background, Nine, Options, Passage, Receipts, RoutingEntropy};

/// The normalized terms (with counts) the index builds a row's postings
/// from: the engine's own analysis, exposed for harness simulations and
/// probes (the index layout study). Changes nothing in the engine.
pub fn index_terms(text: &str) -> Vec<(String, u32)> {
    segment::analyze(text).map(|a| a.terms).unwrap_or_default()
}

/// The normalized term one query word maps to (lowercase, then stemmed
/// when ASCII alphabetic), as query parsing maps each word.
pub fn query_term(word: &str) -> String {
    normalize_word(&word.to_lowercase()).into_owned()
}

/// Journal `sync_data` calls made so far by this process: private journals
/// (one per committed round of each store) and the shared log (one per
/// round across all its stores). For measuring syncs per commit.
pub fn journal_syncs() -> u64 {
    wal::JOURNAL_SYNCS.load(std::sync::atomic::Ordering::Relaxed)
}
/// The frozen NINET003 engine (main@7b98903): reads v3 stores and serves as
/// the comparison arm in benchmarks. New stores should use [`Nine`].
pub mod legacy {
    pub use crate::v3::V3;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TimelineMetrics {
    pub posted_rows_visited: usize,
    pub candidate_records: usize,
    pub scored_records: usize,
    pub query_terms: usize,
    pub background_query_bits: f64,
    pub best_conditional_query_bits: Option<f64>,
    pub routing_entropy_bits: f64,
    pub eligible_outcomes: usize,
    pub cache_hit: bool,
    /// Matches past the query's identifier-anchored view (see
    /// [`SourceMatch::continued`]); 0 when the list was not continued.
    #[serde(default)]
    pub continued_matches: usize,
    /// Whole-view mass added to a small candidate background (see
    /// [`BACKGROUND_FLOOR_RECORDS`]); 0 when the candidates were enough.
    #[serde(default)]
    pub background_pad: f64,
}

/// The candidate-set background (the default) is estimated from at least
/// this many records of `mu` mass: a smaller candidate set is padded with
/// whole-view text, in proportion to the view's own statistics, up to
/// `BACKGROUND_FLOOR_RECORDS * mu`. Without it a single candidate is its own
/// background and scores about 0 bits however well it matches. Candidate
/// sets at or above the floor are scored exactly as before.
pub const BACKGROUND_FLOOR_RECORDS: f64 = 64.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TimelineStats {
    pub records: usize,
    pub raw_text_bytes: usize,
    pub archive_bytes: u64,
    pub delta_records: usize,
    pub vocabulary: usize,
    pub indexed_postings: usize,
    pub persisted_queries: usize,
    pub cache_entries: usize,
    pub cache_bytes_estimate: usize,
    pub learned_outcomes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceMatch {
    pub id: String,
    pub time: i64,
    pub bytes: usize,
    pub score_bits: f64,
    /// A query naming identifiers known to its historical view ranks only
    /// the records containing them (its anchored view). When that view
    /// holds fewer than k records, the list goes on past it with the query's
    /// unanchored ranking (every identifier read as its component words),
    /// skipping records already listed. Such matches are `continued`: they
    /// follow every anchored match, and their `score_bits` come from the
    /// unanchored ranking, so they are not comparable with the bits of the
    /// anchored matches before them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continued: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Matches {
    pub matches: Vec<SourceMatch>,
    pub routing_entropy_bits: f64,
}

