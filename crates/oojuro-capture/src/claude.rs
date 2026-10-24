//! Claude Code transcripts: `~/.claude/projects/<project>/<session>.jsonl`
//! and subagent sidechains `<project>/<session>/subagents/agent-*.jsonl`.
//!
//! Every line is one JSON entry. Content entries:
//! - `assistant`: one content block per entry (`text`, `thinking`, `tool_use`).
//! - `user`: a prompt (string or text/image blocks) or `tool_result` blocks.
//!   `origin.kind` says who sent a prompt (`human`, `peer`, `coordinator`,
//!   `task-notification`); `isMeta` / tag-wrapped strings are harness context.
//! - `attachment`: context the harness injects; `rendered` holds exactly what
//!   entered the model's context. Attachments without `rendered` never reached
//!   the model (hook bookkeeping) and are noise, except `prompt_snapshot`
//!   (the system prompt and tool list the model saw).
//! - `system`: harness messages with `content` (compaction boundaries, ...).
//!
//! Everything else (`queue-operation`, `last-prompt`, titles, file-history
//! snapshots, ...) is metadata noise: counted, not stored. Unknown entry types
//! are counted and skipped; malformed lines are counted and skipped.
use crate::{Emitter, FileCtx, ParseCounts, Rec, image_placeholder, kind, leading_tag, marker, redact, status};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Known metadata-only entry types (noise).
const NOISE_TYPES: &[&str] = &[
    "queue-operation",
    "last-prompt",
    "atis-latch",
    "bridge-session",
    "custom-title",
    "agent-name",
    "pr-link",
    "mode",
    "file-history-snapshot",
    "file-history-delta",
    "cost-state",
    "summary",
    "progress",
    "tag",
];

/// Where Claude Code writes its transcripts (under the home directory).
pub const TRANSCRIPT_ROOT: &str = ".claude/projects";

/// How Claude Code shows itself in its model API calls: user agent
/// `claude-cli/<version> (external, cli|sdk-ts|...)` and `x-app: cli`.
pub const WIRE: crate::WireClient = crate::WireClient { user_agents: &["claude-cli"], headers: &[("x-app", "cli")] };

/// Prompt tags a person types (slash commands, `!` bash mode).
pub const HUMAN_TAGS: &[&str] = &["command-name", "command-message", "command-args", "bash-input"];

/// The project a transcript belongs to: the folder right under `projects/`
/// (Claude Code's own encoding of the launch directory).
pub fn project_of(path: &Path) -> String {
    let comps: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if let Some(i) = comps.iter().rposition(|c| c == "projects")
        && i + 2 < comps.len()
    {
        return comps[i + 1].clone();
    }
    // <project>/<session>/subagents/agent-x.jsonl or <project>/<session>.jsonl
    let n = comps.len();
    if n >= 4 && comps[n - 2] == "subagents" {
        return comps[n - 4].clone();
    }
    if n >= 2 { comps[n - 2].clone() } else { "unknown".into() }
}

/// The transcript of the session that spawned a subagent:
/// `<project>/<session>/subagents/agent-<id>.jsonl` -> `<project>/<session>.jsonl`.
pub fn parent_transcript(path: &Path) -> Option<PathBuf> {
    let sub = path.parent()?;
    if sub.file_name()? != "subagents" {
        return None;
    }
    let session_dir = sub.parent()?;
    let name = session_dir.file_name()?.to_str()?;
    Some(session_dir.parent()?.join(format!("{name}.jsonl")))
}

/// The session id of the session that spawned the subagent writing
/// `path` (0 for a top-level transcript): the hash of its transcript path,
/// as [`new_ctx`] gives it.
pub fn parent_session(path: &Path) -> u64 {
    parent_transcript(path).map_or(0, |p| crate::hash64(&p.to_string_lossy()))
}

/// Context for a new Claude Code transcript.
pub fn new_ctx(path: &Path) -> FileCtx {
    FileCtx {
        writer: format!("claude-code:{}", project_of(path)),
        session: crate::hash64(&path.to_string_lossy()),
        parent_session: parent_session(path),
        sidechain: path.components().any(|c| c.as_os_str() == "subagents"),
        inherit_below: -1,
        results_root: Some(project_dir(path)),
        ..FileCtx::default()
    }
}

/// The project folder of a transcript (`<project>/<session>.jsonl` or
/// `<project>/<session>/subagents/agent-*.jsonl`).
pub fn project_dir(path: &Path) -> PathBuf {
    let up = if path.parent().and_then(Path::file_name).is_some_and(|n| n == "subagents") { 3 } else { 1 };
    path.ancestors().nth(up).unwrap_or(Path::new("")).to_path_buf()
}

/// Largest saved tool output read back in full; longer ones are cut.
pub const MAX_PERSISTED: u64 = 8 << 20;
const PERSISTED_STUB: &str = "<persisted-output>\n";
const SAVED_TO: &str = "Full output saved to: ";

/// Claude Code replaces an oversized tool result with a `<persisted-output>`
/// stub (size, path and a 2 KB preview, which is all the model saw) and saves
/// the output to `<project>/<session>/tool-results/<id>.txt`. For such a stub,
/// the full output (with `persisted=full`), or the stub as it is when the file
/// is gone or outside this project's `tool-results/` (`persisted=missing`).
fn persisted_output(ctx: &FileCtx, text: &str, pc: &mut ParseCounts) -> Option<(String, &'static str)> {
    let first = text.strip_prefix(PERSISTED_STUB)?.lines().next()?;
    let path = first.split_once(SAVED_TO)?.1.trim();
    // Native main-file capture has no authorization for secondary files. Keep the exact stub.
    if ctx.results_root.is_none() {
        pc.persisted.1 += 1;
        return Some((text.to_string(), "unread"));
    }
    match read_saved(ctx, path) {
        Some(full) => {
            pc.persisted.0 += 1;
            pc.persisted.2 += full.len() as u64;
            Some((full, "full"))
        }
        None => {
            pc.persisted.1 += 1;
            Some((text.to_string(), "missing"))
        }
    }
}

/// Read a saved tool output, only from `<project>/<session>/tool-results/*.txt`
/// of this transcript's project (symlinks resolved before the check).
fn read_saved(ctx: &FileCtx, path: &str) -> Option<String> {
    let root = std::fs::canonicalize(ctx.results_root.as_ref()?).ok()?;
    let file = std::fs::canonicalize(path).ok()?;
    let dir = file.parent()?;
    let inside = file.extension()? == "txt"
        && dir.file_name()? == "tool-results"
        && dir.parent()?.parent()? == root
        && std::fs::metadata(&file).ok()?.is_file();
    if !inside {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&file).ok()?.take(MAX_PERSISTED + 1).read_to_end(&mut bytes).ok()?;
    let cut = bytes.len() as u64 > MAX_PERSISTED;
    bytes.truncate(MAX_PERSISTED as usize);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        text.push_str(&format!("\n[cut at {MAX_PERSISTED} bytes]"));
    }
    Some(text)
}

#[derive(Deserialize)]
struct RawLine<'a> {
    #[serde(borrow)]
    message: RawMsg<'a>,
}
#[derive(Deserialize)]
struct RawMsg<'a> {
    #[serde(borrow)]
    content: Vec<RawBlock<'a>>,
}
#[derive(Deserialize)]
struct RawBlock<'a> {
    #[serde(borrow, default)]
    input: Option<&'a RawValue>,
}

fn s<'v>(v: &'v Value, k: &str) -> &'v str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Text of a tool result / prompt content: a string, or text blocks joined by
/// newlines with images replaced by placeholders and other blocks as JSON.
pub fn content_text(c: &Value, pc: &mut ParseCounts) -> String {
    match c {
        Value::String(x) => x.clone(),
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for b in items {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => parts.push(s(b, "text").to_string()),
                    Some("image") => {
                        let src = b.get("source").unwrap_or(&Value::Null);
                        parts.push(image_placeholder(s(src, "media_type"), s(src, "data"), pc));
                    }
                    _ => parts.push(b.to_string()),
                }
            }
            parts.join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse one complete line (without its newline) of a Claude Code transcript.
pub fn parse_line(
    ctx: &mut FileCtx,
    line: &[u8],
    line_off: u64,
    red: &redact::Redactor,
    rc: &mut redact::Counts,
    pc: &mut ParseCounts,
    out: &mut Vec<Rec>,
) {
    let e: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => {
            pc.bad_lines += 1;
            pc.noise("claude.bad_json", line.len());
            return;
        }
    };
    let Some(obj) = e.as_object() else {
        pc.unknown("claude.<non-object>");
        pc.noise("claude.unknown", line.len());
        return;
    };
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("<none>");
    pc.entry(&format!("claude:{ty}"));
    if let Some(cwd) = obj.get("cwd").and_then(Value::as_str)
        && !cwd.is_empty()
        && cwd != ctx.cwd
    {
        ctx.cwd = cwd.to_string();
    }
    let ts = obj.get("timestamp").and_then(Value::as_str).and_then(crate::time::parse_rfc3339_us);
    if let Some(t) = ts {
        ctx.last_ts = t;
    }
    let ts = ts.unwrap_or(ctx.last_ts);
    let mut em = Emitter { red, rc, pc, out, line: line_off, k: 0 };
    match ty {
        "assistant" => assistant(ctx, &e, line, ts, &mut em),
        "user" => user(ctx, &e, ts, &mut em),
        "attachment" => attachment(ctx, &e, line.len(), ts, &mut em),
        "system" => {
            let sub = s(&e, "subtype");
            match e.get("content").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => {
                    em.emit(ctx, ts, kind::SYSTEM, "", c, vec![("sub", sub.to_string())], status::NONE, "")
                }
                _ => em.pc.noise(&format!("claude.system/{sub}"), line.len()),
            }
        }
        t if NOISE_TYPES.contains(&t) => em.pc.noise(&format!("claude.{t}"), line.len()),
        t => {
            em.pc.unknown(&format!("claude.{t}"));
            em.pc.noise("claude.unknown", line.len());
        }
    }
}

fn assistant(ctx: &mut FileCtx, e: &Value, line: &[u8], ts: i64, em: &mut Emitter) {
    let msg = e.get("message").unwrap_or(&Value::Null);
    let model = s(msg, "model").to_string();
    let meta = || if model.is_empty() { vec![] } else { vec![("model", model.clone())] };
    let api_error = e.get("isApiErrorMessage").and_then(Value::as_bool).unwrap_or(false);
    let content = msg.get("content").unwrap_or(&Value::Null);
    let blocks = match content {
        Value::String(t) => {
            let k = if api_error { kind::SYSTEM } else { kind::ASSISTANT };
            em.emit(ctx, ts, k, "", t, meta(), status::NONE, "");
            return;
        }
        Value::Array(b) => b,
        _ => {
            em.pc.unknown("claude.assistant.content");
            return;
        }
    };
    // Tool inputs verbatim (key order and all), from a second, borrowed parse.
    let raw_inputs: Option<Vec<Option<String>>> = blocks
        .iter()
        .any(|b| b.get("input").is_some())
        .then(|| {
            let l = std::str::from_utf8(line).ok()?;
            let r: RawLine = serde_json::from_str(l).ok()?;
            Some(r.message.content.iter().map(|b| b.input.map(|x| x.get().to_string())).collect())
        })
        .flatten();
    for (i, b) in blocks.iter().enumerate() {
        let bt = b.get("type").and_then(Value::as_str).unwrap_or("<none>");
        match bt {
            "text" => {
                let k = if api_error { kind::SYSTEM } else { kind::ASSISTANT };
                let mut m = meta();
                if api_error {
                    m.push(("sub", "api_error".into()));
                }
                em.emit(ctx, ts, k, "", s(b, "text"), m, status::NONE, "");
            }
            "thinking" => {
                let t = s(b, "thinking");
                if t.is_empty() {
                    em.pc.noise("claude.thinking_signature_only", s(b, "signature").len());
                } else {
                    em.emit(ctx, ts, kind::THINKING, "", t, meta(), status::NONE, "");
                }
            }
            "redacted_thinking" => em.pc.noise("claude.redacted_thinking", s(b, "data").len()),
            "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                let input = raw_inputs
                    .as_ref()
                    .and_then(|r| r.get(i).cloned().flatten())
                    .unwrap_or_else(|| b.get("input").map(Value::to_string).unwrap_or_default());
                let mut m = meta();
                if marker::is_oojuro_call(s(b, "name"), s(b, "server_name"), &input) {
                    marker::mark(&mut m, marker::CALL);
                    ctx.open_lookup(s(b, "id"));
                }
                em.emit(ctx, ts, kind::TOOL_CALL, s(b, "name"), &input, m, status::NONE, s(b, "id"));
            }
            t if t.ends_with("_tool_result") => {
                let text = content_text(b.get("content").unwrap_or(&Value::Null), em.pc);
                let mut m = meta();
                if ctx.close_lookup(s(b, "tool_use_id")) {
                    marker::mark(&mut m, marker::RESULT);
                }
                em.emit(ctx, ts, kind::TOOL_RESULT, "", &text, m, status::OK, s(b, "tool_use_id"));
            }
            other => {
                em.pc.unknown(&format!("claude.assistant.block/{other}"));
                em.emit(ctx, ts, kind::SYSTEM, "", &b.to_string(), vec![("sub", other.to_string())], status::NONE, "");
            }
        }
    }
}

/// Who sent a prompt-like user entry, and whether it is a user input or harness context.
fn classify_prompt(ctx: &FileCtx, e: &Value, text: &str) -> (u8, Vec<(&'static str, String)>) {
    let origin = e.get("origin").map(|o| s(o, "kind")).unwrap_or("");
    if e.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        return (kind::SYSTEM, vec![("sub", crate::context::COMPACT_SUMMARY.into())]);
    }
    if e.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return (kind::SYSTEM, vec![("from", "harness".into())]);
    }
    if origin == "task-notification" {
        return (kind::SYSTEM, vec![("from", origin.into())]);
    }
    if let Some(tag) = leading_tag(text)
        && !HUMAN_TAGS.contains(&tag)
    {
        return (kind::SYSTEM, vec![("sub", tag.to_string())]);
    }
    let from = if !origin.is_empty() {
        origin
    } else if ctx.sidechain {
        "parent-agent"
    } else {
        "human"
    };
    (kind::USER_INPUT, vec![("from", from.to_string())])
}

fn user(ctx: &mut FileCtx, e: &Value, ts: i64, em: &mut Emitter) {
    if let Some(r) = e.get("toolUseResult") {
        // the UI's structured copy of the tool result (the model saw the block)
        em.pc.resent("claude.toolUseResult", r.to_string().len());
    }
    let content = e.get("message").and_then(|m| m.get("content")).unwrap_or(&Value::Null);
    match content {
        Value::String(t) => {
            let (k, meta) = classify_prompt(ctx, e, t);
            em.emit(ctx, ts, k, "", t, meta, status::NONE, "");
        }
        Value::Array(blocks) => {
            let mut prompt: Vec<String> = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_result") => {
                        let mut text = content_text(b.get("content").unwrap_or(&Value::Null), em.pc);
                        let mut meta = vec![];
                        if ctx.close_lookup(s(b, "tool_use_id")) {
                            marker::mark(&mut meta, marker::RESULT);
                        }
                        if let Some((full, how)) = persisted_output(ctx, &text, em.pc) {
                            text = full;
                            meta.push(("persisted", how.to_string()));
                        }
                        let st = if b.get("is_error").and_then(Value::as_bool) == Some(true) {
                            status::ERROR
                        } else {
                            status::OK
                        };
                        em.emit(ctx, ts, kind::TOOL_RESULT, "", &text, meta, st, s(b, "tool_use_id"));
                    }
                    Some("text") => prompt.push(s(b, "text").to_string()),
                    Some("image") => {
                        let src = b.get("source").unwrap_or(&Value::Null);
                        prompt.push(image_placeholder(s(src, "media_type"), s(src, "data"), em.pc));
                    }
                    other => {
                        em.pc.unknown(&format!("claude.user.block/{}", other.unwrap_or("<none>")));
                        prompt.push(b.to_string());
                    }
                }
            }
            if !prompt.is_empty() {
                let text = prompt.join("\n");
                let (k, meta) = classify_prompt(ctx, e, &text);
                em.emit(ctx, ts, k, "", &text, meta, status::NONE, "");
            }
        }
        _ => em.pc.unknown("claude.user.content"),
    }
}

fn attachment(ctx: &mut FileCtx, e: &Value, line_len: usize, ts: i64, em: &mut Emitter) {
    let a = e.get("attachment").unwrap_or(&Value::Null);
    let at = a.get("type").and_then(Value::as_str).unwrap_or("<none>").to_string();
    let rendered: Option<String> = e.get("rendered").and_then(Value::as_array).and_then(|items| {
        let parts: Vec<&str> = items
            .iter()
            .filter_map(|x| x.get("content").and_then(Value::as_str).or_else(|| x.as_str()))
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    });
    if at == "queued_command" {
        let text = rendered.or_else(|| match a.get("prompt") {
            Some(Value::String(p)) => Some(p.clone()),
            Some(p @ Value::Array(_)) => Some(content_text(p, em.pc)),
            _ => None,
        });
        let Some(text) = text else {
            em.pc.noise("claude.attachment/queued_command", line_len);
            return;
        };
        let origin = a.get("origin").map(|o| s(o, "kind")).unwrap_or("");
        let from = match (origin, s(a, "commandMode")) {
            ("", "task-notification") => "task-notification",
            ("", _) if a.get("isMeta").and_then(Value::as_bool) == Some(true) => "harness",
            ("", _) if ctx.sidechain => "parent-agent",
            ("", _) => "human",
            (o, _) => o,
        };
        let k = if matches!(from, "task-notification" | "harness") { kind::SYSTEM } else { kind::USER_INPUT };
        em.emit(ctx, ts, k, "", &text, vec![("from", from.to_string()), ("sub", at)], status::NONE, "");
        return;
    }
    if let Some(text) = rendered {
        em.emit(ctx, ts, kind::SYSTEM, "", &text, vec![("sub", at)], status::NONE, "");
        return;
    }
    if at == "prompt_snapshot" {
        // the system prompt and the tool definitions the model saw
        let mut text = String::new();
        if let Some(sp) = a.get("systemPrompt").and_then(Value::as_array) {
            let parts: Vec<String> =
                sp.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect();
            text.push_str(&parts.join("\n\n"));
        }
        if let Some(tools) = a.get("tools").filter(|t| !t.is_null()) {
            text.push_str("\n\n");
            text.push_str(&tools.to_string());
        }
        em.emit(ctx, ts, kind::SYSTEM, "", &text, vec![("sub", "system_prompt".into())], status::NONE, "");
        return;
    }
    // never rendered into the model's context: hook bookkeeping and the like
    em.pc.noise(&format!("claude.attachment/{at}"), line_len);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, lines: &[&str]) -> (FileCtx, Vec<Rec>, ParseCounts, redact::Counts) {
        let red = redact::Redactor::new();
        let mut ctx = new_ctx(Path::new(path));
        let (mut out, mut pc, mut rc) = (Vec::new(), ParseCounts::default(), redact::Counts::new());
        let mut off = 0u64;
        for l in lines {
            parse_line(&mut ctx, l.as_bytes(), off, &red, &mut rc, &mut pc, &mut out);
            off += l.len() as u64 + 1;
        }
        (ctx, out, pc, rc)
    }

    const P: &str = "/h/.claude/projects/-h-work-demo/0f0f0f0f-aaaa-bbbb-cccc-000000000001.jsonl";

    fn stub(path: &Path) -> String {
        let text = format!(
            "<persisted-output>\nOutput too large (9.9KB). Full output saved to: {}\n\nPreview (first 2KB):\nhead\n</persisted-output>",
            path.display()
        );
        let entry = serde_json::json!({"type":"user","timestamp":"2026-01-02T03:04:08Z","message":{"role":"user",
            "content":[{"tool_use_id":"t1","type":"tool_result","content":text}]}});
        entry.to_string()
    }

    #[test]
    fn saved_tool_outputs_are_read_in_full_only_from_the_project() {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("oojuro-persisted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("projects/-h-demo");
        let results = proj.join("sess1/tool-results");
        let other = root.join("projects/-h-other/sess2/tool-results");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(proj.join("sess1/subagents")).unwrap();
        let full = format!("line one\n{}\nthe end, token ghp_{}", "x".repeat(20_000), "A".repeat(36));
        std::fs::write(results.join("b1.txt"), &full).unwrap();
        std::fs::write(other.join("b2.txt"), "someone else's").unwrap();
        std::os::unix::fs::symlink(other.join("b2.txt"), results.join("b3.txt")).unwrap();
        let transcript = proj.join("sess1.jsonl");
        let lines = [
            stub(&results.join("b1.txt")),
            stub(&other.join("b2.txt")),
            stub(&results.join("b3.txt")),
            stub(&results.join("gone.txt")),
            stub(&results.join("../../sess1/tool-results/b1.txt")),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (_, out, pc, _) = run(transcript.to_str().unwrap(), &refs);
        let how: Vec<&str> =
            out.iter().map(|r| r.meta.iter().find(|m| m.0 == "persisted").map_or("", |m| m.1.as_str())).collect();
        assert_eq!(how, ["full", "missing", "missing", "missing", "full"]);
        assert!(out[0].text.starts_with("line one\n") && out[0].text.len() > 20_000);
        assert!(!out[0].text.contains("ghp_AAAA"), "secrets in saved outputs are masked too");
        assert!(out[1].text.starts_with("<persisted-output>") && !out[1].text.contains("someone else"));
        assert!(!out[2].text.contains("someone else"), "a symlink out of the project is refused");
        assert_eq!((pc.persisted.0, pc.persisted.1), (2, 3));
        // a subagent transcript reads its session's outputs too
        let sub = proj.join("sess1/subagents/agent-a1.jsonl");
        assert_eq!(project_dir(&sub), proj);
        let (_, out, _, _) = run(sub.to_str().unwrap(), &[lines[0].as_str()]);
        assert_eq!(out[0].meta, vec![("persisted", "full".to_string())]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn maps_a_synthetic_turn() {
        let lines = [
            r#"{"type":"user","timestamp":"2026-01-02T03:04:05.000Z","sessionId":"s1","message":{"role":"user","content":"please list the files"},"origin":{"kind":"human"}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-02T03:04:06.000Z","message":{"model":"m-1","role":"assistant","content":[{"type":"thinking","thinking":"I should run ls.","signature":"c2ln"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-02T03:04:07.000Z","message":{"model":"m-1","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls","description":"list"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-02T03:04:08.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"a.txt\nb.txt","is_error":false}]},"toolUseResult":{"stdout":"a.txt\nb.txt"}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-02T03:04:09.000Z","message":{"model":"m-1","role":"assistant","content":[{"type":"text","text":"Two files."}]}}"#,
            r#"{"type":"last-prompt","lastPrompt":"please list the files","sessionId":"s1"}"#,
            r#"{"type":"brand-new-entry","x":1}"#,
            r#"{"type":"attachment","timestamp":"2026-01-02T03:04:10.000Z","attachment":{"type":"hook_success","stdout":"ok"}}"#,
            r#"{"type":"attachment","timestamp":"2026-01-02T03:04:11.000Z","attachment":{"type":"date","date":"x"},"rendered":[{"content":"Today is a day."}]}"#,
            r#"{"type":"user","timestamp":"2026-01-02T03:04:12.000Z","message":{"role":"user","content":"<local-command-stdout>done</local-command-stdout>"}}"#,
            r#"{"type":"user","timestamp":"2026-01-02T03:04:13.000Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{not json"#,
        ];
        let (ctx, out, pc, _) = run(P, &lines);
        assert_eq!(ctx.writer, "claude-code:-h-work-demo");
        let kinds: Vec<&str> = out.iter().map(|r| kind::NAMES[r.kind as usize]).collect();
        assert_eq!(
            kinds,
            ["user_input", "thinking", "tool_call", "tool_result", "assistant", "system", "system", "user_input"]
        );
        assert_eq!(out[0].meta, vec![("from", "human".to_string())]);
        assert_eq!(out[0].ts_us, crate::time::parse_rfc3339_us("2026-01-02T03:04:05Z").unwrap());
        assert_eq!(out[2].tool, "Bash");
        assert_eq!(out[2].text, r#"{"command":"ls","description":"list"}"#, "verbatim key order");
        assert_eq!(out[2].call_id, "toolu_1");
        assert_eq!(out[3].call_id, "toolu_1");
        assert_eq!(out[3].status, status::OK);
        assert_eq!(out[3].text, "a.txt\nb.txt");
        assert_eq!(out[6].meta, vec![("sub", "local-command-stdout".to_string())]);
        assert_eq!(out.iter().map(|r| r.seq).collect::<Vec<_>>(), (0..8).collect::<Vec<u32>>());
        assert_eq!(out[7].turn, 2);
        assert_eq!(pc.bad_lines, 1);
        assert_eq!(pc.unknown.get("claude.brand-new-entry"), Some(&1));
        assert_eq!(pc.noise.get("claude.last-prompt").map(|x| x.0), Some(1));
        assert_eq!(pc.noise.get("claude.attachment/hook_success").map(|x| x.0), Some(1));
        assert_eq!(pc.resent.get("claude.toolUseResult").map(|x| x.0), Some(1));
    }

    #[test]
    fn subagent_prompts_come_from_the_parent_and_images_become_placeholders() {
        let p = "/h/.claude/projects/-h-work-demo/sess/subagents/agent-a1.jsonl";
        let lines = [
            r#"{"type":"user","timestamp":"2026-01-02T03:04:05Z","isSidechain":true,"message":{"role":"user","content":"Investigate the parser."}}"#,
            r#"{"type":"user","timestamp":"2026-01-02T03:04:06Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t9","is_error":true,"content":[{"type":"text","text":"boom"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}]}]}}"#,
        ];
        let (ctx, out, pc, _) = run(p, &lines);
        assert_eq!(ctx.writer, "claude-code:-h-work-demo");
        assert!(ctx.sidechain);
        // the parent agent instance: the session whose folder holds this subagent
        let main = "/h/.claude/projects/-h-work-demo/sess.jsonl";
        assert_eq!(parent_transcript(Path::new(p)).unwrap(), Path::new(main));
        assert_eq!(ctx.parent_session, crate::hash64(main));
        assert_eq!(new_ctx(Path::new(main)).session, ctx.parent_session);
        assert_eq!(new_ctx(Path::new(main)).parent_session, 0, "a top-level session has no parent");
        assert!(out.iter().all(|r| r.parent_session == ctx.parent_session && r.session == ctx.session));
        assert_eq!(out[0].meta, vec![("from", "parent-agent".to_string())]);
        assert_eq!(out[1].status, status::ERROR);
        assert!(out[1].text.starts_with("boom\n[image:image/png:8B:"));
        assert_eq!(pc.images, (1, 8));
    }

    #[test]
    fn memory_lookups_and_injected_context_are_marked() {
        let inj = "Earlier records from your journal:\n<oojuro-context v=\"1\" items=\"1\">\n[1] staging deploys need the VPN\n</oojuro-context>";
        let attach = serde_json::json!({"type":"attachment","timestamp":"2026-01-02T03:04:04Z",
            "attachment":{"type":"hook_additional_context","content":[inj]},
            "rendered":[{"content":format!("<system-reminder>\nUserPromptSubmit hook additional context: {inj}\n</system-reminder>")}]})
        .to_string();
        let lines = [
            attach.as_str(),
            r#"{"type":"assistant","timestamp":"2026-01-02T03:04:05Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"oojuro search staging deploys -k 5"}}]}}"#,
            r##"{"type":"user","timestamp":"2026-01-02T03:04:06Z","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"#12 9.1 bits staging deploys need the VPN"}]}}"##,
            r#"{"type":"assistant","timestamp":"2026-01-02T03:04:07Z","message":{"content":[{"type":"tool_use","id":"t2","name":"mcp__oojuro__get","input":{"id":12}}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-02T03:04:08Z","message":{"role":"user","content":[{"tool_use_id":"t2","type":"tool_result","content":[{"type":"text","text":"{\"record\":{}}"}]}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-02T03:04:09Z","message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"grep -rn 'oojuro search' docs"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-02T03:04:10Z","message":{"role":"user","content":[{"tool_use_id":"t3","type":"tool_result","content":"docs/x.md: oojuro search"}]}}"#,
        ];
        let (ctx, out, _, _) = run(P, &lines);
        let marks: Vec<&str> =
            out.iter().map(|r| r.meta.iter().find(|m| m.0 == marker::META).map_or("-", |m| m.1.as_str())).collect();
        assert_eq!(marks, ["context", "call", "result", "call", "result", "-", "-"]);
        assert!(ctx.lookups.is_empty());
        use crate::policy::Decision;
        let indexed: Vec<bool> =
            out.iter().map(|r| Decision::default_for(r) == Decision::Store { indexed: true }).collect();
        assert_eq!(indexed, [false, false, false, false, false, true, true], "marked records are stored, not indexed");
    }

    #[test]
    fn secrets_are_masked_before_records_leave_the_parser() {
        let key = format!("ghp_{}", "aB3".repeat(12));
        let line = format!(
            r#"{{"type":"assistant","timestamp":"2026-01-02T03:04:05Z","message":{{"content":[{{"type":"tool_use","id":"t","name":"Bash","input":{{"command":"git push https://x:{key}@github.com/o/r"}}}}]}}}}"#
        );
        let (_, out, _, rc) = run(P, &[&line]);
        assert!(!out[0].text.contains(&key));
        assert!(out[0].text.contains("[REDACTED:github:"));
        assert_eq!(rc.get("github"), Some(&1));
    }
}
