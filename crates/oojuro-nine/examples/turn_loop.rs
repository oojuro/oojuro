//! Exploration turn-loop probe for the lexical track (the comparable
//! benchmark lives on track/bench). Replays turn records produced by
//! scripts/lexical_turns.py: before each turn, search the whole store with
//! the agent's latest input (top-k); after it, append the turn's records as
//! one durable batch. Prints one JSON line per window of turns.
//!
//! turn_loop ENGINE(v3|v4|v4-thinking-stored) TURNS.jsonl STORE_DIR [MAX_RECORDS] [QUERY_BYTES] [K] [WINDOW]
//!
//! `v4-thinking-stored` stores thinking records without indexing them.
use oojuro_nine::{Nine, legacy::V3};
use serde::Deserialize;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    time::Instant,
};

#[derive(Deserialize)]
struct Turn {
    traj: String,
    turn: i64,
    kind: String,
    text: String,
}

enum Engine {
    V3(Box<V3>),
    V4(Box<Nine>, bool),
}

impl Engine {
    fn open(engine: &str, store: &Path) -> oojuro_core::Result<Self> {
        Ok(match engine {
            "v3" => Engine::V3(Box::new(V3::open(store)?)),
            "v4" => Engine::V4(Box::new(Nine::open(store)?), true),
            "v4-thinking-stored" => Engine::V4(Box::new(Nine::open(store)?), false),
            _ => return Err("engine must be v3 or v4".into()),
        })
    }
    fn append(&mut self, at: i64, texts: &[(String, bool)]) -> oojuro_core::Result<String> {
        match self {
            Engine::V3(db) => {
                let mut last = String::new();
                for (t, _) in texts {
                    last = db.put(at, t)?;
                }
                Ok(last)
            }
            Engine::V4(db, index_thinking) => {
                let items: Vec<(i64, &str, bool)> = texts
                    .iter()
                    .map(|(t, thinking)| (at, t.as_str(), *index_thinking || !*thinking))
                    .collect();
                Ok(db.put_batch_indexed(&items)?.pop().unwrap_or_default())
            }
        }
    }
    fn search(&mut self, text: &str, at: i64, k: usize) -> oojuro_core::Result<usize> {
        Ok(match self {
            Engine::V3(db) => db.find(text, i64::MIN, at, k)?.matches.len(),
            Engine::V4(db, _) => db.find(text, i64::MIN, at, k)?.matches.len(),
        })
    }
    fn readable(&mut self, id: &str) -> oojuro_core::Result<bool> {
        Ok(match self {
            Engine::V3(db) => db.read(id).is_some(),
            Engine::V4(db, _) => db.read(id)?.is_some(),
        })
    }
}

fn status(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(field))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

struct Loop {
    db: Engine,
    query_bytes: usize,
    k: usize,
    turns: usize,
    search_ms: Vec<f64>,
    append_ms: Vec<f64>,
    last_input: String,
}

impl Loop {
    /// One agent turn: search with its input, then append its records.
    fn turn(&mut self, batch: &mut Vec<(String, bool)>) -> oojuro_core::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        self.turns += 1;
        let at = self.turns as i64;
        let mut end = self.last_input.len().min(self.query_bytes);
        while !self.last_input.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            let t = Instant::now();
            self.db.search(&self.last_input[..end], at, self.k)?;
            self.search_ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let t = Instant::now();
        let id = self.db.append(at, batch)?;
        self.append_ms.push(t.elapsed().as_secs_f64() * 1e3);
        // Zero-lag check: the acknowledged record is readable immediately.
        if !self.db.readable(&id)? {
            return Err("acknowledged record is not readable".into());
        }
        self.last_input = batch.last().map(|(t, _)| t.clone()).unwrap_or_default();
        batch.clear();
        Ok(())
    }
}

fn main() -> oojuro_core::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let engine = a[1].clone();
    let store = Path::new(&a[3]);
    let max: usize = a.get(4).map_or(usize::MAX, |v| v.parse().expect("max"));
    let window: usize = a.get(7).map_or(2000, |v| v.parse().expect("window"));
    let opened = Instant::now();
    let db = Engine::open(&engine, store)?;
    eprintln!("open_ms {:.1}", opened.elapsed().as_secs_f64() * 1e3);
    let mut run = Loop {
        db,
        query_bytes: a.get(5).map_or(2048, |v| v.parse().expect("query bytes")),
        k: a.get(6).map_or(20, |v| v.parse().expect("k")),
        turns: 0,
        search_ms: Vec::new(),
        append_ms: Vec::new(),
        last_input: String::new(),
    };
    let (mut records, mut raw) = (0usize, 0u64);
    let mut batch: Vec<(String, bool)> = Vec::new();
    let mut key: Option<(String, i64)> = None;
    let started = Instant::now();
    for line in BufReader::new(File::open(&a[2])?).lines() {
        if records >= max {
            break;
        }
        let t: Turn = serde_json::from_str(&line?)?;
        let this = (t.traj, t.turn);
        if key.as_ref() != Some(&this) {
            run.turn(&mut batch)?;
            key = Some(this);
            if run.turns > 0 && run.turns % window == 0 && !run.search_ms.is_empty() {
                println!(
                    "{}",
                    serde_json::json!({"engine": engine, "turns": run.turns, "records": records,
                        "raw_mb": raw as f64 / 1e6, "store_mb": dir_bytes(store) as f64 / 1e6,
                        "search_p50_ms": pct(&mut run.search_ms, 0.5), "search_p99_ms": pct(&mut run.search_ms, 0.99),
                        "append_p50_ms": pct(&mut run.append_ms, 0.5), "append_p99_ms": pct(&mut run.append_ms, 0.99),
                        "rss_mb": status("VmRSS:") as f64 / 1024., "peak_rss_mb": status("VmHWM:") as f64 / 1024.,
                        "elapsed_s": started.elapsed().as_secs_f64()})
                );
                run.search_ms.clear();
                run.append_ms.clear();
            }
        }
        raw += t.text.len() as u64;
        records += 1;
        batch.push((t.text, t.kind == "thinking"));
    }
    run.turn(&mut batch)?;
    println!(
        "{}",
        serde_json::json!({"engine": engine, "final": true, "turns": run.turns, "records": records,
            "raw_mb": raw as f64 / 1e6, "store_mb": dir_bytes(store) as f64 / 1e6,
            "rss_mb": status("VmRSS:") as f64 / 1024., "peak_rss_mb": status("VmHWM:") as f64 / 1024.,
            "elapsed_s": started.elapsed().as_secs_f64()})
    );
    drop(run);
    let reopened = Instant::now();
    let db = Engine::open(&engine, store)?;
    if let Engine::V4(db, _) = &db {
        let stats = db.stats()?;
        println!(
            "{}",
            serde_json::json!({"engine": engine, "indexed_postings": stats.indexed_postings,
                "vocabulary": stats.vocabulary, "disk_bytes": stats.archive_bytes})
        );
    }
    println!(
        "{}",
        serde_json::json!({"engine": engine, "reopen_ms": reopened.elapsed().as_secs_f64() * 1e3,
            "rss_after_reopen_mb": status("VmRSS:") as f64 / 1024.})
    );
    drop(db);
    Ok(())
}
