//! Independent journals appending from independent threads (docs/PERF_ROUND2.md, item 1).
//!
//! A process serving many projects holds one journal per project. `Owner::append` keeps the
//! "store once" cache of dedupe keys; this probe times N journals, each with
//! its own thread, against the same work done one journal after the other,
//! and prints a digest of every `Appended` it saw so two runs (serial,
//! parallel, or two builds) can be compared exactly.
//!
//! Usage: multistore --dir DIR [--stores 4] [--appends 300] [--batch 3]
//!                   [--repeat 25] [--text 400] [--mode parallel|serial]
//!                   [--threads N] [--label L] [--out FILE.jsonl]
//!
//! `--repeat P`: P percent of the records reuse an earlier dedupe key of
//! their store (they must come back `deduped`). Every record of store s,
//! append a, slot b is a pure function of (s, a, b), so the stores see the
//! same work whatever the schedule. `--threads N` caps how many stores run
//! at once (default: all of them).
use oojuro::{
    append::{Appended, NewRecord},
    journal::{Journal, store_options},
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Instant,
};

fn flag(v: &[String], name: &str) -> Option<String> {
    v.iter().position(|x| x == name).and_then(|i| v.get(i + 1).cloned())
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Work {
    appends: usize,
    batch: usize,
    repeat: u64,
    text: usize,
}

/// Record `b` of append `a` of store `s`.
fn record(w: &Work, s: usize, a: usize, b: usize) -> NewRecord {
    let n = (a * w.batch + b) as u64;
    let h = mix(((s as u64) << 40) ^ n);
    // a repeat reuses the key (and text) of an earlier record of this store
    let source = if n > 0 && h % 100 < w.repeat { mix(h) % n } else { n };
    let mut text = String::with_capacity(w.text + 32);
    let mut x = mix(((s as u64) << 40) ^ source ^ 0xABCD);
    while text.len() < w.text {
        x = mix(x);
        text.push_str(&format!("w{:x} ", x & 0xfffff));
    }
    NewRecord {
        writer: format!("agent{}", source % 3),
        kind: "tool_result".into(),
        ts_us: 1_800_000_000_000_000 + (a * w.batch + b) as i64 * 1000,
        text,
        session: format!("session-{}", (source / 40) % 4),
        via: "hook".into(),
        dedupe: format!("key-{s}-{source}"),
        ..NewRecord::default()
    }
}

/// What one store's run reports: digest of every result, stored, deduped, per-call latencies in µs.
type StoreRun = (u64, u64, u64, Vec<u64>);

fn run_store(dir: PathBuf, s: usize, w: &Work) -> StoreRun {
    let mut j = Journal::open_with(&dir, store_options()).expect("open journal");
    let mut dig = DefaultHasher::new();
    let (mut stored, mut deduped) = (0u64, 0u64);
    let mut lat = Vec::with_capacity(w.appends);
    for a in 0..w.appends {
        let recs: Vec<NewRecord> = (0..w.batch).map(|b| record(w, s, a, b)).collect();
        let t = Instant::now();
        let out: Appended = j.owner().append(&recs).expect("append");
        lat.push(t.elapsed().as_micros() as u64);
        stored += out.ids.len() as u64;
        deduped += out.deduped as u64;
        (&out.ids, out.deduped, out.withheld, out.empty, out.redacted).hash(&mut dig);
    }
    j.close().expect("close");
    (dig.finish(), stored, deduped, lat)
}

fn pct(v: &mut [u64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable();
    v[(((v.len() - 1) as f64) * p).round() as usize] as f64 / 1000.0
}

fn main() {
    let v: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(flag(&v, "--dir").expect("--dir"));
    let stores: usize = flag(&v, "--stores").map_or(4, |x| x.parse().unwrap());
    let w = Work {
        appends: flag(&v, "--appends").map_or(300, |x| x.parse().unwrap()),
        batch: flag(&v, "--batch").map_or(3, |x| x.parse().unwrap()),
        repeat: flag(&v, "--repeat").map_or(25, |x| x.parse().unwrap()),
        text: flag(&v, "--text").map_or(400, |x| x.parse().unwrap()),
    };
    let serial = flag(&v, "--mode").is_some_and(|m| m == "serial");
    let threads: usize = flag(&v, "--threads").map_or(stores, |x| x.parse().unwrap()).max(1);
    let label = flag(&v, "--label").unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dirs: Vec<PathBuf> = (0..stores).map(|s| dir.join(format!("store{s}"))).collect();
    let load0 = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let t0 = Instant::now();
    let mut res: Vec<StoreRun> = Vec::new();
    if serial {
        for (s, d) in dirs.iter().enumerate() {
            res.push(run_store(d.clone(), s, &w));
        }
    } else {
        // at most `threads` stores at once; each store keeps one thread
        let next = std::sync::atomic::AtomicUsize::new(0);
        let slots: Vec<std::sync::Mutex<Option<StoreRun>>> = (0..stores).map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|sc| {
            for _ in 0..threads.min(stores) {
                sc.spawn(|| {
                    loop {
                        let s = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if s >= stores {
                            break;
                        }
                        let r = run_store(dirs[s].clone(), s, &w);
                        *slots[s].lock().unwrap() = Some(r);
                    }
                });
            }
        });
        res = slots.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect();
    }
    let wall = t0.elapsed().as_secs_f64();
    let mut all = DefaultHasher::new();
    let (mut stored, mut deduped) = (0u64, 0u64);
    let mut lat: Vec<u64> = Vec::new();
    for (d, st, dd, l) in &res {
        d.hash(&mut all);
        stored += st;
        deduped += dd;
        lat.extend(l);
    }
    let calls = stores * w.appends;
    let line = serde_json::json!({
        "label": label, "mode": if serial { "serial" } else { "parallel" }, "stores": stores, "threads": threads.min(stores),
        "appends_per_store": w.appends, "batch": w.batch, "repeat_pct": w.repeat, "text_bytes": w.text,
        "wall_s": wall, "calls_per_s": calls as f64 / wall, "records_per_s": (stored + deduped) as f64 / wall,
        "stored": stored, "deduped": deduped,
        "p50_ms": pct(&mut lat.clone(), 0.5), "p99_ms": pct(&mut lat, 0.99),
        "digest": format!("{:016x}", all.finish()),
        "per_store_digest": res.iter().map(|r| format!("{:016x}", r.0)).collect::<Vec<_>>(),
        "load_before": load0.trim(), "load_after": std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim(),
    });
    if let Some(out) = flag(&v, "--out") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out).unwrap();
        writeln!(f, "{line}").unwrap();
    }
    println!("{line}");
    let _ = std::fs::remove_dir_all(&dir);
}
