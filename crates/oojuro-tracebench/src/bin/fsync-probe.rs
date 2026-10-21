//! Write-path bounds for Section D: what durable appends cost on this disk,
//! independent of any database.
//!
//! - `single`: one writer, append S bytes + fdatasync (the per-commit floor).
//! - `files`: W writers, each its own file (a log per agent).
//! - `shared`: W writers, one file, leader/follower group commit (one log).
//! - `window`: W writers into one log whose syncs are spaced `T` ms apart
//!   (acknowledged after the covering sync): throughput vs durability window.
//! Usage: fsync-probe <dir> [seconds per point]   (prints JSON lines)
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1]
}

fn open(p: &Path) -> File {
    let _ = std::fs::remove_file(p);
    OpenOptions::new().create(true).append(true).open(p).unwrap()
}

fn single(dir: &Path, size: usize, secs: f64) {
    let mut f = open(&dir.join("single"));
    let buf = vec![b'x'; size];
    let mut lat = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f64() < secs {
        let t = Instant::now();
        f.write_all(&buf).unwrap();
        f.sync_data().unwrap();
        lat.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let n = lat.len();
    println!(
        "{{\"mode\":\"single\",\"bytes\":{size},\"commits\":{n},\"commits_per_s\":{:.1},\"p50_ms\":{:.3},\"p99_ms\":{:.3}}}",
        n as f64 / t0.elapsed().as_secs_f64(),
        pct(&mut lat.clone(), 0.5),
        pct(&mut lat, 0.99)
    );
}

fn files(dir: &Path, writers: usize, size: usize, secs: f64) {
    let t0 = Instant::now();
    let lats: Vec<Vec<f64>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                s.spawn(move || {
                    let mut f = open(&dir.join(format!("file{w}")));
                    let buf = vec![b'x'; size];
                    let mut lat = Vec::new();
                    while t0.elapsed().as_secs_f64() < secs {
                        let t = Instant::now();
                        f.write_all(&buf).unwrap();
                        f.sync_data().unwrap();
                        lat.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    lat
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut all: Vec<f64> = lats.into_iter().flatten().collect();
    let n = all.len();
    println!(
        "{{\"mode\":\"files\",\"writers\":{writers},\"bytes\":{size},\"commits\":{n},\"commits_per_s\":{:.1},\"p50_ms\":{:.3},\"p99_ms\":{:.3}}}",
        n as f64 / t0.elapsed().as_secs_f64(),
        pct(&mut all.clone(), 0.5),
        pct(&mut all, 0.99)
    );
}

#[derive(Default)]
struct Log {
    pending: Vec<u8>,
    /// commits appended / made durable
    appended: u64,
    durable: u64,
    leader: bool,
}

/// One log, leader/follower group commit; `window_ms > 0` spaces syncs apart
/// (the leader sleeps until the window has passed since its last sync).
fn shared(dir: &Path, writers: usize, size: usize, secs: f64, window_ms: f64) {
    let file = Mutex::new(open(&dir.join("shared")));
    let log = Arc::new((Mutex::new(Log::default()), Condvar::new()));
    let last_sync = Mutex::new(Instant::now());
    let syncs = Mutex::new(0u64);
    let t0 = Instant::now();
    let lats: Vec<Vec<f64>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..writers)
            .map(|_| {
                let (log, file, last_sync, syncs) = (Arc::clone(&log), &file, &last_sync, &syncs);
                s.spawn(move || {
                    let buf = vec![b'x'; size];
                    let mut lat = Vec::new();
                    while t0.elapsed().as_secs_f64() < secs {
                        let t = Instant::now();
                        let (m, cv) = &*log;
                        let mut g = m.lock().unwrap();
                        g.pending.extend_from_slice(&buf);
                        g.appended += 1;
                        let mine = g.appended;
                        loop {
                            if g.durable >= mine {
                                break;
                            }
                            if !g.leader {
                                g.leader = true;
                                drop(g);
                                if window_ms > 0.0 {
                                    let since = last_sync.lock().unwrap().elapsed().as_secs_f64() * 1e3;
                                    if since < window_ms {
                                        std::thread::sleep(Duration::from_secs_f64((window_ms - since) / 1e3));
                                    }
                                }
                                let mut g2 = m.lock().unwrap();
                                let data = std::mem::take(&mut g2.pending);
                                let upto = g2.appended;
                                drop(g2);
                                {
                                    let mut f = file.lock().unwrap();
                                    f.write_all(&data).unwrap();
                                    f.sync_data().unwrap();
                                }
                                *last_sync.lock().unwrap() = Instant::now();
                                *syncs.lock().unwrap() += 1;
                                g = m.lock().unwrap();
                                g.durable = upto;
                                g.leader = false;
                                cv.notify_all();
                            } else {
                                g = cv.wait(g).unwrap();
                            }
                        }
                        drop(g);
                        lat.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    lat
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut all: Vec<f64> = lats.into_iter().flatten().collect();
    let n = all.len();
    let wall = t0.elapsed().as_secs_f64();
    println!(
        "{{\"mode\":\"{}\",\"writers\":{writers},\"bytes\":{size},\"window_ms\":{window_ms},\"commits\":{n},\"syncs\":{},\"commits_per_s\":{:.1},\"mb_per_s\":{:.2},\"p50_ms\":{:.3},\"p99_ms\":{:.3}}}",
        if window_ms > 0.0 { "window" } else { "shared" },
        syncs.lock().unwrap(),
        n as f64 / wall,
        n as f64 * size as f64 / wall / 1e6,
        pct(&mut all.clone(), 0.5),
        pct(&mut all, 0.99)
    );
}

/// Open loop: writers append and return at once (acknowledged before
/// durable); a syncer makes everything durable every `window_ms`. Data at
/// risk on a crash = at most one window of acknowledged appends.
fn async_window(dir: &Path, writers: usize, size: usize, secs: f64, window_ms: f64) {
    let file = Arc::new(Mutex::new(open(&dir.join("async"))));
    let buf_shared = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let syncs = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let syncer = {
        let (file, buf, stop, syncs) = (Arc::clone(&file), Arc::clone(&buf_shared), Arc::clone(&stop), Arc::clone(&syncs));
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs_f64(window_ms / 1e3));
                let data = std::mem::take(&mut *buf.lock().unwrap());
                let mut f = file.lock().unwrap();
                f.write_all(&data).unwrap();
                f.sync_data().unwrap();
                syncs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        })
    };
    let t0 = Instant::now();
    let lats: Vec<Vec<f64>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..writers)
            .map(|_| {
                let buf_shared = Arc::clone(&buf_shared);
                s.spawn(move || {
                    let buf = vec![b'x'; size];
                    let mut lat = Vec::new();
                    while t0.elapsed().as_secs_f64() < secs {
                        let t = Instant::now();
                        buf_shared.lock().unwrap().extend_from_slice(&buf);
                        lat.push(t.elapsed().as_secs_f64() * 1e3);
                        // a turn takes model + tool time; pace like a fast agent (1 ms)
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    lat
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    syncer.join().unwrap();
    let mut all: Vec<f64> = lats.into_iter().flatten().collect();
    let n = all.len();
    let wall = t0.elapsed().as_secs_f64();
    println!(
        "{{\"mode\":\"async\",\"writers\":{writers},\"bytes\":{size},\"window_ms\":{window_ms},\"commits\":{n},\"syncs\":{},\"commits_per_s\":{:.1},\"p50_ms\":{:.4},\"p99_ms\":{:.4},\"at_risk_commits_max\":{:.0}}}",
        syncs.load(std::sync::atomic::Ordering::Relaxed),
        n as f64 / wall,
        pct(&mut all.clone(), 0.5),
        pct(&mut all, 0.99),
        n as f64 / wall * window_ms / 1e3
    );
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("dir"));
    let secs: f64 = std::env::args().nth(2).map_or(5.0, |s| s.parse().unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    if std::env::args().nth(3).as_deref() == Some("async") {
        for window in [1.0, 5.0, 20.0, 100.0] {
            async_window(&dir, 64, 2560, secs, window);
        }
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    // a turn batch is ~2.5 KiB raw on tier 1 (933.7 MB / 367,558 batches)
    for size in [256, 2560, 65536, 1 << 20] {
        single(&dir, size, secs);
    }
    for w in [1, 4, 16, 64] {
        files(&dir, w, 2560, secs);
        shared(&dir, w, 2560, secs, 0.0);
    }
    for window in [1.0, 2.0, 5.0, 10.0, 50.0] {
        shared(&dir, 64, 2560, secs, window);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
