//! `oojurod`: the Oojuro recorder (capture: `docs/CAPTURE.md`; front door:
//! `docs/FRONT_DOOR.md`).
//!
//! ```text
//! oojurod import [--store DIR] [options] [DIR...]   one-shot import, then exit
//! oojurod tail   [--store DIR] [options] [DIR...]   follow logs and serve the journal on its socket
//! oojurod otel   [--store DIR] [--otel-listen ADDR]  OpenTelemetry receiver only (no logs), same socket
//! oojurod stats  [--store DIR]                      what the store holds (store must be free)
//! oojurod proxy  [--listen ADDR] [--writer NAME] [--anthropic-upstream URL] [--openai-upstream URL]
//!                                                   the recording model-API proxy on its own
//! oojurod audit | compact | verify                  see docs/CAPTURE.md
//! ```
//! DIRs default to `~/.claude/projects` and `~/.codex/sessions`. The store
//! defaults to `--store`, else `$OOJURO_CAPTURE_STORE`, else
//! `$OOJURO_HOME/journal` (`~/.oojuro/journal`). Logs are only ever opened
//! read-only. Settings (`record`, `oojuro pause`, `exclude`, thinking) come
//! from `$OOJURO_HOME/config.toml` (or `--config`), project `.oojuro.toml`
//! files and `[agents."<writer>"]` sections, re-read while running.
//!
//! `tail` serves other processes (the `oojuro` CLI, MCP servers, the
//! library) on a Unix socket next to the store (`--socket`, default
//! `<store>/../oojurod.sock`; `--no-socket` to disable): reads in parallel
//! with ingest, writes through this process (the store's single writer).
//!
//! The model API proxy (`docs/CAPTURE.md`, "Model API proxy") runs inside
//! `tail` when `[proxy] enabled = true`, so the daemon is one process.
//! `oojurod proxy` runs it alone: when a recorder serves the journal it
//! sends its batches over that recorder's socket, otherwise it owns the
//! journal itself (and serves the socket) without tailing any logs.
use oojuro::{
    client::{Oojuro, Via},
    config::{parse_listen, proxy_settings},
    journal::{self, Journal, Owner, store_options_for},
    otel,
    paths::Paths,
    policy::RecordingPolicy,
    proto,
    proxy_sink::{self, OwnerSink, SINK_TIMEOUT, SocketSink},
    server::{self, Server, WriteJob},
};
use oojuro_capture::{
    Result,
    ingest::{Capture, Options, TRACE_DIR},
    kind,
    state::State,
    stats::Stats,
};
use oojuro_trace::{Filter, TraceDb};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const USAGE: &str =
    "usage: oojurod <import|tail|proxy|otel|stats|audit|compact|verify> [--store DIR] [--home DIR] [--config FILE]
                [--socket PATH | --no-socket] [--otel-listen ADDR] [--batch-mb N] [--heavy-level N] [--no-dedupe-stats]
                [--poll-ms N] [--idle-exit-secs N] [--max-secs N] [--sample N] [--seed N]
                [--listen ADDR] [--writer NAME] [--anthropic-upstream URL] [--openai-upstream URL] [DIR...]";

struct Args {
    cmd: String,
    paths: Paths,
    socket: bool,
    dirs: Vec<PathBuf>,
    batch_mb: usize,
    heavy_level: Option<i32>,
    dedupe: bool,
    poll_ms: u64,
    idle_exit_secs: Option<u64>,
    max_secs: Option<u64>,
    seed: u64,
    listen: Option<String>,
    writer: Option<String>,
    anthropic: Option<String>,
    openai: Option<String>,
    /// Run the OpenTelemetry receiver on this address (else per `[otel]`).
    otel_listen: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or(USAGE)?;
    let (mut home, mut store, mut socket_path, mut config) = (None, None, None, None);
    let mut a = Args {
        cmd,
        paths: Paths::under(Path::new(".")),
        socket: true,
        dirs: Vec::new(),
        batch_mb: 16,
        heavy_level: None,
        dedupe: true,
        poll_ms: 500,
        idle_exit_secs: None,
        max_secs: None,
        seed: 0x5EED,
        listen: None,
        writer: None,
        anthropic: None,
        openai: None,
        otel_listen: None,
    };
    while let Some(x) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{x} needs a value"));
        match x.as_str() {
            "--store" => store = Some(PathBuf::from(val()?)),
            "--home" => home = Some(PathBuf::from(val()?)),
            "--config" => config = Some(PathBuf::from(val()?)),
            "--socket" => socket_path = Some(PathBuf::from(val()?)),
            "--no-socket" => a.socket = false,
            "--batch-mb" => a.batch_mb = val()?.parse()?,
            "--heavy-level" => a.heavy_level = Some(val()?.parse()?),
            "--no-dedupe-stats" => a.dedupe = false,
            "--poll-ms" => a.poll_ms = val()?.parse()?,
            "--idle-exit-secs" => a.idle_exit_secs = Some(val()?.parse()?),
            "--max-secs" => a.max_secs = Some(val()?.parse()?),
            "--seed" => a.seed = val()?.parse()?,
            "--listen" => a.listen = Some(val()?),
            "--writer" => a.writer = Some(val()?),
            "--anthropic-upstream" => a.anthropic = Some(val()?),
            "--openai-upstream" => a.openai = Some(val()?),
            "--otel-listen" => a.otel_listen = Some(val()?),
            "--sample" => a.batch_mb = val()?.parse()?,
            "-h" | "--help" => return Err(USAGE.into()),
            d if !d.starts_with("--") => a.dirs.push(d.into()),
            other => return Err(format!("unknown option {other}\n{USAGE}").into()),
        }
    }
    let store = store.or_else(|| std::env::var_os("OOJURO_CAPTURE_STORE").map(PathBuf::from));
    a.paths = Paths::resolve(home, store, socket_path, config)?;
    if a.dirs.is_empty() && a.cmd != "proxy" && a.cmd != "otel" {
        let home = oojuro::paths::user_home()?;
        a.dirs = vec![home.join(".claude/projects"), home.join(".codex/sessions")];
    }
    Ok(a)
}

fn options(a: &Args) -> Options {
    let mut o = store_options_for(&a.paths.store);
    if let Some(l) = a.heavy_level {
        o.trace.archive_heavy_level = l;
    }
    o.batch_bytes = a.batch_mb << 20;
    o.dedupe_stats = a.dedupe;
    o
}

fn mb(b: u64) -> f64 {
    b as f64 / 1e6
}

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn top<V: Copy>(m: &std::collections::BTreeMap<String, V>, n: usize, key: impl Fn(&V) -> u64) -> Vec<(String, V)> {
    let mut v: Vec<(String, V)> = m.iter().map(|(k, x)| (k.clone(), *x)).collect();
    v.sort_by(|a, b| key(&b.1).cmp(&key(&a.1)).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

fn print_summary(s: &Stats) {
    println!(
        "files {} (new {}, rotated {}), lines {}, raw {:.1} MB, partial tail {} B",
        s.files,
        s.files_new,
        s.files_rotated,
        s.lines,
        mb(s.raw_bytes),
        s.partial_tail_bytes
    );
    println!(
        "records {} text {:.1} MB, commits {}, recovered {} skipped-existing {}",
        s.records,
        mb(s.text_bytes),
        s.commits,
        s.recovered,
        s.skipped_existing
    );
    println!("by kind (records, text MB):");
    for k in kind::NAMES {
        let (n, b) = s.kinds.get(k).copied().unwrap_or_default();
        println!("  {k:12} {n:>10} {:>10.1}", mb(b));
    }
    println!("user_input senders: {:?}", s.senders);
    println!("writers: {}", s.writers.len());
    println!(
        "withheld by settings: {:?}",
        s.withheld.iter().map(|(k, x)| format!("{k}={}x/{:.1}MB", x.0, mb(x.1))).collect::<Vec<_>>()
    );
    println!("redactions: {:?} (total {})", s.redactions, s.redactions.values().sum::<u64>());
    println!("bad lines {}, images {} ({:.1} MB)", s.parse.bad_lines, s.parse.images.0, mb(s.parse.images.1));
    let ps = s.parse.persisted;
    println!("saved tool outputs: {} read in full ({:.1} MB), {} kept as preview", ps.0, mb(ps.2), ps.1);
    println!("unknown entry types: {:?}", s.parse.unknown);
    let noise: u64 = s.parse.noise.values().map(|x| x.1).sum();
    println!(
        "noise: {} entries {:.1} MB; top: {:?}",
        s.parse.noise.values().map(|x| x.0).sum::<u64>(),
        mb(noise),
        top(&s.parse.noise, 8, |x| x.1).iter().map(|(k, x)| format!("{k}={:.1}MB", mb(x.1))).collect::<Vec<_>>()
    );
    println!(
        "re-sent (not stored): {:?}",
        s.parse.resent.iter().map(|(k, x)| format!("{k}={}x/{:.1}MB", x.0, mb(x.1))).collect::<Vec<_>>()
    );
    let d = &s.dedupe;
    if d.records > 0 {
        println!(
            "dedupe: exact dup records global {} ({:.1} MB, {:.1}% of text), same-writer {} ({:.1} MB, {:.1}%), >=1KiB same-writer {} ({:.1} MB)",
            d.dup_global.0,
            mb(d.dup_global.1),
            100.0 * d.dup_global.1 as f64 / d.bytes.max(1) as f64,
            d.dup_writer.0,
            mb(d.dup_writer.1),
            100.0 * d.dup_writer.1 as f64 / d.bytes.max(1) as f64,
            d.dup_writer_1k.0,
            mb(d.dup_writer_1k.1)
        );
        println!(
            "dedupe by kind (same-writer): {:?}",
            d.dup_writer_by_kind.iter().map(|(k, x)| format!("{k}={}x/{:.1}MB", x.0, mb(x.1))).collect::<Vec<_>>()
        );
        println!(
            "dedupe top tools: {:?}",
            top(&d.dup_writer_by_tool, 8, |x| x.1)
                .iter()
                .map(|(k, x)| format!("{k}={:.1}MB", mb(x.1)))
                .collect::<Vec<_>>()
        );
        println!(
            "dedupe top system subtypes: {:?}",
            top(&d.dup_writer_by_sub, 8, |x| x.1)
                .iter()
                .map(|(k, x)| format!("{k}={:.1}MB", mb(x.1)))
                .collect::<Vec<_>>()
        );
        println!(
            "chunk dedupe (FastCDC ~1KiB, same writer): unique {:.1} of {:.1} MB ({:.1}% saved)",
            mb(d.cdc_unique_bytes),
            mb(d.cdc_bytes),
            100.0 * (1.0 - d.cdc_unique_bytes as f64 / d.cdc_bytes.max(1) as f64)
        );
    }
    if s.store_bytes > 0 {
        println!(
            "store {:.1} MB on disk = {:.3} of raw, {:.3} of stored text; breakdown {:?}",
            mb(s.store_bytes),
            s.store_bytes as f64 / s.raw_bytes.max(1) as f64,
            s.store_bytes as f64 / s.text_bytes.max(1) as f64,
            s.store_breakdown
        );
    }
    if s.wall_secs > 0.0 {
        println!("wall {:.1} s, {:.1} MB/s raw (loadavg {})", s.wall_secs, mb(s.raw_bytes) / s.wall_secs, loadavg());
    }
}

/// `<name>.json` (latest run) and `<name>-<unix secs>.json` (kept).
fn write_stats(store: &Path, name: &str, s: &Stats) -> Result<()> {
    let body = serde_json::to_vec_pretty(s)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    std::fs::write(store.join(format!("{name}-{now}.json")), &body)?;
    std::fs::write(store.join(format!("{name}.json")), &body)?;
    Ok(())
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// SIGTERM / SIGINT: finish the current pass, flush, save, remove the socket.
fn install_signals() {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
}

/// Answer queued writes as the store's owner.
fn serve_writes(cap: &mut Capture, reader: &journal::Reader, rx: &mpsc::Receiver<WriteJob>, wait: Duration) {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let job = if left.is_zero() { rx.try_recv().ok() } else { rx.recv_timeout(left).ok() };
        let Some(job) = job else { return };
        let mut o = Owner { cap: &mut *cap, reader };
        let r = proto::execute_owned(&mut o, job.req).map_err(|e| e.to_string());
        o.sync_writers();
        let _ = job.reply.send(r);
        if STOP.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// `[proxy]` settings with the command-line overrides (first listener):
/// (enabled, config).
fn proxy_config(a: &Args) -> Result<(bool, oojuro_proxy::Config)> {
    let ps = proxy_settings(&a.paths.config, &a.paths.store)?;
    let mut cfg = ps.config;
    let l = &mut cfg.listeners[0];
    if let Some(x) = &a.listen {
        l.listen = parse_listen(x).map_err(|e| format!("--listen {x}: {e}"))?;
    }
    if let Some(w) = &a.writer {
        oojuro::paths::check_writer(w)?;
        l.writer = Some(w.clone());
    }
    if let Some(u) = &a.anthropic {
        l.anthropic_upstream = u.clone();
    }
    if let Some(u) = &a.openai {
        l.openai_upstream = u.clone();
    }
    for l in &cfg.listeners {
        if !l.listen.ip().is_loopback() {
            return Err(format!("proxy: {} is not a loopback address (127.0.0.1 or ::1 only)", l.listen).into());
        }
        for u in [&l.anthropic_upstream, &l.openai_upstream] {
            oojuro_proxy::forward::Upstream::parse(u)?;
        }
    }
    Ok((ps.enabled, cfg))
}

/// One line of proxy counters when they changed.
fn log_proxy(p: &oojuro_proxy::Proxy, shown: &mut String) {
    let s = p.stats();
    let line = format!(
        "proxy: requests {} recorded {} new {} dup {} written {} dropped(queue {} journal {} too-large {}) upstream-unreachable {} parse-errors {}",
        s["requests"],
        s["recorded_calls"],
        s["records_new"],
        s["records_dup"],
        s["records_written"],
        s["dropped_queue_full"],
        s["dropped_journal"],
        s["dropped_too_large"],
        s["upstream_unreachable"],
        s["parse_errors"]
    );
    if line != *shown {
        eprintln!("{line}");
        *shown = line;
    }
}

/// The recorder: own the store, follow `dirs`, serve the socket, and run
/// the model API proxy when `proxy` is given (`proxy_only`: a proxy that
/// cannot start is an error).
fn recorder(a: &Args, dirs: &[PathBuf], proxy: Option<oojuro_proxy::Config>, proxy_only: bool) -> Result<()> {
    let store = a.paths.store.clone();
    let record_tailed = proxy.as_ref().is_some_and(|c| c.record_tailed);
    journal::private_dir(&store)?;
    let mut cap = Capture::open(&store, options(a))?;
    cap.set_policy(Box::new(RecordingPolicy::new(&a.paths.config)));
    let mut j = Journal::from_capture(cap);
    let reader = j.reader();
    let (tx, rx) = mpsc::channel::<WriteJob>();
    let started = oojuro_capture::time::now_us();
    let receiver = start_otel(a, &tx)?;
    // agents whose transcripts this recorder reads: the proxy leaves their calls alone
    let tailed = {
        let st = &j.capture().state;
        proxy_sink::tailed_formats(dirs, st.files.iter().map(|(p, f)| (p.as_str(), f.format)))
    };
    let server = if a.socket {
        let l = server::bind(&a.paths.socket)?;
        let canonical = std::fs::canonicalize(&store)?;
        // the directories tailed: a hook for a transcript under one of them records nothing
        let watch: Vec<PathBuf> =
            dirs.iter().filter_map(|d| std::fs::canonicalize(d).or_else(|_| std::path::absolute(d)).ok()).collect();
        let hello = serde_json::json!({
            "version": proto::VERSION, "store": canonical, "pid": std::process::id(), "started_us": started,
            "watch": watch, "otel": receiver.as_ref().map(|r| r.addr().to_string()),
            "tailed": proxy_sink::names(&tailed),
        });
        Some(Server::start(l, &a.paths.socket, reader.clone(), tx.clone(), hello))
    } else {
        None
    };
    let proxy = match proxy {
        Some(mut cfg) => {
            cfg.tailed = tailed.clone();
            match oojuro_proxy::Proxy::start(cfg, Box::new(OwnerSink { writes: tx.clone(), timeout: SINK_TIMEOUT })) {
                Ok(p) => {
                    eprintln!(
                        "proxy: listening on {:?}; transcripts tailed for {:?} (their calls are not recorded{})",
                        p.local_addrs(),
                        proxy_sink::names(&p.tailed()),
                        if record_tailed { ": overridden by record_tailed" } else { "" }
                    );
                    Some(p)
                }
                Err(e) if proxy_only => return Err(e),
                Err(e) => {
                    eprintln!("oojurod: proxy not started: {e}");
                    None
                }
            }
        }
        None => None,
    };
    drop(tx);
    install_signals();
    let cap = j.capture();
    eprintln!(
        "tail {:?} -> {:?} every {} ms (recovered {}); serving {}",
        dirs,
        store,
        a.poll_ms,
        cap.stats.recovered,
        if a.socket { a.paths.socket.display().to_string() } else { "nothing (--no-socket)".into() }
    );
    let start = Instant::now();
    let mut proxy_logged = Instant::now();
    let mut proxy_shown = String::new();
    let mut idle = Instant::now();
    let (idle_exit, max) = (a.idle_exit_secs.map(Duration::from_secs), a.max_secs.map(Duration::from_secs));
    let mut last_records = 0;
    let mut idle_since = Instant::now();
    let mut flushed = true;
    loop {
        let n = cap.poll_with(dirs, &mut |c| {
            serve_writes(c, &reader, &rx, Duration::ZERO);
            Ok(())
        })?;
        journal::sync_writers(cap, &reader);
        if n > 0 {
            idle = Instant::now();
            idle_since = Instant::now();
            flushed = false;
        } else if !flushed && idle_since.elapsed() >= Duration::from_secs(30) {
            cap.db.flush()?;
            flushed = true;
        }
        cap.save_if_dirty()?;
        if cap.stats.records != last_records {
            eprintln!(
                "tail: +{} records (total {}, raw {} B)",
                cap.stats.records - last_records,
                cap.stats.records,
                cap.stats.raw_bytes
            );
            last_records = cap.stats.records;
        }
        let done = idle_exit.is_some_and(|d| idle.elapsed() >= d) || max.is_some_and(|d| start.elapsed() >= d);
        if done || STOP.load(Ordering::SeqCst) {
            break;
        }
        if let Some(p) = &proxy
            && proxy_logged.elapsed() >= Duration::from_secs(60)
        {
            proxy_logged = Instant::now();
            log_proxy(p, &mut proxy_shown);
        }
        // wait for the next pass, answering writes as they come
        serve_writes(cap, &reader, &rx, Duration::from_millis(a.poll_ms));
    }
    if let Some(s) = server {
        s.stop();
    }
    if let Some(r) = receiver {
        eprintln!("otel: {}", r.stats.snapshot());
        r.stop();
    }
    if let Some(p) = proxy {
        // calls in flight finish, then the queued batches are written here
        let stats = p.stats();
        let h = p.shutdown_in_background(Duration::from_secs(10));
        while !h.is_finished() {
            serve_writes(cap, &reader, &rx, Duration::from_millis(50));
        }
        let _ = h.join();
        eprintln!("proxy: {stats:?}");
    }
    serve_writes(cap, &reader, &rx, Duration::ZERO);
    cap.stats.partial_tail_bytes = cap.pending_tail_bytes();
    cap.finish()?;
    cap.stats.wall_secs = start.elapsed().as_secs_f64();
    print_summary(&cap.stats);
    write_stats(&store, "tail-stats", &cap.stats)?;
    Ok(())
}

/// The OpenTelemetry receiver, when `oojurod otel`, `--otel-listen` or
/// `[otel] enabled = true` asks for it; it appends through the write loop.
fn start_otel(a: &Args, tx: &mpsc::Sender<WriteJob>) -> Result<Option<otel::Receiver>> {
    let cfg = oojuro::config::Config::load(&a.paths.config, None).map_err(|e| format!("settings: {e}"))?;
    let mut s = otel::OtelSettings::from_config(&cfg)?;
    if let Some(l) = &a.otel_listen {
        s.listen = l.clone();
        s.enabled = true;
    }
    if a.cmd == "otel" {
        s.enabled = true;
    }
    if !s.enabled {
        return Ok(None);
    }
    let r = otel::Receiver::start(&s, std::sync::Arc::new(otel::ChannelSink::new(tx.clone())))?;
    eprintln!("otel: OTLP/HTTP on http://{} (/v1/traces, /v1/logs)", r.addr());
    Ok(Some(r))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("oojurod: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let a = parse_args()?;
    let store = a.paths.store.clone();
    match a.cmd.as_str() {
        "import" => {
            let t = Instant::now();
            journal::private_dir(&store)?;
            let mut cap = Capture::open(&store, options(&a))?;
            cap.set_policy(Box::new(RecordingPolicy::new(&a.paths.config)));
            eprintln!(
                "import {:?} -> {:?} (recovered {} records past the saved state)",
                a.dirs, store, cap.stats.recovered
            );
            cap.poll(&a.dirs)?;
            cap.stats.partial_tail_bytes = cap.pending_tail_bytes();
            let ingest_secs = t.elapsed().as_secs_f64();
            cap.finish()?;
            cap.stats.wall_secs = t.elapsed().as_secs_f64();
            print_summary(&cap.stats);
            println!("ingest (before final flush) {ingest_secs:.1} s");
            write_stats(&store, "import-stats", &cap.stats)?;
        }
        "tail" | "otel" => {
            // `oojurod otel`: the OpenTelemetry receiver only (no logs read, no proxy)
            let proxy = match proxy_config(&a) {
                _ if a.cmd == "otel" => None,
                Ok((true, cfg)) => Some(cfg),
                Ok((false, _)) => None,
                Err(e) => {
                    eprintln!("oojurod: proxy not started: {e}");
                    None
                }
            };
            recorder(&a, &a.dirs, proxy, false)?;
        }
        "proxy" => {
            let (_, mut cfg) = proxy_config(&a)?;
            let (oj, hello) = Oojuro::open_within(a.paths.clone(), Duration::from_secs(5));
            match (oj.via, hello) {
                (Via::Recorder { socket, pid }, Some(h)) => {
                    // a recorder owns the journal: batches go over its socket, and the
                    // agents whose transcripts it tails are left to it
                    cfg.tailed = proxy_sink::tailed_in_hello(&h);
                    let record_tailed = cfg.record_tailed;
                    let p = oojuro_proxy::Proxy::start(cfg, Box::new(SocketSink { socket: socket.clone() }))?;
                    eprintln!(
                        "proxy: listening on {:?}; journaling through the recorder (pid {pid}) on {}; transcripts tailed for {:?} (their calls are not recorded{})",
                        p.local_addrs(),
                        socket.display(),
                        proxy_sink::names(&p.tailed()),
                        if record_tailed { ": overridden by record_tailed" } else { "" }
                    );
                    install_signals();
                    let start = Instant::now();
                    let mut last = Instant::now();
                    let mut asked = Instant::now();
                    let mut shown = String::new();
                    while !STOP.load(Ordering::SeqCst)
                        && !a.max_secs.is_some_and(|m| start.elapsed() >= Duration::from_secs(m))
                    {
                        std::thread::sleep(Duration::from_millis(200));
                        if last.elapsed() >= Duration::from_secs(60) {
                            last = Instant::now();
                            log_proxy(&p, &mut shown);
                        }
                        // a restarted recorder may tail other directories; while none
                        // answers, the last answer stands
                        if asked.elapsed() >= Duration::from_secs(5) {
                            asked = Instant::now();
                            if let (_, Some(h)) = Oojuro::open_within(a.paths.clone(), Duration::from_secs(2)) {
                                let now = proxy_sink::tailed_in_hello(&h);
                                if now != p.tailed() {
                                    eprintln!("proxy: transcripts tailed for {:?} now", proxy_sink::names(&now));
                                    p.set_tailed(&now);
                                }
                            }
                        }
                    }
                    let stats = p.stats();
                    p.shutdown(Duration::from_secs(10));
                    println!("proxy: {stats:?}");
                }
                // no recorder: own the journal, tail nothing, record every agent
                _ => recorder(&a, &[], Some(cfg), true)?,
            }
        }
        "audit" => {
            // false-positive audit: sampled hits per kind, shown as shape + masked context
            let per = a.batch_mb.max(1); // --sample N (hits kept per kind)
            let (red, rc, raw, pc) = oojuro_capture::ingest::audit_redactions(&a.dirs, per, a.seed)?;
            println!("audit over {:.1} MB raw: {:?}", mb(raw), rc);
            println!("entry types: {:?}", pc.entries);
            println!("unknown: {:?}; bad lines {}", pc.unknown, pc.bad_lines);
            let g = red.audit().expect("audit");
            for (kind, hits) in &g.samples {
                println!("== {kind}: {} hits, {} sampled", g.seen.get(kind).copied().unwrap_or(0), hits.len());
                for h in hits {
                    let clean = |x: &str| x.replace(['\n', '\r', '\t'], " ");
                    println!(
                        "  len={:<4} H={:.2} shape={:<28} | {}<<{}>>{}",
                        h.len,
                        h.entropy,
                        h.shape.chars().take(28).collect::<String>(),
                        clean(&h.before),
                        h.kind,
                        clean(&h.after)
                    );
                }
            }
        }
        "compact" => {
            // major compaction: every writer store into one heavy-level run with a dictionary
            let t = Instant::now();
            let mut trace = store_options_for(&store).trace;
            if let Some(l) = a.heavy_level {
                trace.archive_heavy_level = l;
            }
            let _lock = oojuro_capture::ingest::lock_store(&store)?;
            let db = TraceDb::open(&store.join(TRACE_DIR), trace)?;
            let before = db.disk_bytes();
            db.compact_archive(2)?;
            db.flush()?;
            let after = db.disk_bytes();
            println!(
                "compact: {:.1} MB -> {:.1} MB in {:.1} s (loadavg {}); breakdown {:?}",
                mb(before),
                mb(after),
                t.elapsed().as_secs_f64(),
                loadavg(),
                db.disk_breakdown()
            );
        }
        "verify" => {
            // stored text: placeholders per kind (total, distinct secrets) and a second
            // redaction pass, which must find nothing left to mask
            let _lock = oojuro_capture::ingest::lock_store(&store)?;
            let db = TraceDb::open(&store.join(TRACE_DIR), store_options_for(&store).trace)?;
            let red = oojuro_capture::redact::Redactor::new();
            let re = regex::Regex::new(r"\[REDACTED:([a-z_]+):([0-9a-f]{8})\]")?;
            let mut per: std::collections::BTreeMap<String, (u64, std::collections::HashSet<String>)> =
                Default::default();
            let mut residual = oojuro_capture::redact::Counts::new();
            let mut residual_records = 0u64;
            let n = db.export_rows(|e| {
                for c in re.captures_iter(&e.text) {
                    let x = per.entry(c[1].to_string()).or_default();
                    x.0 += 1;
                    x.1.insert(c[2].to_string());
                }
                let mut rc = oojuro_capture::redact::Counts::new();
                let _ = red.redact(&e.text, &mut rc);
                if !rc.is_empty() {
                    residual_records += 1;
                    for (k, v) in rc {
                        *residual.entry(k).or_default() += v;
                    }
                }
                Ok(())
            })?;
            println!("verified {n} records");
            for (k, (t, d)) in &per {
                println!("  {k:16} placeholders {t:>6}  distinct secrets {:>5}", d.len());
            }
            println!("second pass: {residual_records} records with new hits {residual:?}");
        }
        "stats" => {
            let state = State::load(&store)?;
            let _lock = oojuro_capture::ingest::lock_store(&store)
                .map_err(|e| format!("{e} (while the recorder runs, use `oojuro stats`)"))?;
            let db = TraceDb::open(&store.join(TRACE_DIR), store_options_for(&store).trace)?;
            println!(
                "store {:?}: {} writers, {} files, next id {}",
                store,
                state.writers.len(),
                state.files.len(),
                state.next_id
            );
            let mut totals = [0u64; kind::NAMES.len()];
            for (w, agent) in &state.writers {
                let mut row = Vec::new();
                for (k, t) in totals.iter_mut().enumerate() {
                    let f = Filter {
                        agent: Some(*agent),
                        kind: Some(k as u8),
                        from_us: i64::MIN,
                        to_us: i64::MAX,
                        ..Filter::default()
                    };
                    let n = db.count(&f)?;
                    *t += n;
                    row.push(n);
                }
                println!("  {w:60} {row:?}");
            }
            println!("events by kind {:?}: {totals:?}", kind::NAMES);
            println!("disk {:.1} MB {:?}", mb(db.disk_bytes()), db.disk_breakdown());
            if let Ok(b) = std::fs::read(store.join("import-stats.json")) {
                let v: serde_json::Value = serde_json::from_slice(&b)?;
                println!(
                    "last import: raw {:.1} MB, records {}, wall {:.1} s",
                    mb(v["raw_bytes"].as_u64().unwrap_or(0)),
                    v["records"],
                    v["wall_secs"].as_f64().unwrap_or(0.0)
                );
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
