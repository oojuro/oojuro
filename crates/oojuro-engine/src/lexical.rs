//! Native bounded in-memory lexical port. CaptureLog is its sole authority.
//! Arithmetic and query semantics come from nine text.rs/segment.rs/v4.rs and
//! trace's EventIn mapping. There is no oracle runtime or second WAL.
//! The opt-in `segments` module adds a bounded disposable disk cache.
//! Retention/erasure, learned outcomes and launch scale are
//! deliberately outside this slice. Each index is bound to one open writer log.
mod args;
mod memory;
pub(crate) mod paged;
pub(crate) mod passage;
pub(crate) mod prepared;
pub(crate) mod record_cache;
pub mod segments;
mod text;
use crate::capture_log::{CaptureLog, Event, ParsedBatch};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use text::{literal_probability, literal_query, normalize_word, phrase_start, query_terms};

const MU: f64 = 128.0;
const FLOOR_RECORDS: f64 = 64.0;
pub const MAX_QUERY_BYTES: usize = 65_536;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Authority,
}
pub type Result<T> = std::result::Result<T, Error>;
/// Every query explicitly names its owning writer and agent. This is scope
/// binding, not a hosted authentication service. Filters apply after scoring,
/// as trace::search_where does; time affects statistics and identifier anchoring.
pub struct Query<'a> {
    pub writer: &'a str,
    pub agent: u32,
    pub text: &'a str,
    pub from: i64,
    pub to: i64,
    pub k: usize,
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub id: u64,
    pub bits: f64,
    pub continued: bool,
    pub time: i64,
    /// Original event ID of the indexed host, not a raw block or Store sequence.
    pub row: u64,
}
struct Document {
    event: Event,
    length: u32,
    scope: u32,
    normalized_args: String,
}
/// Private new-record visibility. Legacy capture scoring always passes None.
pub(crate) struct RecordVisibility<'a> {
    pub ids: &'a BTreeSet<u64>,
    pub keys: &'a BTreeMap<String, u64>,
}
/// Derived plaintext exists in process memory only. Dropping/rebuilding it does
/// not mutate authority, policy, source cursors, IDs or original bytes.
pub struct Index {
    writer: String,
    agent: u32,
    authority: u64,
    applied: usize,
    failed: bool,
    documents: Vec<Document>,
    // Both authorities cap rows at 100k; checked u32 handles avoid tuple padding.
    postings: BTreeMap<String, Vec<(u32, u32)>>,
    // At most 100k events: below trace's 2^17 half-window rotation threshold.
    seen: HashMap<[u8; 16], usize>,
}
impl Index {
    pub fn rebuild(log: &CaptureLog) -> Result<Self> {
        let identity = log.identity();
        let mut index = Self {
            writer: identity.writer.clone(),
            agent: identity.agent,
            authority: log.instance(),
            applied: 0,
            failed: false,
            documents: vec![],
            postings: BTreeMap::new(),
            seen: HashMap::new(),
        };
        index.refresh(log)?;
        Ok(index)
    }
    /// Only committed batches of this exact open authority can be projected.
    /// A failure invalidates the projection; reopen/rebuild before serving.
    pub fn refresh(&mut self, log: &CaptureLog) -> Result<()> {
        if self.failed
            || self.authority != log.instance()
            || self.writer != log.identity().writer
            || self.agent != log.identity().agent
        {
            return Err(Error::Authority);
        }
        let batches = log.batches().map_err(|_| Error::Unavailable)?;
        if self.applied > batches.len() {
            return Err(Error::Authority);
        }
        for b in &batches[self.applied..] {
            if let Err(error) = self.apply(b) {
                self.failed = true;
                return Err(error);
            }
        }
        self.applied = batches.len();
        Ok(())
    }
    pub(crate) fn from_record_rows<'a>(
        writer: &str,
        owner: u32,
        rows: impl Iterator<Item = (u64, i64, &'a str, bool)>,
    ) -> Result<Self> {
        Self::record_rows_mode(writer, owner, rows, true)
    }
    pub(crate) fn record_rows_mode<'a>(
        writer: &str,
        owner: u32,
        rows: impl Iterator<Item = (u64, i64, &'a str, bool)>,
        tokenize: bool,
    ) -> Result<Self> {
        let mut index = Self {
            writer: writer.into(),
            agent: owner,
            authority: 0,
            applied: 0,
            failed: false,
            documents: vec![],
            postings: BTreeMap::new(),
            seen: HashMap::new(),
        };
        index.append_record_rows(rows, tokenize)?;
        Ok(index)
    }
    pub(crate) fn append_record_rows<'a>(
        &mut self,
        rows: impl Iterator<Item = (u64, i64, &'a str, bool)>,
        tokenize: bool,
    ) -> Result<()> {
        if self.authority != 0 || self.failed {
            return Err(Error::Authority);
        }
        // Temporary build work only: never retain corpus vocabulary after return.
        let mut stems = HashMap::new();
        for (id, valid_at, text, indexed) in rows {
            if self.documents.last().is_some_and(|old| old.event.id >= id) {
                return Err(Error::Authority);
            }
            let terms = if tokenize && indexed && !text.is_empty() {
                analyze_with_stems(text, &mut stems)?
            } else {
                vec![]
            };
            let length = u32::try_from(terms.iter().map(|(_, n)| u64::from(*n)).sum::<u64>())
                .map_err(|_| Error::Invalid)?;
            let scope = u32::try_from(terms.len()).map_err(|_| Error::Invalid)?;
            let row = self.documents.len();
            let posting_row = u32::try_from(row).map_err(|_| Error::Invalid)?;
            for (term, tf) in terms {
                self.postings
                    .entry(term)
                    .or_default()
                    .push((posting_row, tf));
            }
            // Internal lexical carrier only. Never a fabricated capture receipt:
            // no harness identity/policy/provenance is exported from this path.
            let event = Event {
                id,
                session: 0,
                parent_session: 0,
                agent: self.agent,
                turn: 0,
                seq: 0,
                parent: 0,
                revises: 0,
                kind: 5,
                status: 0,
                exit_code: None,
                tokens_in: 0,
                tokens_out: 0,
                ts_us: valid_at,
                duration_us: 0,
                tool: String::new(),
                args: String::new(),
                text: text.into(),
                indexed,
                line_start: 0,
                line_end: 0,
                emission: 0,
            };
            self.documents.push(Document {
                event,
                length,
                scope,
                normalized_args: String::new(),
            });
        }
        Ok(())
    }
    pub fn applied_batches(&self) -> usize {
        self.applied
    }
    pub(crate) fn record_text(&self, row: usize) -> Option<&str> {
        self.documents.get(row).map(|d| d.event.text.as_str())
    }
    fn apply(&mut self, batch: &ParsedBatch) -> Result<()> {
        self.apply_mode(batch, true)
    }
    // Cache hydration shares the exact canonical host/dedupe policy, without tokenization.
    fn apply_mode(&mut self, batch: &ParsedBatch, tokenize: bool) -> Result<()> {
        for event in &batch.events {
            if event.agent != self.agent || event.revises != 0 {
                return Err(Error::Authority);
            }
            let indexed = event.indexed && !event.text.is_empty();
            let norm = args::normalize(&event.args).unwrap_or_else(|| event.args.clone());
            let class = indexed == (matches!(event.kind, 1 | 3 | 4 | 5) && !event.text.is_empty());
            let key = class.then(|| dedupe_key(indexed, &event.tool, &norm, &event.text));
            if let Some(host) = key.and_then(|key| self.seen.get(&key).copied()) {
                let old = &self.documents[host];
                // Do not accept a truncated-digest collision as duplicate text.
                if old.event.tool != event.tool
                    || old.normalized_args != norm
                    || old.event.text != event.text
                    || (old.event.indexed && !old.event.text.is_empty()) != indexed
                {
                    return Err(Error::Authority);
                }
                continue;
            }
            let terms = if indexed && tokenize {
                analyze(&event.text)?
            } else {
                vec![]
            };
            let length = terms.iter().map(|(_, n)| u64::from(*n)).sum::<u64>();
            let length = u32::try_from(length).map_err(|_| Error::Invalid)?;
            let scope = u32::try_from(terms.len()).map_err(|_| Error::Invalid)?;
            let row = self.documents.len();
            let posting_row = u32::try_from(row).map_err(|_| Error::Invalid)?;
            for (term, count) in terms {
                self.postings
                    .entry(term)
                    .or_default()
                    .push((posting_row, count));
            }
            self.documents.push(Document {
                event: event.clone(),
                length,
                scope,
                normalized_args: norm,
            });
            if let Some(key) = key {
                self.seen.insert(key, row);
            }
        }
        Ok(())
    }
    pub fn search(&self, q: &Query<'_>) -> Result<Vec<Hit>> {
        self.search_where(q, &mut |_| true)
    }
    pub fn search_where(
        &self,
        q: &Query<'_>,
        keep: &mut dyn FnMut(&Event) -> bool,
    ) -> Result<Vec<Hit>> {
        self.search_mode(q, keep, None, None)
    }
    pub(crate) fn search_record_view(
        &self,
        q: &Query<'_>,
        view: &RecordVisibility<'_>,
    ) -> Result<Vec<Hit>> {
        self.search_mode(q, &mut |_| true, Some(view), None)
    }
    pub(crate) fn search_projected(
        &self,
        q: &Query<'_>,
        view: &RecordVisibility<'_>,
        ids: &[String],
    ) -> Result<Vec<Hit>> {
        if ids.len() != self.documents.len() {
            return Err(Error::Authority);
        }
        self.search_mode(q, &mut |_| true, Some(view), Some(ids))
    }
    fn search_mode(
        &self,
        q: &Query<'_>,
        keep: &mut dyn FnMut(&Event) -> bool,
        view: Option<&RecordVisibility<'_>>,
        original_ids: Option<&[String]>,
    ) -> Result<Vec<Hit>> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if q.writer != self.writer || q.agent != self.agent {
            return Err(Error::Authority);
        }
        if q.text.len() > MAX_QUERY_BYTES || q.k > 10_000 {
            return Err(Error::Invalid);
        }
        if q.k == 0 || q.from > q.to {
            return Ok(vec![]);
        }
        let mut want = q.k;
        loop {
            let ranked = if let Some(view) = view {
                self.continued_view(q, want, true, Some(view), original_ids)
            } else {
                self.continued(q, want, true)
            };
            let got = ranked.len();
            let mut out = Vec::new();
            for (row, hit) in ranked {
                if out.len() == q.k {
                    break;
                }
                if keep(&self.documents[row].event) {
                    out.push(hit);
                }
            }
            if out.len() >= q.k || got < want {
                return Ok(out);
            }
            want = want.saturating_mul(4);
        }
    }
    fn visible(&self, row: usize, q: &Query<'_>, view: Option<&RecordVisibility<'_>>) -> bool {
        let event = &self.documents[row].event;
        event.ts_us >= q.from
            && event.ts_us <= q.to
            && view.is_none_or(|v| v.ids.contains(&event.id))
    }
    fn continued(&self, q: &Query<'_>, k: usize, prune: bool) -> Vec<(usize, Hit)> {
        self.continued_view(q, k, prune, None, None)
    }
    fn continued_view(
        &self,
        q: &Query<'_>,
        k: usize,
        prune: bool,
        view: Option<&RecordVisibility<'_>>,
        original_ids: Option<&[String]>,
    ) -> Vec<(usize, Hit)> {
        let (mut first, anchored) = self.rank(q, k, false, prune, view, original_ids);
        if anchored && first.len() < k {
            let (rest, _) = self.rank(q, k, true, prune, view, original_ids);
            let listed: BTreeSet<usize> = first.iter().map(|(row, _)| *row).collect();
            for (row, mut hit) in rest {
                if first.len() == k {
                    break;
                }
                if !listed.contains(&row) {
                    hit.continued = true;
                    first.push((row, hit));
                }
            }
        }
        first
    }
    fn rank(
        &self,
        q: &Query<'_>,
        k: usize,
        split: bool,
        prune: bool,
        view: Option<&RecordVisibility<'_>>,
        original_ids: Option<&[String]>,
    ) -> (Vec<(usize, Hit)>, bool) {
        prepared::rank(
            self,
            q,
            k,
            prepared::Options {
                split,
                prune,
                view,
                original_ids,
                plan: None,
            },
        )
    }
}
#[cfg(test)]
thread_local! { static ANALYSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn analyze(text: &str) -> Result<Vec<(String, u32)>> {
    analyze_with_stems(text, &mut HashMap::new())
}
pub(crate) fn analyze_with_stems(
    text: &str,
    stems: &mut HashMap<String, String>,
) -> Result<Vec<(String, u32)>> {
    #[cfg(test)]
    ANALYSES.with(|n| n.set(n.get() + 1));
    let mut raw = HashMap::<std::borrow::Cow<'_, str>, usize>::new();
    text::for_each_retrieval_term(text, |token| {
        let token = if token
            .bytes()
            .all(|c| c.is_ascii() && !c.is_ascii_uppercase())
        {
            std::borrow::Cow::Borrowed(token)
        } else {
            std::borrow::Cow::Owned(token.to_lowercase())
        };
        *raw.entry(token).or_default() += 1;
    });
    let mut terms = BTreeMap::<String, usize>::new();
    for (term, count) in raw {
        let normalized = if term.len() <= 256 && term.bytes().all(|c| c.is_ascii_alphabetic()) {
            if let Some(stem) = stems.get(term.as_ref()) {
                stem.clone()
            } else {
                let stem = normalize_word(&term).into_owned();
                if stems.len() < 65536 {
                    stems.insert(term.into_owned(), stem.clone());
                }
                stem
            }
        } else {
            normalize_word(&term).into_owned()
        };
        *terms.entry(normalized).or_default() += count;
    }
    terms
        .into_iter()
        .map(|(t, c)| Ok((t, u32::try_from(c).map_err(|_| Error::Invalid)?)))
        .collect()
}
fn dedupe_key(indexed: bool, tool: &str, norm: &str, text: &str) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(&[indexed as u8]);
    h.update(&(tool.len() as u64).to_le_bytes());
    h.update(tool.as_bytes());
    h.update(&(norm.len() as u64).to_le_bytes());
    h.update(norm.as_bytes());
    h.update(text.as_bytes());
    h.finalize().as_bytes()[..16].try_into().unwrap()
}
// Exact conservative bound and seed selection ported from nine v4.rs.
fn log2_upper(y: f64) -> f64 {
    let bits = y.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    e as f64 + (m - 1.0) + 0.0862
}
fn best_indices(values: &[f64], m: usize) -> Vec<usize> {
    if m >= values.len() {
        return (0..values.len()).collect();
    }
    let mut ix: Vec<_> = (0..values.len()).collect();
    ix.select_nth_unstable_by(m - 1, |a, b| values[*b].total_cmp(&values[*a]));
    ix.truncate(m);
    ix
}
#[cfg(test)]
#[path = "lexical/tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn analysis_count() -> usize {
    ANALYSES.with(|n| n.get())
}
