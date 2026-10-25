//! Opt-in synthetic Event-file diagnostics. Every selected original comes from
//! an actual verified Journal.get; source opens, cold index construction and
//! retained handles are reported separately. OS caches are never evicted.
use ojo::{
    Checkpoint,
    events::{Cursor, Journal},
};
use oojuro_engine::record::{
    self,
    projection::{
        Binding,
        disk::{Compression, Limits},
    },
};
use oojuro_engine::{EventBuilder, EventHit, EventIndex, EventQuery, EventRow};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier, Mutex},
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const INDEX_KEY: [u8; 32] = [42; 32];
const SOURCE_KEY: [u8; 32] = [43; 32];

fn checked<T>(r: record::Result<T>) -> Result<T> {
    r.map_err(|e| format!("projection: {e:?}").into())
}
fn key(encrypted: bool) -> Option<&'static [u8; 32]> {
    encrypted.then_some(&SOURCE_KEY)
}
fn binding(checkpoint: Checkpoint) -> Binding {
    let namespace = blake3::derive_key(
        "juro actual-file benchmark namespace v1",
        &checkpoint.file_id,
    );
    let mut state = blake3::Hasher::new_derive_key("juro actual-file benchmark checkpoint v1");
    state.update(&checkpoint.file_id);
    state.update(&checkpoint.sequence.to_be_bytes());
    state.update(&checkpoint.digest);
    Binding {
        namespace,
        writer: "event-profile-1-benchmark".into(),
        view: *state.finalize().as_bytes(),
    }
}
fn payload(n: usize) -> Vec<u8> {
    if n.is_multiple_of(997) {
        // Binary originals stay readable and are deliberately absent from text results.
        return [vec![0xff, 0x00], (n as u64).to_be_bytes().to_vec()].concat();
    }
    let mut s = format!("Original λ\r\nmeasurement {n} battery electrode needle{n:08} ");
    if n.is_multiple_of(97) {
        s.push_str("lookup_needle ");
    }
    s.push_str("exact original\0終");
    s.into_bytes()
}
fn number(cursor: &[u8]) -> usize {
    usize::try_from(u64::from_be_bytes(cursor[16..24].try_into().unwrap()) - 1).unwrap()
}
fn snapshot(journal: &Journal, expected: Checkpoint) -> Result<()> {
    journal.require_checkpoint(&expected)?;
    if journal.checkpoint() != expected || journal.recovered_tail_bytes() != 0 {
        return Err("source snapshot changed or has an incomplete tail".into());
    }
    Ok(())
}
fn open_source(root: &Path, count: usize, encrypted: bool) -> Result<Journal> {
    let journal = Journal::open(root.join("source.juro"), key(encrypted), false)?;
    if journal.len() != count || journal.recovered_tail_bytes() != 0 {
        return Err("unexpected source length or incomplete source tail".into());
    }
    Ok(journal)
}
fn create_source(root: &Path, count: usize, encrypted: bool) -> Result<Value> {
    if root.exists() {
        return Err("source requires a fresh nonexistent directory".into());
    }
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(root)?;
    let began = Instant::now();
    let mut journal = Journal::create(root.join("source.juro"), key(encrypted))?;
    for start in (0..count).step_by(1024) {
        let data: Vec<_> = (start..(start + 1024).min(count)).map(payload).collect();
        let intents: Vec<_> = data
            .iter()
            .map(|d| journal.prepare(d))
            .collect::<ojo::Result<_>>()?;
        let batch: Vec<_> = intents
            .iter()
            .zip(&data)
            .map(|(intent, d)| (intent, d.as_slice()))
            .collect();
        let receipts = journal.append_prepared_batch(&batch)?;
        for (offset, receipt) in receipts.iter().enumerate() {
            assert_eq!(number(&receipt.cursor.to_bytes()), start + offset);
        }
    }
    let source_ns = began.elapsed().as_nanos();
    let checkpoint = journal.checkpoint();
    snapshot(&journal, checkpoint)?;
    assert_eq!(journal.len(), count);
    drop(journal);
    Ok(
        json!({"phase":"source","rows":count,"source_write_ns":source_ns,
        "source_bytes":fs::metadata(root.join("source.juro"))?.len(),
        "source_compression":"NOCOMPRESS","source_encrypted":encrypted,
        "group_durability_rows":1024,"checkpoint_sequence":checkpoint.sequence,
        "scope":"synthetic non-agent data, actual durable Event profile frames; includes prepare, append batches and final sync"}),
    )
}
fn build_index(
    journal: &mut Journal,
    directory: &Path,
    compression: Compression,
) -> Result<EventIndex> {
    let checkpoint = journal.checkpoint();
    snapshot(journal, checkpoint)?;
    let mut builder = checked(EventBuilder::create(
        directory,
        &INDEX_KEY,
        binding(checkpoint),
        Limits {
            compression,
            ..Limits::default()
        },
    ))?;
    let mut cursor = None;
    loop {
        let mut build_error = None;
        let page = journal.scan(cursor, 1024, |event| {
            assert_eq!(event.data, payload(number(&event.cursor.to_bytes())));
            if let Err(e) = builder.push(&event.cursor.to_bytes(), event.accepted_at, &event.data) {
                build_error = Some(e);
                return Err(ojo::Error::Invalid("benchmark index build failed"));
            }
            Ok(())
        });
        if let Some(error) = build_error {
            return checked(Err(error));
        }
        let page = page?;
        cursor = page.cursor;
        if !page.has_more {
            break;
        }
    }
    snapshot(journal, checkpoint)?;
    let index = checked(builder.finish())?;
    assert_eq!(
        index.row_count() as usize,
        journal.len() - journal.len().div_ceil(997)
    );
    snapshot(journal, checkpoint)?;
    Ok(index)
}
fn search(
    index: &EventIndex,
    journal: &Mutex<Journal>,
    expected: Checkpoint,
    text: &str,
    reuse: bool,
) -> Result<Vec<EventHit>> {
    snapshot(
        &*journal.lock().map_err(|_| "source lock poisoned")?,
        expected,
    )?;
    let query = EventQuery {
        text,
        excerpt_bytes: 80,
        ..EventQuery::new(text)
    };
    let ranks = if reuse {
        index.rank(&binding(expected), &query)
    } else {
        index.rank_uncached(&binding(expected), &query)
    };
    let ranks = checked(ranks)?;
    let hits = checked(
        index.materialize(&binding(expected), &query, &ranks, |bytes| {
            let cursor =
                Cursor::from_bytes(bytes.try_into().map_err(|_| record::Error::Authority)?)
                    .map_err(|_| record::Error::Authority)?;
            let mut source = journal.lock().map_err(|_| record::Error::Authority)?;
            let event = source
                .get(cursor)
                .map_err(|_| record::Error::Authority)?
                .ok_or(record::Error::Authority)?;
            if event.data != payload(number(bytes)) || event.cursor != cursor {
                return Err(record::Error::Authority);
            }
            Ok(EventRow {
                timestamp: event.accepted_at,
                data: event.data,
            })
        }),
    )?;
    snapshot(
        &*journal.lock().map_err(|_| "source lock poisoned")?,
        expected,
    )?;
    for hit in &hits {
        let data = payload(number(&hit.cursor));
        let text = std::str::from_utf8(&data)?;
        assert_eq!(hit.excerpt, text[hit.start..hit.end]);
    }
    Ok(hits)
}
fn signature(hits: &[EventHit]) -> String {
    let rows: Vec<_> = hits
        .iter()
        .map(|hit| {
            json!([
                hit.cursor,
                hit.timestamp,
                hit.bits.to_bits(),
                hit.continued,
                hit.start,
                hit.end,
                hit.excerpt
            ])
        })
        .collect();
    blake3::hash(&serde_json::to_vec(&rows).unwrap())
        .to_hex()
        .to_string()
}
fn distribution(mut times: Vec<u128>) -> Value {
    times.sort_unstable();
    let n = times.len();
    json!({"count":n,"p50_ms":times[(n*50).div_ceil(100)-1] as f64/1e6,
        "p95_ms":times[(n*95).div_ceil(100)-1] as f64/1e6,"samples_ns":times})
}
fn footprint(index: &EventIndex) -> Value {
    json!({"index_bytes":index.index_bytes(),"allocated_index_bytes":index.allocated_index_bytes(),
        "estimated_index_heap_bytes":index.estimated_heap_bytes(),"text_bytes":index.text_bytes(),"indexed_rows":index.row_count(),"build_metrics":index.build_metrics()})
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 5 && args[1] == "source" {
        let count = args[3].parse()?;
        if !(100..=1_000_000).contains(&count) {
            return Err("rows100..1M".into());
        }
        let encrypted = match args[4].as_str() {
            "plain" => false,
            "encrypted" => true,
            _ => return Err("plain|encrypted".into()),
        };
        println!("{}", create_source(Path::new(&args[2]), count, encrypted)?);
        return Ok(());
    }
    if !(8..=9).contains(&args.len()) {
        return Err("source DIR ROWS plain|encrypted; build|retained|retained-fresh|reopen|rebuild DIR ROWS raw|zstd CLIENTS ROUNDS plain|encrypted [CASE]".into());
    }
    let count: usize = args[3].parse()?;
    let clients: usize = args[5].parse()?;
    let rounds: usize = args[6].parse()?;
    if !(100..=1_000_000).contains(&count)
        || !(1..=10).contains(&clients)
        || !(1..=10).contains(&rounds)
    {
        return Err("rows100..1M clients1..10 rounds1..10".into());
    }
    let encrypted = match args[7].as_str() {
        "plain" => false,
        "encrypted" => true,
        _ => return Err("plain|encrypted".into()),
    };
    let compression = match args[4].as_str() {
        "raw" => Compression::Raw,
        "zstd" => Compression::Zstd,
        _ => return Err("raw|zstd".into()),
    };
    let root = Path::new(&args[2]);
    let cache = root.join(format!("index-{}", args[4]));
    let began = Instant::now();
    let mut journal = open_source(root, count, encrypted)?;
    let source_open_ns = began.elapsed().as_nanos();
    let expected = journal.checkpoint();
    let current = binding(expected);
    if args[1] == "build" {
        if cache.exists() {
            return Err("build requires a fresh nonexistent cache".into());
        }
        let began = Instant::now();
        let index = build_index(&mut journal, &cache, compression)?;
        println!(
            "{}",
            json!({"phase":"build","rows":count,"source_open_ns":source_open_ns,
            "index_build_ns":began.elapsed().as_nanos(),"footprint":footprint(&index),
            "source_compression":"NOCOMPRESS","source_encrypted":encrypted,"index_compression":args[4],
            "scope":"actual Journal.scan with exact fixture checks, EventBuilder finish and source checkpoint validation"})
        );
        return Ok(());
    }
    if !["retained", "retained-fresh", "reopen", "rebuild"].contains(&args[1].as_str()) {
        return Err("unknown query mode".into());
    }
    if args[1] == "rebuild" && clients != 1 {
        return Err("full cold index mode requires one client".into());
    }
    let began = Instant::now();
    let index = checked(EventIndex::open(&cache, &INDEX_KEY, &current))?;
    let index_open_ns = began.elapsed().as_nanos();
    let heap = footprint(&index);
    let mut cases = vec![
        (
            "unique",
            format!(
                "needle{:08}",
                (1..count).rev().find(|n| !n.is_multiple_of(997)).unwrap()
            ),
        ),
        ("common", "battery".into()),
        ("missing", "unseenword999".into()),
        ("identifier_anchor", "lookup_needle battery".into()),
    ];
    if count <= 1000 {
        cases.push(("dense_phrase", "\"battery electrode\"".into()));
    }
    if let Some(selected) = args.get(8) {
        cases.retain(|(name, _)| name == selected);
        if cases.is_empty() {
            return Err("unknown case".into());
        }
    }
    let source = Arc::new(Mutex::new(journal));
    let index = Arc::new(index);
    let mut results = Vec::new();
    for (name, text) in cases {
        let began = Instant::now();
        let wanted = signature(&search(&index, &source, expected, &text, false)?);
        let initial_ns = began.elapsed().as_nanos();
        let barrier = Arc::new(Barrier::new(clients));
        let times = std::thread::scope(|scope| -> Result<Vec<u128>> {
            let mut workers = Vec::new();
            for client in 0..clients {
                let barrier = barrier.clone();
                let index = index.clone();
                let source = source.clone();
                let text = text.clone();
                let wanted = wanted.clone();
                let mode = &args[1];
                let cache = &cache;
                let current = &current;
                workers.push(
                    scope.spawn(move || -> std::result::Result<Vec<u128>, String> {
                        let mut times = Vec::new();
                        barrier.wait();
                        for round in 0..rounds {
                            let began = Instant::now();
                            let result = (|| -> Result<(String, Option<PathBuf>)> {
                                if mode == "retained" || mode == "retained-fresh" {
                                    return Ok((
                                        signature(&search(
                                            &index,
                                            &source,
                                            expected,
                                            &text,
                                            mode == "retained",
                                        )?),
                                        None,
                                    ));
                                }
                                let mut journal = open_source(root, count, encrypted)?;
                                snapshot(&journal, expected)?;
                                let directory = root.join(format!(
                                    "temporary-{name}-{client}-{round}-{}",
                                    std::process::id()
                                ));
                                let (index, cleanup) = if mode == "rebuild" {
                                    (
                                        build_index(&mut journal, &directory, compression)?,
                                        Some(directory),
                                    )
                                } else {
                                    (checked(EventIndex::open(cache, &INDEX_KEY, current))?, None)
                                };
                                let hits =
                                    search(&index, &Mutex::new(journal), expected, &text, false)?;
                                Ok((signature(&hits), cleanup))
                            })();
                            let elapsed = began.elapsed().as_nanos();
                            let (got, cleanup) = result.map_err(|e| e.to_string())?;
                            if got != wanted {
                                return Err("exact source/rank signature mismatch".into());
                            }
                            times.push(elapsed);
                            if let Some(directory) = cleanup {
                                fs::remove_dir_all(directory).map_err(|e| e.to_string())?;
                            }
                        }
                        Ok(times)
                    }),
                );
            }
            let mut times = Vec::new();
            for worker in workers {
                times.extend(
                    worker
                        .join()
                        .map_err(|_| "benchmark worker panic")?
                        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
                );
            }
            Ok(times)
        })?;
        results.push(json!({"case":name,"signature":wanted,"initial_query_ns":initial_ns,"timing":distribution(times)}));
    }
    snapshot(
        &*source.lock().map_err(|_| "source lock poisoned")?,
        expected,
    )?;
    println!(
        "{}",
        json!({"phase":args[1],"rows":count,"clients":clients,"rounds":rounds,"queries":results,
        "source_open_ns":source_open_ns,"index_open_ns":index_open_ns,"footprint":heap,
        "source_compression":"NOCOMPRESS","source_encrypted":encrypted,"index_compression":args[4],
        "source_bytes":fs::metadata(root.join("source.juro"))?.len(),
        "scope":"actual local Event file + exact Journal.get + checkpoint checks; OS caches not evicted; no provider/network/auth timing",
        "initial_query_warms_index_pages_and_postings":true,"final_rank_reuse":args[1]=="retained",
        "retained_source_callbacks_serialize_on_one_healthy_handle":true})
    );
    Ok(())
}
