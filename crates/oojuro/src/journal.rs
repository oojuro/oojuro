//! The journal used like SQLite: explicit writes and reads over the
//! recorder's store (`docs/FRONT_DOOR.md`).
//!
//! [`Journal`] is the single writer: it owns the store (exclusive lock,
//! the capture id space and writer registry) and is held either by the
//! recorder (`oojurod`) or, when no recorder runs, by the calling process
//! for one call. [`Reader`] is a cheap, thread-safe read handle on the same
//! open store: the recorder's query threads use it while the recorder
//! appends.
//!
//! Explicit records (`remember`) are ordinary records of the writer's
//! private store with kind `explicit`: indexed like captured text, redacted
//! like captured text, and in the same id space. A key groups versions:
//! every version of `(writer, key)` shares one session (a 64-bit hash of
//! the pair) and its `seq` is the version number; the newest version (by
//! valid time, then id) as of the view time is the one search and `get`
//! return; `history` returns all of them.
use crate::{
    Result,
    config::HARD_MAX_QUERY_BYTES,
    types::{Filters, ForgetReport, Proof, Record, Ref, Remembered, SearchArgs, Stats, Target, WriterStats},
};
use oojuro_capture::{
    hash64,
    ingest::{Capture, DirectReport, Options as CaptureOptions, TRACE_DIR, capture_trace_options},
    kind, redact,
    state::STATE_FILE,
    time::{format_rfc3339_us, now_us},
};
use oojuro_trace::{EventIn, EventInfo, EventOut, Filter, SearchHit, TraceDb};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

/// Largest explicit record accepted (bigger texts: split them).
pub const MAX_TEXT_BYTES: usize = 16 << 20;
pub const MAX_KEY_BYTES: usize = 1024;
/// Most results one search returns.
pub const MAX_K: usize = 10_000;
/// Texts of forgotten records searched for by the erasure proof.
pub const PROOF_NEEDLES: usize = 64;
/// Raw file scans use needles at least this long (shorter ones match noise).
pub const RAW_NEEDLE_MIN: usize = 16;

/// Session of every version of `(writer, key)`.
pub fn key_session(writer: &str, key: &str) -> u64 {
    hash64(&format!("oojuro:key\0{writer}\0{key}"))
}

/// Session of a writer's explicit records without a key.
pub fn plain_session(writer: &str) -> u64 {
    hash64(&format!("oojuro:explicit\0{writer}"))
}

/// Store options for the front door and the recorder: the capture
/// defaults, with the engine accepting queries up to the hard cap (the
/// configured `max_query_bytes` is enforced per call).
pub fn store_options() -> CaptureOptions {
    let mut trace = capture_trace_options();
    trace.max_query_bytes = HARD_MAX_QUERY_BYTES;
    CaptureOptions { dedupe_stats: false, trace, ..CaptureOptions::default() }
}

/// [`store_options`] for an existing store: its journal mode (one shared
/// write log under the writer stores, or a private journal per store) is
/// fixed when it is created, so it is read from the store; a new store
/// gets the engine default.
pub fn store_options_for(store: &Path) -> CaptureOptions {
    let mut o = store_options();
    let trace = store.join(TRACE_DIR);
    if trace.join("shared-log").is_dir() {
        o.trace.shared_log = true;
    } else if std::fs::read_dir(&trace)
        .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("agent-")))
        .unwrap_or(false)
    {
        o.trace.shared_log = false;
    }
    o
}

/// Whether `store` holds a journal.
pub fn exists(store: &Path) -> bool {
    store.join(STATE_FILE).is_file() || store.join(TRACE_DIR).is_dir()
}

/// Create a private directory (mode 700) and its parents.
pub fn private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[derive(Default)]
struct Writers {
    by_name: BTreeMap<String, u32>,
    by_agent: HashMap<u32, String>,
}

/// Thread-safe read handle on an open journal.
#[derive(Clone)]
pub struct Reader {
    db: Arc<TraceDb>,
    writers: Arc<RwLock<Writers>>,
    store: PathBuf,
}

/// The journal's single writer (and a reader of it).
pub struct Journal {
    cap: Capture,
    reader: Reader,
}

impl Journal {
    /// Open (or create) the journal at `store` with the default options.
    pub fn open(store: &Path) -> Result<Self> {
        Self::open_with(store, store_options_for(store))
    }

    /// Open (or create) with explicit store options.
    pub fn open_with(store: &Path, opts: CaptureOptions) -> Result<Self> {
        private_dir(store)?;
        Ok(Self::from_capture(Capture::open(store, opts)?))
    }

    /// Open an existing journal (reads never create one).
    pub fn open_existing(store: &Path) -> Result<Self> {
        if !exists(store) {
            return Err(format!("no journal at {}", store.display()).into());
        }
        Self::open(store)
    }

    /// Wrap a capture owner (the recorder opens it with its own options).
    pub fn from_capture(cap: Capture) -> Self {
        let reader = Reader { db: Arc::clone(&cap.db), writers: Arc::default(), store: cap.dir.clone() };
        let j = Self { cap, reader };
        j.sync_writers();
        j
    }

    pub fn capture(&mut self) -> &mut Capture {
        &mut self.cap
    }

    pub fn reader(&self) -> Reader {
        self.reader.clone()
    }

    /// Publish newly registered writers to readers.
    pub fn sync_writers(&self) {
        sync_writers(&self.cap, &self.reader);
    }

    /// The single writer's operations on this journal.
    pub fn owner(&mut self) -> Owner<'_> {
        Owner { cap: &mut self.cap, reader: &self.reader }
    }

    /// See [`Owner::remember`].
    pub fn remember(&mut self, writer: &str, text: &str, key: Option<&str>, at_us: Option<i64>) -> Result<Remembered> {
        self.owner().remember(writer, text, key, at_us)
    }

    /// See [`Owner::forget`].
    pub fn forget(&mut self, target: &Target, proof: bool) -> Result<ForgetReport> {
        self.owner().forget(target, proof)
    }

    /// Save the capture state if anything was committed.
    pub fn close(mut self) -> Result<()> {
        self.cap.save_if_dirty()
    }
}

/// Publish the writer registry of `cap` to `reader` when it grew.
pub fn sync_writers(cap: &Capture, reader: &Reader) {
    let st = &cap.state.writers;
    if reader.writers.read().unwrap().by_name.len() == st.len() {
        return;
    }
    let mut w = reader.writers.write().unwrap();
    w.by_name = st.clone();
    w.by_agent = st.iter().map(|(n, a)| (*a, n.clone())).collect();
}

/// The store owner's write operations, on an open capture and a reader of
/// it: used by [`Journal`], and by the recorder between file commits.
pub struct Owner<'a> {
    pub cap: &'a mut Capture,
    pub reader: &'a Reader,
}

impl Owner<'_> {
    pub fn sync_writers(&self) {
        sync_writers(self.cap, self.reader);
    }

    /// Store `text` for `writer` as an explicit record; with a key, a new
    /// version of it. Secrets are masked first (as on capture). Valid time
    /// is now unless `at_us` backdates it; recorded time is always now.
    pub fn remember(&mut self, writer: &str, text: &str, key: Option<&str>, at_us: Option<i64>) -> Result<Remembered> {
        crate::paths::check_writer(writer)?;
        if text.trim().is_empty() {
            return Err("nothing to remember: empty text".into());
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(
                format!("text is {} bytes; one record holds at most {MAX_TEXT_BYTES} (split it)", text.len()).into()
            );
        }
        if let Some(k) = key
            && (k.is_empty() || k.len() > MAX_KEY_BYTES || k.chars().any(char::is_control))
        {
            return Err(format!("invalid key (1-{MAX_KEY_BYTES} bytes, no control characters)").into());
        }
        let mut counts = redact::Counts::new();
        let text = self.cap.redactor().redact(text, &mut counts);
        let agent = self.cap.writer_store(writer)?;
        self.sync_writers();
        let now = now_us();
        let ts = at_us.unwrap_or(now);
        let (session, version) = match key {
            Some(k) => {
                let s = key_session(writer, k);
                let last = self.cap.db.session_info(s).iter().filter(|x| x.kind == kind::EXPLICIT).map(|x| x.seq).max();
                (s, last.map_or(1, |v| v + 1))
            }
            None => (plain_session(writer), 0),
        };
        let mut args = serde_json::Map::new();
        args.insert("rt".into(), now.into());
        if let Some(k) = key {
            args.insert("key".into(), k.into());
        }
        let args = serde_json::Value::Object(args).to_string();
        let id = self.cap.next_id();
        let ev = EventIn {
            id,
            session,
            parent_session: 0,
            agent,
            turn: 0,
            seq: version,
            parent: 0,
            revises: 0,
            kind: kind::EXPLICIT,
            status: 0,
            exit_code: None,
            tokens_in: 0,
            tokens_out: 0,
            ts_us: ts,
            duration_us: 0,
            tool: "",
            args: &args,
            text: &text,
            indexed: true,
        };
        self.cap.append_explicit(&[ev])?;
        Ok(Remembered {
            id,
            writer: writer.to_string(),
            key: key.map(str::to_string),
            version,
            ts_us: ts,
            ts: format_rfc3339_us(ts),
            redacted: counts.values().sum(),
            bytes: text.len(),
        })
    }

    /// Append records captured from no log file (the model API proxy):
    /// one writer's batch, redacted again and checked against the settings
    /// (`record`, pause, `exclude`, thinking) like captured records.
    pub fn ingest(&mut self, via: &str, source: &str, records: Vec<oojuro_capture::DirectRec>) -> Result<DirectReport> {
        if via.is_empty() || via.len() > 64 {
            return Err("ingest: `via` must name the capture mode".into());
        }
        if let Some(r) = records.first() {
            crate::paths::check_writer(&r.writer)?;
        }
        let rep = self.cap.append_direct(via, source, records)?;
        self.sync_writers();
        Ok(rep)
    }

    /// Delete records and prove their text is gone: tombstones, then the
    /// engine's reclaim rewrites every affected segment without them, then
    /// (with `proof`) every row of the affected stores is decoded and every
    /// file of the store read byte for byte, looking for their text.
    pub fn forget(&mut self, target: &Target, proof: bool) -> Result<ForgetReport> {
        let r = self.reader.clone();
        let ids = r.target_ids(target)?;
        if ids.is_empty() {
            return Ok(ForgetReport { forgotten: 0, writers: Vec::new(), proof: None });
        }
        let infos: Vec<EventInfo> = ids.iter().filter_map(|id| self.cap.db.info(*id)).collect();
        let agents: BTreeSet<u32> = infos.iter().map(|x| x.agent).collect();
        let mut writers: Vec<String> = agents.iter().map(|a| r.writer_name(*a)).collect();
        writers.sort();
        // the texts to look for afterwards (all, or an even sample)
        let mut needles: Vec<Vec<u8>> = Vec::new();
        if proof {
            let step = ids.len().div_ceil(PROOF_NEEDLES).max(1);
            let sample: Vec<u64> = ids.iter().step_by(step).copied().collect();
            let mut seen = BTreeSet::new();
            for e in self.cap.db.get_many(&sample)?.into_iter().flatten() {
                if !e.text.is_empty() && seen.insert(e.text.clone()) {
                    needles.push(e.text.into_bytes());
                }
            }
        }
        let n = self.cap.db.delete_events(&ids)?;
        self.cap.db.reclaim()?;
        let proof = if proof { Some(self.prove(&agents.into_iter().collect::<Vec<_>>(), &needles)?) } else { None };
        Ok(ForgetReport { forgotten: n, writers, proof })
    }

    fn prove(&self, agents: &[u32], needles: &[Vec<u8>]) -> Result<Proof> {
        let scan = self.cap.db.erasure_scan(Some(agents), needles)?;
        let mut held_live: Vec<bool> = scan.live_by_needle.iter().map(|n| *n > 0).collect();
        // raw bytes of every file: a needle no live record holds must be absent
        let raw: Vec<(usize, &[u8])> = needles
            .iter()
            .enumerate()
            .filter(|(_, n)| n.len() >= RAW_NEEDLE_MIN)
            .map(|(i, n)| (i, n.as_slice()))
            .collect();
        let (files, hits) = scan_files(&self.cap.dir, &raw.iter().map(|x| x.1).collect::<Vec<_>>())?;
        let suspect: Vec<usize> =
            hits.iter().zip(&raw).filter(|(h, (i, _))| **h > 0 && !held_live[*i]).map(|(_, (i, _))| *i).collect();
        if !suspect.is_empty() {
            // the same text may live on in another writer's store
            let others: Vec<u32> =
                self.reader.agents(&[]).into_iter().map(|x| x.0).filter(|a| !agents.contains(a)).collect();
            if !others.is_empty() {
                let sub: Vec<Vec<u8>> = suspect.iter().map(|i| needles[*i].clone()).collect();
                let s2 = self.cap.db.erasure_scan(Some(&others), &sub)?;
                for (k, i) in suspect.iter().enumerate() {
                    held_live[*i] |= s2.live_by_needle[k] > 0;
                }
            }
        }
        let files_with_text =
            hits.iter().zip(&raw).filter(|(h, (i, _))| **h > 0 && !held_live[*i]).map(|(h, _)| *h).sum();
        Ok(Proof {
            needles: needles.len(),
            rows_scanned: scan.rows,
            dead_rows_with_text: scan.dead_rows_with_needle,
            live_rows_with_text: scan.live_rows_with_needle,
            files_scanned: files,
            files_with_text,
            proven: scan.dead_rows_with_needle == 0 && files_with_text == 0,
        })
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.cap.save_if_dirty();
    }
}

/// Read every file under `dir` in chunks; per needle, the number of files
/// whose bytes contain it.
pub fn scan_files(dir: &Path, needles: &[&[u8]]) -> Result<(u64, Vec<u64>)> {
    use std::io::Read;
    let mut hits = vec![0u64; needles.len()];
    let mut files = 0u64;
    if needles.is_empty() {
        return Ok((0, hits));
    }
    let ac = aho_corasick::AhoCorasick::new(needles)?;
    let overlap = needles.iter().map(|n| n.len()).max().unwrap_or(1) - 1;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let Ok(mut f) = std::fs::File::open(&p) else { continue };
            files += 1;
            let mut found = vec![false; needles.len()];
            let mut buf: Vec<u8> = Vec::with_capacity((8 << 20) + overlap);
            let mut chunk = vec![0u8; 8 << 20];
            loop {
                let n = f.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                for m in ac.find_overlapping_iter(&buf) {
                    found[m.pattern().as_usize()] = true;
                }
                let keep = buf.len().min(overlap);
                buf.drain(..buf.len() - keep);
            }
            for (h, f) in hits.iter_mut().zip(found) {
                *h += f as u64;
            }
        }
    }
    Ok((files, hits))
}

fn kind_codes(names: &[String]) -> Result<Option<Vec<u8>>> {
    if names.is_empty() {
        return Ok(None);
    }
    names
        .iter()
        .map(|n| kind::code(n).ok_or_else(|| format!("unknown kind {n:?} (kinds: {})", kind::NAMES.join(", ")).into()))
        .collect::<Result<Vec<u8>>>()
        .map(Some)
}

/// A UTF-8 prefix of `s` of at most `max` bytes.
fn prefix(s: &str, max: usize) -> &str {
    let mut e = s.len().min(max);
    while !s.is_char_boundary(e) {
        e -= 1;
    }
    &s[..e]
}

impl Reader {
    pub fn store(&self) -> &Path {
        &self.store
    }

    pub fn db(&self) -> &TraceDb {
        &self.db
    }

    pub fn writer_name(&self, agent: u32) -> String {
        self.writers.read().unwrap().by_agent.get(&agent).cloned().unwrap_or_else(|| format!("store:{agent}"))
    }

    pub fn agent_of(&self, writer: &str) -> Option<u32> {
        self.writers.read().unwrap().by_name.get(writer).copied()
    }

    /// (store number, writer) of the given writers (unknown ones skipped),
    /// or of every writer when none are given.
    pub fn agents(&self, writers: &[String]) -> Vec<(u32, String)> {
        let w = self.writers.read().unwrap();
        if writers.is_empty() {
            return w.by_name.iter().map(|(n, a)| (*a, n.clone())).collect();
        }
        writers.iter().filter_map(|n| w.by_name.get(n).map(|a| (*a, n.clone()))).collect()
    }

    pub fn writer_names(&self) -> Vec<String> {
        self.writers.read().unwrap().by_name.keys().cloned().collect()
    }

    fn record(&self, e: &EventOut) -> Record {
        let meta: serde_json::Value = if e.args.is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_str(&e.args).unwrap_or_else(|_| serde_json::json!({ "args": e.args }))
        };
        let key =
            (e.kind == kind::EXPLICIT).then(|| meta.get("key").and_then(|k| k.as_str()).map(str::to_string)).flatten();
        Record {
            id: e.id,
            writer: self.writer_name(e.agent),
            kind: kind::name(e.kind).to_string(),
            key,
            seq: e.seq,
            ts_us: e.ts_us,
            ts: format_rfc3339_us(e.ts_us),
            recorded_us: meta.get("rt").and_then(|v| v.as_i64()),
            tool: e.tool.clone(),
            session: format!("{:016x}", e.session),
            turn: e.turn,
            agent_id: format!("{:016x}", e.session),
            parent_id: self.db.parent_of(e.session).map(|p| format!("{p:016x}")),
            meta,
            text: e.text.clone(),
        }
    }

    /// Live versions of `(writer, key)` valid by `as_of`, oldest first.
    fn versions(&self, writer: &str, key: &str, as_of: i64) -> Vec<EventInfo> {
        let Some(agent) = self.agent_of(writer) else { return Vec::new() };
        let mut v: Vec<EventInfo> = self
            .db
            .session_info(key_session(writer, key))
            .into_iter()
            .filter(|x| x.kind == kind::EXPLICIT && x.ts_us <= as_of && x.agent == agent)
            .collect();
        v.sort_by_key(|x| (x.ts_us, x.id));
        v
    }

    /// Ids a forget target names (live records only).
    pub fn target_ids(&self, t: &Target) -> Result<Vec<u64>> {
        Ok(match t {
            Target::Id { id } => match self.db.info(*id) {
                Some(_) => vec![*id],
                None => return Err(format!("no record {id}").into()),
            },
            Target::Key { key, writer } => {
                let ids: Vec<u64> = self.versions(writer, key, i64::MAX).iter().map(|x| x.id).collect();
                // verify against the stored key (64-bit session hash)
                self.db
                    .get_many(&ids)?
                    .into_iter()
                    .flatten()
                    .filter(|e| self.record(e).key.as_deref() == Some(key))
                    .map(|e| e.id)
                    .collect()
            }
            Target::OlderThan { before_us, writer } => self.ids_in(writer.as_deref(), i64::MIN, *before_us)?,
            Target::Window { from_us, to_us, writer } => {
                self.ids_in(writer.as_deref(), *from_us, to_us.saturating_add(1))?
            }
        })
    }

    /// Live event ids with valid time in `[from, to)`, of one writer or all.
    fn ids_in(&self, writer: Option<&str>, from: i64, to: i64) -> Result<Vec<u64>> {
        let agent = match writer {
            Some(w) => match self.agent_of(w) {
                Some(a) => Some(a),
                None => return Err(format!("no writer {w:?} in this journal").into()),
            },
            None => None,
        };
        let f = Filter { agent, from_us: from, to_us: to, ..Filter::default() };
        self.db.filter(&f, usize::MAX)
    }

    /// Where a session's context window starts: the id of its newest
    /// context boundary (a compaction, `oojuro_capture::context`), found by
    /// reading the session's `system` records newest first. The agent's
    /// context holds the records from there on; `None`: the session was
    /// never compacted, so it holds all of them.
    pub fn context_start(&self, session: u64) -> Result<Option<u64>> {
        let mut sys: Vec<u64> =
            self.db.session_info(session).into_iter().filter(|x| x.kind == kind::SYSTEM).map(|x| x.id).collect();
        sys.sort_unstable_by(|a, b| b.cmp(a));
        for chunk in sys.chunks(32) {
            for e in self.db.get_many(chunk)?.into_iter().flatten() {
                let meta: serde_json::Value = serde_json::from_str(&e.args).unwrap_or_default();
                let sub = meta.get("sub").and_then(|s| s.as_str()).unwrap_or("");
                if oojuro_capture::context::is_boundary(e.kind, sub) {
                    return Ok(Some(e.id));
                }
            }
        }
        Ok(None)
    }

    /// The session and first id that `outside_context_of` leaves out.
    fn context_cut(&self, f: &Filters) -> Result<Option<(u64, u64)>> {
        let Some(s) = &f.outside_context_of else { return Ok(None) };
        let session = u64::from_str_radix(s, 16).map_err(|_| format!("not a session id: {s:?} (16 hex digits)"))?;
        Ok(Some((session, self.context_start(session)?.unwrap_or(0))))
    }

    /// Bits-ranked search over the writers' stores; hits carry an exact
    /// excerpt and its byte range. A store's hits past the query's
    /// identifier-anchored view (`oojuro_trace::SearchHit::continued`) never
    /// move above that store's anchored hits.
    pub fn search(&self, a: &SearchArgs) -> Result<Vec<Ref>> {
        let hits = self.search_hits(a)?;
        let ids: Vec<u64> = hits.iter().map(|h| h.1).collect();
        let events = self.db.get_many(&ids)?;
        let pq = prefix(&a.query, 65_536);
        let mut out = Vec::with_capacity(hits.len());
        for ((bits, _), e) in hits.iter().zip(events) {
            let Some(e) = e else { continue }; // deleted meanwhile
            let rec = self.record(&e);
            let (start, end) = match oojuro_nine::extract_passage(&e.text, pq, a.excerpt_bytes.max(1)) {
                Ok(Some(s)) => (s.start, s.end),
                _ => (0, prefix(&e.text, a.excerpt_bytes.max(1)).len()),
            };
            out.push(Ref {
                id: e.id,
                writer: rec.writer,
                kind: rec.kind,
                key: rec.key,
                ts_us: e.ts_us,
                ts: rec.ts,
                score_bits: *bits,
                start,
                end,
                len: e.text.len(),
                excerpt: e.text[start..end].to_string(),
                session: rec.session,
                turn: rec.turn,
            });
        }
        Ok(out)
    }

    /// [`Self::search`] without reading any text: the ranked `(bits, id)`
    /// list (anchors of `--when` and text queries of `periods` use it).
    pub fn search_hits(&self, a: &SearchArgs) -> Result<Vec<(f64, u64)>> {
        if a.query.trim().is_empty() {
            return Err("empty query".into());
        }
        let limit = a.max_query_bytes.min(HARD_MAX_QUERY_BYTES);
        if a.query.len() > limit {
            return Err(format!("query is {} bytes; max_query_bytes is {limit}", a.query.len()).into());
        }
        if a.k == 0 || a.k > MAX_K {
            return Err(format!("k must be in 1..={MAX_K}").into());
        }
        let f = &a.filters;
        let now = now_us();
        let as_of = f.as_of_us.unwrap_or(now);
        let from = f.from_us.unwrap_or(0);
        let to = f.to_us.unwrap_or(i64::MAX).min(as_of);
        if from > to {
            return Ok(Vec::new());
        }
        let mut kinds = kind_codes(&f.kinds)?;
        if f.key.is_some() {
            if kinds.as_ref().is_some_and(|k| !k.contains(&kind::EXPLICIT)) {
                return Ok(Vec::new());
            }
            kinds = Some(vec![kind::EXPLICIT]);
        }
        let cut = self.context_cut(f)?;
        // One list per writer, in its store's order (see `merge_lists`).
        let mut lists: Vec<Vec<SearchHit>> = Vec::new();
        for (agent, writer) in self.agents(&f.writers) {
            let plain = plain_session(&writer);
            let want_session = f.key.as_deref().map(|k| key_session(&writer, k));
            let mut newest: HashMap<u64, Option<u64>> = HashMap::new();
            let mut keep = |x: &EventInfo| {
                if x.ts_us < from || x.ts_us > to {
                    return false;
                }
                if cut.is_some_and(|(s, start)| x.session == s && x.id >= start) {
                    return false; // still in the agent's context window
                }
                if kinds.as_ref().is_some_and(|k| !k.contains(&x.kind)) {
                    return false;
                }
                if want_session.is_some_and(|s| s != x.session) {
                    return false;
                }
                if x.kind != kind::EXPLICIT || x.session == plain {
                    return true;
                }
                // a key's newest version as of the view wins
                let n = *newest.entry(x.session).or_insert_with(|| {
                    self.db
                        .session_info(x.session)
                        .into_iter()
                        .filter(|v| v.kind == kind::EXPLICIT && v.agent == agent && v.ts_us <= as_of)
                        .max_by_key(|v| (v.ts_us, v.id))
                        .map(|v| v.id)
                });
                n == Some(x.id)
            };
            // [from, to] is the engine's view (its statistics and identifier
            // anchoring see only that window); `keep` applies the rest.
            lists.push(self.db.search_where(agent, &a.query, from, to, a.k, &mut keep)?);
        }
        // the stores' lists merged in the engine's order (`SearchHit::rank_cmp`)
        Ok(merge_lists(&lists, a.k).into_iter().map(|h| (h.bits, h.id)).collect())
    }

    /// A record by id (any writer).
    pub fn get_id(&self, id: u64) -> Result<Option<Record>> {
        Ok(self.db.get(id)?.map(|e| self.record(&e)))
    }

    /// The newest version of `(writer, key)` as of `as_of` (default now).
    pub fn get_key(&self, writer: &str, key: &str, as_of: Option<i64>) -> Result<Option<Record>> {
        let v = self.versions(writer, key, as_of.unwrap_or(i64::MAX));
        for x in v.iter().rev() {
            if let Some(e) = self.db.get(x.id)? {
                let r = self.record(&e);
                if r.key.as_deref() == Some(key) {
                    return Ok(Some(r));
                }
            }
        }
        Ok(None)
    }

    /// Every live version of `(writer, key)`, oldest first.
    pub fn history(&self, writer: &str, key: &str) -> Result<Vec<Record>> {
        let ids: Vec<u64> = self.versions(writer, key, i64::MAX).iter().map(|x| x.id).collect();
        Ok(self
            .db
            .get_many(&ids)?
            .into_iter()
            .flatten()
            .map(|e| self.record(&e))
            .filter(|r| r.key.as_deref() == Some(key))
            .collect())
    }

    /// Ids of live records matching the filters in (valid time, id) order.
    fn range_ids(&self, f: &Filters, limit: usize) -> Result<Vec<u64>> {
        let from = f.from_us.unwrap_or(i64::MIN);
        let to = f.to_us.unwrap_or(i64::MAX).min(f.as_of_us.unwrap_or(i64::MAX));
        if from > to || limit == 0 {
            return Ok(Vec::new());
        }
        let kinds = kind_codes(&f.kinds)?;
        let agents = self.agents(&f.writers);
        if !f.writers.is_empty() && agents.is_empty() {
            return Ok(Vec::new());
        }
        let cut = self.context_cut(f)?;
        // ask for as many more as the cut can remove
        let fetch = cut.map_or(limit, |(s, from)| {
            limit.saturating_add(self.db.session_info(s).iter().filter(|x| x.id >= from).count())
        });
        let mut v: Vec<(i64, u64)> = Vec::new();
        if let Some(key) = &f.key {
            if kinds.as_ref().is_some_and(|k| !k.contains(&kind::EXPLICIT)) {
                return Ok(Vec::new());
            }
            for (_, w) in &agents {
                v.extend(self.versions(w, key, to).iter().filter(|x| x.ts_us >= from).map(|x| (x.ts_us, x.id)));
            }
        } else {
            let agent_list: Vec<Option<u32>> =
                if f.writers.is_empty() { vec![None] } else { agents.iter().map(|x| Some(x.0)).collect() };
            let kind_list: Vec<Option<u8>> = match &kinds {
                None => vec![None],
                Some(k) => k.iter().map(|x| Some(*x)).collect(),
            };
            for a in &agent_list {
                for k in &kind_list {
                    let flt =
                        Filter { agent: *a, kind: *k, from_us: from, to_us: to.saturating_add(1), ..Filter::default() };
                    for id in self.db.filter(&flt, fetch)? {
                        if let Some(x) = self.db.info(id)
                            && !cut.is_some_and(|(s, from)| x.session == s && x.id >= from)
                        {
                            v.push((x.ts_us, id));
                        }
                    }
                }
            }
        }
        v.sort_unstable();
        v.dedup();
        v.truncate(limit);
        Ok(v.into_iter().map(|x| x.1).collect())
    }

    /// How many live records match the filters.
    pub fn count(&self, f: &Filters) -> Result<u64> {
        Ok(self.range_ids(f, usize::MAX)?.len() as u64)
    }

    /// Records with valid time in `[from, to]`, oldest first (exact reads,
    /// every version), at most `limit`.
    pub fn range(&self, f: &Filters, limit: usize) -> Result<Vec<Record>> {
        let ids = self.range_ids(f, limit)?;
        Ok(self.db.get_many(&ids)?.into_iter().flatten().map(|e| self.record(&e)).collect())
    }

    /// Every record matching the filters, oldest first, in batches.
    pub fn export(&self, f: &Filters, sink: &mut dyn FnMut(Record) -> Result<()>) -> Result<u64> {
        let ids = self.range_ids(f, usize::MAX)?;
        let mut n = 0;
        for chunk in ids.chunks(1024) {
            for e in self.db.get_many(chunk)?.into_iter().flatten() {
                sink(self.record(&e))?;
                n += 1;
            }
        }
        Ok(n)
    }

    pub fn stats(&self) -> Result<Stats> {
        let mut s = Stats { store: self.store.display().to_string(), ..Stats::default() };
        for (agent, writer) in self.agents(&[]) {
            let mut w = WriterStats { writer, ..WriterStats::default() };
            for (k, name) in kind::NAMES.iter().enumerate() {
                let f = Filter {
                    agent: Some(agent),
                    kind: Some(k as u8),
                    from_us: i64::MIN,
                    to_us: i64::MAX,
                    ..Filter::default()
                };
                let n = self.db.count(&f)?;
                if n > 0 {
                    w.kinds.insert(name.to_string(), n);
                    *s.kinds.entry(name.to_string()).or_default() += n;
                    w.records += n;
                }
            }
            s.records += w.records;
            s.writers.push(w);
        }
        s.disk_bytes = self.db.disk_bytes();
        s.disk = self.db.disk_breakdown().iter().map(|(k, v)| (k.to_string(), *v)).collect();
        Ok(s)
    }
}

/// The best `k` of the stores' ranked lists, each in its store's order:
/// repeatedly the better head by [`SearchHit::rank_cmp`], the engine's own
/// order (bits, then newer, then id as text). Each list is sorted by that
/// order (a continued part follows its anchored part), so the result is
/// the stores' hits in that order, and a smaller `k` is always a prefix of
/// a larger one.
fn merge_lists(lists: &[Vec<SearchHit>], k: usize) -> Vec<SearchHit> {
    let mut at = vec![0usize; lists.len()];
    let mut out = Vec::with_capacity(k.min(lists.iter().map(Vec::len).sum()));
    while out.len() < k {
        let mut best: Option<(SearchHit, usize)> = None;
        for (i, list) in lists.iter().enumerate() {
            if let Some(h) = list.get(at[i])
                && best.is_none_or(|(b, _)| h.rank_cmp(&b).is_lt())
            {
                best = Some((*h, i));
            }
        }
        let Some((h, i)) = best else { break };
        out.push(h);
        at[i] += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(bits: f64, time: i64, id: u64, continued: bool) -> SearchHit {
        SearchHit { id, bits, continued, time, row: id }
    }

    #[test]
    fn merging_ranked_lists_is_the_engine_order_and_keeps_continued_hits_last() {
        let mut s = 7u64;
        let mut next = |m: u64| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) % m
        };
        let mut id = 0;
        for round in 0..300 {
            let lists: Vec<Vec<SearchHit>> = (0..1 + next(4))
                .map(|_| {
                    let mut l: Vec<SearchHit> = (0..next(30))
                        .map(|_| {
                            id += 1 + next(3); // ids cross decimal lengths (9, 10, 99, 100, ...)
                            hit(next(6) as f64 - 2.0, next(3) as i64, id, false) // many exact ties
                        })
                        .collect();
                    l.sort_by(|a, b| a.rank_cmp(b));
                    l
                })
                .collect();
            let mut all: Vec<SearchHit> = lists.iter().flatten().copied().collect();
            all.sort_by(|a, b| a.rank_cmp(b));
            for k in [0, 1, 2, 5, 40, 200] {
                let got = merge_lists(&lists, k);
                assert_eq!(got, all[..k.min(all.len())], "round {round} k {k}");
            }
        }
        // a store's continued hits (here 50 and 40 bits) stay after its anchored one
        let lists = vec![
            vec![hit(1.0, 0, 1, false), hit(50.0, 0, 2, true), hit(40.0, 0, 3, true)],
            vec![hit(30.0, 0, 4, false), hit(0.5, 0, 5, false)],
        ];
        let ids: Vec<u64> = merge_lists(&lists, 5).iter().map(|h| h.id).collect();
        assert_eq!(ids, vec![4, 1, 2, 3, 5]);
    }

    #[test]
    fn decimal_text_order_is_string_order() {
        let mut s = 11u64;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            s
        };
        let mut cases = vec![(0, 0), (1, 10), (9, 10), (10, 100), (12, 1), (u64::MAX, 1), (u64::MAX, u64::MAX / 10)];
        for _ in 0..20_000 {
            let (a, b) = (next() >> (next() % 64), next() >> (next() % 64));
            cases.push((a, b));
        }
        for (a, b) in cases {
            assert_eq!(oojuro_trace::cmp_decimal_text(a, b), a.to_string().cmp(&b.to_string()), "{a} {b}");
        }
    }
}
