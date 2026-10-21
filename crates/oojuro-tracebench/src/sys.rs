//! Process and container memory, directory sizes, a peak sampler.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

fn status_kb(field: &str) -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(field))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// Anonymous resident memory of this process (heap, not mapped files).
pub fn rss_anon() -> u64 {
    status_kb("RssAnon:") * 1024
}

/// Peak resident set of this process (includes touched mapped files).
pub fn vm_hwm() -> u64 {
    status_kb("VmHWM:") * 1024
}

/// Logical bytes of every file under `p`.
pub fn dir_bytes(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                total += dir_bytes(&e.path());
            } else {
                total += md.len();
            }
        }
    }
    total
}

/// Allocated bytes (`st_blocks` x 512, what `du` reports) of every file under
/// `p`: differs from [`dir_bytes`] for sparse, preallocated files.
pub fn dir_alloc_bytes(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                total += dir_alloc_bytes(&e.path());
            } else {
                total += md.blocks() * 512;
            }
        }
    }
    total
}

pub fn files_under(p: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                files_under(&path, out);
            } else {
                out.push(path);
            }
        }
    }
}

/// Docker container id for a name (via `docker inspect`).
pub fn container_id(name: &str) -> Option<String> {
    let out = std::process::Command::new("docker")
        .args(["inspect", "-f", "{{.Id}}", name])
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// anon + shmem of a container's cgroup (engine memory without file cache;
/// Postgres shared_buffers are shmem).
pub fn cgroup_mem(id: &str) -> u64 {
    let path = format!("/sys/fs/cgroup/system.slice/docker-{id}.scope/memory.stat");
    let Ok(s) = fs::read_to_string(path) else { return 0 };
    let mut total = 0;
    for l in s.lines() {
        let mut it = l.split_whitespace();
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == "anon" || k == "shmem" {
                total += v.parse::<u64>().unwrap_or(0);
            }
        }
    }
    total
}

/// Samples `f` every 100 ms on a thread and keeps the peak.
pub struct PeakSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl PeakSampler {
    pub fn start(f: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(f()));
        let (s, p) = (Arc::clone(&stop), Arc::clone(&peak));
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                p.fetch_max(f(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Self { stop, peak, handle: Some(handle) }
    }
    pub fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
}
