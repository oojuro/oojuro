//! Codex rollouts: `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`.
//!
//! Every line is `{timestamp, type, payload, [ordinal], [metadata]}`:
//! - `session_meta` (first line): cwd (gives the writer), origin of the
//!   session (interactive person, subagent, programmatic caller) and the
//!   system prompt (`base_instructions`). A subagent rollout repeats its
//!   parent's history first; lines below `subagent_history_start_ordinal` are
//!   marked inherited.
//! - `response_item`: what the model saw and produced — `message` (user /
//!   developer / assistant), `reasoning` (summary text; the encrypted blob is
//!   not text), `function_call` / `custom_tool_call` and their outputs,
//!   `agent_message` (a message from another agent).
//! - `compacted` (the compaction summary; the replacement history repeats
//!   earlier records), `world_state` (context state), `realtime_item` (voice
//!   transcript segments).
//! - `event_msg`, `token_usage_record`, `turn_context`,
//!   `inter_agent_communication_metadata`: UI views and counters (noise).
use crate::{
    Emitter, FileCtx, ParseCounts, Rec, encode_dir, image_placeholder, kind, leading_tag, marker, redact, status,
};
use serde_json::Value;
use std::path::Path;

const NOISE_TYPES: &[&str] = &["event_msg", "token_usage_record", "turn_context", "inter_agent_communication_metadata"];

/// Where Codex writes its rollouts (under the home directory).
pub const TRANSCRIPT_ROOT: &str = ".codex/sessions";

/// How Codex shows itself in its model API calls: user agent
/// `codex_cli_rs/<version> (...)` (also `codex-tui`, `codex_exec`,
/// `codex_vscode`) and the `originator` header with the same name.
pub const WIRE: crate::WireClient = crate::WireClient { user_agents: &["codex*"], headers: &[("originator", "codex*")] };

/// Context for a new Codex rollout (the writer is set by `session_meta`).
pub fn new_ctx(path: &Path) -> FileCtx {
    FileCtx {
        writer: "codex:unknown".into(),
        session: crate::hash64(&path.to_string_lossy()),
        user_from: "human".into(),
        inherit_below: -1,
        ..FileCtx::default()
    }
}

fn s<'v>(v: &'v Value, k: &str) -> &'v str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Text of message content / tool output: strings verbatim, text blocks
/// joined, images as placeholders, anything else as JSON.
fn text_of(v: &Value, pc: &mut ParseCounts) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for b in items {
                match b.get("type").and_then(Value::as_str) {
                    Some("input_text" | "output_text" | "text" | "summary_text" | "reasoning_text") => {
                        parts.push(s(b, "text").to_string())
                    }
                    Some("input_image") => {
                        let url = s(b, "image_url");
                        let (media, data) = match url.strip_prefix("data:").and_then(|u| u.split_once(";base64,")) {
                            Some((m, d)) => (m, d),
                            None => ("url", url),
                        };
                        parts.push(image_placeholder(media, data, pc));
                    }
                    Some("encrypted_content") => {
                        pc.noise("codex.encrypted_content", s(b, "encrypted_content").len());
                    }
                    _ => parts.push(b.to_string()),
                }
            }
            parts.join("\n")
        }
        Value::Object(o) => match o.get("content") {
            Some(c @ (Value::String(_) | Value::Array(_))) => text_of(c, pc),
            _ => v.to_string(),
        },
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Who writes the user messages of a session.
fn user_from(meta: &Value) -> &'static str {
    let thread_source = s(meta, "thread_source");
    let source = meta.get("source");
    if thread_source == "subagent" || source.is_some_and(Value::is_object) {
        return "parent-agent";
    }
    let originator = s(meta, "originator");
    let src = source.and_then(Value::as_str).unwrap_or("");
    if originator.starts_with("codex") && matches!(src, "cli" | "vscode" | "") && thread_source != "exec" {
        "human"
    } else {
        "caller"
    }
}

/// Parse one complete line (without its newline) of a Codex rollout.
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
            pc.noise("codex.bad_json", line.len());
            return;
        }
    };
    let Some(obj) = e.as_object() else {
        pc.unknown("codex.<non-object>");
        pc.noise("codex.unknown", line.len());
        return;
    };
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("<none>");
    let pl = obj.get("payload").unwrap_or(&Value::Null);
    let pt = s(pl, "type");
    pc.entry(&if pt.is_empty() { format!("codex:{ty}") } else { format!("codex:{ty}/{pt}") });
    let ts = obj.get("timestamp").and_then(Value::as_str).and_then(crate::time::parse_rfc3339_us);
    if let Some(t) = ts {
        ctx.last_ts = t;
    }
    let ts = ts.unwrap_or(ctx.last_ts);
    let inherited = ctx.inherit_below > 0
        && obj.get("ordinal").and_then(Value::as_i64).is_some_and(|o| o < ctx.inherit_below);
    let mut em = Emitter { red, rc, pc, out, line: line_off, k: 0 };
    let base = |m| with_inh(m, inherited);
    match ty {
        "session_meta" => {
            ctx.metas += 1;
            let bi = pl.get("base_instructions").map(|b| s(b, "text")).unwrap_or("");
            if ctx.metas == 1 {
                let cwd = s(pl, "cwd");
                ctx.writer = format!("codex:{}", if cwd.is_empty() { "unknown".into() } else { encode_dir(cwd) });
                ctx.cwd = cwd.to_string();
                ctx.user_from = user_from(pl).to_string();
                ctx.inherit_below = pl.get("subagent_history_start_ordinal").and_then(Value::as_i64).unwrap_or(-1);
                em.emit(ctx, ts, kind::SYSTEM, "", bi, vec![("sub", "system_prompt".into())], status::NONE, "");
            } else {
                // the parent's meta, repeated inside a subagent rollout
                em.pc.resent("codex.session_meta.repeat", bi.len());
            }
            em.pc.noise("codex.session_meta", line.len().saturating_sub(bi.len()));
        }
        "response_item" => response_item(ctx, pl, pt, ts, inherited, &mut em),
        "compacted" => {
            if let Some(rh) = pl.get("replacement_history") {
                em.pc.resent("codex.compacted.replacement_history", rh.to_string().len());
            }
            em.emit(ctx, ts, kind::SYSTEM, "", s(pl, "message"), base(vec![("sub", crate::context::COMPACTION.into())]), status::NONE, "");
        }
        "world_state" => {
            let st = pl.get("state").map(Value::to_string).unwrap_or_default();
            em.emit(ctx, ts, kind::SYSTEM, "", &st, base(vec![("sub", "world_state".into())]), status::NONE, "");
        }
        "realtime_item" if pt == "transcript_segment" => {
            let (k, m) = if s(pl, "role") == "user" {
                (kind::USER_INPUT, vec![("from", "human".into()), ("sub", "voice".into())])
            } else {
                (kind::ASSISTANT, vec![("sub", "voice".into())])
            };
            em.emit(ctx, ts, k, "", s(pl, "text"), base(m), status::NONE, "");
        }
        "realtime_item" => em.pc.noise(&format!("codex.realtime_item/{pt}"), line.len()),
        t if NOISE_TYPES.contains(&t) => {
            let k = if pt.is_empty() { format!("codex.{t}") } else { format!("codex.{t}/{pt}") };
            em.pc.noise(&k, line.len())
        }
        t => {
            em.pc.unknown(&format!("codex.{t}"));
            em.pc.noise("codex.unknown", line.len());
        }
    }
}

type Meta = Vec<(&'static str, String)>;

/// Mark records of a subagent rollout's inherited parent history.
fn with_inh(mut m: Meta, inherited: bool) -> Meta {
    if inherited {
        m.push(("inh", "1".into()));
    }
    m
}

fn response_item(ctx: &mut FileCtx, pl: &Value, pt: &str, ts: i64, inherited: bool, em: &mut Emitter) {
    let base = |m| with_inh(m, inherited);
    match pt {
        "message" => {
            let text = text_of(pl.get("content").unwrap_or(&Value::Null), em.pc);
            match s(pl, "role") {
                "assistant" => {
                    let phase = s(pl, "phase");
                    let m = if phase.is_empty() { vec![] } else { vec![("sub", phase.to_string())] };
                    em.emit(ctx, ts, kind::ASSISTANT, "", &text, base(m), status::NONE, "");
                }
                "user" => {
                    let (k, m) = match leading_tag(&text) {
                        Some(tag) => (kind::SYSTEM, vec![("sub", tag.to_string())]),
                        None => {
                            let from = if inherited { "inherited".into() } else { ctx.user_from.clone() };
                            (kind::USER_INPUT, vec![("from", from)])
                        }
                    };
                    em.emit(ctx, ts, k, "", &text, base(m), status::NONE, "");
                }
                role => {
                    em.emit(ctx, ts, kind::SYSTEM, "", &text, base(vec![("sub", role.to_string())]), status::NONE, "")
                }
            }
        }
        "reasoning" => {
            let mut text = text_of(pl.get("summary").unwrap_or(&Value::Null), em.pc);
            let content = text_of(pl.get("content").unwrap_or(&Value::Null), em.pc);
            if !content.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&content);
            }
            if text.is_empty() {
                em.pc.noise("codex.reasoning_encrypted_only", s(pl, "encrypted_content").len());
            } else {
                em.emit(ctx, ts, kind::THINKING, "", &text, base(vec![]), status::NONE, "");
            }
        }
        "function_call" | "custom_tool_call" => {
            let body = match pl.get(if pt == "function_call" { "arguments" } else { "input" }) {
                Some(Value::String(x)) => x.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            let ns = s(pl, "namespace");
            let mut m = if ns.is_empty() { vec![] } else { vec![("ns", ns.to_string())] };
            if marker::is_oojuro_call(s(pl, "name"), ns, &body) {
                marker::mark(&mut m, marker::CALL);
                ctx.open_lookup(s(pl, "call_id"));
            }
            em.emit(ctx, ts, kind::TOOL_CALL, s(pl, "name"), &body, base(m), status::NONE, s(pl, "call_id"));
        }
        "function_call_output" | "custom_tool_call_output" => {
            let text = text_of(pl.get("output").unwrap_or(&Value::Null), em.pc);
            let mut m = vec![];
            if ctx.close_lookup(s(pl, "call_id")) {
                marker::mark(&mut m, marker::RESULT);
            }
            em.emit(ctx, ts, kind::TOOL_RESULT, "", &text, base(m), status::NONE, s(pl, "call_id"));
        }
        "local_shell_call" | "web_search_call" | "image_generation_call" | "tool_search_call" => {
            let tool = pt.trim_end_matches("_call");
            let body = pl.get("action").map(Value::to_string).unwrap_or_else(|| pl.to_string());
            let mut m = vec![];
            if marker::is_oojuro_call(tool, "", &body) {
                marker::mark(&mut m, marker::CALL);
                ctx.open_lookup(s(pl, "call_id"));
            }
            em.emit(ctx, ts, kind::TOOL_CALL, tool, &body, base(m), status::NONE, s(pl, "call_id"));
        }
        "agent_message" => {
            // a message from another agent of the same run, delivered to this one
            let text = text_of(pl.get("content").unwrap_or(&Value::Null), em.pc);
            let from = format!("agent:{}", s(pl, "author"));
            em.emit(ctx, ts, kind::USER_INPUT, "", &text, base(vec![("from", from)]), status::NONE, "");
        }
        other => {
            em.pc.unknown(&format!("codex.response_item/{other}"));
            em.emit(ctx, ts, kind::SYSTEM, "", &pl.to_string(), base(vec![("sub", other.to_string())]), status::NONE, "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> (FileCtx, Vec<Rec>, ParseCounts) {
        let red = redact::Redactor::new();
        let mut ctx = new_ctx(Path::new("/h/.codex/sessions/2026/01/02/rollout-2026-01-02T03-04-05-x.jsonl"));
        let (mut out, mut pc, mut rc) = (Vec::new(), ParseCounts::default(), redact::Counts::new());
        let mut off = 0;
        for l in lines {
            parse_line(&mut ctx, l.as_bytes(), off, &red, &mut rc, &mut pc, &mut out);
            off += l.len() as u64 + 1;
        }
        (ctx, out, pc)
    }

    #[test]
    fn maps_a_synthetic_rollout() {
        let lines = [
            r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","payload":{"id":"x","cwd":"/h/work/demo.app","originator":"codex-tui","source":"cli","thread_source":"user","base_instructions":{"text":"You are a coding agent."}}}"#,
            r#"{"timestamp":"2026-01-02T03:04:06.000Z","type":"response_item","ordinal":1,"payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<collaboration_mode>default</collaboration_mode>"}]}}"#,
            r#"{"timestamp":"2026-01-02T03:04:07.000Z","type":"response_item","ordinal":2,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix the build"}]}}"#,
            r#"{"timestamp":"2026-01-02T03:04:08.000Z","type":"response_item","ordinal":3,"payload":{"type":"reasoning","summary":[],"encrypted_content":"QUJD"}}"#,
            r#"{"timestamp":"2026-01-02T03:04:09.000Z","type":"response_item","ordinal":4,"payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Check the compiler output."}],"encrypted_content":"QUJD"}}"#,
            r#"{"timestamp":"2026-01-02T03:04:10.000Z","type":"response_item","ordinal":5,"payload":{"type":"custom_tool_call","call_id":"c1","name":"exec","input":"cargo build"}}"#,
            r#"{"timestamp":"2026-01-02T03:04:11.000Z","type":"response_item","ordinal":6,"payload":{"type":"custom_tool_call_output","call_id":"c1","output":[{"type":"input_text","text":"error[E0425]"}]}}"#,
            r#"{"timestamp":"2026-01-02T03:04:12.000Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
            r#"{"timestamp":"2026-01-02T03:04:13.000Z","type":"response_item","ordinal":7,"payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Fixed."}]}}"#,
            r#"{"timestamp":"2026-01-02T03:04:14.000Z","type":"mystery","payload":{}}"#,
        ];
        let (ctx, out, pc) = run(&lines);
        assert_eq!(ctx.writer, "codex:-h-work-demo-app");
        let kinds: Vec<&str> = out.iter().map(|r| kind::NAMES[r.kind as usize]).collect();
        assert_eq!(kinds, ["system", "system", "user_input", "thinking", "tool_call", "tool_result", "assistant"]);
        assert_eq!(out[2].meta, vec![("from", "human".to_string())]);
        assert_eq!(out[4].tool, "exec");
        assert_eq!(out[4].text, "cargo build");
        assert_eq!(out[5].call_id, "c1");
        assert_eq!(out[6].meta, vec![("sub", "final_answer".to_string())]);
        assert_eq!(pc.noise.get("codex.reasoning_encrypted_only").map(|x| x.0), Some(1));
        assert_eq!(pc.noise.get("codex.event_msg/token_count").map(|x| x.0), Some(1));
        assert_eq!(pc.unknown.get("codex.mystery"), Some(&1));
    }

    #[test]
    fn calls_to_oojuro_and_their_outputs_are_marked() {
        let lines = [
            r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","payload":{"id":"s","cwd":"/h/work/demo","originator":"codex_cli_rs"}}"#,
            r#"{"timestamp":"2026-01-02T03:04:10.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"exec_command","arguments":"{\"cmd\":\"oojuro search parser bug\"}"}}"#,
            r##"{"timestamp":"2026-01-02T03:04:11.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"#3 parser bug fixed"}}"##,
            r#"{"timestamp":"2026-01-02T03:04:12.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c2","name":"search","namespace":"oojuro","arguments":"{}"}}"#,
            r#"{"timestamp":"2026-01-02T03:04:13.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"[]"}}"#,
            r#"{"timestamp":"2026-01-02T03:04:14.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c3","name":"exec_command","arguments":"{\"cmd\":\"cargo build\"}"}}"#,
        ];
        let (_, out, _) = run(&lines);
        let marks: Vec<&str> =
            out.iter().map(|r| r.meta.iter().find(|m| m.0 == marker::META).map_or("-", |m| m.1.as_str())).collect();
        assert_eq!(marks, ["call", "result", "call", "result", "-"]);
    }

    #[test]
    fn subagent_rollouts_mark_inherited_history() {
        let lines = [
            r#"{"timestamp":"2026-01-02T03:04:05Z","type":"session_meta","payload":{"cwd":"/h/w","originator":"codex-tui","source":{"subagent":{}},"thread_source":"subagent","subagent_history_start_ordinal":3,"base_instructions":{"text":"sys"}}}"#,
            r#"{"timestamp":"2026-01-02T03:04:05Z","type":"session_meta","payload":{"cwd":"/h/w","base_instructions":{"text":"sys"}}}"#,
            r#"{"timestamp":"2026-01-02T03:04:06Z","type":"response_item","ordinal":1,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"parent task"}]}}"#,
            r#"{"timestamp":"2026-01-02T03:04:07Z","type":"response_item","ordinal":4,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"your task"}]}}"#,
            r#"{"timestamp":"2026-01-02T03:04:08Z","type":"response_item","ordinal":5,"payload":{"type":"agent_message","author":"/root","recipient":"/root/w1","content":[{"type":"input_text","text":"status?"},{"type":"encrypted_content","encrypted_content":"Zm9v"}]}}"#,
        ];
        let (ctx, out, pc) = run(&lines);
        assert_eq!(ctx.user_from, "parent-agent");
        assert_eq!(out[1].meta, vec![("from", "inherited".to_string()), ("inh", "1".to_string())]);
        assert_eq!(out[2].meta, vec![("from", "parent-agent".to_string())]);
        assert_eq!(out[3].meta, vec![("from", "agent:/root".to_string())]);
        assert_eq!(out[3].text, "status?");
        assert_eq!(pc.resent.get("codex.session_meta.repeat").map(|x| x.0), Some(1));
    }
}
