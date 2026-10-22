use super::*;
use crate::capture_log::Identity;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
const KEY: [u8; 32] = [66; 32];
const WRITER: &str = "record-cache-synthetic-owner";
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
                "record-cache-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn open(&self) -> RecordLog {
        RecordLog::open_clock(
            &self.root.join("authority"),
            &KEY,
            identity(),
            Arc::new(|| Ok(100)),
        )
        .unwrap()
    }
    fn cache(&self, log: &RecordLog) -> PersistentIndex {
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
        adapter: "record-cache-synthetic-v1".into(),
    }
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 1,
    }
}
fn input(text: &str, key: Option<&str>, at: i64) -> Input {
    Input {
        agent_id: "reported-agent".into(),
        parent_id: Some("reported-parent".into()),
        valid_at: at,
        kind: Kind::Explicit,
        tool: Some("reported-tool".into()),
        status: Some(Status::Ok),
        exit: None,
        key: key.map(str::to_owned),
        text: text.into(),
        attrs: BTreeMap::from([(
            "stored_only_attr".into(),
            serde_json::json!("untrusted metadata"),
        )]),
        indexed: true,
    }
}
fn append(log: &mut RecordLog, request: &str, text: &str) {
    log.append(request, &[input(text, None, log.records.len() as i64)])
        .unwrap();
}
fn query(text: &str, as_of: AsOf) -> Query<'_> {
    Query {
        scope: scope(),
        as_of,
        text,
        k: 100,
        filter: Filter::default(),
        max_bytes: 1 << 20,
    }
}
fn signature(s: Search) -> Vec<(u64, u64, bool, u64, Vec<u8>)> {
    s.hits
        .into_iter()
        .map(|h| {
            (
                h.record.id,
                h.bits.to_bits(),
                h.continued,
                h.row,
                serde_json::to_vec(&h.record).unwrap(),
            )
        })
        .collect()
}
fn parity(cache: &PersistentIndex, log: &RecordLog) {
    let fresh = Index::rebuild(log).unwrap();
    for cutoff in [
        AsOf::default(),
        AsOf {
            from: i64::MIN,
            valid_at: 20,
            known_at: 102,
        },
        AsOf {
            from: 11,
            valid_at: 30,
            known_at: i64::MAX,
        },
    ] {
        for text in [
            "",
            "alpha",
            "foo_bar",
            "foo",
            "\"alpha beta\"",
            "key:memory",
            "\"!!!\"",
            "日本語",
            "<missing>",
        ] {
            let q = query(text, cutoff);
            assert_eq!(
                signature(cache.search(log, &q).unwrap()),
                signature(fresh.search(log, &q).unwrap()),
                "{text}"
            );
        }
    }
}
fn manifest(log: &RecordLog, dir: &Path) -> Manifest {
    let tip = log.tip().unwrap();
    serde_json::from_slice(
        &log.unseal(
            &manifest_aad(tip.origin, tip.identity),
            &fs::read(dir.join(MANIFEST)).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn write_manifest(log: &RecordLog, dir: &Path, m: &Manifest) {
    fs::write(
        dir.join(MANIFEST),
        log.seal(
            &manifest_aad(m.origin, m.identity),
            &serde_json::to_vec(m).unwrap(),
        ),
    )
    .unwrap();
}
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        assert!(e.file_type().unwrap().is_file());
        fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

#[test]
fn reopen_tail_merge_preserves_versions_original_ids_and_score_bits() {
    let f = Fixture::new();
    let mut log = f.open();
    let mut stored = input("!!!", Some("stored"), 15);
    stored.indexed = false;
    log.append(
        "initial",
        &[
            input("alpha beta foo_bar", Some("memory"), 10),
            input("alpha beta duplicate", None, 11),
            input("alpha beta duplicate", None, 11),
            stored,
            input("日本語 alpha", None, 12),
        ],
    )
    .unwrap();
    let mut cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &log);
    let ids: Vec<_> = cache
        .search(&log, &query("\"alpha beta duplicate\"", AsOf::default()))
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.record.id)
        .collect();
    assert!(ids.contains(&10) && ids.contains(&11));
    assert!(
        cache
            .search(&log, &query("\"!!!\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
    drop(cache);
    drop(log);
    log = f.open();
    let n = lexical::analysis_count();
    cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(n, lexical::analysis_count());
    parity(&cache, &log);
    let old_view = View::open(&log, scope(), AsOf::default()).unwrap();
    log.append(
        "later",
        &[
            input("alpha replacement", Some("memory"), 20),
            input("alpha backdated", Some("memory"), 5),
        ],
    )
    .unwrap();
    assert!(matches!(
        cache.search(&log, &query("alpha", AsOf::default())),
        Err(Error::Unavailable)
    ));
    let n = lexical::analysis_count();
    cache.refresh(&log).unwrap();
    assert_eq!(lexical::analysis_count() - n, 2);
    assert_eq!(cache.segment_count(), 2);
    parity(&cache, &log);
    let old_q = query("\"alpha beta foo_bar\"", AsOf::default());
    assert_eq!(
        cache.search_view(&log, &old_view, &old_q).unwrap().hits[0]
            .record
            .id,
        9
    );
    let before = signature(
        cache
            .search(&log, &query("alpha", AsOf::default()))
            .unwrap(),
    );
    cache.merge(&log).unwrap();
    assert_eq!(cache.segment_count(), 1);
    assert_eq!(
        before,
        signature(
            cache
                .search(&log, &query("alpha", AsOf::default()))
                .unwrap()
        )
    );
    drop(cache);
    let n = lexical::analysis_count();
    cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(n, lexical::analysis_count());
    parity(&cache, &log);
}
#[test]
fn stale_reopen_tokenizes_only_committed_tail_and_exact_retry_adds_no_segment() {
    let f = Fixture::new();
    let mut log = f.open();
    append(&mut log, "one", "alpha first");
    let ack = log
        .append("same", &[input("alpha duplicate", None, 2)])
        .unwrap();
    drop(f.cache(&log));
    append(&mut log, "new", "alpha tail");
    let n = lexical::analysis_count();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Extended);
    assert_eq!(lexical::analysis_count() - n, 1);
    let segments = cache.segment_count();
    drop(cache);
    assert_eq!(
        ack,
        log.append("same", &[input("alpha duplicate", None, 2)])
            .unwrap()
    );
    let n = lexical::analysis_count();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(cache.segment_count(), segments);
    assert_eq!(n, lexical::analysis_count());
    parity(&cache, &log);
}
#[test]
fn absent_corrupt_ahead_malformed_and_foreign_cache_rebuild_verified_log() {
    let f = Fixture::new();
    let mut log = f.open();
    append(&mut log, "one", "alpha original");
    drop(f.cache(&log));
    let dir = f.root.join("cache");
    let original = manifest(&log, &dir);
    let seg = dir.join(format!("seg-{}.idx", original.segments[0].id));
    let bytes = fs::read(&seg).unwrap();
    let mut damaged = bytes.clone();
    let i = damaged.len() / 2;
    damaged[i] ^= 1;
    fs::write(&seg, damaged).unwrap();
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &log);
    drop(cache);
    fs::remove_file(dir.join(MANIFEST)).unwrap();
    assert_eq!(f.cache(&log).opened(), Opened::Rebuilt);
    let mut m = manifest(&log, &dir);
    m.prefix.sequence += 1;
    write_manifest(&log, &dir, &m);
    assert_eq!(f.cache(&log).opened(), Opened::Rebuilt);
    let mut m = manifest(&log, &dir);
    m.rows = MAX_RECORDS + 1;
    write_manifest(&log, &dir, &m);
    assert_eq!(f.cache(&log).opened(), Opened::Rebuilt);
    let mut m = manifest(&log, &dir);
    m.segments[0].rows = u32::MAX;
    write_manifest(&log, &dir, &m);
    assert_eq!(f.cache(&log).opened(), Opened::Rebuilt);
    let g = Fixture::new();
    let mut foreign = g.open();
    append(&mut foreign, "other", "beta foreign");
    let cache = f.cache(&foreign);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert!(
        cache
            .search(&foreign, &query("\"alpha original\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
    parity(&cache, &foreign);
}
#[test]
fn copied_origin_equal_highwater_fork_and_truncation_never_serve_old_postings() {
    let f = Fixture::new();
    let mut a = f.open();
    append(&mut a, "common", "alpha common");
    drop(f.cache(&a));
    drop(a);
    copy_dir(&f.root.join("authority"), &f.root.join("fork"));
    copy_dir(&f.root.join("authority"), &f.root.join("old"));
    a = f.open();
    append(&mut a, "a", "alpha only branch_a");
    let mut cache = f.cache(&a);
    let am = a.tip().unwrap();
    drop(a);
    let mut b = RecordLog::open_clock(&f.root.join("fork"), &KEY, identity(), Arc::new(|| Ok(100)))
        .unwrap();
    append(&mut b, "b", "beta only branch_b");
    let bm = b.tip().unwrap();
    assert_eq!(am.origin, bm.origin);
    assert_eq!(am.records, bm.records);
    assert_eq!(am.sequence, bm.sequence);
    assert!(b.validate(&am).is_err());
    assert!(cache.search(&b, &query("alpha", AsOf::default())).is_err());
    assert!(cache.refresh(&b).is_err());
    assert!(cache.search(&b, &query("beta", AsOf::default())).is_err());
    drop(cache);
    let cache = f.cache(&b);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert!(
        cache
            .search(&b, &query("\"alpha only branch_a\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
    parity(&cache, &b);
    drop(cache);
    drop(b);
    let old = RecordLog::open(&f.root.join("old"), &KEY, identity()).unwrap();
    assert!(old.validate(&am).is_err());
    let cache = f.cache(&old);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    parity(&cache, &old);
}
#[test]
fn cache_lock_symlink_plaintext_scan_and_authority_directory_refusal() {
    let f = Fixture::new();
    let mut log = f.open();
    append(&mut log, "request", "uniqueterm_private_record_cache alpha");
    let cache = f.cache(&log);
    assert!(PersistentIndex::open(&log, &f.root.join("cache")).is_err());
    assert!(PersistentIndex::open(&log, &f.root.join("authority")).is_err());
    for e in fs::read_dir(f.root.join("cache")).unwrap() {
        let e = e.unwrap();
        let bytes = fs::read(e.path()).unwrap();
        for needle in [
            b"uniqueterm_private_record_cache".as_slice(),
            WRITER.as_bytes(),
            b"reported-agent",
            b"untrusted metadata",
        ] {
            assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        }
    }
    drop(cache);
    std::os::unix::fs::symlink(f.root.join("authority"), f.root.join("link")).unwrap();
    assert!(PersistentIndex::open(&log, &f.root.join("link")).is_err());
    let g = Fixture::new();
    let wrong = RecordLog::open(&g.root.join("authority"), &[67; 32], identity()).unwrap();
    assert!(wrong.validate(&log.tip().unwrap()).is_err());
}
#[test]
fn cache_failure_cannot_change_authority_ack_and_poisoned_handle_refuses() {
    let f = Fixture::new();
    let mut log = f.open();
    append(&mut log, "one", "alpha first");
    let mut cache = f.cache(&log);
    let row = input("alpha already committed", None, 2);
    let ack = log.append("two", std::slice::from_ref(&row)).unwrap();
    fs::rename(f.root.join("cache"), f.root.join("displaced")).unwrap();
    assert!(cache.refresh(&log).is_err());
    assert!(
        cache
            .search(&log, &query("alpha", AsOf::default()))
            .is_err()
    );
    drop(cache);
    drop(log);
    let mut log = f.open();
    assert_eq!(ack, log.append("two", &[row]).unwrap());
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert_eq!(
        cache
            .search(&log, &query("alpha", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        2
    );
}
#[test]
fn multisegment_record_row_boundaries_preserve_every_duplicate_logical_id() {
    let f = Fixture::new();
    let mut log = f.open();
    for b in 0..9 {
        let count = if b == 8 { 300 } else { 1000 };
        let rows = (0..count)
            .map(|i| input("alpha duplicate same text", None, b * 1000 + i))
            .collect::<Vec<_>>();
        log.append(&format!("batch-{b}"), &rows).unwrap();
    }
    let mut cache = f.cache(&log);
    assert_eq!(cache.segment_count(), 2);
    let before = signature(
        cache
            .search(&log, &query("alpha", AsOf::default()))
            .unwrap(),
    );
    drop(cache);
    let n = lexical::analysis_count();
    cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(n, lexical::analysis_count());
    assert_eq!(
        before,
        signature(
            cache
                .search(&log, &query("alpha", AsOf::default()))
                .unwrap()
        )
    );
    cache.merge(&log).unwrap();
    assert_eq!(
        before,
        signature(
            cache
                .search(&log, &query("alpha", AsOf::default()))
                .unwrap()
        )
    );
}
#[test]
fn authenticated_malformed_posting_lengths_refuse_before_hydrating_ghosts() {
    let f = Fixture::new();
    let mut log = f.open();
    append(&mut log, "one", "alpha original");
    drop(f.cache(&log));
    let dir = f.root.join("cache");
    let mut m = manifest(&log, &dir);
    let r = &mut m.segments[0];
    r.decoded = usize::MAX;
    write_manifest(&log, &dir, &m);
    assert_eq!(f.cache(&log).opened(), Opened::Rebuilt);
    let mut m = manifest(&log, &dir);
    let mut segment = codec::decode(
        &log.unseal(
            &segment_aad(&m, &m.segments[0]).unwrap(),
            &fs::read(dir.join(format!("seg-{}.idx", m.segments[0].id))).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    segment.rows[0].id += 1;
    segment.rows[0].fingerprint = projection::fingerprint(segment.rows[0].id);
    let bytes = codec::encode(&segment).unwrap();
    let r = &mut m.segments[0];
    r.bytes = bytes.len();
    r.decoded = projection::segment_bytes(&segment).unwrap();
    let aad = segment_aad(&m, &m.segments[0]).unwrap();
    let sealed = log.seal(&aad, &bytes);
    m.segments[0].digest = *blake3::hash(&sealed).as_bytes();
    fs::write(dir.join(format!("seg-{}.idx", m.segments[0].id)), sealed).unwrap();
    write_manifest(&log, &dir, &m);
    let cache = f.cache(&log);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert_eq!(
        cache
            .search(&log, &query("alpha", AsOf::default()))
            .unwrap()
            .hits[0]
            .record
            .id,
        9
    );
}
// Spawned by the driver; never run as an ordinary ignored test.
#[test]
#[ignore]
fn record_crash_publication_child() {
    let root = PathBuf::from(std::env::var("OOJURO_RECORD_CACHE_CRASH_ROOT").unwrap());
    let phase = std::env::var("OOJURO_RECORD_CACHE_CRASH_PHASE").unwrap();
    let mut log = RecordLog::open_clock(
        &root.join("authority"),
        &KEY,
        identity(),
        Arc::new(|| Ok(100)),
    )
    .unwrap();
    let mut cache = PersistentIndex::open(&log, &root.join("cache")).unwrap();
    append(&mut log, "child", "alpha child committed");
    cache.hook = Some(Box::new(move |at| {
        if format!("{at:?}") == phase {
            println!("RECORD_CACHE_BOUNDARY_READY");
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
fn sigkill_record_segments_and_manifest_preserve_authority_retry_and_visibility() {
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
        append(&mut log, "parent", "alpha parent committed");
        drop(f.cache(&log));
        drop(log);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "record::persistent::tests::record_crash_publication_child",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env("OOJURO_RECORD_CACHE_CRASH_ROOT", &f.root)
            .env("OOJURO_RECORD_CACHE_CRASH_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if line.unwrap().contains("RECORD_CACHE_BOUNDARY_READY") {
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
        let mut log = f.open();
        let original = log
            .append("child", &[input("alpha child committed", None, 1)])
            .unwrap();
        assert_eq!(original.ids, vec![10]);
        let cache = f.cache(&log);
        assert_eq!(
            cache
                .search(&log, &query("alpha", AsOf::default()))
                .unwrap()
                .hits
                .len(),
            2,
            "phase={phase}"
        );
        parity(&cache, &log);
    }
}
