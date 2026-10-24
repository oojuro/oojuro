use super::*;
use crate::capture_log::Identity;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
const KEY: [u8; 32] = [109; 32];
const WRITER: &str = "disk-index-synthetic";
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
                "disk-index-{}-{}",
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
    fn reader(&self) -> Reader {
        Reader::open_disk(
            &self.root.join("authority"),
            &KEY,
            identity(),
            &self.root.join("directory"),
            None,
        )
        .unwrap()
    }
    fn index(&self, r: &Reader) -> Index {
        Index::open_disk(r, &self.root.join("index")).unwrap()
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
        adapter: "disk-index-synthetic-v1".into(),
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
        agent_id: "reported-index-agent".into(),
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
        k: 30,
        filter: Filter::default(),
        max_bytes: 1 << 20,
    }
}
fn signature(search: Search) -> Vec<(u64, u64, bool, u64, Vec<u8>)> {
    search
        .hits
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
#[test]
fn actual_disk_reference_tree_reopens_extends_and_merges_without_a_ref_vec() {
    let f = Fixture::new();
    let mut log = f.log();
    let mut stored = row("!!!", Some("stored"), 4);
    stored.indexed = false;
    log.append(
        "initial",
        &[
            row("alpha beta foo_bar", Some("memory"), 1),
            row("alpha duplicate", None, 2),
            row("alpha duplicate", None, 2),
            stored,
            row("foo and bar continuation", None, 3),
        ],
    )
    .unwrap();
    let native = super::super::super::super::Index::rebuild(&log).unwrap();
    let texts = [
        "",
        "alpha",
        "foo_bar",
        "\"alpha beta\"",
        "key:memory",
        "key:stored",
        "\"!!!\"",
    ];
    let expected: Vec<_> = texts
        .iter()
        .map(|t| signature(native.search(&log, &query(t, AsOf::default())).unwrap()))
        .collect();
    drop(native);
    drop(log);
    let reader = f.reader();
    let mut index = f.index(&reader);
    assert_eq!(index.retained_reference_count(), 0);
    assert_eq!(reader.locator_count(), 0);
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(
            signature(index.search(&reader, &query(t, AsOf::default())).unwrap()),
            expected[i]
        );
    }
    let old = reader.view(scope(), AsOf::default()).unwrap();
    drop(reader);
    let mut log = f.log();
    log.append(
        "tail",
        &[row("memory alpha beta newer", Some("memory"), 10)],
    )
    .unwrap();
    drop(log);
    let reader = f.reader();
    assert!(
        index
            .search(&reader, &query("alpha", AsOf::default()))
            .is_err()
    );
    index.refresh(&reader).unwrap();
    assert_eq!(index.segment_count(), 2);
    assert_eq!(index.retained_reference_count(), 0);
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(
            signature(
                index
                    .search_view(&reader, &old, &query(t, AsOf::default()))
                    .unwrap()
            ),
            expected[i]
        );
    }
    index.merge(&reader).unwrap();
    assert_eq!(index.segment_count(), 1);
    assert_eq!(index.retained_reference_count(), 0);
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(
            signature(
                index
                    .search_view(&reader, &old, &query(t, AsOf::default()))
                    .unwrap()
            ),
            expected[i]
        );
    }
    drop(index);
    let n = lexical::analysis_count();
    let index = f.index(&reader);
    assert_eq!(index.opened(), Opened::Loaded);
    assert_eq!(n, lexical::analysis_count());
    assert_eq!(index.retained_reference_count(), 0);
    assert!(reader.metadata_cache_accounted_bytes().unwrap() <= 4 << 20);
}
#[test]
fn reference_tree_corruption_missing_descriptor_and_namespace_misuse_cannot_make_hits() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("one", &[row("alpha original", None, 1)])
        .unwrap();
    drop(log);
    let reader = f.reader();
    drop(f.index(&reader));
    let d = load_descriptor(&reader, &f.root.join("index")).unwrap();
    let r = d.refs.node.as_ref().unwrap();
    let pack =
        f.root
            .join("index")
            .join(format!("pack-{}-{:016x}.bin", hex(&r.generation), r.file));
    let mut bytes = fs::read(&pack).unwrap();
    bytes[r.offset as usize + 9] ^= 1;
    fs::write(pack, bytes).unwrap();
    drop(reader);
    let reader = f.reader();
    let index = f.index(&reader);
    assert_eq!(index.opened(), Opened::Rebuilt);
    assert_eq!(
        index
            .search(&reader, &query("alpha", AsOf::default()))
            .unwrap()
            .hits[0]
            .record
            .id,
        9
    );
    drop(index);
    fs::remove_file(f.root.join("index").join(DISK_MANIFEST)).unwrap();
    assert_eq!(f.index(&reader).opened(), Opened::Rebuilt);
    assert!(Index::open(&reader, &f.root.join("index")).is_err());
    assert!(Index::open_disk(&reader, &f.root.join("authority")).is_err());
}
#[test]
fn streamed_file_inventory_retires_orphans_and_all_disk_content_is_encrypted() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append(
        "private",
        &[row(
            "private_disk_reference alpha",
            Some("private-key-name"),
            1,
        )],
    )
    .unwrap();
    drop(log);
    let reader = f.reader();
    let mut index = f.index(&reader);
    let orphan = f
        .root
        .join("index")
        .join(format!("seg-{}.idx", "ab".repeat(32)));
    fs::write(&orphan, b"synthetic orphan").unwrap();
    index.merge(&reader).unwrap();
    assert!(!orphan.exists());
    for item in fs::read_dir(f.root.join("index")).unwrap() {
        let bytes = fs::read(item.unwrap().path()).unwrap();
        for needle in [
            b"private_disk_reference".as_slice(),
            b"private-key-name",
            WRITER.as_bytes(),
        ] {
            assert!(!bytes.windows(needle.len()).any(|b| b == needle));
        }
    }
}
#[test]
#[ignore]
fn disk_index_crash_child() {
    let root = PathBuf::from(std::env::var("OOJURO_DISK_INDEX_CRASH_ROOT").unwrap());
    let phase = std::env::var("OOJURO_DISK_INDEX_CRASH_PHASE").unwrap();
    let reader = Reader::open_disk(
        &root.join("authority"),
        &KEY,
        identity(),
        &root.join("directory"),
        None,
    )
    .unwrap();
    let mut index = Index::open_disk(&reader, &root.join("index")).unwrap();
    drop(reader);
    let mut log = RecordLog::open_clock(
        &root.join("authority"),
        &KEY,
        identity(),
        Arc::new(|| Ok(100)),
    )
    .unwrap();
    log.append("child", &[row("alpha child", None, 2)]).unwrap();
    drop(log);
    let reader = Reader::open_disk(
        &root.join("authority"),
        &KEY,
        identity(),
        &root.join("directory"),
        None,
    )
    .unwrap();
    index.hook = Some(Box::new(move |at| {
        if format!("{at:?}") == phase {
            println!("DISK_INDEX_BOUNDARY_READY");
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        }
    }));
    index.refresh(&reader).unwrap();
    panic!("phase not reached");
}
#[test]
fn disk_index_sigkill_data_page_tree_and_descriptor_boundaries_preserve_original_receipt() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    for phase in [
        "SegmentTemp",
        "SegmentPublished",
        "DirectoryPagesDurable",
        "ManifestTemp",
        "ManifestPublished",
        "ManifestDurable",
    ] {
        let f = Fixture::new();
        let mut log = f.log();
        log.append("parent", &[row("alpha parent", None, 1)])
            .unwrap();
        drop(log);
        let reader = f.reader();
        drop(f.index(&reader));
        drop(reader);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "record::paged::cache::disk::tests::disk_index_crash_child",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env("OOJURO_DISK_INDEX_CRASH_ROOT", &f.root)
            .env("OOJURO_DISK_INDEX_CRASH_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let listen = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if line.unwrap().contains("DISK_INDEX_BOUNDARY_READY") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        let reached = rx.recv_timeout(Duration::from_secs(8));
        let _ = child.kill();
        let status = child.wait().unwrap();
        listen.join().unwrap();
        assert!(reached.is_ok(), "{phase}");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let reader = f.reader();
        let receipt = reader.receipt("child").unwrap().unwrap();
        assert_eq!(receipt.ids, vec![10]);
        let index = f.index(&reader);
        assert_eq!(index.retained_reference_count(), 0);
        assert_eq!(
            index
                .search(&reader, &query("alpha", AsOf::default()))
                .unwrap()
                .hits
                .len(),
            2
        );
    }
}
