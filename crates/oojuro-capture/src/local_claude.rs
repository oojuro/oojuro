//! Claude-specific bounds before the legacy parser can evict unresolved lookup state.
use super::{Error, Result};
use crate::{FileCtx, claude, marker};
use serde_json::Value;
use std::path::Path;

pub(super) fn new_ctx(path: &Path) -> FileCtx {
    let mut context = claude::new_ctx(path);
    context.results_root = None;
    context
}

pub(super) fn check_call_state(context: &FileCtx, line: &[u8]) -> Result<()> {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Ok(());
    };
    let Some(blocks) = value["message"]["content"].as_array() else {
        return Ok(());
    };
    let mut context = context.clone();
    for block in blocks {
        let kind = block["type"].as_str().unwrap_or("");
        match value["type"].as_str() {
            Some("assistant")
                if matches!(kind, "tool_use" | "server_tool_use" | "mcp_tool_use") =>
            {
                let id = block["id"].as_str().unwrap_or("");
                if id.is_empty() {
                    return Err(Error::Invalid("tool call ID missing"));
                }
                // The legacy emitter drops a missing input before Policy can see its tool.
                // Refuse the candidate instead of allowing a later result to bypass exclusion.
                if block.get("input").is_none() {
                    return Err(Error::Invalid("tool call input missing"));
                }
                if context.calls.iter().any(|call| call.0 == id)
                    || context.lookups.iter().any(|lookup| lookup == id)
                {
                    return Err(Error::Invalid("duplicate unresolved call ID"));
                }
                let name = block["name"].as_str().unwrap_or("");
                let input = block.get("input").map(Value::to_string).unwrap_or_default();
                if marker::is_oojuro_call(name, block["server_name"].as_str().unwrap_or(""), &input)
                {
                    if context.lookups.len() >= FileCtx::MAX_OPEN_CALLS {
                        return Err(Error::Invalid("outstanding lookup capacity"));
                    }
                    context.open_lookup(id);
                }
                if context.calls.len() >= FileCtx::MAX_OPEN_CALLS {
                    return Err(Error::Invalid("outstanding call capacity"));
                }
                context.open_call(id, name, 1);
            }
            Some("assistant") if kind.ends_with("_tool_result") => close(&mut context, block),
            Some("user") if kind == "tool_result" => close(&mut context, block),
            _ => {}
        }
    }
    Ok(())
}

fn close(context: &mut FileCtx, block: &Value) {
    let id = block["tool_use_id"].as_str().unwrap_or("");
    context.close_lookup(id);
    // Empty results retire a parser lookup marker but emit no result receipt, so the call
    // itself must remain unresolved just as links() will leave it.
    if !claude::content_text(&block["content"], &mut crate::ParseCounts::default()).is_empty() {
        context.close_call(id);
    }
}
