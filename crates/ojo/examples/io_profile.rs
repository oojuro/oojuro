//! Fixed synthetic I/O profile. Existing public APIs; verifies every returned byte.
use ojo::{Change, Database};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Instant};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn stats(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    let p = |n: usize| values[(values.len() * n).div_ceil(100) - 1];
    json!({"n":values.len(),"p50":p(50),"p95":p(95),"p99":p(99)})
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err("io_profile raw|encrypted|archive|encrypted-archive NEW_DIR COUNT BATCH VALUE_BYTES READ_SAMPLES".into());
    }
    let mode = &args[0];
    if !["raw", "encrypted", "archive", "encrypted-archive"].contains(&mode.as_str()) {
        return Err("unknown mode".into());
    }
    let root = Path::new(&args[1]);
    let count: usize = args[2].parse()?;
    let batch: usize = args[3].parse()?;
    let width: usize = args[4].parse()?;
    let samples: usize = args[5].parse()?;
    if !(1..=100000).contains(&count)
        || !(1..=1024).contains(&batch)
        || !(8..=16384).contains(&width)
        || !(100..=10000).contains(&samples)
    {
        return Err("invalid dimensions".into());
    }
    fs::create_dir(root)?;
    let path = root.join("data.ojo");
    let key = mode.contains("encrypted").then_some([42; 32]);
    let keys = (0..count as u64).map(u64::to_be_bytes).collect::<Vec<_>>();
    let values = (0..count)
        .map(|i| {
            let mut bytes = b"synthetic trace: source bytes with exact line endings\r\n"
                .iter()
                .copied()
                .cycle()
                .take(width)
                .collect::<Vec<_>>();
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            bytes
        })
        .collect::<Vec<_>>();
    let mut commits = vec![];
    let start = Instant::now();
    let mut db = Database::create(&path, key.as_ref())?;
    for (n, chunk) in values.chunks(batch).enumerate() {
        let changes = chunk
            .iter()
            .enumerate()
            .map(|(j, value)| Change {
                key: &keys[n * batch + j],
                value: Some(value),
            })
            .collect::<Vec<_>>();
        let start = Instant::now();
        db.commit((n as u128 + 1).to_le_bytes(), &changes)?;
        commits.push(start.elapsed().as_secs_f64() * 1e6);
    }
    let write_seconds = start.elapsed().as_secs_f64();
    let source_checkpoint = db.checkpoint();
    let mut archive_seconds = None;
    let selected = if mode.contains("archive") {
        let archive = root.join("archive.ojo");
        let start = Instant::now();
        db.archive_to(&archive)?;
        archive_seconds = Some(start.elapsed().as_secs_f64());
        assert_eq!(db.checkpoint(), source_checkpoint);
        archive
    } else {
        path.clone()
    };
    drop(db);
    let start = Instant::now();
    let mut db = Database::open(&selected, key.as_ref(), false)?;
    let initial_open_seconds = start.elapsed().as_secs_f64();
    let mut gets = vec![];
    let mut read_signature = 0u64;
    for n in 0..samples + 32 {
        let i = (n * 7919 + 31) % count;
        let start = Instant::now();
        let value = db.get(&keys[i])?.ok_or("missing record")?;
        let us = start.elapsed().as_secs_f64() * 1e6;
        assert_eq!(value, values[i]);
        read_signature = read_signature.wrapping_add(u64::from_le_bytes(value[..8].try_into()?));
        if n >= 32 {
            gets.push(us)
        }
    }
    let mut scans = vec![];
    let mut seen_total = 0usize;
    for _ in 0..3 {
        let mut seen = 0;
        let start = Instant::now();
        db.scan(|_, _, key, value| {
            let i = u64::from_be_bytes(key.try_into().unwrap()) as usize;
            assert_eq!(value.unwrap(), values[i]);
            seen += 1;
        })?;
        scans.push(start.elapsed().as_secs_f64() * 1e3);
        assert_eq!(seen, count);
        seen_total += seen;
    }
    drop(db);
    let mut reopen = vec![];
    for _ in 0..5 {
        let start = Instant::now();
        let mut db = Database::open(&selected, key.as_ref(), false)?;
        assert_eq!(db.get(&keys[count - 1])?.unwrap(), values[count - 1]);
        reopen.push(start.elapsed().as_secs_f64() * 1e3);
    }
    // Explicit adverse control: reopen validates the whole file, while retry-only
    // derived fingerprints may be computed on first use. Keep that cost visible.
    let mut db = Database::open(&selected, key.as_ref(), true)?;
    let retry = values[..batch.min(count)]
        .iter()
        .enumerate()
        .map(|(i, value)| Change {
            key: &keys[i],
            value: Some(value),
        })
        .collect::<Vec<_>>();
    let checkpoint = db.checkpoint();
    let start = Instant::now();
    let first = db.commit(1u128.to_le_bytes(), &retry)?;
    let first_retry_us = start.elapsed().as_secs_f64() * 1e6;
    let mut cached_retry = Vec::new();
    for _ in 0..100 {
        let start = Instant::now();
        assert_eq!(db.commit(1u128.to_le_bytes(), &retry)?, first);
        cached_retry.push(start.elapsed().as_secs_f64() * 1e6);
    }
    assert_eq!(db.checkpoint(), checkpoint);
    drop(db);
    println!(
        "{}",
        json!({"mode":mode,"records":count,"batch":batch,"value_bytes":width,"write_seconds":write_seconds,"records_per_second":count as f64/write_seconds,"durable_commit_us":stats(commits),"warm_get_us":stats(gets),"scan_ms":stats(scans),"reopen_and_get_ms":stats(reopen),"initial_open_seconds":initial_open_seconds,"archive_seconds":archive_seconds,"source_bytes":fs::metadata(path)?.len(),"selected_bytes":fs::metadata(selected)?.len(),"first_retry_us":first_retry_us,"cached_retry_us":stats(cached_retry),"read_signature":read_signature,"scan_records_verified":seen_total,"scope":"synthetic fixed bytes, verified complete values; durable local file writes; warm serial read/scan/reopen; archive timing includes separate-file creation/publication; no real agent data"})
    );
    Ok(())
}
