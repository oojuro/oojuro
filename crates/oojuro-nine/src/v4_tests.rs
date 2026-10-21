use super::*;
use crate::v3::V3;
use std::{
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(std::env::temp_dir().join(format!(
            "nine-v4-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        )))
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let (Ok(target), Ok(parent)) =
            (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        {
            if target.parent() == Some(parent.as_path()) {
                let _ = fs::remove_dir_all(target);
            }
        }
    }
}

fn r(id: &str, key: &str, text: &str, valid: i64, known: i64) -> Record {
    let mut r = Record::new(id, key, text, valid);
    r.recorded_at = known;
    r.verified = false;
    r
}

fn small(flush_text_bytes: usize) -> Options {
    Options {
        flush_text_bytes,
        flush_postings: usize::MAX,
        receipts: Receipts::Durable,
        archive: Arc::new(BlockArchive::new(
            512,
            crate::archive::BlockCache::new(1 << 16),
        )),
        ..Options::default()
    }
}

fn same(a: &Matches, b: &Matches, context: &str) {
    let ids = |m: &Matches| m.matches.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(a), ids(b), "{context}");
    for (x, y) in a.matches.iter().zip(&b.matches) {
        assert_eq!(x.score_bits.to_bits(), y.score_bits.to_bits(), "{context}");
        assert_eq!((x.time, x.bytes, x.continued), (y.time, y.bytes, y.continued), "{context}");
    }
    assert!(
        (a.routing_entropy_bits - b.routing_entropy_bits).abs() < 1e-9,
        "{context}: {} vs {}",
        a.routing_entropy_bits,
        b.routing_entropy_bits
    );
}

fn same_metrics(a: &TimelineMetrics, b: &TimelineMetrics, context: &str) {
    assert_eq!(a.candidate_records, b.candidate_records, "{context}");
    assert_eq!(a.scored_records, b.scored_records, "{context}");
    assert_eq!(a.query_terms, b.query_terms, "{context}");
    assert_eq!(a.eligible_outcomes, b.eligible_outcomes, "{context}");
    assert_eq!(a.continued_matches, b.continued_matches, "{context}");
    assert_eq!(a.background_pad.to_bits(), b.background_pad.to_bits(), "{context}");
    assert_eq!(
        a.background_query_bits.to_bits(),
        b.background_query_bits.to_bits(),
        "{context}"
    );
    assert_eq!(
        a.best_conditional_query_bits.map(f64::to_bits),
        b.best_conditional_query_bits.map(f64::to_bits),
        "{context}"
    );
}

const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "gamma",
    "delta",
    "running",
    "runs",
    "file",
    "files",
    "repair_shadow_page",
    "shadow",
    "page",
    "error",
    "E_TEMP",
    "東京",
    "🦀",
    "café",
    "workers",
    "the",
    "of",
    "fn",
];

fn workload(seed: u64, n: usize) -> Vec<Record> {
    let mut s = seed;
    let mut next = move |m: u64| {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (s >> 33) % m
    };
    (0..n)
        .map(|i| {
            let len = 1 + next(40) as usize;
            let text = (0..len)
                .map(|_| WORDS[next(WORDS.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(if next(3) == 0 { "_" } else { " " });
            // Duplicate texts and times create exact score ties.
            let text = if i % 17 == 5 {
                "alpha beta tie".to_string()
            } else {
                text
            };
            let valid = next(12) as i64;
            let known = valid + next(4) as i64;
            let key = if next(3) == 0 {
                format!("key{}", next(9))
            } else {
                format!("own{i}")
            };
            let id = format!("{}-{i:03}", ["a", "b", "zz", "doc-0001"][next(4) as usize]);
            r(&id, &key, &text, valid, known)
        })
        .collect()
}

const QUERIES: &[&str] = &[
    "alpha beta",
    "alpha beta tie",
    "running files",
    "the of",
    "repair_shadow_page",
    "find repair_shadow_page in files",
    "unknown_identifier_x alpha",
    "\"alpha beta\"",
    "\"🦀\"",
    "\"shadow page\"",
    "東京 café",
    "E_TEMP",
    "key:key3",
    "",
    "zzz nothing",
];

#[test]
fn v4_matches_the_v3_oracle_across_segments_views_and_feedback() -> Result<()> {
    for (seed, flush) in [(7u64, 300usize), (11, 1500), (23, usize::MAX)] {
        let a = Scratch::new();
        let b = Scratch::new();
        let mut v3 = V3::open(&a.0)?;
        let v4 = Nine::open_with(&b.0, small(flush))?;
        let records = workload(seed, 220);
        for (i, rec) in records.iter().enumerate() {
            v3.insert(rec.clone())?;
            if i % 5 == 0 {
                v4.insert_batch(vec![rec.clone()])?;
            } else {
                v4.insert(rec.clone())?;
            }
        }
        for (q, source, useful, at) in [
            ("alpha beta", "a-003", true, 20),
            ("alpha beta", "b-010", false, 20),
            ("E_TEMP", "zz-020", true, 9),
            ("\"alpha beta\"", "a-040", true, 30),
            ("key:key3", "doc-0001-050", true, 30),
        ] {
            let ra = v3.record_outcome(q, source, useful, at, "fixture");
            let rb = v4.record_outcome(q, source, useful, at, "fixture");
            assert_eq!(ra.is_ok(), rb.is_ok(), "outcome {q} {source}");
        }
        let check = |v3: &mut V3, v4: &Nine, label: &str| -> Result<()> {
            for text in QUERIES {
                for (valid, known, from) in [
                    (30, 30, i64::MIN),
                    (6, 7, i64::MIN),
                    (8, 3, 2),
                    (11, 40, 5),
                ] {
                    for k in [0, 1, 3, 10, 500] {
                        let mut q = Query::new(text, valid);
                        q.known_at = known;
                        q.k = k;
                        let x = v3.search_refs(&q, from)?;
                        let mx = v3.metrics();
                        let y = v4.search_refs(&q, from)?;
                        let my = v4.metrics();
                        let ctx = format!(
                            "{label} seed={seed} q={text:?} v={valid} k={known} from={from} n={k}"
                        );
                        same(&x, &y, &ctx);
                        same_metrics(&mx, &my, &ctx);
                    }
                }
            }
            Ok(())
        };
        check(&mut v3, &v4, "live")?;
        drop(v4);
        let v4 = Nine::open_with(&b.0, small(flush))?;
        check(&mut v3, &v4, "reopened")?;
        for rec in &records {
            assert_eq!(
                serde_json::to_vec(&v4.record(&rec.id)?.unwrap())?,
                serde_json::to_vec(rec)?
            );
        }
        assert_eq!(
            serde_json::to_vec(&v4.range(-5, 100, 10_000)?)?,
            serde_json::to_vec(&v3.range(-5, 100, 10_000)?)?
        );
        assert_eq!(
            serde_json::to_vec(&v4.range(3, 7, 9)?)?,
            serde_json::to_vec(&v3.range(3, 7, 9)?)?
        );
    }
    Ok(())
}

#[test]
fn a_future_key_cannot_switch_a_past_query_to_key_addressing() -> Result<()> {
    let tmp = Scratch::new();
    let db = Nine::open(&tmp.0)?;
    db.insert(r("note", "note", "deploy checklist reviewed", 1, 1))?;
    let before = db.find("deploy", i64::MIN, 5, 10)?;
    assert_eq!(before.matches.len(), 1);
    // A later record whose key equals the query text.
    db.insert(r("later", "deploy", "unrelated body", 50, 50))?;
    let after = db.find("deploy", i64::MIN, 5, 10)?;
    assert_eq!(
        serde_json::to_vec(&before)?,
        serde_json::to_vec(&after)?,
        "as-of results must not depend on later writes"
    );
    // Once the key exists in the view, the bare text addresses it.
    assert_eq!(db.find("deploy", i64::MIN, 60, 10)?.matches[0].id, "later");
    Ok(())
}

#[test]
fn concurrent_writers_share_syncs_and_every_ack_is_immediately_searchable() -> Result<()> {
    let tmp = Scratch::new();
    let db = Arc::new(Nine::open_with(&tmp.0, small(3_000))?);
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || -> Result<Vec<String>> {
                let mut ids = Vec::new();
                for i in 0..40 {
                    let token = format!("token{t}x{i}");
                    let turn = [
                        (i as i64, format!("tool call {token} start")),
                        (i as i64, format!("tool result {token} ok")),
                    ];
                    let refs: Vec<(i64, &str)> =
                        turn.iter().map(|(a, s)| (*a, s.as_str())).collect();
                    let got = db.put_batch(&refs)?;
                    // Read-your-writes: searchable as soon as the call returns.
                    let found = db.find(&token, i64::MIN, 1_000, 10)?;
                    assert_eq!(found.matches.len(), 2, "{token}");
                    ids.extend(got);
                }
                Ok(ids)
            })
        })
        .collect();
    let mut all = Vec::new();
    for t in threads {
        all.extend(t.join().expect("writer thread")?);
    }
    assert_eq!(all.len(), 8 * 40 * 2);
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), all.len());
    assert_eq!(db.len(), all.len());
    let texts: Vec<_> = all.iter().map(|id| db.read(id)).collect::<Result<_>>()?;
    drop(db);
    let db = Nine::open_with(&tmp.0, small(3_000))?;
    for (id, text) in all.iter().zip(texts) {
        assert_eq!(db.read(id)?, text);
    }
    assert!(
        db.files()?
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "idx"))
    );
    Ok(())
}

#[test]
fn batches_are_atomic_and_duplicate_ids_reject_the_whole_batch() -> Result<()> {
    let tmp = Scratch::new();
    let db = Nine::open(&tmp.0)?;
    db.insert(r("one", "one", "first", 1, 1))?;
    assert!(
        db.insert_batch(vec![
            r("two", "two", "second", 1, 1),
            r("one", "x", "dup", 1, 1)
        ])
        .is_err()
    );
    assert!(
        db.insert_batch(vec![r("three", "t", "a", 1, 1), r("three", "t", "b", 1, 1)])
            .is_err()
    );
    assert_eq!(db.len(), 1);
    assert_eq!(db.read("two")?, None);
    let mut bad = r("vec", "vec", "x", 1, 1);
    bad.embedding = Some(vec![0.5; 3072]);
    assert!(db.insert(bad).is_err());
    db.insert(r("two", "two", "second", 1, 1))?;
    drop(db);
    let db = Nine::open(&tmp.0)?;
    assert_eq!(db.len(), 2);
    Ok(())
}

#[test]
fn torn_journal_tails_keep_every_acknowledged_row_and_header_damage_fails() -> Result<()> {
    let tmp = Scratch::new();
    let ids = {
        let db = Nine::open(&tmp.0)?;
        db.put_batch(&[(1, "preserve this exact original"), (2, "and this one")])?
    };
    let wal = tmp.path(&wal_name(1));
    let size = fs::metadata(&wal)?.len();
    for tail in [&[1u8, 2, 3, 4, 5][..], &[0u8; 64][..]] {
        fs::OpenOptions::new()
            .append(true)
            .open(&wal)?
            .write_all(tail)?;
        let db = Nine::open(&tmp.0)?;
        assert_eq!(db.len(), 2);
        assert_eq!(
            db.read(&ids[0])?.as_deref(),
            Some("preserve this exact original")
        );
        drop(db);
        assert_eq!(fs::metadata(&wal)?.len(), size);
    }
    // A later batch, then damage to the first frame's length: committed
    // frames follow, so opening must fail and leave the file untouched.
    {
        let db = Nine::open(&tmp.0)?;
        db.put(3, "third")?;
    }
    let mut f = fs::OpenOptions::new().read(true).write(true).open(&wal)?;
    f.seek(SeekFrom::Start(32 + 3))?;
    f.write_all(&[0x7f])?;
    f.sync_all()?;
    drop(f);
    let damaged = fs::read(&wal)?;
    assert!(Nine::open(&tmp.0).is_err());
    assert_eq!(fs::read(&wal)?, damaged);
    Ok(())
}

#[test]
fn interrupted_flushes_recover_from_journals_and_orphans_are_removed() -> Result<()> {
    let tmp = Scratch::new();
    let (ids, expected) = {
        let db = Nine::open_with(&tmp.0, small(usize::MAX))?;
        let ids = db.put_batch(&[(1, "alpha one"), (2, "alpha two"), (3, "beta three")])?;
        db.flush()?;
        let more = db.put_batch(&[(4, "alpha four"), (5, "gamma five")])?;
        let expected = db.find("alpha", i64::MIN, 10, 10)?;
        ([ids, more].concat(), expected)
    };
    // Simulate a crash after a later flush wrote its files and started the
    // next journal but before the manifest switched: an orphan segment pair
    // and an unused journal with a torn header.
    fs::write(tmp.path("seg-00000009.idx"), b"partial")?;
    fs::write(tmp.path("seg-00000009.src"), b"partial")?;
    fs::write(tmp.path(&wal_name(3)), b"NINEW")?;
    let db = Nine::open_with(&tmp.0, small(usize::MAX))?;
    assert!(!tmp.path("seg-00000009.idx").exists());
    assert!(!tmp.path(&wal_name(3)).exists());
    same(
        &expected,
        &db.find("alpha", i64::MIN, 10, 10)?,
        "after recovery",
    );
    for id in &ids {
        assert!(db.read(id)?.is_some());
    }
    // A snapshot of a recovered store reopens as the same database.
    let snap = Scratch::new();
    db.snapshot(&snap.0)?;
    let copy = Nine::open_with(&snap.0, small(usize::MAX))?;
    same(&expected, &copy.find("alpha", i64::MIN, 10, 10)?, "snapshot");
    Ok(())
}

#[test]
fn recovery_replays_sealed_journals_left_by_a_crash_between_journal_and_manifest() -> Result<()>
{
    let tmp = Scratch::new();
    let ids = {
        let db = Nine::open_with(&tmp.0, small(usize::MAX))?;
        db.put_batch(&[(1, "first epsilon"), (2, "second epsilon")])?
    };
    // A flush created journal 2 (header durable) and crashed before the
    // manifest named it: the rows live only in journal 1.
    Journal::create(&tmp.path(&wal_name(2)), Kind::Records, 2)?;
    let more = {
        let db = Nine::open_with(&tmp.0, small(usize::MAX))?;
        assert_eq!(db.len(), 2);
        let more = db.put(3, "third epsilon")?;
        let snap = Scratch::new();
        db.snapshot(&snap.0)?;
        let copy = Nine::open_with(&snap.0, small(usize::MAX))?;
        assert_eq!(copy.len(), 3);
        more
    };
    let db = Nine::open_with(&tmp.0, small(usize::MAX))?;
    assert_eq!(db.find("epsilon", i64::MIN, 10, 10)?.matches.len(), 3);
    db.flush()?;
    drop(db);
    let db = Nine::open_with(&tmp.0, small(usize::MAX))?;
    assert_eq!(wal_files(&tmp.0)?.len(), 1);
    for id in ids.iter().chain([&more]) {
        assert!(db.read(id)?.is_some());
    }
    Ok(())
}

#[test]
fn v3_stores_migrate_with_identical_originals_feedback_receipts_and_results() -> Result<()> {
    let tmp = Scratch::new();
    let records = workload(99, 150);
    let mut expected = Vec::new();
    {
        let mut v3 = V3::open(&tmp.0)?;
        for rec in &records {
            v3.insert(rec.clone())?;
        }
        let put = v3.put(7, "a put text with a derived id")?;
        v3.record_outcome("alpha beta", &records[3].id, true, 20, "fixture")?;
        for text in QUERIES {
            expected.push(v3.find(text, i64::MIN, 30, 50)?);
        }
        assert_eq!(v3.read(&put), Some("a put text with a derived id"));
    }
    let legacy = fs::read(tmp.path("timeline.nine"))?;
    let db = Nine::open_with(&tmp.0, small(1 << 30))?;
    assert_eq!(
        fs::read(tmp.path("timeline.nine"))?,
        legacy,
        "v3 files untouched"
    );
    assert_eq!(db.len(), records.len() + 1);
    assert_eq!(db.stats()?.learned_outcomes, 1);
    assert_eq!(db.stats()?.persisted_queries, QUERIES.len());
    for rec in &records {
        assert_eq!(
            serde_json::to_vec(&db.record(&rec.id)?.unwrap())?,
            serde_json::to_vec(rec)?
        );
    }
    let mut matched = 0;
    for (text, want) in QUERIES.iter().zip(&expected) {
        let got = db.find(text, i64::MIN, 30, 50)?;
        same(want, &got, text);
        matched += got.matches.len();
    }
    assert!(matched > 0);
    drop(db);
    // The second open uses the v4 manifest, not the legacy files.
    let db = Nine::open_with(&tmp.0, small(1 << 30))?;
    assert_eq!(db.len(), records.len() + 1);
    Ok(())
}

#[test]
fn originals_roundtrip_byte_exact_through_segments_and_blake3_detects_damage() -> Result<()> {
    let tmp = Scratch::new();
    let mut seed = 761u64;
    let mut payload = String::new();
    for _ in 0..12_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        payload.push((33 + ((seed >> 32) % 90) as u8) as char);
    }
    let originals: Vec<String> = (0..12)
        .map(|n| format!("東京🦀 tool output {payload}\ncompletion={n}\n終わり\r\n\0"))
        .chain(["".to_string(), "x".repeat(70_000)])
        .collect();
    let mut ids = Vec::new();
    // Default 64 KiB blocks: repetition across records is the codec's job.
    let options = Options {
        flush_text_bytes: 1 << 30,
        ..Options::default()
    };
    {
        let db = Nine::open_with(&tmp.0, options.clone())?;
        for (i, text) in originals.iter().enumerate() {
            let mut rec = r(&format!("r{i}"), "tool", text, i as i64, i as i64);
            rec.metadata.insert("k".into(), "v".into());
            db.insert(rec)?;
            ids.push(format!("r{i}"));
        }
        db.flush()?;
        let stats = db.stats()?;
        let originals_on_disk = fs::metadata(tmp.path("seg-00000001.src"))?.len();
        assert!(originals_on_disk < (stats.raw_text_bytes / 2) as u64);
        assert!(Nine::open(&tmp.0).is_err(), "exclusive lock");
    }
    let db = Nine::open_with(&tmp.0, options.clone())?;
    for (id, text) in ids.iter().zip(&originals) {
        let got = db.read(id)?.unwrap();
        assert_eq!(blake3::hash(got.as_bytes()), blake3::hash(text.as_bytes()));
    }
    drop(db);
    // Flip a byte inside the archive: reading must fail, never return
    // different bytes.
    let src = tmp.path("seg-00000001.src");
    let mut bytes = fs::read(&src)?;
    bytes[40] ^= 0x10;
    fs::write(&src, &bytes)?;
    let db = Nine::open_with(&tmp.0, options.clone())?;
    let outcomes: Vec<_> = ids.iter().map(|id| db.read(id)).collect();
    assert!(outcomes.iter().any(|o| o.is_err()));
    for (o, text) in outcomes.iter().zip(&originals) {
        if let Ok(Some(t)) = o {
            assert_eq!(t, text);
        }
    }
    drop(db);
    // Index damage is caught by the segment checksum at open.
    let idx = tmp.path("seg-00000001.idx");
    let mut bytes = fs::read(&idx)?;
    bytes[200] ^= 1;
    fs::write(&idx, &bytes)?;
    assert!(Nine::open_with(&tmp.0, options.clone()).is_err());
    Ok(())
}

#[test]
fn historical_views_ignore_future_rows_statistics_and_revisions() -> Result<()> {
    let tmp = Scratch::new();
    let db = Nine::open_with(&tmp.0, small(200))?;
    db.insert(r("old", "runtime", "compiler runtime Node 22", 10, 10))?;
    let q = Query::new("compiler runtime", 30);
    let before = db.search_refs(&q, i64::MIN)?;
    db.insert(r("new", "runtime", "compiler runtime Node 24", 20, 50))?;
    db.insert(r(
        "future",
        "different",
        "compiler compiler runtime runtime future",
        99,
        99,
    ))?;
    db.flush()?;
    let after = db.search_refs(&q, i64::MIN)?;
    same(&before, &after, "as-of 30");
    let mut known = q.clone();
    known.known_at = 60;
    assert_eq!(db.search_refs(&known, i64::MIN)?.matches[0].id, "new");
    assert_eq!(
        db.range(10, 20, 100)?
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["old", "new"]
    );
    Ok(())
}

#[test]
fn receipts_follow_the_policy_and_cache_entries_never_survive_writes() -> Result<()> {
    for mode in [Receipts::Durable, Receipts::Buffered, Receipts::Off] {
        let tmp = Scratch::new();
        let options = Options {
            receipts: mode,
            ..Options::default()
        };
        {
            let db = Nine::open_with(&tmp.0, options.clone())?;
            db.insert(r("one", "key", "exact cache needle", 1, 1))?;
            let q = Query::new("cache needle", 10);
            db.search_refs(&q, i64::MIN)?;
            assert!(!db.metrics().cache_hit);
            db.search_refs(&q, i64::MIN)?;
            assert!(db.metrics().cache_hit);
            db.insert(r("two", "key", "exact cache needle revised", 2, 2))?;
            assert_eq!(db.search_refs(&q, i64::MIN)?.matches[0].id, "two");
            assert!(!db.metrics().cache_hit);
            assert_eq!(db.stats()?.cache_entries, 1);
            let id = db.put(1, "the shared needle")?;
            db.find("needle", 0, 2, 50)?;
            db.record_outcome("needle", &id, true, 2, "fixture-executor")?;
            assert_eq!(db.stats()?.cache_entries, 0);
        }
        let reopened = Nine::open_with(&tmp.0, options)?;
        let want = if mode == Receipts::Off { 0 } else { 4 };
        assert_eq!(reopened.stats()?.persisted_queries, want, "{mode:?}");
    }
    Ok(())
}

#[test]
fn passages_reads_exports_and_stats_use_exact_originals() -> Result<()> {
    let tmp = Scratch::new();
    let db = Nine::open_with(&tmp.0, small(64))?;
    let original = "Workers are running in café.";
    db.insert(r("old", "workers", original, 1, 1))?;
    db.insert(r("other", "other", "A runner runs nearby.", 1, 1))?;
    let part = db.passage("old", "worker runs café", 64)?.unwrap();
    assert!(part.text.contains("running"));
    assert_eq!(part.text, &original[part.start..part.end]);
    assert!(db.passage("missing", "x", 10)?.is_none());
    let all = db.records_from(0, 10)?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].text, original);
    assert_eq!(db.records_from(1, 10)?.len(), 1);
    assert_eq!(db.records_from(5, 10)?.len(), 0);
    let stats = db.stats()?;
    assert_eq!(stats.records, 2);
    assert_eq!(
        stats.raw_text_bytes,
        original.len() + "A runner runs nearby.".len()
    );
    assert!(stats.vocabulary >= 6);
    let mut text = String::new();
    File::open(tmp.path(MANIFEST))?.read_to_string(&mut text)?;
    assert!(text.contains(FORMAT));
    Ok(())
}


/// The pre-fix entropy: p = w / sum can underflow to 0 for a subnormal w.
fn entropy_before_fix(scores: &[f64]) -> f64 {
    let top = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = scores.iter().map(|s| (s - top).exp2()).collect();
    let sum: f64 = weights.iter().filter(|x| x.is_finite() && **x > 0.0).sum();
    if sum <= 0.0 {
        return 0.0;
    }
    weights
        .iter()
        .filter(|x| x.is_finite() && **x > 0.0)
        .map(|x| {
            let p = x / sum;
            -p * p.log2()
        })
        .sum()
}

#[test]
fn subnormal_routing_weights_keep_entropy_finite_and_stores_reopenable() -> Result<()> {
    // Many identical top candidates (weights summing to 64) plus candidates
    // about 1069-1074 bits lower, whose weights are subnormal: the old formula
    // turned those into 0 * -inf = NaN and wrote an unreadable receipt.
    let tmp = Scratch::new();
    let query: Vec<String> = (0..180).map(|i| format!("w{i}")).collect();
    let query = query.join(" ");
    {
        let db = Nine::open(&tmp.0)?;
        db.put_batch(&vec![(1, query.as_str()); 64])?;
        let mut n = 600f64;
        while n < 1300.0 {
            let text = std::iter::once("w0".to_string())
                .chain((0..n as usize).map(|i| format!("z{i}")))
                .collect::<Vec<_>>()
                .join(" ");
            db.put(1, &text)?;
            n *= 1.004;
        }
        let mut q = Query::new(&query, 2);
        q.k = 10_000;
        let all = db.search_refs(&q, i64::MIN)?;
        let scores: Vec<f64> = all.matches.iter().map(|m| m.score_bits).collect();
        assert_eq!(all.matches.len(), db.len());
        assert!(entropy_before_fix(&scores).is_nan(), "fixture must hit the band");
        assert!(all.routing_entropy_bits.is_finite());
        let top = db.find(&query, i64::MIN, 2, 20)?;
        assert!(top.routing_entropy_bits.is_finite());
    }
    assert_eq!(Nine::open(&tmp.0)?.stats()?.persisted_queries, 2);
    Ok(())
}

#[test]
fn v3_stores_with_null_entropy_receipts_open_and_migrate() -> Result<()> {
    let tmp = Scratch::new();
    let id = {
        let mut v3 = V3::open(&tmp.0)?;
        v3.put(1, "A worker is running.")?
    };
    let (mut log, _) = oojuro_core::Journal::open::<serde_json::Value>(
        &tmp.path("queries.timeline"),
        *b"NINEQ002",
        true,
    )?;
    let zero = [0u8; 32];
    log.append(&serde_json::json!({"query":zero,"from":0,"at":1,"known_at":1,
        "ids":[id],"entropy_bits":null,"model":"dirichlet-scope-128-v1"}))?;
    drop(log);
    assert_eq!(V3::open(&tmp.0)?.stats()?.persisted_queries, 1);
    let db = Nine::open(&tmp.0)?;
    assert_eq!(db.stats()?.persisted_queries, 1);
    drop(db);
    assert_eq!(Nine::open(&tmp.0)?.len(), 1);
    Ok(())
}

#[test]
fn whole_view_background_matches_a_dense_oracle_on_historical_views() -> Result<()> {
    use crate::text::{literal_probability, normalize_word, query_terms, retrieval_terms};
    let tmp = Scratch::new();
    let mu = 40.0;
    let options = Options {
        background: Background::View,
        mu,
        ..small(700)
    };
    let db = Nine::open_with(&tmp.0, options)?;
    let words = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "running", "runs", "file",
        "files",
    ];
    let mut originals = Vec::new();
    for i in 0..160usize {
        let text = (0..(11 + i % 57))
            .map(|j| words[(i * 3 + j * (1 + i % 3)) % words.len()])
            .collect::<Vec<_>>()
            .join(" ");
        let rec = r(
            &format!("row-{i}"),
            &format!("key-{}", i % 80),
            &text,
            (i / 10) as i64,
            (i / 10 + i % 4) as i64,
        );
        db.insert(rec.clone())?;
        originals.push(rec);
    }
    let counts: Vec<HashMap<String, usize>> = originals
        .iter()
        .map(|o| {
            let mut c = HashMap::new();
            for t in retrieval_terms(&o.text) {
                *c.entry(normalize_word(&t).into_owned()).or_default() += 1;
            }
            c
        })
        .collect();
    let mut checked = 0;
    for (at, known, from) in [
        (3, 2, i64::MIN),
        (9, 8, i64::MIN),
        (9, 30, 4),
        (20, 19, 2),
        (100, 100, i64::MIN),
        (100, 100, 5),
    ] {
        for text in ["alpha beta", "beta beta gamma", "epsilon unknownword", "running files"] {
            let mut q = Query::new(text, at);
            q.known_at = known;
            q.k = 1000;
            let visible: Vec<usize> = oojuro_core::visible_latest(&originals, &q)
                .into_iter()
                .filter(|i| originals[*i].valid_at >= from)
                .collect();
            let mut query = query_terms(text);
            query.retain(|t, _| visible.iter().any(|i| counts[*i].contains_key(t)));
            let z: usize = visible.iter().map(|i| counts[*i].len()).sum();
            let bg: HashMap<&String, f64> = query
                .keys()
                .map(|t| {
                    let df = visible.iter().filter(|i| counts[**i].contains_key(t)).count();
                    (t, (df as f64 + literal_probability(t)) / (z as f64 + 1.))
                })
                .collect();
            let dense: HashMap<String, f64> = visible
                .iter()
                .filter(|i| query.keys().any(|t| counts[**i].contains_key(t)))
                .map(|i| {
                    let c = &counts[*i];
                    let length = c.values().sum::<usize>() as f64;
                    let scope = c.len() as f64;
                    let score = query
                        .iter()
                        .map(|(t, n)| {
                            let tf = c.get(t).copied().unwrap_or(0) as f64;
                            let p = (scope * tf / length + mu * bg[t]) / (scope + mu);
                            *n as f64 * (p.log2() - bg[t].log2())
                        })
                        .sum::<f64>();
                    (originals[*i].id.clone(), score)
                })
                .collect();
            let found = db.search_refs(&q, from)?;
            assert_eq!(found.matches.len(), dense.len(), "{text} at {at}/{known} from {from}");
            for m in &found.matches {
                assert!(
                    (m.score_bits - dense[&m.id]).abs() < 1e-9,
                    "{text} {}: {} vs {}",
                    m.id,
                    m.score_bits,
                    dense[&m.id]
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 200);
    assert!(db.files()?.iter().filter(|p| p.extension().is_some_and(|e| e == "idx")).count() > 2);
    Ok(())
}


#[test]
fn merges_preserve_every_result_bit_for_bit_and_survive_reopen() -> Result<()> {
    let tmp = Scratch::new();
    let options = Options {
        merge_fanout: 3,
        ..small(250)
    };
    let records = workload(5, 260);
    let db = Nine::open_with(&tmp.0, Options { merge_fanout: 0, ..options.clone() })?;
    for rec in &records {
        db.insert(rec.clone())?;
    }
    db.flush()?;
    let before_segments = db.files()?.iter().filter(|p| p.extension().is_some_and(|e| e == "idx")).count();
    let mut expected = Vec::new();
    for text in QUERIES {
        for (valid, known, from) in [(30, 30, i64::MIN), (6, 7, i64::MIN), (11, 40, 5)] {
            let mut q = Query::new(text, valid);
            q.known_at = known;
            q.k = 50;
            expected.push((q.clone(), from, db.search_refs(&q, from)?));
        }
    }
    drop(db);
    let db = Nine::open_with(&tmp.0, Options { merge_fanout: 3, ..options.clone() })?;
    let merged = db.compact()?;
    assert!(merged > 0);
    let after_segments = db.files()?.iter().filter(|p| p.extension().is_some_and(|e| e == "idx")).count();
    assert!(after_segments < before_segments, "{before_segments} -> {after_segments}");
    let check = |db: &Nine| -> Result<()> {
        for (q, from, want) in &expected {
            let got = db.search_refs(q, *from)?;
            assert_eq!(serde_json::to_vec(&got)?, serde_json::to_vec(want)?, "{:?}", q.text);
        }
        for rec in &records {
            assert_eq!(
                serde_json::to_vec(&db.record(&rec.id)?.unwrap())?,
                serde_json::to_vec(rec)?
            );
        }
        assert_eq!(
            serde_json::to_vec(&db.range(-5, 100, 10_000)?)?,
            serde_json::to_vec(&records_by_time(&records))?
        );
        Ok(())
    };
    check(&db)?;
    drop(db);
    let db = Nine::open_with(&tmp.0, options)?;
    check(&db)?;
    Ok(())
}

fn records_by_time(records: &[Record]) -> Vec<Record> {
    let mut v: Vec<(i64, usize)> = records.iter().enumerate().map(|(i, r)| (r.valid_at, i)).collect();
    v.sort();
    v.into_iter().map(|(_, i)| records[i].clone()).collect()
}

#[test]
fn background_merges_run_alongside_concurrent_writers_and_readers() -> Result<()> {
    let tmp = Scratch::new();
    let options = Options {
        merge_fanout: 2,
        ..small(400)
    };
    let db = Arc::new(Nine::open_with(&tmp.0, options.clone())?);
    let writers: Vec<_> = (0..4)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || -> Result<Vec<String>> {
                let mut ids = Vec::new();
                for i in 0..60 {
                    let text = format!("worker{t} step{i} merge stress payload alpha beta");
                    ids.push(db.put(i, &text)?);
                    if i % 7 == 0 {
                        let found = db.find(&format!("worker{t} step{i}"), i64::MIN, 1000, 5)?;
                        assert!(!found.matches.is_empty());
                    }
                }
                Ok(ids)
            })
        })
        .collect();
    let mut ids = Vec::new();
    for w in writers {
        ids.extend(w.join().expect("writer")?);
    }
    db.compact()?;
    assert_eq!(db.len(), 240);
    for id in &ids {
        assert!(db.read(id)?.is_some());
    }
    let n = db.find("merge stress", i64::MIN, 1000, 1000)?.matches.len();
    assert_eq!(n, 240);
    drop(db);
    let db = Nine::open_with(&tmp.0, options)?;
    assert_eq!(db.find("merge stress", i64::MIN, 1000, 1000)?.matches.len(), 240);
    // Count the manifest's segments: files on disk may include a merge that
    // the background thread started after reopening.
    let (segments, levels) = {
        let w = lock(&db.writer)?;
        let levels: Vec<(u32, u32)> = w.manifest.segments.iter().map(|e| (e.level, e.rows)).collect();
        (w.manifest.segments.len(), levels)
    };
    assert!(segments <= 6, "{segments} segments after merging: {levels:?}");
    Ok(())
}

#[test]
fn merge_plan_is_a_counter_that_never_strands_older_segments() {
    let manifest = |levels: &[u32]| Manifest {
        format: FORMAT.into(),
        archive: "x".into(),
        segments: levels
            .iter()
            .enumerate()
            .map(|(i, l)| SegmentEntry { id: i as u64, start: i as u32, rows: 1, level: *l })
            .collect(),
        next_segment: 0,
        wal_gen: 1,
        rows: levels.len() as u32,
        migrated_from: None,
        shared_log: None,
    };
    assert_eq!(merge::plan(&manifest(&[0, 0, 0]), 2), Some(0..2));
    assert_eq!(merge::plan(&manifest(&[2, 1, 0, 0]), 2), Some(2..4));
    assert_eq!(merge::plan(&manifest(&[2, 1, 0]), 2), None);
    assert_eq!(merge::plan(&manifest(&[1, 1, 0]), 2), Some(0..2));
    assert_eq!(merge::plan(&manifest(&[0, 0, 0, 0]), 8), None);
    // Simulate n flushes with immediate merging: segments == digit sum.
    for fanout in [2usize, 3, 8] {
        let mut levels: Vec<u32> = Vec::new();
        for n in 1..=200usize {
            levels.push(0);
            while let Some(r) = merge::plan(&manifest(&levels), fanout) {
                let l = levels[r.start] + 1;
                levels.splice(r, [l]);
            }
            assert!(levels.windows(2).all(|w| w[0] >= w[1]));
            let mut digits = 0;
            let mut m = n;
            while m > 0 {
                digits += m % fanout;
                m /= fanout;
            }
            assert_eq!(levels.len(), digits, "fanout {fanout} n {n}");
        }
    }
}

#[test]
fn stored_only_records_are_exact_and_readable_but_never_searched_or_counted() -> Result<()> {
    let tmp = Scratch::new();
    let options = Options {
        merge_fanout: 2,
        background: Background::View,
        ..small(120)
    };
    let indexed_only = Scratch::new();
    let reference = Nine::open_with(&indexed_only.0, options.clone())?;
    let db = Nine::open_with(&tmp.0, options.clone())?;
    let mut thinking = Vec::new();
    for turn in 0..30i64 {
        let call = format!("tool call open_file src/module{turn}.rs");
        let result = format!("tool result fn handler{turn}() returns ok");
        let thought = format!("thinking: maybe handler{turn} secretplan{turn} is wrong");
        let ids = db.put_batch_indexed(&[
            (turn, call.as_str(), true),
            (turn, thought.as_str(), false),
            (turn, result.as_str(), true),
        ])?;
        reference.put_batch(&[(turn, call.as_str()), (turn, result.as_str())])?;
        thinking.push((ids[1].clone(), thought));
    }
    let check = |db: &Nine| -> Result<()> {
        for (id, thought) in &thinking {
            assert_eq!(db.read(id)?.as_deref(), Some(thought.as_str()));
        }
        assert!(db.find("secretplan7", i64::MIN, 100, 10)?.matches.is_empty());
        let hits = db.find("handler7 maybe wrong", i64::MIN, 100, 10)?;
        assert!(hits.matches.iter().all(|m| !thinking.iter().any(|(id, _)| *id == m.id)));
        // Stored-only rows change neither candidates nor statistics.
        let want = reference.find("handler7 maybe wrong", i64::MIN, 100, 10)?;
        assert_eq!(
            hits.matches.iter().map(|m| m.score_bits.to_bits()).collect::<Vec<_>>(),
            want.matches.iter().map(|m| m.score_bits.to_bits()).collect::<Vec<_>>()
        );
        let all = db.range(i64::MIN, i64::MAX, 10_000)?;
        assert_eq!(all.len(), 90);
        assert_eq!(db.range(7, 7, 10)?.len(), 3);
        Ok(())
    };
    check(&db)?;
    db.flush()?;
    db.compact()?;
    check(&db)?;
    drop(db);
    let db = Nine::open_with(&tmp.0, options)?;
    check(&db)?;
    assert_eq!(db.stats()?.indexed_postings, reference.stats()?.indexed_postings);
    Ok(())
}

#[test]
fn query_terms_equal_the_reference_composition() {
    use crate::text::{query_terms, query_terms_reference};
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    let pieces = [
        "Running", "runs", "THE", "the", "of", "Files", "file_name", "__init__", "_", "__",
        "E_TEMP", "東京", "ΣΑΣ", "Straße", "café", "İstanbul", "x2", "42", "repair_shadow_page",
        "a", "an", "Is", "self.x", "", "🦀", "naïve", "WORKERS", "worker's",
    ];
    let seps = [" ", "\n", ", ", "_", ".", "-", "/", "::", "  ", "\u{2014}", "\u{a0}", "\u{b7}", "\u{1F600}", "\u{b2}", "\u{2167}"];
    for case in 0..2000 {
        let n = next(30) as usize;
        let mut text = String::new();
        for _ in 0..n {
            text.push_str(pieces[next(pieces.len() as u64) as usize]);
            text.push_str(seps[next(seps.len() as u64) as usize]);
        }
        assert_eq!(query_terms(&text), query_terms_reference(&text), "case {case}: {text:?}");
        // A second call answers stems from the per-thread cache.
        assert_eq!(query_terms(&text), query_terms_reference(&text), "cached {case}: {text:?}");
    }
    for text in ["", "the of a", "THE", "___", "_x_", "Σ", "ΑΣ ΣΑ"] {
        assert_eq!(query_terms(text), query_terms_reference(text), "{text:?}");
    }
}

#[test]
fn sorted_dictionary_lookups_equal_single_lookups() -> Result<()> {
    let dir = Scratch::new();
    let v4 = Nine::open_with(&dir.0, small(usize::MAX))?;
    let records = workload(5, 400);
    for rec in &records {
        v4.insert(rec.clone())?;
    }
    v4.flush()?;
    let st = v4.read_state()?;
    let mut vocabulary = std::collections::BTreeSet::new();
    for s in &st.segments {
        s.for_each_term(|t, _| {
            vocabulary.insert(String::from_utf8(t.to_vec()).unwrap());
        })?;
    }
    for extra in ["", "0", "aaaa", "alphaz", "zzzz", "~", "東", "running_x"] {
        vocabulary.insert(extra.to_string());
    }
    let all: Vec<&str> = vocabulary.iter().map(String::as_str).collect();
    for s in &st.segments {
        for stride in [1, 2, 3, 7, 50] {
            for offset in 0..stride.min(all.len()) {
                let picked: Vec<&str> = all.iter().skip(offset).step_by(stride).copied().collect();
                let sorted = s.terms_sorted(&picked);
                for (t, got) in picked.iter().zip(sorted) {
                    let want = s.term(t);
                    assert_eq!(
                        got.map(|i| (i.df, i.cf, s.postings_bytes(i).to_vec())),
                        want.map(|i| (i.df, i.cf, s.postings_bytes(i).to_vec())),
                        "term {t:?} stride {stride}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn multi_chunk_scans_equal_across_thread_counts_and_reopen() -> Result<()> {
    // CHUNK_ROWS is 64 under test: 900 rows give multi-chunk segments, so
    // cursors resume across chunks on one thread and restart on others.
    let dir = Scratch::new();
    let opts = |threads: usize| Options {
        query_threads: threads,
        ..small(4_000)
    };
    let records = workload(31, 900);
    {
        let v4 = Nine::open_with(&dir.0, opts(1))?;
        for chunk in records.chunks(50) {
            v4.insert_batch(chunk.to_vec())?;
        }
        v4.compact_all()?;
        for chunk in workload(37, 100).chunks(10) {
            let chunk: Vec<Record> = chunk
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    r.id = format!("late-{}", r.id);
                    r
                })
                .collect();
            v4.insert_batch(chunk)?;
        }
    }
    let results = |threads: usize| -> Result<Vec<(Matches, TimelineMetrics)>> {
        let v4 = Nine::open_with(&dir.0, opts(threads))?;
        let mut out = Vec::new();
        for text in QUERIES.iter().chain(["files running alpha gamma delta workers fn", "page_x shadow"].iter()) {
            for (valid, known, from) in [(30, 30, i64::MIN), (6, 7, i64::MIN), (8, 3, 2), (11, 40, 5)] {
                let mut q = Query::new(text, valid);
                q.known_at = known;
                q.k = 25;
                v4.clear_query_cache();
                out.push((v4.search_refs(&q, from)?, v4.metrics()));
            }
        }
        Ok(out)
    };
    let one = results(1)?;
    for threads in [2, 4] {
        for (i, ((a, ma), (b, mb))) in one.iter().zip(results(threads)?.iter()).enumerate() {
            let ctx = format!("threads {threads} case {i}");
            same(a, b, &ctx);
            assert_eq!(a.routing_entropy_bits.to_bits(), b.routing_entropy_bits.to_bits(), "{ctx}");
            same_metrics(ma, mb, &ctx);
        }
    }
    Ok(())
}

#[test]
fn stores_without_versions_take_the_simple_scan_and_match_the_v3_oracle() -> Result<()> {
    // Unique keys: no version groups, so current-time views of fully
    // visible segments use the branch-free scan loops.
    for (seed, flush, threads) in [(41u64, 700usize, 1usize), (43, 2500, 4), (47, usize::MAX, 1)] {
        let a = Scratch::new();
        let b = Scratch::new();
        let mut v3 = V3::open(&a.0)?;
        let v4 = Nine::open_with(
            &b.0,
            Options {
                query_threads: threads,
                ..small(flush)
            },
        )?;
        let records: Vec<Record> = workload(seed, 500)
            .into_iter()
            .map(|mut r| {
                r.key = r.id.clone();
                r
            })
            .collect();
        for chunk in records.chunks(7) {
            for rec in chunk {
                v3.insert(rec.clone())?;
            }
            v4.insert_batch(chunk.to_vec())?;
        }
        assert!(v4.read_state()?.groups.list.is_empty());
        for label in ["live", "merged"] {
            if label == "merged" {
                v4.compact_all()?;
            }
            for text in QUERIES.iter().chain(["files running alpha gamma delta workers fn", "E_TEMP shadow"].iter()) {
                for (valid, known, from) in [(30, 30, i64::MIN), (100, 100, 0), (6, 7, i64::MIN), (11, 40, 5)] {
                    for k in [1, 20, 500] {
                        let mut q = Query::new(text, valid);
                        q.known_at = known;
                        q.k = k;
                        let x = v3.search_refs(&q, from)?;
                        let mx = v3.metrics();
                        let y = v4.search_refs(&q, from)?;
                        let my = v4.metrics();
                        let ctx = format!("{label} seed={seed} q={text:?} v={valid} from={from} k={k}");
                        same(&x, &y, &ctx);
                        same_metrics(&mx, &my, &ctx);
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn top_k_entropy_returns_the_same_ids_and_score_bits_as_exhaustive_scoring() -> Result<()> {
    for (seed, unique_keys) in [(53u64, false), (59, true), (61, true)] {
        let dir = Scratch::new();
        {
            let v4 = Nine::open_with(&dir.0, small(1_500))?;
            let records: Vec<Record> = workload(seed, 700)
                .into_iter()
                .map(|mut r| {
                    if unique_keys {
                        r.key = r.id.clone();
                    }
                    r
                })
                .collect();
            for chunk in records.chunks(9) {
                v4.insert_batch(chunk.to_vec())?;
            }
            for (q, source, useful, at) in [("alpha beta", "a-003", true, 20), ("alpha beta", "b-010", false, 20)] {
                let _ = v4.record_outcome(q, source, useful, at, "fixture");
            }
            v4.compact_all()?;
            for chunk in workload(seed + 1, 60).chunks(6) {
                let chunk: Vec<Record> = chunk
                    .iter()
                    .map(|r| {
                        let mut r = r.clone();
                        r.id = format!("late-{}", r.id);
                        r.key = r.id.clone();
                        r
                    })
                    .collect();
                v4.insert_batch(chunk)?;
            }
        }
        let run = |entropy: RoutingEntropy, threads: usize| -> Result<Vec<(Matches, TimelineMetrics)>> {
            let v4 = Nine::open_with(
                &dir.0,
                Options {
                    routing_entropy: entropy,
                    query_threads: threads,
                    ..small(1_500)
                },
            )?;
            let mut out = Vec::new();
            for text in QUERIES.iter().chain(["files running alpha gamma delta workers fn page", "E_TEMP shadow"].iter()) {
                for (valid, known, from) in [(30, 30, i64::MIN), (100, 100, 0), (6, 7, i64::MIN), (11, 40, 5)] {
                    for k in [1, 3, 20, 63, 64, 65, 500] {
                        let mut q = Query::new(text, valid);
                        q.known_at = known;
                        q.k = k;
                        out.push((v4.search_refs(&q, from)?, v4.metrics()));
                    }
                }
            }
            Ok(out)
        };
        let all = run(RoutingEntropy::All, 1)?;
        for threads in [1, 4] {
            for (i, ((a, ma), (b, mb))) in all.iter().zip(run(RoutingEntropy::TopK, threads)?.iter()).enumerate() {
                let ctx = format!("seed {seed} threads {threads} case {i}");
                let ids = |m: &Matches| m.matches.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
                assert_eq!(ids(a), ids(b), "{ctx}");
                for (x, y) in a.matches.iter().zip(&b.matches) {
                    assert_eq!(x.score_bits.to_bits(), y.score_bits.to_bits(), "{ctx}");
                    assert_eq!((x.time, x.bytes), (y.time, y.bytes), "{ctx}");
                }
                assert_eq!(ma.candidate_records, mb.candidate_records, "{ctx}");
                assert_eq!(ma.background_query_bits.to_bits(), mb.background_query_bits.to_bits(), "{ctx}");
                assert_eq!(
                    ma.best_conditional_query_bits.map(f64::to_bits),
                    mb.best_conditional_query_bits.map(f64::to_bits),
                    "{ctx}"
                );
                assert!(mb.scored_records <= ma.scored_records, "{ctx}");
            }
        }
    }
    Ok(())
}

#[test]
fn version_one_segments_take_every_scan_path_and_match_the_v3_oracle() -> Result<()> {
    // LEB128 (version 1) segments, as old stores have: written on this
    // thread (merges too, so no background merge thread).
    crate::segment::WRITE_V1.with(|v| v.set(true));
    for (seed, unique_keys, threads) in [(71u64, false, 1usize), (73, true, 1), (79, true, 4)] {
        let a = Scratch::new();
        let b = Scratch::new();
        let mut v3 = V3::open(&a.0)?;
        let v4 = Nine::open_with(
            &b.0,
            Options {
                query_threads: threads,
                background_merges: false,
                ..small(1_200)
            },
        )?;
        let records: Vec<Record> = workload(seed, 600)
            .into_iter()
            .map(|mut r| {
                if unique_keys {
                    r.key = r.id.clone();
                }
                r
            })
            .collect();
        for chunk in records.chunks(11) {
            for rec in chunk {
                v3.insert(rec.clone())?;
            }
            v4.insert_batch(chunk.to_vec())?;
        }
        v4.compact_all()?;
        v4.flush()?;
        assert!(v4.read_state()?.segments.iter().all(|s| s.version() == 1));
        for text in QUERIES.iter().chain(["files running alpha gamma delta workers fn page", "E_TEMP shadow"].iter()) {
            for (valid, known, from) in [(30, 30, i64::MIN), (100, 100, 0), (6, 7, i64::MIN), (11, 40, 5)] {
                for k in [1, 20, 500] {
                    let mut q = Query::new(text, valid);
                    q.known_at = known;
                    q.k = k;
                    let x = v3.search_refs(&q, from)?;
                    let mx = v3.metrics();
                    let y = v4.search_refs(&q, from)?;
                    let my = v4.metrics();
                    let ctx = format!("v1 seed={seed} q={text:?} v={valid} from={from} k={k}");
                    same(&x, &y, &ctx);
                    same_metrics(&mx, &my, &ctx);
                    // The second ask builds skip tables: repeat once.
                    let z = v4.search_refs(&q, from)?;
                    same(&x, &z, &ctx);
                }
            }
        }
    }
    crate::segment::WRITE_V1.with(|v| v.set(false));
    Ok(())
}

#[test]
fn result_ids_read_from_row_prefixes_equal_the_stored_ids() -> Result<()> {
    // Short ids come from the 8-byte prefix when the id table confirms them;
    // ids padded with NUL bytes, exactly 8 bytes, longer, or non-ASCII must
    // still come back exactly.
    let dir = Scratch::new();
    let v4 = Nine::open_with(&dir.0, small(usize::MAX))?;
    let ids = [
        "a", "ab", "ab\u{0}", "ab\u{0}\u{0}", "\u{0}ab", "abcdefg", "abcdefgh", "abcdefgh\u{0}",
        "abcdefghi", "café", "日本語", "12345678901", "x\u{0}y",
    ];
    for (i, id) in ids.iter().enumerate() {
        v4.insert(r(id, &format!("k{i}"), "shared needle word", i as i64, i as i64))?;
    }
    v4.flush()?;
    let mut q = Query::new("needle", 100);
    q.k = 100;
    let got: std::collections::BTreeSet<String> =
        v4.search_refs(&q, i64::MIN)?.matches.into_iter().map(|m| m.id).collect();
    let want: std::collections::BTreeSet<String> = ids.iter().map(|s| s.to_string()).collect();
    assert_eq!(got, want);
    Ok(())
}

/// F2 of the outcome eval (`docs/SEARCH_FIXES.md`): a query naming an
/// identifier anchors its candidate view to the records containing it.
/// When that view holds fewer than k records the list must go on with the
/// query's unanchored ranking (the identifier read as its words) instead of
/// stopping there. A long query whose only identifier occurs in one
/// near-verbatim record used to return that record alone.
#[test]
fn an_anchored_view_smaller_than_k_continues_with_the_unanchored_ranking() -> Result<()> {
    let query = "autowrap with the cython backend fails when array arguments do not appear in the \
                 wrapped expression: the generated function declares autofunc_result with an \
                 incorrect signature for the array argument";
    let split = query.replace("autofunc_result", "autofunc result");
    let words = [
        "autowrap", "cython", "backend", "array", "arguments", "wrapped", "expression", "generated",
        "function", "signature", "result", "incorrect", "declares", "appear",
    ];
    let fillers = ["printer", "latex", "series", "limit", "integral", "solver", "parser", "lambda"];
    let mut records = Vec::new();
    for i in 0..60usize {
        let text = (0..1 + i % 5)
            .map(|j| words[(i * 3 + j * 5) % words.len()])
            .chain((0..1 + i % 4).map(|j| fillers[(i + j) % fillers.len()]))
            .collect::<Vec<_>>()
            .join(" ");
        records.push(r(&format!("r{i:02}"), &format!("r{i:02}"), &text, i as i64, i as i64));
    }
    for i in 0..4usize {
        // a second identifier with four occurrences
        let text = format!("codegen_helper {} {}", words[i], fillers[i]);
        records.push(r(&format!("d{i}"), &format!("d{i}"), &text, 61, 61));
    }
    records.push(r("u0", "u0", "nothing in common here", 62, 62));
    records.push(r("nv", "nv", &format!("issue | {query}"), 70, 70));
    let (a, b) = (Scratch::new(), Scratch::new());
    let mut v3 = V3::open(&a.0)?;
    let v4 = Nine::open_with(&b.0, small(400))?;
    for rec in &records {
        v3.insert(rec.clone())?;
        v4.insert(rec.clone())?;
    }
    let mut find = |text: &str, valid: i64, k: usize| -> Result<Matches> {
        let mut q = Query::new(text, valid);
        q.k = k;
        let got = v4.search_refs(&q, i64::MIN)?;
        same(&v3.search_refs(&q, i64::MIN)?, &got, &format!("oracle q={text:?} at={valid} k={k}"));
        Ok(got)
    };
    let ids = |m: &[SourceMatch]| m.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
    let bits = |m: &[SourceMatch]| m.iter().map(|x| x.score_bits.to_bits()).collect::<Vec<_>>();
    // The query's unanchored ranking: every record sharing a word, nv first.
    let unanchored = find(&split, 100, 500)?.matches;
    assert_eq!(unanchored.len(), 65, "{:?}", ids(&unanchored));
    assert_eq!(unanchored[0].id, "nv");
    let rest: Vec<SourceMatch> = unanchored.iter().filter(|m| m.id != "nv").cloned().collect();
    let longest = find(query, 100, 500)?.matches;
    for k in [1, 2, 5, 20, 64, 65, 500] {
        let got = find(query, 100, k)?.matches;
        assert_eq!(got.len(), k.min(65), "k={k}: {:?}", ids(&got));
        assert_eq!(got[0].id, "nv", "k={k}");
        assert!(!got[0].continued && got[1..].iter().all(|m| m.continued), "k={k}");
        // after the anchored view, the unanchored ranking in its own order and bits
        assert_eq!(ids(&got[1..]), ids(&rest[..got.len() - 1]), "k={k}");
        assert_eq!(bits(&got[1..]), bits(&rest[..got.len() - 1]), "k={k}");
        // a shorter list is a prefix of a longer one
        assert_eq!(ids(&got), ids(&longest[..got.len()]), "k={k}");
    }
    // Before nv existed no record named the identifier: the query read it as
    // its words, as it always did.
    let before = find(query, 69, 20)?.matches;
    assert_eq!(ids(&before), ids(&find(&split, 69, 20)?.matches));
    assert!(before.iter().all(|m| !m.continued));
    assert_eq!(bits(&before), bits(&find(&split, 69, 20)?.matches));
    // An anchored view with at least k records is the whole list, unchanged;
    // past it the list goes on with the unanchored ranking.
    let lookup = "where is codegen_helper declared for the array backend";
    let anchored = find(lookup, 100, 4)?.matches;
    assert_eq!(ids(&anchored).iter().filter(|x| x.starts_with('d')).count(), 4);
    assert!(anchored.iter().all(|m| !m.continued));
    for k in [1, 3, 4] {
        assert_eq!(ids(&find(lookup, 100, k)?.matches), ids(&anchored[..k]), "k={k}");
    }
    let words_only = find(&lookup.replace("codegen_helper", "codegen helper"), 100, 500)?.matches;
    let after: Vec<SourceMatch> = words_only.iter().filter(|m| !m.id.starts_with('d')).cloned().collect();
    assert!(after.len() > 10);
    let longer = find(lookup, 100, 500)?.matches;
    assert_eq!(longer.len(), words_only.len());
    assert_eq!(ids(&longer[..4]), ids(&anchored));
    assert_eq!(bits(&longer[..4]), bits(&anchored));
    assert!(longer[4..].iter().all(|m| m.continued));
    assert_eq!(ids(&longer[4..]), ids(&after));
    assert_eq!(bits(&longer[4..]), bits(&after));
    Ok(())
}

/// F3 (`docs/SEARCH_FIXES.md`): the candidate background of a small
/// candidate set is padded with whole-view text, so the best match of a
/// small set scores on the same scale as matches of a large one (the
/// injection hook's `min_bits` of 8 means the same thing), without future
/// rows and without changing large candidate sets.
#[test]
fn a_small_candidate_set_is_scored_against_a_padded_background() -> Result<()> {
    let vocabulary = [
        "printer", "latex", "series", "limit", "integral", "solver", "parser", "lambda", "matrix",
        "symbol", "expression", "array", "backend", "function", "result", "argument", "module",
        "precision", "float", "rational", "simplify", "derivative", "polynomial", "factor",
        "sympify", "evaluate", "numeric", "complex", "domain", "interval", "plot", "tensor",
        "vector", "quantum", "physics", "units", "geometry", "point", "circle", "logic",
    ];
    let mut records = Vec::new();
    for i in 0..1_500usize {
        let text = (0..12 + i % 20)
            .map(|j| vocabulary[(i * 7 + j * 3 + j * j) % vocabulary.len()])
            .collect::<Vec<_>>()
            .join(" ");
        records.push(r(&format!("r{i:03}"), &format!("r{i:03}"), &text, i as i64, i as i64));
    }
    let issue = "the cython backend declares autofunc_result with an incorrect signature for the \
                 array argument; the generated wrapper rejects the matrix expression";
    records.push(r("issue", "issue", issue, 1_600, 1_600));
    let (a, b) = (Scratch::new(), Scratch::new());
    let mut v3 = V3::open(&a.0)?;
    let v4 = Nine::open_with(&b.0, small(4_000))?;
    for rec in &records {
        v3.insert(rec.clone())?;
        v4.insert(rec.clone())?;
    }
    fn search(v3: &mut V3, v4: &Nine, text: &str, valid: i64, k: usize) -> Result<(Matches, TimelineMetrics)> {
        let mut q = Query::new(text, valid);
        q.k = k;
        let got = v4.search_refs(&q, i64::MIN)?;
        let metrics = v4.metrics();
        same(&v3.search_refs(&q, i64::MIN)?, &got, &format!("oracle q={text:?}"));
        same_metrics(&v3.metrics(), &metrics, &format!("oracle q={text:?}"));
        Ok((got, metrics))
    }
    let mut find = |text: &str, valid: i64, k: usize| search(&mut v3, &v4, text, valid, k);
    // The identifier anchors the view to the issue alone: one candidate.
    let (anchored, m) = find(issue, 2_000, 1)?;
    assert_eq!((anchored.matches[0].id.as_str(), m.candidate_records), ("issue", 1));
    let bits = anchored.matches[0].score_bits;
    assert!(bits >= 8.0, "the best and only candidate must clear min_bits: {bits}");
    assert!(m.background_pad > 0.0);
    // On the scale of the same record when the query is read as words (a
    // large candidate set, not padded).
    let (words, mw) = find(&issue.replace("autofunc_result", "autofunc result"), 2_000, 1)?;
    assert_eq!((words.matches[0].id.as_str(), mw.background_pad), ("issue", 0.0));
    let ratio = bits / words.matches[0].score_bits;
    eprintln!("anchored {bits:.1} bits, as words {:.1} bits", words.matches[0].score_bits);
    assert!((0.25..4.0).contains(&ratio), "{bits} vs {} bits", words.matches[0].score_bits);
    // A rare word in one record: also a single candidate, also above min_bits.
    let (rare, mr) = find("rejects wrapper", 2_000, 5)?;
    assert_eq!(rare.matches.len(), 1);
    assert!(mr.background_pad > 0.0 && rare.matches[0].score_bits >= 8.0, "{:?}", rare.matches);
    // The padding comes from the query's view only: rows written after it
    // change nothing in an earlier view.
    let before = find("rejects wrapper array", 1_700, 10)?.0;
    let big = find("printer latex series limit", 2_000, 10)?.1;
    drop(find);
    for i in 0..50 {
        v3.insert(r(&format!("late{i}"), &format!("late{i}"), "rejects wrapper array array", 3_000, 3_000))?;
        v4.insert(r(&format!("late{i}"), &format!("late{i}"), "rejects wrapper array array", 3_000, 3_000))?;
    }
    let after = search(&mut v3, &v4, "rejects wrapper array", 1_700, 10)?.0;
    same(&before, &after, "future rows");
    // Candidate sets at or above the floor are not padded.
    assert!(big.candidate_records > 100 && big.background_pad == 0.0, "{big:?}");
    Ok(())
}
