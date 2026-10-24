//! Matched, uncompressed, durable append and point-read microbenchmark. Synthetic bytes only.
use ojo::{Change, Database};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Instant};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn stats(samples: &[f64]) -> Value {
    let mut values = samples.to_vec();
    values.sort_by(f64::total_cmp);
    let p = |fraction: f64| {
        values[((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
    };
    json!({"n":values.len(),"p50":p(0.5),"p95":p(0.95),"p99":p(0.99)})
}
fn sqlite(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    Ok(connection)
}
fn key(index: usize) -> [u8; 8] {
    (index as u64).to_be_bytes()
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        return Err("usage: acid_bench ojo|sqlite NEW_DIRECTORY RECORDS BATCH VALUE_BYTES".into());
    }
    let arm = &args[0];
    if arm != "ojo" && arm != "sqlite" {
        return Err("unknown arm".into());
    }
    let root = Path::new(&args[1]);
    let count: usize = args[2].parse()?;
    let batch: usize = args[3].parse()?;
    let size: usize = args[4].parse()?;
    if !(1..=100_000).contains(&count)
        || !(1..=1024).contains(&batch)
        || !(8..=16384).contains(&size)
    {
        return Err("unsupported benchmark size".into());
    }
    fs::create_dir(root)?;
    let path = root.join("data");
    // Repeated log-like text, with a distinct record ordinal, and no pre-serialization in either arm.
    let values: Vec<Vec<u8>> = (0..count)
        .map(|i| {
            let mut value: Vec<u8> =
                b"synthetic tool result: returned status=0; exact UTF-8 text\n"
                    .iter()
                    .copied()
                    .cycle()
                    .take(size)
                    .collect();
            value[..8].copy_from_slice(&(i as u64).to_le_bytes());
            value
        })
        .collect();
    let keys: Vec<_> = (0..count).map(key).collect();
    let mut commits = Vec::new();
    let start = Instant::now();
    if arm == "ojo" {
        let mut db = Database::create(&path, None)?;
        for (ordinal, chunk) in values.chunks(batch).enumerate() {
            let changes: Vec<_> = chunk
                .iter()
                .enumerate()
                .map(|(j, value)| Change {
                    key: &keys[ordinal * batch + j],
                    value: Some(value),
                })
                .collect();
            let request = (ordinal as u128 + 1).to_le_bytes();
            let t = Instant::now();
            db.commit(request, &changes)?;
            commits.push(t.elapsed().as_secs_f64() * 1e6);
        }
    } else {
        let mut db = sqlite(&path)?;
        db.execute_batch(
            "CREATE TABLE records(key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID;",
        )?;
        for (ordinal, chunk) in values.chunks(batch).enumerate() {
            let t = Instant::now();
            let transaction = db.transaction()?;
            {
                let mut statement =
                    transaction.prepare_cached("INSERT INTO records VALUES(?1,?2)")?;
                for (j, value) in chunk.iter().enumerate() {
                    statement.execute(params![&keys[ordinal * batch + j], value])?;
                }
            }
            transaction.commit()?;
            commits.push(t.elapsed().as_secs_f64() * 1e6);
        }
        // Include the final WAL checkpoint/close in the total, and report durable commit latency separately.
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    }
    let write_seconds = start.elapsed().as_secs_f64();
    let mut read = Vec::new();
    let ids: Vec<_> = (0usize..2000)
        .map(|i| (i.wrapping_mul(7919) + 31) % count)
        .collect();
    let scan_start;
    let scan_seconds;
    if arm == "ojo" {
        let mut db = Database::open(&path, None, false)?;
        for &i in &ids {
            let t = Instant::now();
            let value = db.get(&keys[i])?.ok_or("missing value")?;
            read.push(t.elapsed().as_secs_f64() * 1e6);
            assert_eq!(value, values[i]);
        }
        let mut seen = 0;
        scan_start = Instant::now();
        db.scan(|_, _, key, value| {
            let i = u64::from_be_bytes(key.try_into().unwrap()) as usize;
            assert_eq!(value.unwrap(), values[i]);
            seen += 1;
        })?;
        scan_seconds = scan_start.elapsed().as_secs_f64();
        assert_eq!(seen, count);
    } else {
        let db = sqlite(&path)?;
        let mut query = db.prepare_cached("SELECT value FROM records WHERE key=?1")?;
        for &i in &ids {
            let t = Instant::now();
            let value: Vec<u8> = query.query_row([&keys[i]], |row| row.get(0))?;
            read.push(t.elapsed().as_secs_f64() * 1e6);
            assert_eq!(value, values[i]);
        }
        let mut query = db.prepare("SELECT key,value FROM records ORDER BY key")?;
        let mut seen = 0;
        scan_start = Instant::now();
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let key: Vec<u8> = row.get(0)?;
            let value: Vec<u8> = row.get(1)?;
            let i = u64::from_be_bytes(key.try_into().unwrap()) as usize;
            assert_eq!(value, values[i]);
            seen += 1;
        }
        scan_seconds = scan_start.elapsed().as_secs_f64();
        assert_eq!(seen, count);
    }
    let mut reopen = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        if arm == "ojo" {
            let mut db = Database::open(&path, None, false)?;
            assert_eq!(db.get(&keys[0])?.unwrap(), values[0]);
        } else {
            let db = sqlite(&path)?;
            let value: Vec<u8> =
                db.query_row("SELECT value FROM records WHERE key=?1", [&keys[0]], |r| {
                    r.get(0)
                })?;
            assert_eq!(value, values[0]);
        }
        reopen.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let stored: u64 = fs::read_dir(root)?
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    println!(
        "{}",
        json!({"arm":arm,"records":count,"batch":batch,"value_bytes":size,
        "write_seconds":write_seconds,"records_per_second":count as f64/write_seconds,
        "durable_commit_us":stats(&commits),"warm_get_us":stats(&read),"scan_seconds":scan_seconds,
        "reopen_and_get_ms":stats(&reopen),"stored_bytes":stored,"sqlite_version":rusqlite::version(),
        "scope":"plaintext, raw bytes; warm OS cache; SQLite WAL/FULL, prepared statements, WITHOUT ROWID; ojo sync_all per batch; unique inserts; SQLite does not retain retry IDs or verify a cryptographic value digest on every read"})
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
