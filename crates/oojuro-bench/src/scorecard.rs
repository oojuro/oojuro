//! The one scorecard format every suite writes (schema in
//! bench/scorecard.schema.json), plus the per-arm run loop shared by suites.
use crate::{ArmHandle, ArmInfo, ArmSpec, Isolation, Result, sys};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const SCHEMA: &str = "oojuro-scorecard-v1";

/// Median and spread of one measurement across runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stat {
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub n: usize,
    pub runs: Vec<f64>,
}

impl Stat {
    pub fn of(values: &[f64]) -> Self {
        let mut v: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
        if v.is_empty() {
            return Self { median: f64::NAN, min: f64::NAN, max: f64::NAN, n: 0, runs: values.to_vec() };
        }
        v.sort_by(f64::total_cmp);
        let median = if v.len() % 2 == 1 { v[v.len() / 2] } else { (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0 };
        Self { median, min: v[0], max: v[v.len() - 1], n: v.len(), runs: values.to_vec() }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Gate {
    pub pass: bool,
    pub detail: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArmResult {
    pub label: String,
    pub spec: String,
    pub info: ArmInfo,
    pub gates: BTreeMap<String, Gate>,
    pub quality: BTreeMap<String, Stat>,
    pub storage: BTreeMap<String, Stat>,
    pub memory: BTreeMap<String, Stat>,
    pub speed: BTreeMap<String, Stat>,
    /// Per-slice quality, e.g. LongMemEval question type -> metric -> Stat.
    #[serde(default)]
    pub breakdown: BTreeMap<String, BTreeMap<String, Stat>>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scorecard {
    pub schema: String,
    pub created_at: String,
    pub harness: Value,
    pub hardware: Value,
    pub suite: String,
    pub dataset: String,
    pub split: String,
    /// Run conditions: runs, k, cache (warm|cold), build_time_included, ...
    pub settings: BTreeMap<String, Value>,
    pub arms: Vec<ArmResult>,
    /// Published numbers for context, each with "source" and "published": true.
    #[serde(default)]
    pub references: Vec<Value>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Scorecard {
    pub fn new(suite: &str, dataset: &str, split: &str, ctx: &Ctx) -> Self {
        let isolation = if ctx.in_process {
            "in-process (no cgroup memory)".to_string()
        } else {
            format!(
                "child process per run in a systemd --user scope, MemoryMax={}, MemorySwapMax=0",
                ctx.isolation.as_ref().map_or("none", |i| i.memory_max.as_str())
            )
        };
        Self {
            schema: SCHEMA.into(),
            created_at: sys::now_utc(),
            harness: json!({
                "commit": crate::arms::COMMIT,
                "binary": std::env::current_exe().ok().map(|p| p.display().to_string()),
                "args": std::env::args().collect::<Vec<_>>(),
            }),
            hardware: sys::hardware(&ctx.work),
            suite: suite.into(),
            dataset: dataset.into(),
            split: split.into(),
            settings: BTreeMap::from([
                ("runs".into(), json!(ctx.runs)),
                ("isolation".into(), json!(isolation)),
                ("bench_lock".into(), json!(if ctx.lock { "held for the whole run" } else { "NOT held: timings not comparable" })),
            ]),
            ..Self::default()
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

/// Per-run values for one arm, folded into Stats at the end.
#[derive(Default)]
pub struct Runs {
    values: BTreeMap<(String, String), Vec<f64>>,
    breakdown: BTreeMap<(String, String), Vec<f64>>,
    gates: BTreeMap<String, Vec<(bool, Value)>>,
}

impl Runs {
    /// `section` is quality | storage | memory | speed.
    pub fn add(&mut self, section: &str, name: &str, value: f64) {
        self.values.entry((section.into(), name.into())).or_default().push(value);
    }
    pub fn add_breakdown(&mut self, group: &str, name: &str, value: f64) {
        self.breakdown.entry((group.into(), name.into())).or_default().push(value);
    }
    pub fn gate(&mut self, name: &str, pass: bool, detail: Value) {
        self.gates.entry(name.into()).or_default().push((pass, detail));
    }
    /// Storage and memory ratios against the raw text bytes the arm received.
    pub fn footprint(&mut self, raw_text: u64, disk: (u64, u64), steady: Option<&sys::Memory>, end: Option<&sys::Memory>) {
        let raw = raw_text.max(1) as f64;
        self.add("storage", "raw_text_bytes", raw_text as f64);
        self.add("storage", "disk_bytes", disk.0 as f64);
        self.add("storage", "disk_alloc_bytes", disk.1 as f64);
        self.add("storage", "disk_ratio", disk.0 as f64 / raw);
        if let Some(m) = steady {
            if let Some(v) = m.cgroup_current {
                self.add("memory", "steady_cgroup_bytes", v as f64);
                self.add("memory", "steady_cgroup_ratio", v as f64 / raw);
            }
            if let Some(v) = m.cgroup_anon {
                self.add("memory", "steady_anon_bytes", v as f64);
                self.add("memory", "steady_anon_ratio", v as f64 / raw);
            }
            if let Some(v) = m.rss {
                self.add("memory", "steady_rss_bytes", v as f64);
                self.add("memory", "steady_rss_ratio", v as f64 / raw);
            }
        }
        if let Some(m) = end {
            if let Some(v) = m.cgroup_peak {
                self.add("memory", "peak_cgroup_bytes", v as f64);
                self.add("memory", "peak_cgroup_ratio", v as f64 / raw);
            }
            if let Some(v) = m.hwm {
                self.add("memory", "peak_rss_bytes", v as f64);
                self.add("memory", "peak_rss_ratio", v as f64 / raw);
            }
        }
    }
    pub fn into_result(self, spec: &ArmSpec, info: ArmInfo, error: Option<String>) -> ArmResult {
        let mut r = ArmResult { label: spec.label.clone(), spec: spec.spec.clone(), info, error, ..ArmResult::default() };
        for ((section, name), values) in self.values {
            let stat = Stat::of(&values);
            match section.as_str() {
                "quality" => r.quality.insert(name, stat),
                "storage" => r.storage.insert(name, stat),
                "memory" => r.memory.insert(name, stat),
                _ => r.speed.insert(name, stat),
            };
        }
        for ((group, name), values) in self.breakdown {
            r.breakdown.entry(group).or_default().insert(name, Stat::of(&values));
        }
        for (name, results) in self.gates {
            let pass = results.iter().all(|(p, _)| *p);
            let detail = results.iter().find(|(p, _)| !p).or(results.first()).map(|(_, d)| d.clone()).unwrap_or(Value::Null);
            r.gates.insert(name, Gate { pass, detail: json!({"runs": results.len(), "example": detail}) });
        }
        r
    }
}

/// Options shared by every suite.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub work: PathBuf,
    pub data: PathBuf,
    pub runs: usize,
    pub in_process: bool,
    pub isolation: Option<Isolation>,
    pub keep: bool,
    /// Hold the shared bench lock for timed runs (off only for smoke tests).
    pub lock: bool,
}

/// One fresh arm process with one fresh store, for one run.
pub struct Session {
    pub arm: ArmHandle,
    pub info: ArmInfo,
    pub store: PathBuf,
}

impl Ctx {
    pub fn bench_lock(&self) -> Result<Option<sys::BenchLock>> {
        if self.lock { Ok(Some(sys::bench_lock()?)) } else { Ok(None) }
    }
    pub fn session(&self, spec: &ArmSpec, run: usize, name: &str) -> Result<Session> {
        self.session_with(spec, run, name, &BTreeMap::new())
    }
    pub fn session_with(&self, spec: &ArmSpec, run: usize, name: &str, config: &BTreeMap<String, String>) -> Result<Session> {
        let dir = self.work.join(&spec.label).join(format!("run{run}-{name}"));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let store = dir.join("store");
        let mut arm = spec.start(self.in_process, self.isolation.as_ref(), &dir.join("arm.stderr"))?;
        let info = arm.info()?;
        arm.open(&store, config)?;
        Ok(Session { arm, info, store })
    }
    /// Close the store, measure it on disk, stop the process, clean up.
    pub fn finish(&self, mut s: Session) -> Result<((u64, u64), Option<sys::Memory>)> {
        let end = s.arm.memory();
        s.arm.close()?;
        let disk = sys::dir_bytes(&s.store);
        s.arm.finish()?;
        if !self.keep {
            if let Some(dir) = s.store.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        Ok((disk, end))
    }
}
