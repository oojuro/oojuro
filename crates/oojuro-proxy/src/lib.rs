//! Oojuro's recording model-API proxy (`docs/CAPTURE.md`, "Model API proxy").
//!
//! An agent whose model base URL can be changed points it at this proxy
//! (127.0.0.1 only). Every request and response is forwarded unchanged
//! (end-to-end headers and body bytes) and streamed as it arrives. A copy of
//! both bodies goes, off the hot path, through a bounded queue to one
//! journal worker, which parses what the agent saw (system prompt, tool
//! definitions, input messages, tool results) and produced (text, thinking,
//! tool calls, upstream errors), keeps only what it has not stored before
//! for that writer ([`seen`]), masks secrets and hands the batch to a
//! [`Sink`] (the recorder's store). When the queue is full or the sink
//! fails, the agent's call still succeeds and the drop is counted.
//!
//! Recorded routes (POST): a path ending in `/messages` (Anthropic
//! Messages), `/chat/completions` (OpenAI Chat Completions) or `/responses`
//! (OpenAI Responses), plain JSON or streamed (SSE). Every other path is
//! passed through unrecorded.
//!
//! Writer: header `x-oojuro-writer`, else the listener's configured name,
//! else `proxy:<user-agent product>` ([`forward::writer_of`]). The proxy's
//! own headers (`x-oojuro-*`) are stripped before forwarding.
//!
//! The transcript recorder wins (design decision 2026-10-21): a call from an
//! agent whose transcripts the recorder tails (recognised on the wire by
//! its capture format, [`oojuro_capture::Format::of_client`]) is forwarded
//! unchanged and not recorded (`skipped_tailed`), whatever its
//! `x-oojuro-writer`; `record_tailed` turns this off.
pub mod decode;
pub mod extract;
pub mod forward;
pub mod journal;
pub mod seen;
pub mod sse;
pub mod tee;

pub use oojuro_capture::{DirectRec, Format};

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8790";
pub const DEFAULT_ANTHROPIC: &str = "https://api.anthropic.com";
pub const DEFAULT_OPENAI: &str = "https://api.openai.com";
/// `via` of every record the proxy writes.
pub const VIA: &str = "proxy";
/// Writer of the call (stripped before forwarding).
pub const WRITER_HEADER: &str = "x-oojuro-writer";
/// Optional conversation id: messages are deduplicated per writer and
/// conversation (stripped before forwarding).
pub const SESSION_HEADER: &str = "x-oojuro-session";
/// Optional working directory of the agent, for per-project settings
/// (stripped before forwarding).
pub const CWD_HEADER: &str = "x-oojuro-cwd";

/// One listening address and where its calls go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    /// Must be a loopback address.
    pub listen: SocketAddr,
    /// Writer for calls without `x-oojuro-writer`.
    pub writer: Option<String>,
    /// Base URL for Anthropic calls; the incoming path is appended.
    pub anthropic_upstream: String,
    /// Base URL for OpenAI-compatible calls; the incoming path is appended.
    pub openai_upstream: String,
}

impl Default for Listener {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.parse().unwrap(),
            writer: None,
            anthropic_upstream: DEFAULT_ANTHROPIC.into(),
            openai_upstream: DEFAULT_OPENAI.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listeners: Vec<Listener>,
    /// Calls waiting for the journal worker; beyond this they are dropped (counted).
    pub queue_jobs: usize,
    /// Body bytes waiting for the journal worker; beyond this calls are dropped.
    pub queue_bytes: usize,
    /// Largest request or response body kept for the journal (the call is
    /// forwarded in full either way).
    pub max_body_bytes: usize,
    /// Persistent seen-set (`None`: in memory only).
    pub seen_path: Option<PathBuf>,
    /// Message identities remembered per writer (least recently seen go first).
    pub seen_per_writer: usize,
    /// Threads serving connections.
    pub threads: usize,
    pub connect_timeout: Duration,
    /// Agents whose transcripts the recorder tails: their calls are
    /// forwarded but not recorded (the transcript recorder wins). Can be
    /// changed while running ([`Proxy::set_tailed`]).
    pub tailed: Vec<Format>,
    /// Record calls of tailed agents anyway (`[proxy] record_tailed`).
    pub record_tailed: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listeners: vec![Listener::default()],
            queue_jobs: 1024,
            queue_bytes: 512 << 20,
            max_body_bytes: 64 << 20,
            seen_path: None,
            seen_per_writer: 100_000,
            threads: 2,
            connect_timeout: Duration::from_secs(30),
            tailed: Vec::new(),
            record_tailed: false,
        }
    }
}

/// New records of one call, for one writer.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub via: String,
    /// `proxy:<route>`; stands in for the transcript path in the settings policy.
    pub source: String,
    pub records: Vec<DirectRec>,
}

/// Where batches go. `write` returns once the batch is durable (the number
/// of records stored, after the store's own settings); an error means
/// nothing was stored, and those messages are stored when the agent sends
/// them again.
pub trait Sink: Send + 'static {
    fn write(&mut self, batch: Batch) -> std::result::Result<u64, String>;
}

macro_rules! counters {
    ($($name:ident => $s:literal, $doc:literal;)*) => {
        /// Proxy counters.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(usize)]
        pub enum C { $(#[doc = $doc] $name,)* }
        const NAMES: &[&str] = &[$($s,)*];
    };
}

counters! {
    Requests => "requests", "every request received";
    Recorded => "recorded_calls", "requests on a recorded route";
    Passthrough => "passthrough", "requests on other paths (not recorded)";
    SkippedTailed => "skipped_tailed", "calls of agents whose transcripts the recorder tails (forwarded, not recorded)";
    UpstreamUnreachable => "upstream_unreachable", "calls that got no response from upstream (502 to the agent)";
    UpstreamErrorStatus => "upstream_error_status", "recorded calls answered with HTTP status 400 or above";
    StreamErrors => "stream_errors", "upstream bodies that broke off with an error";
    ClientDisconnects => "client_disconnects", "responses the agent stopped reading before the end";
    JobsQueued => "jobs_queued", "calls handed to the journal worker";
    DroppedQueueFull => "dropped_queue_full", "calls not journaled: queue full (journal slow)";
    DroppedTooLarge => "dropped_too_large", "bodies over max_body_bytes (not journaled)";
    DroppedJournal => "dropped_journal", "batches the sink did not store (journal down); retried on re-send";
    DroppedShutdown => "dropped_shutdown", "calls not journaled: proxy shutting down";
    BatchesWritten => "batches_written", "batches stored";
    RecordsNew => "records_new", "messages not seen before";
    RecordsDup => "records_dup", "messages already stored (re-sent history)";
    RecordsWritten => "records_written", "records the sink stored";
    RecordsDropped => "records_dropped", "records in failed batches";
    ParseErrors => "parse_errors", "bodies that could not be parsed";
    Redactions => "redactions", "secrets masked by the proxy";
}

/// Counter values (all monotonic).
#[derive(Debug)]
pub struct Stats([AtomicU64; NAMES.len()]);

impl Default for Stats {
    fn default() -> Self {
        Self(std::array::from_fn(|_| AtomicU64::new(0)))
    }
}

impl Stats {
    pub fn add(&self, c: C, n: u64) {
        self.0[c as usize].fetch_add(n, Ordering::Relaxed);
    }
    pub fn inc(&self, c: C) {
        self.add(c, 1);
    }
    pub fn get(&self, c: C) -> u64 {
        self.0[c as usize].load(Ordering::Relaxed)
    }
    pub fn snapshot(&self) -> BTreeMap<&'static str, u64> {
        NAMES.iter().zip(&self.0).map(|(n, v)| (*n, v.load(Ordering::Relaxed))).collect()
    }
}

/// A running proxy: listeners on a small runtime, and the journal worker.
pub struct Proxy {
    rt: Option<tokio::runtime::Runtime>,
    shared: Arc<forward::Shared>,
    addrs: Vec<SocketAddr>,
    worker: Option<JoinHandle<()>>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Proxy {
    /// Bind every listener (loopback only) and start serving.
    pub fn start(cfg: Config, sink: Box<dyn Sink>) -> Result<Self> {
        if cfg.listeners.is_empty() {
            return Err("proxy: no listener".into());
        }
        let mut routes = Vec::new();
        let mut std_listeners = Vec::new();
        for l in &cfg.listeners {
            if !l.listen.ip().is_loopback() {
                return Err(format!("proxy: {} is not a loopback address (127.0.0.1 or ::1 only)", l.listen).into());
            }
            if let Some(w) = &l.writer
                && !forward::valid_writer(w)
            {
                return Err(format!("proxy: invalid writer {w:?}").into());
            }
            let r = forward::ListenerRoutes::new(l)?;
            let s = std::net::TcpListener::bind(l.listen).map_err(|e| format!("proxy: bind {}: {e}", l.listen))?;
            s.set_nonblocking(true)?;
            routes.push(r);
            std_listeners.push(s);
        }
        let addrs: Vec<SocketAddr> = std_listeners.iter().map(|l| l.local_addr()).collect::<std::io::Result<_>>()?;
        let stats = Arc::new(Stats::default());
        let seen = seen::Seen::open(cfg.seen_path.as_deref(), cfg.seen_per_writer)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<journal::Job>(cfg.queue_jobs.max(1));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(cfg.threads.max(1))
            .thread_name("oojuro-proxy")
            .enable_all()
            .build()?;
        let client = forward::client(cfg.connect_timeout)?;
        let shared = Arc::new(forward::Shared::new(client, tx, Arc::clone(&stats), &cfg));
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("oojuro-proxy-journal".into())
                .spawn(move || journal::run(rx, sink, seen, shared))?
        };
        let (stop, stop_rx) = tokio::sync::watch::channel(false);
        for (l, r) in std_listeners.into_iter().zip(routes) {
            let _g = rt.enter();
            let l = tokio::net::TcpListener::from_std(l)?;
            rt.spawn(forward::serve(l, Arc::clone(&shared), Arc::new(r), stop_rx.clone()));
        }
        Ok(Self { rt: Some(rt), shared, addrs, worker: Some(worker), stop })
    }

    /// The bound addresses (port 0 resolved), in listener order.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    pub fn stats(&self) -> BTreeMap<&'static str, u64> {
        self.shared.stats.snapshot()
    }

    pub fn counter(&self, c: C) -> u64 {
        self.shared.stats.get(c)
    }

    /// Agents whose calls are not recorded because their transcripts are tailed.
    pub fn set_tailed(&self, formats: &[Format]) {
        self.shared.set_tailed(formats);
    }

    pub fn tailed(&self) -> Vec<Format> {
        Format::ALL.into_iter().filter(|f| self.shared.is_tailed(*f)).collect()
    }

    /// Calls in flight (response not finished).
    pub fn active(&self) -> usize {
        self.shared.active()
    }

    /// Stop accepting new connections (calls in flight continue).
    pub fn stop_accepting(&self) {
        let _ = self.stop.send(true);
    }

    /// Stop accepting, let calls in flight finish (up to `grace`), then
    /// journal what is queued through the sink and stop. Blocks; the sink
    /// must keep answering meanwhile.
    pub fn shutdown(mut self, grace: Duration) {
        self.stop_accepting();
        let t = Instant::now();
        while self.shared.active() > 0 && t.elapsed() < grace {
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(Duration::from_secs(1)); // aborted calls are journaled as cut off
        }
        self.shared.close_queue();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }

    /// [`Proxy::shutdown`] on its own thread (for a caller that is the sink).
    pub fn shutdown_in_background(self, grace: Duration) -> JoinHandle<()> {
        std::thread::spawn(move || self.shutdown(grace))
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
        self.shared.close_queue();
    }
}

/// µs since the epoch.
pub fn now_us() -> i64 {
    oojuro_capture::time::now_us()
}
