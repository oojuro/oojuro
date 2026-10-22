//! Opt-in native raw/parsed capture with explicitly selected Codex, Claude or Pi adapters.
//! No legacy recorder, socket or cloud path selects it automatically.
//! Raw consent precedes file inspection. Native derived postings live only in memory;
//! this is not a production swap, confidentiality or key-lifecycle claim.
use crate::{
    FileCtx, ParseCounts, Rec, claude, codex,
    ingest::{Decision, Policy},
    kind, redact, sink,
};
use oojuro_engine::{
    capture_log::{
        self as log, Batch, CaptureLog, Event, Identity, Line, LineKind, MAX_BLOCK_BYTES,
        MAX_LINE_BYTES, Outcome, ParsedBatch, PolicyReceipt, Source,
    },
    file_capture::open_authorized_file,
    lexical::{self, Index, Query},
};
use std::{io::Read, path::Path};

#[path = "local_claude.rs"]
mod claude_adapter;
#[path = "local_pi.rs"]
mod pi_adapter;
#[path = "local_temporal.rs"]
mod temporal_adapter;
pub use temporal_adapter::{Stream, StreamPosition, StreamReceipt};
#[derive(Clone, Copy)]
enum Adapter {
    Codex,
    ClaudeCode,
    Pi,
    Temporal,
}
const ADAPTER: &str = "codex-buffer-1/redact-escaped-1/context-1";
const TEMPORAL_ADAPTER: &str = "temporal-stream-v1/raw-policy-1/context-1";
const PI_ADAPTER: &str = "pi-session-v3/raw-policy-1/redact-escaped-1/context-1";
const CLAUDE_ADAPTER: &str = "claude-buffer-1/no-sidefiles-1/redact-escaped-1/context-1";
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
    pub hit: lexical::Hit,
    pub provenance: Provenance,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub raw_end: u64,
    pub parsed_end: u64,
    pub event_ids: Vec<u64>,
}
pub struct LocalCapture {
    log: CaptureLog,
    options: Options,
    format: Adapter,
    policy: Box<dyn Policy>,
    redactor: redact::Redactor,
    projection: Option<Index>,
    applied: usize,
    #[cfg(test)]
    fail_projection: bool,
}
impl LocalCapture {
    /// Explicit fresh format. Does not discover sources, store keys, or open a legacy journal.
    pub fn open(
        dir: &Path,
        key: &[u8; 32],
        options: Options,
        policy: Box<dyn Policy>,
    ) -> Result<Self> {
        Self::open_adapter(dir, key, options, policy, Adapter::Codex)
    }
    /// Explicit Claude transcript lane. Referenced tool-result files are never inspected.
    pub fn open_claude(
        dir: &Path,
        key: &[u8; 32],
        options: Options,
        policy: Box<dyn Policy>,
    ) -> Result<Self> {
        Self::open_adapter(dir, key, options, policy, Adapter::ClaudeCode)
    }
    /// Explicit Pi session v3 source; no discovery or secondary file reads.
    pub fn open_pi(
        dir: &Path,
        key: &[u8; 32],
        options: Options,
        policy: Box<dyn Policy>,
    ) -> Result<Self> {
        Self::open_adapter(dir, key, options, policy, Adapter::Pi)
    }
    /// Explicit Temporal byte stream; file capture is forbidden in this lane.
    pub fn open_temporal(
        dir: &Path,
        key: &[u8; 32],
        options: Options,
        policy: Box<dyn Policy>,
    ) -> Result<Self> {
        Self::open_adapter(dir, key, options, policy, Adapter::Temporal)
    }
    fn open_adapter(
        dir: &Path,
        key: &[u8; 32],
        options: Options,
        policy: Box<dyn Policy>,
        format: Adapter,
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
        let identity = Identity {
            writer: options.writer.clone(),
            agent: options.agent,
            first_id: options.first_id,
            adapter: match format {
                Adapter::Codex => ADAPTER,
                Adapter::ClaudeCode => CLAUDE_ADAPTER,
                Adapter::Pi => PI_ADAPTER,
                Adapter::Temporal => TEMPORAL_ADAPTER,
            }
            .into(),
        };
        let log = CaptureLog::open(dir, key, identity).map_err(authority)?;
        let mut this = Self {
            log,
            options,
            format,
            policy,
            redactor: redact::Redactor::new(),
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
        if matches!(self.format, Adapter::Temporal) {
            temporal_adapter::validate_log(&self.log, &self.options.writer)?;
        }
        for source in self.log.sources() {
            decode_context(self.log.initial_context(source).map_err(authority)?)?;
            decode_context(&self.log.progress(source).map_err(authority)?.context)?;
        }
        for batch in self.log.batches().map_err(authority)? {
            decode_context(&batch.context)?;
        }
        let projection = Index::rebuild(&self.log).map_err(|_| Error::Unavailable)?;
        self.applied = self.log.batches().map_err(authority)?.len();
        self.projection = Some(projection);
        Ok(())
    }
    fn ready(&self) -> Result<&Index> {
        if self.applied != self.log.batches().map_err(authority)?.len() {
            return Err(Error::Unavailable);
        }
        self.projection.as_ref().ok_or(Error::Unavailable)
    }
    /// Read-only committed authority after the local projection is healthy.
    /// This Rust accessor is not an authorization boundary; the caller owns its key.
    pub fn authority(&self) -> Result<&CaptureLog> {
        self.ready()?;
        Ok(&self.log)
    }
    pub fn get(&self, id: u64) -> Result<Option<(Event, Provenance)>> {
        self.ready()?;
        let Some((source, event)) = self.log.event(id).map_err(authority)? else {
            return Ok(None);
        };
        Ok(Some((event.clone(), provenance(source, event))))
    }
    pub fn search(&self, query: &str, from: i64, to: i64, k: usize) -> Result<Vec<Hit>> {
        if k == 0 || k > 1000 {
            return Err(Error::Invalid("search count"));
        }
        let hits = self
            .ready()?
            .search(&Query {
                writer: &self.options.writer,
                agent: self.options.agent,
                text: query,
                from,
                to,
                k,
            })
            .map_err(|_| Error::Unavailable)?;
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
        if matches!(self.format, Adapter::Temporal) {
            return Err(Error::Invalid("temporal stream is not a file adapter"));
        }
        let Some(mut opened) = open_authorized_file(path, retain_raw).map_err(authority)? else {
            return Ok(None);
        };
        self.policy.refresh();
        let initial = encode_context(&match self.format {
            Adapter::Codex => codex::new_ctx(path),
            Adapter::ClaudeCode => claude_adapter::new_ctx(path),
            Adapter::Pi => pi_adapter::new_ctx(path),
            Adapter::Temporal => unreachable!(),
        })?;
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
            self.finish_append(outcome)?;
            offset += n as u64;
        }
        if opened.file.metadata().map_err(storage)?.len() < opened.observed_length {
            return Err(Error::Invalid(
                "source shrank during capture; new generation required",
            ));
        }
        self.report(source).map(Some)
    }
    fn finish_append(&mut self, outcome: Outcome) -> Result<()> {
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
            .as_mut()
            .ok_or(Error::Unavailable)
            .and_then(|index| index.refresh(&self.log).map_err(|_| Error::Unavailable));
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
        Ok(())
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
        self.ready()?;
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
        if old
            .tail_policy
            .as_ref()
            .is_some_and(|id| id != &self.options.policy_id)
        {
            return Err(Error::Invalid(
                "incomplete line requires its original policy identity",
            ));
        }
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
                if matches!(self.format, Adapter::Temporal) {
                    temporal_adapter::parse_line(
                        &mut context,
                        line,
                        from,
                        &self.redactor,
                        &mut rc,
                        &mut pc,
                        &mut recs,
                    )?;
                } else if matches!(self.format, Adapter::Pi) {
                    pi_adapter::parse_line(
                        &mut context,
                        line,
                        from,
                        &self.redactor,
                        &mut rc,
                        &mut pc,
                        &mut recs,
                    )?;
                } else {
                    match self.format {
                        Adapter::Codex => check_call_state(&context, line)?,
                        Adapter::ClaudeCode => claude_adapter::check_call_state(&context, line)?,
                        Adapter::Pi | Adapter::Temporal => unreachable!(),
                    }
                    let parse = match self.format {
                        Adapter::Codex => codex::parse_line,
                        Adapter::ClaudeCode => claude::parse_line,
                        Adapter::Pi | Adapter::Temporal => unreachable!(),
                    };
                    parse(
                        &mut context,
                        line,
                        from,
                        &self.redactor,
                        &mut rc,
                        &mut pc,
                        &mut recs,
                    );
                }
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
                let parent = links(&mut context, &mut rec, id)?;
                match decision {
                    Decision::Store { indexed } => {
                        let args = sink::args_json(&rec, old.file_no, recorded);
                        let mut event =
                            owned(&rec, id, self.options.agent, parent, args, indexed, through);
                        // Tool/metadata strings cross the same parsed-output boundary as text.
                        // Resume context retains original linking state only inside the sealed authority.
                        if matches!(self.format, Adapter::Pi) {
                            event.exit_code = rec
                                .meta
                                .iter()
                                .find(|(name, _)| *name == "exit_code")
                                .and_then(|(_, value)| value.parse().ok());
                        }
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
        let context = encode_context(&context)?;
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
// Local receipts deliberately do not use FileCtx's legacy serde defaults or skipped fields.
// Every member is required, including empty policy/link state, on every sealed receipt.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeContext {
    version: u32,
    writer: String,
    session: u64,
    parent_session: u64,
    parent_thread: String,
    seq: u32,
    turn: u32,
    last_ts: i64,
    cwd: String,
    sidechain: bool,
    user_from: String,
    inherit_below: i64,
    metas: u32,
    calls: Vec<(String, String, u64)>,
    lookups: Vec<String>,
}
fn encode_context(c: &FileCtx) -> Result<Vec<u8>> {
    serde_json::to_vec(&ResumeContext {
        version: 1,
        writer: c.writer.clone(),
        session: c.session,
        parent_session: c.parent_session,
        parent_thread: c.parent_thread.clone(),
        seq: c.seq,
        turn: c.turn,
        last_ts: c.last_ts,
        cwd: c.cwd.clone(),
        sidechain: c.sidechain,
        user_from: c.user_from.clone(),
        inherit_below: c.inherit_below,
        metas: c.metas,
        calls: c.calls.clone(),
        lookups: c.lookups.clone(),
    })
    .map_err(storage)
}
fn decode_context(bytes: &[u8]) -> Result<FileCtx> {
    let c: ResumeContext = serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)?;
    if c.version != 1
        || c.calls.len() > FileCtx::MAX_OPEN_CALLS
        || c.lookups.len() > FileCtx::MAX_OPEN_CALLS
        || c.calls.iter().enumerate().any(|(i, call)| {
            call.0.is_empty() || c.calls[..i].iter().any(|prior| prior.0 == call.0)
        })
        || c.lookups
            .iter()
            .enumerate()
            .any(|(i, id)| id.is_empty() || c.lookups[..i].contains(id))
    {
        return Err(Error::Corrupt);
    }
    Ok(FileCtx {
        writer: c.writer,
        session: c.session,
        parent_session: c.parent_session,
        parent_thread: c.parent_thread,
        seq: c.seq,
        turn: c.turn,
        last_ts: c.last_ts,
        cwd: c.cwd,
        sidechain: c.sidechain,
        user_from: c.user_from,
        inherit_below: c.inherit_below,
        metas: c.metas,
        calls: c.calls,
        lookups: c.lookups,
        results_root: None,
    })
}
// The legacy parser opens lookup markers before emitting a Rec (an empty body may
// emit none). Refuse overflow and ambiguous unresolved IDs before that mutation.
// Results/noise remain consumable, allowing outstanding markers to close normally.
fn check_call_state(context: &FileCtx, line: &[u8]) -> Result<()> {
    if context.lookups.is_empty() && context.calls.is_empty() {
        return Ok(());
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Ok(());
    };
    let p = &v["payload"];
    if v["type"].as_str() == Some("response_item")
        && matches!(
            p["type"].as_str(),
            Some(
                "function_call"
                    | "custom_tool_call"
                    | "local_shell_call"
                    | "web_search_call"
                    | "image_generation_call"
                    | "tool_search_call"
            )
        )
        && p["call_id"].as_str().is_some_and(|id| !id.is_empty())
    {
        let id = p["call_id"].as_str().unwrap();
        if context.calls.iter().any(|call| call.0 == id)
            || context.lookups.iter().any(|lookup| lookup == id)
        {
            return Err(Error::Invalid("duplicate unresolved call ID"));
        }
        if context.lookups.len() >= FileCtx::MAX_OPEN_CALLS {
            return Err(Error::Invalid("outstanding lookup capacity"));
        }
    }
    Ok(())
}
fn links(context: &mut FileCtx, rec: &mut Rec, id: u64) -> Result<u64> {
    Ok(match rec.kind {
        kind::TOOL_CALL => {
            if !rec.call_id.is_empty() && context.calls.iter().any(|c| c.0 == rec.call_id) {
                return Err(Error::Invalid("duplicate unresolved call ID"));
            }
            if !rec.call_id.is_empty() && context.calls.len() >= FileCtx::MAX_OPEN_CALLS {
                return Err(Error::Invalid("outstanding call capacity"));
            }
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
    })
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
fn provenance(source: Source, e: &Event) -> Provenance {
    Provenance {
        source,
        line_start: e.line_start,
        line_end: e.line_end,
        emission: e.emission,
    }
}
#[cfg(test)]
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "local_claude_tests.rs"]
mod claude_tests;

#[cfg(test)]
#[path = "local_pi_tests.rs"]
mod pi_tests;

#[cfg(test)]
#[path = "local_temporal_tests.rs"]
mod temporal_tests;
