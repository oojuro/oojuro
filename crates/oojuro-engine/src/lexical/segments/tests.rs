use super::*;
use crate::capture_log::{
    Batch, Decision, Event, Identity, Line, LineKind, Outcome, ParsedBatch, PolicyReceipt, Source,
};
use std::sync::atomic::{AtomicU64, Ordering};
const SOURCE: Source = [14; 16];
const KEY: [u8; 32] = [28; 32];
const WRITER: &str = "native-synthetic-owner";
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::current_dir()
            .unwrap()
            .join(".scratch")
            .join(format!(
                "segments-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn open(&self) -> CaptureLog {
        CaptureLog::open(&self.root.join("authority"), &KEY, identity()).unwrap()
    }
    fn cache(&self, log: &CaptureLog) -> PersistentIndex {
        PersistentIndex::open(log, &self.root.join("cache")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn identity() -> Identity {
    Identity {
        writer: WRITER.into(),
        agent: 1,
        first_id: 9,
        adapter: "native-test-v1".into(),
    }
}
fn event(text: &str, ts: i64, kind: u8, indexed: bool) -> Event {
    Event {
        id: 0,
        session: 77,
        parent_session: 0,
        agent: 1,
        turn: 1,
        seq: 0,
        parent: 0,
        revises: 0,
        kind,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: ts,
        duration_us: 0,
        tool: String::new(),
        args: String::new(),
        text: text.into(),
        indexed,
        line_start: 0,
        line_end: 0,
        emission: 0,
    }
}
fn batch(log: &CaptureLog, events: Vec<Event>) -> Batch {
    let p = log.progress(SOURCE).unwrap();
    let next = log.next_id();
    let n = events.len();
    let events: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(i, mut e)| {
            e.id = next + i as u64;
            e.seq = (e.id - 9) as u32;
            e.line_start = p.raw_end + 3 * i as u64;
            e.line_end = e.line_start + 3;
            if e.args.is_empty() {
                e.args = format!(r#"{{"src":"1:{}:0","rt":100,"keep":"same"}}"#, e.line_start);
            }
            e
        })
        .collect();
    Batch {
        raw: b"{}\n".repeat(n),
        parsed: ParsedBatch {
            source: SOURCE,
            raw_start: p.raw_end,
            parsed_before: p.parsed_end,
            outcome: Outcome {
                raw_end: p.raw_end + 3 * n as u64,
                parsed_end: p.raw_end + 3 * n as u64,
                first_id: events.first().map(|e| e.id),
                stored: n as u64,
                next_id: next + n as u64,
            },
            policy: PolicyReceipt {
                policy_id: "test-policy-v1".into(),
                raw_authorization: "synthetic-consent".into(),
                lines: events
                    .iter()
                    .map(|e| Line {
                        start: e.line_start,
                        end: e.line_end,
                        kind: LineKind::Parsed,
                        emissions: vec![Decision::Stored {
                            id: e.id,
                            indexed: e.indexed,
                        }],
                    })
                    .collect(),
            },
            context: br#"{"complete":"synthetic-parser-receipt"}"#.to_vec(),
            events,
        },
    }
}

fn register(log: &mut CaptureLog) {
    log.register(SOURCE, b"synthetic", b"{}").unwrap();
}
fn append(log: &mut CaptureLog, text: &str) {
    let b = batch(log, vec![event(text, log.next_id() as i64, 3, true)]);
    log.commit(b).unwrap();
}
fn query(text: &str) -> Query<'_> {
    Query {
        writer: WRITER,
        agent: 1,
        text,
        from: i64::MIN,
        to: i64::MAX,
        k: 100,
    }
}
fn signature(hits: Vec<Hit>) -> Vec<(u64, u64, bool, i64, u64)> {
    hits.into_iter()
        .map(|h| (h.id, h.bits.to_bits(), h.continued, h.time, h.row))
        .collect()
}
fn parity(cache: &PersistentIndex, log: &CaptureLog) {
    let fresh = Index::rebuild(log).unwrap();
    for text in [
        "",
        "alpha",
        "foo_bar",
        "foo",
        "\"alpha beta\"",
        "key:9",
        "10",
        "durable capture",
        "日本語",
        "<missing>",
    ] {
        assert_eq!(
            signature(cache.search(log, &query(text)).unwrap()),
            signature(fresh.search(&query(text)).unwrap()),
            "query={text}"
        );
    }
    for doc in &cache.index.documents {
        assert!(
            log.event(doc.event.id)
                .unwrap()
                .is_some_and(|(_, e)| e == &doc.event)
        );
    }
}
#[test]
fn reopen_without_tokenization_extend_dedupe_and_merge_exact_scores() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    let mut first = event("alpha beta foo_bar 日本語", 10, 3, true);
    first.tool = "Read".into();
    first.args = r#"{"path":"same","rt":9}"#.into();
    let mut duplicate = first.clone();
    duplicate.ts_us = 90;
    duplicate.args = r#"{"path":"same","rt":99}"#.into();
    log.commit(batch(
        &log,
        vec![
            first,
            duplicate,
            event("alpha durable capture", 30, 3, true),
            event("stored private payload", 40, 2, false),
        ],
    ))
    .unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert_eq!(cache.index.documents.len(), 3);
    parity(&cache, &log);
    drop(cache);
    drop(log);
    let mut log = f.open();
    let before = super::super::ANALYSES.with(|n| n.get());
    let mut cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(
        before,
        super::super::ANALYSES.with(|n| n.get()),
        "valid cache must not analyze any stored text"
    );
    parity(&cache, &log);
    append(&mut log, "alpha durable extension");
    assert_eq!(
        cache.search(&log, &query("alpha")).unwrap_err(),
        Error::Unavailable
    );
    let before = super::super::ANALYSES.with(|n| n.get());
    cache.refresh(&log).unwrap();
    assert_eq!(super::super::ANALYSES.with(|n| n.get()) - before, 1);
    assert_eq!(cache.segment_count(), 2);
    parity(&cache, &log);
    let original = signature(cache.search(&log, &query("alpha")).unwrap());
    cache.merge(&log).unwrap();
    assert_eq!(cache.segment_count(), 1);
    assert_eq!(
        original,
        signature(cache.search(&log, &query("alpha")).unwrap())
    );
    drop(cache);
    let loaded = f.cache(&log);
    assert_eq!(loaded.opened(), Opened::Loaded);
    parity(&loaded, &log);
}
#[test]
fn stale_cache_extends_only_committed_receipts_and_registration_is_a_watermark() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    append(&mut log, "alpha");
    let cache = f.cache(&log);
    drop(cache);
    append(&mut log, "beta alpha");
    let before = super::super::ANALYSES.with(|n| n.get());
    let mut cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Extended);
    assert_eq!(super::super::ANALYSES.with(|n| n.get()) - before, 1);
    log.register([19; 16], b"second", b"{}").unwrap();
    assert!(cache.search(&log, &query("alpha")).is_err());
    let segments = cache.segment_count();
    cache.refresh(&log).unwrap();
    assert_eq!(segments, cache.segment_count());
    parity(&cache, &log);
}
#[test]
fn lost_corrupt_missing_ahead_forked_and_compacted_cache_rebuilds_without_ghosts() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    append(&mut log, "alpha original");
    let cache = f.cache(&log);
    let manifest = cache.manifest.clone();
    let snapshot = fs::read(f.root.join("cache/LEXICAL")).unwrap();
    drop(cache);
    let path = f
        .root
        .join(format!("cache/seg-{}.idx", manifest.segments[0].id));
    let mut bad = fs::read(&path).unwrap();
    bad[50] ^= 1;
    fs::write(&path, bad).unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &log);
    drop(cache);
    fs::remove_file(f.root.join("cache/LEXICAL")).unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    let segment = cache.manifest.segments[0].id.clone();
    drop(cache);
    fs::remove_file(f.root.join(format!("cache/seg-{segment}.idx"))).unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    drop(cache);
    // Valid AEAD but unsupported/ahead manifest is still rejected semantically.
    let mut m: Manifest = serde_json::from_slice(
        &log.cache_cipher()
            .open_frame(
                &manifest_aad(log.cache_origin().unwrap(), super::identity(&log).unwrap()),
                &fs::read(f.root.join("cache/LEXICAL")).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    m.prefix.records += 1;
    let bytes = log.cache_cipher().seal_frame(
        &manifest_aad(m.origin, m.identity),
        &serde_json::to_vec(&m).unwrap(),
    );
    fs::write(f.root.join("cache/LEXICAL"), bytes).unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    drop(cache);
    // New independently-created authority has identical supplied identity/key,
    // but a different sealed origin. Copying the old cache cannot show old rows.
    let other = f.root.join("other");
    let mut second = CaptureLog::open(&other, &KEY, identity()).unwrap();
    register(&mut second);
    append(&mut second, "beta replacement");
    fs::write(f.root.join("cache/LEXICAL"), snapshot).unwrap();
    let cache = f.cache(&second);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &second);
    assert!(
        cache
            .search(&second, &query("original"))
            .unwrap()
            .is_empty()
    );
    drop(cache);
    drop(second);
    log.compact().unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &log);
}
#[test]
fn encrypted_disk_scope_lock_bounds_and_failed_publication_preserve_authority_ack() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    append(&mut log, "highly_unique_synthetic_secret_needle alpha");
    let mut cache = f.cache(&log);
    for entry in fs::read_dir(f.root.join("cache")).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        for needle in [
            b"highly_unique_synthetic_secret_needle".as_slice(),
            WRITER.as_bytes(),
            b"alpha",
        ] {
            assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        }
    }
    assert!(PersistentIndex::open(&log, &f.root.join("cache")).is_err());
    assert!(PersistentIndex::open(&log, &f.root.join("authority")).is_err());
    // Authority append already acknowledged; a derived publication failure does
    // not alter its outcome or allow the stale cache to serve results.
    let b = batch(
        &log,
        vec![event("alpha committed despite failed cache", 20, 3, true)],
    );
    let expected = b.parsed.outcome.clone();
    assert_eq!(log.commit(b).unwrap(), expected);
    fs::remove_file(f.root.join("cache/LEXICAL")).unwrap();
    fs::create_dir(f.root.join("cache/LEXICAL")).unwrap();
    assert_eq!(cache.refresh(&log).unwrap_err(), Error::Unavailable);
    assert!(cache.search(&log, &query("alpha")).is_err());
    assert_eq!(log.next_id(), expected.next_id);
    drop(cache);
    fs::remove_dir(f.root.join("cache/LEXICAL")).unwrap();
    let cache = f.cache(&log);
    parity(&cache, &log);
    drop(cache);
    drop(log);
    let log = f.open();
    assert_eq!(log.next_id(), expected.next_id);
    parity(&f.cache(&log), &log);
}
#[test]
fn authenticated_malformed_cache_cannot_add_ghost_rows_or_plaintext() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    append(&mut log, "alpha");
    let cache = f.cache(&log);
    let mut m = cache.manifest.clone();
    let r = &mut m.segments[0];
    let path = f.root.join(format!("cache/seg-{}.idx", r.id));
    let aad = segment_aad(&cache.manifest, r).unwrap();
    let mut segment = codec::decode(
        &cache
            .cipher
            .open_frame(&aad, &fs::read(&path).unwrap())
            .unwrap(),
    )
    .unwrap();
    segment.rows[0].id = 999;
    segment.rows[0].fingerprint = fingerprint(999);
    let plain = codec::encode(&segment).unwrap();
    r.bytes = plain.len();
    let sealed = cache.cipher.seal_frame(
        &segment_aad(
            &Manifest {
                segments: vec![],
                ..cache.manifest.clone()
            },
            r,
        )
        .unwrap(),
        &plain,
    );
    r.digest = *blake3::hash(&sealed).as_bytes();
    fs::write(path, sealed).unwrap();
    fs::write(
        f.root.join("cache/LEXICAL"),
        cache.cipher.seal_frame(
            &manifest_aad(m.origin, m.identity),
            &serde_json::to_vec(&m).unwrap(),
        ),
    )
    .unwrap();
    drop(cache);
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert!(cache.search(&log, &query("key:999")).unwrap().is_empty());
    parity(&cache, &log);
}
// The ignored helper is launched in a separate OS process, then actually
// SIGKILLed at each durable publication boundary by the parent regression.
#[test]
#[ignore]
fn crash_publication_child() {
    let root = PathBuf::from(std::env::var("OOJURO_SYNTHETIC_CACHE_CRASH_ROOT").unwrap());
    let phase = std::env::var("OOJURO_SYNTHETIC_CACHE_CRASH_PHASE").unwrap();
    let mut log = CaptureLog::open(&root.join("authority"), &KEY, identity()).unwrap();
    let mut cache = PersistentIndex::open(&log, &root.join("cache")).unwrap();
    append(&mut log, "alpha child committed");
    cache.hook = Some(Box::new(move |at| {
        if format!("{at:?}") == phase {
            println!("CACHE_BOUNDARY_READY");
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        }
    }));
    cache.refresh(&log).unwrap();
    panic!("crash hook not reached");
}
#[test]
fn sigkill_segment_and_manifest_publication_never_loses_authoritative_rows() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    for phase in [
        "SegmentTemp",
        "SegmentPublished",
        "ManifestTemp",
        "ManifestPublished",
        "ManifestDurable",
    ] {
        let f = Fixture::new();
        let mut log = f.open();
        register(&mut log);
        append(&mut log, "alpha parent committed");
        drop(f.cache(&log));
        drop(log);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "lexical::segments::tests::crash_publication_child",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env("OOJURO_SYNTHETIC_CACHE_CRASH_ROOT", &f.root)
            .env("OOJURO_SYNTHETIC_CACHE_CRASH_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if line.unwrap().contains("CACHE_BOUNDARY_READY") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        let reached = rx.recv_timeout(Duration::from_secs(8));
        let _ = child.kill();
        let status = child.wait().unwrap();
        reader.join().unwrap();
        assert!(reached.is_ok(), "phase={phase}");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let log = f.open();
        assert_eq!(log.batches().unwrap().len(), 2);
        let cache = f.cache(&log);
        parity(&cache, &log);
        assert_eq!(
            cache.search(&log, &query("alpha")).unwrap().len(),
            2,
            "phase={phase}"
        );
    }
}

#[test]
fn copied_origin_fork_with_equal_highwater_rebuilds_exact_receipt_branch() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    append(&mut log, "common branch history");
    drop(f.cache(&log));
    let origin = log.cache_origin().unwrap();
    drop(log);
    let branch = f.root.join("fork");
    fs::create_dir(&branch).unwrap();
    for entry in fs::read_dir(f.root.join("authority")).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        fs::copy(entry.path(), branch.join(entry.file_name())).unwrap();
    }
    let mut a = f.open();
    append(&mut a, "alpha only branch_a");
    drop(f.cache(&a));
    let a_tip = a.cache_tip().unwrap();
    drop(a);
    let mut b = CaptureLog::open(&branch, &KEY, identity()).unwrap();
    append(&mut b, "beta only branch_b");
    assert_eq!(origin, b.cache_origin().unwrap());
    assert_eq!(a_tip.records, b.cache_tip().unwrap().records);
    assert_ne!(a_tip.digest, b.cache_tip().unwrap().digest);
    let cache = f.cache(&b);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &b);
    assert!(cache.search(&b, &query("branch_a")).unwrap().is_empty());
}
#[test]
fn actual_multisegment_row_boundaries_keep_native_score_bits() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    let events = (0..8300)
        .map(|i| event(&format!("alpha boundary item_{i}"), i, 3, true))
        .collect();
    log.commit(batch(&log, events)).unwrap();
    let mut cache = f.cache(&log);
    assert_eq!(cache.segment_count(), 2);
    let before = signature(cache.search(&log, &query("alpha")).unwrap());
    drop(cache);
    let n = super::super::ANALYSES.with(|n| n.get());
    cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(n, super::super::ANALYSES.with(|n| n.get()));
    assert_eq!(
        before,
        signature(cache.search(&log, &query("alpha")).unwrap())
    );
    cache.merge(&log).unwrap();
    assert_eq!(
        before,
        signature(cache.search(&log, &query("alpha")).unwrap())
    );
}
#[test]
#[ignore]
fn local_native_segment_measurement() {
    let f = Fixture::new();
    let mut log = f.open();
    register(&mut log);
    log.commit(batch(
        &log,
        (0..1000)
            .map(|i| {
                event(
                    &format!("alpha durable capture unique_{i} foo_bar"),
                    i,
                    3,
                    true,
                )
            })
            .collect(),
    ))
    .unwrap();
    let start = std::time::Instant::now();
    let cache = f.cache(&log);
    let rebuild = start.elapsed();
    let count = cache.segment_count();
    let bytes: u64 = fs::read_dir(f.root.join("cache"))
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    drop(cache);
    let mut times = vec![];
    let n = super::super::ANALYSES.with(|n| n.get());
    for _ in 0..10 {
        let start = std::time::Instant::now();
        let cache = f.cache(&log);
        assert_eq!(cache.opened(), Opened::Loaded);
        times.push(start.elapsed().as_micros());
        drop(cache);
    }
    assert_eq!(n, super::super::ANALYSES.with(|n| n.get()));
    times.sort_unstable();
    println!(
        "LOCAL_NATIVE_SEGMENTS rows=1000 segments={count} disk_bytes={bytes} rebuild_us={} reopen_median_us={} reopen_max_us={} valid_reopen_tokenizations=0",
        rebuild.as_micros(),
        times[5],
        times[9]
    );
}
