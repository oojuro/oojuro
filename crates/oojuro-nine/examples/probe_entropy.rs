//! Exploration probe: find turn-loop queries whose routing entropy is not finite.
use oojuro_nine::{Nine, Options, Receipts};
use std::io::BufRead;
fn main() -> oojuro_core::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let db = Nine::open_with(std::path::Path::new(&a[2]), Options { receipts: Receipts::Off, ..Options::default() })?;
    let mut key = None; let mut batch: Vec<String> = Vec::new(); let mut last = String::new(); let mut turns = 0i64;
    for line in std::io::BufReader::new(std::fs::File::open(&a[1])?).lines() {
        let v: serde_json::Value = serde_json::from_str(&line?)?;
        let k = (v["traj"].as_str().unwrap().to_string(), v["turn"].as_i64().unwrap());
        if key.as_ref() != Some(&k) && !batch.is_empty() {
            turns += 1;
            let mut end = last.len().min(2048); while !last.is_char_boundary(end) { end -= 1; }
            if end > 0 {
                let m = db.find(&last[..end], i64::MIN, turns, 20)?;
                if !m.routing_entropy_bits.is_finite() {
                    let x = db.metrics();
                    println!("turn {turns} entropy {} candidates {} scores {:?}", m.routing_entropy_bits, x.candidate_records, m.matches.iter().map(|s| s.score_bits).take(5).collect::<Vec<_>>());
                    println!("query: {:?}", &last[..end.min(300)]);
                    let all = db.find(&last[..end], i64::MIN, turns, 10_000)?;
                    let bad: Vec<_> = all.matches.iter().filter(|m| !m.score_bits.is_finite()).map(|m| (m.id.clone(), m.score_bits, m.bytes)).collect();
                    println!("all {} entropy {} nonfinite {:?}", all.matches.len(), all.routing_entropy_bits, &bad[..bad.len().min(5)]);
                    let mn = all.matches.iter().map(|m| m.score_bits).fold(f64::INFINITY, f64::min);
                    println!("min score {mn}");
                    return Ok(());
                }
            }
            let items: Vec<(i64, &str)> = batch.iter().map(|t| (turns, t.as_str())).collect();
            db.put_batch(&items)?;
            last = batch.last().unwrap().clone(); batch.clear();
        }
        key = Some(k);
        batch.push(v["text"].as_str().unwrap().to_string());
    }
    Ok(())
}
