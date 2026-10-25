//! Event-profile microbenchmark; separate from KV and legacy search claims.
//! One event per durable transaction in both arms; synthetic originals only.
use ojo::events::{Cursor, Journal};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn stats(samples: &[f64]) -> Value {
    let mut values = samples.to_vec();
    values.sort_by(f64::total_cmp);
    let p = |fraction: f64| {
        values[((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
    };
    json!({"n":values.len(),"p50":p(0.5),"p95":p(0.95),"p99":p(0.99)})
}
fn value(index: usize, size: usize) -> Vec<u8> {
    let mut data: Vec<u8> = b"synthetic sensor sample temperature=21.5 status=ok\n"
        .iter()
        .copied()
        .cycle()
        .take(size)
        .collect();
    data[..8].copy_from_slice(&(index as u64).to_le_bytes());
    data
}
fn sqlite(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-65536;",
    )?;
    Ok(connection)
}
fn bytes(root: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(root)? {
        total += entry?.metadata()?.len();
    }
    Ok(total)
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: event_bench ojo|ojo-encrypted|sqlite NEW_DIRECTORY ROWS DATA_BYTES".into(),
        );
    }
    let arm = &args[0];
    if !matches!(arm.as_str(), "ojo" | "ojo-encrypted" | "sqlite") {
        return Err("unknown arm".into());
    }
    let root = Path::new(&args[1]);
    let count: usize = args[2].parse()?;
    let size: usize = args[3].parse()?;
    if !(1..=1_000_000).contains(&count) || !(8..=16384).contains(&size) {
        return Err("benchmark bounds".into());
    }
    fs::create_dir(root)?;
    let path = root.join("events");
    let mut writes = Vec::with_capacity(count);
    let mut reads = Vec::with_capacity(count);
    let reopen_us;
    let scan_us;
    let ingest_us;
    let mut archive = Value::Null;
    if arm != "sqlite" {
        let key = (arm == "ojo-encrypted").then_some([42; 32]); // synthetic benchmark key
        let mut journal = Journal::create(&path, key.as_ref())?;
        let mut cursors = Vec::<Cursor>::with_capacity(count);
        let ingest = Instant::now();
        for index in 0..count {
            let original = value(index, size);
            let start = Instant::now();
            let receipt = journal.append(&original)?;
            writes.push(start.elapsed().as_secs_f64() * 1e6);
            cursors.push(receipt.cursor);
        }
        ingest_us = ingest.elapsed().as_secs_f64() * 1e6;
        drop(journal);
        let start = Instant::now();
        let mut journal = Journal::open(&path, key.as_ref(), false)?;
        reopen_us = start.elapsed().as_secs_f64() * 1e6;
        for offset in 0..count {
            let index = (offset + count / 2) % count;
            let start = Instant::now();
            let event = journal.get(cursors[index])?.ok_or("missing event")?;
            reads.push(start.elapsed().as_secs_f64() * 1e6);
            if event.data != value(index, size) {
                return Err("original mismatch".into());
            }
        }
        let start = Instant::now();
        let mut index = 0;
        let mut cursor = None;
        loop {
            let page = journal.scan(cursor, 1024, |event| {
                if event.data != value(index, size) {
                    return Err(ojo::Error::Corrupt("scan original mismatch"));
                }
                index += 1;
                Ok(())
            })?;
            cursor = page.cursor;
            if !page.has_more {
                break;
            }
        }
        if index != count {
            return Err("scan count mismatch".into());
        }
        scan_us = start.elapsed().as_secs_f64() * 1e6;
        #[cfg(feature = "archive")]
        {
            let compressed = root.join("archive.ojo");
            let start = Instant::now();
            journal.archive_to(&compressed)?;
            let build_us = start.elapsed().as_secs_f64() * 1e6;
            let start = Instant::now();
            let mut packed = Journal::open(&compressed, key.as_ref(), false)?;
            let open_us = start.elapsed().as_secs_f64() * 1e6;
            let mut packed_reads = Vec::with_capacity(count);
            for (index, cursor) in cursors.iter().enumerate() {
                let start = Instant::now();
                let event = packed.get(*cursor)?.ok_or("missing archive event")?;
                packed_reads.push(start.elapsed().as_secs_f64() * 1e6);
                if event.data != value(index, size) {
                    return Err("archive mismatch".into());
                }
            }
            archive = json!({"build_us":build_us,"reopen_us":open_us,"get_us":stats(&packed_reads),"file_bytes":fs::metadata(compressed)?.len()});
        }
    } else {
        let connection = sqlite(&path)?;
        connection.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,accepted_at INTEGER NOT NULL,data BLOB NOT NULL);")?;
        let mut insert =
            connection.prepare_cached("INSERT INTO events(accepted_at,data) VALUES(?1,?2)")?;
        let ingest = Instant::now();
        for index in 0..count {
            let original = value(index, size);
            let start = Instant::now();
            let timestamp =
                i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros())?;
            insert.execute(params![timestamp, original])?;
            writes.push(start.elapsed().as_secs_f64() * 1e6);
        }
        ingest_us = ingest.elapsed().as_secs_f64() * 1e6;
        drop(insert);
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(connection);
        let start = Instant::now();
        let connection = sqlite(&path)?;
        reopen_us = start.elapsed().as_secs_f64() * 1e6;
        let mut get = connection.prepare_cached("SELECT data FROM events WHERE id=?1")?;
        for offset in 0..count {
            let index = (offset + count / 2) % count;
            let start = Instant::now();
            let data: Vec<u8> = get.query_row([(index + 1) as i64], |row| row.get(0))?;
            reads.push(start.elapsed().as_secs_f64() * 1e6);
            if data != value(index, size) {
                return Err("SQLite original mismatch".into());
            }
        }
        let start = Instant::now();
        let mut scan = connection.prepare("SELECT data FROM events ORDER BY id")?;
        let mut rows = scan.query([])?;
        let mut index = 0;
        while let Some(row) = rows.next()? {
            let data: Vec<u8> = row.get(0)?;
            if data != value(index, size) {
                return Err("SQLite scan mismatch".into());
            }
            index += 1;
        }
        if index != count {
            return Err("SQLite scan count mismatch".into());
        }
        scan_us = start.elapsed().as_secs_f64() * 1e6;
    }
    println!(
        "{}",
        json!({"profile":"Event1","arm":arm,"rows":count,"data_bytes":size,
        "one_event_per_transaction":true,"durability":"fsync / SQLite WAL FULL",
        "write_us":stats(&writes),"get_us":stats(&reads),"ingest_us":ingest_us,
        "reopen_us":reopen_us,"verified_scan_us":scan_us,"raw_file_bytes":fs::metadata(&path)?.len(),
        "all_files_bytes":bytes(root)?,"archive":archive,
        "limits":"warm microbenchmark; SQLite arm omits retained retry identities and per-read original checksums; no full-text search measured"})
    );
    Ok(())
}
