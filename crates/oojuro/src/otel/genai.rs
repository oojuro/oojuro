//! OpenTelemetry GenAI semantic conventions to records.
//!
//! A span is a GenAI span when it has a `gen_ai.*` or `llm.*` attribute
//! or a `gen_ai.*` event; anything else is counted and skipped. Messages are
//! read from the first of these that a span carries (instrumentations differ,
//! and some carry the same messages twice):
//! 1. `gen_ai.input.messages` / `gen_ai.output.messages` /
//!    `gen_ai.system_instructions` (current conventions; on the span or on a
//!    `gen_ai.client.inference.operation.details` event), structured parts;
//! 2. indexed `gen_ai.prompt.<i>.*` / `gen_ai.completion.<i>.*` attributes
//!    (OpenLLMetry's OpenAI and Anthropic instrumentations), with
//!    `tool_calls.<j>.*` and `tool_call_id`;
//! 3. `gen_ai.prompt` / `gen_ai.completion` (JSON or text; on the span or on
//!    `gen_ai.content.prompt` / `gen_ai.content.completion` events);
//! 4. message events `gen_ai.{system,user,assistant,tool}.message` and
//!    `gen_ai.choice` (span events, or log records sent to `/v1/logs`).
//!
//! Tool execution spans (`gen_ai.operation.name = execute_tool`, or a
//! `gen_ai.tool.name`) give a `tool_call` (`gen_ai.tool.call.arguments`) and
//! a `tool_result` (`gen_ai.tool.call.result`, or the span's error).
//!
//! Roles map to kinds: system/developer -> `system`, user -> `user_input`
//! (sender `caller`), assistant -> `assistant` (and `tool_call` per tool
//! call, `thinking` for reasoning parts), tool -> `tool_result`. Model,
//! provider, operation, span and trace ids ride in metadata, and every
//! `gen_ai.usage.*` attribute on the call's outputs.
//!
//! **Store once.** Frameworks send the whole history with every call, so
//! every message gets a store-once key: a tool call or result by its call
//! id; a system prompt by its text (once per writer); any other message by
//! (conversation, kind, text, how many identical messages precede it in the
//! same call), where the conversation is `gen_ai.conversation.id` (or
//! `session.id`), else the trace. A history re-sent in the next call
//! repeats keys already stored; a message repeated later in the
//! conversation is a new key.
use super::model::{Any, Attrs, Batch, Event, Log, Span, get, get_str};
use crate::append::NewRecord;
use oojuro_capture::hash64;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// What a batch held and what became of it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Counts {
    pub spans: u64,
    pub genai_spans: u64,
    /// Spans that are not GenAI spans (skipped).
    pub unknown_spans: u64,
    /// GenAI spans without message content (content capture off, embeddings, ...).
    pub no_content: u64,
    pub log_records: u64,
    pub genai_events: u64,
    /// Log records that are not GenAI events (skipped).
    pub unknown_logs: u64,
    /// Records made (before store-once keys and settings).
    pub records: u64,
}

impl Counts {
    pub fn add(&mut self, o: &Counts) {
        self.spans += o.spans;
        self.genai_spans += o.genai_spans;
        self.unknown_spans += o.unknown_spans;
        self.no_content += o.no_content;
        self.log_records += o.log_records;
        self.genai_events += o.genai_events;
        self.unknown_logs += o.unknown_logs;
        self.records += o.records;
    }
}

/// The writer of a resource: the configured mapping of its `service.name`,
/// else `otel:<service.name>`.
pub fn writer_for(resource: &Attrs, writers: &BTreeMap<String, String>) -> String {
    let service = get_str(resource, "service.name").unwrap_or("unknown_service");
    if let Some(w) = writers.get(service) {
        return w.clone();
    }
    let clean: String = service.chars().filter(|c| !c.is_control()).take(200).collect();
    format!("otel:{clean}")
}

/// One message part turned into a record.
#[derive(Clone, Debug, Default, PartialEq)]
struct Msg {
    kind: &'static str,
    text: String,
    tool: String,
    call_id: String,
    status: &'static str,
    role: String,
}

fn kind_of_role(role: &str) -> &'static str {
    match role {
        "system" | "developer" => "system",
        "user" | "human" => "user_input",
        "tool" | "function" | "ipython" => "tool_result",
        _ => "assistant",
    }
}

fn vs<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Text of a JSON value: strings as they are, anything else as JSON.
fn jtext(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Arguments of a tool call: a JSON string as it is, an object as JSON.
fn args_text(v: Option<&Value>) -> String {
    v.map(jtext).unwrap_or_default()
}

/// Messages of one role from structured parts or content blocks
/// (current conventions' `parts`, OpenAI content arrays, Anthropic blocks).
fn parts(role: &str, items: &[Value], out: &mut Vec<Msg>) {
    let mut text: Vec<String> = Vec::new();
    let flush = |text: &mut Vec<String>, out: &mut Vec<Msg>| {
        if !text.is_empty() {
            out.push(Msg { kind: kind_of_role(role), text: text.join("\n"), role: role.into(), ..Msg::default() });
            text.clear();
        }
    };
    for p in items {
        if let Value::String(s) = p {
            text.push(s.clone());
            continue;
        }
        match vs(p, "type") {
            "text" | "input_text" | "output_text" => {
                let t = p.get("content").or_else(|| p.get("text")).map(jtext).unwrap_or_default();
                text.push(t);
            }
            "reasoning" | "thinking" => {
                flush(&mut text, out);
                let t = p.get("content").or_else(|| p.get("thinking")).or_else(|| p.get("text")).map(jtext);
                out.push(Msg { kind: "thinking", text: t.unwrap_or_default(), role: role.into(), ..Msg::default() });
            }
            "tool_call" | "tool_use" | "function_call" => {
                flush(&mut text, out);
                let f = p.get("function").unwrap_or(p);
                out.push(Msg {
                    kind: "tool_call",
                    text: args_text(f.get("arguments").or_else(|| p.get("input")).or_else(|| p.get("arguments"))),
                    tool: vs(f, "name").into(),
                    call_id: p.get("id").or_else(|| p.get("call_id")).map(jtext).unwrap_or_default(),
                    role: role.into(),
                    ..Msg::default()
                });
            }
            "tool_call_response" | "tool_result" | "function_call_output" => {
                flush(&mut text, out);
                let body = p
                    .get("response")
                    .or_else(|| p.get("result"))
                    .or_else(|| p.get("content"))
                    .or_else(|| p.get("output"));
                let body = match body {
                    Some(Value::Array(a)) => {
                        let mut inner = Vec::new();
                        parts("tool", a, &mut inner);
                        inner.into_iter().map(|m| m.text).collect::<Vec<_>>().join("\n")
                    }
                    Some(v) => jtext(v),
                    None => String::new(),
                };
                let error = p.get("is_error").and_then(Value::as_bool) == Some(true);
                out.push(Msg {
                    kind: "tool_result",
                    text: body,
                    call_id: p
                        .get("id")
                        .or_else(|| p.get("tool_use_id"))
                        .or_else(|| p.get("call_id"))
                        .map(jtext)
                        .unwrap_or_default(),
                    status: if error { "error" } else { "ok" },
                    role: role.into(),
                    ..Msg::default()
                });
            }
            "image" | "image_url" | "blob" | "uri" | "file" | "audio" | "input_image" | "input_audio" => {
                let src = p.get("source").unwrap_or(&Value::Null);
                let mime = [vs(p, "mime_type"), vs(p, "media_type"), vs(src, "media_type"), vs(p, "modality")]
                    .into_iter()
                    .find(|x| !x.is_empty())
                    .unwrap_or("");
                text.push(format!("[media:{}{}{}]", vs(p, "type"), if mime.is_empty() { "" } else { ":" }, mime));
            }
            _ => text.push(p.to_string()),
        }
    }
    flush(&mut text, out);
}

/// Messages of one chat message object (`role`, `content` or `parts`,
/// `tool_calls`, `tool_call_id`).
fn message(v: &Value, default_role: &str, out: &mut Vec<Msg>) {
    if let Value::String(s) = v {
        out.push(Msg {
            kind: kind_of_role(default_role),
            text: s.clone(),
            role: default_role.into(),
            ..Msg::default()
        });
        return;
    }
    let role = match vs(v, "role") {
        "" => default_role,
        r => r,
    };
    let call_id = v.get("tool_call_id").or_else(|| v.get("id")).map(jtext).unwrap_or_default();
    let add_text = |content: &Value, out: &mut Vec<Msg>| match content {
        Value::Array(items) => parts(role, items, out),
        Value::String(s) => {
            // legacy instrumentations put content blocks in a JSON string
            match serde_json::from_str::<Value>(s) {
                Ok(Value::Array(items)) if items.iter().all(|b| b.get("type").is_some()) => parts(role, &items, out),
                _ => out.push(Msg { kind: kind_of_role(role), text: s.clone(), role: role.into(), ..Msg::default() }),
            }
        }
        Value::Null => {}
        other => {
            out.push(Msg { kind: kind_of_role(role), text: other.to_string(), role: role.into(), ..Msg::default() })
        }
    };
    let start = out.len();
    if let Some(p) = v.get("parts") {
        add_text(p, out);
    }
    if let Some(c) = v.get("content") {
        add_text(c, out);
    }
    // a tool message's text is the result of its call
    for m in &mut out[start..] {
        if m.kind == "tool_result" && m.call_id.is_empty() {
            m.call_id = call_id.clone();
            if m.status.is_empty() {
                m.status = "ok";
            }
        }
    }
    let calls = match v.get("tool_calls") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(c) => c.clone(),
        None => Value::Null,
    };
    if let Value::Array(calls) = calls {
        for c in &calls {
            let f = c.get("function").unwrap_or(c);
            out.push(Msg {
                kind: "tool_call",
                text: args_text(f.get("arguments").or_else(|| c.get("input"))),
                tool: vs(f, "name").into(),
                call_id: c.get("id").map(jtext).unwrap_or_default(),
                role: role.into(),
                ..Msg::default()
            });
        }
    }
}

/// Messages from a JSON value: an array of messages, one message, or text.
fn messages(v: &Value, default_role: &str, out: &mut Vec<Msg>) {
    match v {
        Value::Array(items) => items.iter().for_each(|m| message(m, default_role, out)),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(inner @ (Value::Array(_) | Value::Object(_))) => messages(&inner, default_role, out),
            _ => message(v, default_role, out),
        },
        Value::Object(_) => message(v, default_role, out),
        _ => {}
    }
}

fn attr_json(attrs: &Attrs, key: &str) -> Option<Value> {
    get(attrs, key).map(Any::to_json).filter(|v| !v.is_null())
}

/// Source 1: current conventions.
fn current(attrs: &Attrs, ins: &mut Vec<Msg>, outs: &mut Vec<Msg>) {
    if let Some(v) = attr_json(attrs, "gen_ai.system_instructions") {
        match &v {
            Value::Array(items) => parts("system", items, ins),
            other => messages(other, "system", ins),
        }
    }
    if let Some(v) = attr_json(attrs, "gen_ai.input.messages") {
        messages(&v, "user", ins);
    }
    if let Some(v) = attr_json(attrs, "gen_ai.output.messages") {
        messages(&v, "assistant", outs);
    }
}

/// Source 2: indexed `gen_ai.prompt.<i>.*` / `gen_ai.completion.<i>.*`.
fn indexed(attrs: &Attrs, prefix: &str, default_role: &str, out: &mut Vec<Msg>) {
    let mut by_index: BTreeMap<u64, serde_json::Map<String, Value>> = BTreeMap::new();
    for (k, v) in attrs {
        let Some(rest) = k.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) else { continue };
        let Some((i, field)) = rest.split_once('.') else { continue };
        let Ok(i) = i.parse::<u64>() else { continue };
        let m = by_index.entry(i).or_default();
        // tool_calls.<j>.<name|arguments|id>
        if let Some(tc) = field.strip_prefix("tool_calls.")
            && let Some((j, f)) = tc.split_once('.')
            && let Ok(j) = j.parse::<usize>()
        {
            let calls = m.entry("tool_calls").or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(a) = calls {
                while a.len() <= j {
                    a.push(Value::Object(Default::default()));
                }
                a[j][f] = v.to_json();
            }
            continue;
        }
        m.insert(field.to_string(), v.to_json());
    }
    for (_, m) in by_index {
        message(&Value::Object(m), default_role, out);
    }
}

/// Source 3: `gen_ai.prompt` / `gen_ai.completion` as one value.
fn blobs(attrs: &Attrs, ins: &mut Vec<Msg>, outs: &mut Vec<Msg>) {
    if let Some(v) = get(attrs, "gen_ai.prompt").filter(|v| !matches!(v, Any::Map(_))) {
        messages(&v.to_json(), "user", ins);
    }
    if let Some(v) = get(attrs, "gen_ai.completion").filter(|v| !matches!(v, Any::Map(_))) {
        messages(&v.to_json(), "assistant", outs);
    }
}

/// The message object of a GenAI event: its log body, else the
/// `gen_ai.event.content` attribute (JSON), else the attributes themselves.
fn event_message(e: &Event) -> Value {
    if let Some(b) = &e.body
        && !matches!(b, Any::Empty)
    {
        return b.to_json();
    }
    if let Some(Any::Str(s)) = get(&e.attrs, "gen_ai.event.content")
        && let Ok(v) = serde_json::from_str::<Value>(s)
    {
        return v;
    }
    Value::Object(e.attrs.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())
}

/// The event name: the field, else the `event.name` attribute.
fn event_name(e: &Event) -> &str {
    if !e.name.is_empty() {
        return &e.name;
    }
    get_str(&e.attrs, "event.name").unwrap_or("")
}

/// Source 4: message events. Returns whether any event was a GenAI event.
fn events(evs: &[&Event], ins: &mut Timed, outs: &mut Timed) -> bool {
    let mut any = false;
    for e in evs {
        let name = event_name(e);
        let role = match name {
            "gen_ai.system.message" => "system",
            "gen_ai.user.message" => "user",
            "gen_ai.assistant.message" => "assistant",
            "gen_ai.tool.message" => "tool",
            "gen_ai.choice" => {
                any = true;
                let v = event_message(e);
                let m = v.get("message").cloned().unwrap_or(v);
                let mut out = Vec::new();
                message(&m, "assistant", &mut out);
                outs.extend(out.into_iter().map(|m| (m, e.time_ns)));
                continue;
            }
            n => {
                any |= n.starts_with("gen_ai.");
                continue;
            }
        };
        any = true;
        let mut out = Vec::new();
        message(&event_message(e), role, &mut out);
        ins.extend(out.into_iter().map(|m| (m, e.time_ns)));
    }
    any
}

fn is_genai(attrs: &Attrs, evs: &[Event]) -> bool {
    attrs.iter().any(|(k, _)| k.starts_with("gen_ai.") || k.starts_with("llm."))
        || evs.iter().any(|e| event_name(e).starts_with("gen_ai."))
}

fn ns_to_us(ns: u64) -> i64 {
    (ns / 1000).min(i64::MAX as u64) as i64
}

/// Messages with their times (ns).
type Timed = Vec<(Msg, u64)>;

/// Shared fields of the records of one call.
struct Call<'a> {
    writer: &'a str,
    service: &'a str,
    /// Conversation scope for store-once keys and the session.
    scope: String,
    meta: BTreeMap<String, String>,
    usage: BTreeMap<String, String>,
}

fn call_meta(
    attrs: &Attrs,
    trace: &str,
    span: &str,
    name: &str,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut meta = BTreeMap::new();
    let mut put = |k: &str, v: Option<&str>| {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            meta.insert(k.to_string(), v.to_string());
        }
    };
    put("provider", get_str(attrs, "gen_ai.provider.name").or_else(|| get_str(attrs, "gen_ai.system")));
    put("model", get_str(attrs, "gen_ai.response.model").or_else(|| get_str(attrs, "gen_ai.request.model")));
    put("op", get_str(attrs, "gen_ai.operation.name"));
    put("agent", get_str(attrs, "gen_ai.agent.name"));
    put("span_name", Some(name));
    put("trace", Some(trace));
    put("span", Some(span));
    let mut usage = BTreeMap::new();
    for (k, v) in attrs {
        if let Some(rest) = k.strip_prefix("gen_ai.usage.").or_else(|| k.strip_prefix("llm.usage.")) {
            usage.insert(format!("usage.{rest}"), v.text());
        }
    }
    if let Some(fr) = get(attrs, "gen_ai.response.finish_reasons") {
        usage.insert("finish_reasons".into(), fr.text());
    }
    (meta, usage)
}

fn scope_of(attrs: &Attrs, trace: &str, span: &str) -> String {
    get_str(attrs, "gen_ai.conversation.id")
        .or_else(|| get_str(attrs, "session.id"))
        .map(|c| format!("conv:{c}"))
        .unwrap_or_else(|| if trace.is_empty() { format!("span:{span}") } else { format!("trace:{trace}") })
}

/// Records of one call's messages (inputs, then outputs), with store-once keys.
fn emit(c: &Call, ins: Timed, outs: Timed, out: &mut Vec<NewRecord>) {
    let n_out = outs.len();
    let mut nth: HashMap<(&'static str, String), usize> = HashMap::new();
    let all: Vec<(Msg, u64, bool)> =
        ins.into_iter().map(|(m, t)| (m, t, false)).chain(outs.into_iter().map(|(m, t)| (m, t, true))).collect();
    let last = all.len().saturating_sub(1);
    for (i, (m, t, is_out)) in all.into_iter().enumerate() {
        if m.text.trim().is_empty() && m.kind != "tool_call" {
            continue;
        }
        let dedupe = match m.kind {
            "tool_call" if !m.call_id.is_empty() => format!("call:{}", m.call_id),
            "tool_result" if !m.call_id.is_empty() => format!("result:{}", m.call_id),
            "system" => format!("system:{:016x}", hash64(&m.text)),
            k => {
                let seen = nth.entry((k, format!("{}\0{}", m.tool, m.text))).or_insert(0);
                let key = format!("{}\0{k}\0{}\0{}\0{}", c.scope, *seen, m.tool, m.text);
                *seen += 1;
                format!("msg:{:016x}", hash64(&key))
            }
        };
        let mut meta = c.meta.clone();
        if !m.role.is_empty() {
            meta.insert("role".into(), m.role.clone());
        }
        if m.kind == "user_input" {
            meta.insert("from".into(), "caller".into());
        }
        // the call's token usage rides on its outputs (or its last record)
        if (is_out && n_out > 0) || (n_out == 0 && i == last) {
            meta.extend(c.usage.clone());
        }
        out.push(NewRecord {
            writer: c.writer.to_string(),
            kind: m.kind.to_string(),
            ts_us: ns_to_us(t),
            text: if m.kind == "tool_call" && m.text.is_empty() { "{}".into() } else { m.text },
            tool: m.tool,
            session: format!("otel:{}", c.scope),
            call_id: m.call_id,
            status: m.status.to_string(),
            source: format!("otel:{}", c.service),
            via: "otel".into(),
            dedupe,
            meta,
            ..NewRecord::default()
        });
    }
}

/// Records of a span's tool execution (`execute_tool`).
fn tool_span(s: &Span, ins: &mut Timed, outs: &mut Timed) -> bool {
    let op = get_str(&s.attrs, "gen_ai.operation.name").unwrap_or("");
    let Some(tool) = get_str(&s.attrs, "gen_ai.tool.name").or((op == "execute_tool").then_some(s.name.as_str())) else {
        return false;
    };
    let call_id = get_str(&s.attrs, "gen_ai.tool.call.id").unwrap_or("").to_string();
    let args = get(&s.attrs, "gen_ai.tool.call.arguments").map(Any::text).unwrap_or_default();
    ins.push((
        Msg { kind: "tool_call", text: args, tool: tool.into(), call_id: call_id.clone(), ..Msg::default() },
        s.start_ns,
    ));
    let result = get(&s.attrs, "gen_ai.tool.call.result").map(Any::text);
    let error = s.status_code == 2;
    let text = match (result, error) {
        (Some(r), _) => Some(r),
        (None, true) => Some(if s.status_message.is_empty() {
            get_str(&s.attrs, "error.type").unwrap_or("error").to_string()
        } else {
            s.status_message.clone()
        }),
        (None, false) => None,
    };
    if let Some(text) = text {
        outs.push((
            Msg {
                kind: "tool_result",
                text,
                tool: tool.into(),
                call_id,
                status: if error { "error" } else { "ok" },
                ..Msg::default()
            },
            s.end_ns,
        ));
    }
    true
}

/// Records of exported spans.
pub fn map_spans(batches: &[Batch<Span>], writers: &BTreeMap<String, String>) -> (Vec<NewRecord>, Counts) {
    let mut out = Vec::new();
    let mut n = Counts::default();
    for b in batches {
        let writer = writer_for(&b.resource, writers);
        let service = get_str(&b.resource, "service.name").unwrap_or("unknown_service");
        for s in &b.items {
            n.spans += 1;
            if !is_genai(&s.attrs, &s.events) {
                n.unknown_spans += 1;
                continue;
            }
            n.genai_spans += 1;
            let (meta, usage) = call_meta(&s.attrs, &s.trace_id, &s.span_id, &s.name);
            let call =
                Call { writer: &writer, service, scope: scope_of(&s.attrs, &s.trace_id, &s.span_id), meta, usage };
            let (mut ins, mut outs): (Timed, Timed) = (Vec::new(), Vec::new());
            let tool = tool_span(s, &mut ins, &mut outs);
            // the first message source the span carries
            let (mut i, mut o) = (Vec::new(), Vec::new());
            current(&s.attrs, &mut i, &mut o);
            if i.is_empty() && o.is_empty() {
                for e in s.events.iter().filter(|e| event_name(e) == "gen_ai.client.inference.operation.details") {
                    current(&e.attrs, &mut i, &mut o);
                }
            }
            if i.is_empty() && o.is_empty() {
                indexed(&s.attrs, "gen_ai.prompt", "user", &mut i);
                indexed(&s.attrs, "gen_ai.completion", "assistant", &mut o);
            }
            if i.is_empty() && o.is_empty() {
                blobs(&s.attrs, &mut i, &mut o);
                for e in s.events.iter().filter(|e| event_name(e).starts_with("gen_ai.content.")) {
                    blobs(&e.attrs, &mut i, &mut o);
                }
            }
            let found = !i.is_empty() || !o.is_empty();
            ins.extend(i.into_iter().map(|m| (m, s.start_ns)));
            outs.extend(o.into_iter().map(|m| (m, s.end_ns)));
            if !found && !tool {
                let evs: Vec<&Event> = s.events.iter().collect();
                events(&evs, &mut ins, &mut outs);
            }
            if ins.is_empty() && outs.is_empty() {
                n.no_content += 1;
                continue;
            }
            let before = out.len();
            emit(&call, ins, outs, &mut out);
            n.records += (out.len() - before) as u64;
        }
    }
    (out, n)
}

/// Records of exported log records (GenAI events sent as logs), grouped by
/// the span they belong to.
pub fn map_logs(batches: &[Batch<Log>], writers: &BTreeMap<String, String>) -> (Vec<NewRecord>, Counts) {
    let mut out = Vec::new();
    let mut n = Counts::default();
    for b in batches {
        let writer = writer_for(&b.resource, writers);
        let service = get_str(&b.resource, "service.name").unwrap_or("unknown_service");
        let mut groups: Vec<((String, String), Vec<&Log>)> = Vec::new();
        for l in &b.items {
            n.log_records += 1;
            let name = event_name(&l.event);
            if !name.starts_with("gen_ai.") {
                n.unknown_logs += 1;
                continue;
            }
            n.genai_events += 1;
            let key = (l.trace_id.clone(), l.span_id.clone());
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.1.push(l),
                None => groups.push((key, vec![l])),
            }
        }
        for ((trace, span), logs) in groups {
            let first = &logs[0].event.attrs;
            let (meta, usage) = call_meta(first, &trace, &span, "");
            let call = Call { writer: &writer, service, scope: scope_of(first, &trace, &span), meta, usage };
            let (mut ins, mut outs) = (Vec::new(), Vec::new());
            let evs: Vec<&Event> = logs.iter().map(|l| &l.event).collect();
            events(&evs, &mut ins, &mut outs);
            let before = out.len();
            emit(&call, ins, outs, &mut out);
            n.records += (out.len() - before) as u64;
        }
    }
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otel::model::{Any, Batch, Event, Log, Span};

    fn s(k: &str, v: &str) -> (String, Any) {
        (k.to_string(), Any::Str(v.to_string()))
    }

    fn i(k: &str, v: i64) -> (String, Any) {
        (k.to_string(), Any::Int(v))
    }

    fn batch(spans: Vec<Span>) -> Vec<Batch<Span>> {
        vec![Batch { resource: vec![s("service.name", "travel-bot")], scope: "x".into(), items: spans }]
    }

    fn span(trace: &str, id: &str, attrs: Vec<(String, Any)>) -> Span {
        Span {
            trace_id: trace.into(),
            span_id: id.into(),
            name: "openai.chat".into(),
            start_ns: 1_700_000_000_000_000_000,
            end_ns: 1_700_000_001_000_000_000,
            attrs,
            ..Span::default()
        }
    }

    /// Keep what a journal would keep: the first record of each store-once key.
    fn stored(recs: &[NewRecord], seen: &mut std::collections::HashSet<(String, String)>) -> Vec<(String, String)> {
        recs.iter()
            .filter(|r| r.dedupe.is_empty() || seen.insert((r.writer.clone(), r.dedupe.clone())))
            .map(|r| {
                (r.kind.clone(), if r.tool.is_empty() { r.text.clone() } else { format!("{}:{}", r.tool, r.text) })
            })
            .collect()
    }

    /// OpenLLMetry's OpenAI instrumentation: indexed prompt/completion
    /// attributes, the whole history on every call.
    fn openllmetry_calls() -> Vec<Span> {
        let sys = || [s("gen_ai.prompt.0.role", "system"), s("gen_ai.prompt.0.content", "You are a travel assistant.")];
        let common = || {
            vec![
                s("gen_ai.system", "openai"),
                s("gen_ai.request.model", "gpt-4o-mini"),
                s("gen_ai.response.model", "gpt-4o-mini-2024-07-18"),
                s("llm.request.type", "chat"),
            ]
        };
        let mut a = common();
        a.extend(sys());
        a.extend([
            s("gen_ai.prompt.1.role", "user"),
            s("gen_ai.prompt.1.content", "What is the capital of France?"),
            s("gen_ai.completion.0.role", "assistant"),
            s("gen_ai.completion.0.content", "Paris."),
            s("gen_ai.completion.0.finish_reason", "stop"),
            i("gen_ai.usage.prompt_tokens", 21),
            i("gen_ai.usage.completion_tokens", 2),
        ]);
        let mut b = common();
        b.extend(sys());
        b.extend([
            s("gen_ai.prompt.1.role", "user"),
            s("gen_ai.prompt.1.content", "What is the capital of France?"),
            s("gen_ai.prompt.2.role", "assistant"),
            s("gen_ai.prompt.2.content", "Paris."),
            s("gen_ai.prompt.3.role", "user"),
            s("gen_ai.prompt.3.content", "And Germany? Use the lookup tool."),
            s("gen_ai.completion.0.role", "assistant"),
            s("gen_ai.completion.0.tool_calls.0.id", "call_1"),
            s("gen_ai.completion.0.tool_calls.0.name", "lookup_capital"),
            s("gen_ai.completion.0.tool_calls.0.arguments", "{\"country\":\"Germany\"}"),
            s("gen_ai.completion.0.finish_reason", "tool_calls"),
        ]);
        let mut c = common();
        c.extend(sys());
        c.extend([
            s("gen_ai.prompt.1.role", "user"),
            s("gen_ai.prompt.1.content", "What is the capital of France?"),
            s("gen_ai.prompt.2.role", "assistant"),
            s("gen_ai.prompt.2.content", "Paris."),
            s("gen_ai.prompt.3.role", "user"),
            s("gen_ai.prompt.3.content", "And Germany? Use the lookup tool."),
            s("gen_ai.prompt.4.role", "assistant"),
            s("gen_ai.prompt.4.tool_calls.0.id", "call_1"),
            s("gen_ai.prompt.4.tool_calls.0.name", "lookup_capital"),
            s("gen_ai.prompt.4.tool_calls.0.arguments", "{\"country\":\"Germany\"}"),
            s("gen_ai.prompt.5.role", "tool"),
            s("gen_ai.prompt.5.tool_call_id", "call_1"),
            s("gen_ai.prompt.5.content", "Berlin"),
            s("gen_ai.prompt.6.role", "user"),
            s("gen_ai.prompt.6.content", "thanks"),
            s("gen_ai.prompt.7.role", "assistant"),
            s("gen_ai.prompt.7.content", "Berlin."),
            s("gen_ai.prompt.8.role", "user"),
            s("gen_ai.prompt.8.content", "thanks"),
            s("gen_ai.completion.0.role", "assistant"),
            s("gen_ai.completion.0.content", "Berlin."),
        ]);
        vec![span("t1", "a", a), span("t1", "b", b), span("t1", "c", c)]
    }

    #[test]
    fn a_history_sent_with_every_call_is_stored_once() {
        let (recs, n) = map_spans(&batch(openllmetry_calls()), &BTreeMap::new());
        assert_eq!((n.spans, n.genai_spans, n.unknown_spans), (3, 3, 0));
        let mut seen = Default::default();
        let got = stored(&recs, &mut seen);
        let want: Vec<(String, String)> = [
            ("system", "You are a travel assistant."),
            ("user_input", "What is the capital of France?"),
            ("assistant", "Paris."),
            ("user_input", "And Germany? Use the lookup tool."),
            ("tool_call", "lookup_capital:{\"country\":\"Germany\"}"),
            ("tool_result", "Berlin"),
            ("user_input", "thanks"),
            ("assistant", "Berlin."),
            ("user_input", "thanks"), // said twice: twice
            ("assistant", "Berlin."), // answered twice: twice
        ]
        .iter()
        .map(|(k, t)| (k.to_string(), t.to_string()))
        .collect();
        assert_eq!(got, want);
        let r = recs.iter().find(|r| r.kind == "assistant").unwrap();
        assert_eq!(r.writer, "otel:travel-bot");
        assert_eq!(r.meta["model"], "gpt-4o-mini-2024-07-18");
        assert_eq!(r.meta["provider"], "openai");
        assert_eq!(r.meta["usage.completion_tokens"], "2");
        assert_eq!(r.ts_us, 1_700_000_001_000_000);
        let u = recs.iter().find(|r| r.kind == "user_input").unwrap();
        assert!(!u.meta.contains_key("usage.prompt_tokens"), "usage rides on the outputs");
        assert_eq!(u.meta["from"], "caller");
        let res = recs.iter().find(|r| r.kind == "tool_result").unwrap();
        assert_eq!((res.call_id.as_str(), res.status.as_str()), ("call_1", "ok"));
        // the same calls again (an exporter retry): nothing new
        let (again, _) = map_spans(&batch(openllmetry_calls()), &BTreeMap::new());
        assert!(stored(&again, &mut seen).is_empty());
    }

    #[test]
    fn anthropic_content_blocks_and_current_conventions() {
        let anthropic = span(
            "t2",
            "a",
            vec![
                s("gen_ai.system", "Anthropic"),
                s("gen_ai.request.model", "claude-x"),
                s("gen_ai.prompt.0.role", "user"),
                s(
                    "gen_ai.prompt.0.content",
                    r#"[{"type":"text","text":"Summarize the zebra report"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGk="}}]"#,
                ),
                s("gen_ai.completion.0.role", "assistant"),
                s("gen_ai.completion.0.content", "The zebra report says stripes help."),
                i("gen_ai.usage.input_tokens", 50),
            ],
        );
        let current = span(
            "t3",
            "b",
            vec![
                s("gen_ai.operation.name", "chat"),
                s("gen_ai.provider.name", "openai"),
                s("gen_ai.conversation.id", "conv-7"),
                s("gen_ai.system_instructions", r#"[{"type":"text","content":"Be brief."}]"#),
                s(
                    "gen_ai.input.messages",
                    r#"[{"role":"user","parts":[{"type":"text","content":"Weather in Paris?"}]}]"#,
                ),
                s(
                    "gen_ai.output.messages",
                    r#"[{"role":"assistant","parts":[{"type":"reasoning","content":"Need the tool."},{"type":"tool_call","id":"call_w","name":"get_weather","arguments":{"city":"Paris"}}],"finish_reason":"tool_call"}]"#,
                ),
            ],
        );
        let mut tool = span(
            "t3",
            "c",
            vec![
                s("gen_ai.operation.name", "execute_tool"),
                s("gen_ai.tool.name", "get_weather"),
                s("gen_ai.tool.call.id", "call_w"),
                s("gen_ai.tool.call.arguments", r#"{"city":"Paris"}"#),
                s("gen_ai.tool.call.result", "rainy, 14 C"),
            ],
        );
        tool.name = "execute_tool get_weather".into();
        let mut failing =
            span("t3", "d", vec![s("gen_ai.operation.name", "execute_tool"), s("gen_ai.tool.name", "get_tides")]);
        failing.status_code = 2;
        failing.status_message = "tide service unreachable".into();
        let (recs, n) = map_spans(&batch(vec![anthropic, current, tool, failing]), &BTreeMap::new());
        assert_eq!(n.genai_spans, 4);
        let mut seen = Default::default();
        let got = stored(&recs, &mut seen);
        assert_eq!(
            got,
            [
                ("user_input", "Summarize the zebra report\n[media:image:image/png]"),
                ("assistant", "The zebra report says stripes help."),
                ("system", "Be brief."),
                ("user_input", "Weather in Paris?"),
                ("thinking", "Need the tool."),
                ("tool_call", "get_weather:{\"city\":\"Paris\"}"),
                ("tool_result", "get_weather:rainy, 14 C"), // the call was stored with the chat span
                ("tool_call", "get_tides:{}"),
                ("tool_result", "get_tides:tide service unreachable"),
            ]
            .map(|(k, t)| (k.to_string(), t.to_string()))
        );
        let conv: Vec<&NewRecord> =
            recs.iter().filter(|r| r.meta.get("trace").map(String::as_str) == Some("t3")).collect();
        assert!(conv[..3].iter().all(|r| r.session == "otel:conv:conv-7"), "the conversation is the session");
        assert_eq!(recs.last().unwrap().status, "error");
    }

    #[test]
    fn message_events_on_spans_and_as_logs() {
        let ev = |name: &str, attrs: Vec<(String, Any)>| Event { time_ns: 5_000, name: name.into(), attrs, body: None };
        let mut sp = span("t4", "a", vec![s("gen_ai.system", "openai")]);
        sp.events = vec![
            ev("gen_ai.system.message", vec![s("content", "You sort socks.")]),
            ev("gen_ai.user.message", vec![s("gen_ai.event.content", r#"{"content":"Sort these socks"}"#)]),
            ev(
                "gen_ai.choice",
                vec![s(
                    "gen_ai.event.content",
                    r#"{"index":0,"finish_reason":"stop","message":{"content":"Sorted by color."}}"#,
                )],
            ),
        ];
        let (recs, n) = map_spans(&batch(vec![sp]), &BTreeMap::new());
        assert_eq!(n.no_content, 0);
        let kinds: Vec<(&str, &str)> = recs.iter().map(|r| (r.kind.as_str(), r.text.as_str())).collect();
        assert_eq!(
            kinds,
            [("system", "You sort socks."), ("user_input", "Sort these socks"), ("assistant", "Sorted by color.")]
        );
        // the official instrumentation's log events, one call per span id
        let log = |name: &str, body: Any, span: &str| Log {
            trace_id: "t5".into(),
            span_id: span.into(),
            event: Event {
                time_ns: 7_000,
                name: name.into(),
                attrs: vec![s("gen_ai.system", "openai")],
                body: Some(body),
            },
        };
        let m = |k: &str, v: &str| Any::Map(vec![(k.into(), Any::Str(v.into()))]);
        let logs = vec![Batch {
            resource: vec![s("service.name", "sock-bot")],
            scope: "opentelemetry.instrumentation.openai_v2".into(),
            items: vec![
                log("gen_ai.user.message", m("content", "Pair the socks"), "s1"),
                log(
                    "gen_ai.choice",
                    Any::Map(vec![("index".into(), Any::Int(0)), ("message".into(), m("content", "Paired."))]),
                    "s1",
                ),
                log("gen_ai.user.message", m("content", "Pair the socks"), "s2"),
                log("app.started", Any::Str("hello".into()), ""),
            ],
        }];
        let mut writers = BTreeMap::new();
        writers.insert("sock-bot".to_string(), "agent:socks".to_string());
        let (recs, n) = map_logs(&logs, &writers);
        assert_eq!((n.log_records, n.genai_events, n.unknown_logs), (4, 3, 1));
        assert!(recs.iter().all(|r| r.writer == "agent:socks" && r.via == "otel"));
        let mut seen = Default::default();
        assert_eq!(
            stored(&recs, &mut seen),
            [("user_input", "Pair the socks"), ("assistant", "Paired.")].map(|(k, t)| (k.to_string(), t.to_string())),
            "the second call (same trace) re-sends the first message"
        );
    }

    #[test]
    fn other_spans_are_counted_and_skipped() {
        let http = Span { name: "GET /health".into(), attrs: vec![s("http.method", "GET")], ..Span::default() };
        let embed = span("t6", "e", vec![s("gen_ai.operation.name", "embeddings"), s("gen_ai.request.model", "e-1")]);
        let (recs, n) = map_spans(&batch(vec![http, embed]), &BTreeMap::new());
        assert!(recs.is_empty());
        assert_eq!((n.spans, n.unknown_spans, n.genai_spans, n.no_content), (2, 1, 1, 1));
    }
}
