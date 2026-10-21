//! MCP server over stdio (`oojuro mcp`): tools `remember`, `search`, `get`,
//! `periods` and `forget` (JSON-RPC 2.0, one message per line, protocol 2025-06-18
//! and earlier). Each call goes to the recorder when it serves the journal,
//! else opens the journal for that call only.
//!
//! The server writes as one writer: `$OOJURO_WRITER`, else the recorder's
//! ID for the agent that started it (Claude Code: `claude-code:<project>`),
//! else `human:<user>`. `search` and `get` read the whole journal on this
//! machine; `forget` only touches this writer's records.
use crate::{
    DEFAULT_EXCERPT_BYTES, Result,
    client::Oojuro,
    config::Config,
    paths::Paths,
    periods::{By, Per, PeriodsArgs},
    timearg::{parse_duration, parse_time},
    types::{Filters, SearchArgs, Target},
    when,
};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct McpServer {
    pub paths: Paths,
    pub writer: String,
}

fn kinds_schema() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string", "enum": ["explicit", "user_input", "assistant", "thinking", "tool_call", "tool_result", "system"] },
        "description": "Only these kinds of records (default: all). `explicit` = remembered notes."
    })
}

/// The tool list (`tools/list`).
pub fn tools(writer: &str) -> Value {
    json!([
        {
            "name": "remember",
            "title": "Remember a note",
            "description": "Store text in your private Oojuro journal so this or a later session can find it again. \
                The text is kept exactly (known secret formats are masked) and is searchable at once. \
                With `key` it becomes the newest version of that key: search and get return only the newest version, \
                older versions stay readable in history. Returns the record id, version and time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "What to remember, verbatim. Any length up to 16 MiB." },
                    "key": { "type": "string", "description": "Optional stable name (e.g. `deploy-steps`): a new version replaces the old one in search." }
                },
                "required": ["text"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        },
        {
            "name": "search",
            "title": "Search the journal",
            "description": "Search everything the journal holds: what your sessions saw, thought and did (prompts, tool calls and results, \
                outputs) and remembered notes. Ranked by bits saved: records that share the query's rarer words rank first. \
                There is no small token window: paste a long query (whole error output, a file excerpt) up to the \
                max_query_bytes setting (64 KiB by default). Returns refs: id, writer, kind, time, score_bits, and an \
                exact excerpt with its byte range [start, end) in the record text. Call `get` with an id for the whole record.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for; longer, specific queries work best." },
                    "k": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Results to return (default 10)." },
                    "kinds": kinds_schema(),
                    "key": { "type": "string", "description": "Only the newest version of this remembered key." },
                    "writer": { "type": "string", "description": "Only this writer's records (default: every writer)." },
                    "from": { "type": "string", "description": "Only records at or after this time: RFC 3339, YYYY-MM-DD, or a duration ago like `7d`." },
                    "to": { "type": "string", "description": "Only records at or before this time." },
                    "as_of": { "type": "string", "description": "Search the journal as it was at this time (newest key versions as of then)." },
                    "when": { "type": "string", "description": "Only records in this period, in plain words: `yesterday`, `last tuesday afternoon`, `since monday`, or relative to an event found by search: `before \"deploy-prod\"`, `after the first \"failed test\"`, `the day \"build broke\"`, `around @4211`. The resolved interval comes back in `when`." },
                    "excerpt_bytes": { "type": "integer", "minimum": 16, "maximum": 65536, "description": "Excerpt size (default 320)." }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "get",
            "title": "Read a record",
            "description": "Read one record exactly, whole: by `id` (from search), or by `key` (the newest version of a \
                remembered key, optionally as of a time). Returns its text, writer, kind, time and metadata.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Record id." },
                    "key": { "type": "string", "description": "A remembered key (newest version)." },
                    "writer": { "type": "string", "description": format!("Writer of the key (default: {writer}).") },
                    "as_of": { "type": "string", "description": "The version current at this time." }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "periods",
            "title": "Find periods of activity",
            "description": "Find stretches of time in the journal by how the agents' activity looked, ranked, with the \
                agent instances involved. `by`: `errors` (failed tool calls, bits of surprise against this agent's usual \
                failure rate), `repeat` (stuck loops: the same call failing again; `include_waiting` adds polling), \
                `unusual` (unlike this agent's last 7 days: rate, tool mix, errors, new tools, record size), `volume` \
                (most records), or `text` (periods mentioning `query`). Periods group an agent with the subagents it \
                spawned (`per`: task, session, writer). Each result has its time range, scores in bits, counts, tools, \
                and sample record ids: open them with `get`, or search inside with `search` and `from`/`to`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "by": { "type": "string", "enum": ["errors", "repeat", "unusual", "volume", "text"], "description": "Ranking (default: `text` with a query, else `volume`)." },
                    "query": { "type": "string", "description": "Only periods whose records match this text." },
                    "when": { "type": "string", "description": "The period to look in (default: the last 7 days): `yesterday`, `last week`, `since monday`, `after the first \"failed test\"`." },
                    "per": { "type": "string", "enum": ["task", "session", "writer"], "description": "Group by task tree (an agent and its subagents; default), one agent instance, or the whole writer." },
                    "window": { "type": "string", "description": "Window length: `5m` (default for errors and repeat), `1h` (otherwise), `1d`." },
                    "writer": { "type": "string", "description": "Only this writer's records (default: every writer)." },
                    "include_waiting": { "type": "boolean", "description": "`repeat`: count waiting/polling loops too." },
                    "k": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Periods to return (default 10)." },
                    "samples": { "type": "integer", "minimum": 0, "maximum": 5, "description": "Sample record ids per period (default 3)." }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "forget",
            "title": "Forget records (permanent)",
            "description": format!("Permanently delete records of this writer ({writer}) and prove it. Give exactly one of: \
                `id`, `key` (every version), `older_than` (e.g. `30d`: everything older), `last` (e.g. `1h`: everything \
                from the last hour). The engine rewrites the affected files without the records, then decodes every \
                row and reads every file looking for their text; `proof.proven` says whether it is gone. Cannot be undone."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "One record." },
                    "key": { "type": "string", "description": "Every version of this remembered key." },
                    "older_than": { "type": "string", "description": "Everything before this time or duration ago (`30d`, `2026-01-01`)." },
                    "last": { "type": "string", "description": "Everything from this long ago until now (`1h`, `15m`)." }
                }
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }
    ])
}

fn s<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str).filter(|x| !x.is_empty())
}

fn time_arg(args: &Value, k: &str) -> Result<Option<i64>> {
    s(args, k).map(parse_time).transpose()
}

impl McpServer {
    pub fn new(paths: Paths, writer: String) -> Self {
        Self { paths, writer }
    }

    fn max_query_bytes(&self) -> usize {
        self.settings().map(|s| s.max_query_bytes as usize).unwrap_or(65_536)
    }

    fn settings(&self) -> Option<crate::Settings> {
        let dir = std::env::current_dir().unwrap_or_default();
        Config::discover(&self.paths.config, &dir).and_then(|c| c.resolve(Some(&self.writer))).ok().map(|r| r.settings)
    }

    fn tz(&self) -> Option<String> {
        self.settings().map(|s| s.timezone).filter(|z| !z.is_empty())
    }

    /// Run one tool; the value is the tool's structured result.
    pub fn call(&self, name: &str, args: &Value) -> Result<Value> {
        let oj = Oojuro::open(self.paths.clone());
        match name {
            "remember" => {
                let text = args.get("text").and_then(Value::as_str).ok_or("`text` is required")?;
                Ok(serde_json::to_value(oj.remember(&self.writer, text, s(args, "key"), None)?)?)
            }
            "search" => {
                let query = s(args, "query").ok_or("`query` is required")?;
                let k = args.get("k").and_then(Value::as_u64).unwrap_or(10).clamp(1, 100) as usize;
                let kinds: Vec<String> = args
                    .get("kinds")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let filters = Filters {
                    from_us: time_arg(args, "from")?,
                    to_us: time_arg(args, "to")?,
                    as_of_us: time_arg(args, "as_of")?,
                    kinds,
                    key: s(args, "key").map(str::to_string),
                    writers: s(args, "writer").map(|w| vec![w.to_string()]).unwrap_or_default(),
                    outside_context_of: None,
                };
                let excerpt_bytes = args
                    .get("excerpt_bytes")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_EXCERPT_BYTES, |x| x.clamp(16, 65_536) as usize);
                let mut filters = filters;
                let w = match s(args, "when") {
                    Some(p) => {
                        let scope =
                            when::Scope { writers: filters.writers.clone(), tz: self.tz(), ..when::Scope::default() };
                        let w = oj.when(p, &scope)?;
                        when::narrow(&mut filters, &w);
                        Some(w)
                    }
                    None => None,
                };
                let refs = oj.search(&SearchArgs {
                    query: query.into(),
                    k,
                    filters,
                    excerpt_bytes,
                    max_query_bytes: self.max_query_bytes(),
                })?;
                Ok(match w {
                    Some(w) => json!({ "when": w, "results": refs }),
                    None => json!({ "results": refs }),
                })
            }
            "periods" => {
                let a = PeriodsArgs {
                    query: s(args, "query").map(str::to_string),
                    by: s(args, "by").map(By::parse).transpose()?,
                    per: s(args, "per").map(Per::parse).transpose()?.unwrap_or_default(),
                    window_us: s(args, "window").map(parse_duration).transpose()?,
                    from_us: None,
                    to_us: None,
                    when: s(args, "when").map(str::to_string),
                    writers: s(args, "writer").map(|w| vec![w.to_string()]).unwrap_or_default(),
                    k: args.get("k").and_then(Value::as_u64).unwrap_or(10).clamp(1, 50) as usize,
                    samples: args.get("samples").and_then(Value::as_u64).unwrap_or(3).min(5) as usize,
                    include_waiting: args.get("include_waiting").and_then(Value::as_bool).unwrap_or(false),
                    outside_context_of: None,
                    tz: self.tz(),
                    min_bits: self.settings().map_or(8.0, |s| s.min_bits),
                    max_query_bytes: self.max_query_bytes(),
                };
                Ok(serde_json::to_value(oj.periods(&a)?)?)
            }
            "get" => {
                let rec = match (args.get("id").and_then(Value::as_u64), s(args, "key")) {
                    (Some(id), None) => oj.get_id(id)?,
                    (None, Some(key)) => {
                        oj.get_key(s(args, "writer").unwrap_or(&self.writer), key, time_arg(args, "as_of")?)?
                    }
                    _ => return Err("give exactly one of `id` or `key`".into()),
                };
                Ok(json!({ "record": rec }))
            }
            "forget" => {
                let given: Vec<&str> = ["id", "key", "older_than", "last"]
                    .into_iter()
                    .filter(|k| args.get(*k).is_some_and(|v| !v.is_null()))
                    .collect();
                if given.len() != 1 {
                    return Err("give exactly one of `id`, `key`, `older_than`, `last`".into());
                }
                let w = Some(self.writer.clone());
                let target = match given[0] {
                    "id" => {
                        let id = args["id"].as_u64().ok_or("`id` must be an integer")?;
                        match oj.get_id(id)? {
                            Some(r) if r.writer == self.writer => Target::Id { id },
                            Some(r) => {
                                return Err(format!("record {id} belongs to {}; this server forgets only {}'s records (use the oojuro CLI)", r.writer, self.writer).into());
                            }
                            None => return Err(format!("no record {id}").into()),
                        }
                    }
                    "key" => Target::Key {
                        key: s(args, "key").ok_or("`key` must be a string")?.into(),
                        writer: self.writer.clone(),
                    },
                    "older_than" => Target::OlderThan {
                        before_us: parse_time(s(args, "older_than").ok_or("`older_than` must be a string")?)?,
                        writer: w,
                    },
                    _ => {
                        let d = parse_duration(s(args, "last").ok_or("`last` must be a duration like 1h")?)?;
                        let now = oojuro_capture::time::now_us();
                        Target::Window { from_us: now - d, to_us: i64::MAX / 2, writer: w }
                    }
                };
                Ok(serde_json::to_value(oj.forget(&target, true)?)?)
            }
            other => Err(format!("unknown tool {other:?}").into()),
        }
    }

    /// Answer one JSON-RPC message (`None` for notifications and responses).
    pub fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = id?; // a notification: no answer
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let ok = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let err = |code: i64, message: String| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
                ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "oojuro", "title": "Oojuro journal", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": format!("Oojuro is this machine's journal of what agents saw, thought and did. \
                        Use `search` before re-deriving something you may have seen before, `remember` to keep a note \
                        (a `key` makes it versioned), `get` for a whole record, `periods` to find when agents \
                        failed, looped or worked unusually, `forget` to delete for good. Writer: {}.", self.writer)
                }))
            }
            "ping" => ok(json!({})),
            "tools/list" => ok(json!({ "tools": tools(&self.writer) })),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(err(-32602, "tools/call needs `name`".into()));
                };
                if !["remember", "search", "get", "periods", "forget"].contains(&name) {
                    return Some(err(-32602, format!("unknown tool {name:?}")));
                }
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.call(name, &args) {
                    Ok(v) => ok(json!({
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                        "structuredContent": v,
                        "isError": false
                    })),
                    Err(e) => {
                        ok(json!({ "content": [{ "type": "text", "text": format!("error: {e}") }], "isError": true }))
                    }
                }
            }
            m => err(-32601, format!("method not found: {m}")),
        })
    }

    /// Serve until `input` ends.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) if msg.is_object() => self.handle(&msg),
                Ok(_) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "expected one JSON-RPC object per line" } }),
                ),
                Err(e) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }),
                ),
            };
            if let Some(r) = reply {
                serde_json::to_writer(&mut output, &r)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}
