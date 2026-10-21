//! OpenTelemetry receiver (`docs/CAPTURE.md`, "OpenTelemetry"): OTLP/HTTP
//! on `127.0.0.1:4318` by default, off unless enabled.
//!
//! - `POST /v1/traces` (and `POST /v1/logs`, where GenAI instrumentations
//!   send message events), `application/x-protobuf` or `application/json`,
//!   optionally gzip-compressed.
//! - GenAI spans and events become records ([`genai`]); everything else is
//!   counted and skipped. Records go to the journal's single writer through
//!   the append path (settings, redaction, store-once keys,
//!   [`crate::append`]).
//! - A malformed body is answered `400` and counted; a body over
//!   `max_body_bytes` `413`; a journal that cannot take the records `503`
//!   (retryable: the exporter sends again, and store-once keys keep it from
//!   storing twice).
//! - `GET /` answers the counters as JSON.
//!
//! Settings, in the global `config.toml` only (a listener is not a
//! per-project matter):
//!
//! ```toml
//! [otel]
//! enabled = true
//! listen = "127.0.0.1:4318"
//! max_body_bytes = 67108864
//! [otel.writers]           # service.name -> writer (default otel:<service.name>)
//! "support-bot" = "agent:support"
//! ```
pub mod genai;
pub mod model;
pub mod pb;

use crate::{
    Result,
    append::{Appended, NewRecord},
    config::Config,
    proto::Request,
    server::WriteJob,
};
use prost::Message;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:4318";
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 << 20;
pub const MAX_CONNECTIONS: usize = 32;
const MAX_HEAD_BYTES: usize = 64 << 10;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest wait for the journal's writer to take a batch.
const APPEND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub struct OtelSettings {
    pub enabled: bool,
    pub listen: String,
    pub max_body_bytes: usize,
    /// `service.name` -> writer.
    pub writers: BTreeMap<String, String>,
}

impl Default for OtelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: DEFAULT_LISTEN.into(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            writers: BTreeMap::new(),
        }
    }
}

impl OtelSettings {
    /// `[otel]` of the global settings file.
    pub fn from_config(c: &Config) -> Result<Self> {
        let mut s = Self::default();
        let Some(g) = &c.global else { return Ok(s) };
        let Some(t) = g.table.get(crate::config::OTEL) else { return Ok(s) };
        let t = t.as_table().ok_or_else(|| format!("{}: [otel] must be a table", g.path.display()))?;
        let bad = |k: &str, what: &str| format!("{}: otel.{k} must be {what}", g.path.display());
        for (k, v) in t {
            match k.as_str() {
                "enabled" => s.enabled = v.as_bool().ok_or_else(|| bad(k, "true or false"))?,
                "listen" => {
                    let l = v.as_str().ok_or_else(|| bad(k, "an address like 127.0.0.1:4318"))?;
                    l.parse::<SocketAddr>().map_err(|_| bad(k, "an address like 127.0.0.1:4318"))?;
                    s.listen = l.to_string();
                }
                "max_body_bytes" => {
                    s.max_body_bytes =
                        v.as_integer()
                            .filter(|n| (1024..=1 << 30).contains(n))
                            .ok_or_else(|| bad(k, "an integer in 1024..=1073741824"))? as usize
                }
                "writers" => {
                    let w = v.as_table().ok_or_else(|| bad(k, "a table of service.name = writer"))?;
                    for (svc, writer) in w {
                        let writer = writer.as_str().ok_or_else(|| bad("writers", "a table of strings"))?;
                        crate::paths::check_writer(writer)?;
                        s.writers.insert(svc.clone(), writer.to_string());
                    }
                }
                other => return Err(format!("{}: unknown otel setting `{other}`", g.path.display()).into()),
            }
        }
        Ok(s)
    }
}

/// Where mapped records go: the journal's single writer.
pub trait Sink: Send + Sync {
    fn append(&self, records: Vec<NewRecord>) -> Result<Appended>;
}

/// A sink that queues appends to the recorder's write loop.
pub struct ChannelSink(Mutex<mpsc::Sender<WriteJob>>);

impl ChannelSink {
    pub fn new(tx: mpsc::Sender<WriteJob>) -> Self {
        Self(Mutex::new(tx))
    }
}

impl Sink for ChannelSink {
    fn append(&self, records: Vec<NewRecord>) -> Result<Appended> {
        let (tx, rx) = mpsc::channel();
        let job = WriteJob { req: Request::Append { records }, reply: tx };
        self.0.lock().unwrap_or_else(|e| e.into_inner()).send(job).map_err(|_| "the recorder is shutting down")?;
        match rx.recv_timeout(APPEND_TIMEOUT) {
            Ok(Ok(v)) => Ok(serde_json::from_value(v)?),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err("the recorder did not take the records in time".into()),
        }
    }
}

/// Receiver counters.
#[derive(Default)]
pub struct Stats {
    pub requests: AtomicU64,
    pub spans: AtomicU64,
    pub genai_spans: AtomicU64,
    pub unknown_spans: AtomicU64,
    pub no_content: AtomicU64,
    pub log_records: AtomicU64,
    pub genai_events: AtomicU64,
    pub unknown_logs: AtomicU64,
    pub records: AtomicU64,
    pub stored: AtomicU64,
    pub deduped: AtomicU64,
    pub withheld: AtomicU64,
    pub redacted: AtomicU64,
    pub malformed: AtomicU64,
    pub too_large: AtomicU64,
    pub unsupported: AtomicU64,
    pub store_errors: AtomicU64,
}

impl Stats {
    fn add(&self, c: &genai::Counts) {
        let r = Ordering::Relaxed;
        self.spans.fetch_add(c.spans, r);
        self.genai_spans.fetch_add(c.genai_spans, r);
        self.unknown_spans.fetch_add(c.unknown_spans, r);
        self.no_content.fetch_add(c.no_content, r);
        self.log_records.fetch_add(c.log_records, r);
        self.genai_events.fetch_add(c.genai_events, r);
        self.unknown_logs.fetch_add(c.unknown_logs, r);
        self.records.fetch_add(c.records, r);
    }

    pub fn snapshot(&self) -> Value {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        json!({
            "requests": g(&self.requests), "spans": g(&self.spans), "genai_spans": g(&self.genai_spans),
            "unknown_spans": g(&self.unknown_spans), "no_content": g(&self.no_content),
            "log_records": g(&self.log_records), "genai_events": g(&self.genai_events),
            "unknown_logs": g(&self.unknown_logs), "records": g(&self.records), "stored": g(&self.stored),
            "deduped": g(&self.deduped), "withheld": g(&self.withheld), "redacted": g(&self.redacted),
            "malformed": g(&self.malformed), "too_large": g(&self.too_large), "unsupported": g(&self.unsupported),
            "store_errors": g(&self.store_errors),
        })
    }
}

/// An HTTP answer.
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

const PROTOBUF: &str = "application/x-protobuf";
const JSON: &str = "application/json";

fn status_answer(status: u16, proto: bool, msg: &str) -> Answer {
    // google.rpc.Status codes: INVALID_ARGUMENT 3, RESOURCE_EXHAUSTED 8, UNAVAILABLE 14
    let code = match status {
        400 | 415 => 3,
        413 => 8,
        _ => 14,
    };
    if proto {
        Answer { status, content_type: PROTOBUF, body: pb::RpcStatus { code, message: msg.into() }.encode_to_vec() }
    } else {
        Answer { status, content_type: JSON, body: json!({ "code": code, "message": msg }).to_string().into_bytes() }
    }
}

/// Answer one request (decompressed body).
pub fn handle(
    method: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
    s: &OtelSettings,
    sink: &dyn Sink,
    st: &Stats,
) -> Answer {
    st.requests.fetch_add(1, Ordering::Relaxed);
    let path = path.split('?').next().unwrap_or("");
    match (method, path) {
        ("GET", "/") => {
            return Answer { status: 200, content_type: JSON, body: st.snapshot().to_string().into_bytes() };
        }
        ("POST", "/v1/traces" | "/v1/logs") => {}
        (_, "/v1/traces" | "/v1/logs") => {
            return Answer { status: 405, content_type: JSON, body: br#"{"message":"use POST"}"#.to_vec() };
        }
        _ => return Answer { status: 404, content_type: JSON, body: br#"{"message":"not found"}"#.to_vec() },
    }
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let proto = match ct.as_str() {
        PROTOBUF => true,
        JSON => false,
        _ => {
            st.unsupported.fetch_add(1, Ordering::Relaxed);
            return status_answer(415, false, "content type must be application/x-protobuf or application/json");
        }
    };
    let mapped = if path == "/v1/traces" {
        let b = if proto { model::traces_from_pb(body) } else { model::traces_from_json(body) };
        b.map(|b| genai::map_spans(&b, &s.writers))
    } else {
        let b = if proto { model::logs_from_pb(body) } else { model::logs_from_json(body) };
        b.map(|b| genai::map_logs(&b, &s.writers))
    };
    let (records, counts) = match mapped {
        Ok(x) => x,
        Err(e) => {
            st.malformed.fetch_add(1, Ordering::Relaxed);
            return status_answer(400, proto, &e);
        }
    };
    st.add(&counts);
    if !records.is_empty() {
        match sink.append(records) {
            Ok(a) => {
                let r = Ordering::Relaxed;
                st.stored.fetch_add(a.ids.len() as u64, r);
                st.deduped.fetch_add(a.deduped as u64, r);
                st.withheld.fetch_add(a.withheld as u64, r);
                st.redacted.fetch_add(a.redacted, r);
            }
            Err(e) => {
                st.store_errors.fetch_add(1, Ordering::Relaxed);
                return status_answer(503, proto, &format!("journal: {e}"));
            }
        }
    }
    // a full success: an empty ExportTraceServiceResponse / ExportLogsServiceResponse
    if proto {
        Answer { status: 200, content_type: PROTOBUF, body: pb::ExportTraceServiceResponse::default().encode_to_vec() }
    } else {
        Answer { status: 200, content_type: JSON, body: b"{}".to_vec() }
    }
}

/// A running receiver.
pub struct Receiver {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    pub stats: Arc<Stats>,
}

impl Receiver {
    /// Listen on `s.listen` and serve until [`Receiver::stop`].
    pub fn start(s: &OtelSettings, sink: Arc<dyn Sink>) -> Result<Self> {
        let l = TcpListener::bind(&s.listen).map_err(|e| format!("otel: cannot listen on {}: {e}", s.listen))?;
        let addr = l.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let active = Arc::new(AtomicUsize::new(0));
        let (stop2, stats2, settings) = (Arc::clone(&stop), Arc::clone(&stats), Arc::new(s.clone()));
        let accept = std::thread::Builder::new().name("oojurod-otel".into()).spawn(move || {
            for conn in l.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    active.fetch_sub(1, Ordering::SeqCst);
                    let _ = respond(&conn, &status_answer(503, false, "too many connections"), true);
                    continue;
                }
                let (sink, stats, settings, active) =
                    (Arc::clone(&sink), Arc::clone(&stats2), Arc::clone(&settings), Arc::clone(&active));
                let spawned = std::thread::Builder::new().name("oojurod-otel-conn".into()).spawn(move || {
                    serve(conn, &settings, sink.as_ref(), &stats);
                    active.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    continue;
                }
            }
        })?;
        Ok(Self { addr, stop, accept: Some(accept), stats })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting and wait for the accept thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1)); // wake the accept loop
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        if self.accept.is_some() {
            self.shutdown();
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

fn respond(mut s: &TcpStream, a: &Answer, close: bool) -> std::io::Result<()> {
    // one write (head and body together): no small-segment stall
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}\r\n",
        a.status,
        reason(a.status),
        a.content_type,
        a.body.len(),
        if close { "Connection: close\r\n" } else { "" }
    )
    .into_bytes();
    out.extend_from_slice(&a.body);
    s.write_all(&out)?;
    s.flush()
}

/// A request head: method, path, HTTP/1.0, lowercase headers.
struct Head {
    method: String,
    path: String,
    http10: bool,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

enum ReadErr {
    /// The peer closed the connection between requests.
    Closed,
    /// Answer with this status, then close.
    Status(u16, String),
    Io,
}

fn read_head(r: &mut impl BufRead) -> std::result::Result<Head, ReadErr> {
    let mut lines = Vec::new();
    let mut total = 0;
    loop {
        let mut line = Vec::new();
        let n = r
            .by_ref()
            .take((MAX_HEAD_BYTES + 1 - total) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| ReadErr::Io)?;
        if n == 0 {
            return Err(if lines.is_empty() { ReadErr::Closed } else { ReadErr::Io });
        }
        total += n;
        if total > MAX_HEAD_BYTES {
            return Err(ReadErr::Status(431, "request head too large".into()));
        }
        let l = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
        if l.is_empty() {
            if lines.is_empty() {
                continue; // stray empty line between requests
            }
            break;
        }
        lines.push(l);
    }
    let mut first = lines[0].split_whitespace();
    let (Some(method), Some(path), Some(version)) = (first.next(), first.next(), first.next()) else {
        return Err(ReadErr::Status(400, "bad request line".into()));
    };
    let headers = lines[1..]
        .iter()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Ok(Head { method: method.into(), path: path.into(), http10: version == "HTTP/1.0", headers })
}

fn read_body(r: &mut impl BufRead, h: &Head, max: usize) -> std::result::Result<Vec<u8>, ReadErr> {
    let chunked = h.header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    if chunked {
        let mut body = Vec::new();
        loop {
            let mut size_line = String::new();
            r.read_line(&mut size_line).map_err(|_| ReadErr::Io)?;
            let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| ReadErr::Status(400, "bad chunk size".into()))?;
            if size == 0 {
                // trailers until an empty line
                loop {
                    let mut t = String::new();
                    if r.read_line(&mut t).map_err(|_| ReadErr::Io)? == 0 || t.trim().is_empty() {
                        break;
                    }
                }
                return Ok(body);
            }
            if body.len() + size > max {
                return Err(ReadErr::Status(413, format!("body over {max} bytes")));
            }
            let start = body.len();
            body.resize(start + size, 0);
            r.read_exact(&mut body[start..]).map_err(|_| ReadErr::Io)?;
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf).map_err(|_| ReadErr::Io)?;
        }
    }
    let len = match h.header("content-length") {
        Some(l) => l.parse::<usize>().map_err(|_| ReadErr::Status(400, "bad content-length".into()))?,
        None if h.method == "POST" => return Err(ReadErr::Status(411, "content-length required".into())),
        None => 0,
    };
    if len > max {
        return Err(ReadErr::Status(413, format!("body over {max} bytes")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(|_| ReadErr::Io)?;
    Ok(body)
}

/// Undo `Content-Encoding` (gzip, or none), at most `max` bytes out.
fn decode(h: &Head, body: Vec<u8>, max: usize) -> std::result::Result<Vec<u8>, (u16, String)> {
    match h.header("content-encoding").map(|e| e.trim().to_ascii_lowercase()) {
        None => Ok(body),
        Some(e) if e.is_empty() || e == "identity" => Ok(body),
        Some(e) if e == "gzip" => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&body[..])
                .take(max as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| (400, format!("bad gzip body: {e}")))?;
            if out.len() > max {
                return Err((413, format!("body over {max} bytes once decompressed")));
            }
            Ok(out)
        }
        Some(e) => Err((415, format!("content encoding {e} is not supported (gzip is)"))),
    }
}

fn serve(conn: TcpStream, s: &OtelSettings, sink: &dyn Sink, st: &Stats) {
    let _ = conn.set_read_timeout(Some(IDLE_TIMEOUT));
    let _ = conn.set_write_timeout(Some(IDLE_TIMEOUT));
    let _ = conn.set_nodelay(true);
    let Ok(read_half) = conn.try_clone() else { return };
    let mut r = BufReader::new(read_half);
    loop {
        let h = match read_head(&mut r) {
            Ok(h) => h,
            Err(ReadErr::Status(code, msg)) => {
                st.malformed.fetch_add(1, Ordering::Relaxed);
                let _ = respond(&conn, &status_answer(code, false, &msg), true);
                return;
            }
            Err(_) => return,
        };
        let body = match read_body(&mut r, &h, s.max_body_bytes) {
            Ok(b) => b,
            Err(ReadErr::Status(code, msg)) => {
                if code == 413 {
                    st.too_large.fetch_add(1, Ordering::Relaxed);
                } else {
                    st.malformed.fetch_add(1, Ordering::Relaxed);
                }
                let proto = h.header("content-type").is_some_and(|c| c.starts_with(PROTOBUF));
                let _ = respond(&conn, &status_answer(code, proto, &msg), true);
                return;
            }
            Err(_) => return,
        };
        let close = h.http10 || h.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let ct = h.header("content-type").unwrap_or("").to_string();
        let a = match decode(&h, body, s.max_body_bytes) {
            Ok(b) => handle(&h.method, &h.path, &ct, &b, s, sink, st),
            Err((code, msg)) => {
                if code == 413 {
                    st.too_large.fetch_add(1, Ordering::Relaxed);
                } else {
                    st.malformed.fetch_add(1, Ordering::Relaxed);
                }
                status_answer(code, ct.starts_with(PROTOBUF), &msg)
            }
        };
        if respond(&conn, &a, close).is_err() || close {
            return;
        }
    }
}
