//! Opt-in, Codex-only native raw/parsed capture. No recorder, CLI, cloud or discovery selects it.
//! Raw consent precedes file inspection. Derived plaintext exists only on verified Linux tmpfs;
//! tmpfs can swap, so this is not a production confidentiality or key-lifecycle claim.
use crate::{
    FileCtx, ParseCounts, Rec, codex,
    ingest::{Decision, Policy},
    kind, redact, sink,
};
use oojuro_engine::{
    capture_log::{
        self as log, Batch, CaptureLog, Event, Identity, Line, LineKind, MAX_BLOCK_BYTES,
        MAX_LINE_BYTES, Outcome, ParsedBatch, PolicyReceipt, Source,
    },
    file_capture::open_authorized_file,
};
use oojuro_trace::{EventIn, EventOut, TraceDb};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const ADAPTER: &str = "codex-buffer-1/redact-escaped-1";
#[derive(Clone)]
pub struct Options {
    pub writer: String,
    pub agent: u32,
    pub first_id: u64,
    pub chunk_bytes: usize,
    pub policy_id: String,
    pub raw_authorization: String,
    pub recorded_at: fn() -> i64,
}
impl Options {
    pub fn new(writer: &str, agent: u32, policy_id: &str) -> Self {
        Self {
            writer: writer.into(),
            agent,
            first_id: 1,
            chunk_bytes: MAX_BLOCK_BYTES,
            policy_id: policy_id.into(),
            raw_authorization: "explicit-source-consent-1".into(),
            recorded_at: crate::time::now_us,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Storage,
    Corrupt,
    Unavailable,
    /// Authority is committed; close/reopen or rebuild before reading. Do not allocate new IDs.
    CommittedUnavailable(Outcome),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid local capture: {s}"),
            Self::Storage => f.write_str("local capture storage unavailable"),
            Self::Corrupt => f.write_str("local capture authority invalid"),
            Self::Unavailable => f.write_str("local capture projection unavailable"),
            Self::CommittedUnavailable(_) => {
                f.write_str("capture committed; projection unavailable")
            }
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
fn storage(_: impl std::fmt::Debug) -> Error {
    Error::Storage
}
fn authority(e: oojuro_store::Error) -> Error {
    e.into()
}
// Keep Store errors private to this boundary; no sensitive parser/provider text is displayed.
impl From<oojuro_store::Error> for Error {
    fn from(e: oojuro_store::Error) -> Self {
        match e {
            oojuro_store::Error::Corrupt(_)
            | oojuro_store::Error::Crypto(_)
            | oojuro_store::Error::WrongKey => Self::Corrupt,
            oojuro_store::Error::Invalid(_) => Self::Invalid("authority input or bound"),
            _ => Self::Storage,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub source: Source,
    pub line_start: u64,
    pub line_end: u64,
    pub emission: u32,
}
pub struct Hit {
    pub hit: oojuro_trace::SearchHit,
    pub provenance: Provenance,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub raw_end: u64,
    pub parsed_end: u64,
    pub event_ids: Vec<u64>,
}
struct Projection {
    db: Option<TraceDb>,
    path: PathBuf,
}
impl Drop for Projection {
    fn drop(&mut self) {
        drop(self.db.take());
        let _ = fs::remove_dir_all(&self.path);
    }
}
pub struct LocalCapture {
    log: CaptureLog,
    options: Options,
    policy: Box<dyn Policy>,
    redactor: redact::Redactor,
    ram_root: PathBuf,
    projection: Option<Projection>,
    applied: usize,
    #[cfg(test)]
    fail_projection: bool,
}
impl LocalCapture {
    /// Explicit fresh format. Does not discover sources, store keys, or open a legacy journal.
    pub fn open(
        dir: &Path,
        key: &[u8; 32],
        ram_root: &Path,
        options: Options,
        policy: Box<dyn Policy>,
    ) -> Result<Self> {
        if options.chunk_bytes == 0
            || options.chunk_bytes > MAX_BLOCK_BYTES
            || options.policy_id.is_empty()
            || options.policy_id.len() > 128
            || options.raw_authorization.is_empty()
            || options.raw_authorization.len() > 128
        {
            return Err(Error::Invalid("capture options"));
        }
        verify_tmpfs(ram_root)?;
        let identity = Identity {
            writer: options.writer.clone(),
            agent: options.agent,
            first_id: options.first_id,
            adapter: ADAPTER.into(),
        };
        let log = CaptureLog::open(dir, key, identity).map_err(authority)?;
        let mut this = Self {
            log,
            options,
            policy,
            redactor: redact::Redactor::new(),
            ram_root: fs::canonicalize(ram_root).map_err(storage)?,
            projection: None,
            applied: 0,
            #[cfg(test)]
            fail_projection: false,
        };
        this.rebuild()?;
        Ok(this)
    }
    /// Reads only authorized parsed receipts. Current policy cannot reinterpret retained raw.
    pub fn rebuild(&mut self) -> Result<()> {
        self.projection = None;
        self.applied = 0;
        for source in self.log.sources() {
            decode_context(&self.log.progress(source).map_err(authority)?.context)?;
        }
        verify_tmpfs(&self.ram_root)?;
        let path = private_directory(&self.ram_root)?;
        let mut projection = Projection { db: None, path };
        let db = TraceDb::open(&projection.path, crate::ingest::capture_trace_options())
            .map_err(storage)?;
        projection.db = Some(db);
        for batch in self.log.batches().map_err(authority)? {
            project(projection.db.as_ref().unwrap(), batch)?;
        }
        self.applied = self.log.batches().map_err(authority)?.len();
        self.projection = Some(projection);
        Ok(())
    }
    fn ready(&self) -> Result<&TraceDb> {
        if self.applied != self.log.batches().map_err(authority)?.len() {
            return Err(Error::Unavailable);
        }
        self.projection
            .as_ref()
            .and_then(|p| p.db.as_ref())
            .ok_or(Error::Unavailable)
    }
    pub fn get(&self, id: u64) -> Result<Option<(EventOut, Provenance)>> {
        let db = self.ready()?;
        let Some((source, event)) = self.log.event(id).map_err(authority)? else {
            return Ok(None);
        };
        let read = db.get(id).map_err(storage)?.ok_or(Error::Corrupt)?;
        Ok(Some((read, provenance(source, event))))
    }
    pub fn search(&self, query: &str, from: i64, to: i64, k: usize) -> Result<Vec<Hit>> {
        if k == 0 || k > 1000 {
            return Err(Error::Invalid("search count"));
        }
        let hits = self
            .ready()?
            .search_where(self.options.agent, query, from, to, k, &mut |_| true)
            .map_err(storage)?;
        hits.into_iter()
            .map(|hit| {
                let (source, event) = self
                    .log
                    .event(hit.id)
                    .map_err(authority)?
                    .ok_or(Error::Corrupt)?;
                Ok(Hit {
                    provenance: provenance(source, event),
                    hit,
                })
            })
            .collect()
    }
    /// One explicitly consented source generation; each block is atomic, the whole file is not.
    /// Files must be append-only during this pass, as with engine::file_capture.
    pub fn capture_file(
        &mut self,
        source: Source,
        path: &Path,
        retain_raw: impl FnMut(&Path) -> bool,
    ) -> Result<Option<Report>> {
        self.ready()?;
        let Some(mut opened) = open_authorized_file(path, retain_raw).map_err(authority)? else {
            return Ok(None);
        };
        self.policy.refresh();
        let initial = serde_json::to_vec(&codex::new_ctx(path)).map_err(storage)?;
        self.log
            .register(source, &opened.descriptor, &initial)
            .map_err(authority)?;
        let progress = self.log.progress(source).map_err(authority)?;
        if opened.observed_length < progress.raw_end {
            return Err(Error::Invalid("source truncated; new generation required"));
        }
        // Revalidate every retained byte before any new byte can be committed.
        let mut offset = 0;
        let mut buffer = vec![0; self.options.chunk_bytes];
        while offset < progress.raw_end {
            let n = (progress.raw_end - offset).min(buffer.len() as u64) as usize;
            opened.file.read_exact(&mut buffer[..n]).map_err(storage)?;
            if self.log.read_raw(source, offset, n).map_err(authority)? != buffer[..n] {
                return Err(Error::Invalid(
                    "source prefix changed; new generation required",
                ));
            }
            offset += n as u64;
        }
        while offset < opened.observed_length {
            let n = (opened.observed_length - offset).min(buffer.len() as u64) as usize;
            opened.file.read_exact(&mut buffer[..n]).map_err(storage)?;
            let batch = self.plan(source, path, &buffer[..n])?;
            let outcome = self.log.commit(batch).map_err(authority)?;
            #[cfg(test)]
            if std::env::var("OOJURO_LOCAL_TEST_CRASH").as_deref() == Ok("after-authority") {
                unsafe {
                    libc::raise(libc::SIGKILL);
                }
                unreachable!();
            }
            #[cfg(test)]
            if self.fail_projection {
                self.projection = None;
                return Err(Error::CommittedUnavailable(outcome));
            }
            let projected = self
                .projection
                .as_ref()
                .and_then(|p| p.db.as_ref())
                .ok_or(Error::Unavailable)
                .and_then(|db| {
                    project(
                        db,
                        self.log
                            .batches()
                            .map_err(authority)?
                            .last()
                            .ok_or(Error::Corrupt)?,
                    )
                });
            if projected.is_err() {
                self.projection = None;
                return Err(Error::CommittedUnavailable(outcome));
            }
            self.applied = self.log.batches().map_err(authority)?.len();
            #[cfg(test)]
            if std::env::var("OOJURO_LOCAL_TEST_CRASH").as_deref() == Ok("after-projection") {
                unsafe {
                    libc::raise(libc::SIGKILL);
                }
                unreachable!();
            }
            offset += n as u64;
        }
        if opened.file.metadata().map_err(storage)?.len() < opened.observed_length {
            return Err(Error::Invalid(
                "source shrank during capture; new generation required",
            ));
        }
        self.report(source).map(Some)
    }
    pub fn report(&self, source: Source) -> Result<Report> {
        self.ready()?;
        let p = self.log.progress(source).map_err(authority)?;
        let event_ids = self
            .log
            .batches()
            .map_err(authority)?
            .iter()
            .filter(|b| b.source == source)
            .flat_map(|b| b.events.iter().map(|e| e.id))
            .collect();
        Ok(Report {
            raw_end: p.raw_end,
            parsed_end: p.parsed_end,
            event_ids,
        })
    }
    /// Explicit raw export, requiring a separate caller decision before reading retained bytes.
    pub fn restore_raw(
        &self,
        source: Source,
        mut out: impl std::io::Write,
        retain_raw: bool,
    ) -> Result<()> {
        if !retain_raw {
            return Err(Error::Invalid("raw export withheld"));
        }
        let end = self.log.progress(source).map_err(authority)?.raw_end;
        let mut at = 0;
        while at < end {
            let n = (end - at).min(MAX_BLOCK_BYTES as u64) as usize;
            out.write_all(&self.log.read_raw(source, at, n).map_err(authority)?)
                .map_err(storage)?;
            at += n as u64;
        }
        Ok(())
    }
    fn plan(&mut self, source: Source, path: &Path, raw: &[u8]) -> Result<Batch> {
        let old = self.log.progress(source).map_err(authority)?;
        let mut context = decode_context(&old.context)?;
        let mut input = self
            .log
            .read_raw(
                source,
                old.parsed_end,
                (old.raw_end - old.parsed_end) as usize,
            )
            .map_err(authority)?;
        input.extend_from_slice(raw);
        let mut lines = Vec::new();
        let mut events = Vec::new();
        let mut start = 0;
        let mut next_id = self.log.next_id();
        let mut pc = ParseCounts::default();
        let mut rc = redact::Counts::new();
        let recorded = (self.options.recorded_at)();
        while let Some(relative) = input[start..].iter().position(|b| *b == b'\n') {
            let end = start + relative + 1;
            if end - start > MAX_LINE_BYTES {
                return Err(Error::Invalid("source line bound"));
            }
            let line = &input[start..end - 1];
            let from = old.parsed_end + start as u64;
            let through = old.parsed_end + end as u64;
            let mut recs = Vec::new();
            let bad = pc.bad_lines;
            let blank = line.iter().all(u8::is_ascii_whitespace);
            if !blank {
                codex::parse_line(
                    &mut context,
                    line,
                    from,
                    &self.redactor,
                    &mut rc,
                    &mut pc,
                    &mut recs,
                );
            }
            let mut emissions = Vec::new();
            for mut rec in recs {
                if rec.writer != self.options.writer {
                    return Err(Error::Invalid("source writer differs from journal binding"));
                }
                let mut decision = self.policy.decide(&rec, &path.to_string_lossy());
                if rec.kind == kind::TOOL_RESULT
                    && !rec.call_id.is_empty()
                    && context
                        .calls
                        .iter()
                        .rev()
                        .find(|c| c.0 == rec.call_id)
                        .is_some_and(|c| c.2 == 0)
                {
                    decision = Decision::Withhold("call_withheld");
                }
                let id = match decision {
                    Decision::Store { .. } => {
                        let id = next_id;
                        next_id = next_id
                            .checked_add(1)
                            .ok_or(Error::Invalid("event ID exhaustion"))?;
                        id
                    }
                    Decision::Withhold(_) => 0,
                };
                let parent = links(&mut context, &mut rec, id);
                match decision {
                    Decision::Store { indexed } => {
                        let args = sink::args_json(&rec, old.file_no, recorded);
                        let mut event =
                            owned(&rec, id, self.options.agent, parent, args, indexed, through);
                        // Tool/metadata strings cross the same parsed-output boundary as text.
                        // Resume context retains original linking state only inside the sealed authority.
                        event.tool = self.redactor.redact(&event.tool, &mut rc);
                        event.args = self.redactor.redact(&event.args, &mut rc);
                        events.push(event);
                        emissions.push(log::Decision::Stored { id, indexed });
                    }
                    Decision::Withhold(reason) => emissions.push(log::Decision::Withheld {
                        reason: reason.into(),
                    }),
                }
            }
            lines.push(Line {
                start: from,
                end: through,
                kind: if blank {
                    LineKind::Blank
                } else if pc.bad_lines > bad {
                    LineKind::Malformed
                } else {
                    LineKind::Parsed
                },
                emissions,
            });
            start = end;
        }
        if input.len() - start > MAX_LINE_BYTES {
            return Err(Error::Invalid("unterminated line bound"));
        }
        let outcome = Outcome {
            raw_end: old.raw_end + raw.len() as u64,
            parsed_end: old.parsed_end + start as u64,
            first_id: events.first().map(|e| e.id),
            stored: events.len() as u64,
            next_id,
        };
        let context = serde_json::to_vec(&context).map_err(storage)?;
        Ok(Batch {
            raw: raw.to_vec(),
            parsed: ParsedBatch {
                source,
                raw_start: old.raw_end,
                parsed_before: old.parsed_end,
                outcome,
                policy: PolicyReceipt {
                    policy_id: self.options.policy_id.clone(),
                    raw_authorization: self.options.raw_authorization.clone(),
                    lines,
                },
                context,
                events,
            },
        })
    }
}
fn decode_context(bytes: &[u8]) -> Result<FileCtx> {
    let context: FileCtx = serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)?;
    if context.calls.len() > FileCtx::MAX_OPEN_CALLS
        || context.lookups.len() > FileCtx::MAX_OPEN_CALLS
    {
        return Err(Error::Corrupt);
    }
    Ok(context)
}
fn links(context: &mut FileCtx, rec: &mut Rec, id: u64) -> u64 {
    match rec.kind {
        kind::TOOL_CALL => {
            context.open_call(&rec.call_id, &rec.tool, id);
            0
        }
        kind::TOOL_RESULT => {
            if let Some((tool, parent)) = context.close_call(&rec.call_id) {
                if rec.tool.is_empty() {
                    rec.tool = tool;
                }
                parent
            } else {
                0
            }
        }
        _ => 0,
    }
}
fn owned(
    rec: &Rec,
    id: u64,
    agent: u32,
    parent: u64,
    args: String,
    indexed: bool,
    line_end: u64,
) -> Event {
    Event {
        id,
        session: rec.session,
        parent_session: rec.parent_session,
        agent,
        turn: rec.turn,
        seq: rec.seq,
        parent,
        revises: 0,
        kind: rec.kind,
        status: rec.status,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: rec.ts_us,
        duration_us: 0,
        tool: rec.tool.clone(),
        args,
        text: rec.text.clone(),
        indexed,
        line_start: rec.line,
        line_end,
        emission: rec.k,
    }
}
fn project(db: &TraceDb, batch: &ParsedBatch) -> Result<()> {
    let events: Vec<_> = batch
        .events
        .iter()
        .map(|e| EventIn {
            id: e.id,
            session: e.session,
            parent_session: e.parent_session,
            agent: e.agent,
            turn: e.turn,
            seq: e.seq,
            parent: e.parent,
            revises: e.revises,
            kind: e.kind,
            status: e.status,
            exit_code: e.exit_code,
            tokens_in: e.tokens_in,
            tokens_out: e.tokens_out,
            ts_us: e.ts_us,
            duration_us: e.duration_us,
            tool: &e.tool,
            args: &e.args,
            text: &e.text,
            indexed: e.indexed,
        })
        .collect();
    db.append(&events).map_err(storage)
}
fn provenance(source: Source, e: &Event) -> Provenance {
    Provenance {
        source,
        line_start: e.line_start,
        line_end: e.line_end,
        emission: e.emission,
    }
}
fn verify_tmpfs(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = fs::canonicalize(path).map_err(storage)?;
    if !path.is_dir() {
        return Err(Error::Invalid("cache root is not a directory"));
    }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(storage)?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: live CString and initialized output checked after successful syscall.
    if unsafe { libc::statfs(c.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(Error::Storage);
    }
    if unsafe { info.assume_init() }.f_type != libc::TMPFS_MAGIC {
        return Err(Error::Invalid("cache requires explicit Linux tmpfs"));
    }
    Ok(())
}
fn private_directory(root: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..8 {
        let path = root.join(format!(
            "oojuro-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(Error::Storage),
        }
    }
    Err(Error::Storage)
}
#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
