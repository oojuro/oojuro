//! Claude Code hooks: `oojuro hook <event>` (`docs/CAPTURE.md`, "Claude
//! Code hooks").
//!
//! Claude Code runs the command with the hook's JSON on stdin. The hook
//! talks to the recorder over its socket (or opens the journal itself when
//! no recorder runs), and always exits 0: a broken journal never blocks or
//! fails the agent (errors go to stderr, stdout stays empty).
//!
//! - **Recording.** When the payload's `transcript_path` lies under a
//!   directory the recorder tails, the log tailer records the session and
//!   the hook records nothing. Otherwise (no recorder, a recorder tailing
//!   other directories, a recorder started with `oojurod otel`) it records
//!   the prompt, tool calls, tool results and the final answer as the same
//!   records the tailer would make (writer, session and kinds), through the
//!   append path (settings, redaction, store-once keys).
//! - **Injection** (`UserPromptSubmit`, when `inject` is on for the writer):
//!   search the agent's own store with the prompt and return the top
//!   records as `additionalContext`, wrapped in an `<oojuro-context>`
//!   envelope (so no capture path ever indexes it again).
use crate::{
    Result,
    append::{Appended, NewRecord},
    client::Oojuro,
    config::{Config, Settings},
    journal::{self, Journal},
    paths::Paths,
    proto::{self, Request},
    types::{Filters, Ref, SearchArgs},
};
use oojuro_capture::{claude::project_of, encode_dir, hash64, marker, time::now_us};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Events the hook handles.
pub const EVENTS: [&str; 7] =
    ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "Stop", "SubagentStop"];

/// Records valid less than this long before the hook started belong to the
/// current turn and are never injected (the prompt may already be in the
/// journal through the log tailer).
pub const TURN_GUARD_US: i64 = 2_000_000;
/// Claude Code caps `additionalContext` at 10,000 characters (longer text
/// is replaced by a file path and a preview).
pub const MAX_CONTEXT_CHARS: usize = 9_500;
/// Longest hook input read from stdin.
pub const MAX_INPUT_BYTES: u64 = 128 << 20;
const HELLO_TIMEOUT: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a direct call waits for another process's hold on the journal.
const LOCK_WAIT: Duration = Duration::from_secs(3);
/// Characters per token when estimating the injection budget.
const CHARS_PER_TOKEN: usize = 4;

fn s<'v>(v: &'v Value, k: &str) -> &'v str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => Path::new(&h).join(rest).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
}

/// Whose records a hook payload holds, and which transcript the log
/// tailer reads for them.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub event: String,
    /// `claude-code:<project>`, as the log tailer names the agent.
    pub writer: String,
    /// The transcript of the session (a subagent's own file inside a
    /// subagent); its path is the session key, as in the tailer.
    pub transcript: String,
    pub cwd: String,
}

pub fn target(event: &str, p: &Value) -> Target {
    let main = expand_home(s(p, "transcript_path"));
    let agent = s(p, "agent_id");
    let transcript = if event == "SubagentStop" && !s(p, "agent_transcript_path").is_empty() {
        expand_home(s(p, "agent_transcript_path"))
    } else if !agent.is_empty() && main.ends_with(".jsonl") {
        // <project>/<session>.jsonl -> <project>/<session>/subagents/agent-<id>.jsonl
        let dir = Path::new(&main).with_extension("");
        dir.join("subagents").join(format!("agent-{agent}.jsonl")).to_string_lossy().into_owned()
    } else {
        main.clone()
    };
    let cwd = s(p, "cwd").to_string();
    let writer = if main.is_empty() {
        format!("claude-code:{}", encode_dir(&cwd))
    } else {
        format!("claude-code:{}", project_of(Path::new(&main)))
    };
    Target { event: event.to_string(), writer, transcript, cwd }
}

/// Whether the recorder that answered (its hello) tails the directory the
/// transcript lives in.
pub fn watched(transcript: &str, hello: Option<&Value>) -> bool {
    let Some(dirs) = hello.and_then(|h| h.get("watch")).and_then(Value::as_array) else { return false };
    if transcript.is_empty() {
        return false;
    }
    let t = Path::new(transcript);
    let t = match t.parent().and_then(|d| std::fs::canonicalize(d).ok()) {
        Some(d) => d.join(t.file_name().unwrap_or_default()),
        None => t.to_path_buf(),
    };
    dirs.iter().filter_map(Value::as_str).any(|d| t.starts_with(d))
}

#[derive(Deserialize, Default)]
struct Raw<'a> {
    #[serde(borrow, default)]
    tool_input: Option<&'a RawValue>,
    #[serde(borrow, default)]
    tool_response: Option<&'a RawValue>,
}

/// Text of a tool response as the model saw it, as far as the hook can
/// tell: a string as it is; Bash's `stdout` (and `stderr`); a `content`
/// string; a file read's `file.content`; MCP text blocks; else the JSON.
pub fn response_text(raw: Option<&RawValue>) -> String {
    let Some(r) = raw else { return String::new() };
    let v: Value = serde_json::from_str(r.get()).unwrap_or(Value::Null);
    match &v {
        Value::String(x) => x.clone(),
        Value::Object(o) => {
            if let Some(out) = o.get("stdout").and_then(Value::as_str) {
                let mut t = out.to_string();
                if let Some(e) = o.get("stderr").and_then(Value::as_str).filter(|e| !e.is_empty()) {
                    if !t.is_empty() {
                        t.push('\n');
                    }
                    t.push_str(e);
                }
                return t;
            }
            if let Some(c) = o.get("content").and_then(Value::as_str) {
                return c.to_string();
            }
            if let Some(c) = o.get("file").and_then(|f| f.get("content")).and_then(Value::as_str) {
                return c.to_string();
            }
            r.get().to_string()
        }
        Value::Array(items) if items.iter().all(|b| b.get("type").and_then(Value::as_str) == Some("text")) => {
            items.iter().map(|b| s(b, "text")).collect::<Vec<_>>().join("\n")
        }
        _ => r.get().to_string(),
    }
}

/// The records a hook payload holds (before settings are applied).
pub fn records(t: &Target, p: &Value, raw_input: &str, ts_us: i64) -> Vec<NewRecord> {
    let raw: Raw = serde_json::from_str(raw_input).unwrap_or_default();
    let base = NewRecord {
        writer: t.writer.clone(),
        ts_us,
        session: t.transcript.clone(),
        cwd: t.cwd.clone(),
        source: t.transcript.clone(),
        via: "hook".into(),
        ..NewRecord::default()
    };
    let tool_use_id = s(p, "tool_use_id");
    let call = || {
        let mut r = NewRecord {
            kind: "tool_call".into(),
            tool: s(p, "tool_name").into(),
            text: raw.tool_input.map(|x| x.get().to_string()).unwrap_or_default(),
            call_id: tool_use_id.into(),
            namespace: p.get("mcp_server").map(|m| s(m, "name")).unwrap_or("").into(),
            ..base.clone()
        };
        if !tool_use_id.is_empty() {
            r.dedupe = format!("call:{tool_use_id}");
        }
        r
    };
    let result = |text: String, status: &str| {
        let mut r = NewRecord {
            kind: "tool_result".into(),
            tool: s(p, "tool_name").into(),
            text,
            call_id: tool_use_id.into(),
            status: status.into(),
            ..base.clone()
        };
        if !tool_use_id.is_empty() {
            r.dedupe = format!("result:{tool_use_id}");
        }
        if let Some(ms) = p.get("duration_ms").and_then(Value::as_u64) {
            r.meta.insert("duration_ms".into(), ms.to_string());
        }
        r
    };
    match t.event.as_str() {
        "UserPromptSubmit" => {
            let mut r = NewRecord { kind: "user_input".into(), text: s(p, "prompt").into(), ..base.clone() };
            let from = if s(p, "agent_id").is_empty() { "human" } else { "parent-agent" };
            r.meta.insert("from".into(), from.into());
            if !s(p, "prompt_id").is_empty() {
                r.dedupe = format!("prompt:{}", s(p, "prompt_id"));
            }
            vec![r]
        }
        "PreToolUse" => vec![call()],
        "PostToolUse" => vec![call(), result(response_text(raw.tool_response), "ok")],
        "PostToolUseFailure" => {
            let mut r = result(s(p, "error").to_string(), "error");
            if p.get("is_interrupt").and_then(Value::as_bool) == Some(true) {
                r.meta.insert("interrupt".into(), "1".into());
            }
            vec![call(), r]
        }
        "Stop" | "SubagentStop" => {
            let text = s(p, "last_assistant_message");
            let mut r = NewRecord { kind: "assistant".into(), text: text.into(), ..base.clone() };
            r.dedupe = format!("answer:{}:{}:{:016x}", s(p, "prompt_id"), s(p, "agent_id"), hash64(text));
            vec![r]
        }
        "SessionStart" if s(p, "source") == "compact" => {
            // a compaction: the context window starts again here (as the
            // transcript's compact_boundary entry tells the log tailer)
            let mut r = NewRecord { kind: "system".into(), text: "Conversation compacted".into(), ..base.clone() };
            r.meta.insert("sub".into(), oojuro_capture::context::COMPACT_BOUNDARY.into());
            r.meta.insert("from".into(), "hook".into());
            vec![r]
        }
        _ => Vec::new(), // SessionStart otherwise: nothing to record
    }
}

/// A hit the agent already has, or Oojuro's own echo from before records
/// were marked: an injection envelope, a call to Oojuro, the prompt itself.
fn echo(h: &Ref, prompt: &str) -> bool {
    h.excerpt.contains(marker::OPEN)
        || (h.kind == "tool_call" && marker::is_oojuro_call("", "", &h.excerpt))
        || (h.kind == "user_input" && h.excerpt.trim() == prompt.trim())
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn cut_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// One injected item: a header line and the excerpt, indented (so no line
/// of it can open or close an envelope).
fn item(n: usize, h: &Ref, max_excerpt: Option<usize>) -> String {
    let when = h.ts.get(..16).map(|x| x.replace('T', " ")).unwrap_or_else(|| h.ts.clone());
    let mut ex = h.excerpt.trim().to_string();
    let mut tail_cut = h.end < h.len;
    if let Some(m) = max_excerpt
        && chars(&ex) > m
    {
        ex = cut_chars(&ex, m).trim_end().to_string();
        tail_cut = true;
    }
    let mut out = format!("[{n}] #{} {when} {} ({:.1} bits)\n", h.id, h.kind, h.score_bits);
    let lines: Vec<&str> = ex.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    for (i, l) in lines.iter().enumerate() {
        out.push_str("  ");
        if i == 0 && h.start > 0 {
            out.push_str("... ");
        }
        out.push_str(l);
        if i + 1 == lines.len() && tail_cut {
            out.push_str(" ...");
        }
        out.push('\n');
    }
    out
}

/// The recency prior of one hit, in bits, for a turn of `session` (16 hex
/// digits, as in [`Ref::session`]): model `session-turn-decay-v1` of the
/// recency research. A record
/// of the current session `d` turns old loses `recency * d` bits; the
/// session's task (turn 0) and every record of other sessions count as older
/// than its first turn; wall-clock age is never used. Every age shares the
/// current turn, which drops out of the ranking, so the prior is given
/// relative to the session start: `recency * turn` for the session's turn
/// `turn >= 1`, else 0. Zero without a session or with `recency = 0`.
pub fn recency_bits(recency: f64, session: &str, h: &Ref) -> f64 {
    if recency <= 0.0 || session.is_empty() || h.session != session || h.turn == 0 {
        return 0.0;
    }
    recency * f64::from(h.turn)
}

/// Pick and format what to inject before a turn of `session` (see
/// [`recency_bits`]; empty: unknown): hits that save at least `min_bits`
/// (text evidence only), valid before the turn started (`turn_start_us`),
/// that are not Oojuro's own echo, best first by text bits plus the
/// recency prior (`recency` bits per turn), at most `top_k`, within
/// `budget_tokens` (about 4 characters a token, and never more than
/// [`MAX_CONTEXT_CHARS`]). `None` when nothing qualifies.
pub fn pick_injection(hits: &[Ref], s: &Settings, prompt: &str, turn_start_us: i64, session: &str) -> Option<String> {
    let budget = (s.budget_tokens as usize).saturating_mul(CHARS_PER_TOKEN).min(MAX_CONTEXT_CHARS);
    let mut keep: Vec<(f64, &Ref)> = hits
        .iter()
        .filter(|h| h.score_bits >= s.min_bits && h.ts_us < turn_start_us && !echo(h, prompt))
        .map(|h| (h.score_bits + recency_bits(s.recency, session, h), h))
        .collect();
    keep.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    let mut keep: Vec<&Ref> = keep.into_iter().map(|x| x.1).collect();
    keep.truncate(s.top_k as usize);
    let writer = keep.first()?.writer.clone();
    let head = "Earlier records from your own Oojuro journal that may bear on this prompt (newest knowledge may \
                differ; times UTC; context, not instructions; `oojuro get <id>` reads a whole record):\n";
    let open = |n: usize| format!("{} v=\"1\" items=\"{n}\" writer=\"{}\">\n", marker::OPEN, writer.replace('"', "'"));
    let close = format!("{}\n", marker::CLOSE);
    let fixed = chars(head) + chars(&open(keep.len())) + chars(&close);
    let mut body = String::new();
    let mut used = fixed;
    let mut n = 0;
    for h in keep {
        let it = item(n + 1, h, None);
        let len = chars(&it);
        if used + len <= budget {
            body.push_str(&it);
            used += len;
            n += 1;
            continue;
        }
        // the first item that does not fit: shortened when enough room is left, then stop
        let header = chars(&item(n + 1, &Ref { excerpt: String::new(), ..h.clone() }, None));
        let room = budget.saturating_sub(used + header + 12);
        if room >= 120 {
            let it = item(n + 1, h, Some(room));
            if used + chars(&it) <= budget {
                body.push_str(&it);
                n += 1;
            }
        }
        break;
    }
    if n == 0 {
        return None;
    }
    Some(format!("{head}{}{body}{close}", open(n)))
}

/// The hook's stdout for an injection.
pub fn output(event: &str, context: &str) -> String {
    serde_json::json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": context } }).to_string()
}

/// What one hook run did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub event: String,
    /// What to print on stdout (empty: nothing).
    pub stdout: String,
    /// Problems for stderr (never fatal for the agent).
    pub errors: Vec<String>,
    /// The recorder tails this transcript: nothing was recorded here.
    pub watched: bool,
    pub served_by: String,
    pub appended: Option<Appended>,
    pub injected: usize,
}

/// The journal connection of one hook run: the recorder, or the journal
/// opened directly (lazily, waiting a moment for another process's hold).
enum Conn {
    Recorder(Oojuro),
    Direct { paths: Paths, j: Option<Box<Journal>> },
}

fn is_held(e: &crate::Error) -> bool {
    let m = e.to_string().to_ascii_lowercase();
    m.contains("another process") || m.contains("would block") || m.contains("locked")
}

impl Conn {
    fn call(&mut self, req: &Request) -> Result<Option<Value>> {
        match self {
            Conn::Recorder(oj) => oj.request_within(req, REQUEST_TIMEOUT).map(Some),
            Conn::Direct { paths, j } => {
                if j.is_none() {
                    let create = matches!(req, Request::Append { .. });
                    if !create && !journal::exists(&paths.store) {
                        return Ok(None);
                    }
                    let deadline = Instant::now() + LOCK_WAIT;
                    let opened = loop {
                        match Journal::open(&paths.store) {
                            Ok(x) => break x,
                            Err(e) if is_held(&e) && Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(15))
                            }
                            Err(e) => return Err(e),
                        }
                    };
                    *j = Some(Box::new(opened));
                    let jj = j.as_mut().unwrap();
                    jj.capture().set_policy(Box::new(crate::policy::RecordingPolicy::new(&paths.config)));
                }
                proto::execute(j.as_mut().unwrap(), req).map(Some)
            }
        }
    }

    fn close(self) -> Result<()> {
        if let Conn::Direct { j: Some(j), .. } = self {
            j.close()?;
        }
        Ok(())
    }
}

/// Characters of excerpt to ask the search for, per result.
fn excerpt_bytes(s: &Settings) -> usize {
    let budget = (s.budget_tokens as usize).saturating_mul(CHARS_PER_TOKEN).min(MAX_CONTEXT_CHARS);
    (budget / (s.top_k.max(1) as usize)).clamp(200, 2000)
}

/// A UTF-8 prefix of at most `max` bytes.
fn prefix(s: &str, max: usize) -> &str {
    let mut e = s.len().min(max);
    while !s.is_char_boundary(e) {
        e -= 1;
    }
    &s[..e]
}

/// Run one hook: `event` from the command line (else the payload's
/// `hook_event_name`), the hook JSON as `input`.
pub fn run(event: Option<&str>, paths: &Paths, input: &str) -> Outcome {
    let started = now_us();
    let mut o = Outcome::default();
    let p: Value = match serde_json::from_str(input) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            o.errors.push("hook input is not a JSON object".into());
            return o;
        }
        Err(e) => {
            o.errors.push(format!("hook input is not JSON: {e}"));
            return o;
        }
    };
    let ev = event.filter(|e| !e.is_empty()).unwrap_or_else(|| s(&p, "hook_event_name")).to_string();
    if !EVENTS.contains(&ev.as_str()) {
        o.errors.push(format!("unknown hook event {ev:?} (known: {})", EVENTS.join(", ")));
        return o;
    }
    o.event = ev.clone();
    let t = target(&ev, &p);
    let cwd = if t.cwd.is_empty() { std::env::current_dir().unwrap_or_default() } else { PathBuf::from(&t.cwd) };
    let settings = match Config::discover(&paths.config, &cwd).and_then(|c| c.resolve(Some(&t.writer))) {
        Ok(r) => Some(r.settings),
        Err(e) => {
            o.errors.push(format!("settings: {e} (no injection)"));
            None
        }
    };
    let (oj, hello) = Oojuro::open_within(paths.clone(), HELLO_TIMEOUT);
    o.served_by = oj.served_by();
    o.watched = watched(&t.transcript, hello.as_ref());
    let mut recs = if o.watched { Vec::new() } else { records(&t, &p, input, started) };
    let prompt = s(&p, "prompt");
    let inject = ev == "UserPromptSubmit" && settings.as_ref().is_some_and(|x| x.inject) && !prompt.trim().is_empty();
    if recs.is_empty() && !inject {
        return o;
    }
    let mut conn = match oj.via {
        crate::Via::Recorder { .. } => Conn::Recorder(oj),
        crate::Via::Direct => Conn::Direct { paths: paths.clone(), j: None },
    };
    // the session as stored (16 hex digits), for the context window and the recency prior
    let session = if t.transcript.is_empty() {
        String::new()
    } else {
        format!("{:016x}", crate::append::session_id(&t.transcript))
    };
    if inject {
        let st = settings.as_ref().unwrap();
        let turn_start = started - TURN_GUARD_US;
        let args = SearchArgs {
            query: prefix(prompt, st.max_query_bytes as usize).to_string(),
            // room for hits dropped as echo (repeats of the prompt, unmarked old lookups)
            k: (st.top_k as usize * 3 + 10).clamp(1, crate::journal::MAX_K),
            // never what the agent still has in context: this session since its last compaction
            filters: Filters {
                to_us: Some(turn_start),
                writers: vec![t.writer.clone()],
                outside_context_of: (!session.is_empty()).then(|| session.clone()),
                ..Filters::default()
            },
            excerpt_bytes: excerpt_bytes(st),
            max_query_bytes: st.max_query_bytes as usize,
        };
        match conn.call(&Request::Search(args)) {
            Ok(Some(v)) => match serde_json::from_value::<Vec<Ref>>(v) {
                Ok(hits) => {
                    if let Some(text) = pick_injection(&hits, st, prompt, turn_start, &session) {
                        o.injected = text.matches("\n[").count();
                        o.stdout = output(&ev, &text);
                        if !o.watched {
                            // the agent sees it: recorded (marked, never indexed)
                            recs.push(NewRecord {
                                writer: t.writer.clone(),
                                kind: "system".into(),
                                ts_us: started,
                                text,
                                session: t.transcript.clone(),
                                cwd: t.cwd.clone(),
                                source: t.transcript.clone(),
                                via: "hook".into(),
                                meta: [("sub".to_string(), "hook_additional_context".to_string())].into(),
                                ..NewRecord::default()
                            });
                        }
                    }
                }
                Err(e) => o.errors.push(format!("search answer: {e}")),
            },
            Ok(None) => {} // no journal yet
            Err(e) => o.errors.push(format!("search: {e}")),
        }
    }
    if !recs.is_empty() {
        match conn.call(&Request::Append { records: recs }) {
            Ok(Some(v)) => match serde_json::from_value::<Appended>(v) {
                Ok(a) => o.appended = Some(a),
                Err(e) => o.errors.push(format!("append answer: {e}")),
            },
            Ok(None) => {}
            Err(e) => o.errors.push(format!("record: {e}")),
        }
    }
    if let Err(e) = conn.close() {
        o.errors.push(format!("close: {e}"));
    }
    o
}

/// Quote a word for a POSIX shell when needed.
pub fn shell_quote(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@%".contains(c)) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// The settings entry to add to Claude Code's `settings.json` (`hooks`),
/// running `command` (the `oojuro` binary and any global options) for each
/// event. Recording-only events run `async` (they never delay the agent);
/// `UserPromptSubmit` waits, because its output is the injection.
pub fn settings_snippet(command: &str) -> Value {
    let entry = |ev: &str, matcher: bool, run_async: bool| {
        let mut h = serde_json::json!({ "type": "command", "command": format!("{command} hook {ev}"), "timeout": 10 });
        if run_async {
            h["async"] = Value::Bool(true);
        }
        let mut group = serde_json::json!({ "hooks": [h] });
        if matcher {
            group["matcher"] = Value::String("*".into());
        }
        serde_json::json!([group])
    };
    serde_json::json!({
        "hooks": {
            "SessionStart": [{ "matcher": "compact", "hooks": entry("SessionStart", false, true)[0]["hooks"].clone() }],
            "UserPromptSubmit": entry("UserPromptSubmit", false, false),
            "PreToolUse": entry("PreToolUse", true, true),
            "PostToolUse": entry("PostToolUse", true, true),
            "PostToolUseFailure": entry("PostToolUseFailure", true, true),
            "Stop": entry("Stop", false, true),
            "SubagentStop": entry("SubagentStop", false, true),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u64, bits: f64, ts_us: i64, kind: &str, excerpt: &str) -> Ref {
        Ref {
            id,
            writer: "claude-code:-h-demo".into(),
            kind: kind.into(),
            key: None,
            ts_us,
            ts: "2026-10-20T14:03:05.000000Z".into(),
            score_bits: bits,
            start: 0,
            end: excerpt.len(),
            len: excerpt.len(),
            excerpt: excerpt.into(),
            session: String::new(),
            turn: 0,
        }
    }

    /// The hit, in `session` at `turn`.
    fn at(mut h: Ref, session: &str, turn: u32) -> Ref {
        h.session = session.into();
        h.turn = turn;
        h
    }

    /// Record ids in injection order.
    fn ids(text: &str) -> Vec<u64> {
        text.lines()
            .filter(|l| l.starts_with('['))
            .map(|l| l.split('#').nth(1).unwrap().split(' ').next().unwrap().parse().unwrap())
            .collect()
    }

    /// Near-ties of the current session `s` at turns 1, 9 and 5, strong
    /// archive evidence, and a late turn of another session.
    fn mixed() -> Vec<Ref> {
        vec![
            at(hit(1, 12.0, 1, "assistant", "alpha one"), "s", 1),
            at(hit(2, 11.9, 1, "assistant", "alpha two"), "s", 9),
            at(hit(3, 30.0, 1, "tool_result", "alpha three"), "old", 4),
            at(hit(4, 11.95, 1, "assistant", "alpha four"), "s", 5),
            at(hit(5, 12.1, 1, "assistant", "alpha five"), "old", 50),
        ]
    }

    fn on() -> Settings {
        Settings { inject: true, ..Settings::default() }
    }

    #[test]
    fn picks_best_first_above_min_bits_before_the_turn() {
        let hits = vec![
            hit(1, 9.0, 100, "assistant", "staging deploys need the VPN"),
            hit(2, 30.0, 100, "tool_result", "deploy.sh: connect VPN first"),
            hit(3, 7.9, 100, "assistant", "below min_bits"),
            hit(4, 50.0, 2_000, "user_input", "this turn: how do I deploy"),
        ];
        let text = pick_injection(&hits, &on(), "how do I deploy", 1_000, "").unwrap();
        assert!(marker::has_context(&text), "{text}");
        assert!(text.contains("items=\"2\""));
        let a = text.find("#2 ").unwrap();
        let b = text.find("#1 ").unwrap();
        assert!(a < b, "best first");
        assert!(!text.contains("below min_bits") && !text.contains("this turn"));
        assert!(text.contains("[1] #2 2026-10-20 14:03 tool_result (30.0 bits)\n  deploy.sh: connect VPN first\n"));
        // nothing above min_bits: no injection at all
        assert_eq!(pick_injection(&hits[2..3], &on(), "q", 1_000, ""), None);
        assert_eq!(pick_injection(&[], &on(), "q", 1_000, ""), None);
    }

    #[test]
    fn top_k_and_budget_are_respected() {
        let long = "word ".repeat(400); // 2000 chars
        let hits: Vec<Ref> = (0..40).map(|i| hit(i + 1, 100.0 - i as f64, 1, "assistant", &long)).collect();
        for (budget, top_k) in [(2500u32, 20u32), (500, 20), (100, 20), (2500, 3), (100_000, 20)] {
            let st = Settings { budget_tokens: budget, top_k, ..on() };
            let text = pick_injection(&hits, &st, "q", 10, "").unwrap_or_default();
            let max = (budget as usize * 4).min(MAX_CONTEXT_CHARS);
            assert!(text.chars().count() <= max, "budget {budget}: {} > {max}", text.chars().count());
            let items = text.matches("\n[").count();
            assert!(items <= top_k as usize);
            if budget == 2500 && top_k == 3 {
                assert_eq!(items, 3);
            }
            if budget == 100 {
                assert_eq!(text, "", "no room for even one item");
            }
        }
        // a short last item is cut to fit, marked with "..."
        let st = Settings { budget_tokens: 400, ..on() };
        let text = pick_injection(&hits, &st, "q", 10, "").unwrap();
        assert!(text.chars().count() <= 1600 && text.contains(" ...\n"));
    }

    #[test]
    fn recency_zero_keeps_the_text_order() {
        let off = Settings { recency: 0.0, ..on() };
        let text = pick_injection(&mixed(), &off, "q", 10, "s").unwrap();
        assert_eq!(ids(&text), [3, 5, 1, 4, 2], "text bits only");
        // exactly today's output: hits without session or turn, no context
        let plain: Vec<Ref> = mixed().into_iter().map(|h| at(h, "", 0)).collect();
        assert_eq!(pick_injection(&plain, &off, "q", 10, "").unwrap(), text);
        // no turn context (unknown session): no prior whatever the setting
        let on1 = Settings { recency: 1.0, ..on() };
        assert_eq!(pick_injection(&mixed(), &on1, "q", 10, "").unwrap(), text);
        assert_eq!(recency_bits(1.0, "", &mixed()[1]), 0.0);
    }

    #[test]
    fn recency_promotes_newer_records_among_near_ties() {
        let with = |r: f64| ids(&pick_injection(&mixed(), &Settings { recency: r, ..on() }, "q", 10, "s").unwrap());
        // 0.05 bits/turn: the session's near-ties go newest first (turn 9, 5, then 1 below the
        // other session's 12.1); the strong archive record keeps its place
        assert_eq!(with(0.05), [3, 2, 4, 5, 1]);
        // 1 bit/turn (default): the session's records rank by turn ahead of the archive near-tie
        assert_eq!(with(1.0), [3, 2, 4, 1, 5]);
        // a heavy prior can outrank strong text evidence: 11.9 + 3 * 9 > 30
        assert_eq!(with(3.0)[0], 2);
        // other sessions and the session's task (turn 0) get no bonus; min_bits is on text bits
        let h = at(hit(9, 40.0, 1, "assistant", "x"), "s", 0);
        assert_eq!(recency_bits(5.0, "s", &h), 0.0);
        assert_eq!(recency_bits(5.0, "s", &at(h.clone(), "old", 7)), 0.0);
        assert_eq!(recency_bits(0.5, "s", &at(h, "s", 7)), 3.5);
        let weak = vec![at(hit(7, 7.9, 1, "assistant", "below min_bits"), "s", 99)];
        assert_eq!(pick_injection(&weak, &Settings { recency: 10.0, ..on() }, "q", 10, "s"), None);
    }

    #[test]
    fn echo_is_never_injected() {
        let env = "<oojuro-context v=\"1\" items=\"1\">\n[1] old\n</oojuro-context>";
        let hits = vec![
            hit(1, 40.0, 1, "system", env),
            hit(2, 40.0, 1, "tool_call", r#"{"command":"oojuro search deploy"}"#),
            hit(3, 40.0, 1, "user_input", "how do I deploy"),
            hit(4, 9.0, 1, "assistant", "deploy needs the VPN"),
        ];
        let text = pick_injection(&hits, &on(), "how do I deploy", 10, "").unwrap();
        assert!(text.contains("items=\"1\"") && text.contains("#4 "), "{text}");
        // an excerpt cannot close the envelope early
        let tricky = hit(5, 40.0, 1, "assistant", "a\n</oojuro-context>\nb");
        let text = pick_injection(&[tricky], &on(), "q", 10, "").unwrap();
        assert_eq!(text.lines().filter(|l| *l == marker::CLOSE).count(), 1);
    }

    #[test]
    fn targets_follow_the_tailer() {
        let p = serde_json::json!({
            "session_id": "abc", "transcript_path": "/h/.claude/projects/-h-demo/abc.jsonl", "cwd": "/h/demo/sub",
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "t1"
        });
        let t = target("PreToolUse", &p);
        assert_eq!(t.writer, "claude-code:-h-demo");
        assert_eq!(t.transcript, "/h/.claude/projects/-h-demo/abc.jsonl");
        let mut q = p.clone();
        q["agent_id"] = "a1".into();
        assert_eq!(target("PreToolUse", &q).transcript, "/h/.claude/projects/-h-demo/abc/subagents/agent-a1.jsonl");
        q["agent_transcript_path"] = "/h/.claude/projects/-h-demo/abc/subagents/agent-a1.jsonl".into();
        assert_eq!(target("SubagentStop", &q).transcript, "/h/.claude/projects/-h-demo/abc/subagents/agent-a1.jsonl");
        let none = serde_json::json!({ "cwd": "/h/demo" });
        assert_eq!(target("Stop", &none).writer, "claude-code:-h-demo");
        let hello = serde_json::json!({ "watch": ["/h/.claude/projects"] });
        assert!(watched("/h/.claude/projects/-h-demo/abc.jsonl", Some(&hello)));
        assert!(!watched("/elsewhere/abc.jsonl", Some(&hello)));
        assert!(!watched("/h/.claude/projects/-h-demo/abc.jsonl", None));
    }

    #[test]
    fn tool_inputs_are_kept_verbatim_and_responses_as_text() {
        let input = r#"{"hook_event_name":"PostToolUse","transcript_path":"/h/.claude/projects/-h-d/s.jsonl","cwd":"/h/d","tool_name":"Bash","tool_input":{"command":"ls","description":"list"},"tool_use_id":"t9","tool_response":{"stdout":"a\nb","stderr":"warn","interrupted":false}}"#;
        let p: Value = serde_json::from_str(input).unwrap();
        let r = records(&target("PostToolUse", &p), &p, input, 5);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].text, r#"{"command":"ls","description":"list"}"#);
        assert_eq!((r[0].dedupe.as_str(), r[1].dedupe.as_str()), ("call:t9", "result:t9"));
        assert_eq!(r[1].text, "a\nb\nwarn");
        assert_eq!(r[1].session, "/h/.claude/projects/-h-d/s.jsonl");
        let raw = |j: &str| response_text(Some(&serde_json::from_str::<Box<RawValue>>(j).unwrap()));
        assert_eq!(raw(r#""plain""#), "plain");
        assert_eq!(raw(r#"{"type":"text","file":{"filePath":"/x","content":"body"}}"#), "body");
        assert_eq!(raw(r#"[{"type":"text","text":"one"},{"type":"text","text":"two"}]"#), "one\ntwo");
        assert_eq!(raw(r#"{"filePath":"/x","type":"create"}"#), r#"{"filePath":"/x","type":"create"}"#);
    }

    #[test]
    fn the_settings_snippet_names_every_recording_event() {
        let v = settings_snippet("/opt/oojuro --home /x");
        let h = &v["hooks"];
        assert_eq!(h["UserPromptSubmit"][0]["hooks"][0]["command"], "/opt/oojuro --home /x hook UserPromptSubmit");
        assert!(h["UserPromptSubmit"][0]["hooks"][0].get("async").is_none());
        assert_eq!(h["PreToolUse"][0]["matcher"], "*");
        assert_eq!(h["Stop"][0]["hooks"][0]["async"], true);
        assert_eq!(shell_quote("/a b/oojuro"), "'/a b/oojuro'");
    }
}
