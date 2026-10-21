//! Single-record `get` and short `time_range` latency on an existing store
//! (docs/PERF_ROUND2.md, item 4). Opens the store `searchprobe build` made.
//!
//! Usage: read_probe --dir DIR [--ids N] [--gets 3000] [--windows 300]
//!                   [--window-s 3600] [--limit 1000] [--seed 1] [--qthreads 1]
//!                   [--label L] [--out F.jsonl]
//!
//! `--ids N`: ids are drawn uniformly from 1..=N (the number of events loaded;
//! `searchprobe build` prints `records`; ids of status revisions are skipped).
//! Gets are measured twice: random ids (cold: almost every one needs a frame
//! the cache does not hold) and a repeated small set (warm). `time_range` windows start at a random event
//! time and are `--window-s` long.
use oojuro_trace::{Options, TraceDb};
use std::{io::Write, path::PathBuf, time::Instant};

fn flag(v: &[String], name: &str) -> Option<String> {
    v.iter().position(|x| x == name).and_then(|i| v.get(i + 1).cloned())
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn summary(mut v: Vec<f64>) -> serde_json::Value {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    serde_json::json!({"n": v.len(), "p50_ms": q(0.5), "p90_ms": q(0.9), "p99_ms": q(0.99), "mean_ms": v.iter().sum::<f64>() / v.len() as f64})
}

fn main() {
    let v: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(flag(&v, "--dir").expect("--dir"));
    let ids: u64 = flag(&v, "--ids").map_or(899_987, |x| x.parse().unwrap());
    let gets: usize = flag(&v, "--gets").map_or(3000, |x| x.parse().unwrap());
    let windows: usize = flag(&v, "--windows").map_or(300, |x| x.parse().unwrap());
    let window_s: i64 = flag(&v, "--window-s").map_or(3600, |x| x.parse().unwrap());
    let limit: usize = flag(&v, "--limit").map_or(1000, |x| x.parse().unwrap());
    let mut rng = Rng(flag(&v, "--seed").map_or(1, |x| x.parse().unwrap()));
    let db = TraceDb::open(&dir, Options { max_query_bytes: 65_536, query_threads: flag(&v, "--qthreads").map_or(1, |x| x.parse().unwrap()), ..Options::default() }).expect("open");
    let load0 = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    // cold gets
    let mut cold = Vec::with_capacity(gets);
    let mut found = 0u64;
    for _ in 0..gets {
        let id = 1 + rng.next() % ids;
        let t = Instant::now();
        let e = db.get(id).expect("get");
        let ms = t.elapsed().as_secs_f64() * 1e3;
        // ids that are status revisions have no event: not a read
        if e.is_some() {
            cold.push(ms);
            found += 1;
        }
    }
    // warm gets: 64 ids, repeated
    let hot: Vec<u64> = (0..64).map(|_| 1 + rng.next() % ids).collect();
    for id in &hot {
        let _ = db.get(*id);
    }
    let mut warm = Vec::with_capacity(gets);
    for i in 0..gets {
        let t = Instant::now();
        let _ = db.get(hot[i % hot.len()]).expect("get");
        warm.push(t.elapsed().as_secs_f64() * 1e3);
    }
    // event times: sample a few ids to find the covered range
    let mut times: Vec<i64> = (0..400).filter_map(|_| db.get(1 + rng.next() % ids).ok().flatten().map(|e| e.ts_us)).collect();
    times.sort_unstable();
    let (lo, hi) = (times[times.len() / 50], times[times.len() * 49 / 50]);
    let mut win = Vec::with_capacity(windows);
    let mut items = 0u64;
    for _ in 0..windows {
        let from = lo + (rng.next() % (hi - lo) as u64) as i64;
        let t = Instant::now();
        let r = db.time_range(from, from + window_s * 1_000_000, limit).expect("time_range");
        win.push(t.elapsed().as_secs_f64() * 1e3);
        items += r.len() as u64;
    }
    let rec = serde_json::json!({
        "label": flag(&v, "--label").unwrap_or_default(), "dir": dir, "ids": ids, "found": found,
        "get_cold": summary(cold), "get_warm": summary(warm),
        "time_range": summary(win), "time_range_items_mean": items as f64 / windows as f64,
        "window_s": window_s, "limit": limit,
        "load_before": load0.trim(), "load_after": std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim(),
    });
    println!("{rec}");
    if let Some(o) = flag(&v, "--out") {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(o).unwrap();
        writeln!(f, "{rec}").unwrap();
    }
}
