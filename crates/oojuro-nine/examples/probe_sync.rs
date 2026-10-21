//! Exploration probe: raw append+fdatasync latency vs one Nine insert.
use oojuro_core::Record;
use std::{io::Write, time::Instant};
fn main() -> oojuro_core::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("dir"));
    std::fs::create_dir_all(&dir)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("raw"))?;
    let t = Instant::now();
    for i in 0..300 {
        f.write_all(format!("frame {i} {}\n", "x".repeat(1000)).as_bytes())?;
        f.sync_data()?;
    }
    println!("raw append+sync_data: {:.3} ms/op", t.elapsed().as_secs_f64() * 1e3 / 300.);
    let db = oojuro_nine::Nine::open(&dir.join("db"))?;
    let t = Instant::now();
    for i in 0..300 {
        db.insert(Record::new(&format!("d{i}"), &format!("d{i}"), &"word ".repeat(200), 1))?;
    }
    println!("nine insert: {:.3} ms/op", t.elapsed().as_secs_f64() * 1e3 / 300.);
    let t = Instant::now();
    for i in 0..30 {
        let texts: Vec<String> = (0..10).map(|j| format!("turn {i} {j} {}", "word ".repeat(200))).collect();
        let items: Vec<(i64, &str)> = texts.iter().map(|s| (1, s.as_str())).collect();
        db.put_batch(&items)?;
    }
    println!("nine put_batch of 10: {:.3} ms/record", t.elapsed().as_secs_f64() * 1e3 / 300.);
    Ok(())
}
