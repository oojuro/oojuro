//! Oojuro v1 capture layer (spec: `docs/CAPTURE.md`, product model: `docs/DESIGN.md`).
//!
//! Attach-and-record: the agent never calls the database. `oojurod` reads the
//! logs agents already write (Claude Code project JSONL, Codex rollout JSONL),
//! maps every entry to a v1 record — a **writer ID** (the agent that observed
//! or produced it), timestamps and verbatim text — infers its structure (kind,
//! tool, sender) as derived metadata, masks secrets **before** anything is
//! stored, and appends into the writer's private store.
//!
//! - [`claude`] / [`codex`]: line parsers (robust to unknown entry types).
//! - [`redact`]: secret redaction on ingest.
//! - [`ingest`]: one-shot import and follow-mode tailer with crash-safe
//!   per-file offsets (see [`state`]).
//! - [`sink`]: the storage target (today `oojuro_trace::TraceDb`, one agent
//!   store per writer).
//! - [`marker`]: records that come from Oojuro itself (memory lookups,
//!   injected context): stored, never indexed.
pub mod claude;
pub mod codex;
pub mod ingest;
pub mod marker;
pub mod redact;
pub mod sink;
pub mod state;
pub mod stats;
pub mod time;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Context windows. A compaction replaces an agent's context with a
/// summary: afterwards the context holds the summary and what follows it.
/// The `system` records with these `sub` values mark where a new window
/// starts (Claude Code's `compact_boundary` entry and the `compact_summary`
/// after it, Codex's `compaction`). Without one, a session's whole history
/// is still in the agent's context.
pub mod context {
    pub const COMPACT_BOUNDARY: &str = "compact_boundary";
    pub const COMPACT_SUMMARY: &str = "compact_summary";
    pub const COMPACTION: &str = "compaction";
    pub const BOUNDARY_SUBS: [&str; 3] = [COMPACT_BOUNDARY, COMPACT_SUMMARY, COMPACTION];

    /// Whether a record (kind code, `sub` metadata) starts a new context window.
    pub fn is_boundary(kind: u8, sub: &str) -> bool {
        kind == super::kind::SYSTEM && BOUNDARY_SUBS.contains(&sub)
    }
}

/// Inferred record kinds. The numbers are the `oojuro_trace` kind codes
/// (`system, user, thinking, output, tool_call, tool_result`).
pub mod kind {
    pub const SYSTEM: u8 = 0;
    pub const USER_INPUT: u8 = 1;
    pub const THINKING: u8 = 2;
    pub const ASSISTANT: u8 = 3;
    pub const TOOL_CALL: u8 = 4;
    pub const TOOL_RESULT: u8 = 5;
    /// Written on purpose through the front door (`oojuro remember`), not
    /// parsed from a log.
    pub const EXPLICIT: u8 = 6;
    pub const NAMES: [&str; 7] = ["system", "user_input", "thinking", "assistant", "tool_call", "tool_result", "explicit"];

    /// Indexed for search: everything except harness context (`system`), which
    /// is stored only. Thinking is indexed (design decision 2026-10-20).
    pub fn indexed(k: u8) -> bool {
        matches!(k, USER_INPUT | THINKING | ASSISTANT | TOOL_CALL | TOOL_RESULT | EXPLICIT)
    }

    /// Kind code of a name (`NAMES`).
    pub fn code(name: &str) -> Option<u8> {
        NAMES.iter().position(|n| *n == name).map(|i| i as u8)
    }

    pub fn name(k: u8) -> &'static str {
        NAMES.get(k as usize).copied().unwrap_or("unknown")
    }
}

/// Tool status codes (same as `oojuro_trace`).
pub mod status {
    pub const NONE: u8 = 0;
    pub const OK: u8 = 2;
    pub const ERROR: u8 = 3;
}

/// Log formats the capture layer understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Format {
    ClaudeCode,
    Codex,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::ClaudeCode, Format::Codex];

    /// By file name: Codex writes `rollout-*.jsonl`; everything else is Claude Code JSONL.
    pub fn of_path(p: &std::path::Path) -> Self {
        match p.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.starts_with("rollout-") => Format::Codex,
            _ => Format::ClaudeCode,
        }
    }

    /// Short name (`claude-code`, `codex`), as in writer IDs.
    pub fn name(self) -> &'static str {
        match self {
            Format::ClaudeCode => "claude-code",
            Format::Codex => "codex",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    /// Where the agent writes its transcripts, relative to the home directory.
    pub fn transcript_root(self) -> &'static str {
        match self {
            Format::ClaudeCode => claude::TRANSCRIPT_ROOT,
            Format::Codex => codex::TRANSCRIPT_ROOT,
        }
    }

    /// How the agent shows itself in its model API calls.
    pub fn wire(self) -> &'static WireClient {
        match self {
            Format::ClaudeCode => &claude::WIRE,
            Format::Codex => &codex::WIRE,
        }
    }

    /// Whether tailing `dir` reads this agent's transcripts: `dir` is at or
    /// below the transcript root (`~/.claude/projects/-h-x`), or the root
    /// is (or would be) below `dir` (`~/.claude`, `~`).
    pub fn tailed_from(self, dir: &std::path::Path) -> bool {
        let root: Vec<&str> = self.transcript_root().split('/').collect();
        let comps: Vec<String> = dir.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        if comps.windows(root.len()).any(|w| w.iter().zip(&root).all(|(a, b)| a == b)) {
            return true;
        }
        // dir ends with the first k parts of the root and holds the rest
        (0..root.len()).any(|k| {
            comps.len() >= k
                && comps[comps.len() - k..].iter().zip(&root[..k]).all(|(a, b)| a == b)
                && dir.join(root[k..].join("/")).is_dir()
        })
    }

    /// The agent that made a model API call, from its `User-Agent` and
    /// other headers (`header` looks one up by lowercase name).
    pub fn of_client(user_agent: &str, header: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        let product = ua_product(user_agent);
        Self::ALL.into_iter().find(|f| {
            let w = f.wire();
            w.user_agents.iter().any(|p| pattern(p, &product))
                || w.headers.iter().any(|(h, p)| header(h).is_some_and(|v| pattern(p, &v.trim().to_ascii_lowercase())))
        })
    }
}

/// How an agent shows itself in its model API calls (declared next to its
/// transcript parser). The model API proxy leaves alone the calls of agents
/// whose transcripts the recorder tails (`docs/CAPTURE.md`).
#[derive(Debug)]
pub struct WireClient {
    /// User-agent product tokens (lowercase, the part before `/`); a
    /// trailing `*` matches a prefix.
    pub user_agents: &'static [&'static str],
    /// Headers that name the client: (lowercase header name, lowercase
    /// value; a trailing `*` matches a prefix).
    pub headers: &'static [(&'static str, &'static str)],
}

fn pattern(p: &str, v: &str) -> bool {
    match p.strip_suffix('*') {
        Some(prefix) => v.starts_with(prefix),
        None => v == p,
    }
}

/// The product token of a `User-Agent` (`claude-cli/2.1 (external, cli)` ->
/// `claude-cli`), lowercase, `[a-z0-9._-]`, at most 64 characters; empty
/// when there is none.
pub fn ua_product(ua: &str) -> String {
    let tok = ua.trim().split(|c: char| c == '/' || c.is_whitespace()).next().unwrap_or("");
    tok.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// One v1 record as parsed (before ids are assigned). Text is already redacted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rec {
    /// Writer ID: the agent whose experience stream this belongs to.
    pub writer: String,
    /// Valid time (when it happened), µs since the epoch.
    pub ts_us: i64,
    pub kind: u8,
    /// Tool name for calls (and results, filled from the call when known).
    pub tool: String,
    /// Working directory of the agent when known (Claude Code `cwd`, Codex
    /// `session_meta.cwd`); per-project settings are found from it.
    pub cwd: String,
    /// Verbatim text (secrets masked).
    pub text: String,
    /// Derived metadata (sender, subtype, model, ...), small string pairs.
    pub meta: Vec<(&'static str, String)>,
    pub status: u8,
    /// Session (hash of the source transcript), sequence in it, and turn
    /// (number of user inputs so far).
    pub session: u64,
    /// The agent instance (session) that spawned this one, when known (0 = none).
    pub parent_session: u64,
    pub seq: u32,
    pub turn: u32,
    /// Tool call id linking a call and its result (not stored as text).
    pub call_id: String,
    /// Byte offset of the source line in its file, and index within the line.
    pub line: u64,
    pub k: u32,
}

/// A record that comes from no log file (the model API proxy,
/// `docs/CAPTURE.md` "Model API proxy"), appended with
/// [`ingest::Capture::append_direct`]. Text is verbatim; the store owner
/// redacts it (again) before storing. Derived metadata rides in `meta`
/// (JSON values: model, usage, sub, partial, ...).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirectRec {
    pub writer: String,
    /// Valid time, µs since the epoch.
    pub ts_us: i64,
    pub kind: u8,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub meta: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub status: u8,
    #[serde(default)]
    pub session: u64,
    /// The agent instance that spawned this one, when the source knows it (0 = none).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub parent_session: u64,
    #[serde(default)]
    pub seq: u32,
    #[serde(default)]
    pub turn: u32,
    /// Links a tool result to its call (not stored as text).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    #[serde(default)]
    pub tokens_in: u32,
    #[serde(default)]
    pub tokens_out: u32,
    #[serde(default)]
    pub duration_us: i64,
    /// Working directory of the agent when known (per-project settings).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl Rec {
    /// Whether the record comes from Oojuro itself (a memory lookup, its
    /// result, or injected context; see [`marker`]): stored, never indexed.
    pub fn from_oojuro(&self) -> bool {
        self.meta.iter().any(|m| m.0 == marker::META)
    }
}

/// Counters a parser updates (merged into [`stats::Stats`]).
#[derive(Default, Debug, Clone, serde::Serialize)]
pub struct ParseCounts {
    /// Entry types seen (`format:type[/subtype]`).
    pub entries: std::collections::BTreeMap<String, u64>,
    /// Entries (or parts) skipped as noise: (count, raw bytes).
    pub noise: std::collections::BTreeMap<String, (u64, u64)>,
    /// Entry types the parser does not know (count).
    pub unknown: std::collections::BTreeMap<String, u64>,
    /// Bytes not stored because they repeat content stored elsewhere
    /// (UI copies of tool results, compaction replacement history, ...).
    pub resent: std::collections::BTreeMap<String, (u64, u64)>,
    /// Image payloads replaced by a placeholder: (count, bytes).
    pub images: (u64, u64),
    /// Oversized tool outputs saved to a file by the agent: (read in full,
    /// file missing or refused so the preview is kept, bytes read).
    pub persisted: (u64, u64, u64),
    pub bad_lines: u64,
}

impl ParseCounts {
    pub fn entry(&mut self, k: &str) {
        *self.entries.entry(k.to_string()).or_default() += 1;
    }
    pub fn noise(&mut self, k: &str, bytes: usize) {
        let e = self.noise.entry(k.to_string()).or_default();
        e.0 += 1;
        e.1 += bytes as u64;
    }
    pub fn unknown(&mut self, k: &str) {
        *self.unknown.entry(k.to_string()).or_default() += 1;
    }
    pub fn resent(&mut self, k: &str, bytes: usize) {
        let e = self.resent.entry(k.to_string()).or_default();
        e.0 += 1;
        e.1 += bytes as u64;
    }
}

/// Per-file parsing context, persisted with the file's offset so a restart
/// resumes mid-file with the same writer, session, sequence and turn.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileCtx {
    pub writer: String,
    /// The agent instance this transcript records (hash of its path).
    pub session: u64,
    /// The agent instance that spawned it (0 = a top-level session or not
    /// known yet): Claude Code subagents from their path, Codex subagents
    /// from `parent_thread`.
    #[serde(default)]
    pub parent_session: u64,
    /// Codex: the parent thread id a subagent rollout names (resolved to
    /// `parent_session` by the capture owner, which knows the rollout files).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_thread: String,
    /// Records emitted from this file so far (the per-session sequence).
    pub seq: u32,
    /// User inputs seen so far (the turn counter).
    pub turn: u32,
    /// Last valid time seen (used for entries without a timestamp).
    pub last_ts: i64,
    /// Working directory of the agent, when the log names it.
    #[serde(default)]
    pub cwd: String,
    /// Claude Code: a subagent (sidechain) transcript.
    #[serde(default)]
    pub sidechain: bool,
    /// Codex: who writes the user messages (`human`, `parent-agent`, `caller`).
    #[serde(default)]
    pub user_from: String,
    /// Codex: ordinals below this were inherited from the parent thread (-1 = none).
    #[serde(default)]
    pub inherit_below: i64,
    /// Codex: session_meta lines seen.
    #[serde(default)]
    pub metas: u32,
    /// Open tool calls (call id, tool, event id), newest last; bounded.
    #[serde(default)]
    pub calls: Vec<(String, String, u64)>,
    /// Open tool calls that go to Oojuro (their results are marked too); bounded.
    #[serde(default)]
    pub lookups: Vec<String>,
    /// Claude Code: the project folder whose `<session>/tool-results/` files
    /// hold oversized tool outputs (set when the file is opened, not persisted).
    #[serde(skip)]
    pub results_root: Option<std::path::PathBuf>,
}

impl FileCtx {
    pub const MAX_OPEN_CALLS: usize = 64;

    pub fn open_call(&mut self, call_id: &str, tool: &str, id: u64) {
        if call_id.is_empty() {
            return;
        }
        if self.calls.len() >= Self::MAX_OPEN_CALLS {
            self.calls.remove(0);
        }
        self.calls.push((call_id.to_string(), tool.to_string(), id));
    }

    /// Remember a call that goes to Oojuro, so its result is marked too.
    pub fn open_lookup(&mut self, call_id: &str) {
        if call_id.is_empty() {
            return;
        }
        if self.lookups.len() >= Self::MAX_OPEN_CALLS {
            self.lookups.remove(0);
        }
        self.lookups.push(call_id.to_string());
    }

    /// Whether `call_id` is an open call to Oojuro (and close it).
    pub fn close_lookup(&mut self, call_id: &str) -> bool {
        match self.lookups.iter().rposition(|c| c == call_id) {
            Some(i) if !call_id.is_empty() => {
                self.lookups.remove(i);
                true
            }
            _ => false,
        }
    }

    /// Resolve (and close) a call: (tool, event id).
    pub fn close_call(&mut self, call_id: &str) -> Option<(String, u64)> {
        let i = self.calls.iter().rposition(|c| c.0 == call_id)?;
        let (_, t, id) = self.calls.remove(i);
        Some((t, id))
    }
}

/// Stable 64-bit hash of a string (session ids).
pub fn hash64(s: &str) -> u64 {
    let h = blake3::hash(s.as_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
}

/// Encode a directory the way Claude Code names its project folders
/// (every character outside `[A-Za-z0-9]` becomes `-`), so the same directory
/// gives the same `<project>` for every agent.
pub fn encode_dir(dir: &str) -> String {
    dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// A placeholder for an image payload: media type, payload size and a short hash.
pub fn image_placeholder(media: &str, data: &str, counts: &mut ParseCounts) -> String {
    counts.images.0 += 1;
    counts.images.1 += data.len() as u64;
    let h = blake3::hash(data.as_bytes()).to_hex();
    format!("[image:{media}:{}B:{}]", data.len(), &h.as_str()[..8])
}

/// Builds the records of one source line: redacts text, numbers records
/// (`seq`, `turn`, index within the line) and drops empty texts.
pub struct Emitter<'a> {
    pub red: &'a redact::Redactor,
    pub rc: &'a mut redact::Counts,
    pub pc: &'a mut ParseCounts,
    pub out: &'a mut Vec<Rec>,
    pub line: u64,
    pub k: u32,
}

impl Emitter<'_> {
    #[allow(clippy::too_many_arguments)]
    pub fn emit(
        &mut self,
        ctx: &mut FileCtx,
        ts_us: i64,
        kind: u8,
        tool: &str,
        text: &str,
        meta: Vec<(&'static str, String)>,
        status: u8,
        call_id: &str,
    ) {
        if text.is_empty() {
            self.pc.noise("empty_text", 0);
            return;
        }
        if kind == kind::USER_INPUT {
            ctx.turn += 1;
        }
        let mut meta = meta;
        if marker::has_context(text) {
            marker::mark(&mut meta, marker::CONTEXT);
        }
        let text = self.red.redact(text, self.rc);
        self.out.push(Rec {
            writer: ctx.writer.clone(),
            ts_us,
            kind,
            tool: tool.to_string(),
            cwd: ctx.cwd.clone(),
            text,
            meta,
            status,
            session: ctx.session,
            parent_session: ctx.parent_session,
            seq: ctx.seq,
            turn: ctx.turn,
            call_id: call_id.to_string(),
            line: self.line,
            k: self.k,
        });
        ctx.seq += 1;
        self.k += 1;
    }
}

/// Leading `<tag>` of a text (after whitespace), if any.
pub fn leading_tag(s: &str) -> Option<&str> {
    let t = s.trim_start();
    let rest = t.strip_prefix('<')?;
    let end = rest.find('>')?;
    let tag = &rest[..end];
    (!tag.is_empty() && tag.len() <= 48 && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .then_some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_recognised_on_the_wire() {
        let none = |_: &str| None;
        assert_eq!(Format::of_client("claude-cli/2.1.3 (external, cli)", &none), Some(Format::ClaudeCode));
        assert_eq!(Format::of_client("claude-cli/2.0.1 (external, sdk-ts)", &none), Some(Format::ClaudeCode));
        assert_eq!(Format::of_client("codex_cli_rs/0.50.0 (Ubuntu 24.4.0; x86_64) xterm", &none), Some(Format::Codex));
        assert_eq!(Format::of_client("codex-tui/0.60.1", &none), Some(Format::Codex));
        assert_eq!(Format::of_client("OpenAI/Python 1.99.0", &none), None);
        assert_eq!(Format::of_client("Anthropic/JS 0.60.0", &none), None);
        assert_eq!(Format::of_client("", &none), None);
        let originator = |h: &str| (h == "originator").then(|| "codex_exec".to_string());
        assert_eq!(Format::of_client("reqwest/0.12", &originator), Some(Format::Codex));
        let x_app = |h: &str| (h == "x-app").then(|| "CLI".to_string());
        assert_eq!(Format::of_client("", &x_app), Some(Format::ClaudeCode));
        assert_eq!(ua_product("claude-cli/2.1 (external)"), "claude-cli");
    }

    #[test]
    fn tailed_formats_from_watched_directories() {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let home = base.join(format!("oj-fmt-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".claude/projects/-h-x")).unwrap();
        let p = |s: &str| home.join(s);
        assert!(Format::ClaudeCode.tailed_from(&p(".claude/projects")));
        assert!(Format::ClaudeCode.tailed_from(&p(".claude/projects/-h-x")));
        assert!(Format::ClaudeCode.tailed_from(&p(".claude")));
        assert!(Format::ClaudeCode.tailed_from(&home));
        assert!(!Format::Codex.tailed_from(&home), "no ~/.codex/sessions here");
        assert!(Format::Codex.tailed_from(&p(".codex/sessions")), "the root itself, even before it exists");
        assert!(!Format::ClaudeCode.tailed_from(&p(".codex/sessions")));
        assert!(!Format::Codex.tailed_from(&p("logs")));
        assert_eq!(Format::from_name("codex"), Some(Format::Codex));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
