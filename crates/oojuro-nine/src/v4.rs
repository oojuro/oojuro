//! The v4 engine: a group-commit journal feeding a searchable in-memory fresh
//! segment, immutable memory-mapped index segments, originals behind the
//! [`Archive`] boundary, and `&self` reads that run concurrently.
//!
//! Durability: an append is acknowledged after its batch is written and
//! `sync_data`ed; concurrent writers share one sync (leader/follower group
//! commit). It is searchable before the call returns (read-your-writes).
//!
//! Directory: `LOCK`, `manifest.nine` (atomically replaced), `wal-G.nine`
//! journals of records not yet in segments, `seg-N.idx` + `seg-N.src`
//! segments, `outcomes.v4` and `receipts.v4`. A v3 directory
//! (`timeline.nine`) is migrated on first open into new files; the v3 files
//! are left untouched.
use crate::archive::{Archive, BlockArchive};
use crate::codec::{decode_record, decode_record_with, derived_id, encode_record_with, fingerprint};
use crate::segment::ROW_UNINDEXED;
use crate::shared::{self, LogBinding};
use crate::segment::{
    Analyzed, Cursor, DiskSegment, Fresh, List, Postings, Row, RowMeta, SegmentWriter, Span, analyze,
};
use crate::text::{literal_probability, literal_query, phrase_start, query_terms};
use crate::wal::{Journal, Kind, sync_dir};
use crate::{
    Matches, RETRIEVAL_COUNT_MODEL, RETRIEVAL_MODEL, RETRIEVAL_NORMALIZATION, SourceMatch,
    TimelineMetrics, TimelineStats, extract_passage,
};
use oojuro_core::{Engine, Hit, MAX_FRAME, Query, Record, Result, SearchResult};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    cell::RefCell,
    cmp::Ordering as Order,
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

const CACHE_BYTES: usize = 8 * 1024 * 1024;
const CACHE_ENTRIES: usize = 32;
const FORMAT: &str = "oojuro-nine-v4";
const MANIFEST: &str = "manifest.nine";

/// How query receipts are persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receipts {
    /// Synced before each search returns (the v3 behaviour).
    Durable,
    /// Written without a sync; made durable by `sync`, snapshots and drop.
    /// A crash can lose the most recent receipts, never records.
    Buffered,
    /// Not recorded.
    Off,
}

#[derive(Clone)]
pub struct Options {
    /// Flush the fresh segment once its originals reach this many bytes.
    pub flush_text_bytes: usize,
    /// ... or once it holds this many postings.
    pub flush_postings: usize,
    pub receipts: Receipts,
    pub archive: Arc<dyn Archive>,
    /// Dirichlet smoothing mass (bits model). v3 used 128.
    pub mu: f64,
    pub background: Background,
    /// Worker threads for one large query (segments scanned in parallel).
    pub query_threads: usize,
    /// Merge this many adjacent same-level segments in the background;
    /// 0 or 1 disables merging.
    pub merge_fanout: usize,
    /// Experimental deferred durability: a batch is acknowledged once written
    /// to the journal (not yet synced); [`Nine::sync_journal`] makes every
    /// acknowledged batch durable. A crash can lose batches acknowledged
    /// since the last sync. Off by default (one sync per batch).
    pub deferred_sync: bool,
    /// Run merges on this store's own thread (true) or only when the owner
    /// calls [`Nine::compact`] (false: e.g. one merge thread for many stores).
    pub background_merges: bool,
    /// Journal into a process-wide [`crate::SharedLog`] under this store id
    /// instead of private `wal-G.nine` files: commits of many stores share
    /// one sync. The binding is recorded in the manifest; the store opens
    /// only with the same log and id. None (default): private journals.
    pub shared_log: Option<LogBinding>,
    /// Longest accepted query text in bytes (a setting, not a format limit).
    pub max_query_bytes: usize,
    /// What `routing_entropy_bits` covers. `All` (default) scores every
    /// candidate exactly; `TopK` reports the entropy of the returned
    /// results only, which lets search score exactly just the candidates
    /// whose score bound can reach the k-th best (same ids and score bits).
    pub routing_entropy: RoutingEntropy,
}

/// The candidate set routing entropy is computed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingEntropy {
    /// Every scored candidate (v3).
    All,
    /// The returned top-k only.
    TopK,
}

/// Which records define the background language model of a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Background {
    /// v3: only candidates (eligible records sharing a query word). Its
    /// normalizer shrinks with the candidate set, so effective smoothing
    /// grows as mu * view / candidates for rare queries.
    Candidates,
    /// Every eligible record of the historical view (two clocks, lower
    /// bound and latest revision); nothing outside the view is counted.
    View,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            flush_text_bytes: 64 * 1024 * 1024,
            flush_postings: 4_000_000,
            receipts: Receipts::Buffered,
            archive: Arc::new(BlockArchive::default()),
            mu: 128.0,
            background: Background::Candidates,
            query_threads: std::thread::available_parallelism().map_or(1, |n| n.get().min(8)),
            merge_fanout: 8,
            deferred_sync: false,
            background_merges: true,
            shared_log: None,
            max_query_bytes: 65_536,
            routing_entropy: RoutingEntropy::All,
        }
    }
}

impl Options {
    /// Ranking model identifier recorded in query receipts.
    pub fn model(&self) -> String {
        let background = match self.background {
            Background::Candidates => "scope",
            Background::View => "view",
        };
        format!(
            "dirichlet-{background}-{}-v1:{RETRIEVAL_COUNT_MODEL}:{RETRIEVAL_NORMALIZATION}",
            self.mu
        )
    }
}

/// An exact byte range of an original, owned.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Passage {
    pub start: usize,
    pub end: usize,
    pub text: String,
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

/// A v3 query receipt (JSON); migrated into the binary v4 form.
#[derive(Serialize, Deserialize)]
struct Activity {
    query: [u8; 32],
    from: i64,
    at: i64,
    known_at: i64,
    ids: Vec<String>,
    #[serde(deserialize_with = "crate::text::nan_if_null")]
    entropy_bits: f64,
    #[serde(default)]
    model: Option<String>,
}

fn current_model() -> String {
    format!("{RETRIEVAL_MODEL}:{RETRIEVAL_COUNT_MODEL}:{RETRIEVAL_NORMALIZATION}")
}

fn mass_of(m: &RowMeta, scope: bool) -> u64 {
    u64::from(if scope { m.scope } else { m.length })
}

/// Binary query receipt: query BLAKE3 (32 B), zigzag from / at /
/// (known_at - at), entropy f64, model tag (0 none, 1 current, 2 + string),
/// then result rows in rank order as a count and zigzag deltas. Rows are
/// immutable ordinals, so a receipt is ~100 bytes instead of 64-hex ids.
fn encode_receipt(
    query: &[u8; 32],
    from: i64,
    at: i64,
    known_at: i64,
    entropy: f64,
    model: Option<&str>,
    rows: &[Row],
) -> Vec<u8> {
    use crate::codec::{put_varint, zigzag};
    let mut b = Vec::with_capacity(64 + 3 * rows.len());
    b.extend_from_slice(query);
    put_varint(&mut b, zigzag(from));
    put_varint(&mut b, zigzag(at));
    put_varint(&mut b, zigzag(known_at.wrapping_sub(at)));
    b.extend_from_slice(&entropy.to_le_bytes());
    match model {
        None => b.push(0),
        Some(m) if m == current_model() => b.push(1),
        Some(m) => {
            b.push(2);
            put_varint(&mut b, m.len() as u64);
            b.extend_from_slice(m.as_bytes());
        }
    }
    put_varint(&mut b, rows.len() as u64);
    let mut previous = 0i64;
    for r in rows {
        put_varint(&mut b, zigzag(i64::from(*r) - previous));
        previous = i64::from(*r);
    }
    b
}

/// Validate one binary receipt against a store of `end` rows.
fn check_receipt(b: &[u8], end: Row) -> Result<()> {
    use crate::codec::{get_varint, unzigzag};
    let mut pos = 32;
    if b.len() < pos {
        return err("truncated query receipt");
    }
    for _ in 0..3 {
        get_varint(b, &mut pos)?;
    }
    let entropy = f64::from_le_bytes(
        b.get(pos..pos + 8)
            .ok_or("truncated query receipt")?
            .try_into()?,
    );
    pos += 8;
    match b.get(pos).copied() {
        Some(0 | 1) => pos += 1,
        Some(2) => {
            pos += 1;
            let n = usize::try_from(get_varint(b, &mut pos)?)?;
            std::str::from_utf8(b.get(pos..pos + n).ok_or("truncated query receipt")?)?;
            pos += n;
        }
        _ => return err("invalid query receipt model"),
    }
    let n = get_varint(b, &mut pos)?;
    let mut row = 0i64;
    for _ in 0..n {
        row += unzigzag(get_varint(b, &mut pos)?);
        if row < 0 || row >= i64::from(end) {
            return err("query receipt names a row outside the store");
        }
    }
    // NaN only appears in receipts migrated from v3 stores written before
    // the entropy underflow fix.
    if pos != b.len() || entropy.is_infinite() || entropy < 0.0 {
        return err("invalid timeline query receipt");
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct SegmentEntry {
    id: u64,
    start: Row,
    rows: u32,
    /// 0 for flushed segments; a merge of level-L segments has level L+1.
    #[serde(default)]
    level: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct Manifest {
    format: String,
    archive: String,
    segments: Vec<SegmentEntry>,
    next_segment: u64,
    /// Journals with this generation or later hold rows not in segments.
    wal_gen: u64,
    /// Rows covered by segments.
    rows: Row,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migrated_from: Option<serde_json::Value>,
    /// Set when the store journals into a shared log (then `wal_gen` is
    /// unused and the store has no `wal-G.nine` files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shared_log: Option<Binding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Binding {
    log: String,
    store: u64,
}

impl Binding {
    fn of(options: &Options) -> Option<Self> {
        options.shared_log.as_ref().map(|b| Self {
            log: b.log.id(),
            store: b.store,
        })
    }
}

/// Version groups for keys with more than one row; rows sorted by
/// (valid_at, recorded_at, row) as in v3.
#[derive(Default)]
struct Groups {
    of: HashMap<Row, u32>,
    list: Vec<Vec<Row>>,
    /// One bit per row: set when the row belongs to a version group, so
    /// the common single-version case never touches the hash map.
    versioned: Vec<u64>,
}

impl Groups {
    fn mark(&mut self, row: Row) {
        let word = row as usize / 64;
        if self.versioned.len() <= word {
            self.versioned.resize(word + 1, 0);
        }
        self.versioned[word] |= 1 << (row % 64);
    }
    #[inline]
    fn is_versioned(&self, row: Row) -> bool {
        self.versioned
            .get(row as usize / 64)
            .is_some_and(|w| w & (1 << (row % 64)) != 0)
    }
    #[inline]
    fn group(&self, row: Row) -> Option<u32> {
        let bit = self
            .versioned
            .get(row as usize / 64)
            .is_some_and(|w| w & (1 << (row % 64)) != 0);
        if bit { self.of.get(&row).copied() } else { None }
    }
}

struct LiveOutcome {
    outcome: Outcome,
    row: Row,
}

struct State {
    segments: Vec<Arc<DiskSegment>>,
    fresh: Fresh,
    groups: Groups,
    outcomes: Vec<LiveOutcome>,
    by_query: HashMap<[u8; 32], Vec<usize>>,
    outcome_ids: HashSet<String>,
    /// Incremented by every visible change; tags cached results.
    epoch: u64,
    /// Scope and length of every row that is not the newest version of
    /// its key, for whole-view statistics of current-time queries.
    superseded: (u64, u64),
}

struct Writer {
    /// The private journal; None when the store uses a shared log.
    wal: Option<Journal>,
    wal_gen: u64,
    /// Closed journals (generation >= manifest.wal_gen) left by recovery;
    /// their rows are in the fresh segment until the next flush.
    sealed: Vec<u64>,
    manifest: Manifest,
    poisoned: bool,
    /// Deferred durability: the journal holds acknowledged but unsynced batches.
    unsynced: bool,
    /// Shared log: the next row to assign (batches still in flight included).
    next_row: u64,
}

/// Shared log: batches become visible in row order once durable.
struct ApplyOrder {
    /// The first row of the next batch to apply.
    next: u64,
    /// A batch failed after taking its rows: later ones fail too.
    failed: bool,
}

/// A batch's turn to apply. Leaving it any way but `succeeded` (an error,
/// a poisoned lock, a panic) marks the store failed and wakes every later
/// batch, which would otherwise wait for this one's rows forever.
struct ApplyTurn<'a> {
    apply: &'a (Mutex<ApplyOrder>, Condvar),
    done: bool,
}

impl<'a> ApplyTurn<'a> {
    fn order(&self) -> std::sync::MutexGuard<'a, ApplyOrder> {
        self.apply.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn succeeded(mut self, order: &mut ApplyOrder, end: u64) {
        order.next = end;
        self.done = true;
    }
}

impl Drop for ApplyTurn<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.order().failed = true;
        }
        self.apply.1.notify_all();
    }
}

struct Incoming {
    record: Record,
    /// The id (and key) are `derived_id` of the row assigned at commit.
    derived: bool,
    /// False: stored exactly and readable by id, key and time, never indexed.
    indexed: bool,
    analyzed: Analyzed,
}

#[derive(Default)]
struct Slot {
    /// Ids and the first row of the ticket (its rows are consecutive).
    result: Mutex<Option<Result<(Vec<String>, Row)>>>,
    done: Condvar,
}

struct Ticket {
    items: Vec<Incoming>,
    slot: Arc<Slot>,
}

#[derive(Default)]
struct Queue {
    pending: Vec<Ticket>,
    leader: bool,
}

struct Cached {
    epoch: u64,
    result: Matches,
    rows: Vec<Row>,
    metrics: TimelineMetrics,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<[u8; 32], Cached>,
    bytes: usize,
    clock: u64,
}

/// Exclusively opened database for one authorized agent/project namespace.
/// The hosting application must enforce tenant access before opening it.
pub struct Nine {
    directory: PathBuf,
    _lock: File,
    options: Options,
    state: Arc<RwLock<State>>,
    writer: Arc<Mutex<Writer>>,
    queue: Mutex<Queue>,
    receipts: Mutex<Option<Journal>>,
    outcomes_log: Mutex<Journal>,
    queries: AtomicUsize,
    cache: Arc<Mutex<Cache>>,
    last_metrics: Mutex<TimelineMetrics>,
    merges: merge::MergeCtx,
    merge_signal: Arc<(Mutex<bool>, Condvar)>,
    merger: Option<std::thread::JoinHandle<()>>,
    /// Shared log: apply order of durable batches, and the ids of batches
    /// queued but not yet visible (for the duplicate check).
    apply: (Mutex<ApplyOrder>, Condvar),
    inflight: Mutex<HashSet<String>>,
    /// Attachment to the shared log (detaches on drop, after the merger).
    _attached: Option<shared::Attached>,
}

#[path = "merge.rs"]
mod merge;

fn err<T>(message: &'static str) -> Result<T> {
    Err(message.into())
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    m.lock().map_err(|_| "engine lock poisoned".into())
}

fn wal_name(generation: u64) -> String {
    format!("wal-{generation:08}.nine")
}

fn seg_names(id: u64) -> (String, String) {
    (format!("seg-{id:08}.idx"), format!("seg-{id:08}.src"))
}

fn create_private(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn write_manifest(directory: &Path, m: &Manifest) -> Result<()> {
    let json = serde_json::to_vec(m)?;
    let mut body = json.clone();
    body.push(b'\n');
    body.extend_from_slice(blake3::hash(&json).to_hex().as_bytes());
    body.push(b'\n');
    let tmp = directory.join("manifest.tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = create_private(&tmp)?;
    f.write_all(&body)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, directory.join(MANIFEST))?;
    sync_dir(directory)
}

fn read_manifest(directory: &Path) -> Result<Manifest> {
    let body = fs::read(directory.join(MANIFEST))?;
    let text = std::str::from_utf8(&body)?;
    let mut lines = text.split('\n');
    let (Some(json), Some(hash), Some(""), None) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return err("malformed manifest");
    };
    if blake3::hash(json.as_bytes()).to_hex().as_str() != hash {
        return err("manifest checksum mismatch");
    }
    let m: Manifest = serde_json::from_str(json)?;
    if m.format != FORMAT {
        return err("unsupported manifest format");
    }
    Ok(m)
}

/// Generation numbers of every `wal-G.nine` file in the directory.
fn wal_files(directory: &Path) -> Result<Vec<u64>> {
    let mut gens = Vec::new();
    for entry in fs::read_dir(directory)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(g) = name
            .strip_prefix("wal-")
            .and_then(|r| r.strip_suffix(".nine"))
        {
            gens.push(g.parse::<u64>()?);
        }
    }
    gens.sort_unstable();
    Ok(gens)
}

fn validate_shape(r: &Record) -> Result<()> {
    r.validate()?;
    if r.embedding.is_some() {
        return err("local Nine does not accept embeddings");
    }
    Ok(())
}

impl State {
    fn end(&self) -> Row {
        self.fresh.end()
    }

    fn segment_of(&self, row: Row) -> Option<&DiskSegment> {
        let i = self.segments.partition_point(|s| s.start <= row);
        let s = self.segments.get(i.checked_sub(1)?)?;
        (row < s.end()).then_some(s)
    }

    #[inline]
    fn meta(&self, row: Row) -> RowMeta {
        if row >= self.fresh.start {
            self.fresh.meta[(row - self.fresh.start) as usize]
        } else {
            let s = self.segment_of(row).expect("row inside a segment");
            s.meta(row - s.start)
        }
    }

    fn record(&self, row: Row) -> Result<Cow<'_, Record>> {
        if row >= self.fresh.start {
            return Ok(Cow::Borrowed(
                &self.fresh.records[(row - self.fresh.start) as usize],
            ));
        }
        let s = self.segment_of(row).ok_or("row outside committed history")?;
        let bytes = s.archive.get(u64::from(row - s.start))?;
        Ok(Cow::Owned(decode_record(&bytes, u64::from(row))?))
    }

    fn rows_with_id(&self, fp: u128) -> Vec<Row> {
        let mut rows = Vec::new();
        for s in &self.segments {
            rows.extend(s.rows_with_id(fp).map(|l| s.start + l));
        }
        if let Some(local) = self.fresh.ids.get(&fp) {
            rows.extend(local.iter().map(|l| self.fresh.start + l));
        }
        rows
    }

    fn rows_with_key(&self, fp: u128) -> Vec<Row> {
        let mut rows = Vec::new();
        for s in &self.segments {
            rows.extend(s.rows_with_key(fp).map(|l| s.start + l));
        }
        if let Some(local) = self.fresh.keys.get(&fp) {
            rows.extend(local.iter().map(|l| self.fresh.start + l));
        }
        rows
    }

    /// The row holding `id`, verified against the decoded original.
    fn find_id(&self, id: &str) -> Result<Option<Row>> {
        for row in self.rows_with_id(fingerprint(id)) {
            if self.record(row)?.id == id {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn id_of(&self, row: Row) -> Result<String> {
        if row < self.fresh.start {
            let s = self.segment_of(row).ok_or("row outside committed history")?;
            if let Some(id) = s.short_id(row - s.start) {
                return Ok(id);
            }
            return s.id_with(row - s.start, || Ok(self.record(row)?.id.clone()));
        }
        Ok(self.record(row)?.id.clone())
    }

    fn version_key(&self, row: Row) -> (i64, i64, Row) {
        let m = self.meta(row);
        (m.valid_at, m.recorded_at, row)
    }

    /// Record `row` (already pushed) as a new version of `existing` rows.
    fn link_versions(&mut self, row: Row, existing: &[Row]) {
        if existing.is_empty() {
            return;
        }
        let group = existing.iter().find_map(|r| self.groups.of.get(r).copied());
        let g = match group {
            Some(g) => g,
            None => {
                let g = self.groups.list.len() as u32;
                let mut rows = existing.to_vec();
                rows.sort_unstable_by_key(|r| self.version_key(*r));
                for r in &rows {
                    self.groups.of.insert(*r, g);
                    self.groups.mark(*r);
                }
                self.groups.list.push(rows);
                g
            }
        };
        let key = self.version_key(row);
        let old_last = *self.groups.list[g as usize].last().expect("group has rows");
        let at = self.groups.list[g as usize].partition_point(|r| self.version_key(*r) <= key);
        self.groups.list[g as usize].insert(at, row);
        self.groups.of.insert(row, g);
        self.groups.mark(row);
        // Exactly one row joins the superseded set: the previous newest
        // version, or the new row itself when it is backdated.
        let newest = *self.groups.list[g as usize].last().expect("group has rows");
        let m = self.meta(if newest == row { old_last } else { row });
        self.superseded.0 += u64::from(m.scope);
        self.superseded.1 += u64::from(m.length);
    }

    /// Total mass of the rows eligible in this view: valid_at in [from,
    /// valid_at], recorded_at <= known_at, newest visible version of its
    /// key. Whole segments use stored totals; only segments straddling a
    /// bound are scanned, and superseded versions are subtracted exactly.
    fn view_mass(&self, q: &Query, from: i64, scope: bool) -> u64 {
        let inside = |m: &RowMeta| {
            m.valid_at >= from && m.valid_at <= q.valid_at && m.recorded_at <= q.known_at
        };
        let mut total = 0u64;
        let mut everything = true;
        let mut visit = |span: &Span, rows: u32, meta: &dyn Fn(u32) -> RowMeta| {
            if rows == 0 {
                return;
            }
            if span.min_valid >= from
                && span.max_valid <= q.valid_at
                && span.max_recorded <= q.known_at
            {
                total += if scope { span.scope } else { span.length };
                return;
            }
            everything = false;
            if span.max_valid < from || span.min_valid > q.valid_at || span.min_recorded > q.known_at {
                return;
            }
            for local in 0..rows {
                let m = meta(local);
                if inside(&m) {
                    total += mass_of(&m, scope);
                }
            }
        };
        for s in &self.segments {
            visit(&s.span, s.rows, &|l| s.meta(l));
        }
        visit(&self.fresh.span, self.fresh.len(), &|l| self.fresh.meta[l as usize]);
        if everything {
            return total - if scope { self.superseded.0 } else { self.superseded.1 };
        }
        for versions in &self.groups.list {
            let end = versions.partition_point(|i| self.meta(*i).valid_at <= q.valid_at);
            let visible = versions[..end]
                .iter()
                .rev()
                .find(|i| self.meta(**i).recorded_at <= q.known_at)
                .copied();
            for r in versions {
                let m = self.meta(*r);
                if Some(*r) != visible && inside(&m) {
                    total -= mass_of(&m, scope);
                }
            }
        }
        total
    }

    /// Occurrences of each term in the view, counted as the background
    /// counts them: records containing it (`scope`) or its total frequency.
    /// Fully visible segments of a store without versions answer from
    /// their dictionary; the rest decode the term's postings and check each
    /// row (as `Background::View` does during its scan).
    fn view_term_counts(&self, terms: &[&str], q: &Query, from: i64, scope: bool) -> Vec<u64> {
        let visible = |span: &Span| {
            span.min_valid >= from && span.max_valid <= q.valid_at && span.max_recorded <= q.known_at
        };
        let unversioned = self.groups.list.is_empty();
        let mut memo = HashMap::new();
        terms
            .iter()
            .map(|term| {
                let mut n = 0u64;
                for s in &self.segments {
                    let Some(info) = s.term(term) else { continue };
                    if unversioned && visible(&s.span) {
                        n += if scope { u64::from(info.df) } else { info.cf };
                        continue;
                    }
                    for (local, tf) in s.postings(info) {
                        let m = s.meta(local);
                        if m.valid_at >= from && self.latest(s.start + local, &m, q, &mut memo) {
                            n += if scope { 1 } else { u64::from(tf) };
                        }
                    }
                }
                if let Some(list) = self.fresh.postings.get(*term) {
                    for &(local, tf) in list {
                        let m = self.fresh.meta[local as usize];
                        if m.valid_at >= from && self.latest(self.fresh.start + local, &m, q, &mut memo) {
                            n += if scope { 1 } else { u64::from(tf) };
                        }
                    }
                }
                n
            })
            .collect()
    }

    /// v3 `latest`: is `row` the visible revision of its key in this view?
    fn latest(
        &self,
        row: Row,
        meta: &RowMeta,
        q: &Query,
        memo: &mut HashMap<u32, Option<Row>>,
    ) -> bool {
        if meta.valid_at > q.valid_at || meta.recorded_at > q.known_at {
            return false;
        }
        let Some(g) = self.groups.group(row) else {
            return true;
        };
        let versions = &self.groups.list[g as usize];
        let newest = *versions.last().expect("group has rows");
        let current = self.meta(newest);
        if current.valid_at <= q.valid_at && current.recorded_at <= q.known_at {
            return newest == row;
        }
        *memo.entry(g).or_insert_with(|| {
            let end = versions.partition_point(|i| self.meta(*i).valid_at <= q.valid_at);
            versions[..end]
                .iter()
                .rev()
                .find(|i| self.meta(**i).recorded_at <= q.known_at)
                .copied()
        }) == Some(row)
    }

    /// Postings of `term` per segment (row base, iterator), in row order.
    fn postings<'a>(&'a self, term: &str) -> Vec<(Row, Postings<'a>)> {
        let mut out = Vec::new();
        for s in &self.segments {
            if let Some(info) = s.term(term) {
                out.push((s.start, s.postings(info)));
            }
        }
        if let Some(list) = self.fresh.postings.get(term) {
            out.push((self.fresh.start, Postings::Fresh(list.iter())));
        }
        out
    }

    fn document_frequency(&self, term: &str) -> u64 {
        let disk: u64 = self
            .segments
            .iter()
            .filter_map(|s| s.term(term))
            .map(|i| u64::from(i.df))
            .sum();
        disk + self.fresh.postings.get(term).map_or(0, |l| l.len() as u64)
    }

    /// Visit rows with valid_at in [from, to] in (valid_at, row) order until
    /// `visit` returns false.
    fn time_rows(
        &self,
        from: i64,
        to: i64,
        mut visit: impl FnMut(Row) -> Result<bool>,
    ) -> Result<()> {
        if from > to {
            return Ok(());
        }
        let mut cursors: Vec<(usize, &DiskSegment)> = self
            .segments
            .iter()
            .map(|s| (s.time_lower_bound(from), s.as_ref()))
            .collect();
        let mut fresh = self
            .fresh
            .time
            .range((from, 0)..=(to, Row::MAX))
            .map(|(_, l)| self.fresh.start + l)
            .peekable();
        loop {
            let mut best: Option<(i64, Row, usize)> = None;
            for (c, (pos, s)) in cursors.iter().enumerate() {
                if *pos < s.rows as usize {
                    let local = s.time_at(*pos);
                    let at = s.meta(local).valid_at;
                    if at <= to && best.is_none_or(|b| (at, s.start + local) < (b.0, b.1)) {
                        best = Some((at, s.start + local, c));
                    }
                }
            }
            if let Some(&row) = fresh.peek() {
                let at = self.meta(row).valid_at;
                if best.is_none_or(|b| (at, row) < (b.0, b.1)) {
                    best = Some((at, row, usize::MAX));
                }
            }
            let Some((_, row, c)) = best else { break };
            if c == usize::MAX {
                fresh.next();
            } else {
                cursors[c].0 += 1;
            }
            if !visit(row)? {
                break;
            }
        }
        Ok(())
    }

    fn has_phrase(&self, row: Row, needle: &str) -> Result<bool> {
        Ok(phrase_start(&self.record(row)?.text, needle).is_some())
    }
}

impl Nine {
    pub fn open(directory: &Path) -> Result<Self> {
        Self::open_with(directory, Options::default())
    }

    pub fn open_with(directory: &Path, options: Options) -> Result<Self> {
        if directory.join("events.nine").exists() && !directory.join("timeline.nine").exists() {
            return err(
                "historical Nine v1 store: use the v1 reader or explicitly import originals",
            );
        }
        fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("LOCK"))?;
        lock.try_lock()?;
        if options.shared_log.is_some() && options.deferred_sync {
            return err("deferred durability is not available with a shared log");
        }
        let created = !directory.join(MANIFEST).exists();
        if created {
            Self::initialize(directory, &options)?;
        }
        Self::load(directory, options, lock, created)
    }

    /// Create an empty v4 store, or migrate a v3 store, then write the
    /// manifest last. Leftover v4 files without a manifest were never
    /// acknowledged and are removed first.
    fn initialize(directory: &Path, options: &Options) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            let leftover = (name.starts_with("wal-") && name.ends_with(".nine"))
                || (name.starts_with("seg-")
                    && (name.ends_with(".idx") || name.ends_with(".src")))
                || name == "outcomes.v4"
                || name == "receipts.v4"
                || name == "manifest.tmp";
            if leftover {
                fs::remove_file(directory.join(name))?;
            }
        }
        let mut manifest = Manifest {
            format: FORMAT.into(),
            archive: options.archive.name().into(),
            segments: Vec::new(),
            next_segment: 1,
            wal_gen: 1,
            rows: 0,
            migrated_from: None,
            shared_log: Binding::of(options),
        };
        let mut outcomes = Journal::create(&directory.join("outcomes.v4"), Kind::Outcomes, 0)?;
        let mut receipts = Journal::create(&directory.join("receipts.v4"), Kind::Receipts, 0)?;
        if directory.join("timeline.nine").exists() {
            let legacy = crate::v3::V3::open(directory)?;
            let (records, legacy_outcomes, legacy_receipts) = legacy.export_all()?;
            drop(legacy);
            let rows = u32::try_from(records.len())?;
            let mut fresh = Fresh::new(0);
            for r in records {
                let analyzed = analyze(&r.text)?;
                let (id_fp, key_fp) = (fingerprint(&r.id), fingerprint(&r.key));
                fresh.push(r, analyzed, true, id_fp, key_fp)?;
            }
            if rows > 0 {
                let multi: Vec<u128> = fresh
                    .keys
                    .iter()
                    .filter(|(_, r)| r.len() > 1)
                    .map(|(fp, _)| *fp)
                    .collect();
                let entry = flush_fresh(directory, options, &fresh, &multi, 1)?;
                manifest.segments.push(entry);
                manifest.next_segment = 2;
                manifest.rows = rows;
            }
            outcomes.append(legacy_outcomes.iter().map(Vec::as_slice), true)?;
            let mut converted = Vec::with_capacity(legacy_receipts.len());
            for json in &legacy_receipts {
                let a: Activity = serde_json::from_slice(json)?;
                let rows = a
                    .ids
                    .iter()
                    .map(|id| {
                        fresh
                            .ids
                            .get(&fingerprint(id))
                            .and_then(|l| l.iter().find(|r| fresh.records[**r as usize].id == *id))
                            .copied()
                            .ok_or("legacy receipt names a missing source")
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                converted.push(encode_receipt(
                    &a.query,
                    a.from,
                    a.at,
                    a.known_at,
                    a.entropy_bits,
                    a.model.as_deref(),
                    &rows,
                ));
            }
            receipts.append(converted.iter().map(Vec::as_slice), true)?;
            let size = fs::metadata(directory.join("timeline.nine"))?.len();
            manifest.migrated_from = Some(serde_json::json!({
                "format": "NINET003", "file": "timeline.nine", "bytes": size, "records": rows,
                "outcomes": legacy_outcomes.len(), "receipts": legacy_receipts.len(),
            }));
        }
        if manifest.shared_log.is_none() {
            Journal::create(
                &directory.join(wal_name(1)),
                Kind::Records,
                u64::from(manifest.rows),
            )?;
        }
        drop((outcomes, receipts));
        write_manifest(directory, &manifest)?;
        // The store's directory entry must be durable before any commit:
        // a shared log may retire rows once this manifest absorbs them.
        sync_dir(&parent_dir(directory))
    }

    fn load(directory: &Path, options: Options, lock: File, created: bool) -> Result<Self> {
        let manifest = read_manifest(directory)?;
        if manifest.archive != options.archive.name() {
            return err("store was written with a different archive codec");
        }
        if manifest.shared_log != Binding::of(&options) {
            return err(match (&manifest.shared_log, &options.shared_log) {
                (Some(_), None) => "store journals into a shared log; open it with that log",
                (None, Some(_)) => "store has a private journal; it cannot join a shared log",
                _ => "store is bound to a different shared log or store id",
            });
        }
        let mut segments = Vec::new();
        let mut expect = 0;
        let mut listed = HashSet::new();
        for e in &manifest.segments {
            if e.start != expect {
                return err("manifest segments are not contiguous");
            }
            let (idx, src) = seg_names(e.id);
            let run = options.archive.open(&directory.join(&src))?;
            let seg = DiskSegment::open(e.id, &directory.join(&idx), run)?;
            if seg.start != e.start || seg.rows != e.rows {
                return err("segment does not match its manifest entry");
            }
            expect = seg.end();
            listed.insert(idx);
            listed.insert(src);
            segments.push(Arc::new(seg));
        }
        if expect != manifest.rows {
            return err("manifest row count mismatch");
        }
        // Segment files absent from the manifest come from an interrupted
        // flush and hold no data that is not also in a journal.
        for entry in fs::read_dir(directory)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("seg-") && !listed.contains(name) {
                fs::remove_file(directory.join(name))?;
            }
        }
        let mut state = State {
            segments,
            fresh: Fresh::new(manifest.rows),
            groups: Groups::default(),
            outcomes: Vec::new(),
            by_query: HashMap::new(),
            outcome_ids: HashSet::new(),
            epoch: 0,
            superseded: (0, 0),
        };
        // Version groups for keys with several rows across segments.
        let mut multi: Vec<u128> = state.segments.iter().flat_map(|s| s.multi_keys()).collect();
        multi.sort_unstable();
        multi.dedup();
        for fp in multi {
            let mut rows = state.rows_with_key(fp);
            if rows.len() > 1 {
                rows.sort_unstable_by_key(|r| state.version_key(*r));
                let g = state.groups.list.len() as u32;
                for r in &rows {
                    state.groups.of.insert(*r, g);
                    state.groups.mark(*r);
                }
                for r in &rows[..rows.len() - 1] {
                    let m = state.meta(*r);
                    state.superseded.0 += u64::from(m.scope);
                    state.superseded.1 += u64::from(m.length);
                }
                state.groups.list.push(rows);
            }
        }
        // Replay journals of rows not yet in segments.
        let mut active: Option<(u64, Journal)> = None;
        let mut sealed = Vec::new();
        let mut attached = None;
        if let Some(binding) = &options.shared_log {
            if !wal_files(directory)?.is_empty() {
                return err("a shared-log store has private journal files");
            }
            let (guard, first, entries) =
                shared::attach(binding, u64::from(manifest.rows), created)?;
            replay_into(&mut state, &entries, first)?;
            attached = Some(guard);
        }
        for generation in wal_files(directory)? {
            let path = directory.join(wal_name(generation));
            if generation < manifest.wal_gen {
                fs::remove_file(&path)?;
                continue;
            }
            if generation > manifest.wal_gen && fs::metadata(&path)?.len() < 32 {
                // Created by an interrupted flush and never used.
                fs::remove_file(&path)?;
                continue;
            }
            let mut pending = Vec::new();
            let journal = Journal::open(&path, Kind::Records, |entry| {
                pending.push(entry.to_vec());
                Ok(())
            })?;
            let first = journal.first_row();
            let skip = u64::from(manifest.rows).saturating_sub(first) as usize;
            replay_into(&mut state, &pending[skip.min(pending.len())..], first + skip as u64)?;
            if let Some((g, _)) = active.replace((generation, journal)) {
                sealed.push(g);
            }
        }
        let (wal_gen, wal) = match active {
            Some((g, wal)) => (g, Some(wal)),
            None if options.shared_log.is_some() => (manifest.wal_gen, None),
            None => return err("store has no active journal"),
        };
        let mut outcomes = Vec::new();
        let outcomes_log = Journal::open(&directory.join("outcomes.v4"), Kind::Outcomes, |e| {
            outcomes.push(serde_json::from_slice::<Outcome>(e)?);
            Ok(())
        })?;
        for o in outcomes {
            let row = validate_outcome(&state, &o)?;
            index_outcome(&mut state, o, row);
        }
        let mut queries = 0usize;
        let end = state.end();
        let receipts = Journal::open(&directory.join("receipts.v4"), Kind::Receipts, |e| {
            check_receipt(e, end)?;
            queries += 1;
            Ok(())
        })?;
        let applied = u64::from(state.end());
        let state = Arc::new(RwLock::new(state));
        let writer = Arc::new(Mutex::new(Writer {
            wal,
            wal_gen,
            sealed,
            manifest,
            poisoned: false,
            unsynced: false,
            next_row: applied,
        }));
        let cache = Arc::new(Mutex::new(Cache::default()));
        let merges = merge::MergeCtx {
            directory: directory.into(),
            options: options.clone(),
            state: Arc::clone(&state),
            writer: Arc::clone(&writer),
            cache: Arc::clone(&cache),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            merging: Arc::new(Mutex::new(())),
        };
        let merge_signal = Arc::new((Mutex::new(true), Condvar::new()));
        let merger = (options.merge_fanout >= 2 && options.background_merges)
            .then(|| {
                let (ctx, signal) = (merges.clone(), Arc::clone(&merge_signal));
                std::thread::Builder::new()
                    .name("nine-merge".into())
                    .spawn(move || merge::run(ctx, signal))
            })
            .transpose()?;
        Ok(Self {
            directory: directory.into(),
            _lock: lock,
            receipts: Mutex::new((options.receipts != Receipts::Off).then_some(receipts)),
            options,
            state,
            writer,
            queue: Mutex::new(Queue::default()),
            outcomes_log: Mutex::new(outcomes_log),
            queries: AtomicUsize::new(queries),
            cache,
            last_metrics: Mutex::new(TimelineMetrics::default()),
            merges,
            merge_signal,
            merger,
            apply: (
                Mutex::new(ApplyOrder {
                    next: applied,
                    failed: false,
                }),
                Condvar::new(),
            ),
            inflight: Mutex::new(HashSet::new()),
            _attached: attached,
        })
    }

    fn read_state(&self) -> Result<std::sync::RwLockReadGuard<'_, State>> {
        self.state.read().map_err(|_| "engine lock poisoned".into())
    }

    fn write_state(&self) -> Result<std::sync::RwLockWriteGuard<'_, State>> {
        self.state.write().map_err(|_| "engine lock poisoned".into())
    }

    /// The simplest write API; returns an immutable local source ID.
    pub fn put(&self, at: i64, text: &str) -> Result<String> {
        Ok(self.put_batch(&[(at, text)])?.remove(0))
    }

    /// Append several texts as one atomic, durable batch (one sync), e.g. a
    /// finished agent turn. Every text is searchable when this returns.
    pub fn put_batch(&self, items: &[(i64, &str)]) -> Result<Vec<String>> {
        let items: Vec<(i64, &str, bool)> = items.iter().map(|(a, t)| (*a, *t, true)).collect();
        self.put_batch_indexed(&items)
    }

    /// Like `put_batch`, choosing per text whether it is indexed for search
    /// (`true`) or stored only (`false`: exact, readable by id and time,
    /// never matched by a query and absent from ranking statistics).
    pub fn put_batch_indexed(&self, items: &[(i64, &str, bool)]) -> Result<Vec<String>> {
        let items = items
            .iter()
            .map(|(at, text, indexed)| {
                let mut record = Record::new("pending", "pending", text, *at);
                record.verified = false;
                Ok(Incoming {
                    analyzed: if *indexed { analyze(text)? } else { Analyzed::none() },
                    record,
                    derived: true,
                    indexed: *indexed,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.submit(items)
    }

    /// Insert one record with a caller-chosen immutable id.
    pub fn insert(&self, record: Record) -> Result<()> {
        self.insert_batch(vec![record]).map(|_| ())
    }

    /// Insert records as one atomic, durable batch; all or none commit.
    pub fn insert_batch(&self, records: Vec<Record>) -> Result<Vec<String>> {
        self.insert_batch_indexed(records.into_iter().map(|r| (r, true)).collect())
    }

    /// Like `insert_batch`, choosing per record whether it is indexed for
    /// search or stored only (see `put_batch_indexed`).
    pub fn insert_batch_indexed(&self, records: Vec<(Record, bool)>) -> Result<Vec<String>> {
        let items = records
            .into_iter()
            .map(|(record, indexed)| {
                Ok(Incoming {
                    analyzed: if indexed {
                        analyze(&record.text)?
                    } else {
                        Analyzed::none()
                    },
                    record,
                    derived: false,
                    indexed,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.submit(items)
    }

    fn submit(&self, items: Vec<Incoming>) -> Result<Vec<String>> {
        self.submit_rows(items).map(|(ids, _)| ids)
    }

    /// Like `insert_batch_indexed`, also returning the first row of the batch;
    /// the batch's records occupy consecutive rows from there. Rows are stable
    /// for the life of the store (merges keep row numbers) and read back with
    /// [`Nine::record_at`] without an id lookup.
    pub fn insert_batch_rows(&self, records: Vec<(Record, bool)>) -> Result<(Vec<String>, u32)> {
        let items = records
            .into_iter()
            .map(|(record, indexed)| {
                Ok(Incoming {
                    analyzed: if indexed {
                        analyze(&record.text)?
                    } else {
                        Analyzed::none()
                    },
                    record,
                    derived: false,
                    indexed,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.submit_rows(items)
    }

    /// Physically erase `rows`: each immutable segment holding one is
    /// rewritten over the same row range with those rows blank (same id, key
    /// and clocks; no text, no metadata, not indexed) and every term only
    /// they used removed; its archive run and dictionary are rebuilt from the
    /// surviving rows and the old files are deleted. Rows still in the fresh
    /// segment are flushed first (which also retires their journal). Row
    /// numbers never change. Returns (segments rewritten, rows erased).
    pub fn erase_rows(&self, rows: &[u32]) -> Result<(usize, usize)> {
        self.rewrite_rows(rows, &HashMap::new())
    }

    /// [`Nine::erase_rows`] plus metadata edits in the same segment
    /// rewrites: a row in `meta` gets exactly that metadata; it keeps its
    /// text and index entry unless it is also in `rows` (then it is blank
    /// apart from that metadata). Ids, keys, clocks and rows never change.
    pub fn rewrite_rows(&self, rows: &[u32], meta: &HashMap<u32, BTreeMap<String, String>>) -> Result<(usize, usize)> {
        let fresh = {
            let st = self.read_state()?;
            rows.iter().chain(meta.keys()).any(|r| *r >= st.fresh.start)
        };
        if fresh {
            self.flush()?;
        }
        self.merges.erase(rows, meta)
    }

    /// The record stored at `row` (any version, superseded or not).
    pub fn record_at(&self, row: u32) -> Result<Record> {
        let st = self.read_state()?;
        if row >= st.end() {
            return err("row outside committed history");
        }
        Ok(st.record(row)?.into_owned())
    }

    fn submit_rows(&self, items: Vec<Incoming>) -> Result<(Vec<String>, Row)> {
        if let Some(binding) = &self.options.shared_log {
            return self.submit_shared(binding, items);
        }
        let slot = Arc::new(Slot::default());
        let lead = {
            let mut q = lock(&self.queue)?;
            q.pending.push(Ticket {
                items,
                slot: Arc::clone(&slot),
            });
            !std::mem::replace(&mut q.leader, true)
        };
        if lead {
            self.lead();
        }
        let mut result = lock(&slot.result)?;
        while result.is_none() {
            result = slot
                .done
                .wait(result)
                .map_err(|_| "engine lock poisoned")?;
        }
        result.take().expect("filled")
    }

    /// Shared-log commit, pipelined per store. Under the writer lock the
    /// batch takes its rows (after every batch already queued), its ids are
    /// checked and it is queued in the log; queue order is row order. The
    /// lock is released while the round syncs, so the store's next batch can
    /// join the same or the next round instead of waiting for this one.
    /// Durable batches are applied in row order, under the writer lock (no
    /// flush runs meanwhile), and are searchable when this returns.
    fn submit_shared(&self, binding: &LogBinding, items: Vec<Incoming>) -> Result<(Vec<String>, Row)> {
        let (pending, ids, rows, first) = {
            let mut w = lock(&self.writer)?;
            if w.poisoned {
                return err("journal I/O previously failed; close and reopen for recovery");
            }
            let first = w.next_row;
            let mut row = first;
            let mut ids = Vec::with_capacity(items.len());
            let mut seen = HashSet::with_capacity(items.len());
            let mut entries = Vec::with_capacity(items.len());
            let mut rows = Vec::with_capacity(items.len());
            for mut item in items {
                if row >= u64::from(Row::MAX) {
                    return err("store row limit reached");
                }
                if item.derived {
                    let id = derived_id(row, item.record.valid_at, &item.record.text);
                    item.record.key = id.clone();
                    item.record.id = id;
                }
                validate_shape(&item.record)?;
                if !seen.insert(item.record.id.clone()) {
                    return err("duplicate immutable timeline source ID");
                }
                let mut bytes = Vec::with_capacity(item.record.text.len() + 64);
                encode_record_with(&item.record, row, item.indexed, &mut bytes);
                if bytes.len() > MAX_FRAME {
                    return err("record exceeds frame limit; split into records");
                }
                entries.push(bytes);
                ids.push(item.record.id.clone());
                rows.push((item.record, item.analyzed, item.indexed));
                row += 1;
            }
            // In flight first, then the state: a batch leaves the in-flight
            // set only once it is visible, and joins it only under this lock.
            let inflight_ids = lock(&self.inflight)?;
            if seen.iter().any(|id| inflight_ids.contains(id)) {
                return err("duplicate immutable timeline source ID");
            }
            drop(inflight_ids);
            {
                let st = self.read_state()?;
                for id in &seen {
                    if st.find_id(id)?.is_some() {
                        return err("duplicate immutable timeline source ID");
                    }
                }
            }
            let mut inflight = lock(&self.inflight)?;
            let pending = binding.log.enqueue(binding.store, first, &entries)?;
            inflight.extend(seen);
            w.next_row = row;
            (pending, ids, rows, first)
        };
        let durable = pending.wait();
        let n = rows.len() as u64;
        let turn = ApplyTurn {
            apply: &self.apply,
            done: false,
        };
        let mut next = turn.order();
        while next.next != first && !next.failed {
            next = self
                .apply
                .1
                .wait(next)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut w = lock(&self.writer)?;
        let applied: Result<()> = if next.failed {
            err("an earlier batch of this store failed; close and reopen for recovery")
        } else {
            durable.and_then(|_| {
                let mut st = self.write_state()?;
                for (record, analyzed, indexed) in rows {
                    apply(&mut st, record, analyzed, indexed)?;
                }
                st.epoch += 1;
                Ok(())
            })
        };
        if applied.is_ok() {
            turn.succeeded(&mut next, first + n);
            drop(next);
        } else {
            drop(next);
            drop(turn);
        }
        if let Ok(mut f) = lock(&self.inflight) {
            for id in &ids {
                f.remove(id);
            }
        }
        self.clear_query_cache();
        if let Err(e) = applied {
            // Rows were taken: the store is consistent again only on reopen.
            w.poisoned = true;
            return Err(e);
        }
        if self.maybe_flush(&mut w).is_err() {
            // The batch is durable; refuse further writes until reopen.
            w.poisoned = true;
        }
        Ok((ids, first as Row))
    }

    /// Commit queued tickets until the queue is empty, one sync per round.
    fn lead(&self) {
        loop {
            let batch = {
                let Ok(mut q) = self.queue.lock() else { return };
                if q.pending.is_empty() {
                    q.leader = false;
                    return;
                }
                std::mem::take(&mut q.pending)
            };
            let slots: Vec<_> = batch.iter().map(|t| Arc::clone(&t.slot)).collect();
            let results =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.commit(batch)))
                    .unwrap_or_else(|_| {
                        if let Ok(mut w) = self.writer.lock() {
                            w.poisoned = true;
                        }
                        slots
                            .iter()
                            .map(|_| err("commit panicked; reopen the store"))
                            .collect()
                    });
            for (slot, result) in slots.iter().zip(results) {
                if let Ok(mut r) = slot.result.lock() {
                    *r = Some(result);
                }
                slot.done.notify_all();
            }
        }
    }

    fn commit(&self, batch: Vec<Ticket>) -> Vec<Result<(Vec<String>, Row)>> {
        let mut w = match self.writer.lock() {
            Ok(w) => w,
            Err(_) => return batch.iter().map(|_| err("engine lock poisoned")).collect(),
        };
        if w.poisoned {
            return batch
                .iter()
                .map(|_| err("journal I/O previously failed; close and reopen for recovery"))
                .collect();
        }
        let mut results: Vec<Result<(Vec<String>, Row)>> = Vec::with_capacity(batch.len());
        // Sequence and validate under a read lock: only this leader mutates
        // the state, so the view cannot change before it is applied.
        let mut accepted: Vec<(usize, Vec<(Record, Analyzed, bool)>)> = Vec::new();
        let mut entries = Vec::new();
        let start;
        {
            let st = match self.read_state() {
                Ok(s) => s,
                Err(_) => return batch.iter().map(|_| err("engine lock poisoned")).collect(),
            };
            let mut next = u64::from(st.end());
            start = next;
            let mut batch_ids = HashSet::new();
            for (t, ticket) in batch.into_iter().enumerate() {
                let mut rows = Vec::new();
                let mut ids = HashSet::new();
                let mut encoded = Vec::new();
                let mut row = next;
                let checked: Result<()> = (|| {
                    for mut item in ticket.items {
                        if row >= u64::from(Row::MAX) {
                            return err("store row limit reached");
                        }
                        if item.derived {
                            let id = derived_id(row, item.record.valid_at, &item.record.text);
                            item.record.key = id.clone();
                            item.record.id = id;
                        }
                        validate_shape(&item.record)?;
                        if batch_ids.contains(&item.record.id)
                            || !ids.insert(item.record.id.clone())
                            || st.find_id(&item.record.id)?.is_some()
                        {
                            return err("duplicate immutable timeline source ID");
                        }
                        let mut bytes = Vec::with_capacity(item.record.text.len() + 64);
                        encode_record_with(&item.record, row, item.indexed, &mut bytes);
                        if bytes.len() > MAX_FRAME {
                            return err("record exceeds frame limit; split into records");
                        }
                        encoded.push(bytes);
                        rows.push((item.record, item.analyzed, item.indexed));
                        row += 1;
                    }
                    Ok(())
                })();
                match checked {
                    Ok(()) => {
                        let first = next as Row;
                        next = row;
                        batch_ids.extend(ids);
                        entries.extend(encoded);
                        results.push(Ok((rows.iter().map(|(r, _, _)| r.id.clone()).collect(), first)));
                        accepted.push((t, rows));
                    }
                    Err(e) => results.push(Err(e)),
                }
            }
        }
        if accepted.is_empty() {
            return results;
        }
        let deferred = self.options.deferred_sync;
        w.unsynced |= deferred;
        // Accepted tickets hold consecutive rows from `start`.
        let written = match (&self.options.shared_log, w.wal.as_mut()) {
            // Not reached: shared-log stores commit through `submit_shared`.
            (Some(b), _) => b.log.enqueue(b.store, start, &entries).and_then(|p| p.wait()),
            (None, Some(wal)) => wal.append(entries.iter().map(Vec::as_slice), !deferred),
            (None, None) => err("store has no journal"),
        };
        if let Err(e) = written {
            w.poisoned = true;
            let message = e.to_string();
            for (t, _) in &accepted {
                results[*t] = Err(message.clone().into());
            }
            return results;
        }
        let applied: Result<()> = (|| {
            let mut st = self.write_state()?;
            for (_, rows) in accepted {
                for (record, analyzed, indexed) in rows {
                    apply(&mut st, record, analyzed, indexed)?;
                }
            }
            st.epoch += 1;
            Ok(())
        })();
        self.clear_query_cache();
        if let Err(e) = applied {
            // Durable but not indexed: the store must be reopened.
            w.poisoned = true;
            let message = e.to_string();
            return results
                .into_iter()
                .map(|r| r.and_then(|_| Err(message.clone().into())))
                .collect();
        }
        if self.maybe_flush(&mut w).is_err() {
            // The batch is durable; refuse further writes until reopen so a
            // half-finished flush can never strand acknowledged rows.
            w.poisoned = true;
        }
        results
    }

    fn maybe_flush(&self, w: &mut Writer) -> Result<()> {
        let due = {
            let st = self.read_state()?;
            st.fresh.text_bytes >= self.options.flush_text_bytes
                || st.fresh.posting_count >= self.options.flush_postings
        };
        if due { self.flush_locked(w) } else { Ok(()) }
    }

    /// Write the fresh segment as an immutable segment now.
    pub fn flush(&self) -> Result<()> {
        let mut w = lock(&self.writer)?;
        if w.poisoned {
            return err("journal I/O previously failed; close and reopen for recovery");
        }
        let flushed = self.flush_locked(&mut w);
        if flushed.is_err() {
            w.poisoned = true;
        }
        flushed
    }

    fn flush_locked(&self, w: &mut Writer) -> Result<()> {
        let id = w.manifest.next_segment;
        let (entry, end) = {
            let st = self.read_state()?;
            if st.fresh.len() == 0 {
                return Ok(());
            }
            let multi: Vec<u128> = st
                .fresh
                .keys
                .iter()
                .filter(|(fp, rows)| {
                    rows.len() > 1
                        || st
                            .segments
                            .iter()
                            .any(|s| s.rows_with_key(**fp).next().is_some())
                })
                .map(|(fp, _)| *fp)
                .collect();
            let entry = flush_fresh(&self.directory, &self.options, &st.fresh, &multi, id)?;
            (entry, st.end())
        };
        let (idx, src) = seg_names(id);
        let run = self.options.archive.open(&self.directory.join(&src))?;
        let segment = DiskSegment::open(id, &self.directory.join(&idx), run)?;
        if let Some(binding) = &self.options.shared_log {
            let mut manifest = w.manifest.clone();
            manifest.segments.push(entry);
            manifest.next_segment = id + 1;
            manifest.rows = end;
            write_manifest(&self.directory, &manifest)?;
            self.install_flushed(segment, end)?;
            w.manifest = manifest;
            // The rows are durable in segments: the shared log may drop them.
            binding.log.absorbed(binding.store, u64::from(end));
            self.signal_merge();
            return Ok(());
        }
        let generation = w.wal_gen + 1;
        let wal = Journal::create(
            &self.directory.join(wal_name(generation)),
            Kind::Records,
            u64::from(end),
        )?;
        let mut manifest = w.manifest.clone();
        manifest.segments.push(entry);
        manifest.next_segment = id + 1;
        manifest.wal_gen = generation;
        manifest.rows = end;
        write_manifest(&self.directory, &manifest)?;
        self.install_flushed(segment, end)?;
        let mut obsolete = Vec::new();
        if let Some(old) = w.wal.replace(wal) {
            obsolete.push(old.path().to_path_buf());
        }
        obsolete.extend(w.sealed.drain(..).map(|g| self.directory.join(wal_name(g))));
        w.wal_gen = generation;
        w.manifest = manifest;
        for path in obsolete {
            fs::remove_file(path)?;
        }
        sync_dir(&self.directory)?;
        self.signal_merge();
        Ok(())
    }

    /// Make a just-flushed segment visible and start an empty fresh segment.
    fn install_flushed(&self, segment: DiskSegment, end: Row) -> Result<()> {
        {
            let mut st = self.write_state()?;
            st.segments.push(Arc::new(segment));
            st.fresh = Fresh::new(end);
            st.epoch += 1;
        }
        self.clear_query_cache();
        Ok(())
    }

    fn signal_merge(&self) {
        let (due, wake) = &*self.merge_signal;
        if let Ok(mut due) = due.lock() {
            *due = true;
            wake.notify_one();
        }
    }

    /// Run every due segment merge now, on this thread. Returns how many.
    /// Major compaction: rewrite every immutable segment as one (its
    /// originals become one archive run; a single segment is rewritten
    /// too). Commits continue meanwhile.
    pub fn compact_all(&self) -> Result<bool> {
        self.merges.merge_all()
    }

    pub fn compact(&self) -> Result<usize> {
        if self.options.merge_fanout < 2 {
            return Ok(0);
        }
        self.merges.compact()
    }

    /// Make buffered query receipts durable.
    /// Make every acknowledged batch durable (deferred durability only;
    /// a no-op otherwise). Returns whether a sync was needed.
    pub fn sync_journal(&self) -> Result<bool> {
        let mut w = lock(&self.writer)?;
        if !w.unsynced {
            return Ok(false);
        }
        if let Some(wal) = w.wal.as_mut() {
            wal.sync()?;
        }
        w.unsynced = false;
        Ok(true)
    }

    pub fn sync(&self) -> Result<()> {
        self.sync_journal()?;
        if let Some(j) = lock(&self.receipts)?.as_mut() {
            j.sync()?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.read_state().map_or(0, |s| s.end() as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read(&self, id: &str) -> Result<Option<String>> {
        let st = self.read_state()?;
        match st.find_id(id)? {
            Some(row) => Ok(Some(st.record(row)?.into_owned().text)),
            None => Ok(None),
        }
    }

    pub fn record(&self, id: &str) -> Result<Option<Record>> {
        let st = self.read_state()?;
        match st.find_id(id)? {
            Some(row) => Ok(Some(st.record(row)?.into_owned())),
            None => Ok(None),
        }
    }

    /// Query-addressed exact bytes inside an immutable original.
    pub fn passage(&self, id: &str, query: &str, max_bytes: usize) -> Result<Option<Passage>> {
        let Some(text) = self.read(id)? else {
            return Ok(None);
        };
        Ok(extract_passage(&text, query, max_bytes)?.map(|s| Passage {
            start: s.start,
            end: s.end,
            text: s.text.to_owned(),
        }))
    }

    /// Originals in append order, including superseded versions, starting
    /// at `cursor`. Callers must authorize the owning database first.
    pub fn records_from(&self, cursor: usize, limit: usize) -> Result<Vec<Record>> {
        let st = self.read_state()?;
        let end = (st.end() as usize).min(cursor.saturating_add(limit));
        (cursor..end)
            .map(|row| Ok(st.record(row as Row)?.into_owned()))
            .collect()
    }

    /// Inclusive event-time interval, returning exact originals in time order.
    pub fn range(&self, from: i64, to: i64, limit: usize) -> Result<Vec<Record>> {
        if from > to || limit > 10_000 {
            return err("invalid time range or result limit");
        }
        let st = self.read_state()?;
        let mut out = Vec::new();
        st.time_rows(from, to, |row| {
            if out.len() == limit {
                return Ok(false);
            }
            out.push(st.record(row)?.into_owned());
            Ok(true)
        })?;
        Ok(out)
    }

    /// The best `limit` matches for `text` among records valid in
    /// `[from, to]` (and recorded by `to`). Unlike a [`Query`], `limit` is
    /// not capped at 10,000: callers that filter matches outside the engine
    /// ask for more until they have enough or the engine has no more.
    pub fn find(&self, text: &str, from: i64, to: i64, limit: usize) -> Result<Matches> {
        let mut q = Query::new(text, to);
        q.k = limit;
        self.search_any_k(&q, from)
    }

    pub fn metrics(&self) -> TimelineMetrics {
        self.last_metrics
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default()
    }

    /// Administrative memory release; also permits measuring uncached retrieval.
    pub fn clear_query_cache(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.entries.clear();
            c.bytes = 0;
        }
    }

    pub fn stats(&self) -> Result<TimelineStats> {
        let st = self.read_state()?;
        let mut vocabulary = HashSet::new();
        for s in &st.segments {
            s.for_each_term(|t, _| {
                if !vocabulary.contains(t) {
                    vocabulary.insert(t.to_vec());
                }
            })?;
        }
        for t in st.fresh.postings.keys() {
            if !vocabulary.contains(t.as_bytes()) {
                vocabulary.insert(t.as_bytes().to_vec());
            }
        }
        let cache = lock(&self.cache)?;
        Ok(TimelineStats {
            records: st.end() as usize,
            raw_text_bytes: (st.segments.iter().map(|s| s.total_text).sum::<u64>()
                + st.fresh.text_bytes as u64) as usize,
            archive_bytes: self.disk_bytes()?,
            delta_records: 0,
            vocabulary: vocabulary.len(),
            indexed_postings: (st.segments.iter().map(|s| s.total_scope).sum::<u64>()
                + st.fresh.posting_count as u64) as usize,
            persisted_queries: self.queries.load(Ordering::Relaxed),
            cache_entries: cache.entries.len(),
            cache_bytes_estimate: cache.bytes,
            learned_outcomes: st.outcomes.len(),
        })
    }

    /// Every file of the store, for quota accounting and backups.
    pub fn files(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let ours = name == MANIFEST
                || name == "outcomes.v4"
                || name == "receipts.v4"
                || (name.starts_with("wal-") && name.ends_with(".nine"))
                || (name.starts_with("seg-")
                    && (name.ends_with(".idx") || name.ends_with(".src")));
            if ours {
                out.push(entry.path());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Total bytes of every v4 file of this store.
    pub fn disk_bytes(&self) -> Result<u64> {
        self.files()?
            .iter()
            .try_fold(0u64, |n, p| Ok(n + fs::metadata(p)?.len()))
    }

    /// Copy the store at one committed boundary into a new directory that
    /// opens as the same database. Writes wait while it runs.
    /// A shared-log store is flushed first and its copy gets a private
    /// journal (a snapshot never depends on the shared log).
    pub fn snapshot(&self, destination: &Path) -> Result<()> {
        fs::create_dir(destination)?;
        let mut w = lock(&self.writer)?;
        if self.options.shared_log.is_some() {
            if w.poisoned {
                return err("journal I/O previously failed; close and reopen for recovery");
            }
            if let Err(e) = self.flush_locked(&mut w) {
                w.poisoned = true;
                return Err(e);
            }
        }
        let mut receipts = lock(&self.receipts)?;
        let mut outcomes = lock(&self.outcomes_log)?;
        let st = self.read_state()?;
        let copy_file = |name: &str| -> Result<()> {
            let mut out = create_private(&destination.join(name))?;
            std::io::copy(&mut File::open(self.directory.join(name))?, &mut out)?;
            out.sync_all()?;
            Ok(())
        };
        for s in &st.segments {
            let (idx, src) = seg_names(s.id);
            copy_file(&idx)?;
            copy_file(&src)?;
        }
        for g in &w.sealed {
            copy_file(&wal_name(*g))?;
        }
        let copy_journal = |name: &str, j: &mut Journal| -> Result<()> {
            let mut out = create_private(&destination.join(name))?;
            j.copy_committed(&mut out)?;
            out.sync_all()?;
            Ok(())
        };
        let active = wal_name(w.wal_gen);
        let rows = u64::from(w.manifest.rows);
        match w.wal.as_mut() {
            Some(wal) => copy_journal(&active, wal)?,
            // Flushed above: every row is in a segment.
            None => drop(Journal::create(&destination.join(&active), Kind::Records, rows)?),
        }
        copy_journal("outcomes.v4", &mut outcomes)?;
        match receipts.as_mut() {
            Some(j) => copy_journal("receipts.v4", j)?,
            None => copy_file("receipts.v4")?,
        }
        let mut manifest = w.manifest.clone();
        manifest.shared_log = None;
        write_manifest(destination, &manifest)?;
        sync_dir(destination)
    }

    /// Kept for callers of the v3 API; the snapshot is a complete v4 store.
    pub fn snapshot_journals(&self, destination: &Path) -> Result<()> {
        self.snapshot(destination)
    }

    /// Only explicit, caller-verified source usefulness is learned. Source text,
    /// verification flags and timestamps are never promoted or rewritten.
    pub fn record_outcome(
        &self,
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
        let mut log = lock(&self.outcomes_log)?;
        let row = validate_outcome(&*self.read_state()?, &outcome)?;
        log.append([serde_json::to_vec(&outcome)?.as_slice()], true)?;
        let mut st = self.write_state()?;
        index_outcome(&mut st, outcome, row);
        st.epoch += 1;
        drop(st);
        self.clear_query_cache();
        Ok(())
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

    fn persist_query(&self, q: &Query, from: i64, result: &Matches, rows: &[Row]) -> Result<()> {
        if self.options.receipts == Receipts::Off {
            return Ok(());
        }
        let receipt = encode_receipt(
            blake3::hash(q.text.as_bytes()).as_bytes(),
            from,
            q.valid_at,
            q.known_at,
            result.routing_entropy_bits,
            Some(&self.options.model()),
            rows,
        );
        if let Some(j) = lock(&self.receipts)?.as_mut() {
            j.append(
                [receipt.as_slice()],
                self.options.receipts == Receipts::Durable,
            )?;
        }
        self.queries.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Complete local query path, including the query receipt.
    pub fn search_refs(&self, q: &Query, from: i64) -> Result<Matches> {
        q.validate()?;
        self.search_any_k(q, from)
    }

    /// [`Self::search_refs`] for any `q.k`.
    fn search_any_k(&self, q: &Query, from: i64) -> Result<Matches> {
        if q.embedding.is_some() {
            return err("local Nine queries do not accept embeddings");
        }
        if from > q.valid_at || q.text.len() > self.options.max_query_bytes {
            return err("invalid query interval or oversized query");
        }
        let total_lap = crate::profile::start();
        let key = Self::cache_key(q, from);
        let (result, metrics, rows) = {
            let st = self.read_state()?;
            let hit = {
                let mut c = lock(&self.cache)?;
                c.clock += 1;
                let clock = c.clock;
                match c.entries.get_mut(&key) {
                    Some(e) if e.epoch == st.epoch => {
                        e.used = clock;
                        let mut m = e.metrics.clone();
                        m.cache_hit = true;
                        m.scored_records = 0;
                        m.posted_rows_visited = 0;
                        Some((e.result.clone(), m, e.rows.clone()))
                    }
                    _ => None,
                }
            };
            match hit {
                Some(h) => h,
                None => {
                    let (result, metrics, rows) = rank_continued(&st, q, from, &self.options)?;
                    self.cache_result(key, st.epoch, &result, &metrics, &rows);
                    (result, metrics, rows)
                }
            }
        };
        self.persist_query(q, from, &result, &rows)?;
        if let Ok(mut m) = self.last_metrics.lock() {
            *m = metrics;
        }
        crate::profile::since(&total_lap, crate::profile::SEARCH);
        Ok(result)
    }

    fn cache_result(
        &self,
        key: [u8; 32],
        epoch: u64,
        result: &Matches,
        metrics: &TimelineMetrics,
        rows: &[Row],
    ) {
        let bytes = result
            .matches
            .iter()
            .map(|m| m.id.len() + 96)
            .sum::<usize>()
            + 1024;
        if bytes > CACHE_BYTES {
            return;
        }
        let Ok(mut c) = self.cache.lock() else { return };
        // The caller holds the state read lock, so `epoch` is current.
        c.entries.retain(|_, e| e.epoch == epoch);
        c.bytes = c.entries.values().map(|e| e.bytes).sum();
        while c.entries.len() >= CACHE_ENTRIES || c.bytes + bytes > CACHE_BYTES {
            let Some(old) = c
                .entries
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(v) = c.entries.remove(&old) {
                c.bytes -= v.bytes;
            }
        }
        c.bytes += bytes;
        let used = c.clock;
        c.entries.insert(
            key,
            Cached {
                epoch,
                result: result.clone(),
                rows: rows.to_vec(),
                metrics: metrics.clone(),
                bytes,
                used,
            },
        );
    }
}

impl Drop for Nine {
    fn drop(&mut self) {
        let _ = self.sync();
        self.merges.stop.store(true, Ordering::Relaxed);
        let (due, wake) = &*self.merge_signal;
        if let Ok(_guard) = due.lock() {
            wake.notify_all();
        }
        if let Some(t) = self.merger.take() {
            let _ = t.join();
        }
    }
}

/// Write `fresh` as segment `id` (index + archive) and return its entry.
fn flush_fresh(
    directory: &Path,
    options: &Options,
    fresh: &Fresh,
    multi: &[u128],
    id: u64,
) -> Result<SegmentEntry> {
    let (idx, src) = seg_names(id);
    let src_path = directory.join(&src);
    let encode = |i: usize| {
        let r = &fresh.records[i];
        let mut b = Vec::with_capacity(r.text.len() + 64);
        let indexed = fresh.meta[i].flags & ROW_UNINDEXED == 0;
        encode_record_with(r, u64::from(fresh.start) + i as u64, indexed, &mut b);
        b
    };
    let info = options
        .archive
        .write(&src_path, &mut (0..fresh.records.len()).map(encode))?;
    if info.rows != u64::from(fresh.len()) {
        return err("archive wrote a different row count");
    }
    // Verify every original byte-for-byte before the manifest refers to it.
    let run = options.archive.open(&src_path)?;
    let mut checked = 0usize;
    run.scan(0, run.len(), &mut |i, bytes| {
        if blake3::hash(bytes) != blake3::hash(&encode(i as usize)) {
            return err("archive readback differs from the original");
        }
        checked += 1;
        Ok(())
    })?;
    drop(run);
    if checked != fresh.records.len() {
        return err("archive readback is incomplete");
    }
    let mut w = SegmentWriter::create(&directory.join(&idx), fresh.start)?;
    w.rows(fresh.meta.iter().copied())?;
    let sorted = |fps: &[u128]| {
        let mut pairs: Vec<(u128, u32)> = fps.iter().copied().zip(0u32..).collect();
        pairs.sort_unstable();
        pairs
    };
    w.ids(sorted(&fresh.id_fps))?;
    w.keys(sorted(&fresh.key_fps))?;
    w.time(fresh.time.iter().map(|(_, local)| *local))?;
    let mut multi = multi.to_vec();
    multi.sort_unstable();
    multi.dedup();
    w.multi(multi)?;
    let mut terms: Vec<(&Box<str>, &Vec<(Row, u32)>)> = fresh.postings.iter().collect();
    terms.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (t, postings) in terms {
        w.term(t.as_bytes(), postings.iter().copied())?;
    }
    w.finish()?;
    sync_dir(directory)?;
    Ok(SegmentEntry {
        id,
        start: fresh.start,
        rows: fresh.len(),
        level: 0,
    })
}

/// Replay journal entries for rows `first..` into the state: decoding and
/// tokenizing are pure and spread over threads, then rows are applied in
/// order exactly as the original commits did. Entries must continue the
/// state's rows without a gap.
fn replay_into(state: &mut State, entries: &[Vec<u8>], first: u64) -> Result<()> {
    let decoded = replay_parallel(entries, first)?;
    for (i, (record, analyzed, indexed)) in decoded.into_iter().enumerate() {
        if first + i as u64 != u64::from(state.end()) {
            return err("journal rows are not contiguous");
        }
        validate_shape(&record)?;
        if state.find_id(&record.id)?.is_some() {
            return err("duplicate immutable timeline source ID");
        }
        apply(state, record, analyzed, indexed)?;
    }
    Ok(())
}

/// The directory holding `path` (for syncing its entry).
fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Decode and analyze journal entries for rows `first..` on all cores.
fn replay_parallel(entries: &[Vec<u8>], first: u64) -> Result<Vec<(Record, Analyzed, bool)>> {
    let one = |i: usize| -> Result<(Record, Analyzed, bool)> {
        let (record, indexed) = decode_record_with(&entries[i], first + i as u64)?;
        let analyzed = if indexed {
            analyze(&record.text)?
        } else {
            Analyzed::none()
        };
        Ok((record, analyzed, indexed))
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    if entries.len() < 256 || threads < 2 {
        return (0..entries.len()).map(one).collect();
    }
    let per = entries.len().div_ceil(threads);
    let parts: Vec<Result<Vec<_>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let one = &one;
                scope.spawn(move || {
                    (t * per..((t + 1) * per).min(entries.len()))
                        .map(one)
                        .collect::<Result<Vec<_>>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| err("journal replay worker panicked")))
            .collect()
    });
    let mut out = Vec::with_capacity(entries.len());
    for part in parts {
        out.extend(part?);
    }
    Ok(out)
}

/// Index one committed record into the fresh segment and version groups.
fn apply(st: &mut State, record: Record, analyzed: Analyzed, indexed: bool) -> Result<()> {
    let row = st.end();
    let id_fp = fingerprint(&record.id);
    let key_fp = fingerprint(&record.key);
    let existing = st.rows_with_key(key_fp);
    st.fresh.push(record, analyzed, indexed, id_fp, key_fp)?;
    st.link_versions(row, &existing);
    Ok(())
}

fn validate_outcome(st: &State, o: &Outcome) -> Result<Row> {
    let row = st
        .find_id(&o.source)?
        .ok_or("outcome source does not exist")?;
    if o.verifier.is_empty()
        || o.verifier.len() > 1024
        || o.at < st.meta(row).recorded_at
        || st.outcome_ids.contains(&o.id)
    {
        return err("invalid, premature or duplicate outcome receipt");
    }
    Ok(row)
}

fn index_outcome(st: &mut State, o: Outcome, row: Row) {
    st.outcome_ids.insert(o.id.clone());
    st.by_query
        .entry(o.query)
        .or_default()
        .push(st.outcomes.len());
    st.outcomes.push(LiveOutcome { outcome: o, row });
}

/// Per-chunk row states as bitsets (one bit per chunk row, 8 KiB each for a
/// full chunk, so they stay in L1/L2): candidate, evaluated, in the view.
/// Reused across queries on a thread; never sized to the whole store.
/// Candidates come out in row order by walking the candidate bits.
struct Scratch {
    cand: Vec<u64>,
    seen: Vec<u64>,
    view: Vec<u64>,
}

impl Scratch {
    fn begin(&mut self, rows: usize) {
        let words = rows.div_ceil(64);
        for b in [&mut self.cand, &mut self.seen, &mut self.view] {
            b.clear();
            b.resize(words, 0);
        }
    }
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = const {
        RefCell::new(Scratch { cand: Vec::new(), seen: Vec::new(), view: Vec::new() })
    };
}

#[derive(Clone, Copy)]
struct Candidate {
    row: Row,
    meta: RowMeta,
}

struct Scored {
    row: Row,
    meta: RowMeta,
    score: f64,
    conditional: f64,
}

/// Rows per independently scanned range of a segment (tiny under test, so
/// small fixtures exercise multi-chunk segments and resumed cursors).
#[cfg(not(test))]
const CHUNK_ROWS: u32 = 1 << 16;
#[cfg(test)]
const CHUNK_ROWS: u32 = 64;
/// Posting volume above which a query is scanned on several threads.
#[cfg(not(test))]
const PARALLEL_POSTINGS: u64 = 200_000;
#[cfg(test)]
const PARALLEL_POSTINGS: u64 = 64;

/// Where one query term's postings live in one segment.
#[derive(Clone, Copy)]
enum TermSource<'a> {
    Disk(List<'a>, crate::segment::TermInfo),
    Fresh(&'a [(Row, u32)]),
}

struct Chunk<'a> {
    /// Index of the segment in the scan (chunks of one segment are adjacent).
    segment: usize,
    /// Every row of the segment lies inside the query's time view, so only
    /// version groups can make a row ineligible (no row metadata is read
    /// while scanning).
    visible: bool,
    base: Row,
    lo: u32,
    hi: u32,
    disk: Option<&'a DiskSegment>,
    sources: &'a [Option<TermSource<'a>>],
}

/// Where each term's disk postings stopped in the previous chunk a worker
/// scanned: the cursor before the first posting at or past that chunk's end. A worker that moves on to a later chunk of the same
/// segment resumes there instead of decoding the list from its start.
#[derive(Default)]
struct Resume {
    segment: usize,
    hi: u32,
    at: Vec<Option<Cursor>>,
}

struct ChunkOut {
    /// Row of offset 0 for `hits` and `cand`.
    first_row: Row,
    /// Candidate rows in canonical order (row order for scanned chunks).
    rows: Vec<Row>,
    /// (scope, length) of each candidate.
    mass: Vec<(u32, u32)>,
    /// Candidate bits of a scanned chunk and the number of set bits before
    /// each word: a hit names its candidate by its rank among the bits.
    cand: Vec<u64>,
    rank: Vec<u32>,
    /// (row offset in the chunk, tf), grouped in one run per term.
    hits: Vec<(u32, u32)>,
    /// (term index, start, end) of each non-empty run in `hits`, by term.
    runs: Vec<(u32, u32, u32)>,
    view_counts: Vec<usize>,
    visited: usize,
}

impl ChunkOut {
    /// Unscanned candidates (key addressing, time scans): no hits.
    fn listed(candidates: Vec<Candidate>) -> Self {
        Self {
            first_row: 0,
            rows: candidates.iter().map(|c| c.row).collect(),
            mass: candidates.iter().map(|c| (c.meta.scope, c.meta.length)).collect(),
            cand: Vec::new(),
            rank: Vec::new(),
            hits: Vec::new(),
            runs: Vec::new(),
            view_counts: Vec::new(),
            visited: 0,
        }
    }
    /// The candidate index of a hit at chunk row offset `at`.
    #[inline]
    fn index(&self, at: u32) -> usize {
        let (w, b) = ((at / 64) as usize, at % 64);
        (self.rank[w] + (self.cand[w] & ((1u64 << b) - 1)).count_ones()) as usize
    }
}

/// Decode state at the block holding the first posting at or after row
/// `lo` (or the last block before it).
fn skip_start(list: &List<'_>, skips: &[crate::segment::Skip], lo: u32) -> Cursor {
    let b = skips.partition_point(|s| s.first_row <= lo).saturating_sub(1);
    skips.get(b).map_or(list.start(), |s| s.at)
}

/// Postings at the candidate rows of one chunk (bits of `cand`, offsets
/// from `lo`), in row order, found through the list's skip table: the same
/// (offset, tf) hits a full decode of the chunk keeps. Returns the number of
/// postings decoded.
fn lookup_postings(
    list: &List<'_>,
    skips: &[crate::segment::Skip],
    lo: u32,
    cand: &[u64],
    hits: &mut Vec<(u32, u32)>,
) -> usize {
    let mut c = list.start();
    let mut decoded = 0usize;
    // The last decoded posting (row, tf); None before the first.
    let mut current: Option<(u32, u32)> = None;
    for (w, word) in cand.iter().enumerate() {
        let mut bits = *word;
        while bits != 0 {
            let at = (w * 64) as u32 + bits.trailing_zeros();
            bits &= bits - 1;
            let target = lo + at;
            if let Some((r, tf)) = current {
                if r >= target {
                    if r == target {
                        hits.push((at, tf));
                    }
                    continue;
                }
            }
            // Jump forward to the block holding the target, if later.
            let b = skips.partition_point(|s| s.first_row <= target);
            if b == 0 {
                continue;
            }
            let s = &skips[b - 1];
            if s.at.pos > c.pos || current.is_none() {
                c = s.at;
            }
            current = None;
            while let Some((row, tf)) = list.next(&mut c) {
                decoded += 1;
                if row >= target {
                    current = Some((row, tf));
                    break;
                }
            }
            match current {
                Some((r, tf)) if r == target => hits.push((at, tf)),
                Some(_) => {}
                None => return decoded,
            }
        }
    }
    decoded
}

/// Decode one term's disk postings from `start` and visit those in
/// [lo, hi); returns where the next chunk resumes (the cursor before the
/// first posting at or past `hi`, or the end).
#[inline(always)]
fn scan_postings(
    list: &List<'_>,
    start: Cursor,
    lo: u32,
    hi: u32,
    skipped: &mut u64,
    mut visit: impl FnMut(u32, u32) -> Result<()>,
) -> Result<Cursor> {
    let mut c = start;
    let leb = list.leb();
    loop {
        // Version 1 dense lists: four one-byte (gap, tf) pairs per load.
        if let Some(buf) = leb {
            if !c.first && c.row <= u32::MAX - 512 {
                if let Some(word) = buf.get(c.pos..c.pos + 8) {
                    let word = u64::from_le_bytes(word.try_into().expect("8 bytes"));
                    if word & 0x8080_8080_8080_8080 == 0 {
                        for i in 0..4 {
                            let save = c;
                            c.row += (word >> (16 * i)) as u8 as u32;
                            let tf = (word >> (16 * i + 8)) as u8 as u32;
                            c.pos += 2;
                            if c.row < lo {
                                *skipped += 1;
                                continue;
                            }
                            if c.row >= hi {
                                return Ok(save);
                            }
                            visit(c.row, tf)?;
                        }
                        continue;
                    }
                }
            }
        }
        let save = c;
        // CRC-checked at open; a malformed stream ends the list.
        let Some((row, tf)) = list.next(&mut c) else {
            return Ok(list.end());
        };
        if row < lo {
            *skipped += 1;
            continue;
        }
        if row >= hi {
            return Ok(save);
        }
        visit(row, tf)?;
    }
}

struct ScanContext<'a> {
    st: &'a State,
    q: &'a Query,
    from: i64,
    literal: Option<&'a str>,
    first_pass: &'a [bool],
    whole_view: bool,
    scope_model: bool,
    admitted: &'a [Row],
}

impl ScanContext<'_> {
    /// Candidate discovery and per-term frequencies for one row range.
    fn chunk(&self, c: &Chunk<'_>, resume: &mut Resume) -> Result<ChunkOut> {
        let st = self.st;
        let meta = |local: u32| match c.disk {
            Some(s) => s.meta(local),
            None => st.fresh.meta[local as usize],
        };
        let mut memo = HashMap::new();
        let in_view = |row: Row, m: &RowMeta, memo: &mut HashMap<u32, Option<Row>>| {
            m.valid_at >= self.from && st.latest(row, m, self.q, memo)
        };
        let terms = self.first_pass.len();
        let mut out = ChunkOut {
            first_row: c.base + c.lo,
            rows: Vec::new(),
            mass: Vec::new(),
            cand: Vec::new(),
            rank: Vec::new(),
            hits: Vec::new(),
            runs: Vec::new(),
            view_counts: if self.whole_view { vec![0; terms] } else { Vec::new() },
            visited: 0,
        };
        let resumable = resume.segment == c.segment && resume.hi <= c.lo && resume.at.len() == terms;
        // The previous chunk this worker scanned ends where this one starts.
        let adjacent = resumable && resume.hi == c.lo;
        if !resumable {
            resume.at.clear();
            resume.at.resize(terms, None);
        }
        resume.segment = c.segment;
        resume.hi = c.hi;
        let (lo, hi) = (c.lo, c.hi);
        let (whole_view, scope_model) = (self.whole_view, self.scope_model);
        // Every posting row is eligible in a fully visible chunk of a store
        // without version groups; with no literal and no whole-view counts,
        // the first pass makes every posting a candidate hit and the second
        // keeps the postings of candidates (both loops branch-free).
        let simple = c.visible && st.groups.list.is_empty() && self.literal.is_none() && !whole_view;
        let mut skipped = 0u64;
        let mut first_touch = 0u64;
        let mut cands: Option<u64> = None;
        SCRATCH.with(|cell| -> Result<()> {
            let mut guard = cell.borrow_mut();
            let sc = &mut *guard;
            sc.begin((hi - lo) as usize);
            let first = self.admitted.partition_point(|r| *r < c.base + lo);
            for row in &self.admitted[first..] {
                if *row >= c.base + hi {
                    break;
                }
                let at = row - c.base - lo;
                let (w, bit) = ((at / 64) as usize, 1u64 << (at % 64));
                sc.cand[w] |= bit;
                sc.seen[w] |= bit;
                sc.view[w] |= bit;
            }
            let mut lap = crate::profile::start();
            for pass in [true, false] {
                for (ti, source) in c.sources.iter().enumerate() {
                    if self.first_pass[ti] != pass {
                        continue;
                    }
                    let Some(source) = source else { continue };
                    let start = out.hits.len();
                    let resume_at = match (source, resume.at[ti]) {
                        (_, Some(s)) if adjacent => s,
                        // A cached skip table starts the decode at lo's block
                        // (or the saved cursor of an earlier chunk, if later).
                        (TermSource::Disk(list, info), saved) => {
                            let saved = saved.filter(|_| resumable);
                            let skip = c.disk.and_then(|d| d.cached_skips(*info)).map(|k| skip_start(list, &k, lo));
                            match (saved, skip) {
                                (Some(a), Some(b)) => if b.pos > a.pos { b } else { a },
                                (Some(a), None) => a,
                                (None, Some(b)) => b,
                                (None, None) => list.start(),
                            }
                        }
                        (TermSource::Fresh(_), _) => Cursor { pos: 0, row: 0, first: true, left: 0 },
                    };
                    // Candidate-driven second pass: few candidates against a
                    // long list are looked up through its skip table (built
                    // once per segment and term, kept across queries) instead
                    // of decoding every posting in the chunk.
                    if !pass && !whole_view {
                        if let (TermSource::Disk(list, info), Some(d)) = (source, c.disk) {
                            if cands.is_none() {
                                cands = Some(sc.cand.iter().map(|w| w.count_ones() as u64).sum());
                            }
                            let est = u64::from(info.df) * u64::from(hi - lo) / u64::from(d.rows.max(1));
                            // Under test, alternate terms take each path.
                            let factor = if cfg!(test) { (ti as u64 % 2) * 1_000_000 } else { 32 };
                            let skips = (info.df >= crate::segment::SKIP_MIN
                                && cands.unwrap_or(0) * factor < est)
                                .then(|| d.skips(*info))
                                .flatten();
                            if let Some(skips) = skips {
                                let visited = lookup_postings(list, &skips, lo, &sc.cand, &mut out.hits);
                                out.visited += visited;
                                resume.at[ti] = None;
                                if out.hits.len() > start {
                                    out.runs.push((ti as u32, start as u32, out.hits.len() as u32));
                                }
                                continue;
                            }
                        }
                    }
                    if simple {
                        // At most one posting per row of the chunk, and every
                        // posting takes at least two bytes.
                        let bound = match source {
                            TermSource::Disk(list, _) => (list.df as usize).min((hi - lo) as usize),
                            TermSource::Fresh(list) => list.len().min((hi - lo) as usize),
                        };
                        out.hits.reserve(bound + 1);
                        let spare = out.hits.spare_capacity_mut();
                        let mut n = 0usize;
                        let mut visited = 0usize;
                        let cand = &mut sc.cand;
                        // Pass 1: every posting is a candidate hit. Pass 2:
                        // write the hit, keep it only if the row is one.
                        let mut visit = |local: u32, tf: u32| -> Result<()> {
                            let at = local - lo;
                            let (w, b) = ((at / 64) as usize, at % 64);
                            spare[n].write((at, tf));
                            if pass {
                                cand[w] |= 1 << b;
                                n += 1;
                            } else {
                                n += ((cand[w] >> b) & 1) as usize;
                            }
                            visited += 1;
                            Ok(())
                        };
                        match source {
                            TermSource::Disk(list, _) => {
                                resume.at[ti] =
                                    Some(scan_postings(list, resume_at, lo, hi, &mut skipped, &mut visit)?);
                            }
                            TermSource::Fresh(list) => {
                                let a = list.partition_point(|(r, _)| *r < lo);
                                let b = list.partition_point(|(r, _)| *r < hi);
                                for (local, tf) in &list[a..b] {
                                    visit(*local, *tf)?;
                                }
                            }
                        }
                        // SAFETY: the first `n` spare slots were written above.
                        unsafe { out.hits.set_len(start + n) };
                        out.visited += visited;
                    } else {
                        let weight = |tf: u32| if scope_model { 1 } else { tf as usize };
                        // One posting: candidates get a hit; unseen rows are
                        // evaluated once (candidate, in view only, or rejected).
                        let mut visit = |local: u32, tf: u32| -> Result<()> {
                            out.visited += 1;
                            let at = local - lo;
                            let (w, bit) = ((at / 64) as usize, 1u64 << (at % 64));
                            if sc.cand[w] & bit != 0 {
                                if whole_view {
                                    out.view_counts[ti] += weight(tf);
                                }
                                out.hits.push((at, tf));
                                return Ok(());
                            }
                            if sc.seen[w] & bit != 0 {
                                if whole_view && sc.view[w] & bit != 0 {
                                    out.view_counts[ti] += weight(tf);
                                }
                                return Ok(());
                            }
                            if !(pass || whole_view) {
                                return Ok(());
                            }
                            first_touch += 1;
                            sc.seen[w] |= bit;
                            let row = c.base + local;
                            let eligible = if c.visible && !st.groups.is_versioned(row) {
                                true
                            } else {
                                in_view(row, &meta(local), &mut memo)
                            };
                            if !eligible {
                                return Ok(());
                            }
                            sc.view[w] |= bit;
                            if whole_view {
                                out.view_counts[ti] += weight(tf);
                            }
                            if pass
                                && match self.literal {
                                    Some(needle) => st.has_phrase(row, needle)?,
                                    None => true,
                                }
                            {
                                sc.cand[w] |= bit;
                                out.hits.push((at, tf));
                            }
                            Ok(())
                        };
                        match source {
                            TermSource::Disk(list, _) => {
                                resume.at[ti] =
                                    Some(scan_postings(list, resume_at, lo, hi, &mut skipped, &mut visit)?);
                            }
                            TermSource::Fresh(list) => {
                                let a = list.partition_point(|(r, _)| *r < lo);
                                let b = list.partition_point(|(r, _)| *r < hi);
                                for (local, tf) in &list[a..b] {
                                    visit(*local, *tf)?;
                                }
                            }
                        }
                    }
                    if out.hits.len() > start {
                        out.runs.push((ti as u32, start as u32, out.hits.len() as u32));
                    }
                }
                crate::profile::lap(
                    &mut lap,
                    if pass { crate::profile::SCAN_PASS1 } else { crate::profile::SCAN_PASS2 },
                );
            }
            // Canonical candidate order (by row): walk the candidate bits,
            // reading (scope, length) in row order.
            let mut count = 0u32;
            out.rank.reserve_exact(sc.cand.len());
            for (w, word) in sc.cand.iter().enumerate() {
                out.rank.push(count);
                count += word.count_ones();
                let mut bits = *word;
                while bits != 0 {
                    let local = lo + (w * 64) as u32 + bits.trailing_zeros();
                    bits &= bits - 1;
                    out.rows.push(c.base + local);
                    out.mass.push(match c.disk {
                        Some(s) => s.scope_length(local),
                        None => {
                            let m = &st.fresh.meta[local as usize];
                            (m.scope, m.length)
                        }
                    });
                }
            }
            out.cand = sc.cand.clone();
            crate::profile::lap(&mut lap, crate::profile::SCAN_ORDER);
            Ok(())
        })?;
        // Pass-1 runs, then pass-2 runs: each ascending by term; merge them
        // so every candidate receives its terms in term order.
        out.runs.sort_unstable_by_key(|r| r.0);
        crate::profile::count(crate::profile::N_SKIPPED, skipped);
        crate::profile::count(crate::profile::N_VISITED, out.visited as u64);
        crate::profile::count(crate::profile::N_FIRST_TOUCH, first_touch);
        Ok(out)
    }
}

/// The v3 ranking model over segments. Candidates are found per segment
/// with generation-stamped scratch slots, so no allocation is proportional
/// to the whole store; scores add term contributions in v3's order.
/// The anchored ranking of `q`; when its anchored view holds fewer than
/// `q.k` records, the list goes on with the unanchored ranking (`split`),
/// past the rows already listed (see `SourceMatch::continued`). Equal to
/// v3's `rank_continued`.
fn rank_continued(
    st: &State,
    q: &Query,
    from: i64,
    options: &Options,
) -> Result<(Matches, TimelineMetrics, Vec<Row>)> {
    let (mut result, mut metrics, mut rows, continuable) = rank(st, q, from, options, false)?;
    if !continuable || result.matches.len() >= q.k {
        return Ok((result, metrics, rows));
    }
    let (rest, more, rest_rows, _) = rank(st, q, from, options, true)?;
    let listed: HashSet<Row> = rows.iter().copied().collect();
    for (m, row) in rest.matches.into_iter().zip(rest_rows) {
        if result.matches.len() == q.k {
            break;
        }
        if !listed.contains(&row) {
            result.matches.push(SourceMatch { continued: true, ..m });
            rows.push(row);
            metrics.continued_matches += 1;
        }
    }
    metrics.posted_rows_visited += more.posted_rows_visited;
    metrics.scored_records += more.scored_records;
    metrics.candidate_records = more.candidate_records;
    metrics.routing_entropy_bits = more.routing_entropy_bits;
    result.routing_entropy_bits = more.routing_entropy_bits;
    Ok((result, metrics, rows))
}

/// One ranking of `q`; with `split`, every identifier is read as its
/// component words (as when the view holds no occurrence of it). Also
/// returns whether the ranking was anchored (and so may be continued).
fn rank(
    st: &State,
    q: &Query,
    from: i64,
    options: &Options,
    split: bool,
) -> Result<(Matches, TimelineMetrics, Vec<Row>, bool)> {
    let total_lap = crate::profile::start();
    let mut lap = total_lap;
    let mu = options.mu;
    let whole_view = options.background == Background::View;
    let mut metrics = TimelineMetrics::default();
    let mut memo = HashMap::new();
    let literal = literal_query(&q.text);
    let mut query = query_terms(&q.text);
    crate::profile::lap(&mut lap, crate::profile::PARSE);
    // Exact identifiers are literals. Fall back to components only when this
    // historical view has no eligible occurrence of the whole name.
    let mut backoff = Vec::new();
    for (term, count) in &query {
        if !term.contains('_') {
            continue;
        }
        if split {
            backoff.push((term.clone(), *count));
            continue;
        }
        let mut seen = false;
        'lists: for (base, postings) in st.postings(term) {
            for (local, _) in postings {
                metrics.posted_rows_visited += 1;
                let row = base + local;
                let m = st.meta(row);
                if m.valid_at >= from && st.latest(row, &m, q, &mut memo) {
                    seen = true;
                    break 'lists;
                }
            }
        }
        if !seen {
            backoff.push((term.clone(), *count));
        }
    }
    for (term, count) in backoff {
        query.remove(&term);
        for (part, n) in query_terms(&term.replace('_', " ")) {
            *query.entry(part).or_default() += n * count;
        }
    }
    metrics.query_terms = query.len();
    crate::profile::lap(&mut lap, crate::profile::BACKOFF);
    // A bare query equal to a key addresses that key only when a version of
    // it exists in this historical view. (v3 also matched keys first written
    // after the as-of time, letting future writes change past results.)
    let direct = literal
        .is_none()
        .then(|| {
            q.text.strip_prefix("key:").or_else(|| {
                st.rows_with_key(fingerprint(&q.text))
                    .into_iter()
                    .any(|r| {
                        let m = st.meta(r);
                        m.valid_at <= q.valid_at && m.recorded_at <= q.known_at
                    })
                    .then_some(q.text.as_str())
            })
        })
        .flatten();
    crate::profile::lap(&mut lap, crate::profile::DIRECT);
    let anchored = direct.is_none() && query.keys().any(|term| term.contains('_'));
    let scope_model = direct.is_none() && !anchored && literal.is_none() && !q.text.is_empty();
    let scan_time = q.text.is_empty() || (literal.is_some() && query.is_empty());
    let terms: Vec<(&String, usize)> = query.iter().map(|(t, c)| (t, *c)).collect();

    let in_view = |row: Row, m: &RowMeta, memo: &mut HashMap<u32, Option<Row>>| {
        m.valid_at >= from && st.latest(row, m, q, memo)
    };
    let eligible = |row: Row, m: &RowMeta, memo: &mut HashMap<u32, Option<Row>>| -> Result<bool> {
        Ok(in_view(row, m, memo)
            && match literal {
                Some(needle) => st.has_phrase(row, needle)?,
                None => true,
            })
    };
    // Per-term occurrences among eligible records of the whole view.
    let mut view_counts = vec![0usize; terms.len()];

    // Learned outcomes for this exact query text.
    let mut learned = HashMap::<Row, (usize, usize)>::new();
    let mut admitted = Vec::<Row>::new();
    let hash = *blake3::hash(q.text.as_bytes()).as_bytes();
    if let Some(indices) = st.by_query.get(&hash) {
        for i in indices {
            let live = &st.outcomes[*i];
            let o = &live.outcome;
            if o.at > q.valid_at || o.at > q.known_at {
                continue;
            }
            let m = st.meta(live.row);
            if !eligible(live.row, &m, &mut memo)? {
                continue;
            }
            metrics.eligible_outcomes += 1;
            let count = learned.entry(live.row).or_default();
            if o.useful {
                count.0 += 1;
                admitted.push(live.row);
            } else {
                count.1 += 1;
            }
        }
    }
    admitted.sort_unstable();
    admitted.dedup();
    crate::profile::lap(&mut lap, crate::profile::LEARNED);

    // Candidates in canonical order, grouped by the scan chunk that found
    // them, with each chunk's per-term hit runs.
    let mut outs: Vec<ChunkOut> = Vec::new();
    let mut threads = 1;
    if let Some(key) = direct {
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut rows = st.rows_with_key(fingerprint(key));
        rows.sort_unstable();
        for row in rows {
            let m = st.meta(row);
            if eligible(row, &m, &mut memo)? {
                candidates.push(Candidate { row, meta: m });
            }
        }
        for row in &admitted {
            if !candidates.iter().any(|c| c.row == *row) {
                candidates.push(Candidate {
                    row: *row,
                    meta: st.meta(*row),
                });
            }
        }
        outs.push(ChunkOut::listed(candidates));
    } else if scan_time {
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen = HashSet::new();
        st.time_rows(from, q.valid_at, |row| {
            let m = st.meta(row);
            if eligible(row, &m, &mut memo)? {
                candidates.push(Candidate { row, meta: m });
                seen.insert(row);
            }
            Ok(true)
        })?;
        for row in &admitted {
            if seen.insert(*row) {
                candidates.push(Candidate {
                    row: *row,
                    meta: st.meta(*row),
                });
            }
        }
        outs.push(ChunkOut::listed(candidates));
    } else {
        // A word-bounded exact phrase contains every query term, so its
        // rarest posting list is a complete candidate generator.
        let rarest = literal.and_then(|_| {
            terms
                .iter()
                .enumerate()
                .min_by_key(|(_, (t, _))| st.document_frequency(t))
                .map(|(i, _)| i)
        });
        let first_pass: Vec<bool> = terms
            .iter()
            .enumerate()
            .map(|(i, (t, _))| rarest.map_or(!anchored || t.contains('_'), |r| r == i))
            .collect();
        // Every segment is scanned in fixed row ranges, in parallel when the
        // query touches many postings. Chunk results merge in row order, so
        // results never depend on thread count or timing.
        let segs: Vec<(Row, u32, Option<&DiskSegment>)> = st
            .segments
            .iter()
            .map(|s| (s.start, s.rows, Some(s.as_ref())))
            .chain(std::iter::once((st.fresh.start, st.fresh.len(), None)))
            .collect();
        let visible = |span: &Span| {
            span.min_valid >= from && span.max_valid <= q.valid_at && span.max_recorded <= q.known_at
        };
        let mut work = 0u64;
        let names: Vec<&str> = terms.iter().map(|(t, _)| t.as_str()).collect();
        let sources: Vec<Vec<Option<TermSource<'_>>>> = segs
            .iter()
            .map(|(_, _, disk)| match disk {
                Some(d) => d
                    .terms_sorted(&names)
                    .into_iter()
                    .map(|info| {
                        info.map(|info| {
                            work += u64::from(info.df);
                            TermSource::Disk(d.list(info), info)
                        })
                    })
                    .collect(),
                None => names
                    .iter()
                    .map(|t| {
                        st.fresh.postings.get(*t).map(|l| {
                            work += l.len() as u64;
                            TermSource::Fresh(l.as_slice())
                        })
                    })
                    .collect(),
            })
            .collect();
        crate::profile::lap(&mut lap, crate::profile::DICT);
        let mut chunks = Vec::new();
        for (i, (base, rows, disk)) in segs.iter().enumerate() {
            let mut lo = 0;
            while lo < *rows {
                let hi = rows.saturating_sub(lo).min(CHUNK_ROWS) + lo;
                chunks.push(Chunk {
                    segment: i,
                    visible: visible(match disk {
                        Some(d) => &d.span,
                        None => &st.fresh.span,
                    }),
                    base: *base,
                    lo,
                    hi,
                    disk: *disk,
                    sources: &sources[i],
                });
                lo = hi;
            }
        }
        crate::profile::count(crate::profile::N_CHUNKS, chunks.len() as u64);
        let scan = ScanContext {
            st,
            q,
            from,
            literal,
            first_pass: &first_pass,
            whole_view,
            scope_model,
            admitted: &admitted,
        };
        threads = if work >= PARALLEL_POSTINGS {
            options.query_threads.max(1).min(chunks.len())
        } else {
            1
        };
        if threads > 1 {
            // Workers take chunks out of order, so a chunk's decode cannot
            // resume where the previous chunk stopped: give every long list
            // of a multi-chunk segment its skip table first (built in
            // parallel, kept), so each chunk starts at its own block.
            let mut missing = Vec::new();
            for (i, (_, rows, disk)) in segs.iter().enumerate() {
                let Some(d) = disk else { continue };
                if *rows <= CHUNK_ROWS {
                    continue;
                }
                for source in &sources[i] {
                    if let Some(TermSource::Disk(_, info)) = source {
                        if info.df >= crate::segment::SKIP_MIN && d.cached_skips(*info).is_none() {
                            missing.push((*d, *info));
                        }
                    }
                }
            }
            for_parts(threads, missing, |(d, info)| {
                d.skips_now(info);
            });
        }
        outs = if threads <= 1 {
            let mut resume = Resume::default();
            chunks
                .iter()
                .map(|c| scan.chunk(c, &mut resume))
                .collect::<Result<_>>()?
        } else {
            let next = AtomicUsize::new(0);
            let slots: Vec<Mutex<Option<Result<ChunkOut>>>> =
                chunks.iter().map(|_| Mutex::new(None)).collect();
            std::thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        let mut resume = Resume::default();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(c) = chunks.get(i) else { break };
                            let out = scan.chunk(c, &mut resume);
                            if let Ok(mut slot) = slots[i].lock() {
                                *slot = Some(out);
                            }
                        }
                    });
                }
            });
            slots
                .into_iter()
                .map(|m| {
                    m.into_inner()
                        .map_err(|_| "query worker poisoned")?
                        .ok_or("query chunk was not scanned")?
                })
                .collect::<Result<_>>()?
        };
        crate::profile::lap(&mut lap, crate::profile::SCAN);
        for out in &outs {
            for (v, n) in view_counts.iter_mut().zip(&out.view_counts) {
                *v += n;
            }
            metrics.posted_rows_visited += out.visited;
        }
    }
    let any_hits = outs.iter().any(|o| !o.hits.is_empty());
    let candidate_count: usize = outs.iter().map(|o| o.rows.len()).sum();
    crate::profile::lap(&mut lap, crate::profile::MERGE);
    // Background statistics: candidates only (v3) or the whole view.
    let mut collection = vec![0usize; terms.len()];
    let mut retained = vec![false; terms.len()];
    let total = if whole_view && any_hits {
        for (i, n) in view_counts.iter().enumerate() {
            collection[i] = *n;
            retained[i] = *n > 0;
        }
        st.view_mass(q, from, scope_model) as f64
    } else {
        for o in &outs {
            for &(ti, a, b) in &o.runs {
                let ti = ti as usize;
                collection[ti] += if scope_model {
                    (b - a) as usize
                } else {
                    o.hits[a as usize..b as usize]
                        .iter()
                        .map(|(_, tf)| *tf as usize)
                        .sum()
                };
                retained[ti] = true;
            }
        }
        outs.iter()
            .flat_map(|o| &o.mass)
            .map(|(scope, length)| u64::from(if scope_model { *scope } else { *length }))
            .sum::<u64>() as f64
    };
    // A candidate background holding less than BACKGROUND_FLOOR_RECORDS * mu
    // of mass is padded with whole-view text up to that mass: with one
    // candidate it would be that record, which then saves no bits
    // (docs/SEARCH_FIXES.md, F3). Larger candidate sets are unchanged.
    let floor = crate::BACKGROUND_FLOOR_RECORDS * mu;
    let pad = if !(whole_view && any_hits) && any_hits && total < floor {
        let view_total = st.view_mass(q, from, scope_model);
        let names: Vec<&str> = terms
            .iter()
            .enumerate()
            .filter(|(i, _)| retained[*i])
            .map(|(_, (t, _))| t.as_str())
            .collect();
        let mut counts = st.view_term_counts(&names, q, from, scope_model).into_iter();
        let view: Vec<u64> = (0..terms.len()).map(|i| if retained[i] { counts.next().unwrap_or(0) } else { 0 }).collect();
        (view_total > 0).then(|| (floor - total, view, view_total as f64))
    } else {
        None
    };
    let background: Vec<f64> = match &pad {
        None => terms
            .iter()
            .enumerate()
            .map(|(i, (t, _))| (collection[i] as f64 + literal_probability(t)) / (total + 1.0))
            .collect(),
        Some((r, view, view_total)) => terms
            .iter()
            .enumerate()
            .map(|(i, (t, _))| {
                (collection[i] as f64 + r * view[i] as f64 / view_total + literal_probability(t)) / (total + r + 1.0)
            })
            .collect(),
    };
    metrics.background_pad = pad.as_ref().map_or(0.0, |p| p.0);
    let baseline = terms
        .iter()
        .enumerate()
        .filter(|(i, _)| retained[*i])
        .map(|(i, (_, count))| -(*count as f64) * background[i].log2())
        .sum::<f64>();
    metrics.background_query_bits = baseline;
    metrics.candidate_records = candidate_count;
    let query_count = terms
        .iter()
        .enumerate()
        .filter(|(i, _)| retained[*i])
        .map(|(_, (_, c))| *c)
        .sum::<usize>() as f64;
    crate::profile::lap(&mut lap, crate::profile::BACKGROUND);
    // The length penalty depends only on the candidate's mass: computed
    // once per distinct small mass (same expression, same bits).
    let mut penalties: Vec<Option<f64>> = vec![None; 4096];
    let mut gains: Vec<f64> = outs
        .iter()
        .flat_map(|o| &o.mass)
        .map(|(scope, length)| {
            let mass = if scope_model { *scope } else { *length };
            let penalty = || -query_count * (1. + mass as f64 / mu).log2();
            match penalties.get_mut(mass as usize) {
                Some(Some(p)) => *p,
                Some(slot) => *slot.insert(penalty()),
                None => penalty(),
            }
        })
        .collect();
    crate::profile::lap(&mut lap, crate::profile::GAINS);
    // Per candidate, add term contributions in term order as v3 does: each
    // chunk's runs are ordered by term and a candidate appears at most once
    // per run, so no per-candidate sort is needed.
    let usefulness = |(positive, negative): (usize, usize)| {
        ((positive + 1) as f64 / (negative + 1) as f64)
            .log2()
            .clamp(-8.0, 8.0)
    };
    let neutral = usefulness((0, 0));
    let bonus = |row: &Row| match learned.get(row) {
        Some(counts) => usefulness(*counts),
        None => neutral,
    };
    // Exact term contributions for one chunk's candidates (all, or those
    // marked in `only`), added in term order per candidate as v3 does: each
    // chunk's runs are ordered by term and a candidate appears at most once
    // per run, so no per-candidate sort is needed.
    let score_chunk = |o: &ChunkOut, gains: &mut [f64], only: Option<&[u64]>| {
        for &(ti, a, b) in &o.runs {
            let ti = ti as usize;
            let scale = mu * background[ti];
            let count = terms[ti].1 as f64;
            for &(at, tf) in &o.hits[a as usize..b as usize] {
                if only.is_some_and(|only| (only[at as usize / 64] >> (at % 64)) & 1 == 0) {
                    continue;
                }
                let c = o.index(at);
                let mass = if scope_model {
                    let (scope, length) = o.mass[c];
                    tf as f64 * scope as f64 / length as f64
                } else {
                    tf as f64
                };
                gains[c] += count * (1. + mass / scale).log2();
            }
        }
        crate::profile::count(crate::profile::N_HITS, o.hits.len() as u64);
    };
    let offsets: Vec<usize> = outs
        .iter()
        .scan(0, |at, o| {
            let start = *at;
            *at += o.rows.len();
            Some(start)
        })
        .collect();
    // Each chunk owns its candidates' slice, so chunks score independently
    // (on the scan's threads when it ran in parallel).
    let score_all = |gains: &mut [f64], only: Option<&[Vec<u64>]>| {
        let parts = split_by(gains, &outs);
        for_parts(threads, parts.into_iter().enumerate().collect(), |(i, g)| {
            score_chunk(&outs[i], g, only.map(|m| m[i].as_slice()));
        });
    };
    let exact_all = options.routing_entropy == RoutingEntropy::All || q.k >= candidate_count;
    // Candidates whose exact score is known (all of them unless pruned).
    let mut exact: Option<Vec<bool>> = None;
    if exact_all {
        score_all(&mut gains, None);
    } else {
        // Upper bounds: tf * scope / length <= tf, and log2 replaced by a
        // cheap upper bound (one constant per term for tf = 1). Exact scores
        // for the best bounds give a k-th best score; every candidate whose
        // bound reaches it is scored exactly, so the top k (ids and score
        // bits) equal exhaustive scoring.
        let mut bound = gains.clone();
        {
            let parts = split_by(&mut bound, &outs);
            for_parts(threads, parts.into_iter().enumerate().collect(), |(i, u)| {
                let o = &outs[i];
                for &(ti, a, b) in &o.runs {
                    let ti = ti as usize;
                    let scale = mu * background[ti];
                    let count = terms[ti].1 as f64;
                    let once = count * log2_upper(1. + 1. / scale);
                    for &(at, tf) in &o.hits[a as usize..b as usize] {
                        u[o.index(at)] += if tf == 1 {
                            once
                        } else {
                            count * log2_upper(1. + tf as f64 / scale)
                        };
                    }
                }
            });
        }
        for (u, row) in bound.iter_mut().zip(outs.iter().flat_map(|o| &o.rows)) {
            *u += bonus(row);
        }
        // Candidate c as a bit at its row offset in its chunk (listed
        // chunks have no hits: their gains are already exact).
        let empty = || outs.iter().map(|o| vec![0u64; o.cand.len()]).collect::<Vec<_>>();
        let mark = |marks: &mut Vec<Vec<u64>>, c: usize| {
            let i = offsets.partition_point(|start| *start <= c) - 1;
            if outs[i].cand.is_empty() {
                return;
            }
            let at = (outs[i].rows[c - offsets[i]] - outs[i].first_row) as usize;
            marks[i][at / 64] |= 1 << (at % 64);
        };
        let mut chosen = vec![false; candidate_count];
        let mut seed = empty();
        for c in best_indices(&bound, (2 * q.k).max(64)) {
            chosen[c] = true;
            mark(&mut seed, c);
        }
        score_all(&mut gains, Some(&seed));
        let mut seeded: Vec<f64> = outs
            .iter()
            .flat_map(|o| &o.rows)
            .enumerate()
            .filter(|(c, _)| chosen[*c])
            .map(|(c, row)| gains[c] + bonus(row))
            .collect();
        seeded.sort_unstable_by(|a, b| b.total_cmp(a));
        let theta = seeded[q.k - 1];
        let reach = theta - 1e-9 * (1.0 + theta.abs());
        let mut more = empty();
        let mut extra = 0;
        for c in 0..candidate_count {
            if !chosen[c] && !(bound[c] < reach) {
                chosen[c] = true;
                mark(&mut more, c);
                extra += 1;
            }
        }
        if extra > 0 {
            score_all(&mut gains, Some(&more));
        }
        exact = Some(chosen);
    }
    crate::profile::count(crate::profile::N_TERMS, terms.len() as u64);
    crate::profile::count(crate::profile::N_CANDIDATES, candidate_count as u64);
    crate::profile::lap(&mut lap, crate::profile::HITS);
    // Scores of exactly scored candidates; pruned ones rank below the k-th
    // best and are left out.
    let rows: Vec<Row> = outs.iter().flat_map(|o| o.rows.iter().copied()).collect();
    let keep = |c: usize| exact.as_ref().is_none_or(|e: &Vec<bool>| e[c]);
    let scored_ix: Vec<usize> = (0..candidate_count).filter(|c| keep(*c)).collect();
    let scores: Vec<f64> = scored_ix.iter().map(|c| gains[*c] + bonus(&rows[*c])).collect();
    metrics.scored_records = scores.len();
    crate::profile::lap(&mut lap, crate::profile::SCORED);
    if exact_all {
        let maximum = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // exp2 of anything below -1080 is exactly 0, which `entropy` ignores:
        // leaving those weights out changes no bit of the result.
        let weights: Vec<f64> = scores
            .iter()
            .map(|s| s - maximum)
            .filter(|x| !(*x < -1080.0))
            .map(f64::exp2)
            .collect();
        metrics.routing_entropy_bits = oojuro_core::entropy(&weights);
    }
    crate::profile::lap(&mut lap, crate::profile::ENTROPY);
    // Only candidates at or above the k-th best (score, time, id prefix)
    // can be returned; `select_top` orders that small set exactly as it
    // would the whole list.
    let scored_rows: Vec<Row> = scored_ix.iter().map(|c| rows[*c]).collect();
    let mut finalists: Vec<Scored> = Vec::new();
    if let Some(pivot) = top_pivot(st, scored_rows.iter().zip(&scores), q.k) {
        for ((row, score), c) in scored_rows.iter().zip(&scores).zip(&scored_ix) {
            // Row metadata only for scores not below the pivot's.
            if pivot.0.total_cmp(score) == Order::Greater {
                continue;
            }
            let meta = st.meta(*row);
            if coarse_order(*score, &meta, pivot.0, pivot.1, pivot.2) != Order::Greater {
                finalists.push(Scored {
                    row: *row,
                    meta,
                    score: *score,
                    conditional: baseline - gains[*c],
                });
            }
        }
    }
    drop(outs);
    let pool = select_top(st, &mut finalists, q.k)?;
    if !exact_all {
        let maximum = pool.iter().map(|(_, s)| s.score).fold(f64::NEG_INFINITY, f64::max);
        let weights: Vec<f64> = pool.iter().map(|(_, s)| (s.score - maximum).exp2()).collect();
        metrics.routing_entropy_bits = oojuro_core::entropy(&weights);
    }
    crate::profile::lap(&mut lap, crate::profile::SELECT);
    crate::profile::since(&total_lap, crate::profile::RANK);
    metrics.best_conditional_query_bits = pool.first().map(|(_, s)| s.conditional);
    let rows = pool.iter().map(|(_, s)| s.row).collect();
    let matches = pool
        .into_iter()
        .map(|(id, s)| SourceMatch {
            id,
            time: s.meta.valid_at,
            bytes: s.meta.text_len as usize,
            score_bits: s.score,
            continued: false,
        })
        .collect();
    Ok((
        Matches {
            matches,
            routing_entropy_bits: metrics.routing_entropy_bits,
        },
        metrics,
        rows,
        anchored && literal.is_none(),
    ))
}

/// An upper bound of `log2(y)` for finite y >= 1 without libm: with
/// y = m * 2^e and m in [1, 2), log2(m) <= (m - 1) + 0.086071 (equality at
/// m = 1/ln 2); 0.0862 leaves 1.2e-4 for libm's rounding of the exact log2.
#[inline]
fn log2_upper(y: f64) -> f64 {
    let bits = y.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    e as f64 + (m - 1.0) + 0.0862
}

/// Indices of the `m` largest values (by `total_cmp`; any order).
fn best_indices(values: &[f64], m: usize) -> Vec<usize> {
    if m >= values.len() {
        return (0..values.len()).collect();
    }
    let mut ix: Vec<usize> = (0..values.len()).collect();
    ix.select_nth_unstable_by(m - 1, |a, b| values[*b].total_cmp(&values[*a]));
    ix.resize(m, 0);
    ix
}

/// One mutable slice of `values` per chunk, in chunk order.
fn split_by<'a>(values: &'a mut [f64], outs: &[ChunkOut]) -> Vec<&'a mut [f64]> {
    let mut parts = Vec::with_capacity(outs.len());
    let mut rest = values;
    for o in outs {
        let (mine, tail) = std::mem::take(&mut rest).split_at_mut(o.rows.len());
        parts.push(mine);
        rest = tail;
    }
    parts
}

/// Run `f` on every part, on `threads` scoped threads when above one.
fn for_parts<T: Send>(threads: usize, parts: Vec<T>, f: impl Fn(T) + Sync) {
    if threads <= 1 || parts.len() <= 1 {
        parts.into_iter().for_each(f);
        return;
    }
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<T>>> = parts.into_iter().map(|p| Mutex::new(Some(p))).collect();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(slots.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = slots.get(i) else { break };
                    if let Some(part) = slot.lock().ok().and_then(|mut s| s.take()) {
                        f(part);
                    }
                }
            });
        }
    });
}

/// `select_top`'s coarse order of a candidate against (score, valid_at,
/// id prefix): Less when the candidate ranks strictly better.
#[inline]
fn coarse_order(score: f64, meta: &RowMeta, score_b: f64, valid_b: i64, prefix_b: u64) -> Order {
    score_b
        .total_cmp(&score)
        .then_with(|| valid_b.cmp(&meta.valid_at))
        .then_with(|| meta.id_prefix.cmp(&prefix_b))
}

/// The k-th best (score, valid_at, id prefix) among the candidates under
/// `select_top`'s coarse order (the pivot its selection would find), with a
/// k-entry heap instead of moving every candidate; None when k is 0 or
/// there are no candidates.
fn top_pivot<'a>(
    st: &State,
    candidates: impl Iterator<Item = (&'a Row, &'a f64)>,
    k: usize,
) -> Option<(f64, i64, u64)> {
    struct Key(f64, i64, u64);
    impl PartialEq for Key {
        fn eq(&self, o: &Self) -> bool {
            self.cmp(o) == Order::Equal
        }
    }
    impl Eq for Key {}
    impl PartialOrd for Key {
        fn partial_cmp(&self, o: &Self) -> Option<Order> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Key {
        /// Greater = ranks worse, so the heap's top is the worst kept.
        fn cmp(&self, o: &Self) -> Order {
            o.0.total_cmp(&self.0)
                .then_with(|| o.1.cmp(&self.1))
                .then_with(|| self.2.cmp(&o.2))
        }
    }
    if k == 0 {
        return None;
    }
    let mut heap = std::collections::BinaryHeap::with_capacity(k.min(1 << 16));
    for (row, score) in candidates {
        let key = |row: Row| {
            let m = st.meta(row);
            Key(*score, m.valid_at, m.id_prefix)
        };
        if heap.len() < k {
            heap.push(key(*row));
        } else if let Some(mut worst) = heap.peek_mut() {
            // Row metadata only when the score could beat or tie the worst.
            if worst.0.total_cmp(score) != Order::Greater {
                let key = key(*row);
                if key < *worst {
                    *worst = key;
                }
            }
        }
    }
    heap.peek().map(|w| (w.0, w.1, w.2))
}

/// The best `k` candidates ordered by score desc, valid_at desc, id asc.
/// The 8-byte id prefix orders most ties; full ids are read only for the
/// selected rows and for rows tied with the last selected one.
fn select_top(st: &State, scored: &mut Vec<Scored>, k: usize) -> Result<Vec<(String, Scored)>> {
    let coarse = |a: &Scored, b: &Scored| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.meta.valid_at.cmp(&a.meta.valid_at))
            .then_with(|| a.meta.id_prefix.cmp(&b.meta.id_prefix))
    };
    let pool = k.min(scored.len());
    if pool == 0 {
        return Ok(Vec::new());
    }
    if pool < scored.len() {
        scored.select_nth_unstable_by(pool - 1, coarse);
        let (ps, pv, pp) = {
            let p = &scored[pool - 1];
            (p.score, p.meta.valid_at, p.meta.id_prefix)
        };
        let versus_pivot = |s: &Scored| {
            ps.total_cmp(&s.score)
                .then_with(|| pv.cmp(&s.meta.valid_at))
                .then_with(|| s.meta.id_prefix.cmp(&pp))
        };
        let mut kept = Vec::with_capacity(pool);
        let mut ties = Vec::new();
        for s in scored.drain(..) {
            match versus_pivot(&s) {
                Order::Less => kept.push(s),
                Order::Equal => ties.push(s),
                Order::Greater => {}
            }
        }
        let mut ties = ties
            .into_iter()
            .map(|s| Ok((st.id_of(s.row)?, s)))
            .collect::<Result<Vec<_>>>()?;
        ties.sort_by(|a, b| a.0.cmp(&b.0));
        let room = pool - kept.len();
        let mut named = kept
            .into_iter()
            .map(|s| Ok((st.id_of(s.row)?, s)))
            .collect::<Result<Vec<_>>>()?;
        named.extend(ties.into_iter().take(room));
        sort_named(&mut named);
        return Ok(named);
    }
    let mut named = scored
        .drain(..)
        .map(|s| Ok((st.id_of(s.row)?, s)))
        .collect::<Result<Vec<_>>>()?;
    sort_named(&mut named);
    Ok(named)
}

fn sort_named(named: &mut [(String, Scored)]) {
    named.sort_by(|(ia, a), (ib, b)| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.meta.valid_at.cmp(&a.meta.valid_at))
            .then_with(|| ia.cmp(ib))
    });
}

impl Engine for Nine {
    fn name(&self) -> &'static str {
        "nine"
    }
    fn insert(&mut self, record: Record) -> Result<()> {
        Nine::insert(self, record)
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
                record: self.record(&m.id)?.ok_or("indexed source")?,
                score: m.score_bits as f32,
                uncertainty_bits: None,
            });
        }
        Ok(SearchResult {
            engine: "nine".into(),
            hits,
            routing_entropy_bits: refs.routing_entropy_bits,
            notes: vec![
                "Local keyword postings + conditional query code length; no embeddings or LLM."
                    .into(),
                "Routing entropy describes candidate ambiguity, not factual confidence. Exact originals are returned.".into(),
            ],
        })
    }
    fn len(&self) -> usize {
        Nine::len(self)
    }
}

#[cfg(test)]
#[path = "v4_tests.rs"]
mod tests;
