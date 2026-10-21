//! A local mock upstream (Anthropic and OpenAI shapes), a test client and an
//! in-memory sink. No real model API is ever called.
#![allow(dead_code)]
use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode};
use http_body::{Body, Frame};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::body::Incoming;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use oojuro_proxy::{Batch, Config, DirectRec, Listener, Proxy, Sink};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    io::Write,
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub type MockBody = UnsyncBoxBody<Bytes, Infallible>;

/// What the mock upstream received.
#[derive(Clone, Debug)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub struct MockState {
    pub received: Mutex<Vec<Received>>,
    /// Streams whose reader went away before the end.
    pub disconnects: AtomicUsize,
}

pub struct Mock {
    pub addr: SocketAddr,
    pub state: Arc<MockState>,
}

impl Mock {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn received(&self) -> Vec<Received> {
        self.state.received.lock().unwrap().clone()
    }
}

/// A body fed from a channel (a stream the mock writes event by event).
struct ChanBody {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
}

impl Body for ChanBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.rx.poll_recv(cx).map(|o| o.map(|b| Ok(Frame::data(b))))
    }
}

fn stream(chunks: Vec<String>, delay: Duration, st: Arc<MockState>) -> MockBody {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        for (i, c) in chunks.into_iter().enumerate() {
            if i > 0 && !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if tx.send(Bytes::from(c)).await.is_err() {
                st.disconnects.fetch_add(1, Ordering::SeqCst);
                return;
            }
        }
    });
    ChanBody { rx }.boxed_unsync()
}

fn full(b: impl Into<Bytes>) -> MockBody {
    Full::new(b.into()).boxed_unsync()
}

pub fn sse(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Last user text of an Anthropic request (string content or last text block).
fn last_user_text(req: &Value) -> String {
    let msgs = req["messages"].as_array().cloned().unwrap_or_default();
    for m in msgs.iter().rev() {
        if m["role"] != "user" {
            continue;
        }
        match &m["content"] {
            Value::String(s) => return s.clone(),
            Value::Array(b) => {
                if let Some(t) = b.iter().rev().find(|x| x["type"] == "text") {
                    return t["text"].as_str().unwrap_or("").to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// The deterministic answer of the mock: thinking, text, and a tool call
/// when the user text contains `[call]`.
pub fn mock_answer(user: &str) -> (String, String, Option<(String, String)>) {
    let call = user.contains("[call]").then(|| {
        let id = format!("toolu_{}", &blake3::hash(user.as_bytes()).to_hex()[..16]);
        (id, format!("{{\"path\": \"{}.txt\", \"limit\": 5}}", user.len()))
    });
    (format!("thinking about {user}"), format!("answer to: {user}"), call)
}

pub fn anthropic_sse_chunks(user: &str) -> Vec<String> {
    let (thinking, text, call) = mock_answer(user);
    let mut v = vec![
        sse(
            "message_start",
            &json!({"type":"message_start","message":{"id":"msg_mock","type":"message","role":"assistant","model":"claude-mock-1","content":[],"stop_reason":null,"usage":{"input_tokens":21,"cache_read_input_tokens":5,"output_tokens":1}}}),
        ),
        sse(
            "content_block_start",
            &json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        ),
        sse(
            "content_block_delta",
            &json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":thinking}}),
        ),
        sse(
            "content_block_delta",
            &json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
        ),
        sse("content_block_stop", &json!({"type":"content_block_stop","index":0})),
        sse(
            "content_block_start",
            &json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        ),
        sse("ping", &json!({"type":"ping"})),
    ];
    let (a, b) = text.split_at(text.len() / 2);
    for part in [a, b] {
        v.push(sse(
            "content_block_delta",
            &json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":part}}),
        ));
    }
    v.push(sse("content_block_stop", &json!({"type":"content_block_stop","index":1})));
    if let Some((id, input)) = &call {
        v.push(sse("content_block_start", &json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":id,"name":"Read","input":{}}})));
        let (a, b) = input.split_at(input.len() / 2);
        for part in [a, b] {
            v.push(sse("content_block_delta", &json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":part}})));
        }
        v.push(sse("content_block_stop", &json!({"type":"content_block_stop","index":2})));
    }
    let stop = if call.is_some() { "tool_use" } else { "end_turn" };
    v.push(sse(
        "message_delta",
        &json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"output_tokens":17}}),
    ));
    v.push(sse("message_stop", &json!({"type":"message_stop"})));
    v
}

pub fn chat_sse_chunks() -> Vec<String> {
    let d = |v: Value| format!("data: {v}\n\n");
    vec![
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"reasoning_content":"need the weather"}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"content":"Let me check "}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"content":"both cities."}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\": \"Par"}}]}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"is\"}"}}]}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"get_weather","arguments":"{\"city\": \"Oslo\"}"}}]}}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ),
        d(
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-mock","choices":[],"usage":{"prompt_tokens":40,"completion_tokens":22,"prompt_tokens_details":{"cached_tokens":8}}}),
        ),
        "data: [DONE]\n\n".to_string(),
    ]
}

pub fn responses_sse_chunks() -> Vec<String> {
    let e = |t: &str, v: Value| format!("event: {t}\ndata: {v}\n\n");
    let msg = json!({"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Ran it: 3 files.","annotations":[]}]});
    let call = json!({"type":"function_call","id":"fc_1","call_id":"call_r1","name":"shell","arguments":"{\"cmd\": [\"ls\"]}","status":"completed"});
    let reasoning = json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"List the files first."}],"encrypted_content":"gAAAA"});
    vec![
        e(
            "response.created",
            json!({"type":"response.created","response":{"id":"resp_1","model":"gpt-mock-r","status":"in_progress","output":[]}}),
        ),
        e(
            "response.output_item.added",
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
        ),
        e(
            "response.reasoning_summary_text.delta",
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"List the files first."}),
        ),
        e("response.output_item.done", json!({"type":"response.output_item.done","output_index":0,"item":reasoning})),
        e(
            "response.output_item.added",
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"msg_1","role":"assistant","content":[]}}),
        ),
        e(
            "response.output_text.delta",
            json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Ran it: "}),
        ),
        e(
            "response.output_text.delta",
            json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"3 files."}),
        ),
        e("response.output_item.done", json!({"type":"response.output_item.done","output_index":1,"item":msg})),
        e(
            "response.output_item.added",
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_1","call_id":"call_r1","name":"shell","arguments":""}}),
        ),
        e(
            "response.function_call_arguments.delta",
            json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"cmd\": [\"ls\"]}"}),
        ),
        e("response.output_item.done", json!({"type":"response.output_item.done","output_index":2,"item":call})),
        e(
            "response.completed",
            json!({"type":"response.completed","response":{"id":"resp_1","model":"gpt-mock-r","status":"completed","output":[reasoning, msg, call],"usage":{"input_tokens":50,"output_tokens":9,"output_tokens_details":{"reasoning_tokens":4}}}}),
        ),
    ]
}

pub fn gzip(b: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

async fn handle(req: Request<Incoming>, st: Arc<MockState>) -> Result<Response<MockBody>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    st.received.lock().unwrap().push(Received {
        method: parts.method.to_string(),
        path: parts.uri.to_string(),
        headers: parts.headers.clone(),
        body: body.to_vec(),
    });
    let scenario = parts.headers.get("x-mock").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let req_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let sse_resp = |b: MockBody| {
        Response::builder()
            .header("content-type", "text/event-stream; charset=utf-8")
            .header("cache-control", "no-cache")
            .header("request-id", "req_mock_1")
            .body(b)
            .unwrap()
    };
    let json_resp = |v: Vec<u8>| {
        Response::builder()
            .header("content-type", "application/json")
            .header("x-request-id", "req_mock_2")
            .body(full(v))
            .unwrap()
    };
    let r = match scenario.as_str() {
        "anthropic-sse" => sse_resp(stream(
            anthropic_sse_chunks(&last_user_text(&req_json)),
            Duration::from_millis(2),
            Arc::clone(&st),
        )),
        "anthropic-sse-fast" => {
            sse_resp(stream(anthropic_sse_chunks(&last_user_text(&req_json)), Duration::ZERO, Arc::clone(&st)))
        }
        "anthropic-sse-gzip" => {
            let z = gzip(anthropic_sse_chunks(&last_user_text(&req_json)).concat().as_bytes());
            Response::builder()
                .header("content-type", "text/event-stream")
                .header("content-encoding", "gzip")
                .body(full(z))
                .unwrap()
        }
        "anthropic-json" => {
            let (thinking, text, _) = mock_answer(&last_user_text(&req_json));
            // spacing and key order as a server might send them
            let b = format!(
                "{{\"id\": \"msg_j\",  \"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-mock-1\",\"content\":[{{\"type\":\"thinking\",\"thinking\":{},\"signature\":\"s\"}},{{\"type\":\"text\",\"text\":{}}},{{\"type\":\"tool_use\",\"id\":\"toolu_j\",\"name\":\"Bash\",\"input\":{{\"command\": \"ls -la\", \"description\": \"list\"}}}}],\"stop_reason\":\"tool_use\",\"usage\":{{\"input_tokens\":30,\"output_tokens\":12}}}}",
                json!(thinking),
                json!(text)
            );
            json_resp(b.into_bytes())
        }
        "chat-sse" => sse_resp(stream(chat_sse_chunks(), Duration::from_millis(2), Arc::clone(&st))),
        "responses-sse" => sse_resp(stream(responses_sse_chunks(), Duration::from_millis(2), Arc::clone(&st))),
        "responses-json" => {
            let v = json!({"id":"resp_2","object":"response","model":"gpt-mock-r","status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"plain answer"}]}],
                "usage":{"input_tokens":5,"output_tokens":2}});
            json_resp(v.to_string().into_bytes())
        }
        "slow-sse" => {
            let mut chunks = anthropic_sse_chunks("slow")[..6].to_vec();
            for i in 0..2000 {
                chunks.push(sse("content_block_delta", &json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":format!("tick{i} ")}})));
            }
            sse_resp(stream(chunks, Duration::from_millis(10), Arc::clone(&st)))
        }
        "headers" => Response::builder()
            .status(201)
            .header("content-type", "application/json")
            .header("x-custom", "abc")
            .header("set-cookie", "a=1")
            .header("set-cookie", "b=2")
            .header("request-id", "req_h")
            .body(full(&b"{\"weird\" :  [1,2 ,3]}\n"[..]))
            .unwrap(),
        s if s.starts_with("status:") => {
            let code: u16 = s[7..].parse().unwrap();
            let body =
                json!({"type":"error","error":{"type":"overloaded_error","message":format!("mock status {code}")}});
            Response::builder()
                .status(StatusCode::from_u16(code).unwrap())
                .header("content-type", "application/json")
                .header("request-id", "req_err")
                .body(full(body.to_string()))
                .unwrap()
        }
        _ => Response::builder().header("content-type", "application/json").body(full(&b"{\"data\":[]}"[..])).unwrap(),
    };
    Ok(r)
}

/// Start the mock upstream on `rt`.
pub fn mock(rt: &tokio::runtime::Runtime) -> Mock {
    let state = Arc::new(MockState::default());
    let l = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let addr = l.local_addr().unwrap();
    let st = Arc::clone(&state);
    rt.spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { continue };
            let _ = s.set_nodelay(true); // like a real server: no Nagle delay on small SSE writes
            let st = Arc::clone(&st);
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |r| handle(r, Arc::clone(&st)));
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(s), svc).await;
            });
        }
    });
    Mock { addr, state }
}

/// Sink modes.
pub const OK: u8 = 0;
pub const FAIL: u8 = 1;
pub const HANG: u8 = 2;

#[derive(Clone, Default)]
pub struct MemSink {
    pub batches: Arc<Mutex<Vec<Batch>>>,
    pub mode: Arc<AtomicU8>,
}

impl Sink for MemSink {
    fn write(&mut self, b: Batch) -> Result<u64, String> {
        while self.mode.load(Ordering::SeqCst) == HANG {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.mode.load(Ordering::SeqCst) == FAIL {
            return Err("journal down".into());
        }
        let n = b.records.len() as u64;
        self.batches.lock().unwrap().push(b);
        Ok(n)
    }
}

impl MemSink {
    pub fn records(&self) -> Vec<DirectRec> {
        self.batches.lock().unwrap().iter().flat_map(|b| b.records.clone()).collect()
    }
    pub fn json(&self) -> String {
        let b = self.batches.lock().unwrap();
        let v: Vec<Value> = b.iter().flat_map(|b| b.records.iter().map(|r| serde_json::to_value(r).unwrap())).collect();
        serde_json::to_string(&v).unwrap()
    }
}

pub fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("oojuro-proxy-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn start_proxy(mock: &Mock, sink: &MemSink, seen: Option<PathBuf>, tweak: impl FnOnce(&mut Config)) -> Proxy {
    let mut cfg = Config {
        listeners: vec![Listener {
            listen: "127.0.0.1:0".parse().unwrap(),
            writer: None,
            anthropic_upstream: mock.url(),
            openai_upstream: mock.url(),
        }],
        seen_path: seen,
        ..Config::default()
    };
    tweak(&mut cfg);
    Proxy::start(cfg, Box::new(sink.clone())).unwrap()
}

pub type TestClient = Client<HttpConnector, Full<Bytes>>;

pub fn client() -> TestClient {
    let mut http = HttpConnector::new();
    http.set_nodelay(true);
    Client::builder(TokioExecutor::new()).build(http)
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// Time to the first body byte.
    pub ttfb: Duration,
}

pub async fn call(
    c: &TestClient,
    addr: SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    body: impl Into<Bytes>,
) -> Reply {
    let mut b = Request::post(format!("http://{addr}{path}"));
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let t = Instant::now();
    let resp = c.request(b.body(Full::new(body.into())).unwrap()).await.unwrap();
    let (parts, mut body) = resp.into_parts();
    let mut out = Vec::new();
    let mut ttfb = None;
    while let Some(f) = body.frame().await {
        if let Ok(d) = f.unwrap().into_data() {
            ttfb.get_or_insert_with(|| t.elapsed());
            out.extend_from_slice(&d);
        }
    }
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: Bytes::from(out),
        ttfb: ttfb.unwrap_or_else(|| t.elapsed()),
    }
}

pub fn wait_until(limit: Duration, f: impl FnMut() -> bool) {
    wait_for(limit, f, String::new);
}

/// [`wait_until`], with `why` in the failure message.
pub fn wait_for(limit: Duration, mut f: impl FnMut() -> bool, why: impl Fn() -> String) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < limit, "timed out after {limit:?} {}", why());
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn texts(recs: &[DirectRec], kind: &str) -> Vec<String> {
    let k = oojuro_capture::kind::code(kind).unwrap();
    recs.iter().filter(|r| r.kind == k).map(|r| r.text.clone()).collect()
}
