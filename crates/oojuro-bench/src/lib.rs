//! Shared benchmark harness for every Oojuro research path.
//!
//! An engine plugs in as an [`Arm`]: in-process through the trait, or as any
//! executable that speaks the JSON-lines protocol in [`protocol`] on
//! stdin/stdout (`oojuro-bench serve --arm <name>` is the reference server).
//! Suites (BEIR, turn-loop replay, memory QA, gates) drive arms through
//! [`ArmHandle`] and write one scorecard JSON per run; `report` compares them.
pub mod arms;
pub mod beir;
pub mod gates;
pub mod memqa;
pub mod metrics;
mod owned_scope;
pub mod protocol;
pub mod report;
pub mod scorecard;
pub mod sys;
pub mod turnloop;

pub use protocol::{ArmInfo, Doc, Hit, Request, Response, SearchReq};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Instant,
};

/// The engine interface every path implements. `append_batch` must return only
/// after the batch is durable (survives a crash) and visible to the next
/// `search`. `search` must never return a record whose `valid_at` exceeds the
/// query's `valid_at` or whose `recorded_at` exceeds `known_at`, and must return
/// at most the latest visible revision per `key`.
pub trait Arm {
    fn info(&self) -> ArmInfo;
    fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<()>;
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()>;
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>>;
    /// Exact original text for each id, `None` when absent.
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>>;
    /// Records with `from <= valid_at <= to`, in valid-time order.
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>>;
    fn stats(&mut self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }
    fn close(&mut self) -> Result<()>;
}

/// Serve one arm over the JSON-lines protocol until stdin closes.
pub fn serve(mut arm: Box<dyn Arm>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let started = Instant::now();
        let mut response = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::error(format!("bad request: {e}")),
            Ok(request) => dispatch(arm.as_mut(), request),
        };
        response.elapsed_us = started.elapsed().as_micros() as u64;
        serde_json::to_writer(&mut out, &response)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    arm.close()
}

fn dispatch(arm: &mut dyn Arm, request: Request) -> Response {
    let mut r = Response::default();
    let outcome: Result<()> = (|| {
        match request {
            Request::Hello => r.info = Some(arm.info()),
            Request::Open { dir, config } => arm.open(Path::new(&dir), &config)?,
            Request::AppendBatch { docs } => arm.append_batch(&docs)?,
            Request::Search { query } => r.hits = Some(arm.search(&query)?),
            Request::Get { ids } => r.texts = Some(arm.get(&ids)?),
            Request::GetRange { from, to, limit } => r.docs = Some(arm.get_range(from, to, limit)?),
            Request::Stats => r.stats = Some(arm.stats()?),
            Request::Close => arm.close()?,
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            r.ok = true;
            r
        }
        Err(e) => Response::error(e.to_string()),
    }
}

/// Resource limits for an isolated arm process (a transient systemd scope).
#[derive(Clone, Debug)]
pub struct Isolation {
    pub memory_max: String,
}

/// An arm living in a child process; the driver talks JSON lines to it.
pub struct ProcessArm {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    scope: Option<owned_scope::OwnedScope>,
    reaped: bool,
    stdout: BufReader<ChildStdout>,
    pub cgroup: Option<PathBuf>,
    pub argv: Vec<String>,
}

impl ProcessArm {
    pub fn spawn(label: &str, argv: &[String], isolation: Option<&Isolation>, stderr: &Path) -> Result<Self> {
        let mut scope_unit = None;
        let mut command = if let Some(iso) = isolation {
            let unit = format!(
                "oojuro-arm-{}-{}-{}",
                label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>(),
                std::process::id(),
                sys::unique_suffix()
            );
            scope_unit = Some(format!("{unit}.scope"));
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "--collect"])
                .arg(format!("--unit={unit}"))
                .arg(format!("--property=MemoryMax={}", iso.memory_max))
                .arg("--property=MemorySwapMax=0")
                .arg("--property=TasksMax=512")
                .arg("--property=RuntimeMaxSec=600")
                .arg("--property=TimeoutStopSec=5")
                .arg("--")
                .args(argv);
            c
        } else {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        };
        let log = std::fs::File::create(stderr)?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("spawn {argv:?}: {e}"))?;
        let stdin = BufWriter::new(child.stdin.take().ok_or("no stdin")?);
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut arm = Self { child, stdin: Some(stdin), stdout, scope: None, reaped: false, cgroup: None, argv: argv.to_vec() };
        // The hello round trip guarantees systemd-run has exec'd into the scope.
        arm.call(&Request::Hello)?;
        if let Some(unit) = scope_unit {
            let scope = owned_scope::OwnedScope::capture(unit, arm.child.id())?;
            arm.cgroup = Some(scope.cgroup.clone());
            arm.scope = Some(scope);
        }
        Ok(arm)
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn call(&mut self, request: &Request) -> Result<Response> {
        let stdin = self.stdin.as_mut().ok_or("arm input already closed")?;
        serde_json::to_writer(&mut *stdin, request)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            let status = self.child.try_wait()?;
            self.reaped = status.is_some();
            return Err(format!(
                "arm process exited ({status:?}); see its stderr log. An exit under a MemoryMax scope is usually an OOM kill"
            )
            .into());
        }
        let response: Response = serde_json::from_str(&line)?;
        if !response.ok {
            return Err(format!("arm error: {}", response.error.unwrap_or_default()).into());
        }
        Ok(response)
    }
    /// Write a request without reading its answer (crash tests kill next).
    pub fn send_only(&mut self, request: &Request) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or("arm input already closed")?;
        serde_json::to_writer(&mut *stdin, request)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }
    /// Kill every process in the exact isolated scope before recovery. A
    /// leader-only kill cannot qualify an engine with server descendants.
    pub fn kill(&mut self) -> Result<()> {
        if let Some(scope) = &self.scope {
            scope.kill_all()?;
        } else if !self.reaped {
            // Single-process compatibility path. Forking arms require isolation.
            self.child.kill()?;
        }
        self.stdin.take();
        self.wait_leader(std::time::Duration::from_secs(5))?;
        Ok(())
    }
    fn wait_leader(&mut self, timeout: std::time::Duration) -> Result<Option<std::process::ExitStatus>> {
        if self.reaped { return Ok(None); }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                return Ok(Some(status));
            }
            if Instant::now() >= deadline { return Err("benchmark leader did not exit by deadline".into()); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    pub fn finish(mut self) -> Result<()> {
        self.stdin.take();
        let status = self.wait_leader(std::time::Duration::from_secs(30))?
            .ok_or("benchmark leader already reaped before finish")?;
        if !status.success() { return Err(format!("arm exited with {status}").into()); }
        if let Some(scope) = &self.scope {
            scope.wait_empty(std::time::Duration::from_secs(5))?;
        }
        Ok(())
    }
}
impl Drop for ProcessArm {
    fn drop(&mut self) {
        // Error paths retain no live owned leader/scope. A cleanup failure is
        // still an outer-controller failure; the finite scope is the last bound.
        let _ = self.kill();
    }
}

/// What a suite holds: an in-process arm, or a child process.
pub enum ArmHandle {
    InProcess(Box<dyn Arm>),
    Process(ProcessArm),
}

/// Timing of one call: inside the arm, and including protocol round trip.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub arm_us: u64,
    pub total_us: u64,
}

impl ArmHandle {
    fn run(&mut self, request: Request) -> Result<(Response, Timing)> {
        let started = Instant::now();
        let response = match self {
            ArmHandle::Process(p) => p.call(&request)?,
            ArmHandle::InProcess(a) => {
                let mut r = dispatch(a.as_mut(), request);
                r.elapsed_us = started.elapsed().as_micros() as u64;
                if !r.ok {
                    return Err(format!("arm error: {}", r.error.unwrap_or_default()).into());
                }
                r
            }
        };
        let total_us = started.elapsed().as_micros() as u64;
        let arm_us = response.elapsed_us.min(total_us);
        Ok((response, Timing { arm_us, total_us }))
    }
    pub fn info(&mut self) -> Result<ArmInfo> {
        Ok(self.run(Request::Hello)?.0.info.unwrap_or_default())
    }
    pub fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<Timing> {
        let dir = dir.to_str().ok_or("non-UTF-8 store path")?.to_string();
        Ok(self.run(Request::Open { dir, config: config.clone() })?.1)
    }
    pub fn append_batch(&mut self, docs: Vec<Doc>) -> Result<Timing> {
        Ok(self.run(Request::AppendBatch { docs })?.1)
    }
    pub fn search(&mut self, query: SearchReq) -> Result<(Vec<Hit>, Timing)> {
        let (r, t) = self.run(Request::Search { query })?;
        Ok((r.hits.unwrap_or_default(), t))
    }
    pub fn get(&mut self, ids: Vec<String>) -> Result<Vec<Option<String>>> {
        Ok(self.run(Request::Get { ids })?.0.texts.unwrap_or_default())
    }
    pub fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        Ok(self.run(Request::GetRange { from, to, limit })?.0.docs.unwrap_or_default())
    }
    pub fn stats(&mut self) -> Result<serde_json::Value> {
        Ok(self.run(Request::Stats)?.0.stats.unwrap_or_default())
    }
    pub fn close(&mut self) -> Result<Timing> {
        Ok(self.run(Request::Close)?.1)
    }
    pub fn memory(&self) -> Option<sys::Memory> {
        match self {
            ArmHandle::Process(p) => Some(sys::memory(p.pid(), p.cgroup.as_deref())),
            ArmHandle::InProcess(_) => None,
        }
    }
    pub fn process(&mut self) -> Option<&mut ProcessArm> {
        match self {
            ArmHandle::Process(p) => Some(p),
            ArmHandle::InProcess(_) => None,
        }
    }
    pub fn finish(self) -> Result<()> {
        match self {
            ArmHandle::Process(p) => p.finish(),
            ArmHandle::InProcess(_) => Ok(()),
        }
    }
}

/// `label=spec` where spec is `nine`, `tantivy`, `sqlite` or `cmd:<argv...>`.
#[derive(Clone, Debug)]
pub struct ArmSpec {
    pub label: String,
    pub spec: String,
}

impl ArmSpec {
    pub fn parse(s: &str) -> Result<Self> {
        let (label, spec) = match s.split_once('=') {
            Some((l, r)) if !l.contains(':') && !l.contains(' ') => (l.to_string(), r.to_string()),
            _ => (s.split([':', ' ']).next().unwrap_or(s).to_string(), s.to_string()),
        };
        if label.is_empty() || spec.is_empty() {
            return Err(format!("bad arm spec {s:?}").into());
        }
        Ok(Self { label, spec })
    }
    /// Start the arm. Built-in arms run in a child `serve` process of this
    /// binary unless `in_process`; `cmd:` arms always run as their own process.
    pub fn start(&self, in_process: bool, isolation: Option<&Isolation>, stderr: &Path) -> Result<ArmHandle> {
        if let Some(cmd) = self.spec.strip_prefix("cmd:") {
            let argv: Vec<String> = cmd.split_whitespace().map(String::from).collect();
            if argv.is_empty() {
                return Err("empty cmd: arm".into());
            }
            return Ok(ArmHandle::Process(ProcessArm::spawn(&self.label, &argv, isolation, stderr)?));
        }
        if in_process {
            return Ok(ArmHandle::InProcess(arms::builtin(&self.spec)?));
        }
        arms::builtin(&self.spec)?; // validate the name before spawning
        let exe = std::env::current_exe()?.to_str().ok_or("exe path")?.to_string();
        let argv = vec![exe, "serve".into(), "--arm".into(), self.spec.clone()];
        Ok(ArmHandle::Process(ProcessArm::spawn(&self.label, &argv, isolation, stderr)?))
    }
}
