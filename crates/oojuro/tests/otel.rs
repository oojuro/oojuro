//! The OpenTelemetry receiver as exporters use it (a real `oojurod`, HTTP
//! over loopback, synthetic fixtures modelled on the GenAI conventions and
//! the Python OpenAI/Anthropic instrumentations): protobuf and JSON, gzip,
//! traces and logs, a history re-sent with every call stored once, unknown
//! spans counted, malformed and oversized bodies refused without harm, a
//! large batch, redaction.
mod common;
use common::Scratch;
use oojuro::otel::pb::{self, any_value::Value as V};
use prost::Message;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon {
    child: Child,
    addr: String,
}

impl Daemon {
    fn start(home: &Path, args: &[&str]) -> Self {
        let log = std::fs::File::create(home.join("daemon.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_oojurod"))
            .args(args)
            .args(["--poll-ms", "50", "--no-dedupe-stats"])
            .env("OOJURO_HOME", home)
            .env("HOME", home)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let start = Instant::now();
        let addr = loop {
            if let Some(h) = oojuro::client::hello(&home.join("oojurod.sock")) {
                break h["otel"].as_str().expect("receiver address in hello").to_string();
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "daemon did not start: {}",
                std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        Daemon { child, addr }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: plain signal to our own child.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One HTTP request on its own connection: (status, body).
fn http(addr: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length")) {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let _ = s.write_all(body); // the server may answer (413) before reading it all
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    let status = text.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").map_or(out.len(), |p| p + 4);
    (status, out[at..].to_vec())
}

fn post(addr: &str, path: &str, ctype: &str, body: &[u8]) -> (u16, Vec<u8>) {
    http(addr, "POST", path, &[("Content-Type", ctype)], body)
}

const PB: &str = "application/x-protobuf";
const JS: &str = "application/json";

fn kv(k: &str, v: V) -> pb::KeyValue {
    pb::KeyValue { key: k.into(), value: Some(pb::AnyValue { value: Some(v) }) }
}

fn st(k: &str, v: &str) -> pb::KeyValue {
    kv(k, V::StringValue(v.into()))
}

fn pb_span(trace: u8, id: u8, name: &str, attrs: Vec<pb::KeyValue>) -> pb::Span {
    pb::Span {
        trace_id: vec![trace; 16],
        span_id: vec![id; 8],
        name: name.into(),
        kind: 3,
        start_time_unix_nano: 1_760_000_000_000_000_000 + u64::from(id) * 1_000_000_000,
        end_time_unix_nano: 1_760_000_000_500_000_000 + u64::from(id) * 1_000_000_000,
        attributes: attrs,
        ..pb::Span::default()
    }
}

fn request(service: &str, spans: Vec<pb::Span>) -> pb::ExportTraceServiceRequest {
    pb::ExportTraceServiceRequest {
        resource_spans: vec![pb::ResourceSpans {
            resource: Some(pb::Resource { attributes: vec![st("service.name", service)] }),
            scope_spans: vec![pb::ScopeSpans {
                scope: Some(pb::InstrumentationScope {
                    name: "opentelemetry.instrumentation.openai".into(),
                    version: "0.40".into(),
                }),
                spans,
            }],
        }],
    }
}

/// Two calls of an OpenLLMetry-instrumented OpenAI chat (the second
/// re-sends the history), one HTTP server span, one database span.
fn openai_chat(secret: &str) -> pb::ExportTraceServiceRequest {
    let common = || {
        vec![
            st("gen_ai.system", "openai"),
            st("gen_ai.request.model", "gpt-4o-mini"),
            st("llm.request.type", "chat"),
            st("gen_ai.prompt.0.role", "system"),
            st("gen_ai.prompt.0.content", "You plan zebra migrations."),
            st("gen_ai.prompt.1.role", "user"),
            st("gen_ai.prompt.1.content", &format!("Plan the zebra migration; the API key is {secret}")),
        ]
    };
    let mut first = common();
    first.extend([
        st("gen_ai.completion.0.role", "assistant"),
        st("gen_ai.completion.0.content", "Move the herd north in March."),
        kv("gen_ai.usage.prompt_tokens", V::IntValue(40)),
        kv("gen_ai.usage.completion_tokens", V::IntValue(9)),
    ]);
    let mut second = common();
    second.extend([
        st("gen_ai.prompt.2.role", "assistant"),
        st("gen_ai.prompt.2.content", "Move the herd north in March."),
        st("gen_ai.prompt.3.role", "user"),
        st("gen_ai.prompt.3.content", "Why March?"),
        st("gen_ai.completion.0.role", "assistant"),
        st("gen_ai.completion.0.content", "The rains start in April."),
    ]);
    request(
        "travel-bot",
        vec![
            pb_span(1, 1, "openai.chat", first),
            pb_span(1, 2, "openai.chat", second),
            pb_span(1, 3, "GET /plan", vec![st("http.request.method", "GET")]),
            pb_span(1, 4, "SELECT herds", vec![st("db.system", "postgresql")]),
        ],
    )
}

/// Current conventions in OTLP/JSON: a chat call with a tool call and the
/// tool's execution span; 64-bit integers as strings, ids in hex.
fn current_json() -> Value {
    let attr = |k: &str, v: &str| json!({ "key": k, "value": { "stringValue": v } });
    json!({ "resourceSpans": [{
        "resource": { "attributes": [attr("service.name", "weather-agent")] },
        "scopeSpans": [{ "scope": { "name": "agent-framework" }, "spans": [
            { "traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b174", "name": "chat gpt-4o",
              "kind": 3, "startTimeUnixNano": "1760000100000000000", "endTimeUnixNano": "1760000101000000000",
              "attributes": [
                attr("gen_ai.operation.name", "chat"), attr("gen_ai.provider.name", "openai"),
                attr("gen_ai.request.model", "gpt-4o"), attr("gen_ai.conversation.id", "conv-42"),
                attr("gen_ai.input.messages", r#"[{"role":"user","parts":[{"type":"text","content":"Is it raining in Lisbon?"}]}]"#),
                attr("gen_ai.output.messages", r#"[{"role":"assistant","parts":[{"type":"tool_call","id":"call_lx","name":"get_weather","arguments":{"city":"Lisbon"}}],"finish_reason":"tool_call"}]"#),
                { "key": "gen_ai.usage.input_tokens", "value": { "intValue": "31" } }
              ] },
            { "traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b175", "parentSpanId": "eee19b7ec3c1b174",
              "name": "execute_tool get_weather", "kind": 1,
              "startTimeUnixNano": "1760000101100000000", "endTimeUnixNano": "1760000101200000000",
              "attributes": [
                attr("gen_ai.operation.name", "execute_tool"), attr("gen_ai.tool.name", "get_weather"),
                attr("gen_ai.tool.call.id", "call_lx"), attr("gen_ai.tool.call.arguments", r#"{"city":"Lisbon"}"#),
                attr("gen_ai.tool.call.result", "light rain, 17 C")
              ] }
        ] }]
    }] })
}

/// The official OpenAI instrumentation's message events, sent as logs.
fn log_events_json() -> Value {
    let rec = |name: &str, body: Value, span: &str| {
        json!({ "timeUnixNano": "1760000200000000000", "traceId": "0af7651916cd43dd8448eb211c80319c", "spanId": span,
                "eventName": name, "attributes": [{ "key": "gen_ai.system", "value": { "stringValue": "openai" } }],
                "body": body })
    };
    let content =
        |c: &str| json!({ "kvlistValue": { "values": [{ "key": "content", "value": { "stringValue": c } }] } });
    json!({ "resourceLogs": [{
        "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": "sock-bot" } }] },
        "scopeLogs": [{ "scope": { "name": "opentelemetry.instrumentation.openai_v2" }, "logRecords": [
            rec("gen_ai.user.message", content("Pair the striped socks"), "b7ad6b7169203331"),
            rec("gen_ai.choice", json!({ "kvlistValue": { "values": [
                { "key": "index", "value": { "intValue": "0" } },
                { "key": "message", "value": { "kvlistValue": { "values": [{ "key": "content", "value": { "stringValue": "Paired 4 striped socks." } }] } } }
            ] } }), "b7ad6b7169203331"),
            { "timeUnixNano": "1760000200000000000", "body": { "stringValue": "worker started" } }
        ] }]
    }] })
}

fn oojuro(home: &Path, args: &[&str]) -> Value {
    let o = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .arg("--json")
        .args(args)
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn texts(home: &Path, writer: &str) -> Vec<(String, String)> {
    let rs = oojuro(home, &["range", "--writer", writer, "--limit", "100000"]);
    rs.as_array()
        .unwrap()
        .iter()
        .map(|r| (r["kind"].as_str().unwrap().into(), r["text"].as_str().unwrap().into()))
        .collect()
}

fn stats(addr: &str) -> Value {
    let (status, body) = http(addr, "GET", "/", &[], b"");
    assert_eq!(status, 200);
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn the_receiver_stores_genai_spans_once_and_refuses_bad_bodies() {
    let s = Scratch::new("ot");
    let home = &s.0;
    std::fs::write(
        home.join("config.toml"),
        "[otel]\nmax_body_bytes = 4194304\n[otel.writers]\n\"sock-bot\" = \"agent:socks\"\n",
    )
    .unwrap();
    let d = Daemon::start(home, &["otel", "--otel-listen", "127.0.0.1:0"]);
    let secret = format!("sk-ant-api03-{}", "Zx9".repeat(30));

    // protobuf: two calls, the history re-sent; two non-GenAI spans
    let body = openai_chat(&secret).encode_to_vec();
    let (status, answer) = post(&d.addr, "/v1/traces", PB, &body);
    assert_eq!(status, 200);
    assert!(pb::ExportTraceServiceResponse::decode(&answer[..]).is_ok(), "an ExportTraceServiceResponse");
    let want = [
        ("system", "You plan zebra migrations."),
        ("user_input", "Plan the zebra migration; the API key is [REDACTED:anthropic:"),
        ("assistant", "Move the herd north in March."),
        ("user_input", "Why March?"),
        ("assistant", "The rains start in April."),
    ];
    let got = texts(home, "otel:travel-bot");
    assert_eq!(got.len(), want.len(), "{got:#?}");
    for ((k, t), (wk, wt)) in got.iter().zip(want) {
        assert_eq!(k, wk);
        assert!(t.starts_with(wt), "{t} vs {wt}");
    }
    assert!(got.iter().all(|(_, t)| !t.contains(&secret)), "redacted before storing");
    let n = stats(&d.addr);
    assert_eq!(
        (n["spans"].as_u64(), n["genai_spans"].as_u64(), n["unknown_spans"].as_u64()),
        (Some(4), Some(2), Some(2))
    );
    // the exporter retries: nothing new
    assert_eq!(post(&d.addr, "/v1/traces", PB, &body).0, 200);
    assert_eq!(texts(home, "otel:travel-bot").len(), 5);

    // JSON (current conventions, tool execution), gzip-compressed
    let raw = current_json().to_string();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(raw.as_bytes()).unwrap();
    let gz = gz.finish().unwrap();
    let (status, answer) =
        http(&d.addr, "POST", "/v1/traces", &[("Content-Type", JS), ("Content-Encoding", "gzip")], &gz);
    assert_eq!((status, answer.as_slice()), (200, &b"{}"[..]));
    let got = texts(home, "otel:weather-agent");
    let kinds: Vec<&str> = got.iter().map(|x| x.0.as_str()).collect();
    assert_eq!(kinds, ["user_input", "tool_call", "tool_result"], "{got:#?}");
    assert_eq!(got[2].1, "light rain, 17 C");
    let rs = oojuro(home, &["range", "--writer", "otel:weather-agent"]);
    assert_eq!(rs[0]["meta"]["provider"], "openai");
    assert_eq!(rs[1]["meta"]["usage.input_tokens"], "31");
    assert_eq!(rs[2]["meta"]["via"], "otel");

    // logs: message events of the official instrumentation, mapped writer
    let (status, _) = post(&d.addr, "/v1/logs", JS, log_events_json().to_string().as_bytes());
    assert_eq!(status, 200);
    assert_eq!(
        texts(home, "agent:socks"),
        [
            ("user_input".to_string(), "Pair the striped socks".to_string()),
            ("assistant".into(), "Paired 4 striped socks.".into())
        ]
    );

    // malformed bodies: 400, counted, nothing stored, the receiver keeps serving
    let before = oojuro(home, &["stats"])["records"].as_u64().unwrap();
    let (status, answer) = post(&d.addr, "/v1/traces", PB, b"\xff\xff\xff\x07garbage");
    assert_eq!(status, 400);
    assert_eq!(pb::RpcStatus::decode(&answer[..]).unwrap().code, 3);
    assert_eq!(post(&d.addr, "/v1/traces", JS, b"{\"resourceSpans\": [ {").0, 400);
    assert_eq!(
        post(&d.addr, "/v1/traces", JS, br#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"zz"}]}]}]}"#).0,
        400
    );
    assert_eq!(
        http(&d.addr, "POST", "/v1/traces", &[("Content-Type", JS), ("Content-Encoding", "gzip")], b"not gzip").0,
        400
    );
    assert_eq!(post(&d.addr, "/v1/traces", "text/plain", b"hi").0, 415);
    assert_eq!(post(&d.addr, "/v1/metrics", PB, b"").0, 404);
    // too large: refused from the header, before reading the body
    let (status, _) = http(
        &d.addr,
        "POST",
        "/v1/traces",
        &[("Content-Type", PB), ("Content-Length", &(5u64 << 20).to_string())],
        b"x",
    );
    assert_eq!(status, 413);
    let n = stats(&d.addr);
    assert_eq!(
        (n["malformed"].as_u64(), n["too_large"].as_u64(), n["unsupported"].as_u64()),
        (Some(4), Some(1), Some(1))
    );
    assert_eq!(oojuro(home, &["stats"])["records"].as_u64().unwrap(), before);

    // a large batch: 5,000 chat spans (each its own trace) in one request
    let spans: Vec<pb::Span> = (0..5000u32)
        .map(|i| {
            let mut sp = pb_span(
                (i % 250) as u8,
                (i % 200) as u8,
                "openai.chat",
                vec![
                    st("gen_ai.system", "openai"),
                    st("gen_ai.prompt.0.role", "user"),
                    st("gen_ai.prompt.0.content", &format!("batch question {i} about herd {}", i * 7)),
                    st("gen_ai.completion.0.role", "assistant"),
                    st("gen_ai.completion.0.content", &format!("batch answer {i}")),
                ],
            );
            sp.trace_id = i.to_be_bytes().repeat(4);
            sp
        })
        .collect();
    let big = request("bulk-bot", spans).encode_to_vec();
    assert!(big.len() > 500_000, "{}", big.len());
    let t = Instant::now();
    assert_eq!(post(&d.addr, "/v1/traces", PB, &big).0, 200);
    let took = t.elapsed();
    let got = texts(home, "otel:bulk-bot");
    assert_eq!(got.len(), 10_000);
    eprintln!("large batch: {} spans, {} bytes, {:?}", 5000, big.len(), took);
    let n = stats(&d.addr);
    assert_eq!(n["store_errors"].as_u64(), Some(0));
}

#[test]
fn the_receiver_runs_inside_tail_when_enabled() {
    let s = Scratch::new("ott");
    let home = &s.0;
    std::fs::write(home.join("config.toml"), "[otel]\nenabled = true\nlisten = \"127.0.0.1:0\"\n").unwrap();
    let logs = home.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let d = Daemon::start(home, &["tail", logs.to_str().unwrap()]);
    assert_eq!(post(&d.addr, "/v1/traces", PB, &openai_chat("none").encode_to_vec()).0, 200);
    assert_eq!(texts(home, "otel:travel-bot").len(), 5);
    // settings apply: paused means nothing is stored
    oojuro_plain(home, &["pause"]);
    std::thread::sleep(Duration::from_millis(1100)); // mtime granularity
    let other = request(
        "other-bot",
        vec![pb_span(
            9,
            9,
            "openai.chat",
            vec![
                st("gen_ai.system", "openai"),
                st("gen_ai.prompt.0.role", "user"),
                st("gen_ai.prompt.0.content", "paused question"),
            ],
        )],
    );
    assert_eq!(post(&d.addr, "/v1/traces", PB, &other.encode_to_vec()).0, 200);
    assert_eq!(stats(&d.addr)["withheld"].as_u64(), Some(1));
    assert!(texts(home, "otel:other-bot").is_empty());
}

fn oojuro_plain(home: &Path, args: &[&str]) {
    let o = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .args(args)
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(o.status.success());
}
