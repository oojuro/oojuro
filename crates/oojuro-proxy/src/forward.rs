//! The hot path: accept, forward, stream back. Nothing here waits for the
//! journal: a recorded call's bodies are teed ([`crate::tee`]) and, when the
//! response ends, handed to the journal queue with `try_send` (a full queue
//! drops the copy and counts it).
//!
//! Headers: end-to-end headers go upstream unchanged, credentials included
//! (they are never journaled). Removed: hop-by-hop headers (`Connection` and
//! the ones it names, `Keep-Alive`, `Transfer-Encoding`, `TE`, `Trailer`,
//! `Upgrade`, `Proxy-*`), `Expect` (answered here), `Host` (set from the
//! upstream URL) and the proxy's own `x-oojuro-*`. Responses come back
//! with their status, end-to-end headers and body bytes as received.
use crate::{
    C, Config, Format, Listener, Stats,
    extract::Route,
    journal::Job,
    now_us,
    tee::{Captured, End, Tee},
};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, Request, Response, StatusCode, Uri, header};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::body::Incoming;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicUsize, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    time::Duration,
};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = UnsyncBoxBody<Bytes, BoxError>;
type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>;

/// State shared by every connection and the journal worker.
pub struct Shared {
    client: HttpsClient,
    tx: Mutex<Option<SyncSender<Job>>>,
    pub stats: Arc<Stats>,
    inflight: AtomicUsize,
    active: AtomicUsize,
    queue_bytes: usize,
    max_body: usize,
    /// Bit per [`Format`] (its index in `Format::ALL`): tailed agents.
    tailed: AtomicU32,
    record_tailed: bool,
}

fn bit(f: Format) -> u32 {
    1 << Format::ALL.iter().position(|x| *x == f).unwrap_or(0)
}

impl Shared {
    pub(crate) fn new(client: HttpsClient, tx: SyncSender<Job>, stats: Arc<Stats>, cfg: &Config) -> Self {
        Self {
            client,
            tx: Mutex::new(Some(tx)),
            stats,
            inflight: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            queue_bytes: cfg.queue_bytes,
            max_body: cfg.max_body_bytes,
            tailed: AtomicU32::new(cfg.tailed.iter().map(|f| bit(*f)).fold(0, |a, b| a | b)),
            record_tailed: cfg.record_tailed,
        }
    }

    pub(crate) fn set_tailed(&self, formats: &[Format]) {
        self.tailed.store(formats.iter().map(|f| bit(*f)).fold(0, |a, b| a | b), Ordering::SeqCst);
    }

    pub(crate) fn is_tailed(&self, f: Format) -> bool {
        self.tailed.load(Ordering::SeqCst) & bit(f) != 0
    }

    /// Whether a call from this client is left to the transcript recorder.
    fn left_to_tailer(&self, h: &HeaderMap) -> bool {
        if self.record_tailed || self.tailed.load(Ordering::SeqCst) == 0 {
            return false;
        }
        let ua = header_str(h, header::USER_AGENT.as_str()).unwrap_or("");
        Format::of_client(ua, &|name: &str| header_str(h, name).map(str::to_string)).is_some_and(|f| self.is_tailed(f))
    }

    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Largest decoded body the journal worker builds.
    pub(crate) fn max_decoded(&self) -> usize {
        self.max_body.saturating_mul(4).min(1 << 30)
    }

    pub(crate) fn release(&self, bytes: usize) {
        self.inflight.fetch_sub(bytes, Ordering::SeqCst);
    }

    pub(crate) fn close_queue(&self) {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// Queue a finished call for the journal, never waiting.
    fn enqueue(&self, job: Job) {
        let bytes = job.bytes();
        if self.inflight.fetch_add(bytes, Ordering::SeqCst) + bytes > self.queue_bytes {
            self.release(bytes);
            self.stats.inc(C::DroppedQueueFull);
            return;
        }
        let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let r = match tx {
            Some(tx) => tx.try_send(job),
            None => Err(TrySendError::Disconnected(job)),
        };
        match r {
            Ok(()) => self.stats.inc(C::JobsQueued),
            Err(TrySendError::Full(_)) => {
                self.release(bytes);
                self.stats.inc(C::DroppedQueueFull);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.release(bytes);
                self.stats.inc(C::DroppedShutdown);
            }
        }
    }
}

/// Counts a call as in flight while it lives.
struct Active(Arc<Shared>);

impl Active {
    fn new(s: &Arc<Shared>) -> Self {
        s.active.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(s))
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// An upstream base URL: scheme, authority and a path prefix.
#[derive(Clone, Debug)]
pub struct Upstream {
    scheme: http::uri::Scheme,
    authority: http::uri::Authority,
    prefix: String,
}

impl Upstream {
    pub fn parse(s: &str) -> crate::Result<Self> {
        let u: Uri = s.parse().map_err(|e| format!("upstream {s:?}: {e}"))?;
        let scheme = u.scheme().cloned().ok_or_else(|| format!("upstream {s:?}: needs http:// or https://"))?;
        if scheme != http::uri::Scheme::HTTP && scheme != http::uri::Scheme::HTTPS {
            return Err(format!("upstream {s:?}: only http and https").into());
        }
        let authority = u.authority().cloned().ok_or_else(|| format!("upstream {s:?}: no host"))?;
        if u.query().is_some() {
            return Err(format!("upstream {s:?}: no query string allowed").into());
        }
        Ok(Self { scheme, authority, prefix: u.path().trim_end_matches('/').to_string() })
    }

    /// The upstream URL for an incoming path and query.
    pub fn join(&self, pq: Option<&http::uri::PathAndQuery>) -> Result<Uri, http::Error> {
        let pq = pq.map_or("/", |x| x.as_str());
        Uri::builder()
            .scheme(self.scheme.clone())
            .authority(self.authority.clone())
            .path_and_query(format!("{}{pq}", self.prefix))
            .build()
    }
}

/// One listener's writer and upstreams, parsed.
pub struct ListenerRoutes {
    writer: Option<String>,
    anthropic: Upstream,
    openai: Upstream,
}

impl ListenerRoutes {
    pub fn new(l: &Listener) -> crate::Result<Self> {
        Ok(Self {
            writer: l.writer.clone(),
            anthropic: Upstream::parse(&l.anthropic_upstream)?,
            openai: Upstream::parse(&l.openai_upstream)?,
        })
    }
}

pub(crate) fn client(connect_timeout: Duration) -> crate::Result<HttpsClient> {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http.set_connect_timeout(Some(connect_timeout));
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())?
        .https_or_http()
        .enable_http1()
        .wrap_connector(http);
    Ok(Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(90)).build(https))
}

/// A writer ID: 1-256 bytes, no control characters.
pub fn valid_writer(w: &str) -> bool {
    !w.is_empty() && w.len() <= 256 && !w.chars().any(char::is_control)
}

/// The product token of a `User-Agent` (`claude-cli/2.1 (external)` ->
/// `claude-cli`), lowercase, `[a-z0-9._-]`, at most 64 characters;
/// `unknown` when there is none.
pub fn ua_product(ua: &str) -> String {
    let p = oojuro_capture::ua_product(ua);
    if p.is_empty() { "unknown".into() } else { p }
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| std::str::from_utf8(v.as_bytes()).ok()).map(str::trim).filter(|s| !s.is_empty())
}

/// Writer of a call: `x-oojuro-writer`, else the listener's name, else
/// `proxy:<user-agent product>`.
pub fn writer_of(h: &HeaderMap, listener: Option<&str>) -> String {
    if let Some(w) = header_str(h, crate::WRITER_HEADER).filter(|w| valid_writer(w)) {
        return w.to_string();
    }
    if let Some(w) = listener.filter(|w| valid_writer(w)) {
        return w.to_string();
    }
    format!("proxy:{}", ua_product(header_str(h, header::USER_AGENT.as_str()).unwrap_or("")))
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "proxy-authorization",
    "proxy-authenticate",
];

fn strip_hop_by_hop(h: &mut HeaderMap) {
    let named: Vec<HeaderName> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|t| HeaderName::from_bytes(t.trim().as_bytes()).ok())
        .collect();
    for n in named {
        h.remove(n);
    }
    for n in HOP_BY_HOP {
        h.remove(*n);
    }
}

/// Headers as they go upstream (see the module docs).
pub fn upstream_headers(h: &mut HeaderMap) {
    strip_hop_by_hop(h);
    h.remove(header::HOST);
    h.remove(header::EXPECT);
    let own: Vec<HeaderName> = h.keys().filter(|k| k.as_str().starts_with("x-oojuro-")).cloned().collect();
    for k in own {
        h.remove(k);
    }
}

fn plain_response(status: StatusCode, body: String) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(body)).map_err(|e: Infallible| match e {}).boxed_unsync());
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
    r
}

pub(crate) async fn serve(
    l: tokio::net::TcpListener,
    shared: Arc<Shared>,
    routes: Arc<ListenerRoutes>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let conn = tokio::select! {
            c = l.accept() => c,
            _ = stop.changed() => break,
        };
        let (stream, _) = match conn {
            Ok(c) => c,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await; // e.g. out of file descriptors
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let (shared, routes) = (Arc::clone(&shared), Arc::clone(&routes));
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req| handle(req, Arc::clone(&shared), Arc::clone(&routes)));
            let _ = hyper::server::conn::http1::Builder::new()
                .keep_alive(true)
                .serve_connection(TokioIo::new(stream), svc)
                .await;
        });
    }
}

async fn handle(
    req: Request<Incoming>,
    sh: Arc<Shared>,
    routes: Arc<ListenerRoutes>,
) -> Result<Response<Body>, Infallible> {
    sh.stats.inc(C::Requests);
    let active = Active::new(&sh);
    let started_us = now_us();
    let (mut parts, body) = req.into_parts();
    let mut route = if parts.method == http::Method::POST { Route::of(parts.uri.path()) } else { None };
    let upstream = match route {
        Some(Route::AnthropicMessages) => &routes.anthropic,
        Some(_) => &routes.openai,
        None if parts.headers.contains_key("anthropic-version") || parts.uri.path().contains("/messages") => {
            &routes.anthropic
        }
        None => &routes.openai,
    };
    // the transcript recorder wins: an agent whose transcripts are tailed is not recorded here
    let tailed = route.is_some() && sh.left_to_tailer(&parts.headers);
    if tailed {
        route = None;
    }
    let writer = writer_of(&parts.headers, routes.writer.as_deref());
    let session = header_str(&parts.headers, crate::SESSION_HEADER).map(str::to_string);
    let cwd = header_str(&parts.headers, crate::CWD_HEADER).map(str::to_string);
    let req_encoding = header_str(&parts.headers, header::CONTENT_ENCODING.as_str()).map(str::to_string);
    upstream_headers(&mut parts.headers);
    parts.uri = match upstream.join(parts.uri.path_and_query()) {
        Ok(u) => u,
        Err(e) => return Ok(plain_response(StatusCode::BAD_REQUEST, format!("{{\"error\":\"oojuro proxy: {e}\"}}"))),
    };
    parts.version = http::Version::HTTP_11;
    let Some(route) = route else {
        sh.stats.inc(if tailed { C::SkippedTailed } else { C::Passthrough });
        let req = Request::from_parts(parts, body.map_err(BoxError::from).boxed_unsync());
        return Ok(match sh.client.request(req).await {
            Ok(resp) => {
                let (mut rp, rb) = resp.into_parts();
                strip_hop_by_hop(&mut rp.headers);
                Response::from_parts(rp, Tee::plain(rb).with_guard(active).map_err(BoxError::from).boxed_unsync())
            }
            Err(e) => {
                sh.stats.inc(C::UpstreamUnreachable);
                unreachable_response(&e)
            }
        });
    };
    sh.stats.inc(C::Recorded);
    let slot: Arc<Mutex<Option<Captured>>> = Arc::default();
    let req_body = {
        let slot = Arc::clone(&slot);
        Tee::new(body, sh.max_body, move |c| *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(c))
    };
    let req = Request::from_parts(parts, req_body.map_err(BoxError::from).boxed_unsync());
    let mut job = Job {
        route,
        writer,
        session,
        cwd,
        started_us,
        ended_us: 0,
        req: None,
        req_encoding,
        status: None,
        resp: None,
        resp_type: String::new(),
        resp_encoding: None,
        request_id: None,
        error: None,
    };
    match sh.client.request(req).await {
        Ok(resp) => {
            let (mut rp, rb) = resp.into_parts();
            strip_hop_by_hop(&mut rp.headers);
            job.status = Some(rp.status.as_u16());
            if rp.status.as_u16() >= 400 {
                sh.stats.inc(C::UpstreamErrorStatus);
            }
            job.resp_type = header_str(&rp.headers, header::CONTENT_TYPE.as_str()).unwrap_or("").to_ascii_lowercase();
            job.resp_encoding = header_str(&rp.headers, header::CONTENT_ENCODING.as_str()).map(str::to_string);
            job.request_id =
                ["request-id", "x-request-id"].iter().find_map(|h| header_str(&rp.headers, h)).map(str::to_string);
            let sh2 = Arc::clone(&sh);
            let tee = Tee::new(rb, sh.max_body, move |c: Captured| {
                job.ended_us = now_us();
                match &c.end {
                    End::Dropped => sh2.stats.inc(C::ClientDisconnects),
                    End::Error(_) => sh2.stats.inc(C::StreamErrors),
                    End::Complete => {}
                }
                job.req = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                job.resp = Some(c);
                sh2.enqueue(job);
            })
            .with_guard(active);
            Ok(Response::from_parts(rp, tee.map_err(BoxError::from).boxed_unsync()))
        }
        Err(e) => {
            sh.stats.inc(C::UpstreamUnreachable);
            job.ended_us = now_us();
            job.error = Some(format!("upstream unreachable: {}", error_chain(&e)));
            job.req = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            sh.enqueue(job);
            Ok(unreachable_response(&e))
        }
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(x) = src {
        s.push_str(": ");
        s.push_str(&x.to_string());
        src = x.source();
    }
    s
}

fn unreachable_response(e: &hyper_util::client::legacy::Error) -> Response<Body> {
    let msg = serde_json::json!({
        "type": "error",
        "error": {"type": "api_error", "message": format!("oojuro proxy: upstream unreachable: {}", error_chain(e))}
    });
    plain_response(StatusCode::BAD_GATEWAY, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_identity() {
        let mut h = HeaderMap::new();
        h.insert(header::USER_AGENT, "claude-cli/2.0.14 (external, cli)".parse().unwrap());
        assert_eq!(writer_of(&h, None), "proxy:claude-cli");
        assert_eq!(writer_of(&h, Some("bot")), "bot");
        h.insert(crate::WRITER_HEADER, "claude-code:-home-me-repo".parse().unwrap());
        assert_eq!(writer_of(&h, Some("bot")), "claude-code:-home-me-repo");
        assert_eq!(ua_product("OpenAI/Python 1.40.0"), "openai");
        assert_eq!(ua_product(""), "unknown");
        assert_eq!(writer_of(&HeaderMap::new(), None), "proxy:unknown");
    }

    #[test]
    fn upstream_join_and_headers() {
        let u = Upstream::parse("https://openrouter.ai/api/").unwrap();
        let pq: http::uri::PathAndQuery = "/v1/chat/completions?x=1".parse().unwrap();
        assert_eq!(u.join(Some(&pq)).unwrap().to_string(), "https://openrouter.ai/api/v1/chat/completions?x=1");
        assert!(Upstream::parse("ftp://x").is_err());
        let mut h = HeaderMap::new();
        for (k, v) in [
            ("host", "127.0.0.1:8790"),
            ("connection", "keep-alive, x-hop"),
            ("x-hop", "1"),
            ("x-oojuro-writer", "w"),
            ("authorization", "Bearer k"),
            ("x-api-key", "k"),
            ("content-length", "10"),
            ("accept-encoding", "gzip"),
        ] {
            h.insert(HeaderName::from_static(k), v.parse().unwrap());
        }
        upstream_headers(&mut h);
        let mut keys: Vec<&str> = h.keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["accept-encoding", "authorization", "content-length", "x-api-key"]);
    }
}
