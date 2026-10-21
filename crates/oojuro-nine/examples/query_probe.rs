//! Exploration probe for per-turn search cost on agent text (track/bench owns
//! the comparable benchmark). Builds a store from scripts/lexical_turns.py
//! output once (large batches), then times top-k searches whose queries are
//! later tool outputs, alternating ranking configurations.
//!
//! query_probe TURNS.jsonl STORE_DIR MAX_RECORDS QUERIES QUERY_BYTES CONFIG...
//! CONFIG is BACKGROUND:MU[:THREADS], e.g. candidates:128 view:512:1
use oojuro_nine::{Background, Nine, Options, Receipts};
use serde::Deserialize;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    time::Instant,
};

#[derive(Deserialize)]
struct Turn {
    kind: String,
    text: String,
}

fn options(config: &str) -> oojuro_core::Result<Options> {
    let mut parts = config.split(':');
    let background = parts.next().ok_or("CONFIG is BACKGROUND:MU[:THREADS]")?;
    let mu = parts.next().ok_or("CONFIG is BACKGROUND:MU[:THREADS]")?;
    let threads = parts.next().map(str::parse).transpose()?;
    let defaults = Options::default();
    Ok(Options {
        query_threads: threads.unwrap_or(defaults.query_threads),
        background: match background {
            "candidates" => Background::Candidates,
            "view" => Background::View,
            _ => return Err("background must be candidates or view".into()),
        },
        mu: mu.parse()?,
        receipts: Receipts::Off,
        ..defaults
    })
}

fn main() -> oojuro_core::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let store = Path::new(&a[2]);
    let max: usize = a[3].parse()?;
    let wanted: usize = a[4].parse()?;
    let query_bytes: usize = a[5].parse()?;
    let configs = &a[6..];
    let mut texts = Vec::new();
    let mut results = Vec::new();
    for line in BufReader::new(File::open(&a[1])?).lines().take(max) {
        let t: Turn = serde_json::from_str(&line?)?;
        if t.kind == "tool_result" {
            results.push(texts.len());
        }
        texts.push(t.text);
    }
    if !store.join("manifest.nine").exists() {
        let t = Instant::now();
        let db = Nine::open(store)?;
        for (c, chunk) in texts.chunks(2000).enumerate() {
            let items: Vec<(i64, &str)> = chunk
                .iter()
                .enumerate()
                .map(|(i, s)| ((c * 2000 + i) as i64, s.as_str()))
                .collect();
            db.put_batch(&items)?;
        }
        db.flush()?;
        eprintln!(
            "built {} records in {:.1}s",
            texts.len(),
            t.elapsed().as_secs_f64()
        );
    }
    let step = (results.len() / wanted.max(1)).max(1);
    let queries: Vec<String> = results
        .iter()
        .step_by(step)
        .take(wanted)
        .map(|i| {
            let s = &texts[*i];
            let mut end = s.len().min(query_bytes);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s[..end].to_string()
        })
        .collect();
    drop(texts);
    let at = max as i64;
    let mut times = vec![Vec::new(); configs.len()];
    let mut visited = vec![0usize; configs.len()];
    let mut candidates = vec![0usize; configs.len()];
    let mut records = 0;
    // Two rounds in ABAB order; one store handle at a time (exclusive lock).
    for _round in 0..2 {
        for (i, c) in configs.iter().enumerate() {
            let db = Nine::open_with(store, options(c)?)?;
            records = db.len();
            for q in &queries {
                db.clear_query_cache();
                let t = Instant::now();
                db.find(q, i64::MIN, at, 20)?;
                times[i].push(t.elapsed().as_secs_f64() * 1e3);
                let m = db.metrics();
                visited[i] += m.posted_rows_visited;
                candidates[i] += m.candidate_records;
            }
        }
    }
    for (i, c) in configs.iter().enumerate() {
        let v = &mut times[i];
        v.sort_by(f64::total_cmp);
        let p = |x: f64| v[((v.len() - 1) as f64 * x).round() as usize];
        println!(
            "{}",
            serde_json::json!({"config": c, "records": records, "queries": queries.len(),
                "p50_ms": p(0.5), "p90_ms": p(0.9), "p99_ms": p(0.99),
                "mean_postings": visited[i] / (2 * queries.len()).max(1),
                "mean_candidates": candidates[i] / (2 * queries.len()).max(1)})
        );
    }
    Ok(())
}
