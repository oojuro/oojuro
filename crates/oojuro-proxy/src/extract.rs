//! What the agent saw and produced, from the bodies of one model call.
//!
//! Requests give the system prompt, the tool definitions and every message
//! of the history (the journal worker keeps only the ones it has not stored
//! before). Responses give the outputs: text, thinking (as far as the
//! provider exposes it as text), tool calls with their input verbatim, and
//! errors; plain JSON or assembled from server-sent events (a stream cut off
//! gives what arrived, marked `partial`).
//!
//! Kinds are the capture layer's (`docs/CAPTURE.md`): `system` (system
//! prompt, tool definitions, developer messages, tag-wrapped harness
//! context such as `<system-reminder>`), `user_input`, `assistant`,
//! `thinking`, `tool_call`, `tool_result`.
use crate::sse;
use oojuro_capture::{
    ParseCounts,
    claude::{HUMAN_TAGS, content_text},
    kind, leading_tag, status,
};
use serde::Deserialize;
use serde_json::{Map, Value, value::RawValue};
use std::collections::BTreeMap;

/// The APIs recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    AnthropicMessages,
    OpenAiChat,
    OpenAiResponses,
}

impl Route {
    pub fn name(self) -> &'static str {
        match self {
            Route::AnthropicMessages => "anthropic.messages",
            Route::OpenAiChat => "openai.chat",
            Route::OpenAiResponses => "openai.responses",
        }
    }

    /// The recorded route of a POST to `path` (query excluded).
    pub fn of(path: &str) -> Option<Route> {
        let p = path.trim_end_matches('/');
        if p.ends_with("/messages") || p == "messages" {
            Some(Route::AnthropicMessages)
        } else if p.ends_with("/chat/completions") {
            Some(Route::OpenAiChat)
        } else if p.ends_with("/responses") {
            Some(Route::OpenAiResponses)
        } else {
            None
        }
    }
}

/// One record as parsed (text verbatim, before redaction).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub kind: u8,
    pub tool: String,
    pub call_id: String,
    pub text: String,
    /// Canonical form for the identity when it differs from the text (a
    /// tool call's input: the model's raw JSON and the client's re-encoding
    /// of it must match).
    pub canon: Option<String>,
    pub status: u8,
    pub meta: Vec<(&'static str, Value)>,
    /// User inputs so far in the conversation (this one included).
    pub turn: u32,
}

#[derive(Debug, Default)]
pub struct Request {
    pub model: String,
    /// Conversation id when the client names one.
    pub session: Option<String>,
    pub items: Vec<Item>,
}

#[derive(Debug, Default)]
pub struct Response {
    pub model: String,
    pub id: String,
    pub items: Vec<Item>,
    /// Token counts (`in`, `out`, `cache_read`, `cache_write`, `reasoning`).
    pub usage: Map<String, Value>,
    pub stop: String,
    /// Errors reported inside a 2xx body (stream `error` events, `response.failed`).
    pub errors: Vec<String>,
    /// The terminal event arrived (streams) or the body parsed whole (JSON).
    pub complete: bool,
}

/// Records in conversation order, numbering turns.
struct Out<'a> {
    items: Vec<Item>,
    turn: u32,
    pc: &'a mut ParseCounts,
}

impl<'a> Out<'a> {
    fn new(pc: &'a mut ParseCounts, turn: u32) -> Self {
        Self { items: Vec::new(), turn, pc }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        k: u8,
        tool: &str,
        call_id: &str,
        text: String,
        canon: Option<String>,
        st: u8,
        meta: Vec<(&'static str, Value)>,
    ) {
        if text.is_empty() {
            return;
        }
        if k == kind::USER_INPUT {
            self.turn += 1;
        }
        self.items.push(Item {
            kind: k,
            tool: tool.to_string(),
            call_id: call_id.to_string(),
            text,
            canon,
            status: st,
            meta,
            turn: self.turn,
        });
    }

    fn simple(&mut self, k: u8, text: String, meta: Vec<(&'static str, Value)>) {
        self.push(k, "", "", text, None, status::NONE, meta);
    }

    /// A user text: tag-wrapped harness context (`<system-reminder>`,
    /// `<environment_context>`, ...) is `system`, the rest `user_input`.
    fn user_text(&mut self, text: &str) {
        match leading_tag(text) {
            Some(tag) if !HUMAN_TAGS.contains(&tag) => {
                let sub = tag.to_string();
                self.simple(kind::SYSTEM, text.to_string(), vec![("sub", sub.into())]);
            }
            _ => self.simple(kind::USER_INPUT, text.to_string(), vec![]),
        }
    }

    fn tool_call(&mut self, tool: &str, call_id: &str, input: String, json: bool) {
        let canon = if json { canon_json(&input) } else { None };
        self.push(kind::TOOL_CALL, tool, call_id, input, canon, status::NONE, vec![]);
    }

    fn tool_result(&mut self, tool: &str, call_id: &str, text: String, st: u8) {
        self.push(kind::TOOL_RESULT, tool, call_id, text, None, st, vec![]);
    }

    fn other(&mut self, what: &str, v: &Value) {
        self.pc.unknown(what);
        self.simple(kind::SYSTEM, v.to_string(), vec![("sub", what.to_string().into())]);
    }
}

fn s<'v>(v: &'v Value, k: &str) -> &'v str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn u(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}

/// Compact JSON with object keys sorted: equal for equal values however
/// they were spaced or ordered.
pub fn canon_json(raw: &str) -> Option<String> {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*k).clone()).to_string());
                    out.push(':');
                    write(&m[k.as_str()], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let v: Value = serde_json::from_str(raw).ok()?;
    let mut out = String::new();
    write(&v, &mut out);
    Some(out)
}

/// `[<what>:<media type>:<bytes>B:<hash8>]` for an inline payload (same
/// shape as the capture layer's image placeholders).
fn payload(what: &str, media: &str, data: &str, pc: &mut ParseCounts) -> String {
    pc.images.0 += 1;
    pc.images.1 += data.len() as u64;
    let h = blake3::hash(data.as_bytes()).to_hex();
    format!("[{what}:{media}:{}B:{}]", data.len(), &h.as_str()[..8])
}

/// An image or document source (Anthropic `source`).
fn source_text(what: &str, src: &Value, pc: &mut ParseCounts) -> String {
    match s(src, "type") {
        "base64" => payload(what, s(src, "media_type"), s(src, "data"), pc),
        "url" => format!("[{what}:url:{}]", s(src, "url")),
        "file" => format!("[{what}:file:{}]", s(src, "file_id")),
        "text" => s(src, "data").to_string(),
        "content" => content_text(src.get("content").unwrap_or(&Value::Null), pc),
        _ => format!("[{what}:{}]", src),
    }
}

/// An image given as a URL (`data:` URLs become a placeholder).
fn url_text(what: &str, url: &str, pc: &mut ParseCounts) -> String {
    if let Some(rest) = url.strip_prefix("data:")
        && let Some((head, data)) = rest.split_once(',')
    {
        let media = head.split(';').next().unwrap_or("");
        return payload(what, media, data, pc);
    }
    format!("[{what}:url:{url}]")
}

#[derive(Deserialize)]
struct RawTop<'a> {
    #[serde(borrow, default)]
    tools: Option<&'a RawValue>,
    #[serde(borrow, default)]
    messages: Option<Vec<&'a RawValue>>,
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct RawContent<'a> {
    #[serde(borrow)]
    content: Vec<RawBlock<'a>>,
}

#[derive(Deserialize)]
struct RawBlock<'a> {
    #[serde(borrow, default)]
    input: Option<&'a RawValue>,
}

/// Tool inputs of a content array, verbatim (key order kept), by block.
fn raw_inputs(content: Option<&RawValue>) -> Vec<Option<String>> {
    let Some(c) = content else { return Vec::new() };
    serde_json::from_str::<Vec<RawBlock>>(c.get())
        .map(|v| v.into_iter().map(|b| b.input.map(|x| x.get().to_string())).collect())
        .unwrap_or_default()
}

fn raw_message_inputs(msg: &RawValue) -> Vec<Option<String>> {
    serde_json::from_str::<RawContent>(msg.get())
        .map(|m| m.content.into_iter().map(|b| b.input.map(|x| x.get().to_string())).collect())
        .unwrap_or_default()
}

fn tools_text(raw: Option<&RawValue>) -> Option<String> {
    let t = raw?.get();
    let empty = matches!(t.trim(), "null" | "[]" | "");
    (!empty).then(|| t.to_string())
}

// ---------------------------------------------------------------- Anthropic

/// Conversation id in Claude Code's `metadata.user_id`
/// (`user_<hash>_account_<uuid>_session_<uuid>`, or JSON with `session_id`).
fn anthropic_session(v: &Value) -> Option<String> {
    let uid = v.get("metadata")?.get("user_id")?.as_str()?;
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(uid)
        && let Some(sid) = m.get("session_id").and_then(Value::as_str)
    {
        return Some(sid.to_string());
    }
    let i = uid.rfind("session_")?;
    let sid = &uid[i + "session_".len()..];
    (sid.len() >= 8).then(|| sid.to_string())
}

fn anthropic_blocks(o: &mut Out, role: &str, content: Option<&Value>, raw: &[Option<String>]) {
    let assistant = role == "assistant";
    let blocks = match content {
        Some(Value::String(t)) => {
            if assistant {
                o.simple(kind::ASSISTANT, t.clone(), vec![]);
            } else {
                o.user_text(t);
            }
            return;
        }
        Some(Value::Array(b)) => b,
        Some(Value::Null) | None => return,
        Some(other) => return o.other("anthropic.content", other),
    };
    for (i, b) in blocks.iter().enumerate() {
        let bt = s(b, "type");
        match bt {
            "text" if assistant => o.simple(kind::ASSISTANT, s(b, "text").to_string(), vec![]),
            "text" => o.user_text(s(b, "text")),
            "thinking" => o.simple(kind::THINKING, s(b, "thinking").to_string(), vec![]),
            "redacted_thinking" => o.pc.noise("anthropic.redacted_thinking", s(b, "data").len()),
            "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                let input = raw
                    .get(i)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| b.get("input").map(Value::to_string).unwrap_or_else(|| "{}".into()));
                o.tool_call(s(b, "name"), s(b, "id"), input, true);
            }
            "tool_result" => {
                let text = content_text(b.get("content").unwrap_or(&Value::Null), o.pc);
                let st =
                    if b.get("is_error").and_then(Value::as_bool) == Some(true) { status::ERROR } else { status::OK };
                o.tool_result("", s(b, "tool_use_id"), text, st);
            }
            t if t.ends_with("_tool_result") => {
                let c = b.get("content").unwrap_or(&Value::Null);
                let text = match c {
                    Value::String(_) | Value::Array(_) => content_text(c, o.pc),
                    other => other.to_string(),
                };
                let err = c.get("type").and_then(Value::as_str).is_some_and(|x| x.contains("error"));
                o.tool_result("", s(b, "tool_use_id"), text, if err { status::ERROR } else { status::OK });
            }
            "image" => {
                let t = source_text("image", b.get("source").unwrap_or(&Value::Null), o.pc);
                o.simple(kind::USER_INPUT, t, vec![]);
            }
            "document" => {
                let t = source_text("document", b.get("source").unwrap_or(&Value::Null), o.pc);
                o.simple(kind::USER_INPUT, t, vec![("sub", "document".into())]);
            }
            "search_result" => {
                let mut t = format!("{}\n{}\n", s(b, "title"), s(b, "source"));
                t.push_str(&content_text(b.get("content").unwrap_or(&Value::Null), o.pc));
                o.simple(kind::USER_INPUT, t, vec![("sub", "search_result".into())]);
            }
            other => o.other(&format!("anthropic.block/{other}"), b),
        }
    }
}

pub fn anthropic_request(body: &[u8], pc: &mut ParseCounts) -> Result<Request, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("request: {e}"))?;
    let raw: RawTop = serde_json::from_slice(body).map_err(|e| format!("request: {e}"))?;
    let mut o = Out::new(pc, 0);
    match v.get("system") {
        Some(Value::String(t)) => o.simple(kind::SYSTEM, t.clone(), vec![("sub", "system_prompt".into())]),
        Some(Value::Array(blocks)) => {
            for b in blocks {
                match b.get("text").and_then(Value::as_str) {
                    Some(t) => o.simple(kind::SYSTEM, t.to_string(), vec![("sub", "system_prompt".into())]),
                    None => o.other("anthropic.system_block", b),
                }
            }
        }
        _ => {}
    }
    if let Some(t) = tools_text(raw.tools) {
        o.simple(kind::SYSTEM, t, vec![("sub", "tools".into())]);
    }
    let raw_msgs = raw.messages.unwrap_or_default();
    for (i, m) in v.get("messages").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let inputs = raw_msgs.get(i).map(|r| raw_message_inputs(r)).unwrap_or_default();
        anthropic_blocks(&mut o, s(m, "role"), m.get("content"), &inputs);
    }
    Ok(Request { model: s(&v, "model").to_string(), session: anthropic_session(&v), items: o.items })
}

fn anthropic_usage(usage: &Value, into: &mut Map<String, Value>) {
    for (from, to) in [
        ("input_tokens", "in"),
        ("output_tokens", "out"),
        ("cache_read_input_tokens", "cache_read"),
        ("cache_creation_input_tokens", "cache_write"),
    ] {
        if let Some(n) = u(usage, from) {
            into.insert(to.into(), n.into());
        }
    }
}

pub fn anthropic_response_json(body: &[u8], turn: u32, pc: &mut ParseCounts) -> Result<Response, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("response: {e}"))?;
    let raw: RawTop = serde_json::from_slice(body).map_err(|e| format!("response: {e}"))?;
    let mut r =
        Response { model: s(&v, "model").into(), id: s(&v, "id").into(), complete: true, ..Response::default() };
    if s(&v, "type") == "error" {
        r.errors.push(v.get("error").unwrap_or(&v).to_string());
    }
    if let Some(us) = v.get("usage") {
        anthropic_usage(us, &mut r.usage);
    }
    r.stop = s(&v, "stop_reason").to_string();
    let mut o = Out::new(pc, turn);
    anthropic_blocks(&mut o, "assistant", v.get("content"), &raw_inputs(raw.content));
    r.items = o.items;
    Ok(r)
}

pub fn anthropic_response_sse(events: &[sse::Event], turn: u32, pc: &mut ParseCounts) -> Response {
    #[derive(Default)]
    struct B {
        start: Value,
        text: String,
        json: String,
        stopped: bool,
    }
    let mut r = Response::default();
    let mut blocks: BTreeMap<u64, B> = BTreeMap::new();
    for e in events {
        let Ok(d) = serde_json::from_str::<Value>(&e.data) else { continue };
        let idx = u(&d, "index").unwrap_or(0);
        match s(&d, "type") {
            "message_start" => {
                let m = d.get("message").unwrap_or(&Value::Null);
                r.model = s(m, "model").into();
                r.id = s(m, "id").into();
                if let Some(us) = m.get("usage") {
                    anthropic_usage(us, &mut r.usage);
                }
            }
            "content_block_start" => {
                let cb = d.get("content_block").cloned().unwrap_or(Value::Null);
                let text = s(&cb, "text").to_string() + s(&cb, "thinking");
                blocks.insert(idx, B { start: cb, text, ..B::default() });
            }
            "content_block_delta" => {
                let b = blocks.entry(idx).or_default();
                let dl = d.get("delta").unwrap_or(&Value::Null);
                match s(dl, "type") {
                    "text_delta" => b.text.push_str(s(dl, "text")),
                    "thinking_delta" => b.text.push_str(s(dl, "thinking")),
                    "input_json_delta" => b.json.push_str(s(dl, "partial_json")),
                    _ => {} // signatures, citations
                }
            }
            "content_block_stop" => blocks.entry(idx).or_default().stopped = true,
            "message_delta" => {
                if let Some(sr) = d.get("delta").and_then(|x| x.get("stop_reason")).and_then(Value::as_str) {
                    r.stop = sr.into();
                }
                if let Some(us) = d.get("usage") {
                    anthropic_usage(us, &mut r.usage);
                }
            }
            "message_stop" => r.complete = true,
            "error" => r.errors.push(d.get("error").unwrap_or(&d).to_string()),
            _ => {} // ping
        }
    }
    let mut o = Out::new(pc, turn);
    for b in blocks.values() {
        let n = o.items.len();
        let bt = s(&b.start, "type");
        match bt {
            "text" => o.simple(kind::ASSISTANT, b.text.clone(), vec![]),
            "thinking" => o.simple(kind::THINKING, b.text.clone(), vec![]),
            "redacted_thinking" => o.pc.noise("anthropic.redacted_thinking", s(&b.start, "data").len()),
            "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                let input = if b.json.is_empty() {
                    b.start.get("input").map(Value::to_string).unwrap_or_else(|| "{}".into())
                } else {
                    b.json.clone()
                };
                o.tool_call(s(&b.start, "name"), s(&b.start, "id"), input, true);
            }
            "" => {}
            _ => anthropic_blocks(&mut o, "assistant", Some(&Value::Array(vec![b.start.clone()])), &[]),
        }
        if !b.stopped {
            for it in &mut o.items[n..] {
                it.meta.push(("partial", true.into()));
            }
        }
    }
    r.items = o.items;
    r
}

// ---------------------------------------------------------------- OpenAI chat

/// Text of chat content: a string, or text parts joined.
fn chat_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| {
                p.get("text")
                    .or_else(|| p.get("refusal"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| p.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// An assistant message (request history or response choice).
fn chat_assistant(o: &mut Out, m: &Value, extra: &[(&'static str, Value)]) {
    let n = o.items.len();
    for k in ["reasoning_content", "reasoning"] {
        if let Some(t) = m.get(k).and_then(Value::as_str) {
            o.simple(kind::THINKING, t.to_string(), vec![]);
            break;
        }
    }
    match m.get("content") {
        Some(Value::Array(parts)) => {
            for p in parts {
                match s(p, "type") {
                    "refusal" => o.simple(kind::ASSISTANT, s(p, "refusal").into(), vec![("sub", "refusal".into())]),
                    _ => o.simple(
                        kind::ASSISTANT,
                        p.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| p.to_string()),
                        vec![],
                    ),
                }
            }
        }
        c => o.simple(kind::ASSISTANT, chat_text(c), vec![]),
    }
    if let Some(t) = m.get("refusal").and_then(Value::as_str) {
        o.simple(kind::ASSISTANT, t.to_string(), vec![("sub", "refusal".into())]);
    }
    for tc in m.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let f = tc.get("function").unwrap_or(&Value::Null);
        let args = match f.get("arguments") {
            Some(Value::String(a)) => a.clone(),
            Some(other) => other.to_string(),
            None => tc.get("custom").map(|c| s(c, "input").to_string()).unwrap_or_default(),
        };
        let name = if f.is_null() { tc.get("custom").map(|c| s(c, "name")).unwrap_or("") } else { s(f, "name") };
        o.tool_call(name, s(tc, "id"), args, !f.is_null());
    }
    if let Some(f) = m.get("function_call").filter(|f| f.is_object()) {
        o.tool_call(s(f, "name"), "", s(f, "arguments").to_string(), true);
    }
    for it in &mut o.items[n..] {
        it.meta.extend(extra.iter().cloned());
    }
}

pub fn chat_request(body: &[u8], pc: &mut ParseCounts) -> Result<Request, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("request: {e}"))?;
    let raw: RawTop = serde_json::from_slice(body).map_err(|e| format!("request: {e}"))?;
    let mut o = Out::new(pc, 0);
    if let Some(t) = tools_text(raw.tools) {
        o.simple(kind::SYSTEM, t, vec![("sub", "tools".into())]);
    }
    for m in v.get("messages").and_then(Value::as_array).into_iter().flatten() {
        let role = s(m, "role");
        match role {
            "system" | "developer" => {
                o.simple(kind::SYSTEM, chat_text(m.get("content")), vec![("sub", role.to_string().into())])
            }
            "user" => match m.get("content") {
                Some(Value::Array(parts)) => {
                    for p in parts {
                        match s(p, "type") {
                            "text" | "input_text" => o.user_text(s(p, "text")),
                            "image_url" => {
                                let url = p
                                    .get("image_url")
                                    .map(|x| {
                                        x.get("url").and_then(Value::as_str).unwrap_or_else(|| x.as_str().unwrap_or(""))
                                    })
                                    .unwrap_or("");
                                let t = url_text("image", url, o.pc);
                                o.simple(kind::USER_INPUT, t, vec![]);
                            }
                            "input_audio" => {
                                let a = p.get("input_audio").unwrap_or(&Value::Null);
                                let t = payload("audio", s(a, "format"), s(a, "data"), o.pc);
                                o.simple(kind::USER_INPUT, t, vec![]);
                            }
                            "file" => {
                                let f = p.get("file").unwrap_or(&Value::Null);
                                let t = match f.get("file_data").and_then(Value::as_str) {
                                    Some(d) => url_text("file", d, o.pc),
                                    None => format!("[file:{}]", s(f, "file_id")),
                                };
                                o.simple(kind::USER_INPUT, t, vec![("sub", "file".into())]);
                            }
                            other => o.other(&format!("openai.chat.part/{other}"), p),
                        }
                    }
                }
                c => o.user_text(&chat_text(c)),
            },
            "assistant" => chat_assistant(&mut o, m, &[]),
            "tool" => o.tool_result("", s(m, "tool_call_id"), chat_text(m.get("content")), status::OK),
            "function" => o.tool_result(s(m, "name"), "", chat_text(m.get("content")), status::OK),
            other => o.other(&format!("openai.chat.role/{other}"), m),
        }
    }
    Ok(Request { model: s(&v, "model").to_string(), session: None, items: o.items })
}

fn chat_usage(us: &Value, into: &mut Map<String, Value>) {
    if let Some(n) = u(us, "prompt_tokens") {
        into.insert("in".into(), n.into());
    }
    if let Some(n) = u(us, "completion_tokens") {
        into.insert("out".into(), n.into());
    }
    if let Some(n) = us.get("prompt_tokens_details").and_then(|d| u(d, "cached_tokens")) {
        into.insert("cache_read".into(), n.into());
    }
    if let Some(n) = us.get("completion_tokens_details").and_then(|d| u(d, "reasoning_tokens")) {
        into.insert("reasoning".into(), n.into());
    }
}

fn choice_meta(index: u64) -> Vec<(&'static str, Value)> {
    if index == 0 { vec![] } else { vec![("choice", index.into())] }
}

pub fn chat_response_json(body: &[u8], turn: u32, pc: &mut ParseCounts) -> Result<Response, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("response: {e}"))?;
    let mut r =
        Response { model: s(&v, "model").into(), id: s(&v, "id").into(), complete: true, ..Response::default() };
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        r.errors.push(e.to_string());
    }
    if let Some(us) = v.get("usage") {
        chat_usage(us, &mut r.usage);
    }
    let mut o = Out::new(pc, turn);
    for c in v.get("choices").and_then(Value::as_array).into_iter().flatten() {
        let idx = u(c, "index").unwrap_or(0);
        if idx == 0 {
            r.stop = s(c, "finish_reason").to_string();
        }
        chat_assistant(&mut o, c.get("message").unwrap_or(&Value::Null), &choice_meta(idx));
    }
    r.items = o.items;
    Ok(r)
}

pub fn chat_response_sse(events: &[sse::Event], turn: u32, pc: &mut ParseCounts) -> Response {
    #[derive(Default)]
    struct Choice {
        reasoning: String,
        content: String,
        refusal: String,
        tools: BTreeMap<u64, (String, String, String)>,
        finish: String,
    }
    let mut r = Response::default();
    let mut choices: BTreeMap<u64, Choice> = BTreeMap::new();
    for e in events {
        if e.data.trim() == "[DONE]" {
            r.complete = true;
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(&e.data) else { continue };
        if let Some(err) = d.get("error").filter(|x| !x.is_null()) {
            r.errors.push(err.to_string());
            continue;
        }
        if r.model.is_empty() {
            r.model = s(&d, "model").into();
            r.id = s(&d, "id").into();
        }
        if let Some(us) = d.get("usage").filter(|x| x.is_object()) {
            chat_usage(us, &mut r.usage);
        }
        for c in d.get("choices").and_then(Value::as_array).into_iter().flatten() {
            let ch = choices.entry(u(c, "index").unwrap_or(0)).or_default();
            if let Some(f) = c.get("finish_reason").and_then(Value::as_str) {
                ch.finish = f.into();
            }
            let dl = c.get("delta").unwrap_or(&Value::Null);
            for k in ["reasoning_content", "reasoning"] {
                if let Some(t) = dl.get(k).and_then(Value::as_str) {
                    ch.reasoning.push_str(t);
                    break;
                }
            }
            if let Some(t) = dl.get("content").and_then(Value::as_str) {
                ch.content.push_str(t);
            }
            if let Some(t) = dl.get("refusal").and_then(Value::as_str) {
                ch.refusal.push_str(t);
            }
            for tc in dl.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                let t = ch.tools.entry(u(tc, "index").unwrap_or(0)).or_default();
                if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|x| !x.is_empty()) {
                    t.0 = id.into();
                }
                let f = tc.get("function").unwrap_or(&Value::Null);
                if let Some(n) = f.get("name").and_then(Value::as_str).filter(|x| !x.is_empty())
                    && t.1.is_empty()
                {
                    t.1 = n.into();
                }
                if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                    t.2.push_str(a);
                }
            }
        }
    }
    let mut o = Out::new(pc, turn);
    for (idx, ch) in &choices {
        if *idx == 0 {
            r.stop = ch.finish.clone();
        }
        let mut msg = Map::new();
        msg.insert("role".into(), "assistant".into());
        if !ch.reasoning.is_empty() {
            msg.insert("reasoning_content".into(), ch.reasoning.clone().into());
        }
        msg.insert("content".into(), ch.content.clone().into());
        if !ch.refusal.is_empty() {
            msg.insert("refusal".into(), ch.refusal.clone().into());
        }
        let calls: Vec<Value> = ch
            .tools
            .values()
            .map(|(id, name, args)| serde_json::json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}))
            .collect();
        if !calls.is_empty() {
            msg.insert("tool_calls".into(), Value::Array(calls));
        }
        let mut meta = choice_meta(*idx);
        if !r.complete && ch.finish.is_empty() {
            meta.push(("partial", true.into()));
        }
        chat_assistant(&mut o, &Value::Object(msg), &meta);
    }
    r.items = o.items;
    r
}

// ---------------------------------------------------------------- OpenAI Responses

/// Text of a Responses output/content field: a string, or parts joined.
fn parts_text(c: Option<&Value>, pc: &mut ParseCounts) -> String {
    match c {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| match s(p, "type") {
                "input_image" => match p.get("image_url").and_then(Value::as_str) {
                    Some(url) => url_text("image", url, pc),
                    None => format!("[image:file:{}]", s(p, "file_id")),
                },
                _ => p.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| p.to_string()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// One input or output item of the Responses API.
fn responses_item(o: &mut Out, it: &Value) {
    let ty = s(it, "type");
    let role = s(it, "role");
    let call_id = || it.get("call_id").or_else(|| it.get("id")).and_then(Value::as_str).unwrap_or("").to_string();
    match ty {
        "message" | "" if !role.is_empty() => {
            let parts: Vec<Value> = match it.get("content") {
                Some(Value::String(t)) => vec![serde_json::json!({"type": "input_text", "text": t})],
                Some(Value::Array(p)) => p.clone(),
                _ => vec![],
            };
            for p in &parts {
                let pt = s(p, "type");
                let text = match pt {
                    "input_text" | "output_text" | "text" => s(p, "text").to_string(),
                    "refusal" => s(p, "refusal").to_string(),
                    "input_image" => parts_text(Some(&Value::Array(vec![p.clone()])), o.pc),
                    "input_file" => match p.get("file_data").and_then(Value::as_str) {
                        Some(d) => url_text("file", d, o.pc),
                        None => format!("[file:{}{}]", s(p, "file_id"), s(p, "filename")),
                    },
                    _ => {
                        o.other(&format!("openai.responses.part/{pt}"), p);
                        continue;
                    }
                };
                match role {
                    "user" if matches!(pt, "input_image" | "input_file") => o.simple(kind::USER_INPUT, text, vec![]),
                    "user" => o.user_text(&text),
                    "assistant" if pt == "refusal" => o.simple(kind::ASSISTANT, text, vec![("sub", "refusal".into())]),
                    "assistant" => o.simple(kind::ASSISTANT, text, vec![]),
                    _ => o.simple(kind::SYSTEM, text, vec![("sub", role.to_string().into())]),
                }
            }
        }
        "function_call" => o.tool_call(s(it, "name"), &call_id(), s(it, "arguments").to_string(), true),
        "custom_tool_call" => o.tool_call(s(it, "name"), &call_id(), s(it, "input").to_string(), false),
        "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
            let text = parts_text(it.get("output"), o.pc);
            o.tool_result("", &call_id(), text, status::OK);
        }
        "reasoning" => {
            let join = |k: &str| {
                it.get(k)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter().filter_map(|x| x.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n\n")
                    })
                    .unwrap_or_default()
            };
            o.simple(kind::THINKING, join("summary"), vec![]);
            o.simple(kind::THINKING, join("content"), vec![("sub", "reasoning_text".into())]);
            if it.get("encrypted_content").is_some_and(|x| x.is_string()) {
                o.pc.noise("openai.encrypted_reasoning", s(it, "encrypted_content").len());
            }
        }
        "item_reference" => {}
        t if t.ends_with("_call_output") => {
            let text = parts_text(it.get("output"), o.pc);
            o.tool_result(t.trim_end_matches("_call_output"), &call_id(), text, status::OK);
        }
        t if t.ends_with("_call") => {
            let body = ["arguments", "action", "input", "code", "query", "queries"]
                .iter()
                .find_map(|k| it.get(*k))
                .map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))
                .unwrap_or_else(|| it.to_string());
            let name = it.get("name").and_then(Value::as_str).unwrap_or(t.trim_end_matches("_call"));
            o.tool_call(name, &call_id(), body, true);
        }
        other => o.other(&format!("openai.responses.item/{other}"), it),
    }
}

pub fn responses_request(body: &[u8], pc: &mut ParseCounts) -> Result<Request, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("request: {e}"))?;
    let raw: RawTop = serde_json::from_slice(body).map_err(|e| format!("request: {e}"))?;
    let mut o = Out::new(pc, 0);
    if let Some(t) = v.get("instructions").and_then(Value::as_str) {
        o.simple(kind::SYSTEM, t.to_string(), vec![("sub", "instructions".into())]);
    }
    if let Some(t) = tools_text(raw.tools) {
        o.simple(kind::SYSTEM, t, vec![("sub", "tools".into())]);
    }
    match v.get("input") {
        Some(Value::String(t)) => o.user_text(t),
        Some(Value::Array(items)) => {
            for it in items {
                responses_item(&mut o, it);
            }
        }
        _ => {}
    }
    Ok(Request { model: s(&v, "model").to_string(), session: None, items: o.items })
}

fn responses_usage(us: &Value, into: &mut Map<String, Value>) {
    if let Some(n) = u(us, "input_tokens") {
        into.insert("in".into(), n.into());
    }
    if let Some(n) = u(us, "output_tokens") {
        into.insert("out".into(), n.into());
    }
    if let Some(n) = us.get("input_tokens_details").and_then(|d| u(d, "cached_tokens")) {
        into.insert("cache_read".into(), n.into());
    }
    if let Some(n) = us.get("output_tokens_details").and_then(|d| u(d, "reasoning_tokens")) {
        into.insert("reasoning".into(), n.into());
    }
}

/// Model, id, usage, status and error of a whole `response` object.
fn responses_head(resp: &Value, r: &mut Response) {
    if !s(resp, "model").is_empty() {
        r.model = s(resp, "model").into();
    }
    if !s(resp, "id").is_empty() {
        r.id = s(resp, "id").into();
    }
    if let Some(us) = resp.get("usage").filter(|x| x.is_object()) {
        responses_usage(us, &mut r.usage);
    }
    let st = s(resp, "status");
    let reason = resp.get("incomplete_details").map(|d| s(d, "reason")).unwrap_or("");
    if !st.is_empty() {
        r.stop = if reason.is_empty() { st.to_string() } else { format!("{st}:{reason}") };
    }
    if let Some(e) = resp.get("error").filter(|e| !e.is_null()) {
        r.errors.push(e.to_string());
    }
}

pub fn responses_response_json(body: &[u8], turn: u32, pc: &mut ParseCounts) -> Result<Response, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("response: {e}"))?;
    let mut r = Response { complete: true, ..Response::default() };
    responses_head(&v, &mut r);
    let mut o = Out::new(pc, turn);
    for it in v.get("output").and_then(Value::as_array).into_iter().flatten() {
        responses_item(&mut o, it);
    }
    r.items = o.items;
    Ok(r)
}

pub fn responses_response_sse(events: &[sse::Event], turn: u32, pc: &mut ParseCounts) -> Response {
    #[derive(Default)]
    struct St {
        added: Option<Value>,
        done: Option<Value>,
        text: String,
        args: String,
        summary: String,
    }
    let mut r = Response::default();
    let mut items: BTreeMap<u64, St> = BTreeMap::new();
    let mut final_output: Option<Vec<Value>> = None;
    for e in events {
        let Ok(d) = serde_json::from_str::<Value>(&e.data) else { continue };
        let oi = u(&d, "output_index").unwrap_or(0);
        match s(&d, "type") {
            "response.created" | "response.in_progress" => {
                responses_head(d.get("response").unwrap_or(&Value::Null), &mut r)
            }
            "response.output_item.added" => items.entry(oi).or_default().added = d.get("item").cloned(),
            "response.output_item.done" => items.entry(oi).or_default().done = d.get("item").cloned(),
            "response.output_text.delta" | "response.refusal.delta" => {
                items.entry(oi).or_default().text.push_str(s(&d, "delta"))
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                items.entry(oi).or_default().args.push_str(s(&d, "delta"))
            }
            "response.reasoning_summary_text.delta" => items.entry(oi).or_default().summary.push_str(s(&d, "delta")),
            "response.completed" | "response.incomplete" | "response.failed" => {
                r.complete = true;
                let resp = d.get("response").unwrap_or(&Value::Null);
                responses_head(resp, &mut r);
                final_output = resp.get("output").and_then(Value::as_array).cloned();
            }
            "error" => r.errors.push(d.get("error").unwrap_or(&d).to_string()),
            _ => {}
        }
    }
    let mut o = Out::new(pc, turn);
    if items.is_empty()
        && let Some(out) = &final_output
    {
        for it in out {
            responses_item(&mut o, it);
        }
    }
    for st in items.values() {
        if let Some(done) = &st.done {
            responses_item(&mut o, done);
            continue;
        }
        // cut off before the item was done: what arrived of it
        let Some(added) = &st.added else { continue };
        let n = o.items.len();
        match s(added, "type") {
            "message" => o.simple(kind::ASSISTANT, st.text.clone(), vec![]),
            "function_call" => o.tool_call(s(added, "name"), s(added, "call_id"), st.args.clone(), true),
            "custom_tool_call" => o.tool_call(s(added, "name"), s(added, "call_id"), st.args.clone(), false),
            "reasoning" => o.simple(kind::THINKING, st.summary.clone(), vec![]),
            _ => responses_item(&mut o, added),
        }
        for it in &mut o.items[n..] {
            it.meta.push(("partial", true.into()));
        }
    }
    r.items = o.items;
    r
}

/// Parse a request body of `route`.
pub fn request(route: Route, body: &[u8], pc: &mut ParseCounts) -> Result<Request, String> {
    match route {
        Route::AnthropicMessages => anthropic_request(body, pc),
        Route::OpenAiChat => chat_request(body, pc),
        Route::OpenAiResponses => responses_request(body, pc),
    }
}

/// Parse a 2xx response body of `route` (SSE when `sse`). `turn` is the
/// request's turn count (outputs belong to the last turn).
pub fn response(
    route: Route,
    body: &[u8],
    sse_body: bool,
    turn: u32,
    pc: &mut ParseCounts,
) -> Result<Response, String> {
    if sse_body {
        let text = String::from_utf8_lossy(body);
        let events = sse::parse(&text);
        return Ok(match route {
            Route::AnthropicMessages => anthropic_response_sse(&events, turn, pc),
            Route::OpenAiChat => chat_response_sse(&events, turn, pc),
            Route::OpenAiResponses => responses_response_sse(&events, turn, pc),
        });
    }
    match route {
        Route::AnthropicMessages => anthropic_response_json(body, turn, pc),
        Route::OpenAiChat => chat_response_json(body, turn, pc),
        Route::OpenAiResponses => responses_response_json(body, turn, pc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(items: &[Item]) -> Vec<(&'static str, &str)> {
        items.iter().map(|i| (kind::name(i.kind), i.text.as_str())).collect()
    }

    #[test]
    fn routes() {
        assert_eq!(Route::of("/v1/messages"), Some(Route::AnthropicMessages));
        assert_eq!(Route::of("/v1/messages/count_tokens"), None);
        assert_eq!(Route::of("/api/v1/chat/completions"), Some(Route::OpenAiChat));
        assert_eq!(Route::of("/v1/responses"), Some(Route::OpenAiResponses));
        assert_eq!(Route::of("/v1/responses/resp_1"), None);
        assert_eq!(Route::of("/v1/models"), None);
    }

    #[test]
    fn canonical_json_ignores_spacing_and_key_order() {
        assert_eq!(
            canon_json(r#"{"b": 1, "a": {"y": [1, 2], "x": "s"}}"#),
            canon_json(r#"{"a":{"x":"s","y":[1,2]},"b":1}"#)
        );
        assert_eq!(canon_json("{\"a\""), None);
    }

    #[test]
    fn anthropic_request_maps_kinds_and_keeps_inputs_verbatim() {
        let body = br#"{"model":"claude-x","metadata":{"user_id":"user_ab_account_cd_session_0123456789ab"},
          "system":[{"type":"text","text":"You are a test agent."}],
          "tools":[{"name":"Read","input_schema":{}}],
          "messages":[
            {"role":"user","content":[{"type":"text","text":"<system-reminder>be nice</system-reminder>"},{"type":"text","text":"read a.txt"}]},
            {"role":"assistant","content":[{"type":"thinking","thinking":"hmm","signature":"x"},{"type":"tool_use","id":"t1","name":"Read","input":{"z": 1, "a": 2}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"hello","is_error":true}]}
          ]}"#;
        let mut pc = ParseCounts::default();
        let r = anthropic_request(body, &mut pc).unwrap();
        assert_eq!(r.session.as_deref(), Some("0123456789ab"));
        assert_eq!(
            kinds(&r.items),
            vec![
                ("system", "You are a test agent."),
                ("system", r#"[{"name":"Read","input_schema":{}}]"#),
                ("system", "<system-reminder>be nice</system-reminder>"),
                ("user_input", "read a.txt"),
                ("thinking", "hmm"),
                ("tool_call", r#"{"z": 1, "a": 2}"#),
                ("tool_result", "hello"),
            ]
        );
        assert_eq!(r.items[5].canon.as_deref(), Some(r#"{"a":2,"z":1}"#));
        assert_eq!(r.items[6].status, status::ERROR);
        assert_eq!(r.items[6].call_id, "t1");
        assert_eq!(r.items[3].turn, 1);
    }

    #[test]
    fn anthropic_stream_assembles_blocks_usage_and_partial() {
        let ev = |t: &str| sse::Event { event: String::new(), data: t.to_string() };
        let events = vec![
            ev(
                r#"{"type":"message_start","message":{"id":"m1","model":"claude-x","usage":{"input_tokens":10,"cache_read_input_tokens":4}}}"#,
            ),
            ev(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
            ev(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me"}}"#),
            ev(r#"{"type":"content_block_stop","index":0}"#),
            ev(r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            ev(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}"#),
            ev(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}"#),
            ev(r#"{"type":"content_block_stop","index":1}"#),
            ev(
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t9","name":"Bash","input":{}}}"#,
            ),
            ev(
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\": "}}"#,
            ),
            ev(
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
            ),
        ];
        let mut pc = ParseCounts::default();
        let r = anthropic_response_sse(&events, 3, &mut pc);
        assert!(!r.complete);
        assert_eq!(
            kinds(&r.items),
            vec![("thinking", "let me"), ("assistant", "Hello"), ("tool_call", r#"{"command": "ls"}"#)]
        );
        assert!(r.items[2].meta.contains(&("partial", true.into())));
        assert_eq!(r.usage["in"], 10);
        assert_eq!(r.items[1].turn, 3);
    }

    #[test]
    fn chat_stream_with_tool_calls() {
        let ev = |t: &str| sse::Event { event: String::new(), data: t.to_string() };
        let events = vec![
            ev(
                r#"{"id":"c1","model":"gpt-x","choices":[{"index":0,"delta":{"role":"assistant","content":"I will "}}]}"#,
            ),
            ev(r#"{"id":"c1","model":"gpt-x","choices":[{"index":0,"delta":{"content":"look."}}]}"#),
            ev(
                r#"{"id":"c1","model":"gpt-x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"grep","arguments":"{\"q\":"}}]}}]}"#,
            ),
            ev(
                r#"{"id":"c1","model":"gpt-x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]}}]}"#,
            ),
            ev(
                r#"{"id":"c1","model":"gpt-x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
            ),
            ev("[DONE]"),
        ];
        let mut pc = ParseCounts::default();
        let r = chat_response_sse(&events, 1, &mut pc);
        assert!(r.complete);
        assert_eq!(r.stop, "tool_calls");
        assert_eq!(kinds(&r.items), vec![("assistant", "I will look."), ("tool_call", r#"{"q":"x"}"#)]);
        assert_eq!(r.items[1].tool, "grep");
        assert_eq!(r.items[1].call_id, "call_1");
        // the same call re-sent in the next request's history has the same canonical input
        let req = br#"{"model":"gpt-x","messages":[{"role":"assistant","content":"I will look.","tool_calls":[{"id":"call_1","type":"function","function":{"name":"grep","arguments":"{\"q\": \"x\"}"}}]},{"role":"tool","tool_call_id":"call_1","content":"found"}]}"#;
        let q = chat_request(req, &mut pc).unwrap();
        assert_eq!(q.items[1].canon, r.items[1].canon);
        assert_eq!(kinds(&q.items)[2], ("tool_result", "found"));
    }

    #[test]
    fn responses_items_in_and_out() {
        let req = br#"{"model":"gpt-y","instructions":"be brief","input":[
            {"type":"message","role":"developer","content":[{"type":"input_text","text":"dev note"}]},
            {"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"},{"type":"input_text","text":"fix it"}]},
            {"type":"reasoning","summary":[{"type":"summary_text","text":"think"}],"encrypted_content":"xx"},
            {"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":[\"ls\"]}"},
            {"type":"function_call_output","call_id":"c1","output":"a.txt"}]}"#;
        let mut pc = ParseCounts::default();
        let q = responses_request(req, &mut pc).unwrap();
        assert_eq!(
            kinds(&q.items),
            vec![
                ("system", "be brief"),
                ("system", "dev note"),
                ("system", "<environment_context>cwd</environment_context>"),
                ("user_input", "fix it"),
                ("thinking", "think"),
                ("tool_call", r#"{"cmd":["ls"]}"#),
                ("tool_result", "a.txt"),
            ]
        );
        let ev = |t: &str| sse::Event { event: String::new(), data: t.to_string() };
        let events = vec![
            ev(r#"{"type":"response.created","response":{"id":"r1","model":"gpt-y"}}"#),
            ev(
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}"#,
            ),
            ev(r#"{"type":"response.output_text.delta","output_index":0,"delta":"Done"}"#),
            ev(
                r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            ),
            ev(
                r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"c2","name":"shell"}}"#,
            ),
            ev(r#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"cmd\""}"#),
        ];
        let r = responses_response_sse(&events, 1, &mut pc);
        assert!(!r.complete);
        assert_eq!(kinds(&r.items), vec![("assistant", "Done."), ("tool_call", r#"{"cmd""#)]);
        assert!(r.items[1].meta.contains(&("partial", true.into())));
    }
}
