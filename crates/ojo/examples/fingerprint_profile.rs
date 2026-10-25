//! Opt-in synthetic retry/replay memory diagnostics, not production data or an SLA.
use ojo::{Change, Database, Error, events::Journal};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Instant};

const KEY: [u8; 32] = [43; 32];
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn key(encrypted: bool) -> Option<&'static [u8; 32]> {
    encrypted.then_some(&KEY)
}
fn payload(n: usize, bytes: usize) -> Vec<u8> {
    let mut value = vec![b'x'; bytes];
    value[..8].copy_from_slice(&(n as u64).to_le_bytes());
    value
}
fn name(n: usize) -> Vec<u8> {
    format!("key-{n:08}").into_bytes()
}
fn request(n: usize) -> [u8; 16] {
    ((n + 1) as u128).to_le_bytes()
}
fn distribution(mut times: Vec<u128>) -> Value {
    times.sort_unstable();
    let n = times.len();
    json!({"count":n,"p50_us":times[(n*50).div_ceil(100)-1] as f64/1e3,
        "p95_us":times[(n*95).div_ceil(100)-1] as f64/1e3,"samples_ns":times})
}
fn create(path: &Path, rows: usize, bytes: usize, encrypted: bool) -> Result<Value> {
    let began = Instant::now();
    let mut db = Database::create(path, key(encrypted))?;
    for n in 0..rows {
        db.commit(
            request(n),
            &[Change {
                key: &name(n),
                value: Some(&payload(n, bytes)),
            }],
        )?;
    }
    db.confirm()?;
    Ok(
        json!({"phase":"create","rows":rows,"value_bytes":bytes,"elapsed_ns":began.elapsed().as_nanos(),
        "file_bytes":fs::metadata(path)?.len(),"source_encrypted":encrypted,"source_compression":"NOCOMPRESS"}),
    )
}
fn replay(path: &Path, rows: usize, encrypted: bool) -> Result<Value> {
    let began = Instant::now();
    let mut journal = Journal::open(path, key(encrypted), false)?;
    let open_ns = began.elapsed().as_nanos();
    assert_eq!(journal.len(), rows);
    assert_eq!(journal.recovered_tail_bytes(), 0);
    let checkpoint = journal.checkpoint();
    let mut samples = Vec::new();
    journal.scan(None, 2, |event| {
        samples.push(event);
        Ok(())
    })?;
    let mut digest = Sha256::new();
    for sample in &samples {
        digest.update(sample.cursor.to_bytes());
        digest.update(sample.accepted_at.to_le_bytes());
        digest.update((sample.data.len() as u64).to_le_bytes());
        digest.update(&sample.data);
    }
    let original_signature = format!("{:x}", digest.finalize());
    let mut times = Vec::new();
    for n in 0..1000 {
        let original = &samples[n % samples.len()];
        let began = Instant::now();
        let event = journal.get(original.cursor)?.ok_or("missing original")?;
        times.push(began.elapsed().as_nanos());
        assert_eq!(&event, original);
    }
    journal.require_checkpoint(&checkpoint)?;
    assert_eq!(journal.checkpoint(), checkpoint);
    Ok(
        json!({"phase":"replay","rows":rows,"open_ns":open_ns,"get":distribution(times),"original_signature":original_signature,
        "file_bytes":fs::metadata(path)?.len(),"source_encrypted":encrypted,
        "scope":"actual Event-file replay, one retained source, exact cursor/time/data reads; no index/provider/network"}),
    )
}
fn retry(
    path: &Path,
    rows: usize,
    bytes: usize,
    working_set: usize,
    rounds: usize,
    encrypted: bool,
) -> Result<Value> {
    let began = Instant::now();
    let mut db = Database::open_checked(path, key(encrypted), true, |verified| {
        if verified.checkpoint().sequence != rows as u64 || verified.recovered_tail_bytes() != 0 {
            return Err(Error::Invalid("not a complete retry fixture"));
        }
        Ok(())
    })?;
    let open_ns = began.elapsed().as_nanos();
    db.confirm()?;
    let checkpoint = db.checkpoint();
    assert_eq!(checkpoint.sequence, rows as u64);
    assert_eq!(db.recovered_tail_bytes(), 0);
    let expected_len = fs::metadata(path)?.len();
    let inputs: Vec<_> = (0..working_set)
        .map(|n| (name(n), payload(n, bytes)))
        .collect();
    for (n, (key, value)) in inputs.iter().enumerate() {
        let receipt = db.commit(
            request(n),
            &[Change {
                key,
                value: Some(value),
            }],
        )?;
        assert_eq!(receipt.checkpoint.sequence, n as u64 + 1);
        assert_eq!(receipt.operations, 1);
    }
    let mut times = Vec::new();
    for _ in 0..rounds {
        for (n, (key, value)) in inputs.iter().enumerate() {
            let began = Instant::now();
            let receipt = db.commit(
                request(n),
                &[Change {
                    key,
                    value: Some(value),
                }],
            )?;
            times.push(began.elapsed().as_nanos());
            assert_eq!(receipt.checkpoint.sequence, n as u64 + 1);
        }
    }
    assert_eq!(db.checkpoint(), checkpoint);
    assert_eq!(fs::metadata(path)?.len(), expected_len);
    for (key, value) in &inputs {
        assert_eq!(db.get(key)?.as_deref(), Some(value.as_slice()));
    }
    Ok(
        json!({"phase":"retry","rows":rows,"value_bytes":bytes,"working_set":working_set,"rounds":rounds,
        "open_ns":open_ns,"retry":distribution(times),"file_bytes":expected_len,"source_encrypted":encrypted,
        "scope":"exact durable duplicate KV commits after confirmation and one warm pass; source/values unchanged; no new writes"}),
    )
}
#[cfg(feature = "archive")]
fn archive(path: &Path, target: &Path, encrypted: bool) -> Result<Value> {
    let began = Instant::now();
    let mut db = Database::open(path, key(encrypted), false)?;
    let before = db.checkpoint();
    let checkpoint = db.archive_to(target)?;
    db.require_checkpoint(&before)?;
    assert_eq!(db.checkpoint(), before);
    let mut archived = Database::open(target, key(encrypted), false)?;
    assert_eq!(archived.checkpoint(), checkpoint);
    assert_eq!(db.get(&name(0))?, archived.get(&name(0))?);
    Ok(
        json!({"phase":"archive","elapsed_ns":began.elapsed().as_nanos(),"source_bytes":fs::metadata(path)?.len(),
        "archive_bytes":fs::metadata(target)?.len(),"source_encrypted":encrypted,"source_checkpoint_unchanged":true}),
    )
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mode=args.get(1).ok_or("create FILE ROWS BYTES plain|encrypted; replay EVENTFILE ROWS plain|encrypted; retry FILE ROWS BYTES WORKSET ROUNDS plain|encrypted; archive FILE TARGET plain|encrypted")?;
    let path = Path::new(args.get(2).ok_or("file path required")?);
    let encrypted = match args.last().map(String::as_str) {
        Some("plain") => false,
        Some("encrypted") => true,
        _ => return Err("plain|encrypted required".into()),
    };
    let result = match mode.as_str() {
        "create" if args.len() == 6 => {
            let rows = args[3].parse()?;
            let bytes = args[4].parse()?;
            if !(1..=8192).contains(&rows) || !(8..=1 << 20).contains(&bytes) {
                return Err("create rows1..8192 bytes8..1MiB".into());
            }
            create(path, rows, bytes, encrypted)?
        }
        "replay" if args.len() == 5 => {
            let rows = args[3].parse()?;
            if !(2..=1_000_000).contains(&rows) {
                return Err("replay rows2..1M".into());
            }
            replay(path, rows, encrypted)?
        }
        "retry" if args.len() == 8 => {
            let rows = args[3].parse()?;
            let bytes = args[4].parse()?;
            let working_set = args[5].parse()?;
            let rounds = args[6].parse()?;
            if !(1..=8192).contains(&rows)
                || !(8..=1 << 20).contains(&bytes)
                || !(1..=rows).contains(&working_set)
                || !(1..=10).contains(&rounds)
            {
                return Err("retry bounds".into());
            }
            retry(path, rows, bytes, working_set, rounds, encrypted)?
        }
        #[cfg(feature = "archive")]
        "archive" if args.len() == 5 => archive(path, Path::new(&args[3]), encrypted)?,
        _ => return Err("invalid mode/arguments; archive needs --features archive".into()),
    };
    println!("{result}");
    Ok(())
}
