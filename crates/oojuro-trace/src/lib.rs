//! oojuro-trace v0: an agent-trace store composed from existing parts.
//!
//! - One v4 engine (`oojuro_nine::Nine`) per agent: group-commit journal (one
//!   sync per turn batch, shared by concurrent writers of that agent),
//!   searchable fresh segment, immutable memory-mapped index segments and
//!   SPUD-Dirichlet code-length ranking. Search is scoped to an agent by
//!   construction.
//! - Originals of immutable segments go through [`ZstdArchive`]: the
//!   compression track's run files (zstd + per-run dictionary + content-defined
//!   dedupe), fast level for flushes, heavy level for merged runs.
//! - Typed fields ride in each record's metadata (durable in the same journal
//!   batch) and are mirrored into an in-memory column table with time zone
//!   maps for filters and aggregates (rebuilt from the records on open).
//! - A status revision is its own unindexed record (key `r<call id>`); the
//!   column table keeps the latest status.
//! - Deletes are durable tombstones (`tombstones.log`, synced) applied to every
//!   read; [`TraceDb::reclaim`] rewrites each affected agent store with live
//!   records only and removes the old files, so deleted text leaves the store
//!   (journals, segments, dictionaries and dedupe chunks included).
//! - Optional shared log ([`Options::shared_log`]): every agent store
//!   journals into one [`SharedLog`] (`shared-log/`), so concurrent turns of
//!   different agents share one sync. Each agent store keeps its own index,
//!   segments and runs. Reclaim then also makes every store absorb its rows
//!   and scrubs the log (zeroed and removed), see `docs/SHARED_LOG.md`.
//! - Record-level dedupe (`Options::dedupe_records`, see `dedupe.rs`): an
//!   event repeating an earlier event's text, tool and args in the same agent
//!   store is stored once and referenced, with reference counts enforced by
//!   reclaim.
use oojuro_core::Record;
use oojuro_nine::{
    LogBinding, Nine, Options as NineOptions, Receipts, RoutingEntropy, SharedLog, SharedLogOptions,
    archive::{Archive, Run, RunInfo},
};
pub use oojuro_nine::{SharedLogStats, journal_syncs};
use oojuro_store::run::{RunOptions, RunReader, SharedDict, write_run};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub const KIND_TOOL_CALL: u8 = 4;
pub const ST_ERROR: u8 = 3;

// ---------------------------------------------------------------- archive

/// Archive adapter over `oojuro_store::run`. Runs below `heavy_threshold`
/// raw bytes (segment flushes) use `fast_level` and no trained dictionary
/// (the starter dictionary when `starter` is set); larger runs (background
/// merges, the archive tier) use `heavy_level` and a dictionary trained on
/// the run (the starter when the run is too small to train one).
pub struct ZstdArchive {
    pub fast_level: i32,
    pub heavy_level: i32,
    pub heavy_threshold: usize,
    pub dedupe_chunk: u32,
    pub dict: bool,
    pub cache_bytes: usize,
    /// Shared dictionary new runs may use (never stored in them).
    pub starter: Option<SharedDict>,
    name: String,
}

impl ZstdArchive {
    pub fn new(fast_level: i32, heavy_level: i32, dedupe_chunk: u32, dict: bool) -> Self {
        Self {
            fast_level,
            heavy_level,
            heavy_threshold: 48 << 20,
            dedupe_chunk,
            dict,
            cache_bytes: 4 << 20,
            starter: None,
            // the reader identity (run files describe their own codec and
            // dictionary, so stores written with other settings still open)
            name: format!("ojrun1-z{fast_level}-z{heavy_level}-c{dedupe_chunk}-d{}", dict as u8),
        }
    }
}

/// Return freed heap to the OS (glibc keeps per-arena slack after the large
/// alloc/free cycles of segment flushes and merges; measured 63% of RSS on
/// a loaded smoke store).
pub fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        unsafe {
            malloc_trim(0);
        }
    }
}

/// Heavy runs below this many raw bytes use the starter dictionary.
const SMALL_RUN: usize = 4 << 20;

thread_local! {
    /// Set while this thread runs a major compaction: every run it writes
    /// uses the heavy level and a dictionary, whatever its size.
    static FORCE_HEAVY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct ZRun(RunReader);

fn store_err(e: oojuro_store::Error) -> oojuro_core::Error {
    e.to_string().into()
}

impl Run for ZRun {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn file_bytes(&self) -> u64 {
        self.0.file_bytes()
    }
    fn get(&self, i: u64) -> oojuro_core::Result<Vec<u8>> {
        // Decode (and cache) the whole frame: a ranged read decodes only a
        // prefix, so the next row of the same frame would decode it again.
        let mut out = None;
        self.0
            .scan(i, i + 1, &mut |_, b| {
                out = Some(b.to_vec());
                Ok(())
            })
            .map_err(store_err)?;
        out.ok_or_else(|| "row outside the run".into())
    }
    fn scan(
        &self,
        start: u64,
        end: u64,
        visit: &mut dyn FnMut(u64, &[u8]) -> oojuro_core::Result<()>,
    ) -> oojuro_core::Result<()> {
        let mut err = None;
        let r = self.0.scan(start, end, &mut |i, b| {
            visit(i, b).map_err(|e| {
                err = Some(e);
                oojuro_store::Error::Invalid("visit failed")
            })
        });
        if let Some(e) = err {
            return Err(e);
        }
        r.map_err(store_err)
    }
}

impl Archive for ZstdArchive {
    fn name(&self) -> &str {
        &self.name
    }
    fn write(&self, path: &Path, originals: &mut dyn Iterator<Item = Vec<u8>>) -> oojuro_core::Result<RunInfo> {
        // Buffer up to the threshold to tell a flush from a merge.
        let mut head = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        while let Some(r) = originals.next() {
            bytes += r.len();
            head.push(r);
            if bytes >= self.heavy_threshold {
                more = true;
                break;
            }
        }
        let heavy = more || FORCE_HEAVY.with(|f| f.get());
        // a heavy run under 4 MiB compresses better with the starter
        // dictionary than with one trained on itself (PERF_STORE P3)
        let small = !more && bytes < SMALL_RUN && self.starter.is_some();
        let opts = RunOptions {
            level: if heavy { self.heavy_level } else { self.fast_level },
            dedupe_chunk: self.dedupe_chunk,
            // dictionaries only for merged (archive) runs: training one per
            // flush costs ~0.2 s under the agent's writer lock
            dict_bytes: if self.dict && heavy && !small { 512 << 10 } else { 0 },
            threads: 2,
            shared_dict: self.starter.clone(),
            ..RunOptions::default()
        };
        let mut it = head.into_iter().chain(originals);
        let info = write_run(path, &mut it, &opts).map_err(store_err)?;
        Ok(RunInfo { rows: info.rows, raw_bytes: info.raw_bytes, file_bytes: info.file_bytes })
    }
    fn open(&self, path: &Path) -> oojuro_core::Result<Box<dyn Run>> {
        // runs written with the starter dictionary open whatever the option
        let r = RunReader::open_with(path, self.cache_bytes, Some(oojuro_store::starter::v1())).map_err(store_err)?;
        Ok(Box::new(ZRun(r)))
    }
}

// ---------------------------------------------------------------- events

/// An event to append. `revises != 0` makes it a status revision of that call.
#[derive(Clone, Copy, Debug)]
pub struct EventIn<'a> {
    pub id: u64,
    pub session: u64,
    pub agent: u32,
    pub turn: u32,
    pub seq: u32,
    pub parent: u64,
    pub revises: u64,
    pub kind: u8,
    pub status: u8,
    pub exit_code: Option<i32>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub ts_us: i64,
    pub duration_us: i64,
    pub tool: &'a str,
    pub args: &'a str,
    pub text: &'a str,
    /// Indexed for search (false = stored only).
    pub indexed: bool,
}

/// An event read back, latest status applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventOut {
    pub id: u64,
    pub session: u64,
    pub agent: u32,
    pub turn: u32,
    pub seq: u32,
    pub parent: u64,
    pub kind: u8,
    pub status: u8,
    pub exit_code: Option<i32>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub ts_us: i64,
    pub duration_us: i64,
    pub tool: String,
    pub args: String,
    pub text: String,
}

/// A live event's typed fields from the column table (no text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventInfo {
    pub id: u64,
    pub agent: u32,
    pub session: u64,
    pub turn: u32,
    pub seq: u32,
    pub kind: u8,
    pub status: u8,
    pub ts_us: i64,
    pub text_len: u32,
}

impl EventInfo {
    fn of(c: &Col) -> Self {
        Self {
            id: c.id,
            agent: c.agent,
            session: c.session,
            turn: c.turn,
            seq: c.seq,
            kind: c.kind,
            status: c.status,
            ts_us: c.ts,
            text_len: c.text_len,
        }
    }
}

/// One hit of [`TraceDb::search_where`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchHit {
    pub id: u64,
    pub bits: f64,
    /// Past the query's identifier-anchored view
    /// (`oojuro_nine::SourceMatch::continued`): after every anchored hit of
    /// this store, with bits from the query's unanchored ranking.
    pub continued: bool,
}

/// Result of [`TraceDb::erasure_scan`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErasureScan {
    /// Engine rows decoded.
    pub rows: u64,
    /// Rows holding a needle that a live event still needs.
    pub live_rows_with_needle: u64,
    /// Rows holding a needle that nothing live needs (zero after a reclaim).
    pub dead_rows_with_needle: u64,
    /// The same two counts per needle (in the order given).
    pub live_by_needle: Vec<u64>,
    pub dead_by_needle: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub agent: Option<u32>,
    pub kind: Option<u8>,
    pub tool: Option<String>,
    pub status: Option<u8>,
    pub from_us: i64,
    pub to_us: i64,
}

pub use oojuro_nine::{profile_reset, profile_snapshot};
mod columns;
mod dedupe;
use columns::{Col, ColStore, NO_EXIT};
use dedupe::{DupEntry, Src};

fn meta_line(e: &EventIn) -> String {
    let ex = e.exit_code.map(|x| x.to_string()).unwrap_or_default();
    format!(
        "{},{},{},{},{},{},{},{},{},{},{}",
        e.session, e.turn, e.seq, e.parent, e.kind, e.status, ex, e.tokens_in, e.tokens_out, e.duration_us, e.revises
    )
}

struct Parsed {
    session: u64,
    turn: u32,
    seq: u32,
    parent: u64,
    kind: u8,
    status: u8,
    exit: i32,
    tin: u32,
    tout: u32,
    duration: i64,
    revises: u64,
}

fn parse_meta(s: &str) -> Result<Parsed> {
    let f: Vec<&str> = s.split(',').collect();
    if f.len() != 11 {
        return Err("malformed event metadata".into());
    }
    Ok(Parsed {
        session: f[0].parse()?,
        turn: f[1].parse()?,
        seq: f[2].parse()?,
        parent: f[3].parse()?,
        kind: f[4].parse()?,
        status: f[5].parse()?,
        exit: if f[6].is_empty() { NO_EXIT } else { f[6].parse()? },
        tin: f[7].parse()?,
        tout: f[8].parse()?,
        duration: f[9].parse()?,
        revises: f[10].parse()?,
    })
}

// ---------------------------------------------------------------- store

#[derive(Clone)]
pub struct Options {
    pub archive_fast_level: i32,
    pub archive_heavy_level: i32,
    pub dedupe_chunk: u32,
    pub dict: bool,
    /// Per-agent fresh segment size before it is flushed to an archive run.
    pub flush_text_bytes: usize,
    pub merge_fanout: usize,
    pub query_threads: usize,
    /// Seal column blocks (zstd, ~6-7 B/event) instead of keeping them plain (64 B/event).
    pub seal_columns: bool,
    /// Reclaim by erasing only the segments that hold dead rows (true) or by
    /// rewriting each affected agent store (false).
    pub erase_segments: bool,
    /// Experimental deferred durability: an append is acknowledged once its
    /// batch is written to the agent's journal; a background syncer makes it
    /// durable within ~2 ms, and the agent's next search or append waits for
    /// that sync (a crash can lose at most each agent's last turn). Off by
    /// default: one sync per turn before acknowledging (design decision).
    pub deferred_sync: bool,
    /// One merge thread for all agent stores (true) instead of one per agent.
    pub shared_merges: bool,
    /// Memory for decoded copies of sealed column blocks.
    pub column_cache_bytes: usize,
    /// One shared write log under all agent stores (commits of different
    /// agents share one sync) instead of a private journal per agent. Still
    /// one durable sync before each append is acknowledged. Needs
    /// `erase_segments` and no `deferred_sync`. Off by default.
    pub shared_log: bool,
    /// Shared log: size of each log file (zero-filled ahead of use).
    pub log_segment_bytes: u64,
    /// Shared log: write into zero-filled files so a sync flushes data only
    /// (no file size change for the filesystem journal to commit).
    pub log_preallocate: bool,
    /// Shared log: past this size, the agent stores whose unabsorbed rows
    /// keep its oldest file are flushed so the file can be removed.
    pub log_max_bytes: u64,
    /// Store an event whose (tool, args, text) repeats an earlier event of
    /// the same agent store once (no second record, no second index entry).
    pub dedupe_records: bool,
    /// Distinct texts remembered per agent store for record dedupe.
    pub dedupe_window: usize,
    /// Compress flush runs (and runs too small to train a dictionary) with
    /// the shipped starter dictionary.
    pub starter_dict: bool,
    /// Longest accepted search query in bytes.
    pub max_query_bytes: usize,
    /// Score every candidate exactly for the routing entropy (the engine
    /// default). Off here: search returns ids only, so the entropy covers
    /// the top k and only candidates that can reach the top k are scored
    /// exactly (the same ids in the same order either way).
    pub exact_entropy: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            archive_fast_level: 3,
            archive_heavy_level: 19,
            dedupe_chunk: 1024,
            dict: true,
            flush_text_bytes: 16 << 20,
            merge_fanout: 8,
            query_threads: 1,
            seal_columns: true,
            erase_segments: true,
            deferred_sync: false,
            shared_merges: true,
            column_cache_bytes: 128 << 20,
            shared_log: true,
            log_segment_bytes: 32 << 20,
            log_preallocate: true,
            log_max_bytes: 256 << 20,
            dedupe_records: true,
            dedupe_window: 1 << 18,
            starter_dict: true,
            max_query_bytes: 65_536,
            exact_entropy: false,
        }
    }
}

pub struct TraceDb {
    dir: PathBuf,
    opts: Options,
    archive: Arc<ZstdArchive>,
    parts: Arc<RwLock<HashMap<u32, Arc<Nine>>>>,
    cols: RwLock<ColStore>,
    /// Deferred durability: agents with acknowledged, unsynced batches.
    dirty: Arc<Mutex<std::collections::HashSet<u32>>>,
    syncer: Option<(Arc<std::sync::atomic::AtomicBool>, std::thread::JoinHandle<()>)>,
    /// Shared merges: one thread compacting every agent store in turn.
    merger: Option<(Arc<std::sync::atomic::AtomicBool>, std::thread::JoinHandle<()>)>,
    /// The shared write log, when enabled.
    log: Option<Arc<SharedLog>>,
    /// Shared log: flushes the stores that pin its oldest file once it
    /// outgrows `log_max_bytes`.
    checkpointer: Option<(Arc<std::sync::atomic::AtomicBool>, std::thread::JoinHandle<()>)>,
    tombstones: Mutex<File>,
    /// Appends take it shared; reclaim and checkpoints take it exclusively.
    reclaim_lock: RwLock<()>,
    /// Record dedupe: per agent, text key -> engine row holding it.
    seen: Mutex<HashMap<u32, dedupe::Window>>,
}

fn part_dir(dir: &Path, agent: u32) -> PathBuf {
    dir.join(format!("agent-{agent:05}"))
}

const CHECKPOINT: &str = "cols.ckpt";
const LOG_DIR: &str = "shared-log";

impl TraceDb {
    pub fn open(dir: &Path, opts: Options) -> Result<Self> {
        if opts.shared_log && !opts.erase_segments {
            return Err("the shared log needs reclaim by segment erase (erase_segments)".into());
        }
        if opts.shared_log && opts.deferred_sync {
            return Err("deferred durability is not available with the shared log".into());
        }
        fs::create_dir_all(dir)?;
        // The log opens first: agent stores replay their rows from it.
        let log = if opts.shared_log {
            let o = SharedLogOptions {
                segment_bytes: opts.log_segment_bytes,
                preallocate: opts.log_preallocate,
                ..SharedLogOptions::default()
            };
            Some(SharedLog::open(&dir.join(LOG_DIR), o)?)
        } else {
            None
        };
        let mut archive = ZstdArchive::new(opts.archive_fast_level, opts.archive_heavy_level, opts.dedupe_chunk, opts.dict);
        if opts.starter_dict {
            archive.starter = Some(oojuro_store::starter::v1().clone());
        }
        let archive = Arc::new(archive);
        let tomb_path = dir.join("tombstones.log");
        let tombstones = OpenOptions::new().create(true).append(true).read(true).open(&tomb_path)?;
        // Columns: checkpoint (if valid) + replay of each agent's rows past its watermark.
        let (cols, marks) = match ColStore::load(&dir.join(CHECKPOINT), opts.seal_columns, opts.column_cache_bytes)? {
            Some((c, m)) => (c, m),
            None => (ColStore::with_cache(opts.seal_columns, opts.column_cache_bytes), HashMap::new()),
        };
        let parts: Arc<RwLock<HashMap<u32, Arc<Nine>>>> = Arc::new(RwLock::new(HashMap::new()));
        let dirty: Arc<Mutex<std::collections::HashSet<u32>>> = Arc::default();
        let syncer = opts.deferred_sync.then(|| {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (p, d, s) = (Arc::clone(&parts), Arc::clone(&dirty), Arc::clone(&stop));
            let h = std::thread::spawn(move || {
                while !s.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    let agents: Vec<u32> = d.lock().unwrap().drain().collect();
                    for a in agents {
                        let nine = p.read().unwrap().get(&a).cloned();
                        if let Some(n) = nine {
                            let _ = n.sync_journal();
                        }
                    }
                }
            });
            (stop, h)
        });
        let merger = opts.shared_merges.then(|| {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (p, s) = (Arc::clone(&parts), Arc::clone(&stop));
            let h = std::thread::Builder::new()
                .name("trace-merge".into())
                .spawn(move || {
                    while !s.load(std::sync::atomic::Ordering::Relaxed) {
                        let stores: Vec<Arc<Nine>> = p.read().unwrap().values().cloned().collect();
                        let mut merged = 0;
                        for n in stores {
                            if s.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            merged += n.compact().unwrap_or(0);
                        }
                        if merged == 0 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        } else {
                            trim_heap();
                        }
                    }
                })
                .expect("merge thread");
            (stop, h)
        });
        let checkpointer = log.as_ref().map(|log| {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (p, s, log, max) = (Arc::clone(&parts), Arc::clone(&stop), Arc::clone(log), opts.log_max_bytes);
            let h = std::thread::Builder::new()
                .name("trace-log-checkpoint".into())
                .spawn(move || {
                    while !s.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        if log.bytes() <= max {
                            continue;
                        }
                        for store in log.pinning() {
                            let nine = u32::try_from(store).ok().and_then(|a| p.read().unwrap().get(&a).cloned());
                            if let Some(n) = nine {
                                let _ = n.flush();
                            }
                        }
                        let _ = log.checkpoint();
                    }
                })
                .expect("checkpoint thread");
            (stop, h)
        });
        let db = Self {
            dir: dir.to_path_buf(),
            opts,
            merger,
            log,
            checkpointer,
            archive,
            parts,
            cols: RwLock::new(cols),
            dirty,
            syncer,
            tombstones: Mutex::new(tombstones),
            reclaim_lock: RwLock::new(()),
            seen: Mutex::new(HashMap::new()),
        };
        let mut agents = Vec::new();
        for e in fs::read_dir(dir)? {
            let name = e?.file_name();
            if let Some(a) =
                name.to_str().and_then(|n| n.strip_prefix("agent-")).and_then(|n| n.parse::<u32>().ok())
            {
                agents.push(a);
            }
        }
        // Stores with rows in the shared log open now so they replay them.
        if let Some(log) = &db.log {
            for s in log.stores() {
                agents.push(u32::try_from(s).map_err(|_| "shared log names a store that is not an agent")?);
            }
        }
        agents.sort_unstable();
        agents.dedup();
        for a in agents {
            let nine = db.part(a)?;
            db.replay(a, &nine, marks.get(&a).copied().unwrap_or(0) as usize)?;
        }
        let text = fs::read_to_string(&tomb_path)?;
        for line in text.split_inclusive('\n') {
            // a torn last line (no newline) is ignored: its delete was never
            // acknowledged, and a cut-off number must not name another event
            let Some(line) = line.strip_suffix('\n') else { break };
            match line.split_once(' ') {
                Some(("S", s)) => db.mark_session(s.parse()?),
                Some(("P", t)) => db.mark_before(t.parse()?),
                Some(("E", ids)) => {
                    let ids = ids.split(' ').map(str::parse).collect::<std::result::Result<Vec<u64>, _>>()?;
                    db.mark_ids(&ids);
                }
                _ => {}
            }
        }
        Ok(db)
    }

    fn nine_options(&self, agent: u32) -> NineOptions {
        NineOptions {
            shared_log: self.log.as_ref().map(|log| LogBinding { log: Arc::clone(log), store: u64::from(agent) }),
            flush_text_bytes: self.opts.flush_text_bytes,
            receipts: Receipts::Off,
            archive: self.archive.clone(),
            query_threads: self.opts.query_threads,
            merge_fanout: self.opts.merge_fanout,
            deferred_sync: self.opts.deferred_sync,
            background_merges: !self.opts.shared_merges,
            max_query_bytes: self.opts.max_query_bytes,
            routing_entropy: if self.opts.exact_entropy { RoutingEntropy::All } else { RoutingEntropy::TopK },
            ..NineOptions::default()
        }
    }

    fn part(&self, agent: u32) -> Result<Arc<Nine>> {
        if let Some(p) = self.parts.read().unwrap().get(&agent) {
            return Ok(Arc::clone(p));
        }
        let mut g = self.parts.write().unwrap();
        if let Some(p) = g.get(&agent) {
            return Ok(Arc::clone(p));
        }
        let nine = Arc::new(Nine::open_with(&part_dir(&self.dir, agent), self.nine_options(agent))?);
        g.insert(agent, Arc::clone(&nine));
        Ok(nine)
    }

    /// Apply an agent's records from row `from` on to the columns.
    fn replay(&self, agent: u32, nine: &Nine, from: usize) -> Result<()> {
        let n = nine.len();
        let mut row = from;
        while row < n {
            let recs = nine.records_from(row, 4096)?;
            if recs.is_empty() {
                break;
            }
            let mut c = self.cols.write().unwrap();
            let mut seen = self.seen.lock().unwrap();
            let window = seen.entry(agent).or_default();
            let mut slot = c.next_slot(agent);
            for (k, r) in recs.iter().enumerate() {
                let at = (row + k) as u32;
                let tool = r.metadata.get("t").map(String::as_str).unwrap_or("");
                let args = r.metadata.get("a").map(String::as_str).unwrap_or("");
                // duplicates first, as appended (a revision may complete a duplicated call)
                if let Some(b) = r.metadata.get("B") {
                    for e in dedupe::decode(b).ok_or("malformed duplicate entries")? {
                        Self::apply_dup(&mut c, agent, slot, at, &e);
                        slot += 1;
                    }
                }
                if let Some(meta) = r.metadata.get("e") {
                    let id: u64 = r.id.parse()?;
                    let p = parse_meta(meta)?;
                    let text_len = r.text.len() as u32;
                    Self::apply_col(&mut c, id, agent, slot, at, at, &p, tool, text_len, r.valid_at);
                    slot += 1;
                    if self.opts.dedupe_records && p.revises == 0 && !r.metadata.contains_key("x") {
                        let idx = is_indexed_kind(p.kind) && !r.text.is_empty();
                        window.insert(dedupe::key(idx, tool, args, &r.text), at, self.opts.dedupe_window);
                    }
                } else if let Some(h) = r.metadata.get("h") {
                    // a text holder: a deleted event's text kept for its duplicates
                    let kind: u8 = h.parse()?;
                    let tool_id = c.tool(tool);
                    let holder = Col {
                        id: r.id.parse()?,
                        ts: r.valid_at,
                        row: slot,
                        src: at,
                        host: at,
                        agent,
                        tool: tool_id,
                        kind,
                        text_len: r.text.len() as u32,
                        live: true,
                        holder: true,
                        ..Col::VACANT
                    };
                    c.put(holder);
                    slot += 1;
                    if self.opts.dedupe_records {
                        let idx = is_indexed_kind(kind) && !r.text.is_empty();
                        window.insert(dedupe::key(idx, tool, args, &r.text), at, self.opts.dedupe_window);
                    }
                } else if !r.metadata.contains_key("B") {
                    // an erased row (blank: no text, no metadata)
                    if r.text.is_empty() && r.metadata.is_empty() {
                        continue;
                    }
                    return Err("event metadata missing".into());
                }
            }
            row += recs.len();
        }
        Ok(())
    }

    /// A duplicate's slot: fields from its entry, text at the entry's source.
    fn apply_dup(c: &mut ColStore, agent: u32, slot: u32, host: u32, e: &DupEntry) {
        let tool = c.tool(&e.tool);
        let src = match e.src {
            Src::Row(r) => r,
            Src::Local(j) => host + j,
        };
        c.put(Col {
            id: e.id,
            ts: e.ts,
            duration: e.duration,
            session: e.session,
            parent: e.parent,
            row: slot,
            src,
            host,
            turn: e.turn,
            seq: e.seq,
            tin: e.tin,
            tout: e.tout,
            exit: e.exit,
            text_len: e.text_len,
            agent,
            tool,
            kind: e.kind,
            status: e.status,
            live: true,
            revision: false,
            dup: true,
            holder: false,
        });
    }

    /// A duplicate's entry as written (`src` relative to `host` when the
    /// source is at or after it).
    fn dup_entry(c: &Col, tools: &[String]) -> DupEntry {
        DupEntry {
            id: c.id,
            src: if c.src >= c.host { Src::Local(c.src - c.host) } else { Src::Row(c.src) },
            session: c.session,
            turn: c.turn,
            seq: c.seq,
            parent: c.parent,
            kind: c.kind,
            status: c.status,
            exit: c.exit,
            tin: c.tin,
            tout: c.tout,
            ts: c.ts,
            duration: c.duration,
            text_len: c.text_len,
            tool: tools[c.tool as usize].clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_col(c: &mut ColStore, id: u64, agent: u32, slot: u32, src: u32, host: u32, p: &Parsed, tool: &str, text_len: u32, ts: i64) {
        let tool = c.tool(tool);
        if p.revises != 0 {
            c.revise(p.revises, p.status, p.exit, p.duration);
            c.put(Col { id, kind: p.kind, revision: true, live: true, agent, row: slot, src, host, parent: p.revises, ..Col::VACANT });
            return;
        }
        c.put(Col {
                id,
                ts,
                duration: p.duration,
                session: p.session,
                parent: p.parent,
                row: slot,
                src,
                host,
                turn: p.turn,
                seq: p.seq,
                tin: p.tin,
                tout: p.tout,
                exit: p.exit,
                text_len,
                agent,
                tool,
                kind: p.kind,
                status: p.status,
                live: true,
                revision: false,
                dup: false,
                holder: false,
            });
    }

    /// Append one turn batch (one agent). Durable and searchable on return.
    pub fn append(&self, batch: &[EventIn]) -> Result<()> {
        let Some(first) = batch.first() else { return Ok(()) };
        let agent = first.agent;
        if batch.iter().any(|e| e.agent != agent) {
            return Err("a batch belongs to one agent".into());
        }
        let _g = self.reclaim_lock.read().unwrap();
        self.barrier(agent)?;
        self.append_agent(agent, batch)?;
        if self.opts.deferred_sync {
            self.dirty.lock().unwrap().insert(agent);
        }
        Ok(())
    }

    /// Deferred durability: make the agent's acknowledged batches durable
    /// before serving its next call.
    fn barrier(&self, agent: u32) -> Result<()> {
        if self.opts.deferred_sync {
            let nine = self.parts.read().unwrap().get(&agent).cloned();
            if let Some(n) = nine {
                n.sync_journal()?;
            }
        }
        Ok(())
    }

    /// Bulk path: many turn batches, one atomic durable commit per agent.
    /// Order within an agent is kept.
    pub fn append_many(&self, batches: &[Vec<EventIn>]) -> Result<()> {
        let _g = self.reclaim_lock.read().unwrap();
        let mut by_agent: BTreeMap<u32, Vec<EventIn>> = BTreeMap::new();
        for b in batches {
            for e in b {
                by_agent.entry(e.agent).or_default().push(*e);
            }
        }
        for (agent, evs) in by_agent {
            self.append_agent(agent, &evs)?;
        }
        Ok(())
    }

    fn append_agent(&self, agent: u32, batch: &[EventIn]) -> Result<()> {
        let nine = self.part(agent)?;
        // Split the batch into stored records and duplicates of stored text.
        let mut stored: Vec<usize> = Vec::with_capacity(batch.len());
        let mut keys: Vec<Option<dedupe::Key>> = Vec::with_capacity(batch.len());
        let mut dups: Vec<(usize, Src)> = Vec::new();
        if self.opts.dedupe_records {
            // hash outside the lock
            let hashed: Vec<Option<dedupe::Key>> =
                batch.iter().map(|e| dedupe_class(e).map(|idx| dedupe::key(idx, e.tool, e.args, e.text))).collect();
            let mut seen = self.seen.lock().unwrap();
            let window = seen.entry(agent).or_default();
            let mut local: dedupe::KeyMap<u32> = dedupe::KeyMap::default();
            for (i, h) in hashed.into_iter().enumerate() {
                let Some(k) = h else {
                    stored.push(i);
                    keys.push(None);
                    continue;
                };
                if let Some(r) = window.get(k, self.opts.dedupe_window) {
                    dups.push((i, Src::Row(r)));
                } else if let Some(&j) = local.get(&k) {
                    dups.push((i, Src::Local(j)));
                } else {
                    local.insert(k, stored.len() as u32);
                    stored.push(i);
                    keys.push(Some(k));
                }
            }
        } else {
            stored.extend(0..batch.len());
            keys.resize(batch.len(), None);
        }
        let mut records: Vec<(Record, bool)> = stored
            .iter()
            .map(|&i| {
                let e = &batch[i];
                let id = e.id.to_string();
                let key = if e.revises != 0 { format!("r{}", e.revises) } else { id.clone() };
                let mut r = Record::new(&id, &key, e.text, e.ts_us);
                let mut m = BTreeMap::new();
                m.insert("e".to_string(), meta_line(e));
                if !e.tool.is_empty() {
                    m.insert("t".to_string(), e.tool.to_string());
                }
                if !e.args.is_empty() {
                    m.insert("a".to_string(), e.args.to_string());
                }
                if self.opts.dedupe_records && e.revises == 0 && dedupe_class(e).is_none() {
                    m.insert("x".to_string(), "1".to_string());
                }
                r.metadata = m;
                (r, e.indexed && e.revises == 0 && !e.text.is_empty())
            })
            .collect();
        let entries: Vec<DupEntry> = dups
            .iter()
            .map(|&(i, src)| {
                let e = &batch[i];
                DupEntry {
                    id: e.id,
                    src,
                    session: e.session,
                    turn: e.turn,
                    seq: e.seq,
                    parent: e.parent,
                    kind: e.kind,
                    status: e.status,
                    exit: e.exit_code.unwrap_or(NO_EXIT),
                    tin: e.tokens_in,
                    tout: e.tokens_out,
                    ts: e.ts_us,
                    duration: e.duration_us,
                    text_len: e.text.len() as u32,
                    tool: e.tool.to_string(),
                }
            })
            .collect();
        if !entries.is_empty() {
            let b = dedupe::encode(&entries);
            match records.first_mut() {
                Some((r, _)) => {
                    r.metadata.insert("B".to_string(), b);
                }
                None => {
                    let id = format!("b{}", entries[0].id);
                    let mut r = Record::new(&id, &id, "", entries[0].ts);
                    r.metadata.insert("B".to_string(), b);
                    records.push((r, false));
                }
            }
        }
        let (_, first_row) = nine.insert_batch_rows(records)?;
        if self.opts.dedupe_records {
            let mut seen = self.seen.lock().unwrap();
            let window = seen.entry(agent).or_default();
            for (k, key) in keys.iter().enumerate() {
                if let Some(key) = key {
                    window.insert(*key, first_row + k as u32, self.opts.dedupe_window);
                }
            }
        }
        let mut c = self.cols.write().unwrap();
        let mut slot = c.next_slot(agent);
        // duplicates first: a stored status revision may complete a duplicated call
        for e in &entries {
            Self::apply_dup(&mut c, agent, slot, first_row, e);
            slot += 1;
        }
        for (k, &i) in stored.iter().enumerate() {
            let e = &batch[i];
            let p = Parsed {
                session: e.session,
                turn: e.turn,
                seq: e.seq,
                parent: e.parent,
                kind: e.kind,
                status: e.status,
                exit: e.exit_code.unwrap_or(NO_EXIT),
                tin: e.tokens_in,
                tout: e.tokens_out,
                duration: e.duration_us,
                revises: e.revises,
            };
            let row = first_row + k as u32;
            Self::apply_col(&mut c, e.id, agent, slot, row, row, &p, e.tool, e.text.len() as u32, e.ts_us);
            slot += 1;
        }
        Ok(())
    }

    /// Make the fresh segments immutable and checkpoint the columns (end of
    /// a bulk load). Appends wait while the checkpoint is written.
    pub fn flush(&self) -> Result<()> {
        let parts: Vec<Arc<Nine>> = self.parts.read().unwrap().values().cloned().collect();
        for p in parts {
            p.flush()?;
        }
        if let Some(log) = &self.log {
            // With no appends running, every row is now in a segment and the
            // log shrinks to one empty file.
            log.checkpoint()?;
        }
        let _g = self.reclaim_lock.write().unwrap();
        self.checkpoint()?;
        trim_heap();
        Ok(())
    }

    /// Major compaction: merge each agent store into one segment, so its
    /// originals land in one archive run at the heavy level with a trained
    /// dictionary (the cold tier). `threads` agents at a time.
    pub fn compact_archive(&self, threads: usize) -> Result<()> {
        let parts: Vec<Arc<Nine>> = self.parts.read().unwrap().values().cloned().collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let failed: Mutex<Option<String>> = Mutex::new(None);
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(p) = parts.get(i) else { break };
                    FORCE_HEAVY.with(|f| f.set(true));
                    let r = p.flush().and_then(|_| p.compact_all());
                    FORCE_HEAVY.with(|f| f.set(false));
                    if let Err(e) = r {
                        *failed.lock().unwrap() = Some(e.to_string());
                    }
                });
            }
        });
        if let Some(e) = failed.into_inner().unwrap() {
            return Err(e.into());
        }
        Ok(())
    }

    /// Caller holds `reclaim_lock` exclusively: no append is between its
    /// engine commit and its column update.
    fn checkpoint(&self) -> Result<()> {
        let marks: HashMap<u32, u32> =
            self.parts.read().unwrap().iter().map(|(a, n)| (*a, n.len() as u32)).collect();
        self.cols.read().unwrap().save(&self.dir.join(CHECKPOINT), &marks)?;
        Ok(())
    }

    /// (sealed column bytes, plain column bytes, id location bytes) in memory.
    pub fn column_bytes(&self) -> (usize, usize, usize) {
        self.cols.read().unwrap().bytes()
    }

    fn out(id: u64, c: &Col, rec: &Record, tools: &[String]) -> EventOut {
        EventOut {
            id,
            session: c.session,
            agent: c.agent,
            turn: c.turn,
            seq: c.seq,
            parent: c.parent,
            kind: c.kind,
            status: c.status,
            exit_code: (c.exit != NO_EXIT).then_some(c.exit),
            tokens_in: c.tin,
            tokens_out: c.tout,
            ts_us: c.ts,
            duration_us: c.duration,
            tool: tools[c.tool as usize].clone(),
            args: rec.metadata.get("a").cloned().unwrap_or_default(),
            text: rec.text.clone(),
        }
    }

    fn col(&self, id: u64) -> Option<Col> {
        self.cols.read().unwrap().get(id).filter(Col::is_event)
    }

    pub fn get(&self, id: u64) -> Result<Option<EventOut>> {
        let Some(c) = self.col(id) else { return Ok(None) };
        let rec = self.part(c.agent)?.record_at(c.src)?;
        let tools = self.cols.read().unwrap().tool_names();
        Ok(Some(Self::out(id, &c, &rec, &tools)))
    }

    pub fn text_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let Some(c) = self.col(id) else { return Ok(None) };
        let rec = self.part(c.agent)?.record_at(c.src)?;
        let t = rec.text.as_bytes();
        let e = end.min(t.len());
        Ok(Some(t[start.min(e)..e].to_vec()))
    }

    /// Fetch records in (agent, row) order, so rows sharing an archive frame
    /// decode it once; results keep the order of `ids`.
    fn fetch(&self, ids: &[(u64, Col)]) -> Result<Vec<EventOut>> {
        let tools = self.cols.read().unwrap().tool_names();
        let mut order: Vec<usize> = (0..ids.len()).collect();
        order.sort_by_key(|&i| (ids[i].1.agent, ids[i].1.src));
        let mut out: Vec<Option<EventOut>> = vec![None; ids.len()];
        let mut nine: Option<(u32, Arc<Nine>)> = None;
        for i in order {
            let (id, c) = &ids[i];
            if nine.as_ref().is_none_or(|(a, _)| *a != c.agent) {
                nine = Some((c.agent, self.part(c.agent)?));
            }
            let rec = nine.as_ref().unwrap().1.record_at(c.src)?;
            out[i] = Some(Self::out(*id, c, &rec, &tools));
        }
        Ok(out.into_iter().map(|x| x.expect("fetched")).collect())
    }

    fn session_cols(&self, session: u64) -> Vec<(u64, Col)> {
        let c = self.cols.read().unwrap();
        let mut v: Vec<(u64, Col)> = c
            .sessions
            .get(&session)
            .into_iter()
            .flatten()
            .filter_map(|id| c.get(*id).filter(Col::is_event).map(|x| (*id, x)))
            .collect();
        v.sort_by_key(|(_, x)| x.seq);
        v
    }

    pub fn session(&self, session: u64) -> Result<Vec<EventOut>> {
        self.fetch(&self.session_cols(session))
    }

    pub fn time_range(&self, from: i64, to: i64, limit: usize) -> Result<Vec<EventOut>> {
        let mut v: Vec<(i64, u64, Col)> = Vec::new();
        self.cols.read().unwrap().scan_time(from, to, |c| v.push((c.ts, c.id, *c)));
        v.sort_unstable_by_key(|x| (x.0, x.1));
        v.truncate(limit);
        let ids: Vec<(u64, Col)> = v.into_iter().map(|x| (x.1, x.2)).collect();
        self.fetch(&ids)
    }

    fn pred(c: &ColStore, f: &Filter) -> Option<columns::Pred> {
        let tool = match &f.tool {
            Some(t) => Some(*c.tool_ids.get(t)?),
            None => None,
        };
        Some(columns::Pred { kind: f.kind, status: f.status, tool })
    }

    /// Ids in (ts, id) order.
    pub fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let c = self.cols.read().unwrap();
        let Some(p) = Self::pred(&c, f) else { return Ok(Vec::new()) };
        Ok(c.first_where(f.from_us, f.to_us, f.agent, &p, limit).into_iter().map(|x| x.1).collect())
    }

    pub fn count(&self, f: &Filter) -> Result<u64> {
        let c = self.cols.read().unwrap();
        let Some(p) = Self::pred(&c, f) else { return Ok(0) };
        Ok(c.count_where(f.from_us, f.to_us, f.agent, &p))
    }

    /// The engine holding one agent's store, if it exists (probes and tools).
    pub fn engine(&self, agent: u32) -> Result<Option<Arc<Nine>>> {
        if !self.parts.read().unwrap().contains_key(&agent) && !part_dir(&self.dir, agent).exists() {
            return Ok(None);
        }
        Ok(Some(self.part(agent)?))
    }

    /// Run the shared merge step on every agent store until none merges.
    pub fn settle(&self) -> Result<usize> {
        let mut total = 0;
        loop {
            let stores: Vec<Arc<Nine>> = self.parts.read().unwrap().values().cloned().collect();
            let mut merged = 0;
            for n in stores {
                merged += n.compact()?;
            }
            if merged == 0 {
                return Ok(total);
            }
            total += merged;
        }
    }

    /// Top-k event ids for `query` in one agent's history as of `as_of`.
    pub fn search(&self, agent: u32, query: &str, as_of: i64, k: usize) -> Result<Vec<u64>> {
        Ok(self.search_where(agent, query, 0, as_of, k, &mut |_| true)?.into_iter().map(|x| x.id).collect())
    }

    /// Top-k hits for `query` in one agent's history, in the engine's
    /// order: valid time in `[from, to]` (the engine's view, so its
    /// statistics and identifier anchoring see only that window), only
    /// events `keep` accepts (it runs outside the column lock, so it may
    /// read the store). Hits on deleted or filtered events are replaced by
    /// asking the engine for more, until there are k or the engine has no
    /// more.
    pub fn search_where(
        &self,
        agent: u32,
        query: &str,
        from: i64,
        to: i64,
        k: usize,
        keep: &mut dyn FnMut(&EventInfo) -> bool,
    ) -> Result<Vec<SearchHit>> {
        if !self.parts.read().unwrap().contains_key(&agent) && !part_dir(&self.dir, agent).exists() {
            return Ok(Vec::new());
        }
        self.barrier(agent)?;
        let nine = self.part(agent)?;
        let mut want = k;
        loop {
            let m = nine.find(query, from, to, want)?;
            let got = m.matches.len();
            let cands: Vec<(EventInfo, f64, bool)> = {
                let c = self.cols.read().unwrap();
                let mut taken = std::collections::HashSet::new();
                m.matches
                    .iter()
                    .filter_map(|x| Some((x.id.parse::<u64>().ok()?, x.score_bits, x.continued)))
                    .filter_map(|(id, bits, continued)| {
                        // a hit is its record's text: report the record's event,
                        // or (deleted, or a holder) its earliest live duplicate
                        let x = c.get(id)?;
                        if x.is_event() {
                            return Some((x, bits, continued));
                        }
                        c.dups_of(agent, x.src)
                            .iter()
                            .filter_map(|d| c.get(*d))
                            .filter(|d| d.is_event() && d.ts <= to)
                            .min_by_key(|d| (d.ts, d.id))
                            .map(|d| (d, bits, continued))
                    })
                    .filter(|(x, _, _)| taken.insert(x.id))
                    .map(|(x, bits, continued)| (EventInfo::of(&x), bits, continued))
                    .collect()
            };
            let mut out = Vec::with_capacity(k.min(cands.len()));
            for (info, bits, continued) in cands {
                if out.len() == k {
                    break;
                }
                if keep(&info) {
                    out.push(SearchHit { id: info.id, bits, continued });
                }
            }
            // Dropped hits are replaced from further down while the engine
            // has more: fewer than `want` matches means it has no more.
            if out.len() >= k || got < want {
                return Ok(out);
            }
            want = want.saturating_mul(4);
        }
    }

    /// Typed fields of a live event (no text read).
    pub fn info(&self, id: u64) -> Option<EventInfo> {
        self.col(id).map(|c| EventInfo::of(&c))
    }

    /// Live events of a session in `seq` order (no text read).
    pub fn session_info(&self, session: u64) -> Vec<EventInfo> {
        self.session_cols(session).iter().map(|(_, c)| EventInfo::of(c)).collect()
    }

    /// Live events by id (`None` for unknown or deleted ids), read in store
    /// order so rows sharing an archive frame decode it once.
    pub fn get_many(&self, ids: &[u64]) -> Result<Vec<Option<EventOut>>> {
        let found: Vec<(usize, (u64, Col))> =
            ids.iter().enumerate().filter_map(|(i, id)| self.col(*id).map(|c| (i, (*id, c)))).collect();
        let pairs: Vec<(u64, Col)> = found.iter().map(|(_, p)| *p).collect();
        let mut out: Vec<Option<EventOut>> = vec![None; ids.len()];
        for ((i, _), e) in found.iter().zip(self.fetch(&pairs)?) {
            out[*i] = Some(e);
        }
        Ok(out)
    }

    /// Delete events by id: a durable tombstone, then the events leave every
    /// read; [`Self::reclaim`] removes their bytes. Returns how many were live.
    pub fn delete_events(&self, ids: &[u64]) -> Result<usize> {
        let live: Vec<u64> = {
            let c = self.cols.read().unwrap();
            let mut v: Vec<u64> = ids.iter().copied().filter(|id| c.get(*id).is_some_and(|x| x.is_event())).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        for chunk in live.chunks(4096) {
            let mut line = String::from("E");
            for id in chunk {
                line.push(' ');
                line.push_str(&id.to_string());
            }
            line.push('\n');
            self.log_tombstone(&line)?;
        }
        self.mark_ids(&live);
        Ok(live.len())
    }

    fn mark_ids(&self, ids: &[u64]) {
        let mut c = self.cols.write().unwrap();
        for id in ids {
            c.kill(*id);
        }
    }

    /// Erasure check: decode every row of the given agent stores (all when
    /// `None`) and count rows whose text or metadata contains a needle,
    /// split by whether a live event still needs the row (its own text, or
    /// the text of a live duplicate). After a reclaim, rows nothing live
    /// needs must not contain erased text.
    pub fn erasure_scan(&self, agents: Option<&[u32]>, needles: &[Vec<u8>]) -> Result<ErasureScan> {
        let mut live_rows: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        self.cols.read().unwrap().for_each(|x| {
            if x.is_event() {
                live_rows.insert((x.agent, x.src));
            }
        });
        let parts: Vec<(u32, Arc<Nine>)> = self
            .parts
            .read()
            .unwrap()
            .iter()
            .filter(|(a, _)| agents.is_none_or(|v| v.contains(a)))
            .map(|(a, n)| (*a, Arc::clone(n)))
            .collect();
        let pats: Vec<&[u8]> = needles.iter().filter(|n| !n.is_empty()).map(Vec::as_slice).collect();
        let index: Vec<usize> = needles.iter().enumerate().filter(|(_, n)| !n.is_empty()).map(|(i, _)| i).collect();
        let ac = aho_corasick::AhoCorasick::new(&pats)?;
        let mut scan = ErasureScan {
            live_by_needle: vec![0; needles.len()],
            dead_by_needle: vec![0; needles.len()],
            ..ErasureScan::default()
        };
        let mut hit = vec![false; needles.len()];
        for (agent, nine) in parts {
            let mut row = 0;
            while row < nine.len() {
                let recs = nine.records_from(row, 4096)?;
                if recs.is_empty() {
                    break;
                }
                for (k, r) in recs.iter().enumerate() {
                    scan.rows += 1;
                    hit.iter_mut().for_each(|h| *h = false);
                    let mut any = false;
                    for hay in std::iter::once(r.text.as_bytes()).chain(r.metadata.values().map(|v| v.as_bytes())) {
                        for m in ac.find_overlapping_iter(hay) {
                            hit[index[m.pattern().as_usize()]] = true;
                            any = true;
                        }
                    }
                    if !any {
                        continue;
                    }
                    let live = live_rows.contains(&(agent, (row + k) as u32));
                    if live {
                        scan.live_rows_with_needle += 1;
                    } else {
                        scan.dead_rows_with_needle += 1;
                    }
                    for (i, h) in hit.iter().enumerate() {
                        if *h {
                            if live {
                                scan.live_by_needle[i] += 1;
                            } else {
                                scan.dead_by_needle[i] += 1;
                            }
                        }
                    }
                }
                row += recs.len();
            }
        }
        Ok(scan)
    }

    /// (events, tokens_in, tokens_out, tool_calls, tool_errors, text_bytes)
    pub fn agg_session(&self, session: u64) -> (u64, u64, u64, u64, u64, u64) {
        let (mut ev, mut tin, mut tout, mut calls, mut errs, mut bytes) = (0, 0, 0, 0, 0, 0);
        for (_, c) in self.session_cols(session) {
            ev += 1;
            tin += c.tin as u64;
            tout += c.tout as u64;
            bytes += c.text_len as u64;
            if c.kind == KIND_TOOL_CALL {
                calls += 1;
                errs += (c.status == ST_ERROR) as u64;
            }
        }
        (ev, tin, tout, calls, errs, bytes)
    }

    /// (tool, calls, errors, total duration) sorted by tool name; whole
    /// sealed blocks inside the window answer from their summaries.
    pub fn agg_tools(&self, from: i64, to: i64) -> Vec<(String, u64, u64, i64)> {
        let c = self.cols.read().unwrap();
        let mut acc: HashMap<u16, (u64, u64, i64)> = HashMap::new();
        let mut part: HashMap<u16, (u64, u64, i64)> = HashMap::new();
        c.scan_agg(
            from,
            to,
            |s| {
                for &(t, n, e, d) in &s.tools {
                    let a = acc.entry(t).or_default();
                    a.0 += n;
                    a.1 += e;
                    a.2 += d;
                }
            },
            |x| {
                if x.kind == KIND_TOOL_CALL {
                    let a = part.entry(x.tool).or_default();
                    a.0 += 1;
                    a.1 += (x.status == ST_ERROR) as u64;
                    a.2 += x.duration;
                }
            },
        );
        for (t, v) in part {
            let a = acc.entry(t).or_default();
            a.0 += v.0;
            a.1 += v.1;
            a.2 += v.2;
        }
        let mut v: Vec<(String, u64, u64, i64)> =
            acc.into_iter().map(|(t, a)| (c.tools[t as usize].clone(), a.0, a.1, a.2)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    pub fn top_sessions(&self, from: i64, to: i64, n: usize) -> Vec<(u64, u64)> {
        let c = self.cols.read().unwrap();
        let mut acc: HashMap<u64, u64> = HashMap::new();
        let mut part: HashMap<u64, u64> = HashMap::new();
        c.scan_agg(
            from,
            to,
            |s| {
                for &(sess, t) in &s.sessions {
                    *acc.entry(sess).or_default() += t;
                }
            },
            |x| *part.entry(x.session).or_default() += x.tin as u64 + x.tout as u64,
        );
        for (s, t) in part {
            *acc.entry(s).or_default() += t;
        }
        let mut v: Vec<(u64, u64)> = acc.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    fn log_tombstone(&self, line: &str) -> Result<()> {
        let mut f = self.tombstones.lock().unwrap();
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        Ok(())
    }

    fn mark_session(&self, session: u64) {
        let mut c = self.cols.write().unwrap();
        let ids = c.sessions.get(&session).cloned().unwrap_or_default();
        for id in ids {
            c.kill(id);
        }
    }

    fn mark_before(&self, ts: i64) {
        self.cols.write().unwrap().kill_before(ts);
    }

    pub fn delete_session(&self, session: u64) -> Result<()> {
        self.log_tombstone(&format!("S {session}\n"))?;
        self.mark_session(session);
        Ok(())
    }

    pub fn purge_before(&self, ts: i64) -> Result<()> {
        self.log_tombstone(&format!("P {ts}\n"))?;
        self.mark_before(ts);
        Ok(())
    }

    /// Physically remove deleted events: every agent store holding a dead
    /// record is rewritten with its live records (and the revisions of live
    /// calls) into a new directory that replaces the old one. The column
    /// checkpoint is dropped first (a crash mid-way rebuilds the columns
    /// from the records) and written again at the end; then the tombstone
    /// log is emptied.
    pub fn reclaim(&self) -> Result<()> {
        let _g = self.reclaim_lock.write().unwrap();
        let mut agents: Vec<u32> = Vec::new();
        self.cols.read().unwrap().for_each(|x| {
            if !x.revision && !x.live {
                agents.push(x.agent);
            }
        });
        agents.sort_unstable();
        agents.dedup();
        if agents.is_empty() {
            return Ok(());
        }
        let ck = self.dir.join(CHECKPOINT);
        if ck.exists() {
            fs::remove_file(&ck)?;
            File::open(&self.dir)?.sync_all()?;
        }
        for agent in agents {
            if self.opts.erase_segments {
                self.erase_agent(agent)?;
            } else {
                self.rewrite_agent(agent)?;
            }
        }
        if let Some(log) = &self.log {
            // Erased text may still be in the shared log (also rows already
            // in segments, kept there by other stores' unabsorbed rows):
            // every store absorbs its rows, then the log is zeroed and
            // removed. Appends wait on the reclaim lock meanwhile.
            let parts: Vec<Arc<Nine>> = self.parts.read().unwrap().values().cloned().collect();
            for p in parts {
                p.flush()?;
            }
            log.scrub()?;
        }
        self.checkpoint()?;
        let f = self.tombstones.lock().unwrap();
        f.set_len(0)?;
        f.sync_all()?;
        Ok(())
    }

    /// The agent's slots and which of them survive a reclaim: live events,
    /// revisions of live calls, and live duplicates (holders are decided by
    /// the caller).
    fn reclaim_plan(&self, agent: u32) -> (Vec<Col>, HashMap<u64, bool>, Arc<Vec<String>>) {
        let c = self.cols.read().unwrap();
        let mut slots = Vec::new();
        c.for_each(|x| {
            if x.agent == agent {
                slots.push(*x);
            }
        });
        let live_event: HashMap<u64, bool> =
            slots.iter().filter(|x| !x.revision && !x.holder).map(|x| (x.id, x.live)).collect();
        let alive: HashMap<u64, bool> = slots
            .iter()
            .map(|x| {
                let a = if x.holder {
                    false
                } else if x.revision {
                    live_event.get(&x.parent).copied().unwrap_or(false)
                } else {
                    x.live
                };
                (x.id, a)
            })
            .collect();
        (slots, alive, c.tool_names())
    }

    /// Erase an agent's dead rows in place (segment rewrites; rows stable).
    /// Reference counts: a row keeps its text while its own event or a live
    /// duplicate needs it; a row whose own event died but whose text is
    /// still referenced becomes a holder (the dead event's fields go); a row
    /// nothing references is blanked. Duplicate entries of dead events leave
    /// their host's `B` list.
    fn erase_agent(&self, agent: u32) -> Result<()> {
        let nine = self.part(agent)?;
        let (slots, alive, tools) = self.reclaim_plan(agent);
        let mut own: HashMap<u32, &Col> = HashMap::new();
        let mut hosted: HashMap<u32, Vec<&Col>> = HashMap::new();
        let mut need: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut kept: HashMap<u64, (u32, u32, u32)> = HashMap::new();
        for x in &slots {
            if x.dup {
                hosted.entry(x.host).or_default().push(x);
                if alive[&x.id] {
                    need.insert(x.src);
                }
            } else {
                own.insert(x.src, x);
            }
            if alive[&x.id] {
                kept.insert(x.id, (x.row, x.src, x.host));
            }
        }
        let mut rows: Vec<u32> = own.iter().filter(|(_, x)| !alive[&x.id]).map(|(r, _)| *r).collect();
        rows.extend(hosted.iter().filter(|(_, v)| v.iter().any(|x| !alive[&x.id])).map(|(r, _)| *r));
        rows.sort_unstable();
        rows.dedup();
        let mut blank = Vec::new();
        let mut meta: HashMap<u32, BTreeMap<String, String>> = HashMap::new();
        let mut holders = std::collections::HashSet::new();
        let mut gone_text = std::collections::HashSet::new();
        for r in rows {
            let o = own.get(&r).copied();
            let mut live_hosted: Vec<&Col> = hosted.get(&r).into_iter().flatten().copied().filter(|x| alive[&x.id]).collect();
            live_hosted.sort_by_key(|x| x.id);
            let changed = hosted.get(&r).is_some_and(|v| v.iter().any(|x| !alive[&x.id]));
            let b = (!live_hosted.is_empty())
                .then(|| dedupe::encode(&live_hosted.iter().map(|x| Self::dup_entry(x, &tools)).collect::<Vec<_>>()));
            if o.is_some_and(|x| alive[&x.id]) {
                if changed {
                    let mut m = nine.record_at(r)?.metadata;
                    m.remove("B");
                    if let Some(b) = b {
                        m.insert("B".into(), b);
                    }
                    meta.insert(r, m);
                }
                continue;
            }
            if need.contains(&r) {
                let o = o.ok_or("referenced text row without an owner")?;
                let rec = nine.record_at(r)?;
                let mut m = BTreeMap::new();
                m.insert("h".to_string(), o.kind.to_string());
                for k in ["t", "a"] {
                    if let Some(v) = rec.metadata.get(k) {
                        m.insert(k.to_string(), v.clone());
                    }
                }
                if let Some(b) = b {
                    m.insert("B".into(), b);
                }
                if m != rec.metadata {
                    meta.insert(r, m);
                }
                kept.insert(o.id, (o.row, o.src, o.host));
                holders.insert(o.id);
            } else {
                blank.push(r);
                gone_text.insert(r);
                if let Some(b) = b {
                    meta.insert(r, BTreeMap::from([("B".to_string(), b)]));
                }
            }
        }
        nine.rewrite_rows(&blank, &meta)?;
        self.cols.write().unwrap().rewrite_agent(agent, &kept, &holders);
        if let Some(w) = self.seen.lock().unwrap().get_mut(&agent) {
            w.forget(&gone_text);
        }
        Ok(())
    }

    /// Rewrite an agent store with what survives a reclaim (see
    /// [`Self::erase_agent`]): live records (without their duplicate
    /// lists), holders for referenced text of dead events, then the live
    /// duplicates as bundle records.
    fn rewrite_agent(&self, agent: u32) -> Result<()> {
        if self.log.is_some() {
            return Err("rewrite reclaim is not available with the shared log".into());
        }
        let old = self.part(agent)?;
        let (slots, alive, tools) = self.reclaim_plan(agent);
        let mut need: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for x in slots.iter().filter(|x| x.dup && alive[&x.id]) {
            need.insert(x.src);
        }
        // records to copy, in row order: (old row, slot owner)
        let mut copy: Vec<&Col> = slots.iter().filter(|x| !x.dup && (alive[&x.id] || need.contains(&x.src))).collect();
        copy.sort_by_key(|x| x.src);
        let mut dups: Vec<&Col> = slots.iter().filter(|x| x.dup && alive[&x.id]).collect();
        dups.sort_by_key(|x| x.row);
        let new_dir = self.dir.join(format!("agent-{agent:05}.new"));
        if new_dir.exists() {
            fs::remove_dir_all(&new_dir)?;
        }
        let mut kept: HashMap<u64, (u32, u32, u32)> = HashMap::with_capacity(copy.len() + dups.len());
        let mut holders = std::collections::HashSet::new();
        let mut new_row: HashMap<u32, u32> = HashMap::with_capacity(copy.len());
        {
            let fresh = Nine::open_with(&new_dir, self.nine_options(agent))?;
            for chunk in copy.chunks(2048) {
                let mut recs = Vec::with_capacity(chunk.len());
                for x in chunk {
                    let mut r = old.record_at(x.src)?;
                    r.metadata.remove("B");
                    let holder = !alive[&x.id];
                    if holder {
                        let mut m = BTreeMap::new();
                        m.insert("h".to_string(), x.kind.to_string());
                        for k in ["t", "a"] {
                            if let Some(v) = r.metadata.get(k) {
                                m.insert(k.to_string(), v.clone());
                            }
                        }
                        r.metadata = m;
                    }
                    let idx = is_indexed_kind(x.kind)
                        && !x.revision
                        && !r.text.is_empty()
                        && (holder || !r.metadata.contains_key("x") || !self.opts.dedupe_records);
                    recs.push((r, idx));
                }
                let (_, first) = fresh.insert_batch_rows(recs)?;
                for (k, x) in chunk.iter().enumerate() {
                    new_row.insert(x.src, first + k as u32);
                    kept.insert(x.id, (x.row, first + k as u32, first + k as u32));
                    if !alive[&x.id] {
                        holders.insert(x.id);
                    }
                }
            }
            for chunk in dups.chunks(4096) {
                let entries: Vec<DupEntry> = chunk
                    .iter()
                    .map(|x| {
                        let mut e = Self::dup_entry(x, &tools);
                        e.src = Src::Row(new_row[&x.src]);
                        e
                    })
                    .collect();
                let id = format!("b{}", entries[0].id);
                let mut r = Record::new(&id, &id, "", entries[0].ts);
                r.metadata.insert("B".to_string(), dedupe::encode(&entries));
                let (_, host) = fresh.insert_batch_rows(vec![(r, false)])?;
                for x in chunk {
                    kept.insert(x.id, (x.row, new_row[&x.src], host));
                }
            }
            fresh.flush()?;
        }
        // swap: drop the old engine, move directories, fsync the parent
        let mut parts = self.parts.write().unwrap();
        parts.remove(&agent);
        drop(old);
        let cur = part_dir(&self.dir, agent);
        let trash = self.dir.join(format!("agent-{agent:05}.old"));
        fs::rename(&cur, &trash)?;
        fs::rename(&new_dir, &cur)?;
        File::open(&self.dir)?.sync_all()?;
        fs::remove_dir_all(&trash)?;
        let nine = Arc::new(Nine::open_with(&cur, self.nine_options(agent))?);
        parts.insert(agent, nine);
        drop(parts);
        self.cols.write().unwrap().rewrite_agent(agent, &kept, &holders);
        // rows moved: rebuild the agent's dedupe window from the new store's text rows
        let mut seen = self.seen.lock().unwrap();
        let w = seen.entry(agent).or_default();
        *w = dedupe::Window::default();
        if self.opts.dedupe_records {
            let c = self.cols.read().unwrap();
            let nine = self.part(agent)?;
            for x in &copy {
                if x.revision {
                    continue;
                }
                let r = nine.record_at(new_row[&x.src])?;
                if r.metadata.contains_key("x") {
                    continue;
                }
                let t = r.metadata.get("t").map(String::as_str).unwrap_or("");
                let a = r.metadata.get("a").map(String::as_str).unwrap_or("");
                let idx = is_indexed_kind(x.kind) && !r.text.is_empty();
                w.insert(dedupe::key(idx, t, a, &r.text), new_row[&x.src], self.opts.dedupe_window);
            }
            drop(c);
        }
        Ok(())
    }

    /// Every live event in (session, seq) order.
    pub fn export_rows(&self, mut f: impl FnMut(&EventOut) -> Result<()>) -> Result<u64> {
        let mut sessions: Vec<u64> = self.cols.read().unwrap().sessions.keys().copied().collect();
        sessions.sort_unstable();
        let mut n = 0;
        for s in sessions {
            for e in self.session(s)? {
                f(&e)?;
                n += 1;
            }
        }
        Ok(n)
    }

    pub fn files(&self) -> Vec<PathBuf> {
        fn walk(p: &Path, out: &mut Vec<PathBuf>) {
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        walk(&path, out);
                    } else {
                        out.push(path);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.dir, &mut out);
        out
    }

    /// Bytes on disk by role: index segments, archive runs (originals),
    /// journals, column checkpoint, other.
    pub fn disk_breakdown(&self) -> [(&'static str, u64); 5] {
        let mut b = [("index", 0u64), ("archive", 0), ("journal", 0), ("columns", 0), ("other", 0)];
        for p in self.files() {
            let n = fs::metadata(&p).map_or(0, |m| m.len());
            let name = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
            let k = if name.ends_with(".idx") {
                0
            } else if name.ends_with(".src") {
                1
            } else if name.starts_with("wal-") || name.starts_with("log-") || name.starts_with("spare-") {
                2
            } else if name == CHECKPOINT {
                3
            } else {
                4
            };
            b[k].1 += n;
        }
        b
    }

    /// Shared log counters (rounds = syncs, batches = commits), if enabled.
    pub fn shared_log_stats(&self) -> Option<SharedLogStats> {
        self.log.as_ref().map(|l| l.stats())
    }

    pub fn disk_bytes(&self) -> u64 {
        self.files().iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum()
    }

    /// Decode every stored row of every agent store (live, dead, erased)
    /// and count rows whose text or metadata contains any needle.
    pub fn scan_decoded(&self, needles: &[Vec<u8>]) -> Result<u64> {
        let parts: Vec<Arc<Nine>> = self.parts.read().unwrap().values().cloned().collect();
        let mut hits = 0;
        let find = |hay: &[u8]| needles.iter().any(|n| hay.windows(n.len()).any(|w| w == &n[..]));
        for nine in parts {
            let mut row = 0;
            while row < nine.len() {
                let recs = nine.records_from(row, 4096)?;
                if recs.is_empty() {
                    break;
                }
                for r in &recs {
                    if find(r.text.as_bytes()) || r.metadata.values().any(|v| find(v.as_bytes())) {
                        hits += 1;
                    }
                }
                row += recs.len();
            }
        }
        Ok(hits)
    }

    /// Engine records across agent stores (events stored by reference to an
    /// earlier record's text have none of their own).
    pub fn stored_records(&self) -> usize {
        self.parts.read().unwrap().values().map(|n| n.len()).sum()
    }

    /// (distinct texts remembered for record dedupe, duplicate slots).
    pub fn dedupe_stats(&self) -> (usize, usize) {
        let w = self.seen.lock().unwrap().values().map(dedupe::Window::len).sum();
        let d = self.cols.read().unwrap().dup_slots;
        (w, d)
    }

    /// Live events (status revisions excluded).
    pub fn len(&self) -> usize {
        let mut n = 0;
        self.cols.read().unwrap().for_each(|c| n += c.is_event() as usize);
        n
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for TraceDb {
    fn drop(&mut self) {
        if let Some((stop, h)) = self.checkpointer.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = h.join();
        }
        if let Some((stop, h)) = self.merger.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = h.join();
        }
        if let Some((stop, h)) = self.syncer.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = h.join();
        }
        for n in self.parts.read().unwrap().values() {
            let _ = n.sync_journal();
        }
    }
}

/// TraceBench v1 indexing rule (user, output, tool_call, tool_result).
pub fn is_indexed_kind(kind: u8) -> bool {
    matches!(kind, 1 | 3 | 4 | 5)
}

/// `Some(indexed)` when an event may be stored by reference: not a status
/// revision, and indexed exactly by the standard rule (so a replayed
/// record's class follows from its kind).
fn dedupe_class(e: &EventIn) -> Option<bool> {
    if e.revises != 0 {
        return None;
    }
    let idx = e.indexed && !e.text.is_empty();
    (idx == (is_indexed_kind(e.kind) && !e.text.is_empty())).then_some(idx)
}
