//! Native bounded in-memory lexical port. CaptureLog is its sole authority.
//! Arithmetic and query semantics come from nine text.rs/segment.rs/v4.rs and
//! trace's EventIn mapping. There is no oracle runtime or second WAL.
//! The opt-in `segments` module adds a bounded disposable disk cache.
//! Retention/erasure, learned outcomes and launch scale are
//! deliberately outside this slice. Each index is bound to one open writer log.
mod args;
pub(crate) mod paged;
pub(crate) mod record_cache;
pub mod segments;
mod text;
use crate::capture_log::{CaptureLog, Event, ParsedBatch};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use text::{
    literal_probability, literal_query, normalize_word, phrase_start, query_terms, retrieval_terms,
};

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
    postings: BTreeMap<String, Vec<(usize, u32)>>,
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
        for (id, valid_at, text, indexed) in rows {
            if self.documents.last().is_some_and(|old| old.event.id >= id) {
                return Err(Error::Authority);
            }
            let terms = if tokenize && indexed && !text.is_empty() {
                analyze(text)?
            } else {
                vec![]
            };
            let length = u32::try_from(terms.iter().map(|(_, n)| u64::from(*n)).sum::<u64>())
                .map_err(|_| Error::Invalid)?;
            let scope = u32::try_from(terms.len()).map_err(|_| Error::Invalid)?;
            let row = self.documents.len();
            for (term, tf) in terms {
                self.postings.entry(term).or_default().push((row, tf));
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
            for (term, count) in terms {
                self.postings.entry(term).or_default().push((row, count));
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
        self.search_mode(q, keep, None)
    }
    pub(crate) fn search_record_view(
        &self,
        q: &Query<'_>,
        view: &RecordVisibility<'_>,
    ) -> Result<Vec<Hit>> {
        self.search_mode(q, &mut |_| true, Some(view))
    }
    fn search_mode(
        &self,
        q: &Query<'_>,
        keep: &mut dyn FnMut(&Event) -> bool,
        view: Option<&RecordVisibility<'_>>,
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
                self.continued_view(q, want, true, Some(view))
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
        self.continued_view(q, k, prune, None)
    }
    fn continued_view(
        &self,
        q: &Query<'_>,
        k: usize,
        prune: bool,
        view: Option<&RecordVisibility<'_>>,
    ) -> Vec<(usize, Hit)> {
        let (mut first, anchored) = self.rank(q, k, false, prune, view);
        if anchored && first.len() < k {
            let (rest, _) = self.rank(q, k, true, prune, view);
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
    ) -> (Vec<(usize, Hit)>, bool) {
        let literal = literal_query(q.text);
        let mut query = query_terms(q.text);
        let mut backoff = Vec::new();
        for (term, count) in &query {
            if term.contains('_')
                && (split
                    || !self.postings.get(term).is_some_and(|list| {
                        list.iter().any(|(row, _)| self.visible(*row, q, view))
                    }))
            {
                backoff.push((term.clone(), *count));
            }
        }
        for (term, count) in backoff {
            query.remove(&term);
            for (part, n) in query_terms(&term.replace('_', " ")) {
                *query.entry(part).or_default() += n * count;
            }
        }
        // Trace records have key = decimal event ID, recorded_at = valid_at.
        let key_row = |key: &str| {
            if let Some(view) = view {
                return view
                    .keys
                    .get(key)
                    .and_then(|id| self.documents.iter().position(|d| d.event.id == *id));
            }
            self.documents
                .iter()
                .position(|d| d.event.id.to_string() == key && d.event.ts_us <= q.to)
        };
        let direct = literal
            .is_none()
            .then(|| {
                q.text
                    .strip_prefix("key:")
                    .map(key_row)
                    .or_else(|| key_row(q.text).map(Some))
            })
            .flatten();
        let anchored = direct.is_none() && query.keys().any(|t| t.contains('_'));
        let scope_model = direct.is_none() && !anchored && literal.is_none() && !q.text.is_empty();
        let scan_time = q.text.is_empty() || (literal.is_some() && query.is_empty());
        let terms: Vec<_> = query.iter().map(|(term, n)| (term, *n)).collect();
        let eligible = |row| {
            self.visible(row, q, view)
                // Record-only correction: a nonword literal has no postings
                // and scans text, but must still honor initial eligibility.
                // Empty timelines and actual-key reads retain stored-only rows.
                && (view.is_none()
                    || direct.is_some()
                    || q.text.is_empty()
                    || self.documents[row].event.indexed)
                && literal.is_none_or(|phrase| {
                    phrase_start(&self.documents[row].event.text, phrase).is_some()
                })
        };
        let mut candidates = BTreeSet::new();
        if let Some(row) = direct {
            if let Some(row) = row.filter(|row| eligible(*row)) {
                candidates.insert(row);
            }
        } else if scan_time {
            candidates.extend((0..self.documents.len()).filter(|row| eligible(*row)));
        } else {
            let rarest = literal.and_then(|_| {
                terms
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (term, _))| {
                        self.postings.get(term.as_str()).map_or(0, Vec::len)
                    })
                    .map(|(i, _)| i)
            });
            for (i, (term, _)) in terms.iter().enumerate() {
                let first = rarest.map_or(!anchored || term.contains('_'), |r| r == i);
                if first && let Some(list) = self.postings.get(term.as_str()) {
                    candidates.extend(
                        list.iter()
                            .map(|(row, _)| *row)
                            .filter(|row| eligible(*row)),
                    );
                }
            }
        }
        let rows: Vec<_> = candidates.into_iter().collect();
        // Fixed term order then canonical row order reproduces v4 ChunkOut runs.
        let hits: Vec<Vec<(usize, u32)>> = if direct.is_some() || scan_time {
            vec![vec![]; terms.len()]
        } else {
            terms
                .iter()
                .map(|(term, _)| {
                    self.postings
                        .get(term.as_str())
                        .into_iter()
                        .flatten()
                        .filter_map(|(row, tf)| rows.binary_search(row).ok().map(|i| (i, *tf)))
                        .collect()
                })
                .collect()
        };
        let retained: Vec<_> = hits.iter().map(|h| !h.is_empty()).collect();
        let collection: Vec<usize> = hits
            .iter()
            .map(|h| {
                if scope_model {
                    h.len()
                } else {
                    h.iter().map(|(_, tf)| *tf as usize).sum()
                }
            })
            .collect();
        let mass = |row: usize| {
            let d = &self.documents[row];
            if scope_model { d.scope } else { d.length }
        };
        let total = rows.iter().map(|row| u64::from(mass(*row))).sum::<u64>() as f64;
        let any_hits = retained.iter().any(|x| *x);
        let floor = FLOOR_RECORDS * MU;
        let pad = if any_hits && total < floor {
            let view_total = (0..self.documents.len())
                .filter(|row| self.visible(*row, q, view))
                .map(|row| u64::from(mass(row)))
                .sum::<u64>();
            let view: Vec<u64> = terms
                .iter()
                .enumerate()
                .map(|(i, (term, _))| {
                    if !retained[i] {
                        return 0;
                    }
                    self.postings
                        .get(term.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|(row, _)| self.visible(*row, q, view))
                        .map(|(_, tf)| if scope_model { 1 } else { u64::from(*tf) })
                        .sum()
                })
                .collect();
            (view_total > 0).then_some((floor - total, view, view_total as f64))
        } else {
            None
        };
        let background: Vec<f64> = terms
            .iter()
            .enumerate()
            .map(|(i, (term, _))| match &pad {
                None => (collection[i] as f64 + literal_probability(term)) / (total + 1.0),
                Some((r, view, view_total)) => {
                    (collection[i] as f64
                        + r * view[i] as f64 / view_total
                        + literal_probability(term))
                        / (total + r + 1.0)
                }
            })
            .collect();
        let query_count = terms
            .iter()
            .enumerate()
            .filter(|(i, _)| retained[*i])
            .map(|(_, (_, count))| *count)
            .sum::<usize>() as f64;
        let mut gains: Vec<f64> = rows
            .iter()
            .map(|row| -query_count * (1. + mass(*row) as f64 / MU).log2())
            .collect();
        let score = |gains: &mut [f64], only: Option<&[bool]>| {
            for (ti, list) in hits.iter().enumerate() {
                let scale = MU * background[ti];
                let count = terms[ti].1 as f64;
                for (at, tf) in list {
                    if only.is_some_and(|chosen| !chosen[*at]) {
                        continue;
                    }
                    let d = &self.documents[rows[*at]];
                    let mass = if scope_model {
                        *tf as f64 * d.scope as f64 / d.length as f64
                    } else {
                        *tf as f64
                    };
                    gains[*at] += count * (1. + mass / scale).log2();
                }
            }
        };
        let mut exact = vec![true; rows.len()];
        if !prune || k >= rows.len() {
            score(&mut gains, None);
        } else {
            // Port of v4's conservative term bounds, exact seeds and reach guard.
            let mut bound = gains.clone();
            for (ti, list) in hits.iter().enumerate() {
                let scale = MU * background[ti];
                let count = terms[ti].1 as f64;
                let once = count * log2_upper(1. + 1. / scale);
                for (at, tf) in list {
                    bound[*at] += if *tf == 1 {
                        once
                    } else {
                        count * log2_upper(1. + *tf as f64 / scale)
                    };
                }
            }
            exact.fill(false);
            for at in best_indices(&bound, (2 * k).max(64)) {
                exact[at] = true;
            }
            score(&mut gains, Some(&exact));
            let mut seeded: Vec<_> = gains
                .iter()
                .enumerate()
                .filter(|(i, _)| exact[*i])
                .map(|(_, gain)| *gain + 0.0)
                .collect();
            seeded.sort_unstable_by(|a, b| b.total_cmp(a));
            let theta = seeded[k - 1];
            let reach = theta - 1e-9 * (1.0 + theta.abs());
            let mut more = vec![false; rows.len()];
            for at in 0..rows.len() {
                if !exact[at]
                    && (bound[at] + 0.0).partial_cmp(&reach) != Some(std::cmp::Ordering::Less)
                {
                    exact[at] = true;
                    more[at] = true;
                }
            }
            score(&mut gains, Some(&more));
        }
        let mut result: Vec<_> = rows
            .into_iter()
            .enumerate()
            .filter(|(i, _)| exact[*i])
            .map(|(i, row)| {
                let e = &self.documents[row].event;
                (
                    row,
                    Hit {
                        id: e.id,
                        bits: gains[i] + 0.0,
                        continued: false,
                        time: e.ts_us,
                        row: e.id,
                    },
                )
            })
            .collect();
        // F4 full textual ID order. Coarse prefix selection is an optimization,
        // not another tie rule; all exact finalists use the full decimal string.
        result.sort_by(|(_, a), (_, b)| {
            b.bits
                .total_cmp(&a.bits)
                .then_with(|| b.time.cmp(&a.time))
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        });
        result.truncate(k);
        (result, anchored && literal.is_none())
    }
}
#[cfg(test)]
thread_local! { static ANALYSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn analyze(text: &str) -> Result<Vec<(String, u32)>> {
    #[cfg(test)]
    ANALYSES.with(|n| n.set(n.get() + 1));
    let mut raw = HashMap::<String, usize>::new();
    for token in retrieval_terms(text) {
        *raw.entry(token).or_default() += 1;
    }
    let mut terms = BTreeMap::<String, usize>::new();
    for (term, count) in raw {
        *terms.entry(normalize_word(&term).into_owned()).or_default() += count;
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
