//! The NINET003 engine (main@7b98903): every original and the whole index are
//! memory resident after replay. Kept as the reader for v3 stores and as the
//! ranking oracle for v4 equivalence tests. Do not change its ranking; the
//! only change since main is reading receipts with a null entropy, which the
//! shared entropy underflow (fixed in oojuro-core) used to write.
use crate::text::{
    literal_probability, literal_query, normalize_word, phrase_start, query_terms, retrieval_terms,
};
use crate::{Matches, SourceMatch, TextSlice, TimelineMetrics, TimelineStats, extract_passage};
use oojuro_core::{Engine, Hit, Journal, MAX_FRAME, Query, Record, Result, SearchResult};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

const DATA_MAGIC: [u8; 8] = *b"NINET003";
const QUERY_MAGIC: [u8; 8] = *b"NINEQ002";
const LEARN_MAGIC: [u8; 8] = *b"NINEL002";
const MAX_DELTA_DEPTH: u8 = 4;
const CACHE_BYTES: usize = 8 * 1024 * 1024;
const CACHE_ENTRIES: usize = 32;
const MU: f64 = 128.0;
pub(crate) const RETRIEVAL_MODEL: &str = "dirichlet-scope-128-v1";
pub(crate) const RETRIEVAL_COUNT_MODEL: &str = "scope-scaled-raw-frequency-v1";
pub(crate) const RETRIEVAL_NORMALIZATION: &str = "english-snowball-1.2.0-v1";

#[derive(Clone, Serialize, Deserialize)]
struct Delta {
    base: usize,
    prefix: usize,
    suffix: usize,
    middle: String,
}
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    record: Option<Record>,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta: Option<Delta>,
}
struct Row {
    record: Record,
    length: usize,
    scope: usize,
    depth: u8,
}
#[derive(Clone, Copy)]
struct Posting {
    row: usize,
    count: usize,
}
#[derive(Clone, Serialize, Deserialize)]
struct Outcome {
    id: String,
    query: [u8; 32],
    source: String,
    useful: bool,
    at: i64,
    verifier: String,
}
#[derive(Serialize, Deserialize)]
struct Activity {
    query: [u8; 32],
    from: i64,
    at: i64,
    known_at: i64,
    ids: Vec<String>,
    /// Receipts written before the entropy underflow fix may hold `null`
    /// (serialized NaN); they are read as NaN so those stores open again.
    #[serde(deserialize_with = "crate::text::nan_if_null")]
    entropy_bits: f64,
    #[serde(default)]
    model: Option<String>,
}

struct Cached {
    result: Matches,
    metrics: TimelineMetrics,
    bytes: usize,
    used: u64,
}


/// Small, exclusively opened database for one authorized agent/project namespace.
/// Source text and the derived index are currently memory resident after replay.
/// The hosting application must enforce tenant access before opening a directory.
pub struct V3 {
    directory: PathBuf,
    archive: Journal,
    activity: Journal,
    learning: Journal,
    rows: Vec<Row>,
    ids: HashMap<String, usize>,
    time: BTreeMap<(i64, usize), usize>,
    versions: HashMap<String, Vec<usize>>,
    postings: HashMap<String, Vec<Posting>>,
    outcomes: Vec<Outcome>,
    by_query: HashMap<[u8; 32], Vec<usize>>,
    outcome_ids: BTreeSet<String>,
    cache: HashMap<[u8; 32], Cached>,
    cache_bytes: usize,
    clock: u64,
    queries: usize,
    last_metrics: TimelineMetrics,
    /// Receipts as read at open, retained only so v4 can migrate them.
    receipts: Vec<Vec<u8>>,
}

impl V3 {
    pub fn open(directory: &Path) -> Result<Self> {
        if directory.join("events.nine").exists() && !directory.join("timeline.nine").exists() {
            return Err(
                "historical Nine v1 store: use the v1 reader or explicitly import originals".into(),
            );
        }
        let (archive, stored) =
            Journal::open::<Stored>(&directory.join("timeline.nine"), DATA_MAGIC, true)?;
        let (activity, activities) =
            Journal::open::<Activity>(&directory.join("queries.timeline"), QUERY_MAGIC, true)?;
        let (learning, outcomes) =
            Journal::open::<Outcome>(&directory.join("outcomes.timeline"), LEARN_MAGIC, true)?;
        let mut db = Self {
            directory: directory.into(),
            archive,
            activity,
            learning,
            rows: Vec::new(),
            ids: HashMap::new(),
            time: BTreeMap::new(),
            versions: HashMap::new(),
            postings: HashMap::new(),
            outcomes: Vec::new(),
            by_query: HashMap::new(),
            outcome_ids: BTreeSet::new(),
            cache: HashMap::new(),
            cache_bytes: 0,
            clock: 0,
            queries: 0,
            last_metrics: TimelineMetrics::default(),
            receipts: Vec::new(),
        };
        for stored in stored {
            let (record, depth) = if let Some(delta) = stored.delta {
                let base = db
                    .rows
                    .get(delta.base)
                    .ok_or("delta points outside prior committed history")?;
                if base.depth >= MAX_DELTA_DEPTH {
                    return Err("delta chain exceeds recovery bound".into());
                }
                let original = if stored.record.is_some() {
                    base.record.text.clone()
                } else {
                    serde_json::to_string(&base.record)?
                };
                if delta.prefix > original.len()
                    || delta.suffix > original.len() - delta.prefix
                    || !original.is_char_boundary(delta.prefix)
                    || !original.is_char_boundary(original.len() - delta.suffix)
                {
                    return Err("invalid UTF-8 delta boundaries".into());
                }
                let size = delta
                    .prefix
                    .checked_add(delta.middle.len())
                    .and_then(|n| n.checked_add(delta.suffix))
                    .ok_or("delta size overflow")?;
                if size > MAX_FRAME {
                    return Err("decoded source exceeds frame bound".into());
                }
                let encoded = format!(
                    "{}{}{}",
                    &original[..delta.prefix],
                    delta.middle,
                    &original[original.len() - delta.suffix..]
                );
                let record = if let Some(mut record) = stored.record {
                    record.text = encoded;
                    record
                } else {
                    serde_json::from_str::<Record>(&encoded)?
                };
                (record, base.depth + 1)
            } else if let Some(record) = stored.record {
                (record, 0)
            } else {
                return Err("source frame must contain exactly one literal or delta".into());
            };
            if blake3::hash(&serde_json::to_vec(&record)?)
                .to_hex()
                .as_str()
                != stored.digest
            {
                return Err("timeline original digest mismatch".into());
            }
            db.validate_record(&record)?;
            db.index(record, depth);
        }
        for outcome in outcomes {
            db.validate_outcome(&outcome)?;
            db.index_outcome(outcome);
        }
        for a in activities {
            if a.entropy_bits.is_infinite()
                || a.entropy_bits < 0.0
                || a.ids.iter().any(|id| !db.ids.contains_key(id))
            {
                return Err("invalid timeline query receipt".into());
            }
            db.queries += 1;
            db.receipts.push(serde_json::to_vec(&a)?);
        }
        Ok(db)
    }
    /// Every original in append order plus outcome and receipt entries, for
    /// migration into a v4 store. All were verified by `open`.
    pub(crate) fn export_all(&self) -> Result<(Vec<Record>, Vec<Vec<u8>>, Vec<Vec<u8>>)> {
        let records = self.rows.iter().map(|r| r.record.clone()).collect();
        let outcomes = self
            .outcomes
            .iter()
            .map(serde_json::to_vec)
            .collect::<std::result::Result<_, _>>()?;
        Ok((records, outcomes, self.receipts.clone()))
    }
    /// The simplest write API; returns an immutable local source ID.
    pub fn put(&mut self, at: i64, text: &str) -> Result<String> {
        let mut hash = blake3::Hasher::new();
        hash.update(&(self.rows.len() as u64).to_le_bytes());
        hash.update(&at.to_le_bytes());
        hash.update(text.as_bytes());
        let id = hash.finalize().to_hex().to_string();
        let mut record = Record::new(&id, &id, text, at);
        record.verified = false;
        self.insert(record)?;
        Ok(id)
    }
    pub fn read(&self, id: &str) -> Option<&str> {
        self.ids.get(id).map(|i| self.rows[*i].record.text.as_str())
    }
    /// Query-addressed exact bytes inside an immutable original.
    pub fn passage(
        &self,
        id: &str,
        query: &str,
        max_bytes: usize,
    ) -> Result<Option<TextSlice<'_>>> {
        self.read(id)
            .map(|text| extract_passage(text, query, max_bytes))
            .transpose()
            .map(Option::flatten)
    }
    pub fn record(&self, id: &str) -> Option<&Record> {
        self.ids.get(id).map(|i| &self.rows[*i].record)
    }
    /// Immutable originals in append order, including superseded versions.
    /// Callers must authorize the owning database before exposing this iterator.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &Record> {
        self.rows.iter().map(|row| &row.record)
    }
    /// Copy the three journals at one committed boundary. Exclusive mutable
    /// access excludes concurrent queries/writes, including their receipts.
    /// An incomplete destination is never a usable backup without a manifest.
    pub fn snapshot_journals(&mut self, destination: &Path) -> Result<()> {
        std::fs::create_dir(destination)?;
        for (name, journal) in [
            ("timeline.nine", &mut self.archive),
            ("queries.timeline", &mut self.activity),
            ("outcomes.timeline", &mut self.learning),
        ] {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(destination.join(name))?;
            journal.copy_committed(&mut file)?;
            file.sync_all()?;
        }
        Ok(())
    }
    /// Inclusive event-time interval, returning exact originals in time order.
    pub fn range(&self, from: i64, to: i64, limit: usize) -> Result<Vec<&Record>> {
        if from > to || limit > 10_000 {
            return Err("invalid time range or result limit".into());
        }
        Ok(self
            .time
            .range((from, 0)..=(to, usize::MAX))
            .take(limit)
            .map(|(_, i)| &self.rows[*i].record)
            .collect())
    }
    pub fn find(&mut self, text: &str, from: i64, to: i64, limit: usize) -> Result<Matches> {
        let mut q = Query::new(text, to);
        q.k = limit;
        self.search_refs(&q, from)
    }
    pub fn metrics(&self) -> TimelineMetrics {
        self.last_metrics.clone()
    }
    /// Administrative memory release; also permits measuring uncached retrieval.
    pub fn clear_query_cache(&mut self) {
        self.clear_cache();
    }
    pub fn stats(&self) -> Result<TimelineStats> {
        Ok(TimelineStats {
            records: self.rows.len(),
            raw_text_bytes: self.rows.iter().map(|r| r.record.text.len()).sum(),
            archive_bytes: std::fs::metadata(self.directory.join("timeline.nine"))?.len(),
            delta_records: self.rows.iter().filter(|r| r.depth > 0).count(),
            vocabulary: self.postings.len(),
            indexed_postings: self.postings.values().map(Vec::len).sum(),
            persisted_queries: self.queries,
            cache_entries: self.cache.len(),
            cache_bytes_estimate: self.cache_bytes,
            learned_outcomes: self.outcomes.len(),
        })
    }
    fn validate_record(&self, r: &Record) -> Result<()> {
        r.validate()?;
        if r.embedding.is_some() {
            return Err("local Nine does not accept embeddings".into());
        }
        if self.ids.contains_key(&r.id) {
            return Err("duplicate immutable timeline source ID".into());
        }
        Ok(())
    }
    fn index(&mut self, r: Record, depth: u8) {
        let row = self.rows.len();
        let mut raw = HashMap::new();
        for token in retrieval_terms(&r.text) {
            *raw.entry(token).or_insert(0usize) += 1;
        }
        let mut terms = HashMap::new();
        for (term, count) in raw {
            let term = match normalize_word(&term) {
                std::borrow::Cow::Borrowed(_) => term,
                std::borrow::Cow::Owned(stem) => stem,
            };
            *terms.entry(term).or_insert(0usize) += count;
        }
        let length = terms.values().sum();
        let scope = terms.len();
        for (term, count) in &terms {
            self.postings
                .entry(term.clone())
                .or_default()
                .push(Posting { row, count: *count });
        }
        self.ids.insert(r.id.clone(), row);
        self.time.insert((r.valid_at, row), row);
        let versions = self.versions.entry(r.key.clone()).or_default();
        let tuple = (r.valid_at, r.recorded_at, row);
        let at = versions.partition_point(|i| {
            let old = &self.rows[*i].record;
            (old.valid_at, old.recorded_at, *i) <= tuple
        });
        versions.insert(at, row);
        self.rows.push(Row {
            record: r,
            length,
            scope,
            depth,
        });
        self.clear_cache();
    }
    fn latest<'a>(
        &'a self,
        row: usize,
        q: &Query,
        view: &mut HashMap<&'a str, Option<usize>>,
    ) -> bool {
        let r = &self.rows[row].record;
        if r.valid_at > q.valid_at || r.recorded_at > q.known_at {
            return false;
        }
        let versions = &self.versions[&r.key];
        let newest = *versions.last().expect("indexed source has a revision");
        let current = &self.rows[newest].record;
        if current.valid_at <= q.valid_at && current.recorded_at <= q.known_at {
            return newest == row;
        }
        *view.entry(r.key.as_str()).or_insert_with(|| {
            let end = versions.partition_point(|i| self.rows[*i].record.valid_at <= q.valid_at);
            versions[..end]
                .iter()
                .rev()
                .find(|i| self.rows[**i].record.recorded_at <= q.known_at)
                .copied()
        }) == Some(row)
    }
    fn frame_cost(stored: &Stored) -> Result<usize> {
        let raw = serde_json::to_vec(stored)?;
        Ok(13 + raw.len().min(lz4_flex::block::compress(&raw).len()))
    }
    fn encode(&self, r: &Record) -> Result<(Stored, u8)> {
        let b = serde_json::to_string(r)?;
        let mut chosen = Stored {
            record: Some(r.clone()),
            digest: blake3::hash(b.as_bytes()).to_hex().to_string(),
            delta: None,
        };
        let mut cost = Self::frame_cost(&chosen)?;
        let mut depth = 0;
        let mut bases: Vec<_> = (self.rows.len().saturating_sub(2)..self.rows.len()).collect();
        if let Some(i) = self.versions.get(&r.key).and_then(|v| v.last()) {
            bases.push(*i);
        }
        bases.sort_unstable();
        bases.dedup();
        for base in bases {
            let old = &self.rows[base];
            if old.depth >= MAX_DELTA_DEPTH {
                continue;
            }
            // Compare both source-text and full-envelope deltas. An ID near the
            // start of an envelope must not hide a long repeated tool payload.
            for whole_record in [false, true] {
                let a = if whole_record {
                    serde_json::to_string(&old.record)?
                } else {
                    old.record.text.clone()
                };
                let b = if whole_record {
                    b.as_str()
                } else {
                    r.text.as_str()
                };
                let mut prefix = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
                while !a.is_char_boundary(prefix) || !b.is_char_boundary(prefix) {
                    prefix -= 1;
                }
                let mut suffix = a.as_bytes()[prefix..]
                    .iter()
                    .rev()
                    .zip(b.as_bytes()[prefix..].iter().rev())
                    .take_while(|(x, y)| x == y)
                    .count();
                while !a.is_char_boundary(a.len() - suffix) || !b.is_char_boundary(b.len() - suffix)
                {
                    suffix -= 1;
                }
                if prefix + suffix < 64 {
                    continue;
                }
                let proposal = Stored {
                    record: if whole_record {
                        None
                    } else {
                        let mut record = r.clone();
                        record.text.clear();
                        Some(record)
                    },
                    digest: chosen.digest.clone(),
                    delta: Some(Delta {
                        base,
                        prefix,
                        suffix,
                        middle: b[prefix..b.len() - suffix].to_string(),
                    }),
                };
                let next = Self::frame_cost(&proposal)?;
                if next < cost {
                    cost = next;
                    chosen = proposal;
                    depth = old.depth + 1;
                }
            }
        }
        Ok((chosen, depth))
    }
    /// Only explicit, caller-verified source usefulness is learned. Source text,
    /// verification flags and timestamps are never promoted or rewritten.
    pub fn record_outcome(
        &mut self,
        query: &str,
        source: &str,
        useful: bool,
        at: i64,
        verifier: &str,
    ) -> Result<()> {
        let q = *blake3::hash(query.as_bytes()).as_bytes();
        let id = blake3::hash(&serde_json::to_vec(&(q, source, useful, at, verifier))?)
            .to_hex()
            .to_string();
        let outcome = Outcome {
            id,
            query: q,
            source: source.into(),
            useful,
            at,
            verifier: verifier.into(),
        };
        self.validate_outcome(&outcome)?;
        self.learning.append(&outcome)?;
        self.index_outcome(outcome);
        Ok(())
    }
    fn validate_outcome(&self, o: &Outcome) -> Result<()> {
        let row = self
            .ids
            .get(&o.source)
            .ok_or("outcome source does not exist")?;
        if o.verifier.is_empty()
            || o.verifier.len() > 1024
            || o.at < self.rows[*row].record.recorded_at
            || self.outcome_ids.contains(&o.id)
        {
            return Err("invalid, premature or duplicate outcome receipt".into());
        }
        Ok(())
    }
    fn index_outcome(&mut self, o: Outcome) {
        self.outcome_ids.insert(o.id.clone());
        self.by_query
            .entry(o.query)
            .or_default()
            .push(self.outcomes.len());
        self.outcomes.push(o);
        self.clear_cache();
    }
    fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_bytes = 0;
    }
    fn cache_key(q: &Query, from: i64) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(q.text.as_bytes());
        for n in [q.valid_at, q.known_at, from] {
            h.update(&n.to_le_bytes());
        }
        h.update(&(q.k as u64).to_le_bytes());
        h.update(&(q.max_context_bytes as u64).to_le_bytes());
        *h.finalize().as_bytes()
    }
    fn cache_result(&mut self, key: [u8; 32], result: &Matches, metrics: &TimelineMetrics) {
        let bytes = result
            .matches
            .iter()
            .map(|m| m.id.len() + 96)
            .sum::<usize>()
            + 1024;
        if bytes > CACHE_BYTES {
            return;
        }
        while self.cache.len() >= CACHE_ENTRIES || self.cache_bytes + bytes > CACHE_BYTES {
            let Some(old) = self
                .cache
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(v) = self.cache.remove(&old) {
                self.cache_bytes -= v.bytes;
            }
        }
        self.cache_bytes += bytes;
        self.cache.insert(
            key,
            Cached {
                result: result.clone(),
                metrics: metrics.clone(),
                bytes,
                used: self.clock,
            },
        );
    }
    fn persist_query(&mut self, q: &Query, from: i64, result: &Matches) -> Result<()> {
        self.activity.append(&Activity {
            query: *blake3::hash(q.text.as_bytes()).as_bytes(),
            from,
            at: q.valid_at,
            known_at: q.known_at,
            ids: result.matches.iter().map(|h| h.id.clone()).collect(),
            entropy_bits: result.routing_entropy_bits,
            model: Some(format!(
                "{RETRIEVAL_MODEL}:{}:{}",
                RETRIEVAL_COUNT_MODEL, RETRIEVAL_NORMALIZATION
            )),
        })?;
        self.queries += 1;
        Ok(())
    }
    /// Complete local query path; includes a synchronous durable query receipt.
    pub fn search_refs(&mut self, q: &Query, from: i64) -> Result<Matches> {
        q.validate()?;
        if q.embedding.is_some() {
            return Err("local Nine queries do not accept embeddings".into());
        }
        if from > q.valid_at || q.text.len() > 65_536 {
            return Err("invalid query interval or oversized query".into());
        }
        self.clock = self.clock.wrapping_add(1);
        let key = Self::cache_key(q, from);
        if let Some(c) = self.cache.get_mut(&key) {
            c.used = self.clock;
            let result = c.result.clone();
            let mut metrics = c.metrics.clone();
            metrics.cache_hit = true;
            metrics.scored_records = 0;
            metrics.posted_rows_visited = 0;
            self.persist_query(q, from, &result)?;
            self.last_metrics = metrics;
            return Ok(result);
        }
        let (result, metrics) = self.rank_continued(q, from)?;
        self.persist_query(q, from, &result)?;
        self.cache_result(key, &result, &metrics);
        self.last_metrics = metrics;
        Ok(result)
    }
    /// The anchored ranking of `q`; when its anchored view holds fewer than
    /// `q.k` records, continued with the unanchored ranking (`split`), past
    /// the records already listed (see `SourceMatch::continued`).
    fn rank_continued(&self, q: &Query, from: i64) -> Result<(Matches, TimelineMetrics)> {
        let (mut result, mut metrics, continuable) = self.rank(q, from, false)?;
        if !continuable || result.matches.len() >= q.k {
            return Ok((result, metrics));
        }
        let (rest, more, _) = self.rank(q, from, true)?;
        let listed: HashSet<String> = result.matches.iter().map(|m| m.id.clone()).collect();
        for m in rest.matches {
            if result.matches.len() == q.k {
                break;
            }
            if !listed.contains(&m.id) {
                result.matches.push(SourceMatch { continued: true, ..m });
                metrics.continued_matches += 1;
            }
        }
        metrics.posted_rows_visited += more.posted_rows_visited;
        metrics.scored_records += more.scored_records;
        metrics.candidate_records = more.candidate_records;
        metrics.routing_entropy_bits = more.routing_entropy_bits;
        result.routing_entropy_bits = more.routing_entropy_bits;
        Ok((result, metrics))
    }
    /// One ranking of `q`; with `split`, every identifier is read as its
    /// component words (as when the view holds no occurrence of it). Also
    /// returns whether the ranking was anchored (and so may be continued).
    fn rank(&self, q: &Query, from: i64, split: bool) -> Result<(Matches, TimelineMetrics, bool)> {
        let mut metrics = TimelineMetrics::default();
        let mut view = HashMap::new();
        let literal = literal_query(&q.text);
        let mut query = query_terms(&q.text);
        // Exact identifiers are literals. Fall back to components only when
        // this historical view has no eligible occurrence of the whole name.
        let backoff: Vec<_> = query
            .iter()
            .filter(|(term, _)| {
                term.contains('_')
                    && (split
                        || !self.postings.get(*term).is_some_and(|ps| {
                        ps.iter().any(|p| {
                            self.rows[p.row].record.valid_at >= from
                                && self.latest(p.row, q, &mut view)
                        })
                    }))
            })
            .map(|(term, count)| (term.clone(), *count))
            .collect();
        for (term, count) in backoff {
            query.remove(&term);
            for (part, n) in query_terms(&term.replace('_', " ")) {
                *query.entry(part).or_default() += n * count;
            }
        }
        metrics.query_terms = query.len();
        let direct = literal
            .is_none()
            .then(|| {
                q.text.strip_prefix("key:").or_else(|| {
                    self.versions
                        .contains_key(&q.text)
                        .then_some(q.text.as_str())
                })
            })
            .flatten();
        let anchored = direct.is_none() && query.keys().any(|term| term.contains('_'));
        let scope_model = direct.is_none() && !anchored && literal.is_none() && !q.text.is_empty();
        let mut candidates = Vec::<usize>::new();
        // Each source's two-clock eligibility is checked once, even when it
        // matches many terms. States also address sparse score updates below.
        let mut eligible = vec![0u8; self.rows.len()];
        let mut consider = |i: usize, view: &mut _| {
            if eligible[i] == 0 {
                eligible[i] = if self.rows[i].record.valid_at >= from
                    && self.latest(i, q, view)
                    && literal.is_none_or(|needle| {
                        phrase_start(&self.rows[i].record.text, needle).is_some()
                    }) {
                    candidates.push(i);
                    2
                } else {
                    1
                };
            }
            eligible[i] == 2
        };
        let mut collection = HashMap::<String, usize>::new();
        if let Some(key) = direct {
            if let Some(v) = self.versions.get(key) {
                for i in v {
                    consider(*i, &mut view);
                }
            }
        } else if q.text.is_empty() || (literal.is_some() && query.is_empty()) {
            for (_, i) in self.time.range((from, 0)..=(q.valid_at, usize::MAX)) {
                consider(*i, &mut view);
            }
        } else {
            // A word-bounded exact phrase contains every query term. One rare
            // posting list is a complete candidate generator for that mode.
            let rarest = literal.and_then(|_| {
                query
                    .keys()
                    .min_by_key(|term| self.postings.get(*term).map_or(0, Vec::len))
            });
            for term in query
                .keys()
                .filter(|term| rarest.map_or(!anchored || term.contains('_'), |r| *term == r))
            {
                if let Some(postings) = self.postings.get(term) {
                    for p in postings {
                        metrics.posted_rows_visited += 1;
                        if consider(p.row, &mut view) {
                            *collection.entry(term.clone()).or_default() +=
                                if scope_model { 1 } else { p.count };
                        }
                    }
                }
            }
        }
        let mut learned = HashMap::<usize, (usize, usize)>::new();
        let hash = *blake3::hash(q.text.as_bytes()).as_bytes();
        if let Some(indices) = self.by_query.get(&hash) {
            for i in indices {
                let o = &self.outcomes[*i];
                if o.at > q.valid_at || o.at > q.known_at {
                    continue;
                }
                let row = self.ids[&o.source];
                if self.rows[row].record.valid_at < from
                    || !self.latest(row, q, &mut view)
                    || literal.is_some_and(|needle| {
                        phrase_start(&self.rows[row].record.text, needle).is_none()
                    })
                {
                    continue;
                }
                metrics.eligible_outcomes += 1;
                let count = learned.entry(row).or_default();
                if o.useful {
                    count.0 += 1;
                    consider(row, &mut view);
                } else {
                    count.1 += 1;
                }
            }
        }
        drop(consider);
        if anchored || literal.is_some() {
            // Surrounding words refine the literal-addressed view; they cannot
            // replace a recognized identifier with unrelated candidate sources.
            collection.clear();
            for term in query.keys() {
                if let Some(ps) = self.postings.get(term) {
                    for p in ps {
                        metrics.posted_rows_visited += 1;
                        if eligible[p.row] == 2 {
                            *collection.entry(term.clone()).or_default() += p.count;
                        }
                    }
                }
            }
        }
        // Only the query-addressed historical view supplies this background model;
        // unrelated or future records cannot perturb its probabilities.
        query.retain(|term, _| collection.contains_key(term));
        let total = candidates
            .iter()
            .map(|i| {
                if scope_model {
                    self.rows[*i].scope
                } else {
                    self.rows[*i].length
                }
            })
            .sum::<usize>() as f64;
        // A candidate background below BACKGROUND_FLOOR_RECORDS * MU of mass
        // is padded with whole-view text up to that mass (the v4 rule).
        let floor = crate::BACKGROUND_FLOOR_RECORDS * MU;
        let pad = (!collection.is_empty() && total < floor).then(|| {
            let mass = |i: usize| if scope_model { self.rows[i].scope } else { self.rows[i].length };
            let in_view = |i: usize, view: &mut HashMap<_, _>| {
                self.rows[i].record.valid_at >= from && self.latest(i, q, view)
            };
            let view_total: usize = (0..self.rows.len())
                .filter(|i| in_view(*i, &mut view))
                .map(mass)
                .sum();
            let counts: HashMap<&String, usize> = query
                .keys()
                .map(|term| {
                    let n = self.postings[term]
                        .iter()
                        .filter(|p| in_view(p.row, &mut view))
                        .map(|p| if scope_model { 1 } else { p.count })
                        .sum();
                    (term, n)
                })
                .collect();
            (floor - total, counts, view_total as f64)
        });
        let pad = pad.filter(|p| p.2 > 0.0);
        let background: HashMap<_, _> = query
            .keys()
            .map(|term| {
                let count = collection[term] as f64;
                let p = match &pad {
                    None => (count + literal_probability(term)) / (total + 1.0),
                    Some((r, view, view_total)) => {
                        (count + r * view[term] as f64 / view_total + literal_probability(term)) / (total + r + 1.0)
                    }
                };
                (term.clone(), p)
            })
            .collect();
        metrics.background_pad = pad.as_ref().map_or(0.0, |p| p.0);
        let baseline = query
            .iter()
            .map(|(term, count)| -(*count as f64) * background[term].log2())
            .sum::<f64>();
        metrics.background_query_bits = baseline;
        metrics.candidate_records = candidates.len();
        // Algebraically identical to L(q|B)-L(q|d). Absent terms share the
        // length penalty; only nonzero postings need individual log updates.
        let query_count = query.values().sum::<usize>() as f64;
        let mut gains = vec![0.; self.rows.len()];
        for i in &candidates {
            let mass = if scope_model {
                self.rows[*i].scope
            } else {
                self.rows[*i].length
            };
            gains[*i] = -query_count * (1. + mass as f64 / MU).log2();
        }
        for (term, count) in &query {
            let scale = MU * background[term];
            for p in &self.postings[term] {
                metrics.posted_rows_visited += 1;
                if eligible[p.row] == 2 {
                    let mass = if scope_model {
                        p.count as f64 * self.rows[p.row].scope as f64
                            / self.rows[p.row].length as f64
                    } else {
                        p.count as f64
                    };
                    gains[p.row] += *count as f64 * (1. + mass / scale).log2();
                }
            }
        }
        let mut scored: Vec<_> = candidates
            .into_iter()
            .map(|i| {
                let conditional = baseline - gains[i];
                let (positive, negative) = learned.get(&i).copied().unwrap_or_default();
                let usefulness = ((positive + 1) as f64 / (negative + 1) as f64)
                    .log2()
                    .clamp(-8.0, 8.0);
                (i, gains[i] + usefulness, conditional)
            })
            .collect();
        metrics.scored_records = scored.len();
        let maximum = scored
            .iter()
            .map(|(_, s, _)| *s)
            .fold(f64::NEG_INFINITY, f64::max);
        let weights: Vec<_> = scored
            .iter()
            .map(|(_, s, _)| (*s - maximum).exp2())
            .collect();
        metrics.routing_entropy_bits = oojuro_core::entropy(&weights);
        let compare = |a: &(usize, f64, f64), b: &(usize, f64, f64)| {
            b.1.total_cmp(&a.1)
                .then_with(|| {
                    self.rows[b.0]
                        .record
                        .valid_at
                        .cmp(&self.rows[a.0].record.valid_at)
                })
                .then_with(|| self.rows[a.0].record.id.cmp(&self.rows[b.0].record.id))
        };
        let pool = q.k.min(scored.len());
        if pool < scored.len() {
            scored.select_nth_unstable_by(pool, compare);
            scored.truncate(pool);
        }
        scored.sort_unstable_by(compare);
        metrics.best_conditional_query_bits = scored.first().map(|(_, _, bits)| *bits);
        let mut matches = Vec::new();
        for (i, score, _) in scored {
            if matches.len() == q.k {
                break;
            }
            let r = &self.rows[i].record;
            matches.push(SourceMatch {
                id: r.id.clone(),
                time: r.valid_at,
                bytes: r.text.len(),
                score_bits: score,
                continued: false,
            });
        }
        Ok((
            Matches {
                matches,
                routing_entropy_bits: metrics.routing_entropy_bits,
            },
            metrics,
            anchored && literal.is_none(),
        ))
    }
}

impl Engine for V3 {
    fn name(&self) -> &'static str {
        "nine"
    }
    fn insert(&mut self, record: Record) -> Result<()> {
        self.validate_record(&record)?;
        let (stored, depth) = self.encode(&record)?;
        self.archive.append(&stored)?;
        self.index(record, depth);
        Ok(())
    }
    fn search(&mut self, query: &Query) -> Result<SearchResult> {
        query.validate()?;
        let mut pool = query.clone();
        pool.k = query.k.saturating_mul(8).clamp(query.k, 10_000);
        let refs = self.search_refs(&pool, i64::MIN)?;
        let mut hits = Vec::new();
        let mut bytes = 0usize;
        for m in refs.matches {
            if hits.len() == query.k {
                break;
            }
            if bytes.saturating_add(m.bytes) > query.max_context_bytes {
                continue;
            }
            bytes += m.bytes;
            hits.push(Hit {
                record: self.record(&m.id).expect("indexed source").clone(),
                score: m.score_bits as f32,
                uncertainty_bits: None,
            });
        }
        Ok(SearchResult{engine:"nine".into(),hits,routing_entropy_bits:refs.routing_entropy_bits,
            notes:vec!["Local keyword postings + conditional query code length; no embeddings or LLM.".into(),
                "Routing entropy describes candidate ambiguity, not factual confidence. Exact originals are returned.".into()]})
    }
    fn len(&self) -> usize {
        self.rows.len()
    }
}

#[cfg(test)]
#[path = "v3_tests.rs"]
mod tests;
