//! Import (one shot) and tail (follow mode) over log directories.
//!
//! One pass ([`Capture::poll`]) lists every `*.jsonl` under the given
//! directories, registers new files, and for each file reads the complete
//! lines past its committed offset, parses and redacts them, and commits.
//!
//! Commit unit: one chunk of one file (at most `batch_bytes` of lines). Its
//! records get the next ids, grouped by writer in ascending store number, and
//! each group is one durable `TraceDb::append`. Then the file's offset and
//! parse context move past the chunk. So the committed records always form
//! a prefix of the id range, and on open every record beyond the saved
//! `next_id` is found by probing ids; its source position (`src`) is skipped
//! when the line is read again. A restart neither duplicates nor loses.
//!
//! An incomplete last line (no newline yet) is left for the next pass. A file
//! whose identity changes or that shrinks below its offset is read again
//! from the start as a new file.
//!
//! A [`Policy`] (settings: `record`, pause, `exclude`, thinking) decides per
//! parsed record whether it is stored and indexed; a withheld record takes
//! no id, and the result of a withheld tool call is withheld with it.
//!
//! One process owns a store: [`Capture::open`] takes an exclusive lock on
//! `<store>/LOCK`. Explicit writes (records that come from no log) take
//! the next ids of the same id space ([`Capture::append_explicit`]); they
//! carry kind [`kind::EXPLICIT`] and no source position. Records from the
//! model API proxy ([`Capture::append_direct`]) carry their capture kinds
//! and `"via"` instead of a source position.
use crate::{
    DirectRec, FileCtx, Format, ParseCounts, Rec, Result, claude, codex, kind, marker, redact,
    sink::{self, args_json, to_event},
    state::{FileState, State},
    stats::Stats,
    time::now_us,
};
use oojuro_trace::{EventIn, TraceDb};
use std::{
    collections::HashMap,
    sync::Arc,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Options {
    /// Raw bytes of lines per commit (one file chunk).
    pub batch_bytes: usize,
    /// Read size.
    pub read_chunk: usize,
    /// Measure exact and chunk-level duplicates (memory: ~40 B per record + chunk).
    pub dedupe_stats: bool,
    /// Save the state file at most this often (it is also saved when files
    /// or writers are registered, and at the end of an import).
    pub save_every: Duration,
    pub trace: oojuro_trace::Options,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            batch_bytes: 16 << 20,
            read_chunk: 4 << 20,
            dedupe_stats: true,
            save_every: Duration::from_secs(2),
            trace: capture_trace_options(),
        }
    }
}

/// Store options for capture: smaller per-store fresh segments (many writers
/// in one process), everything else as the engine defaults.
pub fn capture_trace_options() -> oojuro_trace::Options {
    oojuro_trace::Options { flush_text_bytes: 4 << 20, column_cache_bytes: 32 << 20, ..oojuro_trace::Options::default() }
}

/// What happens to one parsed record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Store it, indexed for search or stored only.
    Store { indexed: bool },
    /// Do not store it (the reason is counted in the stats).
    Withhold(&'static str),
}

impl Decision {
    /// The default: store, indexed by [`kind::indexed`] unless the record
    /// comes from Oojuro itself ([`Rec::from_oojuro`]).
    pub fn default_for(rec: &Rec) -> Self {
        Decision::Store { indexed: kind::indexed(rec.kind) && !rec.from_oojuro() }
    }
}

/// Recording policy: settings applied to every parsed record before it is
/// stored. `refresh` runs at the start of every pass (re-read settings).
pub trait Policy: Send {
    fn refresh(&mut self) {}
    /// `source` is the transcript path the record was read from.
    fn decide(&mut self, rec: &Rec, source: &str) -> Decision;
}

/// A record about to be appended: (index in the chunk, id, parent, args, indexed).
type Pending = (usize, u64, u64, String, bool);

pub struct Capture {
    pub dir: PathBuf,
    /// Shared so other threads (the query server) can read while this
    /// owner appends; every engine read is `&self`.
    pub db: Arc<TraceDb>,
    pub state: State,
    pub stats: Stats,
    red: redact::Redactor,
    /// Source positions already stored beyond the saved state -> their ids.
    skip: HashMap<(u32, u64, u32), u64>,
    opts: Options,
    last_save: Instant,
    dirty: bool,
    policy: Option<Box<dyn Policy>>,
    /// Open tool calls of records from no log file, per writer (in memory:
    /// a result arriving after a restart is not linked to its call).
    direct: HashMap<String, FileCtx>,
    _lock: File,
}

/// What [`Capture::append_direct`] did with a batch.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirectReport {
    pub stored: u64,
    pub withheld: u64,
    /// Id of the first stored record (0 when none was stored).
    pub first_id: u64,
}

/// Writers whose open tool calls are remembered for direct records.
const DIRECT_WRITERS: usize = 4096;

pub const TRACE_DIR: &str = "trace";

impl Capture {
    pub fn open(dir: &Path, opts: Options) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let lock = lock_store(dir)?;
        let db = Arc::new(TraceDb::open(&dir.join(TRACE_DIR), opts.trace.clone())?);
        let mut state = State::load(dir)?;
        // Records committed after the last state save: ids are a dense prefix.
        let mut skip = HashMap::new();
        let mut id = state.next_id;
        while let Some(ev) = db.get(id)? {
            match sink::parse_src(&ev.args) {
                Some(src) => {
                    skip.insert(src, id);
                }
                // explicit writes and records from the proxy, hooks and OpenTelemetry come from no log
                None if ev.kind == kind::EXPLICIT || sink::is_external(&ev.args) => {}
                None => return Err(format!("record {id} has no source position").into()),
            }
            id += 1;
        }
        let stats = Stats { recovered: id - state.next_id, ..Stats::default() };
        state.next_id = id;
        Ok(Self {
            dir: dir.to_path_buf(),
            db,
            state,
            stats,
            red: redact::Redactor::new(),
            skip,
            opts,
            last_save: Instant::now(),
            dirty: false,
            policy: None,
            direct: HashMap::new(),
            _lock: lock,
        })
    }

    /// The redactor applied to everything stored (explicit writes too).
    pub fn redactor(&self) -> &redact::Redactor {
        &self.red
    }

    /// Install the recording policy (settings) applied to parsed records.
    pub fn set_policy(&mut self, policy: Box<dyn Policy>) {
        self.policy = Some(policy);
    }

    /// The store number of `writer`, registering it (durably, before any
    /// record cites it) when new.
    pub fn writer_store(&mut self, writer: &str) -> Result<u32> {
        let (agent, new) = self.state.writer_agent(writer);
        if new {
            self.save()?;
        }
        Ok(agent)
    }

    /// The id the next stored record gets.
    pub fn next_id(&self) -> u64 {
        self.state.next_id
    }

    /// Append explicit records (from no log) as one durable batch into one
    /// writer's store. Their ids must be `next_id()..` in order and their
    /// store registered with [`Self::writer_store`]. On error nothing moves.
    pub fn append_explicit(&mut self, events: &[EventIn]) -> Result<()> {
        let Some(first) = events.first() else { return Ok(()) };
        if !self.state.writers.values().any(|a| *a == first.agent) {
            return Err("explicit records for an unregistered writer store".into());
        }
        for (k, e) in events.iter().enumerate() {
            if e.id != self.state.next_id + k as u64 || e.kind != kind::EXPLICIT {
                return Err("explicit records must take the next ids and kind explicit".into());
            }
        }
        self.db.append(events)?;
        self.state.next_id += events.len() as u64;
        self.dirty = true;
        Ok(())
    }

    /// Append records that come from no log file (the model API proxy) as
    /// one durable batch of one writer: each text is redacted, the recording
    /// policy decides per record (`source` stands in for the transcript
    /// path), a withheld tool call takes its result with it, results link to
    /// their calls, and stored records take the next ids. Metadata gets
    /// `"via"` and the recorded time `"rt"`. On error nothing moves.
    pub fn append_direct(&mut self, via: &str, source: &str, recs: Vec<DirectRec>) -> Result<DirectReport> {
        let Some(writer) = recs.first().map(|r| r.writer.clone()) else { return Ok(DirectReport::default()) };
        if recs.iter().any(|r| r.writer != writer) {
            return Err("direct records: one writer per batch".into());
        }
        if let Some(r) = recs.iter().find(|r| r.kind > kind::TOOL_RESULT) {
            return Err(format!("direct records: kind {} is not a capture kind", r.kind).into());
        }
        let mut ctx = self.direct.get(&writer).cloned().unwrap_or_default();
        let mut rc = redact::Counts::new();
        let mut next = self.state.next_id;
        // (record, id, parent, indexed) of the records to store
        let mut keep: Vec<(Rec, DirectRec, u64, u64, bool)> = Vec::new();
        let mut withheld = 0;
        for mut d in recs {
            if d.text.is_empty() {
                continue;
            }
            let text = self.red.redact(&d.text, &mut rc);
            let meta = ["sub", "from"]
                .iter()
                .filter_map(|k| d.meta.get(*k).and_then(|v| v.as_str()).map(|v| (*k, v.to_string())))
                .collect();
            let mut rec = Rec {
                writer: writer.clone(),
                ts_us: d.ts_us,
                kind: d.kind,
                tool: std::mem::take(&mut d.tool),
                cwd: std::mem::take(&mut d.cwd),
                text,
                meta,
                status: d.status,
                session: d.session,
                parent_session: d.parent_session,
                seq: d.seq,
                turn: d.turn,
                call_id: std::mem::take(&mut d.call_id),
                ..Rec::default()
            };
            d.text = String::new();
            // Oojuro's own echo (marker.rs), marked as on every capture path: stored, never indexed
            if marker::has_context(&rec.text) {
                marker::mark(&mut rec.meta, marker::CONTEXT);
            }
            if rec.kind == kind::TOOL_CALL && marker::is_oojuro_call(&rec.tool, "", &rec.text) {
                marker::mark(&mut rec.meta, marker::CALL);
                ctx.open_lookup(&rec.call_id);
            }
            if rec.kind == kind::TOOL_RESULT && ctx.close_lookup(&rec.call_id) {
                marker::mark(&mut rec.meta, marker::RESULT);
            }
            let mut decision = match self.policy.as_mut() {
                Some(p) => p.decide(&rec, source),
                None => Decision::default_for(&rec),
            };
            let mut parent = 0;
            if rec.kind == kind::TOOL_RESULT
                && let Some((tool, call)) = ctx.close_call(&rec.call_id)
            {
                if rec.tool.is_empty() {
                    rec.tool = tool;
                }
                if call == 0 {
                    if !matches!(decision, Decision::Withhold(_)) {
                        decision = Decision::Withhold("call_withheld");
                    }
                } else {
                    parent = call;
                }
            }
            let id = match decision {
                Decision::Store { .. } => {
                    next += 1;
                    next - 1
                }
                Decision::Withhold(why) => {
                    self.stats.withhold(why, rec.text.len());
                    withheld += 1;
                    0 // id 0 marks a withheld call (its result is withheld too)
                }
            };
            if rec.kind == kind::TOOL_CALL {
                ctx.open_call(&rec.call_id, &rec.tool, id);
            }
            if let Decision::Store { indexed } = decision {
                keep.push((rec, d, id, parent, indexed));
            }
        }
        let report = DirectReport { stored: keep.len() as u64, withheld, first_id: keep.first().map_or(0, |k| k.2) };
        if !keep.is_empty() {
            let agent = self.writer_store(&writer)?;
            let rt = now_us();
            let args: Vec<String> = keep
                .iter()
                .map(|(r, d, ..)| {
                    let mut m: serde_json::Map<String, serde_json::Value> =
                        d.meta.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    for (k, v) in &r.meta {
                        m.entry(k.to_string()).or_insert_with(|| v.clone().into()); // sub, from, oojuro
                    }
                    m.insert("via".into(), via.into());
                    m.insert("rt".into(), rt.into());
                    serde_json::Value::Object(m).to_string()
                })
                .collect();
            let evs: Vec<EventIn> = keep
                .iter()
                .zip(&args)
                .map(|((r, d, id, parent, indexed), a)| {
                    let mut e = to_event(r, *id, agent, *parent, a, *indexed);
                    e.tokens_in = d.tokens_in;
                    e.tokens_out = d.tokens_out;
                    e.duration_us = d.duration_us;
                    e
                })
                .collect();
            self.db.append(&evs)?;
            self.state.next_id = next;
            self.stats.commits += 1;
            for (r, ..) in &keep {
                self.stats.record(r);
            }
            self.dirty = true;
        }
        for (k, v) in rc {
            *self.stats.redactions.entry(k.to_string()).or_default() += v;
        }
        if self.direct.len() >= DIRECT_WRITERS && !self.direct.contains_key(&writer) {
            self.direct.clear();
        }
        self.direct.insert(writer, ctx);
        if self.dirty && self.last_save.elapsed() >= self.opts.save_every {
            self.save()?;
        }
        Ok(report)
    }

    /// Append records that come from no log file (hooks, the OpenTelemetry
    /// receiver) as one durable batch into one writer's store: any kind but
    /// `explicit`, ids `next_id()..` in order, the store registered with
    /// [`Self::writer_store`], and `args` naming where they came from
    /// (`"via"`, see [`sink::is_external`]) so a restart does not look for a
    /// source position. On error nothing moves.
    pub fn append_external(&mut self, events: &[EventIn]) -> Result<()> {
        let Some(first) = events.first() else { return Ok(()) };
        if !self.state.writers.values().any(|a| *a == first.agent) {
            return Err("external records for an unregistered writer store".into());
        }
        for (k, e) in events.iter().enumerate() {
            if e.id != self.state.next_id + k as u64 || e.kind == kind::EXPLICIT || !sink::is_external(e.args) {
                return Err("external records must take the next ids, a capture kind and a `via`".into());
            }
        }
        self.db.append(events)?;
        self.state.next_id += events.len() as u64;
        self.dirty = true;
        Ok(())
    }

    /// The recording policy's decision for a record that comes from no log
    /// (`source` names where it came from, e.g. the transcript a hook
    /// reported). Without a policy: [`Decision::default_for`].
    pub fn decide(&mut self, rec: &Rec, source: &str) -> Decision {
        match self.policy.as_mut() {
            Some(p) => p.decide(rec, source),
            None => Decision::default_for(rec),
        }
    }

    /// Re-read the recording policy's settings (see [`Policy::refresh`]).
    pub fn refresh_policy(&mut self) {
        if let Some(p) = self.policy.as_mut() {
            p.refresh();
        }
    }

    /// Save the state when records were committed and `save_every` has passed
    /// since the last save. Records are found again after a crash by probing
    /// the ids past the saved `next_id` (see [`Capture::open`]), so a state a
    /// little behind the data costs nothing but a longer probe, while every
    /// save is two flushes (the file and its directory).
    pub fn save_if_due(&mut self) -> Result<()> {
        if self.dirty && self.last_save.elapsed() >= self.opts.save_every {
            self.save()?;
        }
        Ok(())
    }

    /// Save the state when records were committed since the last save.
    pub fn save_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            self.save()?;
        }
        Ok(())
    }

    pub fn save(&mut self) -> Result<()> {
        self.state.save(&self.dir)?;
        self.dirty = false;
        self.last_save = Instant::now();
        Ok(())
    }

    /// Every `*.jsonl` under `dirs` (recursive, sorted, symlinks not followed).
    pub fn scan(dirs: &[PathBuf]) -> Vec<PathBuf> {
        fn walk(p: &Path, out: &mut Vec<PathBuf>) {
            let Ok(rd) = fs::read_dir(p) else { return };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let path = e.path();
                if ft.is_dir() {
                    walk(&path, out);
                } else if ft.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        for d in dirs {
            if d.is_file() {
                out.push(d.clone());
            } else {
                walk(d, &mut out);
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn register(&mut self, key: String, path: &Path, dev: u64, ino: u64) {
        let format = Format::of_path(path);
        let ctx = match format {
            Format::ClaudeCode => claude::new_ctx(path),
            Format::Codex => codex::new_ctx(path),
        };
        let no = self.state.next_file_no;
        self.state.next_file_no += 1;
        self.state.files.insert(key, FileState { no, format, offset: 0, dev, ino, ctx });
    }

    /// One pass over every file; returns the raw bytes consumed.
    pub fn poll(&mut self, dirs: &[PathBuf]) -> Result<u64> {
        self.poll_with(dirs, &mut |_| Ok(()))
    }

    /// [`Self::poll`], calling `between` after each file (the owner serves
    /// queued explicit writes there, so a long import does not hold them).
    pub fn poll_with(&mut self, dirs: &[PathBuf], between: &mut dyn FnMut(&mut Self) -> Result<()>) -> Result<u64> {
        if let Some(p) = self.policy.as_mut() {
            p.refresh();
        }
        let paths = Self::scan(dirs);
        let mut registered = false;
        for p in &paths {
            let Ok(md) = fs::metadata(p) else { continue };
            let key = p.to_string_lossy().into_owned();
            match self.state.files.get(&key) {
                Some(f) if f.dev == md.dev() && f.ino == md.ino() && md.len() >= f.offset => {}
                Some(_) => {
                    self.stats.files_rotated += 1;
                    self.register(key, p, md.dev(), md.ino());
                    registered = true;
                }
                None => {
                    self.stats.files_new += 1;
                    self.register(key, p, md.dev(), md.ino());
                    registered = true;
                }
            }
        }
        self.stats.files = self.state.files.len() as u64;
        if registered {
            self.save()?; // file numbers are durable before any record cites them
        }
        let mut total = 0;
        for p in &paths {
            total += self.pump(p)?;
            between(self)?;
        }
        if self.dirty && self.last_save.elapsed() >= self.opts.save_every {
            self.save()?;
        }
        Ok(total)
    }

    /// Read and commit every complete line of one file past its offset.
    fn pump(&mut self, path: &Path) -> Result<u64> {
        let key = path.to_string_lossy().into_owned();
        let Some(mut fst) = self.state.files.get(&key).cloned() else { return Ok(0) };
        if fst.format == Format::ClaudeCode && fst.ctx.results_root.is_none() {
            fst.ctx.results_root = Some(claude::project_dir(path)); // not persisted with the state
        }
        let Ok(mut f) = File::open(path) else { return Ok(0) }; // read-only
        let len = f.metadata()?.len();
        if len <= fst.offset {
            return Ok(0);
        }
        f.seek(SeekFrom::Start(fst.offset))?;
        let mut pc = ParseCounts::default();
        let mut rc = redact::Counts::new();
        let mut buf: Vec<u8> = Vec::with_capacity(self.opts.read_chunk);
        let mut buf_off = fst.offset; // file offset of buf[0]
        let mut pending: Vec<Rec> = Vec::new();
        let mut pending_raw = 0usize;
        let mut consumed = 0u64;
        let mut tmp = vec![0u8; self.opts.read_chunk];
        loop {
            let n = f.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let mut start = 0usize;
            while let Some(nl) = buf[start..].iter().position(|&b| b == b'\n') {
                let line = &buf[start..start + nl];
                let line_off = buf_off + start as u64;
                let mut out = Vec::new();
                if !line.iter().all(u8::is_ascii_whitespace) {
                    match fst.format {
                        Format::ClaudeCode => {
                            claude::parse_line(&mut fst.ctx, line, line_off, &self.red, &mut rc, &mut pc, &mut out)
                        }
                        Format::Codex => {
                            codex::parse_line(&mut fst.ctx, line, line_off, &self.red, &mut rc, &mut pc, &mut out)
                        }
                    }
                }
                pending.extend(out);
                self.stats.lines += 1;
                pending_raw += nl + 1;
                consumed += nl as u64 + 1;
                start += nl + 1;
                fst.offset = buf_off + start as u64;
                if pending_raw >= self.opts.batch_bytes {
                    self.commit(&key, &mut fst, std::mem::take(&mut pending))?;
                    pending_raw = 0;
                }
            }
            buf.drain(..start);
            buf_off += start as u64;
        }
        if pending_raw > 0 {
            self.commit(&key, &mut fst, pending)?;
        }
        self.stats.raw_bytes += consumed;
        self.stats.merge_parse(pc);
        for (k, v) in rc {
            *self.stats.redactions.entry(k.to_string()).or_default() += v;
        }
        Ok(consumed)
    }

    /// Commit one file chunk: ids, call links, one append per writer, then
    /// the file's new offset and context.
    fn commit(&mut self, key: &str, fst: &mut FileState, mut recs: Vec<Rec>) -> Result<()> {
        // a Codex subagent names its parent thread; its session is the hash
        // of that thread's rollout path, found among the files tailed
        if fst.ctx.parent_session == 0 && !fst.ctx.parent_thread.is_empty() {
            let thread = fst.ctx.parent_thread.as_str();
            if let Some(p) = self.state.files.keys().find(|k| codex::is_rollout_of(k, thread)) {
                fst.ctx.parent_session = crate::hash64(p);
            }
        }
        if fst.ctx.parent_session != 0 {
            for r in recs.iter_mut().filter(|r| r.parent_session == 0) {
                r.parent_session = fst.ctx.parent_session;
            }
        }
        // the recording policy, in parse order (a result follows its call:
        // the result of a withheld call is withheld too); records already
        // stored before a restart are kept as they are
        let mut decisions: Vec<Option<Decision>> = vec![None; recs.len()];
        {
            let mut call_withheld: HashMap<&str, bool> = HashMap::new();
            for (i, r) in recs.iter().enumerate() {
                if self.skip.contains_key(&(fst.no, r.line, r.k)) {
                    continue;
                }
                let mut d = match self.policy.as_mut() {
                    Some(p) => p.decide(r, key),
                    None => Decision::default_for(r),
                };
                match r.kind {
                    kind::TOOL_CALL => {
                        call_withheld.insert(r.call_id.as_str(), matches!(d, Decision::Withhold(_)));
                    }
                    kind::TOOL_RESULT if !r.call_id.is_empty() => {
                        let earlier = || fst.ctx.calls.iter().rev().find(|c| c.0 == r.call_id).map(|c| c.2 == 0);
                        if call_withheld.get(r.call_id.as_str()).copied().or_else(earlier).unwrap_or(false)
                            && !matches!(d, Decision::Withhold(_))
                        {
                            d = Decision::Withhold("call_withheld");
                        }
                    }
                    _ => {}
                }
                decisions[i] = Some(d);
            }
        }
        // writers -> store numbers (new writers are saved before use); a
        // writer whose records are all withheld is not registered
        let mut new_writer = false;
        let mut agents: Vec<u32> = Vec::with_capacity(recs.len());
        for (i, r) in recs.iter().enumerate() {
            if matches!(decisions[i], Some(Decision::Withhold(_))) {
                agents.push(0);
                continue;
            }
            let (a, new) = self.state.writer_agent(&r.writer);
            new_writer |= new;
            agents.push(a);
        }
        if new_writer {
            self.save()?;
        }
        let mut order: Vec<usize> = (0..recs.len()).collect();
        order.sort_by_key(|&i| agents[i]); // stable: parse order within a writer
        let rt = now_us();
        // (agent, [(rec index, id, parent, args, indexed)])
        let mut groups: Vec<(u32, Vec<Pending>)> = Vec::new();
        let mut next = self.state.next_id;
        for i in order {
            let r = &mut recs[i];
            let prior = self.skip.remove(&(fst.no, r.line, r.k));
            let (id, indexed) = match (prior, decisions[i]) {
                (Some(id), _) => (id, false),
                (None, Some(Decision::Store { indexed })) => {
                    next += 1;
                    (next - 1, indexed)
                }
                (None, Some(Decision::Withhold(why))) => {
                    self.stats.withhold(why, r.text.len());
                    (0, false) // id 0 marks a withheld call (its result is withheld too)
                }
                (None, None) => return Err("internal: a new record without a decision".into()),
            };
            let mut parent = 0;
            match r.kind {
                kind::TOOL_CALL => fst.ctx.open_call(&r.call_id, &r.tool, id),
                kind::TOOL_RESULT => {
                    if let Some((tool, call)) = fst.ctx.close_call(&r.call_id) {
                        parent = call;
                        if r.tool.is_empty() {
                            r.tool = tool;
                        }
                    }
                }
                _ => {}
            }
            if id == 0 {
                continue;
            }
            if prior.is_some() {
                self.stats.skipped_existing += 1;
                continue;
            }
            let args = args_json(r, fst.no, rt);
            if groups.last().is_none_or(|g| g.0 != agents[i]) {
                groups.push((agents[i], Vec::new()));
            }
            groups.last_mut().unwrap().1.push((i, id, parent, args, indexed));
        }
        for (agent, g) in &groups {
            let evs: Vec<_> = g
                .iter()
                .map(|(i, id, parent, args, indexed)| to_event(&recs[*i], *id, *agent, *parent, args, *indexed))
                .collect();
            self.db.append(&evs)?;
        }
        for (agent, g) in &groups {
            for (i, _, _, _, _) in g {
                self.stats.record(&recs[*i]);
                if self.opts.dedupe_stats {
                    self.stats.dedupe.observe(*agent, &recs[*i]);
                }
            }
        }
        self.state.next_id = next;
        self.state.files.insert(key.to_string(), fst.clone());
        self.stats.commits += 1;
        self.dirty = true;
        // keep the recovery window (records past the saved state) short
        if self.last_save.elapsed() >= self.opts.save_every {
            self.save()?;
        }
        Ok(())
    }

    /// Bytes of incomplete last lines (left for the next pass).
    pub fn pending_tail_bytes(&self) -> u64 {
        self.state
            .files
            .iter()
            .filter_map(|(p, f)| fs::metadata(p).ok().map(|m| m.len().saturating_sub(f.offset)))
            .sum()
    }

    /// End of an import: flush the stores, checkpoint, save the state, and
    /// record the store size.
    pub fn finish(&mut self) -> Result<()> {
        self.db.flush()?;
        self.save()?;
        self.measure_store();
        Ok(())
    }

    pub fn measure_store(&mut self) {
        self.stats.store_bytes = self.db.disk_bytes();
        self.stats.store_breakdown = self.db.disk_breakdown().iter().map(|(k, v)| (k.to_string(), *v)).collect();
    }

    /// Follow mode: poll until `stop` says so. Flushes the stores after
    /// `idle_flush` without new data.
    pub fn tail(
        &mut self,
        dirs: &[PathBuf],
        every: Duration,
        idle_flush: Duration,
        mut stop: impl FnMut(&Self, u64) -> bool,
    ) -> Result<()> {
        let mut idle_since = Instant::now();
        let mut flushed = true;
        loop {
            let n = self.poll(dirs)?;
            if n > 0 {
                idle_since = Instant::now();
                flushed = false;
            } else if !flushed && idle_since.elapsed() >= idle_flush {
                self.db.flush()?;
                flushed = true;
            }
            if self.dirty {
                self.save()?;
            }
            if stop(self, n) {
                return Ok(());
            }
            std::thread::sleep(every);
        }
    }
}

/// Take the store's exclusive lock (`<store>/LOCK`): one owning process
/// (the recorder, or a front-door call when no recorder runs) at a time.
pub fn lock_store(dir: &Path) -> Result<File> {
    let f = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("LOCK"))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(fs::TryLockError::WouldBlock) => Err(format!("store {} is open in another process", dir.display()).into()),
        Err(fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Redaction audit: parse every line with an auditing redactor (nothing is
/// stored) and return the redactor with its sampled hits.
pub fn audit_redactions(
    dirs: &[PathBuf],
    per_kind: usize,
    seed: u64,
) -> Result<(redact::Redactor, redact::Counts, u64, ParseCounts)> {
    let red = redact::Redactor::with_audit(per_kind, seed);
    let mut rc = redact::Counts::new();
    let mut pc = ParseCounts::default();
    let mut raw = 0u64;
    for path in Capture::scan(dirs) {
        let format = Format::of_path(&path);
        let mut ctx = match format {
            Format::ClaudeCode => claude::new_ctx(&path),
            Format::Codex => codex::new_ctx(&path),
        };
        let Ok(data) = fs::read(&path) else { continue };
        let mut off = 0u64;
        let mut out = Vec::new();
        for line in data.split_inclusive(|&b| b == b'\n') {
            if line.last() != Some(&b'\n') {
                break; // partial last line
            }
            let l = &line[..line.len() - 1];
            match format {
                Format::ClaudeCode => claude::parse_line(&mut ctx, l, off, &red, &mut rc, &mut pc, &mut out),
                Format::Codex => codex::parse_line(&mut ctx, l, off, &red, &mut rc, &mut pc, &mut out),
            }
            out.clear();
            off += line.len() as u64;
        }
        raw += off;
    }
    Ok((red, rc, raw, pc))
}
