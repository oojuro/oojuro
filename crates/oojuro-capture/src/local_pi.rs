//! Pi session v3 capture. No session discovery, side-file reads or active-branch reconstruction.
//! Shapes: https://pi.dev/docs/latest/session-format and /message-types (2026-10-22).
use super::{Error, Result};
use crate::{Emitter, FileCtx, ParseCounts, Rec, kind, marker, redact, status};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

pub(super) fn new_ctx(path: &Path) -> FileCtx {
    let project = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    FileCtx {
        writer: format!("pi:{project}"),
        session: crate::hash64(&path.to_string_lossy()),
        inherit_below: -1,
        ..FileCtx::default()
    }
}
fn invalid() -> Error {
    Error::Invalid("invalid pi v3 session entry")
}
fn id(value: &Value) -> Result<&str> {
    let s = value.as_str().ok_or_else(invalid)?;
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(s)
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn stamp(value: &Value, fallback: i64) -> Result<i64> {
    match value.get("timestamp") {
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|v| v.checked_mul(1000))
            .ok_or_else(invalid),
        Some(Value::String(s)) => crate::time::parse_rfc3339_us(s).ok_or_else(invalid),
        Some(_) => Err(invalid()),
        None => Ok(fallback),
    }
}
fn content(value: &Value, pc: &mut ParseCounts) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match string(block, "type") {
                    "text" => parts.push(block["text"].as_str().ok_or_else(invalid)?.into()),
                    "image" => parts.push(crate::image_placeholder(
                        string(block, "mimeType"),
                        string(block, "data"),
                        pc,
                    )),
                    _ => pc.noise("pi.unknown_content_block", block.to_string().len()),
                }
            }
            Ok(parts.join("\n"))
        }
        Value::Null => Ok(String::new()),
        _ => Err(invalid()),
    }
}
fn meta(entry: &Value) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("adapter", "pi-session-v3".into()),
        ("entry_id", string(entry, "id").into()),
    ];
    if let Some(parent) = entry["parentId"].as_str() {
        fields.push(("parent_entry_id", parent.into()));
    }
    fields
}
/// Unknown entry/message types remain in the encrypted raw source. No invented parsed semantics.
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_line(
    ctx: &mut FileCtx,
    line: &[u8],
    offset: u64,
    red: &redact::Redactor,
    rc: &mut redact::Counts,
    pc: &mut ParseCounts,
    out: &mut Vec<Rec>,
) -> Result<()> {
    let entry: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => {
            pc.bad_lines += 1;
            pc.noise("pi.bad_json", line.len());
            return Ok(());
        }
    };
    let ty = string(&entry, "type");
    pc.entry(&format!("pi:{ty}"));
    if ty == "session" {
        if ctx.metas != 0 || entry["version"].as_u64() != Some(3) {
            return Err(invalid());
        }
        id(&entry["id"])?;
        let cwd = entry["cwd"].as_str().ok_or_else(invalid)?;
        if cwd.len() > 4096 || cwd.chars().any(char::is_control) {
            return Err(invalid());
        }
        ctx.cwd = cwd.into();
        ctx.last_ts = stamp(&entry, 0)?;
        ctx.user_from = id(&entry["id"])?.into();
        ctx.metas = 1;
        return Ok(());
    }
    if ctx.metas != 1 {
        return Err(Error::Invalid("pi v3 session header required"));
    }
    id(&entry["id"])?;
    if !entry["parentId"].is_null() {
        id(&entry["parentId"])?;
    }
    let ts = stamp(&entry, ctx.last_ts)?;
    ctx.last_ts = ts;
    let fields = meta(&entry);
    if ty == "message" {
        let msg = &entry["message"];
        let ts = stamp(msg, ts)?;
        message(ctx, &entry, msg, ts, offset, red, rc, pc, out)?;
    } else if matches!(
        ty,
        "compaction" | "branch_summary" | "custom_message" | "context_edit"
    ) {
        let mut fields = fields;
        fields.push((
            "sub",
            if ty == "compaction" {
                crate::context::COMPACTION
            } else {
                ty
            }
            .into(),
        ));
        // Preserve the actual event, including system checkpoint/context edits; do not rewrite history.
        let text = match ty {
            "custom_message" => content(&entry["content"], pc)?,
            _ => entry.to_string(),
        };
        let mut e = Emitter {
            red,
            rc,
            pc,
            out,
            line: offset,
            k: 0,
        };
        e.emit(ctx, ts, kind::SYSTEM, "", &text, fields, status::NONE, "");
    } else {
        pc.noise("pi.metadata_or_unknown", line.len());
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn message(
    ctx: &mut FileCtx,
    entry: &Value,
    msg: &Value,
    ts: i64,
    offset: u64,
    red: &redact::Redactor,
    rc: &mut redact::Counts,
    pc: &mut ParseCounts,
    out: &mut Vec<Rec>,
) -> Result<()> {
    let role = string(msg, "role");
    let mut fields = meta(entry);
    fields.push(("role", role.into()));
    if role == "assistant" {
        let blocks = msg["content"].as_array().ok_or_else(invalid)?;
        let mut calls: BTreeSet<String> = ctx.calls.iter().map(|c| c.0.clone()).collect();
        let mut lookups = ctx.lookups.clone();
        // Check the whole message before emission; never evict unresolved policy state.
        for block in blocks {
            if string(block, "type") == "toolCall" {
                let call = id(&block["id"])?;
                let tool = id(&block["name"])?;
                if !block["arguments"].is_object()
                    || calls.len() >= FileCtx::MAX_OPEN_CALLS
                    || !calls.insert(call.into())
                {
                    return Err(Error::Invalid("pi tool call identity or capacity"));
                }
                if marker::is_oojuro_call(
                    tool,
                    string(block, "namespace"),
                    &block["arguments"].to_string(),
                ) {
                    if lookups.len() >= FileCtx::MAX_OPEN_CALLS {
                        return Err(Error::Invalid("pi lookup capacity"));
                    }
                    lookups.push(call.into());
                }
            }
        }
        let mut e = Emitter {
            red,
            rc,
            pc,
            out,
            line: offset,
            k: 0,
        };
        for block in blocks {
            let mut fields = fields.clone();
            match string(block, "type") {
                "text" => e.emit(
                    ctx,
                    ts,
                    kind::ASSISTANT,
                    "",
                    block["text"].as_str().ok_or_else(invalid)?,
                    fields,
                    status::NONE,
                    "",
                ),
                "thinking" => e.emit(
                    ctx,
                    ts,
                    kind::THINKING,
                    "",
                    string(block, "thinking"),
                    fields,
                    status::NONE,
                    "",
                ),
                "toolCall" => {
                    let call = id(&block["id"])?;
                    let tool = id(&block["name"])?;
                    let args = block["arguments"].to_string();
                    if marker::is_oojuro_call(tool, string(block, "namespace"), &args) {
                        marker::mark(&mut fields, marker::CALL);
                        ctx.open_lookup(call);
                    }
                    e.emit(
                        ctx,
                        ts,
                        kind::TOOL_CALL,
                        tool,
                        &args,
                        fields,
                        status::NONE,
                        call,
                    );
                }
                _ => {
                    e.pc.noise("pi.unknown_assistant_block", block.to_string().len());
                }
            }
        }
    } else {
        let (kind, tool, text, state, call) = match role {
            "user" => (
                kind::USER_INPUT,
                String::new(),
                content(&msg["content"], pc)?,
                status::NONE,
                String::new(),
            ),
            "system" => (
                kind::SYSTEM,
                String::new(),
                msg.to_string(),
                status::NONE,
                String::new(),
            ),
            "custom" | "hookMessage" => (
                kind::SYSTEM,
                String::new(),
                content(&msg["content"], pc)?,
                status::NONE,
                String::new(),
            ),
            "compactionSummary" | "branchSummary" => {
                fields.push((
                    "sub",
                    if role == "compactionSummary" {
                        crate::context::COMPACTION
                    } else {
                        "branch_summary"
                    }
                    .into(),
                ));
                (
                    kind::SYSTEM,
                    String::new(),
                    msg["summary"].as_str().ok_or_else(invalid)?.into(),
                    status::NONE,
                    String::new(),
                )
            }
            "toolResult" => {
                let call = id(&msg["toolCallId"])?;
                let (tool, _) = ctx
                    .calls
                    .iter()
                    .find(|c| c.0 == call)
                    .map(|c| (c.1.clone(), c.2))
                    .ok_or(Error::Invalid("pi tool result has no retained call"))?;
                if string(msg, "toolName") != tool {
                    return Err(Error::Invalid("pi tool result name differs from call"));
                }
                let text = content(&msg["content"], pc)?;
                if ctx.close_lookup(call) {
                    marker::mark(&mut fields, marker::RESULT);
                }
                (
                    kind::TOOL_RESULT,
                    tool,
                    if text.is_empty() {
                        "[empty tool result]".into()
                    } else {
                        text
                    },
                    if msg["isError"].as_bool().ok_or_else(invalid)? {
                        status::ERROR
                    } else {
                        status::OK
                    },
                    call.into(),
                )
            }
            "bashExecution" => {
                let command = msg["command"].as_str().ok_or_else(invalid)?;
                let output = msg["output"].as_str().ok_or_else(invalid)?;
                let cancelled = msg["cancelled"].as_bool().ok_or_else(invalid)?;
                let truncated = msg["truncated"].as_bool().ok_or_else(invalid)?;
                let exit = match msg.get("exitCode") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_i64()
                            .and_then(|n| i32::try_from(n).ok())
                            .ok_or_else(invalid)?,
                    ),
                };
                if let Some(exit) = exit {
                    fields.push(("exit_code", exit.to_string()));
                }
                fields.push(("sub", "bash_execution".into()));
                if marker::is_oojuro_call("bash", "", &json!({"command":command}).to_string()) {
                    marker::mark(&mut fields, marker::RESULT);
                }
                (
                    kind::TOOL_RESULT,
                    "bash".into(),
                    json!({"command":command,"output":output,"cancelled":cancelled,
                    "truncated":truncated,"excludeFromContext":msg["excludeFromContext"]})
                    .to_string(),
                    if exit.is_some_and(|n| n != 0) || cancelled {
                        status::ERROR
                    } else {
                        status::OK
                    },
                    String::new(),
                )
            }
            _ => {
                pc.noise("pi.unknown_role", msg.to_string().len());
                return Ok(());
            }
        };
        let mut e = Emitter {
            red,
            rc,
            pc,
            out,
            line: offset,
            k: 0,
        };
        e.emit(ctx, ts, kind, &tool, &text, fields, state, &call);
    }
    Ok(())
}
