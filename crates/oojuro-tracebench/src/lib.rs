//! TraceBench: the agent-trace database workload. Spec: `docs/TRACEBENCH.md`.
//!
//! Every arm implements [`TraceStore`]. The runner (`src/bin/tracebench.rs`)
//! replays a tier's turn batches through it, times every operation and
//! checks every deterministic answer against the [`arms::oracle`] arm.
pub mod arms;
pub mod events;
pub mod stats;
pub mod sys;
pub mod ycsb;

pub use events::Event;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// An operation the arm's engine cannot do under the contract (for example
/// full-text search in a key-value store, a server-side aggregation an
/// engine does not have, or a delete that is not invisible on return). Arms
/// return it instead of an approximation; the suites skip the operation and
/// list it, with `why`, under `unsupported` in their output.
#[derive(Debug)]
pub struct Unsupported {
    pub op: &'static str,
    pub why: String,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported: {}: {}", self.op, self.why)
    }
}

impl std::error::Error for Unsupported {}

/// `Err(Unsupported)` for operation `op`.
pub fn unsupported<T>(op: &'static str, why: impl Into<String>) -> Result<T> {
    Err(Box::new(Unsupported { op, why: why.into() }))
}

/// The reason if `e` is an [`Unsupported`] error.
pub fn unsupported_reason(e: &Error) -> Option<String> {
    e.downcast_ref::<Unsupported>().map(|u| u.why.clone())
}

/// One event as read back, with the latest status of a tool call applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
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

impl Row {
    pub fn from_event(e: &Event) -> Self {
        Self {
            id: e.id,
            session: e.session,
            agent: e.agent,
            turn: e.turn,
            seq: e.seq,
            parent: e.parent,
            kind: e.kind,
            status: e.status,
            exit_code: e.exit_code,
            tokens_in: e.tokens_in,
            tokens_out: e.tokens_out,
            ts_us: e.ts_us,
            duration_us: e.duration_us,
            tool: e.tool.to_owned(),
            args: e.args.to_owned(),
            text: e.text.to_owned(),
        }
    }
    /// Apply a status revision (the tool call completed).
    pub fn apply(&mut self, rev: &Event) {
        self.status = rev.status;
        self.exit_code = rev.exit_code;
        self.duration_us = rev.duration_us;
    }
}

/// Typed filter. Every set condition must hold. Time is `[from_us, to_us)`
/// on the event clock. `status` matches the latest status (after revisions).
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub agent: Option<u32>,
    pub kind: Option<u8>,
    pub tool: Option<String>,
    pub status: Option<u8>,
    pub from_us: i64,
    pub to_us: i64,
}

/// Per-session aggregate (status revisions are not events here).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionAgg {
    pub events: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub text_bytes: u64,
}

/// Per-tool aggregate over tool calls with `ts` in a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolAgg {
    pub tool: String,
    pub calls: u64,
    pub errors: u64,
    pub duration_us: i64,
}

/// The contract every arm implements. Every method may be called from many
/// threads at once. Status revisions arrive inside `append` batches; an arm
/// may apply them in place (UPDATE) or as appended revisions, but every read
/// must return the latest status. An operation the engine cannot do returns
/// [`Unsupported`] (never an approximation); the suites skip and label it.
pub trait TraceStore: Send + Sync {
    fn name(&self) -> String;
    /// Durable on return: the batch survives a crash (one commit per batch).
    fn append(&self, batch: &[Event]) -> Result<()>;
    /// Bulk path used to build stores for the read suites (many batches per
    /// commit is allowed). Durable once `finish_load` returns. Default: append.
    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        for b in batches {
            self.append(b)?;
        }
        Ok(())
    }
    /// One durable commit holding several turn batches, possibly of several
    /// agents (the batched variant of the `ycsb` suite: N client writes per
    /// commit). Durable on return. Default: one `append` of all the events;
    /// arms whose `append` takes one agent per batch override it.
    fn append_many(&self, batches: &[Vec<Event>]) -> Result<()> {
        let all: Vec<Event> = batches.concat();
        self.append(&all)
    }
    /// End of a bulk load: make loaded data durable and settled (checkpoint,
    /// flush, ANALYZE); timed separately from queries.
    fn finish_load(&self) -> Result<()> {
        Ok(())
    }
    fn get(&self, id: u64) -> Result<Option<Row>>;
    /// Bytes `start..end` of the event's text (clamped to the text length).
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>>;
    /// The whole session in `seq` order (status revisions applied, not listed).
    fn session(&self, session: u64) -> Result<Vec<Row>>;
    /// Events with `from_us <= ts < to_us` in (ts, id) order, at most `limit`.
    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>>;
    /// Ids of matching events in (ts, id) order, at most `limit`.
    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>>;
    fn count(&self, f: &Filter) -> Result<u64>;
    /// Top-`k` ids for `query` among one agent's indexed events with
    /// `ts <= as_of_us`, best first. Ranking is the arm's own.
    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>>;
    fn agg_session(&self, session: u64) -> Result<SessionAgg>;
    /// Per-tool call counts, errors and total duration over tool calls with
    /// `from_us <= ts < to_us`, sorted by tool name.
    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>>;
    /// Sessions with the most tokens (in + out) among events in the window,
    /// best first, ties by session id ascending; at most `n`.
    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>>;
    /// Logical delete; invisible to every read on return, durable.
    fn delete_session(&self, session: u64) -> Result<()>;
    /// Delete every event with `ts < ts_us`; invisible on return, durable.
    fn purge_before(&self, ts_us: i64) -> Result<()>;
    /// Make deletes physical: afterwards no store file may contain deleted text.
    fn reclaim(&self) -> Result<()>;
    /// Every live event as one JSON line ([`export_line`]), in (session, seq)
    /// order. Returns (events, bytes written).
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)>;
    /// Bytes on disk of everything the store needs (data, indexes, logs).
    fn disk_bytes(&self) -> Result<u64>;
    /// Resident memory of the database engine, excluding the file cache.
    fn mem_bytes(&self) -> Result<u64>;
    /// Files the erasure proof scans (every file of the store).
    fn data_paths(&self) -> Result<Vec<std::path::PathBuf>>;
    /// Optional arm-specific measurements (e.g. a disk breakdown), recorded
    /// after the load.
    fn details(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Optional durability counters (e.g. journal syncs since start), read
    /// at the end of an ingest run to report syncs per commit.
    fn sync_counters(&self) -> Option<serde_json::Value> {
        None
    }
    /// Format-level erasure check: decode every stored record (live or
    /// not) and count needle occurrences. `None` = the arm cannot.
    fn deep_scan(&self, _needles: &[Vec<u8>]) -> Result<Option<u64>> {
        Ok(None)
    }
}

/// The JSON line `export` writes for one row (shared so arms agree byte for byte).
pub fn export_line(r: &Row, out: &mut Vec<u8>) {
    let v = serde_json::json!({
        "id": r.id, "session": r.session, "agent": r.agent, "turn": r.turn, "seq": r.seq,
        "parent": r.parent, "kind": events::KIND_NAMES[r.kind as usize], "status": r.status,
        "exit_code": r.exit_code, "tokens_in": r.tokens_in, "tokens_out": r.tokens_out,
        "ts_us": r.ts_us, "duration_us": r.duration_us, "tool": r.tool, "args": r.args, "text": r.text,
    });
    serde_json::to_writer(&mut *out, &v).expect("json");
    out.push(b'\n');
}
