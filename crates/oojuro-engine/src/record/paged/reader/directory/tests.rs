use super::*;
use crate::record::{Index as MemoryIndex, paged::Index as PagedIndex};
use std::sync::atomic::{AtomicU64, Ordering};
const KEY: [u8; 32] = [101; 32];
const WRITER: &str = "disk-metadata-synthetic";
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
                "disk-directory-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn log(&self) -> RecordLog {
        RecordLog::open_clock(
            &self.root.join("authority"),
            &KEY,
            identity(),
            Arc::new(|| Ok(100)),
        )
        .unwrap()
    }
    fn reader(&self, min: Option<&Watermark>) -> Reader {
        Reader::open_disk(
            &self.root.join("authority"),
            &KEY,
            identity(),
            &self.root.join("directory"),
            min,
        )
        .unwrap()
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
        adapter: "disk-directory-synthetic-v1".into(),
    }
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 1,
    }
}
fn row(text: &str, key: Option<&str>, at: i64) -> Input {
    Input {
        agent_id: "reported-disk-agent".into(),
        parent_id: None,
        valid_at: at,
        kind: Kind::Explicit,
        tool: None,
        status: None,
        exit: None,
        key: key.map(str::to_owned),
        text: text.into(),
        attrs: BTreeMap::new(),
        indexed: true,
    }
}
fn query(text: &str, as_of: AsOf) -> Query<'_> {
    Query {
        scope: scope(),
        as_of,
        text,
        k: 20,
        filter: Filter::default(),
        max_bytes: 1 << 20,
    }
}
fn signature(s: Search) -> Vec<(u64, u64, bool, u64)> {
    s.hits
        .into_iter()
        .map(|h| (h.record.id, h.bits.to_bits(), h.continued, h.row))
        .collect()
}
fn packed(path: &Path) -> usize {
    fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_str().unwrap().starts_with("pack-"))
        .count()
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}
#[test]
fn actual_disk_reader_uses_no_locator_population_and_exact_original_receipts_keys_and_query() {
    let f = Fixture::new();
    let mut log = f.log();
    let mut first = None;
    for i in 0..140 {
        let ack = log
            .append(
                &format!("r-{i}"),
                &[row("alpha beta foo_bar", Some(&format!("key-{i}")), i)],
            )
            .unwrap();
        if i == 0 {
            first = Some(ack);
        }
    }
    let ack = first.unwrap();
    let min = log.tip().unwrap();
    let native = MemoryIndex::rebuild(&log).unwrap();
    let expected = signature(
        native
            .search(&log, &query("alpha", AsOf::default()))
            .unwrap(),
    );
    drop(native);
    drop(log);
    let reader = f.reader(Some(&min));
    assert!(reader.uses_disk_directory());
    assert_eq!(reader.locator_count(), 0);
    assert_eq!(reader.refs.capacity(), 0);
    assert_eq!(reader.receipt("r-0").unwrap(), Some(ack));
    let s = reader.view(scope(), AsOf::default()).unwrap();
    assert_eq!(
        reader.get_key(&s, "key-139", 10000).unwrap().unwrap().id,
        148
    );
    let index = PagedIndex::open(&reader, &f.root.join("index")).unwrap();
    assert_eq!(
        signature(
            index
                .search(&reader, &query("alpha", AsOf::default()))
                .unwrap()
        ),
        expected
    );
    drop(index);
    drop(reader);
    let reader = f.reader(Some(&min));
    assert_eq!(reader.locator_count(), 0);
    assert_eq!(
        reader
            .get_key(
                &reader.view(scope(), AsOf::default()).unwrap(),
                "key-0",
                10000
            )
            .unwrap()
            .unwrap()
            .id,
        9
    );
}
#[test]
fn disk_cow_extend_preserves_birth_proofs_snapshot_winners_and_compacts_old_packs() {
    let f = Fixture::new();
    let mut log = f.log();
    for i in 0..140 {
        log.append(&format!("r-{i}"), &[row("alpha old", None, i)])
            .unwrap();
    }
    let original = log.tip().unwrap();
    drop(log);
    let reader = f.reader(None);
    let old = reader.view(scope(), AsOf::default()).unwrap();
    let initial = packed(&f.root.join("directory"));
    drop(reader);
    let mut log = f.log();
    log.append(
        "tail",
        &[
            row("alpha new", Some("memory"), 200),
            row("alpha backdated", Some("memory"), 100),
        ],
    )
    .unwrap();
    drop(log);
    let mut reader = f.reader(Some(&original));
    assert_eq!(reader.locator_count(), 0);
    assert_eq!(
        reader
            .get_key(
                &reader.view(scope(), AsOf::default()).unwrap(),
                "memory",
                10000
            )
            .unwrap()
            .unwrap()
            .id,
        149
    );
    assert!(reader.get_key(&old, "memory", 10000).unwrap().is_none());
    assert!(!reader.births.borrow().is_empty());
    assert!(packed(&f.root.join("directory")) <= initial + 4);
    let before = reader.receipt("tail").unwrap();
    reader.compact_directory().unwrap();
    assert_eq!(reader.receipt("tail").unwrap(), before);
    assert!(packed(&f.root.join("directory")) <= 4);
    assert_eq!(reader.locator_count(), 0);
}
#[test]
fn no_key_tail_advances_empty_key_root_and_old_snapshot_never_sees_future_version() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("one", &[row("alpha", None, 1)]).unwrap();
    drop(log);
    drop(f.reader(None));
    let mut log = f.log();
    log.append("two", &[row("beta", None, 2)]).unwrap();
    drop(log);
    let reader = f.reader(None);
    let s = reader.view(scope(), AsOf::default()).unwrap();
    assert!(reader.get_key(&s, "absent", 10000).unwrap().is_none());
}
#[test]
fn disk_directory_loss_corruption_semantic_forgery_fork_and_known_floor_cannot_supply_authority() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("common", &[row("alpha common", None, 1)])
        .unwrap();
    drop(log);
    copy(&f.root.join("authority"), &f.root.join("fork"));
    let mut log = f.log();
    let min = log
        .append("a", &[row("alpha branch_a", None, 2)])
        .unwrap()
        .watermark;
    drop(log);
    let reader = f.reader(None);
    let path = f.root.join("directory");
    let mut roots = load_roots(&reader, &path).unwrap();
    let mut false_rows = Builder::new(reader.tree_context(&path, Purpose::Record)).unwrap();
    false_rows
        .push(Entry::new(9u64.to_be_bytes().to_vec(), slot(2, 0).unwrap()).unwrap())
        .unwrap();
    false_rows
        .push(Entry::new(10u64.to_be_bytes().to_vec(), slot(1, 0).unwrap()).unwrap())
        .unwrap();
    roots.record = false_rows.finish().unwrap();
    publish(&reader, &path, &roots).unwrap();
    drop(reader);
    let reader = f.reader(Some(&min));
    assert_eq!(
        reader
            .get_id(&reader.view(scope(), AsOf::default()).unwrap(), 9, 10000)
            .unwrap()
            .unwrap()
            .text,
        "alpha common"
    );
    drop(reader);
    let mut bytes = fs::read(path.join(ROOT)).unwrap();
    let i = bytes.len() / 2;
    bytes[i] ^= 1;
    fs::write(path.join(ROOT), bytes).unwrap();
    drop(f.reader(Some(&min)));
    fs::remove_file(path.join(ROOT)).unwrap();
    drop(f.reader(Some(&min)));
    let mut fork =
        RecordLog::open_clock(&f.root.join("fork"), &KEY, identity(), Arc::new(|| Ok(100)))
            .unwrap();
    fork.append("b", &[row("beta branch_b", None, 2)]).unwrap();
    drop(fork);
    assert!(Reader::open_disk(&f.root.join("fork"), &KEY, identity(), &path, Some(&min)).is_err());
    let reader = Reader::open_disk(&f.root.join("fork"), &KEY, identity(), &path, None).unwrap();
    assert_eq!(
        reader
            .get_id(&reader.view(scope(), AsOf::default()).unwrap(), 10, 10000)
            .unwrap()
            .unwrap()
            .text,
        "beta branch_b"
    );
}
#[test]
fn source_duplicate_receipt_is_rejected_without_a_request_set_or_index_fallback() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("same", &[row("alpha", None, 1)]).unwrap();
    drop(log);
    drop(f.reader(None));
    let mut reader = Reader::open(&f.root.join("authority"), &KEY, identity(), None).unwrap();
    let assigned = vec![Assigned {
        id: 10,
        recorded_at: 101,
        input: row("beta", None, 2),
    }];
    let request = reader.tip.request;
    let inputs: Vec<_> = assigned.iter().map(|r| &r.input).collect();
    let fingerprint = reader.tag("typed-input-v1", &inputs).unwrap();
    let canonical = json(
        &Unsigned {
            schema: 2,
            request,
            fingerprint,
            assigned: &assigned,
        },
        MAX_BATCH_BYTES,
    )
    .unwrap();
    let digest = authority::chain(reader.tip.digest, &canonical);
    let mut w = Watermark {
        schema: 1,
        origin: reader.origin,
        identity: reader.identity_tag,
        sequence: 2,
        records: 2,
        commitment: vec![],
    };
    w.commitment = reader
        .cipher
        .seal_frame(&authority::aad(&w).unwrap(), &digest);
    let b = Batch {
        schema: 2,
        request,
        fingerprint,
        assigned,
        watermark: w,
    };
    let bytes = json(&b, MAX_BATCH_BYTES).unwrap();
    reader
        .store
        .append(&[oojuro_store::NewRecord {
            kind: 7,
            valid_at: 0,
            recorded_at: 0,
            key: b"",
            attrs: b"",
            text: &bytes,
        }])
        .unwrap();
    drop(reader);
    assert!(
        Reader::open_disk(
            &f.root.join("authority"),
            &KEY,
            identity(),
            &f.root.join("directory"),
            None
        )
        .is_err()
    );
}
#[test]
fn directory_scope_lock_symlinks_and_plaintext_are_refused() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append(
        "private-request",
        &[row(
            "private_disk_directory_content",
            Some("private-logical-key"),
            1,
        )],
    )
    .unwrap();
    drop(log);
    let reader = f.reader(None);
    assert!(
        Reader::open_disk(
            &f.root.join("authority"),
            &KEY,
            identity(),
            &f.root.join("authority"),
            None
        )
        .is_err()
    );
    for e in fs::read_dir(f.root.join("directory")).unwrap() {
        let bytes = fs::read(e.unwrap().path()).unwrap();
        for needle in [
            b"private_disk_directory_content".as_slice(),
            b"private-logical-key",
            b"private-request",
            WRITER.as_bytes(),
        ] {
            assert!(!bytes.windows(needle.len()).any(|b| b == needle));
        }
    }
    drop(reader);
    std::os::unix::fs::symlink(f.root.join("directory"), f.root.join("link")).unwrap();
    assert!(
        Reader::open_disk(
            &f.root.join("authority"),
            &KEY,
            identity(),
            &f.root.join("link"),
            None
        )
        .is_err()
    );
}
