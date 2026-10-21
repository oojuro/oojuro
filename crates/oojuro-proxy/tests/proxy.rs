//! The recording proxy against a local mock upstream (synthetic data only;
//! no real model API is called).
mod common;

use common::*;
use http::HeaderMap;
use http_body_util::{BodyExt, Full};
use oojuro_capture::kind;
use oojuro_proxy::{C, DirectRec, Listener};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const SYSTEM: &str = "You are a careful test agent. Answer briefly.";

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn anthropic_body(msgs: &[Value], stream: bool) -> Vec<u8> {
    json!({
        "model": "claude-mock-1",
        "max_tokens": 1024,
        "stream": stream,
        "metadata": {"user_id": "user_x_account_y_session_5f1c2d3e-aaaa-bbbb-cccc-000000000001"},
        "system": [{"type": "text", "text": SYSTEM, "cache_control": {"type": "ephemeral"}}],
        "tools": [{"name": "Read", "description": "read a file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}}],
        "messages": msgs,
    })
    .to_string()
    .into_bytes()
}

fn user(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "text", "text": text}]})
}

const ANT: &[(&str, &str)] = &[("content-type", "application/json"), ("anthropic-version", "2023-06-01")];

fn hdrs<'a>(base: &[(&'a str, &'a str)], more: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    base.iter().chain(more).copied().collect()
}

fn by_kind(recs: &[DirectRec]) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for r in recs {
        *m.entry(kind::name(r.kind)).or_default() += 1;
    }
    m
}

fn meta<'a>(r: &'a DirectRec, k: &str) -> Option<&'a Value> {
    r.meta.get(k)
}

#[test]
fn anthropic_plain_and_streaming() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    // streaming
    let body = anthropic_body(&[user("hello there")], true);
    let r = rt.block_on(call(&c, addr, "/v1/messages", &hdrs(ANT, &[("x-mock", "anthropic-sse")]), body));
    assert_eq!(r.status, 200);
    assert_eq!(r.body, anthropic_sse_chunks("hello there").concat().as_bytes());
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1);
    let recs = sink.records();
    let k = by_kind(&recs);
    assert_eq!(k["system"], 2, "system prompt and tool definitions: {recs:#?}");
    assert_eq!((k["user_input"], k["thinking"], k["assistant"]), (1, 1, 1));
    assert_eq!(texts(&recs, "assistant"), vec!["answer to: hello there"]);
    assert_eq!(texts(&recs, "thinking"), vec!["thinking about hello there"]);
    let a = recs.iter().find(|r| r.kind == kind::THINKING).unwrap();
    assert_eq!(meta(a, "model"), Some(&json!("claude-mock-1")));
    assert_eq!(meta(a, "usage"), Some(&json!({"in": 21, "out": 17, "cache_read": 5})));
    assert_eq!((a.tokens_in, a.tokens_out), (21, 17));
    assert_eq!(meta(a, "rid"), Some(&json!("req_mock_1")));
    assert_eq!(meta(a, "stop"), Some(&json!("end_turn")));
    assert!(recs.iter().all(|r| r.writer == "proxy:unknown"));
    let u = recs.iter().find(|r| r.kind == kind::USER_INPUT).unwrap();
    assert!(u.ts_us < a.ts_us && a.duration_us > 0);
    // plain JSON, with a tool call whose input is kept verbatim
    let body = anthropic_body(
        &[
            user("hello there"),
            json!({"role":"assistant","content":[{"type":"thinking","thinking":"thinking about hello there","signature":"sig"},{"type":"text","text":"answer to: hello there"}]}),
            user("list files"),
        ],
        false,
    );
    let r = rt.block_on(call(&c, addr, "/v1/messages", &hdrs(ANT, &[("x-mock", "anthropic-json")]), body));
    assert_eq!(r.status, 200);
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 2);
    let b = sink.batches.lock().unwrap()[1].records.clone();
    // only the new user message and the outputs: the re-sent history is not stored again
    assert_eq!(
        b.iter().map(|r| (kind::name(r.kind), r.text.as_str())).collect::<Vec<_>>(),
        vec![
            ("user_input", "list files"),
            ("thinking", "thinking about list files"),
            ("assistant", "answer to: list files"),
            ("tool_call", r#"{"command": "ls -la", "description": "list"}"#),
        ]
    );
    assert_eq!(b[3].tool, "Bash");
    assert_eq!(b[3].call_id, "toolu_j");
    assert_eq!(b[1].meta.get("rid"), Some(&json!("req_mock_2")));
    assert_eq!(b[0].turn, 2);
    assert_eq!(proxy.counter(C::RecordsDup), 5);
    proxy.shutdown(Duration::from_secs(5));
}

#[test]
fn openai_chat_stream_with_tool_calls() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    let req = json!({"model":"gpt-mock","stream":true,"stream_options":{"include_usage":true},
        "tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object"}}}],
        "messages":[{"role":"system","content":"You are a weather bot."},{"role":"user","content":"Weather in Paris and Oslo?"}]});
    let h = [
        ("content-type", "application/json"),
        ("authorization", "Bearer sk-test"),
        ("x-mock", "chat-sse"),
        ("user-agent", "OpenAI/Python 1.99.0"),
    ];
    let r = rt.block_on(call(&c, addr, "/v1/chat/completions", &h, req.to_string()));
    assert_eq!(r.status, 200);
    assert_eq!(r.body, chat_sse_chunks().concat().as_bytes());
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1);
    let recs = sink.records();
    assert!(recs.iter().all(|r| r.writer == "proxy:openai"));
    assert_eq!(texts(&recs, "system"), vec![req["tools"].to_string(), "You are a weather bot.".into()]);
    assert_eq!(texts(&recs, "user_input"), vec!["Weather in Paris and Oslo?"]);
    assert_eq!(texts(&recs, "thinking"), vec!["need the weather"]);
    assert_eq!(texts(&recs, "assistant"), vec!["Let me check both cities."]);
    let calls: Vec<(&str, &str, &str)> = recs
        .iter()
        .filter(|r| r.kind == kind::TOOL_CALL)
        .map(|r| (r.tool.as_str(), r.call_id.as_str(), r.text.as_str()))
        .collect();
    assert_eq!(
        calls,
        vec![("get_weather", "call_a", r#"{"city": "Paris"}"#), ("get_weather", "call_b", r#"{"city": "Oslo"}"#)]
    );
    let first_out = recs.iter().find(|r| r.kind == kind::THINKING).unwrap();
    assert_eq!(first_out.meta.get("usage"), Some(&json!({"in": 40, "out": 22, "cache_read": 8})));
    assert_eq!(first_out.meta.get("stop"), Some(&json!("tool_calls")));
    // next turn: the client sends the calls back (re-encoded) with their results
    let mut msgs = req["messages"].as_array().unwrap().clone();
    msgs.push(json!({"role":"assistant","content":"Let me check both cities.","reasoning_content":"need the weather","tool_calls":[
        {"id":"call_a","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}},
        {"id":"call_b","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}}]}));
    msgs.push(json!({"role":"tool","tool_call_id":"call_a","content":"18C"}));
    msgs.push(json!({"role":"tool","tool_call_id":"call_b","content":"9C"}));
    let req2 = json!({"model":"gpt-mock","stream":true,"tools":req["tools"],"messages":msgs});
    let r = rt.block_on(call(&c, addr, "/v1/chat/completions", &h, req2.to_string()));
    assert_eq!(r.status, 200);
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 2);
    let b = sink.batches.lock().unwrap()[1].records.clone();
    let results: Vec<(&str, &str)> =
        b.iter().filter(|r| r.kind == kind::TOOL_RESULT).map(|r| (r.call_id.as_str(), r.text.as_str())).collect();
    assert_eq!(results, vec![("call_a", "18C"), ("call_b", "9C")]);
    // nothing of the first call was stored again (the reply is the mock's same reply: identical
    // outputs at a new position in the conversation are new records)
    assert_eq!(b.iter().filter(|r| r.kind == kind::SYSTEM || r.kind == kind::USER_INPUT).count(), 0);
    proxy.shutdown(Duration::from_secs(5));
}

#[test]
fn responses_api_stream_and_plain() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    let req = json!({"model":"gpt-mock-r","stream":true,"instructions":"You are Codex.","store":false,
        "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}],
        "input":[{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions>sandboxed</permissions>"}]},
                 {"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd=/r</environment_context>"}]},
                 {"type":"message","role":"user","content":[{"type":"input_text","text":"how many files?"}]}]});
    let h = [
        ("content-type", "application/json"),
        ("x-mock", "responses-sse"),
        ("user-agent", "codex_cli_rs/0.50.0 (Linux)"),
    ];
    let r = rt.block_on(call(&c, addr, "/v1/responses", &h, req.to_string()));
    assert_eq!(r.body, responses_sse_chunks().concat().as_bytes());
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1);
    let recs = sink.records();
    assert!(recs.iter().all(|r| r.writer == "proxy:codex_cli_rs"));
    assert_eq!(
        recs.iter().map(|r| (kind::name(r.kind), r.text.as_str())).collect::<Vec<_>>(),
        vec![
            ("system", "You are Codex."),
            ("system", req["tools"].to_string().as_str()),
            ("system", "<permissions>sandboxed</permissions>"),
            ("system", "<environment_context>cwd=/r</environment_context>"),
            ("user_input", "how many files?"),
            ("thinking", "List the files first."),
            ("assistant", "Ran it: 3 files."),
            ("tool_call", r#"{"cmd": ["ls"]}"#),
        ]
    );
    assert_eq!(recs[5].meta.get("usage"), Some(&json!({"in": 50, "out": 9, "reasoning": 4})));
    assert_eq!(recs[5].meta.get("stop"), Some(&json!("completed")));
    // the next call sends the output items back (Codex: store=false, full history)
    let mut input = req["input"].as_array().unwrap().clone();
    input.extend(responses_sse_chunks().iter().filter(|c| c.starts_with("event: response.completed")).flat_map(|c| {
        let d: Value = serde_json::from_str(c.split("data: ").nth(1).unwrap().trim()).unwrap();
        d["response"]["output"].as_array().unwrap().clone()
    }));
    input.push(json!({"type":"function_call_output","call_id":"call_r1","output":"a\nb\nc"}));
    let req2 =
        json!({"model":"gpt-mock-r","stream":false,"instructions":"You are Codex.","tools":req["tools"],"input":input});
    let h2 = [
        ("content-type", "application/json"),
        ("x-mock", "responses-json"),
        ("user-agent", "codex_cli_rs/0.50.0 (Linux)"),
    ];
    let r = rt.block_on(call(&c, addr, "/v1/responses", &h2, req2.to_string()));
    assert_eq!(r.status, 200);
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 2);
    let b = sink.batches.lock().unwrap()[1].records.clone();
    assert_eq!(
        b.iter().map(|r| (kind::name(r.kind), r.text.as_str())).collect::<Vec<_>>(),
        vec![("tool_result", "a\nb\nc"), ("assistant", "plain answer")]
    );
    assert_eq!(b[0].call_id, "call_r1");
    proxy.shutdown(Duration::from_secs(5));
}

#[test]
fn upstream_errors_are_passed_and_journaled() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |cfg| {
        // a second listener whose upstream does not answer
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        cfg.listeners.push(Listener {
            listen: "127.0.0.1:0".parse().unwrap(),
            writer: Some("dead-upstream".into()),
            anthropic_upstream: format!("http://{dead}"),
            openai_upstream: format!("http://{dead}"),
        });
    });
    let (addr, dead_addr) = (proxy.local_addrs()[0], proxy.local_addrs()[1]);
    let c = client();
    for code in ["400", "429", "500", "529"] {
        let x = format!("status:{code}");
        let body = anthropic_body(&[user(&format!("question for {code}"))], true);
        let direct = rt.block_on(call(&c, mock.addr, "/v1/messages", &hdrs(ANT, &[("x-mock", &x)]), body.clone()));
        let r = rt.block_on(call(&c, addr, "/v1/messages", &hdrs(ANT, &[("x-mock", &x)]), body));
        assert_eq!(r.status.as_str(), code);
        assert_eq!(r.body, direct.body);
    }
    let r = rt.block_on(call(
        &c,
        addr,
        "/v1/chat/completions",
        &[("x-mock", "status:503")],
        r#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#,
    ));
    assert_eq!(r.status, 503);
    let r = rt.block_on(call(&c, dead_addr, "/v1/messages", ANT, anthropic_body(&[user("anyone there?")], true)));
    assert_eq!(r.status, 502);
    assert!(String::from_utf8_lossy(&r.body).contains("upstream unreachable"));
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 6);
    let recs = sink.records();
    let errors: Vec<&DirectRec> = recs.iter().filter(|r| r.meta.get("sub") == Some(&json!("upstream_error"))).collect();
    assert_eq!(errors.len(), 6);
    assert!(errors.iter().all(|e| e.status == oojuro_capture::status::ERROR && e.kind == kind::SYSTEM));
    let codes: Vec<Option<&Value>> = errors.iter().map(|e| e.meta.get("http")).collect();
    assert_eq!(
        codes,
        vec![Some(&json!(400)), Some(&json!(429)), Some(&json!(500)), Some(&json!(529)), Some(&json!(503)), None]
    );
    assert!(errors[3].text.starts_with("HTTP 529: ") && errors[3].text.contains("mock status 529"));
    assert!(errors[5].text.contains("upstream unreachable") && errors[5].writer == "dead-upstream");
    // what the agent sent is journaled when the call failed upstream (a call that never reached
    // the upstream did not send its body: its retry is journaled)
    assert_eq!(texts(&recs, "user_input").len(), 5);
    assert_eq!(proxy.counter(C::UpstreamErrorStatus), 5);
    assert_eq!(proxy.counter(C::UpstreamUnreachable), 1);
    proxy.shutdown(Duration::from_secs(5));
}

#[test]
fn client_disconnect_mid_stream() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    let got = rt.block_on(async {
        let req = http::Request::post(format!("http://{addr}/v1/messages"))
            .header("x-mock", "slow-sse")
            .header("anthropic-version", "2023-06-01")
            .body(Full::new(bytes::Bytes::from(anthropic_body(&[user("start a long answer")], true))))
            .unwrap();
        let resp = c.request(req).await.unwrap();
        let mut body = resp.into_body();
        let mut got = Vec::new();
        while !String::from_utf8_lossy(&got).contains("tick1 ") {
            let f = body.frame().await.unwrap().unwrap();
            if let Ok(d) = f.into_data() {
                got.extend_from_slice(&d);
            }
        }
        got // the body is dropped here: the agent hangs up mid-stream
    });
    assert!(String::from_utf8_lossy(&got).contains("tick"));
    drop(c);
    // the upstream sees the hang-up too (generation stops), and what arrived is journaled
    wait_until(Duration::from_secs(10), || mock.state.disconnects.load(Ordering::SeqCst) == 1);
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1);
    let recs = sink.records();
    let a = recs.iter().find(|r| r.kind == kind::ASSISTANT).expect("partial answer journaled");
    assert!(a.text.starts_with("tick0 tick1 "), "{}", a.text);
    assert_eq!(a.meta.get("partial"), Some(&json!("client_disconnect")));
    assert_eq!(texts(&recs, "user_input"), vec!["start a long answer"]);
    assert_eq!(proxy.counter(C::ClientDisconnects), 1);
    assert_eq!(proxy.active(), 0);
    proxy.shutdown(Duration::from_secs(5));
}

/// Response headers that must match, without the per-response date.
fn comparable(h: &HeaderMap) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> =
        h.iter().filter(|(k, _)| k.as_str() != "date").map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())).collect();
    v.sort();
    v
}

#[test]
fn byte_identical_passthrough_and_writer_identity() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |cfg| {
        cfg.listeners.push(Listener { writer: Some("named-listener".into()), ..cfg.listeners[0].clone() });
    });
    let (addr, named) = (proxy.local_addrs()[0], proxy.local_addrs()[1]);
    let c = client();
    for (path, scenario) in [
        ("/v1/messages", "anthropic-sse"),
        ("/v1/messages", "headers"),
        ("/v1/messages", "anthropic-sse-gzip"),
        ("/v1/chat/completions", "chat-sse"),
        ("/v1/models", "models"),
        ("/v1/messages/count_tokens", "headers"),
    ] {
        // odd spacing, escapes, unicode and key order in the request: forwarded byte for byte
        let body = format!(
            "{{ \"stream\":true,\"model\" : \"claude-mock-1\",\"max_tokens\":64,\"messages\":[{{\"role\":\"user\",\"content\":\"h\\u00e9llo  wörld \\ud83d\\ude00 {scenario}\"}}] }}"
        );
        let h = [
            ("content-type", "application/json"),
            ("anthropic-version", "2023-06-01"),
            ("anthropic-beta", "interleaved-thinking-2025-05-14"),
            ("x-api-key", "test-key-not-real"),
            ("accept-encoding", "gzip, br"),
            ("x-stainless-retry-count", "0"),
            ("x-mock", scenario),
            ("x-oojuro-writer", "writer-from-header"),
        ];
        let before = mock.received().len();
        let d = rt.block_on(call(&c, mock.addr, path, &h, body.clone()));
        let p = rt.block_on(call(&c, addr, path, &h, body.clone()));
        assert_eq!(d.status, p.status, "{scenario}");
        assert_eq!(d.body, p.body, "{scenario}: response body bytes");
        assert_eq!(comparable(&d.headers), comparable(&p.headers), "{scenario}: response headers");
        let rec = mock.received();
        let (rd, rp) = (&rec[before], &rec[before + 1]);
        assert_eq!(rd.body, rp.body, "{scenario}: request body bytes");
        assert_eq!(rp.body, body.as_bytes());
        assert_eq!((rd.method.as_str(), rd.path.as_str()), (rp.method.as_str(), rp.path.as_str()));
        let mut want = comparable(&rd.headers);
        want.retain(|(k, _)| !k.starts_with("x-oojuro-"));
        assert_eq!(want, comparable(&rp.headers), "{scenario}: request headers upstream");
        assert!(rp.headers.get("x-oojuro-writer").is_none());
    }
    // a GET on a recorded path and other paths are not journaled
    let r = rt.block_on(async {
        let req = http::Request::get(format!("http://{addr}/v1/models")).body(Full::new(bytes::Bytes::new())).unwrap();
        c.request(req).await.unwrap().status()
    });
    assert_eq!(r, 200);
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 4);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(proxy.counter(C::Recorded), 4);
    assert_eq!(proxy.counter(C::Passthrough), 3);
    let recs = sink.records();
    assert!(recs.iter().all(|r| r.writer == "writer-from-header"));
    // the gzip-encoded stream reached the agent as sent and the journal decoded it
    assert_eq!(texts(&recs, "assistant").iter().filter(|t| t.starts_with("answer to: h")).count(), 2);
    // writer: the listener's name, else the user-agent product
    let msg = anthropic_body(&[user("who am I")], true);
    rt.block_on(call(&c, named, "/v1/messages", &hdrs(ANT, &[("x-mock", "anthropic-sse")]), msg.clone()));
    rt.block_on(call(
        &c,
        addr,
        "/v1/messages",
        &hdrs(ANT, &[("x-mock", "anthropic-sse"), ("user-agent", "claude-cli/2.1.3 (external, cli)")]),
        msg,
    ));
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 6);
    let b = sink.batches.lock().unwrap();
    assert!(b[4].records.iter().all(|r| r.writer == "named-listener"));
    assert!(b[5].records.iter().all(|r| r.writer == "proxy:claude-cli"));
    drop(b);
    proxy.shutdown(Duration::from_secs(5));
}

/// A Claude Code-like loop: every call re-sends the whole history (with the
/// cache marker moving to the newest message), some turns call a tool, and
/// "yes" is said twice. Every message must be stored exactly as many times
/// as it occurs in the conversation, and a restarted proxy remembers.
#[test]
fn history_resent_every_turn_is_stored_once() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let dir = scratch("dedupe");
    let seen = dir.join("seen.bin");
    let proxy = start_proxy(&mock, &sink, Some(seen.clone()), |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    let mut history: Vec<Value> = Vec::new();
    let mut pending: Option<String> = None;
    let mut expected: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
    let mut add = |k: &'static str, t: String| *expected.entry((k, t)).or_default() += 1;
    add("system", SYSTEM.into());
    // the tool definitions exactly as the client serialized them
    let sent: Value = serde_json::from_slice(&anthropic_body(&[], true)).unwrap();
    add("system", sent["tools"].to_string());
    let canon = |s: &str| serde_json::from_str::<Value>(s).unwrap().to_string();
    for turn in 1..=50 {
        let text = match turn {
            10 | 20 => "yes".to_string(),
            t if t % 7 == 0 => format!("turn {t}: read the file [call]"),
            t => format!("turn {t}: question {t}"),
        };
        for m in history.iter_mut().filter(|m| m["role"] == "user") {
            for b in m["content"].as_array_mut().unwrap() {
                b.as_object_mut().unwrap().remove("cache_control");
            }
        }
        let mut content = Vec::new();
        if let Some(id) = pending.take() {
            let out = format!("file contents for turn {turn}");
            add("tool_result", out.clone());
            content.push(json!({"type":"tool_result","tool_use_id":id,"content":out}));
        }
        content.push(json!({"type":"text","text":text,"cache_control":{"type":"ephemeral"}}));
        add("user_input", text.clone());
        history.push(json!({"role":"user","content":content}));
        let r = rt.block_on(call(
            &c,
            addr,
            "/v1/messages",
            &hdrs(ANT, &[("x-mock", "anthropic-sse-fast")]),
            anthropic_body(&history, true),
        ));
        assert_eq!(r.status, 200);
        let (thinking, answer, tool) = mock_answer(&text);
        add("thinking", thinking.clone());
        add("assistant", answer.clone());
        let mut ac = vec![
            json!({"type":"thinking","thinking":thinking,"signature":"sig"}),
            json!({"type":"text","text":answer}),
        ];
        if let Some((id, input)) = tool {
            add("tool_call", canon(&input));
            // the client re-encodes the input it parsed (spacing differs from the model's)
            ac.push(
                json!({"type":"tool_use","id":id,"name":"Read","input":serde_json::from_str::<Value>(&input).unwrap()}),
            );
            pending = Some(id);
        }
        history.push(json!({"role":"assistant","content":ac}));
    }
    wait_until(Duration::from_secs(30), || sink.batches.lock().unwrap().len() == 50);
    let recs = sink.records();
    let mut got: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
    for r in &recs {
        let t = if r.kind == kind::TOOL_CALL { canon(&r.text) } else { r.text.clone() };
        *got.entry((kind::name(r.kind), t)).or_default() += 1;
    }
    assert_eq!(got, expected);
    assert_eq!(recs.len(), 2 + 50 * 3 + 7 + 7);
    assert_eq!(expected[&("user_input", "yes".to_string())], 2);
    // each turn re-sends the system prompt, the tools and about 3 records per earlier turn
    assert!(proxy.counter(C::RecordsDup) > 3000, "{:?}", proxy.stats());
    proxy.shutdown(Duration::from_secs(5));

    // a restarted proxy (same seen file) stores only what is new
    let sink2 = MemSink::default();
    let proxy = start_proxy(&mock, &sink2, Some(seen.clone()), |_| {});
    let addr = proxy.local_addrs()[0];
    history.push(user("turn 51: one more"));
    rt.block_on(call(
        &c,
        addr,
        "/v1/messages",
        &hdrs(ANT, &[("x-mock", "anthropic-sse-fast")]),
        anthropic_body(&history, true),
    ));
    wait_until(Duration::from_secs(10), || sink2.batches.lock().unwrap().len() == 1);
    let r = sink2.records();
    // the tool result of turn 49's call was never sent; the new user message and the outputs are new
    assert_eq!(
        r.iter().map(|r| (kind::name(r.kind), r.text.as_str())).collect::<Vec<_>>(),
        vec![
            ("user_input", "turn 51: one more"),
            ("thinking", "thinking about turn 51: one more"),
            ("assistant", "answer to: turn 51: one more"),
        ]
    );
    proxy.shutdown(Duration::from_secs(5));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn secrets_masked_in_journal_forwarded_intact_no_credential_headers() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    // built at run time so no key-shaped literal sits in the source
    let secret_owned = ["sk-ant-", "api03-Zq8vT3xLm2Pk9Rw4Yb7Nc1Hd6Jf0Gs5Ue2Ia8Ko3Lp7Mq4Nr9Os1Pt6Qu0Rv5Sw2Tx8Uy3Vz-AbCdEfGhAA"].concat();
    let secret = secret_owned.as_str();
    let bearer_owned = ["Bearer ", "oat01-HEADER-ONLY-VALUE-7f3a9c"].concat();
    let bearer = bearer_owned.as_str();
    let api_key_owned = ["HEADER-ONLY-", "API-KEY-55e1d0"].concat();
    let api_key = api_key_owned.as_str();
    let text = format!("my key is {secret} please store it in .env");
    let h = hdrs(ANT, &[("x-mock", "anthropic-sse"), ("authorization", bearer), ("x-api-key", api_key)]);
    let r = rt.block_on(call(&c, addr, "/v1/messages", &h, anthropic_body(&[user(&text)], true)));
    assert_eq!(r.status, 200);
    // forwarded intact: body and credential headers
    let got = mock.received().pop().unwrap();
    assert!(String::from_utf8_lossy(&got.body).contains(secret));
    assert_eq!(got.headers["authorization"], bearer);
    assert_eq!(got.headers["x-api-key"], api_key);
    // the agent got the mock's echo of it unchanged too
    assert!(String::from_utf8_lossy(&r.body).contains(secret));
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1);
    let j = sink.json();
    assert!(!j.contains(secret), "secret stored");
    assert!(!j.contains("sk-ant-api03"), "secret prefix stored");
    assert!(j.contains("[REDACTED:anthropic:"));
    assert!(texts(&sink.records(), "user_input")[0].starts_with("my key is [REDACTED:anthropic:"));
    assert!(texts(&sink.records(), "assistant")[0].contains("[REDACTED:anthropic:"));
    for v in [bearer, api_key, "oat01", "HEADER-ONLY"] {
        assert!(!j.contains(v), "credential header {v} reached the journal");
    }
    assert!(proxy.counter(C::Redactions) >= 3);
    proxy.shutdown(Duration::from_secs(5));
}

#[test]
fn journal_down_or_slow_calls_still_succeed() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    sink.mode.store(FAIL, Ordering::SeqCst);
    let proxy = start_proxy(&mock, &sink, None, |cfg| cfg.queue_jobs = 2);
    let addr = proxy.local_addrs()[0];
    let c = client();
    let h = hdrs(ANT, &[("x-mock", "anthropic-sse-fast")]);
    // journal down: every call succeeds, the batches are counted as dropped
    let why = || format!("{:?} batches {}", proxy.stats(), sink.batches.lock().unwrap().len());
    let mut history = vec![user("first question")];
    for i in 0..3 {
        // one at a time: the queue holds 2 calls (a third arriving before the worker
        // takes the first is dropped as queue-full, which is not what this phase tests)
        wait_for(Duration::from_secs(10), || proxy.counter(C::DroppedJournal) == i, why);
        let r = rt.block_on(call(&c, addr, "/v1/messages", &h, anthropic_body(&history, true)));
        assert_eq!(r.status, 200);
        assert_eq!(
            r.body,
            anthropic_sse_chunks(history.last().unwrap()["content"][0]["text"].as_str().unwrap()).concat().as_bytes()
        );
        let t = format!("question {i}");
        history.push(json!({"role":"assistant","content":"ok"}));
        history.push(user(&t));
    }
    wait_for(Duration::from_secs(10), || proxy.counter(C::DroppedJournal) == 3, why);
    assert_eq!(proxy.counter(C::DroppedQueueFull), 0);
    assert_eq!(proxy.counter(C::BatchesWritten), 0);
    // journal back: the next call stores the history the failed calls could not
    sink.mode.store(OK, Ordering::SeqCst);
    rt.block_on(call(&c, addr, "/v1/messages", &h, anthropic_body(&history, true)));
    wait_for(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1, why);
    let u = texts(&sink.records(), "user_input");
    assert_eq!(u, vec!["first question", "question 0", "question 1", "question 2"]);
    // journal hanging: the queue (2 calls) fills, later calls are dropped, none waits
    sink.mode.store(HANG, Ordering::SeqCst);
    let t = Instant::now();
    for i in 0..10 {
        let r = rt.block_on(call(
            &c,
            addr,
            "/v1/messages",
            &h,
            anthropic_body(&[user(&format!("while hanging {i}"))], true),
        ));
        assert_eq!(r.status, 200);
    }
    assert!(t.elapsed() < Duration::from_secs(10), "calls waited for the journal: {:?}", t.elapsed());
    assert!(proxy.counter(C::DroppedQueueFull) >= 6, "{:?}", proxy.stats());
    sink.mode.store(OK, Ordering::SeqCst);
    let dropped = proxy.counter(C::DroppedQueueFull) as usize;
    wait_for(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 1 + 10 - dropped, why);
    proxy.shutdown(Duration::from_secs(5));
}

#[test]
fn thirty_two_parallel_streams() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    let t = Instant::now();
    let replies = rt.block_on(async {
        let mut tasks = Vec::new();
        for i in 0..32 {
            let c = c.clone();
            tasks.push(tokio::spawn(async move {
                let text = format!("parallel question {i}");
                let w = format!("agent-{}", i % 4);
                let h =
                    [("anthropic-version", "2023-06-01"), ("x-mock", "anthropic-sse"), ("x-oojuro-writer", w.as_str())];
                let r = call(&c, addr, "/v1/messages", &h, anthropic_body(&[user(&text)], true)).await;
                (text, r)
            }));
        }
        let mut out = Vec::new();
        for t in tasks {
            out.push(t.await.unwrap());
        }
        out
    });
    let wall = t.elapsed();
    for (text, r) in &replies {
        assert_eq!(r.status, 200);
        assert_eq!(r.body, anthropic_sse_chunks(text).concat().as_bytes());
    }
    wait_until(Duration::from_secs(20), || sink.batches.lock().unwrap().len() == 32);
    let recs = sink.records();
    let mut answers = texts(&recs, "assistant");
    answers.sort();
    let mut want: Vec<String> = (0..32).map(|i| format!("answer to: parallel question {i}")).collect();
    want.sort();
    assert_eq!(answers, want);
    // one system prompt per writer (4 writers), stored once each despite parallel calls
    assert_eq!(texts(&recs, "system").iter().filter(|t| *t == SYSTEM).count(), 4);
    assert_eq!(proxy.active(), 0);
    eprintln!("32 parallel streams: {wall:?} wall");
    proxy.shutdown(Duration::from_secs(5));
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

/// Time-to-first-byte overhead against the mock: run with
/// `cargo test --release -p oojuro-proxy --test proxy overhead -- --ignored --nocapture`.
#[test]
#[ignore]
fn overhead() {
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |_| {});
    let addr = proxy.local_addrs()[0];
    let c = client();
    let n = 400;
    let (mut direct, mut proxied, mut dtotal, mut ptotal) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut history = Vec::new();
    for i in 0..n {
        // a growing history like an agent's (up to ~100 KB of request)
        history.push(user(&format!("question {i}: {}", "context ".repeat(30))));
        let body = anthropic_body(&history, true);
        let h = hdrs(ANT, &[("x-mock", "anthropic-sse-fast")]);
        for (target, ttfb, total) in [(mock.addr, &mut direct, &mut dtotal), (addr, &mut proxied, &mut ptotal)] {
            let t = Instant::now();
            let r = rt.block_on(call(&c, target, "/v1/messages", &h, body.clone()));
            assert_eq!(r.status, 200);
            ttfb.push(r.ttfb.as_secs_f64() * 1e3);
            total.push(t.elapsed().as_secs_f64() * 1e3);
        }
        history.push(json!({"role":"assistant","content":"ok"}));
    }
    wait_until(Duration::from_secs(60), || sink.batches.lock().unwrap().len() == n);
    let mut d_minus: Vec<f64> = proxied.iter().zip(&direct).map(|(p, d)| p - d).collect();
    let mut t_minus: Vec<f64> = ptotal.iter().zip(&dtotal).map(|(p, d)| p - d).collect();
    eprintln!(
        "TTFB ms direct p50 {:.3} p99 {:.3} | proxied p50 {:.3} p99 {:.3} | overhead p50 {:.3} p90 {:.3} p99 {:.3}",
        percentile(&mut direct, 0.5),
        percentile(&mut direct, 0.99),
        percentile(&mut proxied, 0.5),
        percentile(&mut proxied, 0.99),
        percentile(&mut d_minus, 0.5),
        percentile(&mut d_minus, 0.9),
        percentile(&mut d_minus, 0.99),
    );
    eprintln!(
        "whole call ms overhead p50 {:.3} p90 {:.3} p99 {:.3}; request size last {} KB; journal {:?}",
        percentile(&mut t_minus, 0.5),
        percentile(&mut t_minus, 0.9),
        percentile(&mut t_minus, 0.99),
        anthropic_body(&history, true).len() / 1024,
        proxy.stats()
    );
    proxy.shutdown(Duration::from_secs(5));
}

/// The transcript recorder wins: calls from an agent whose transcripts are
/// tailed are forwarded unchanged and not recorded, whatever their writer
/// header; other agents are recorded; `record_tailed` turns it off.
#[test]
fn agents_whose_transcripts_are_tailed_are_forwarded_not_recorded() {
    use oojuro_proxy::Format;
    let rt = rt();
    let mock = mock(&rt);
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |cfg| cfg.tailed = vec![Format::ClaudeCode]);
    let addr = proxy.local_addrs()[0];
    let c = client();
    let claude_ua = ("user-agent", "claude-cli/2.1.3 (external, cli)");
    let ask = |h: &[(&str, &str)], text: &str| {
        let body = anthropic_body(&[user(text)], true);
        let all = hdrs(ANT, &[&[("x-mock", "anthropic-sse")], h].concat());
        let direct = rt.block_on(call(&c, mock.addr, "/v1/messages", &all, body.clone()));
        let r = rt.block_on(call(&c, addr, "/v1/messages", &all, body));
        assert_eq!((r.status, &r.body), (direct.status, &direct.body), "forwarded unchanged");
    };
    // Claude Code is tailed: not recorded, even with a writer header or only `x-app: cli`
    ask(&[claude_ua], "claude one");
    ask(&[claude_ua, ("x-oojuro-writer", "my-writer")], "claude two");
    ask(&[("x-app", "cli")], "claude three");
    // Codex and an SDK are not tailed: recorded
    ask(&[("user-agent", "codex_cli_rs/0.50.0 (Linux)")], "codex one");
    ask(&[("user-agent", "Anthropic/Python 0.60.0")], "sdk one");
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 2);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(proxy.counter(C::SkippedTailed), 3);
    assert_eq!((proxy.counter(C::Recorded), proxy.counter(C::Passthrough)), (2, 0));
    let users = texts(&sink.records(), "user_input");
    assert_eq!(users, vec!["codex one", "sdk one"]);
    // the recorder's watch list changes while running: Codex is tailed now, Claude Code is not
    proxy.set_tailed(&[Format::Codex]);
    assert_eq!(proxy.tailed(), vec![Format::Codex]);
    ask(&[("user-agent", "reqwest/0.12"), ("originator", "codex_exec")], "codex two");
    ask(&[claude_ua], "claude four");
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 3);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(proxy.counter(C::SkippedTailed), 4);
    assert_eq!(texts(&sink.records(), "user_input").last().unwrap(), "claude four");
    proxy.shutdown(Duration::from_secs(5));

    // record_tailed = true: everything is recorded
    let sink = MemSink::default();
    let proxy = start_proxy(&mock, &sink, None, |cfg| {
        cfg.tailed = Format::ALL.to_vec();
        cfg.record_tailed = true;
    });
    let addr = proxy.local_addrs()[0];
    for (ua, text) in [("claude-cli/2.1.3 (external, cli)", "kept claude"), ("codex-tui/0.60.1", "kept codex")] {
        let h = hdrs(ANT, &[("x-mock", "anthropic-sse"), ("user-agent", ua)]);
        assert_eq!(rt.block_on(call(&c, addr, "/v1/messages", &h, anthropic_body(&[user(text)], true))).status, 200);
    }
    wait_until(Duration::from_secs(10), || sink.batches.lock().unwrap().len() == 2);
    assert_eq!(proxy.counter(C::SkippedTailed), 0);
    assert_eq!(texts(&sink.records(), "user_input"), vec!["kept claude", "kept codex"]);
    proxy.shutdown(Duration::from_secs(5));
}
