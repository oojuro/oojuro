//! Matched baselines on the same normalized corpora (feature `baselines`).
//!
//!   baselines nine      <corpus.rec> <dir> <max_bytes>   current engine journal (NINET003)
//!   baselines nine-read <dir>                            open time + RSS of the engine
//!   baselines sqlite    <corpus.rec> <file> <max_bytes>  one row per record, VACUUMed
//!   baselines tantivy   <corpus.rec> <dir> <max_bytes> <lz4|zstd:L> <store|index|index-only|index-freqs> [kinds]
//!   baselines postgres  <corpus.rec> <host:port> <max_bytes> <pglz|lz4>
//!
//! `max_bytes` (0 = all) takes an append-order prefix. Every baseline reads its
//! originals back and checks BLAKE3 against the source before reporting size.

use oojuro_store::corpus::{CorpusRecord, read_corpus};
use serde_json::json;
use std::path::Path;
use std::time::Instant;

fn prefix(path: &str, max: usize) -> Vec<CorpusRecord> {
    let recs = read_corpus(Path::new(path)).unwrap();
    if max == 0 {
        return recs;
    }
    let mut total = 0;
    recs.into_iter()
        .take_while(|r| {
            let keep = total < max;
            total += r.text.len();
            keep
        })
        .collect()
}

fn raw(recs: &[CorpusRecord]) -> usize {
    recs.iter().map(|r| r.text.len()).sum()
}

fn tree_bytes(p: &Path) -> u64 {
    if p.is_file() {
        return p.metadata().unwrap().len();
    }
    std::fs::read_dir(p).unwrap().map(|e| tree_bytes(&e.unwrap().path())).sum()
}

fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn name(p: &str) -> String {
    Path::new(p).file_stem().unwrap().to_string_lossy().into()
}

fn text(r: &CorpusRecord) -> &str {
    std::str::from_utf8(&r.text).expect("corpus text is UTF-8")
}

fn nine(path: &str, dir: &Path, max: usize) {
    let _ = std::fs::remove_dir_all(dir);
    let recs = prefix(path, max);
    let t = Instant::now();
    let mut db = oojuro_nine::Nine::open(dir).unwrap();
    let mut ids = Vec::with_capacity(recs.len());
    for r in &recs {
        ids.push(db.put(r.session as i64 * 100_000 + r.turn as i64, text(r)).unwrap());
    }
    let load_s = t.elapsed().as_secs_f64();
    let bad = recs.iter().zip(&ids).filter(|(r, id)| db.read(id).map(|s| blake3::hash(s.as_bytes())) != Some(blake3::hash(&r.text))).count();
    let rss = rss_kib();
    drop(db);
    let disk = tree_bytes(dir);
    println!(
        "{}",
        json!({"system":"nine-NINET003","corpus":name(path),"records":recs.len(),"raw":raw(&recs),"disk":disk,
            "ratio":disk as f64/raw(&recs) as f64,"load_s":load_s,"per_put_us":load_s*1e6/recs.len() as f64,
            "rss_kib_loaded":rss,"verify_mismatches":bad})
    );
    assert_eq!(bad, 0);
}

fn nine_read(dir: &Path) {
    let rss0 = rss_kib();
    let t = Instant::now();
    let db = oojuro_nine::Nine::open(dir).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let rss = rss_kib();
    let raw: usize = db.records().map(|r| r.text.len()).sum();
    println!(
        "{}",
        json!({"system":"nine-NINET003","phase":"read","records":db.records().len(),"raw":raw,"open_ms":open_ms,
            "rss_kib_start":rss0,"rss_kib_open":rss,"rss_ratio":(rss*1024) as f64/raw as f64})
    );
}

fn sqlite(path: &str, file: &Path, max: usize) {
    let _ = std::fs::remove_file(file);
    let recs = prefix(path, max);
    let mut db = rusqlite::Connection::open(file).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE r(seq INTEGER PRIMARY KEY, kind INTEGER, valid_at INTEGER, recorded_at INTEGER, text TEXT NOT NULL);").unwrap();
    let t = Instant::now();
    let tx = db.transaction().unwrap();
    {
        let mut st = tx.prepare("INSERT INTO r VALUES (?1, ?2, ?3, ?3, ?4)").unwrap();
        for (i, r) in recs.iter().enumerate() {
            st.execute(rusqlite::params![i as i64, r.kind, r.session as i64 * 100_000 + r.turn as i64, text(r)]).unwrap();
        }
    }
    tx.commit().unwrap();
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").unwrap();
    let load_s = t.elapsed().as_secs_f64();
    let mut bad = 0;
    {
        let mut st = db.prepare("SELECT text FROM r WHERE seq = ?1").unwrap();
        for (i, r) in recs.iter().enumerate() {
            let s: String = st.query_row([i as i64], |row| row.get(0)).unwrap();
            bad += usize::from(blake3::hash(s.as_bytes()) != blake3::hash(&r.text));
        }
    }
    drop(db);
    let wal = file.with_file_name(format!("{}-wal", file.file_name().unwrap().to_string_lossy()));
    let disk = file.metadata().unwrap().len() + std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0);
    println!(
        "{}",
        json!({"system":"sqlite","version":rusqlite::version(),"corpus":name(path),"records":recs.len(),"raw":raw(&recs),"disk":disk,
            "ratio":disk as f64/raw(&recs) as f64,"load_s":load_s,"verify_mismatches":bad})
    );
    assert_eq!(bad, 0);
}

fn tantivy(path: &str, dir: &Path, max: usize, codec: &str, mode: &str, kinds: Option<&str>) {
    use tantivy::schema::{STORED, Schema, TEXT, Value};
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let mut recs = prefix(path, max);
    if let Some(k) = kinds {
        let keep: Vec<u8> = k.split(',').map(|n| oojuro_store::corpus::KIND_NAMES.iter().position(|x| x == &n).unwrap() as u8).collect();
        recs.retain(|r| keep.contains(&r.kind));
    }
    let mut sb = Schema::builder();
    let body = match mode {
        "store" => sb.add_text_field("text", STORED),
        "index" => sb.add_text_field("text", TEXT | STORED),
        "index-only" => sb.add_text_field("text", TEXT),
        "index-freqs" => {
            // BM25-capable postings without positions (no phrase queries).
            let idx = tantivy::schema::TextFieldIndexing::default()
                .set_tokenizer("default")
                .set_index_option(tantivy::schema::IndexRecordOption::WithFreqs);
            sb.add_text_field("text", tantivy::schema::TextOptions::default().set_indexing_options(idx))
        }
        _ => panic!("mode store|index|index-only|index-freqs"),
    };
    let schema = sb.build();
    let compression = match codec {
        "lz4" => tantivy::store::Compressor::Lz4,
        c => tantivy::store::Compressor::Zstd(tantivy::store::ZstdCompressor { compression_level: Some(c.trim_start_matches("zstd:").parse().unwrap()) }),
    };
    let settings = tantivy::IndexSettings { docstore_compression: compression, ..Default::default() };
    let index = tantivy::Index::builder().schema(schema).settings(settings).create_in_dir(dir).unwrap();
    let t = Instant::now();
    let mut w: tantivy::IndexWriter = index.writer_with_num_threads(1, 200_000_000).unwrap();
    for r in &recs {
        let mut d = tantivy::TantivyDocument::default();
        d.add_text(body, text(r));
        w.add_document(d).unwrap();
    }
    w.commit().unwrap();
    // One segment, as a compacted store would be.
    let ids = index.searchable_segment_ids().unwrap();
    if ids.len() > 1 {
        w.merge(&ids).wait().unwrap();
    }
    w.wait_merging_threads().unwrap();
    let load_s = t.elapsed().as_secs_f64();
    let mut bad = 0;
    if mode == "store" || mode == "index" {
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let mut got: Vec<blake3::Hash> = Vec::new();
        for seg in searcher.segment_readers() {
            let store = seg.get_store_reader(64).unwrap();
            for doc in store.iter::<tantivy::TantivyDocument>(None) {
                let doc = doc.unwrap();
                got.push(blake3::hash(doc.get_first(body).and_then(|v| v.as_str()).unwrap().as_bytes()));
            }
        }
        // Single-threaded writer + one merge keeps insertion order.
        bad = recs.iter().zip(&got).filter(|(r, h)| blake3::hash(&r.text) != **h).count() + recs.len().abs_diff(got.len());
    }
    let disk = tree_bytes(dir);
    println!(
        "{}",
        json!({"system":format!("tantivy-0.27-{mode}-{codec}"),"corpus":name(path),"kinds":kinds,"records":recs.len(),"raw":raw(&recs),"disk":disk,
            "ratio":disk as f64/raw(&recs) as f64,"load_s":load_s,"verify_mismatches":bad})
    );
    assert_eq!(bad, 0);
}

fn postgres(path: &str, addr: &str, max: usize, toast: &str) {
    let recs = prefix(path, max);
    let (host, port) = addr.split_once(':').unwrap();
    let mut c = postgres::Client::connect(&format!("host={host} port={port} user=postgres password=bench dbname=postgres"), postgres::NoTls).unwrap();
    let table = format!("r_{}_{}", name(path), toast);
    c.batch_execute(&format!(
        "DROP TABLE IF EXISTS {table}; CREATE TABLE {table}(seq bigint PRIMARY KEY, kind smallint, valid_at bigint, recorded_at bigint, body bytea COMPRESSION {toast} NOT NULL);"
    ))
    .unwrap();
    let t = Instant::now();
    let mut tx = c.transaction().unwrap();
    let st = tx.prepare(&format!("INSERT INTO {table} VALUES ($1, $2, $3, $3, $4)")).unwrap();
    for (i, r) in recs.iter().enumerate() {
        tx.execute(&st, &[&(i as i64), &(r.kind as i16), &(r.session as i64 * 100_000 + r.turn as i64), &r.text]).unwrap();
    }
    tx.commit().unwrap();
    c.execute(&format!("VACUUM FULL {table}"), &[]).unwrap();
    c.execute("CHECKPOINT", &[]).unwrap();
    let load_s = t.elapsed().as_secs_f64();
    let mut bad = 0;
    for row in c.query(&format!("SELECT seq, body FROM {table} ORDER BY seq"), &[]).unwrap() {
        let seq: i64 = row.get(0);
        let body: &[u8] = row.get(1);
        bad += usize::from(blake3::hash(body) != blake3::hash(&recs[seq as usize].text));
    }
    let total: i64 = c.query_one(&format!("SELECT pg_total_relation_size('{table}')"), &[]).unwrap().get(0);
    let heap: i64 = c.query_one(&format!("SELECT pg_relation_size('{table}')"), &[]).unwrap().get(0);
    let version: String = c.query_one("SHOW server_version", &[]).unwrap().get(0);
    println!(
        "{}",
        json!({"system":format!("postgres-{version}-toast-{toast}"),"corpus":name(path),"records":recs.len(),"raw":raw(&recs),
            "disk":total,"heap":heap,"ratio":total as f64/raw(&recs) as f64,"load_s":load_s,"verify_mismatches":bad,
            "note":"bytea column (text rejects NUL); pg_total_relation_size: heap + TOAST + primary key index; excludes WAL"})
    );
    c.batch_execute(&format!("DROP TABLE {table};")).unwrap();
    assert_eq!(bad, 0);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n = |i: usize| a[i].parse::<usize>().unwrap();
    match a.get(1).map(String::as_str) {
        Some("nine") => nine(&a[2], Path::new(&a[3]), n(4)),
        Some("nine-read") => nine_read(Path::new(&a[2])),
        Some("sqlite") => sqlite(&a[2], Path::new(&a[3]), n(4)),
        Some("tantivy") => tantivy(&a[2], Path::new(&a[3]), n(4), &a[5], &a[6], a.get(7).map(String::as_str)),
        Some("postgres") => postgres(&a[2], &a[3], n(4), &a[5]),
        _ => eprintln!("usage: see module docs"),
    }
}
