//! MCP over stdio: initialize, tools/list, tools/call for every tool, and
//! JSON-RPC errors, against a real `oojuro mcp` process.
mod common;
use common::Scratch;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

struct Mcp {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn send(&mut self, v: &Value) {
        let mut line = serde_json::to_vec(v).unwrap();
        line.push(b'\n');
        self.stdin.write_all(&line).unwrap();
        self.stdin.flush().unwrap();
    }
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed");
        serde_json::from_str(&line).unwrap()
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let r = self.recv();
        assert_eq!(r["id"], id);
        assert_eq!(r["jsonrpc"], "2.0");
        r
    }
    fn tool(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.call("tools/call", json!({ "name": name, "arguments": args }));
        let res = &r["result"];
        let is_err = res["isError"].as_bool().unwrap();
        let text = res["content"][0]["text"].as_str().unwrap();
        assert_eq!(res["content"][0]["type"], "text");
        if is_err {
            (true, Value::String(text.to_string()))
        } else {
            let parsed: Value = serde_json::from_str(text).unwrap();
            assert_eq!(parsed, res["structuredContent"]);
            (false, parsed)
        }
    }
}

#[test]
fn mcp_initialize_list_and_call_every_tool() {
    let s = Scratch::new("mcp");
    let mut child = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .arg("mcp")
        .current_dir(&s.0)
        .env("OOJURO_HOME", &s.0)
        .env("OOJURO_WRITER", "agent:mcp-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut m =
        Mcp { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next: 0 };

    let init = m.call(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "oojuro");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["instructions"].as_str().unwrap().contains("agent:mcp-test"));
    m.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })); // no answer
    let old = m.call("initialize", json!({ "protocolVersion": "1999-01-01" }));
    assert_eq!(old["result"]["protocolVersion"], "2025-06-18", "unknown versions get the latest");
    assert_eq!(m.call("ping", json!({}))["result"], json!({}));

    let list = m.call("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["remember", "search", "get", "periods", "forget"]);
    for t in tools {
        assert!(t["description"].as_str().unwrap().len() > 80, "describe {}", t["name"]);
        assert_eq!(t["inputSchema"]["type"], "object");
    }
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["text"]));
    assert_eq!(tools[1]["inputSchema"]["required"], json!(["query"]));
    assert_eq!(tools[3]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[4]["annotations"]["destructiveHint"], true);

    // remember (twice under a key), search, get
    let (e, r1) = m.tool("remember", json!({ "text": "the staging DB lives in us-east-2", "key": "staging-db" }));
    assert!(!e, "{r1}");
    assert_eq!((r1["writer"].as_str(), r1["version"].as_u64()), (Some("agent:mcp-test"), Some(1)));
    let (_, r2) = m.tool("remember", json!({ "text": "the staging DB moved to eu-west-1", "key": "staging-db" }));
    assert_eq!(r2["version"], 2);
    let (_, other) = m.tool("remember", json!({ "text": "cron jobs run at 03:00 UTC" }));
    let (e, found) = m.tool("search", json!({ "query": "where is the staging DB", "k": 5 }));
    assert!(!e);
    let results = found["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["id"], r2["id"]);
    assert_eq!(results[0]["excerpt"], "the staging DB moved to eu-west-1");
    let (_, by_kind) = m.tool("search", json!({ "query": "staging cron", "kinds": ["tool_call"] }));
    assert!(by_kind["results"].as_array().unwrap().is_empty());
    let (_, g) = m.tool("get", json!({ "id": other["id"] }));
    assert_eq!(g["record"]["text"], "cron jobs run at 03:00 UTC");
    let (_, g) = m.tool("get", json!({ "key": "staging-db" }));
    assert_eq!(g["record"]["id"], r2["id"]);
    let (_, g) = m.tool("get", json!({ "key": "staging-db", "as_of": r1["ts"] }));
    assert_eq!(g["record"]["id"], r1["id"]);
    let (_, g) = m.tool("get", json!({ "id": 999 }));
    assert!(g["record"].is_null());

    // periods, and `when` on search (the resolved interval comes back)
    let (e, p) = m.tool("periods", json!({ "by": "volume", "when": "today", "per": "writer" }));
    assert!(!e, "{p}");
    assert_eq!(p["by"], "volume");
    assert_eq!(p["window"], "1h");
    assert_eq!(p["when"]["phrase"], "today");
    let n: u64 = p["periods"].as_array().unwrap().iter().map(|x| x["events"].as_u64().unwrap()).sum();
    assert_eq!(n, 3, "the three notes, all today");
    let (e, msg) = m.tool("periods", json!({ "by": "novelty" }));
    assert!(e && msg.as_str().unwrap().contains("novelty"), "{msg}");
    let (e, found) = m.tool("search", json!({ "query": "staging DB", "when": "today" }));
    assert!(!e && found["when"]["to_us"].as_i64().is_some(), "{found}");
    assert_eq!(found["results"].as_array().unwrap().len(), 1);
    let (_, found) = m.tool("search", json!({ "query": "staging DB", "when": "yesterday" }));
    assert!(found["results"].as_array().unwrap().is_empty());
    let (e, msg) = m.tool("search", json!({ "query": "staging DB", "when": "purple elephant" }));
    assert!(e && msg.as_str().unwrap().contains("not a date phrase"), "{msg}");

    // tool errors come back as isError results
    let (e, msg) = m.tool("get", json!({}));
    assert!(e && msg.as_str().unwrap().contains("exactly one"));
    let (e, _) = m.tool("search", json!({ "query": "x".repeat(70_000) }));
    assert!(e, "longer than max_query_bytes");
    let (e, _) = m.tool("forget", json!({ "id": 1, "key": "staging-db" }));
    assert!(e);

    // forget only this writer's records, with proof
    let human = {
        let mut j = oojuro::Journal::open(&s.0.join("journal")).unwrap();
        j.remember("human:ann", "ann's private note", None, None).unwrap().id
    };
    let (e, msg) = m.tool("forget", json!({ "id": human }));
    assert!(e && msg.as_str().unwrap().contains("belongs to human:ann"), "{msg}");
    let (e, f) = m.tool("forget", json!({ "key": "staging-db" }));
    assert!(!e, "{f}");
    assert_eq!(f["forgotten"], 2);
    assert_eq!(f["proof"]["proven"], true);
    let (_, f) = m.tool("forget", json!({ "last": "1h" }));
    assert_eq!(f["forgotten"], 1, "only this writer's cron note, not ann's");
    let (_, g) = m.tool("get", json!({ "id": human }));
    assert_eq!(g["record"]["text"], "ann's private note");

    // protocol errors
    let r = m.call("tools/call", json!({ "name": "drop_tables", "arguments": {} }));
    assert_eq!(r["error"]["code"], -32602);
    let r = m.call("resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    m.stdin.write_all(b"{not json\n").unwrap();
    m.stdin.flush().unwrap();
    assert_eq!(m.recv()["error"]["code"], -32700);

    drop(m.stdin);
    assert!(child.wait().unwrap().success());
}
