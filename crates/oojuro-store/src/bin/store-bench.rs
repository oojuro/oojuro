//! Store scorecard on a normalized corpus.
//!
//!   store-bench build <corpus.rec> <dir>   append every turn batch, compact, verify
//!   store-bench read  <dir> [queries]      fresh-process open time, RSS, excerpt latency
//!
//! Options come from the environment (defaults = `Options::default()`):
//! HOT_LEVEL, COLD_LEVEL, COLD_FRAME, DICT, DICT_AFTER, DICT_SPACING, DICT_SCALE, CACHE, DEDUPE, DEDUPE_WINDOW, THREADS, SYNC_BATCHES
//! (leading batches appended with fdatasync; the rest load unsynced),
//! SYNC_DURING (batches appended with fdatasync while a background compaction
//! runs), COMPACT (0/1 at the end), COMPACT_EVERY (raw bytes between
//! background compactions, which run on a thread while appends continue).
//! Synthetic clocks: valid_at = recorded_at = session * 100_000 + turn.

use oojuro_store::corpus::{read_corpus, turn_batches};
use oojuro_store::util::{Rng, percentile};
use oojuro_store::{NewRecord, Options, Store};
use serde_json::json;
use std::path::Path;
use std::time::Instant;

fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn options() -> Options {
    let d = Options::default();
    Options {
        hot_level: env("HOT_LEVEL", d.hot_level),
        cold_level: env("COLD_LEVEL", d.cold_level),
        cold_frame_bytes: env("COLD_FRAME", d.cold_frame_bytes),
        dict_bytes: env("DICT", d.dict_bytes),
        dict_train_after: env("DICT_AFTER", d.dict_train_after),
        cache_bytes: env("CACHE", d.cache_bytes),
        dict_spacing: env("DICT_SPACING", d.dict_spacing),
        dict_scale: env("DICT_SCALE", d.dict_scale),
        dedupe_chunk: env("DEDUPE", d.dedupe_chunk),
        dedupe_window: env("DEDUPE_WINDOW", d.dedupe_window),
        compact_threads: env("THREADS", d.compact_threads),
        ..d
    }
}

fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum()
}

fn build(corpus: &str, dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let recs = read_corpus(Path::new(corpus)).unwrap();
    let raw: u64 = recs.iter().map(|r| r.text.len() as u64).sum();
    let batches = turn_batches(&recs);
    // Leading batches appended with fdatasync (quiet latency), plus up to
    // SYNC_DURING batches appended with fdatasync while a background
    // compaction runs (latency under compaction).
    let sync_batches: usize = env("SYNC_BATCHES", 2000);
    let sync_during: usize = env("SYNC_DURING", 500);
    let compact_every: u64 = env("COMPACT_EVERY", 0);
    let opts = options();
    let mut store = Store::open(dir, Options { sync: true, ..opts.clone() }).unwrap();
    let (mut lat, mut lat_during, mut batch_bytes) = (Vec::new(), Vec::new(), Vec::new());
    let t_all = Instant::now();
    let mut since_compact = 0u64;
    let mut compactions = Vec::new();
    let mut job: Option<std::thread::JoinHandle<oojuro_store::Result<oojuro_store::store::PreparedSegment>>> = None;
    for (n, b) in batches.iter().enumerate() {
        let batch: Vec<NewRecord> = recs[b.clone()]
            .iter()
            .map(|r| {
                let t = r.session as i64 * 100_000 + r.turn as i64;
                NewRecord { kind: r.kind, valid_at: t, recorded_at: t, key: b"", attrs: b"", text: &r.text }
            })
            .collect();
        let bytes: u64 = batch.iter().map(|r| r.text.len() as u64).sum();
        let quiet = n < sync_batches && job.is_none();
        let during = job.is_some() && lat_during.len() < sync_during;
        store.set_sync(quiet || during);
        let t = Instant::now();
        store.append(&batch).unwrap();
        let us = t.elapsed().as_secs_f64() * 1e6;
        if quiet {
            lat.push(us);
            batch_bytes.push(bytes as f64);
        } else if during {
            lat_during.push(us);
        }
        since_compact += bytes;
        if job.as_ref().is_some_and(|j| j.is_finished()) {
            let prepared = job.take().unwrap().join().unwrap().unwrap();
            compactions.push(store.finish_compaction(prepared).unwrap());
        }
        if job.is_none() && compact_every > 0 && since_compact >= compact_every {
            if let Some(j) = store.begin_compaction().unwrap() {
                job = Some(std::thread::spawn(move || j.run()));
            }
            since_compact = 0;
        }
    }
    if let Some(j) = job.take() {
        let prepared = j.join().unwrap().unwrap();
        compactions.push(store.finish_compaction(prepared).unwrap());
    }
    let load_s = t_all.elapsed().as_secs_f64();
    let pre_final_disk = dir_bytes(dir);
    let hot_frames = store.stats().unwrap().hot_frames;
    if env("COMPACT", 1) == 1 {
        compactions.push(store.compact().unwrap());
    }
    let compact_json = if compactions.is_empty() {
        json!(null)
    } else {
        let raw_c: u64 = compactions.iter().map(|c| c.raw_bytes).sum();
        let secs: f64 = compactions.iter().map(|c| c.seconds).sum();
        json!({"runs":compactions.len(),"raw_bytes":raw_c,"seconds":secs,"mbps":raw_c as f64/1e6/secs,
            "ref_bytes":compactions.iter().map(|c| c.ref_bytes).sum::<u64>()})
    };
    let disk = dir_bytes(dir);
    let st = store.stats().unwrap();
    // Gate: every record byte-identical (BLAKE3) to its source.
    let t = Instant::now();
    let mismatches = recs.iter().enumerate().filter(|(i, r)| blake3::hash(&store.get(*i as u64).unwrap()) != blake3::hash(&r.text)).count();
    let verify_s = t.elapsed().as_secs_f64();
    assert_eq!(st.raw_text_bytes, raw);
    println!(
        "{}",
        json!({"phase":"build","corpus":Path::new(corpus).file_stem().unwrap().to_string_lossy(),"options":format!("{opts:?}"),
            "records":recs.len(),"batches":batches.len(),"raw":raw,
            "append_us_p50":percentile(&mut lat.clone(),0.5),"append_us_p99":percentile(&mut lat,0.99),
            "append_during_compaction_us_p50":percentile(&mut lat_during.clone(),0.5),"append_during_compaction_us_p99":percentile(&mut lat_during,0.99),
            "synced_batches":lat.len(),"synced_during_compaction":lat_during.len(),"synced_batch_bytes_p50":percentile(&mut batch_bytes,0.5),
            "load_s":load_s,"pre_final_disk":pre_final_disk,"pre_final_ratio":pre_final_disk as f64/raw as f64,"hot_frames":hot_frames,
            "hot_ratio":pre_final_disk as f64/raw as f64,
            "compaction":compact_json,"disk":disk,"ratio":disk as f64/raw as f64,"cold_frames":st.cold_frames,
            "dicts":st.dicts,"verify_mismatches":mismatches,"verify_s":verify_s})
    );
    assert_eq!(mismatches, 0, "byte-exact gate failed");
}

fn read(dir: &Path, queries: usize) {
    let rss0 = rss_kib();
    let t = Instant::now();
    let store = Store::open(dir, options()).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let rss_open = rss_kib();
    let n = store.len() as usize;
    let mut rng = Rng::new(42);
    let mut per_query = Vec::new();
    let mut check = 0u64;
    for _ in 0..queries {
        let t = Instant::now();
        for _ in 0..20 {
            let seq = rng.below(n) as u64;
            // A passage reader asks for a 2 KiB window; lengths are unknown up
            // front, so start inside the first 4 KiB and fall back to the head.
            let start = rng.below(4096);
            let mut out = store.get_range(seq, start, start + 2048).unwrap();
            if out.is_empty() {
                out = store.get_range(seq, 0, 2048).unwrap();
            }
            check = check.wrapping_add(out.len() as u64);
        }
        per_query.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let rss_after = rss_kib();
    let raw = store.stats().unwrap().raw_text_bytes;
    println!(
        "{}",
        json!({"phase":"read","records":n,"raw":raw,"open_ms":open_ms,"rss_kib_start":rss0,"rss_kib_open":rss_open,"rss_kib_after":rss_after,
            "rss_ratio":(rss_after * 1024) as f64/raw as f64,
            "query20_us_p50":percentile(&mut per_query.clone(),0.5),"query20_us_p99":percentile(&mut per_query,0.99),"check":check})
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("build") => build(&a[2], Path::new(&a[3])),
        Some("read") => read(Path::new(&a[2]), a.get(3).and_then(|q| q.parse().ok()).unwrap_or(1000)),
        _ => eprintln!("usage: store-bench build <corpus.rec> <dir> | read <dir> [queries]"),
    }
}
