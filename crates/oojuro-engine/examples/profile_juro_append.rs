//! Synthetic actual-file append + derived-index refresh diagnostic. The retained
//! intents are shared between copied fixtures for bit-identical before/after rows.
use ojo::{
    Checkpoint,
    events::{AppendIntent, Cursor, Journal},
};
use oojuro_engine::record::{
    self,
    projection::{
        Binding,
        disk::{Compression, Limits},
    },
};
use oojuro_engine::{EventAppender, EventBuilder, EventHit, EventIndex, EventQuery, EventRow};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path, sync::OnceLock, time::Instant};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const SOURCE_KEY: [u8; 32] = [43; 32];
const INDEX_KEY: [u8; 32] = [42; 32];
#[derive(Clone, Copy, PartialEq)]
enum Dataset {
    Homogeneous,
    Diverse,
}
static DATASET: OnceLock<Dataset> = OnceLock::new();
fn dataset_name() -> &'static str {
    if DATASET.get() == Some(&Dataset::Diverse) {
        "diverse"
    } else {
        "homogeneous"
    }
}

fn checked<T>(r: record::Result<T>) -> Result<T> {
    r.map_err(|e| format!("projection: {e:?}").into())
}
fn key(encrypted: bool) -> Option<&'static [u8; 32]> {
    encrypted.then_some(&SOURCE_KEY)
}
fn binding(cp: Checkpoint) -> Binding {
    let namespace = blake3::derive_key("juro actual-file benchmark namespace v1", &cp.file_id);
    let mut state = blake3::Hasher::new_derive_key("juro actual-file benchmark checkpoint v1");
    state.update(&cp.file_id);
    state.update(&cp.sequence.to_be_bytes());
    state.update(&cp.digest);
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
    if DATASET.get() == Some(&Dataset::Diverse) {
        // Synthetic non-agent measurements with heterogeneous lengths, scopes,
        // and TF correlations. Original bytes remain the only result oracle.
        for repeated in 0..(n % 73) {
            if repeated % 3 == 0 {
                s.push_str("battery ");
            }
            if repeated % 7 == 0 {
                s.push_str("electrode ");
            }
            if repeated % 11 == 0 {
                s.push_str("original ");
            }
            s.push_str("thermal_coating response ");
        }
        for word in 0..(n % 41) {
            s.push_str(&format!(
                "material{} region{} supplier{} ",
                word,
                (n + word) % 23,
                (n * 3 + word) % 31
            ));
        }
    }
    s.push_str("exact original\0終");
    s.into_bytes()
}
fn number(cursor: &[u8]) -> usize {
    u64::from_be_bytes(cursor[16..24].try_into().unwrap()) as usize - 1
}
fn signature(hits: &[EventHit]) -> String {
    let rows: Vec<_> = hits
        .iter()
        .map(|h| {
            json!([
                h.cursor,
                h.timestamp,
                h.bits.to_bits(),
                h.continued,
                h.start,
                h.end,
                h.excerpt
            ])
        })
        .collect();
    blake3::hash(&serde_json::to_vec(&rows).unwrap())
        .to_hex()
        .to_string()
}
fn search(index: &EventIndex, source: &mut Journal, text: &str) -> Result<String> {
    let cp = source.checkpoint();
    let q = EventQuery {
        text,
        excerpt_bytes: 80,
        ..EventQuery::new(text)
    };
    let ranks = checked(index.rank_uncached(&binding(cp), &q))?;
    let hits = checked(index.materialize(&binding(cp), &q, &ranks, |bytes| {
        let cursor = Cursor::from_bytes(bytes.try_into().map_err(|_| record::Error::Authority)?)
            .map_err(|_| record::Error::Authority)?;
        let event = source
            .get(cursor)
            .map_err(|_| record::Error::Authority)?
            .ok_or(record::Error::Authority)?;
        if event.data != payload(number(bytes)) {
            return Err(record::Error::Authority);
        }
        Ok(EventRow {
            timestamp: event.accepted_at,
            data: event.data,
        })
    }))?;
    source.require_checkpoint(&cp)?;
    assert_eq!(source.checkpoint(), cp);
    Ok(signature(&hits))
}
fn oracle(
    source: &mut Journal,
    path: &Path,
    compression: Compression,
    compact: bool,
) -> Result<EventIndex> {
    let cp = source.checkpoint();
    let create = |path: &Path, key: &[u8; 32], binding, limits| {
        if compact {
            EventBuilder::create_compact(path, key, binding, limits)
        } else {
            EventBuilder::create(path, key, binding, limits)
        }
    };
    let mut builder = checked(create(
        path,
        &INDEX_KEY,
        binding(cp),
        Limits {
            compression,
            ..Limits::default()
        },
    ))?;
    let mut cursor = None;
    loop {
        let mut failure = None;
        let page = source.scan(cursor, 1024, |event| {
            if event.data != payload(number(&event.cursor.to_bytes())) {
                return Err(ojo::Error::Invalid("synthetic source differs"));
            }
            if let Err(e) = builder.push(&event.cursor.to_bytes(), event.accepted_at, &event.data) {
                failure = Some(e);
                return Err(ojo::Error::Invalid("projection failed"));
            }
            Ok(())
        });
        if let Some(e) = failure {
            return checked(Err(e));
        }
        let page = page?;
        cursor = page.cursor;
        if !page.has_more {
            break;
        }
    }
    let index = checked(builder.finish())?;
    source.require_checkpoint(&cp)?;
    assert_eq!(source.checkpoint(), cp);
    Ok(index)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if !((args.get(1).is_some_and(|mode| mode == "prepare") && args.len() == 7)
        || (args.get(1).is_some_and(|mode| mode == "append") && args.len() == 9))
    {
        return Err("prepare ROOT ROWS DELTA INTENTS plain|encrypted; append ROOT ROWS DELTA raw|zstd INTENTS plain|encrypted validate|measure".into());
    }
    let dataset = match std::env::var("JURO_BENCH_DATASET").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("homogeneous") => Dataset::Homogeneous,
        Ok("diverse") => Dataset::Diverse,
        _ => return Err("JURO_BENCH_DATASET must be homogeneous or diverse".into()),
    };
    DATASET.set(dataset).map_err(|_| "dataset already set")?;
    let flag = |name: &str| -> Result<bool> {
        match std::env::var(name).as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("0") => Ok(false),
            Ok("1") => Ok(true),
            _ => Err(format!("{name} must be 0 or 1").into()),
        }
    };
    let prewarm = flag("JURO_BENCH_PREWARM")?;
    let extended = flag("JURO_BENCH_APPEND_EXTENDED")?;
    let compact = match std::env::var("JURO_BENCH_COMPACT").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("0") => false,
        Ok("1") => true,
        _ => return Err("JURO_BENCH_COMPACT must be 0 or 1".into()),
    };
    let root = Path::new(&args[2]);
    let rows: usize = args[3].parse()?;
    let delta: usize = args[4].parse()?;
    if !(100..=1_000_000).contains(&rows) || !(1..=1024).contains(&delta) {
        return Err("rows100..1M delta1..1024".into());
    }
    let encryption = if args[1] == "prepare" {
        &args[6]
    } else {
        &args[7]
    };
    let encrypted = match encryption.as_str() {
        "plain" => false,
        "encrypted" => true,
        _ => return Err("plain|encrypted".into()),
    };
    // Refuse an incomplete/wrong fixture before any writable recovery.
    let mut before = Journal::open(root.join("source.juro"), key(encrypted), false)?;
    if before.len() != rows || before.recovered_tail_bytes() != 0 {
        return Err("not a complete source fixture".into());
    }
    before.scan(None, 2, |event| {
        if event.data != payload(number(&event.cursor.to_bytes())) {
            return Err(ojo::Error::Invalid("synthetic dataset differs"));
        }
        Ok(())
    })?;
    let expected = before.checkpoint();
    drop(before);
    let mut source = Journal::open(root.join("source.juro"), key(encrypted), true)?;
    source.require_checkpoint(&expected)?;
    assert_eq!(source.checkpoint(), expected);
    if args[1] == "prepare" && args.len() == 7 {
        let began = Instant::now();
        let mut bytes = b"JUBEN001".to_vec();
        bytes.extend((rows as u64).to_le_bytes());
        bytes.extend((delta as u64).to_le_bytes());
        for n in rows..rows + delta {
            bytes.extend(source.prepare(&payload(n))?.to_bytes());
        }
        let path = Path::new(&args[5]);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::File::open(path.parent().ok_or("intent parent")?)?.sync_all()?;
        println!(
            "{}",
            json!({"phase":"prepare","dataset":dataset_name(),"rows":rows,"delta":delta,"durable_intents_ns":began.elapsed().as_nanos(),"source_checkpoint_unchanged":source.checkpoint()==expected})
        );
        return Ok(());
    }
    if args[1] != "append" || args.len() != 9 {
        return Err("invalid append arguments".into());
    }
    let compression = match args[5].as_str() {
        "raw" => Compression::Raw,
        "zstd" => Compression::Zstd,
        _ => return Err("raw|zstd".into()),
    };
    let validate = match args[8].as_str() {
        "validate" => true,
        "measure" => false,
        _ => return Err("validate|measure".into()),
    };
    let intent_path = Path::new(&args[6]);
    if fs::metadata(intent_path)?.len() != 24 + (delta as u64) * 80 {
        return Err("intent length".into());
    }
    let bytes = fs::read(intent_path)?;
    if &bytes[..8] != b"JUBEN001"
        || u64::from_le_bytes(bytes[8..16].try_into().unwrap()) != rows as u64
        || u64::from_le_bytes(bytes[16..24].try_into().unwrap()) != delta as u64
    {
        return Err("intent fixture mismatch".into());
    }
    let intents: Vec<_> = bytes[24..]
        .as_chunks::<80>()
        .0
        .iter()
        .map(|b| AppendIntent::from_bytes(*b))
        .collect::<ojo::Result<_>>()?;
    let data: Vec<_> = (rows..rows + delta).map(payload).collect();
    let batch: Vec<_> = intents
        .iter()
        .zip(&data)
        .map(|(i, d)| (i, d.as_slice()))
        .collect();
    let previous = checked(EventIndex::open(
        root.join(format!(
            "index-{}{}",
            args[5],
            if compact { "-v2" } else { "" }
        )),
        &INDEX_KEY,
        &binding(expected),
    ))?;
    let old_bytes = previous.index_bytes();
    let prewarm_ns = if prewarm {
        let started = Instant::now();
        for text in ["battery", "battery electrode", "battery electrode original"] {
            search(&previous, &mut source, text)?;
        }
        Some(started.elapsed().as_nanos())
    } else {
        None
    };
    let began = Instant::now();
    let receipts = source.append_prepared_batch(&batch)?;
    let source_ns = began.elapsed().as_nanos();
    let current = source.checkpoint();
    source.require_checkpoint(&expected)?;
    assert_eq!(source.len(), rows + delta);
    let refresh = Instant::now();
    let mut append = checked(EventAppender::extend_validated_prefix(
        &previous,
        binding(current),
        Limits {
            compression,
            ..Limits::default()
        },
    ))?;
    for (n, receipt) in receipts.iter().enumerate() {
        let event = source
            .get(receipt.cursor)?
            .ok_or("missing accepted original")?;
        assert_eq!(event.data, data[n]);
        assert_eq!(event.accepted_at, receipt.accepted_at);
        checked(append.push(&event.cursor.to_bytes(), event.accepted_at, &event.data))?;
    }
    let index = checked(append.finish())?;
    let refresh_ns = refresh.elapsed().as_nanos();
    let total_ns = began.elapsed().as_nanos();
    source.require_checkpoint(&current)?;
    assert_eq!(source.checkpoint(), current);
    let unique = (rows..rows + delta)
        .rev()
        .find(|n| !n.is_multiple_of(997))
        .ok_or("no indexed appended original")?;
    let mut cases = vec![
        "battery".to_owned(),
        "lookup_needle battery".to_owned(),
        format!("needle{unique:08}"),
    ];
    if extended {
        cases.extend([
            "battery electrode".to_owned(),
            "battery electrode original".to_owned(),
        ]);
    }
    let mut results: Vec<Value> = Vec::new();
    let mut source_refresh_first_query_ns = None;
    for text in &cases {
        let query_started = Instant::now();
        let found = search(&index, &mut source, text)?;
        let first_query_ns = query_started.elapsed().as_nanos();
        if source_refresh_first_query_ns.is_none() {
            source_refresh_first_query_ns = Some(began.elapsed().as_nanos());
        }
        let warm_query_ns = if extended || prewarm {
            let repeated = Instant::now();
            assert_eq!(search(&index, &mut source, text)?, found);
            Some(repeated.elapsed().as_nanos())
        } else {
            None
        };
        results.push(json!({"query":text,"signature":found,"first_query_ns":first_query_ns,"warm_query_ns":warm_query_ns}));
    }
    let mut oracle_ns = None;
    if validate {
        let began = Instant::now();
        let fresh = oracle(
            &mut source,
            &root.join(format!("oracle-{}", args[5])),
            compression,
            compact,
        )?;
        for (case, result) in cases.iter().zip(&results) {
            assert_eq!(
                search(&fresh, &mut source, case)?,
                result["signature"].as_str().unwrap()
            );
        }
        oracle_ns = Some(began.elapsed().as_nanos());
    }
    println!(
        "{}",
        json!({"phase":"append","dataset":dataset_name(),"warm_before_append":prewarm,"prewarm_ns":prewarm_ns,"source_plus_refresh_first_query_ns":source_refresh_first_query_ns,"rows":rows,"delta":delta,"compression":args[5],"source_encrypted":encrypted,"source_compression":"NOCOMPRESS","derived_layout":if compact { 2 } else { 1 },
        "source_append_ns":source_ns,"index_refresh_ns":refresh_ns,"source_plus_refresh_ns":total_ns,"old_index_bytes":old_bytes,"new_index_bytes":index.index_bytes(),
        "allocated_index_bytes":index.allocated_index_bytes(),"estimated_heap_bytes":index.estimated_heap_bytes(),"build_metrics":index.build_metrics(),"queries":results,"oracle_ns":oracle_ns,
        "scope":"healthy actual source durable append plus COW refresh; excludes source/index open and separately retained intent persistence; exact original fetches/checkpoints"})
    );
    Ok(())
}
