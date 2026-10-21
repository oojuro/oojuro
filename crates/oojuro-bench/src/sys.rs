//! Host facts, cgroup memory, on-disk size and the shared bench lock.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn unique_suffix() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{}{:09}", NEXT.fetch_add(1, Ordering::Relaxed), nanos)
}

pub fn cgroup_of(pid: u32) -> Option<PathBuf> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(PathBuf::from(format!("/sys/fs/cgroup{path}")))
}

/// One memory sample of an arm process. cgroup values cover the whole scope
/// (anon + page cache charged to it); rss/hwm are the process's own counters.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Memory {
    pub cgroup_current: Option<u64>,
    pub cgroup_peak: Option<u64>,
    pub cgroup_anon: Option<u64>,
    pub cgroup_file: Option<u64>,
    pub rss: Option<u64>,
    pub hwm: Option<u64>,
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn memory(pid: u32, cgroup: Option<&Path>) -> Memory {
    let mut m = Memory::default();
    if let Some(cg) = cgroup {
        m.cgroup_current = read_u64(&cg.join("memory.current"));
        m.cgroup_peak = read_u64(&cg.join("memory.peak"));
        if let Ok(stat) = std::fs::read_to_string(cg.join("memory.stat")) {
            for line in stat.lines() {
                let mut parts = line.split_whitespace();
                match (parts.next(), parts.next().and_then(|v| v.parse().ok())) {
                    (Some("anon"), Some(v)) => m.cgroup_anon = Some(v),
                    (Some("file"), Some(v)) => m.cgroup_file = Some(v),
                    _ => {}
                }
            }
        }
    }
    if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in status.lines() {
            let kb = |l: &str| l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).map(|v| v * 1024);
            if line.starts_with("VmRSS:") {
                m.rss = kb(line);
            } else if line.starts_with("VmHWM:") {
                m.hwm = kb(line);
            }
        }
    }
    m
}

/// (apparent bytes, allocated bytes) of every file under `path`.
pub fn dir_bytes(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let mut total = (0, 0);
    let Ok(entries) = std::fs::read_dir(path) else { return total };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (a, b) = dir_bytes(&entry.path());
            total.0 += a;
            total.1 += b;
        } else {
            total.0 += meta.len();
            total.1 += meta.blocks() * 512;
        }
    }
    total
}

fn first_line_with(path: &str, prefix: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find(|l| l.starts_with(prefix))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

fn filesystem_of(path: &Path) -> Value {
    let Ok(path) = path.canonicalize() else { return Value::Null };
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return Value::Null };
    let best = mounts
        .lines()
        .filter_map(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            (f.len() >= 3 && path.starts_with(f[1])).then(|| (f[1].len(), f[0].to_string(), f[1].to_string(), f[2].to_string()))
        })
        .max();
    best.map_or(Value::Null, |(_, dev, mount, fs)| json!({"device": dev, "mount": mount, "fstype": fs}))
}

pub fn hardware(work: &Path) -> Value {
    json!({
        "cpu": first_line_with("/proc/cpuinfo", "model name"),
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "mem_total": first_line_with("/proc/meminfo", "MemTotal"),
        "swap_total": first_line_with("/proc/meminfo", "SwapTotal"),
        "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).ok(),
        "work_filesystem": filesystem_of(work),
        "rustc": env!("OOJURO_BENCH_RUSTC"),
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "load_avg": std::fs::read_to_string("/proc/loadavg").map(|s| s.trim().to_string()).ok(),
    })
}

/// Exclusive lock shared by every session's timed runs.
pub struct BenchLock(#[allow(dead_code)] File);

pub fn bench_lock() -> Result<BenchLock> {
    let path = std::env::var("OOJURO_BENCH_LOCK").unwrap_or_else(|_| {
        format!("{}/.cache/oojuro-bench.lock", std::env::var("HOME").unwrap_or_default())
    });
    if let Some(dir) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if file.try_lock().is_err() {
        eprintln!("waiting for bench lock {path} (another timed run holds it)");
        file.lock()?;
    }
    Ok(BenchLock(file))
}

pub fn now_utc() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant), proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Nearest-rank percentile of an unsorted sample.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    v[rank.min(v.len()) - 1]
}
