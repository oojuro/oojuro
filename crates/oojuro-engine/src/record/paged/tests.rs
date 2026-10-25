use super::*;
use crate::capture_log::Identity;
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const KEY: [u8; 32] = [79; 32];
const WRITER: &str = "paged-native-synthetic-writer";
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
                "paged-{}-{}",
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
    fn reader(&self, minimum: Option<&Watermark>) -> Reader {
        Reader::open(&self.root.join("authority"), &KEY, identity(), minimum).unwrap()
    }
    fn index(&self, r: &Reader) -> Index {
        Index::open(r, &self.root.join("cache")).unwrap()
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
        adapter: "paged-synthetic-v1".into(),
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
        agent_id: "reported-paged-agent".into(),
        parent_id: Some("reported-parent".into()),
        valid_at: at,
        kind: Kind::Explicit,
        tool: None,
        status: Some(Status::Ok),
        exit: None,
        key: key.map(str::to_owned),
        text: text.into(),
        attrs: BTreeMap::from([("untrusted".into(), serde_json::json!("stored_metadata"))]),
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
        max_bytes: 2 << 20,
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
fn copy(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        assert!(e.file_type().unwrap().is_file());
        fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}
#[test]
fn streamed_reader_exact_receipts_original_reads_and_sealed_pages() {
    let f = Fixture::new();
    let mut log = f.log();
    let ack = log
        .append(
            "first",
            &[
                row("alpha old", Some("memory"), 10),
                row("alpha original duplicate", None, 11),
                row("alpha original duplicate", None, 11),
            ],
        )
        .unwrap();
    log.append(
        "second",
        &[
            row("alpha latest", Some("memory"), 20),
            row("alpha backdated", Some("memory"), 5),
        ],
    )
    .unwrap();
    let old = View::open(
        &log,
        scope(),
        AsOf {
            from: i64::MIN,
            valid_at: 20,
            known_at: 102,
        },
    )
    .unwrap();
    let expected = old.get_key(&log, "memory", 1 << 20).unwrap().unwrap();
    let min = log.tip().unwrap();
    drop(log);
    let reader = f.reader(Some(&min));
    assert_eq!(reader.locator_count(), 3);
    assert_eq!(reader.receipt("first").unwrap(), Some(ack));
    assert_eq!(reader.receipt("missing").unwrap(), None);
    let s = reader
        .view(
            scope(),
            AsOf {
                from: i64::MIN,
                valid_at: 20,
                known_at: 102,
            },
        )
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&reader.get_key(&s, "memory", 1 << 20).unwrap().unwrap()).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    let history = reader
        .history(
            &s,
            "memory",
            Limits {
                entries: 1,
                bytes: 10000,
            },
        )
        .unwrap();
    assert_eq!(history.records[0].id, 9);
    assert!(history.next.is_none());
    let s = reader.view(scope(), AsOf::default()).unwrap();
    let mut page = reader
        .range(
            &s,
            Versions::All,
            Filter::default(),
            Limits {
                entries: 2,
                bytes: 10000,
            },
        )
        .unwrap();
    let mut ids = vec![];
    let cursor = page.next.clone().unwrap();
    assert!(!cursor.contains("memory"));
    loop {
        ids.extend(page.records.iter().map(|r| r.id));
        let Some(next) = page.next else {
            break;
        };
        page = reader
            .continue_page(
                scope(),
                &next,
                Limits {
                    entries: 2,
                    bytes: 10000,
                },
            )
            .unwrap();
    }
    assert_eq!(ids, vec![13, 9, 10, 11, 12]);
    assert!(
        reader
            .continue_page(
                Scope {
                    writer: "foreign",
                    owner: 1
                },
                &cursor,
                Limits::default()
            )
            .is_err()
    );
    let mut bad = cursor.clone();
    bad.replace_range(4..6, "ff");
    assert!(
        reader
            .continue_page(scope(), &bad, Limits::default())
            .is_err()
    );
    assert!(reader.get_id(&s, 999, 3).is_err());
    assert!(
        reader
            .range(
                &s,
                Versions::All,
                Filter::default(),
                Limits {
                    entries: 2,
                    bytes: 4
                }
            )
            .is_err()
    );
}
#[test]
fn paged_query_matches_native_bits_visibility_literals_identifiers_and_key_winners() {
    let f = Fixture::new();
    let mut log = f.log();
    let mut stored = row("!!! alpha stored_only", Some("stored"), 12);
    stored.indexed = false;
    log.append(
        "initial",
        &[
            row("alpha beta foo_bar", Some("memory"), 10),
            row("alpha beta duplicate", None, 11),
            row("alpha beta duplicate", None, 11),
            stored,
            row("日本語 alpha", None, 12),
        ],
    )
    .unwrap();
    log.append(
        "later",
        &[
            row("memory alpha replacement", Some("memory"), 20),
            row("alpha backdated", Some("memory"), 5),
            row("foo and bar continuation", None, 30),
        ],
    )
    .unwrap();
    let memory = super::super::Index::rebuild(&log).unwrap();
    let cutoffs = [
        AsOf::default(),
        AsOf {
            from: i64::MIN,
            valid_at: 20,
            known_at: 104,
        },
        AsOf {
            from: 11,
            valid_at: 30,
            known_at: i64::MAX,
        },
    ];
    let texts = [
        "",
        "alpha",
        "foo_bar",
        "foo",
        "\"alpha beta\"",
        "key:memory",
        "memory",
        "key:stored",
        "\"!!!\"",
        "\"the\"",
        "日本語",
        "<missing>",
    ];
    let expected: Vec<_> = cutoffs
        .iter()
        .flat_map(|a| {
            texts
                .iter()
                .map(|t| signature(memory.search(&log, &query(t, *a)).unwrap()))
        })
        .collect();
    drop(memory);
    drop(log);
    let reader = f.reader(None);
    let index = f.index(&reader);
    assert_eq!(index.opened(), Opened::Rebuilt);
    let mut at = 0;
    for a in cutoffs {
        for text in texts {
            assert_eq!(
                signature(index.search(&reader, &query(text, a)).unwrap()),
                expected[at],
                "query={text} at={at}"
            );
            at += 1;
        }
    }
    assert!(
        index
            .search(&reader, &query("\"!!!\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
}
#[test]
fn paged_above_floor_large_candidates_and_full_decimal_ties_match_exact_scorer() {
    let f = Fixture::new();
    let mut log = f.log();
    for b in 0..2 {
        let rows = (0..600)
            .map(|i| {
                row(
                    &format!("alpha beta background item_{}", b * 600 + i),
                    None,
                    7,
                )
            })
            .collect::<Vec<_>>();
        log.append(&format!("b-{b}"), &rows).unwrap();
    }
    let native = super::super::Index::rebuild(&log).unwrap();
    let mut q = query("alpha", AsOf::default());
    q.k = 100;
    let expected = signature(native.search(&log, &q).unwrap());
    drop(native);
    drop(log);
    let reader = f.reader(None);
    let mut cache = f.index(&reader);
    assert!(cache.segment_count() > 1);
    assert_eq!(signature(cache.search(&reader, &q).unwrap()), expected);
    cache.merge(&reader).unwrap();
    assert_eq!(cache.segment_count(), 1);
    assert_eq!(signature(cache.search(&reader, &q).unwrap()), expected);
    drop(cache);
    let n = crate::lexical::analysis_count();
    let cache = f.index(&reader);
    assert_eq!(cache.opened(), Opened::Loaded);
    assert_eq!(n, crate::lexical::analysis_count());
    assert_eq!(signature(cache.search(&reader, &q).unwrap()), expected);
}
#[test]
fn restart_tail_refresh_original_snapshot_and_cursor_do_not_adopt_future_rows() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("first", &[row("alpha old", None, 10)]).unwrap();
    drop(log);
    let reader = f.reader(None);
    let old = reader.view(scope(), AsOf::default()).unwrap();
    let mut index = f.index(&reader);
    drop(reader);
    let mut log = f.log();
    let ack = log.append("tail", &[row("alpha new", None, 20)]).unwrap();
    drop(log);
    let reader = f.reader(Some(&ack.watermark));
    assert!(
        index
            .search(&reader, &query("alpha", AsOf::default()))
            .is_err()
    );
    let n = crate::lexical::analysis_count();
    index.refresh(&reader).unwrap();
    assert_eq!(crate::lexical::analysis_count() - n, 1);
    assert_eq!(
        index
            .search_view(&reader, &old, &query("alpha", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(
        index
            .search(&reader, &query("alpha", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        2
    );
    assert_eq!(reader.receipt("tail").unwrap(), Some(ack));
}
#[test]
fn paged_known_witness_fork_truncation_and_corrupt_cache_refuse_ghosts() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("common", &[row("alpha common", None, 1)])
        .unwrap();
    drop(log);
    copy(&f.root.join("authority"), &f.root.join("fork"));
    copy(&f.root.join("authority"), &f.root.join("old"));
    let mut log = f.log();
    let a = log.append("a", &[row("alpha branch_a", None, 2)]).unwrap();
    drop(log);
    let reader = f.reader(None);
    let mut index = f.index(&reader);
    drop(reader);
    let mut fork =
        RecordLog::open_clock(&f.root.join("fork"), &KEY, identity(), Arc::new(|| Ok(100)))
            .unwrap();
    fork.append("b", &[row("beta branch_b", None, 2)]).unwrap();
    drop(fork);
    assert!(Reader::open(&f.root.join("fork"), &KEY, identity(), Some(&a.watermark)).is_err());
    let fork = Reader::open(&f.root.join("fork"), &KEY, identity(), None).unwrap();
    assert!(
        index
            .search(&fork, &query("alpha", AsOf::default()))
            .is_err()
    );
    assert!(index.refresh(&fork).is_err());
    drop(index);
    index = f.index(&fork);
    assert_eq!(index.opened(), Opened::Rebuilt);
    assert!(
        index
            .search(&fork, &query("\"alpha branch_a\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
    drop(index);
    drop(fork);
    assert!(Reader::open(&f.root.join("old"), &KEY, identity(), Some(&a.watermark)).is_err());
    let reader = f.reader(None);
    drop(f.index(&reader));
    let segment = fs::read_dir(f.root.join("cache"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("seg-"))
        .unwrap();
    let mut bytes = fs::read(&segment).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 1;
    fs::write(segment, bytes).unwrap();
    let cache = f.index(&reader);
    assert_eq!(cache.opened(), Opened::Rebuilt);
    assert_eq!(
        cache
            .search(&reader, &query("alpha", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        2
    );
}
#[test]
fn paged_reader_schema_corruption_scope_bounds_and_encrypted_disk_are_enforced() {
    let f = Fixture::new();
    assert!(Reader::open(&f.root.join("absent"), &KEY, identity(), None).is_err());
    let mut log = f.log();
    log.append(
        "sensitive-request",
        &[row("private_paged_text alpha", None, 1)],
    )
    .unwrap();
    let min = log.tip().unwrap();
    drop(log);
    let reader = f.reader(Some(&min));
    let index = f.index(&reader);
    assert!(Index::open(&reader, &f.root.join("cache")).is_err());
    assert!(Index::open(&reader, &f.root.join("authority")).is_err());
    assert!(
        reader
            .view(
                Scope {
                    writer: "foreign",
                    owner: 1
                },
                AsOf::default()
            )
            .is_err()
    );
    let mut q = query("alpha", AsOf::default());
    q.max_bytes = 4;
    assert!(index.search(&reader, &q).is_err());
    for e in fs::read_dir(f.root.join("cache")).unwrap() {
        let bytes = fs::read(e.unwrap().path()).unwrap();
        for needle in [
            b"private_paged_text".as_slice(),
            WRITER.as_bytes(),
            b"reported-paged-agent",
            b"stored_metadata",
        ] {
            assert!(!bytes.windows(needle.len()).any(|b| b == needle));
        }
    }
    drop(index);
    drop(reader);
    assert!(Reader::open(&f.root.join("authority"), &[80; 32], identity(), None).is_err());
    let mut changed = identity();
    changed.adapter = "different".into();
    assert!(Reader::open(&f.root.join("authority"), &KEY, changed, None).is_err());
    // Authenticated but semantically malformed application envelope, below Store AEAD.
    let cipher = Arc::new(oojuro_store::crypto::FrameCipher::new(&KEY));
    let mut store = oojuro_store::Store::open(
        &f.root.join("authority"),
        oojuro_store::Options {
            cipher: Some(cipher),
            require_sealed: true,
            ..Default::default()
        },
    )
    .unwrap();
    let bytes = br#"{"schema":2,"missing":"required original fields"}"#;
    store
        .append(&[oojuro_store::NewRecord {
            kind: 7,
            valid_at: 0,
            recorded_at: 0,
            key: b"",
            attrs: b"",
            text: bytes,
        }])
        .unwrap();
    drop(store);
    assert!(Reader::open(&f.root.join("authority"), &KEY, identity(), None).is_err());
}
#[test]
fn paged_page_selection_does_not_skip_oversized_next_row() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append(
        "page",
        &[row("small", None, 1), row(&"long ".repeat(5000), None, 2)],
    )
    .unwrap();
    drop(log);
    let reader = f.reader(None);
    let s = reader.view(scope(), AsOf::default()).unwrap();
    let page = reader
        .range(
            &s,
            Versions::All,
            Filter::default(),
            Limits {
                entries: 2,
                bytes: 8000,
            },
        )
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert!(page.next.is_some());
    assert!(
        reader
            .continue_page(
                scope(),
                page.next.as_deref().unwrap(),
                Limits {
                    entries: 2,
                    bytes: 8000
                }
            )
            .is_err()
    );
}

#[test]
fn paged_winner_precedes_lower_bounds_and_typed_filter_statistics() {
    let f = Fixture::new();
    let mut log = f.log();
    let mut old = row("alpha old_tool word", Some("memory"), 10);
    old.kind = Kind::Input;
    old.tool = Some("old-tool".into());
    log.append("old", &[old]).unwrap();
    log.append("new", &[row("alpha newest", Some("memory"), 20)])
        .unwrap();
    let native = super::super::Index::rebuild(&log).unwrap();
    let mut q = query("alpha", AsOf::default());
    q.filter = Filter {
        kind: Some(Kind::Input),
        tool: Some("old-tool".into()),
        status: Some(Status::Ok),
        agent_id: Some("reported-paged-agent".into()),
    };
    let hidden = signature(native.search(&log, &q).unwrap());
    assert!(hidden.is_empty());
    q.as_of.known_at = 100;
    let past = signature(native.search(&log, &q).unwrap());
    assert_eq!(past.len(), 1);
    drop(native);
    drop(log);
    let reader = f.reader(None);
    let index = f.index(&reader);
    q.as_of = AsOf::default();
    assert_eq!(signature(index.search(&reader, &q).unwrap()), hidden);
    q.as_of.known_at = 100;
    assert_eq!(signature(index.search(&reader, &q).unwrap()), past);
    q.as_of.from = 11;
    assert!(index.search(&reader, &q).unwrap().hits.is_empty());
}
#[test]
#[ignore]
fn paged_crash_publication_child() {
    let root = PathBuf::from(std::env::var("OOJURO_PAGED_CRASH_ROOT").unwrap());
    let phase = std::env::var("OOJURO_PAGED_CRASH_PHASE").unwrap();
    let reader = Reader::open(&root.join("authority"), &KEY, identity(), None).unwrap();
    let mut cache = Index::open(&reader, &root.join("cache")).unwrap();
    drop(reader);
    let mut log = RecordLog::open_clock(
        &root.join("authority"),
        &KEY,
        identity(),
        Arc::new(|| Ok(100)),
    )
    .unwrap();
    log.append("child", &[row("alpha child committed", None, 2)])
        .unwrap();
    drop(log);
    let reader = Reader::open(&root.join("authority"), &KEY, identity(), None).unwrap();
    cache.hook_for_test(Box::new(move |at| {
        if at == phase {
            println!("PAGED_CACHE_BOUNDARY_READY");
            use std::io::Write;
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        }
    }));
    cache.refresh(&reader).unwrap();
    panic!("crash boundary not reached");
}
#[test]
fn paged_sigkill_new_cache_publication_preserves_original_ack_and_query() {
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
        let mut log = f.log();
        log.append("parent", &[row("alpha parent", None, 1)])
            .unwrap();
        drop(log);
        let reader = f.reader(None);
        drop(f.index(&reader));
        drop(reader);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "record::paged::tests::paged_crash_publication_child",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env("OOJURO_PAGED_CRASH_ROOT", &f.root)
            .env("OOJURO_PAGED_CRASH_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let listener = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.unwrap().contains("PAGED_CACHE_BOUNDARY_READY") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        let reached = rx.recv_timeout(Duration::from_secs(8));
        let _ = child.kill();
        let status = child.wait().unwrap();
        listener.join().unwrap();
        assert!(reached.is_ok(), "phase={phase}");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let reader = f.reader(None);
        let commit = reader.receipt("child").unwrap().unwrap();
        assert_eq!(commit.ids, vec![10]);
        let cache = f.index(&reader);
        assert_eq!(
            cache
                .search(&reader, &query("alpha", AsOf::default()))
                .unwrap()
                .hits
                .len(),
            2,
            "phase={phase}"
        );
        drop(cache);
        drop(reader);
        let mut log = f.log();
        assert_eq!(
            log.append("child", &[row("alpha child committed", None, 2)])
                .unwrap(),
            commit
        );
    }
}

#[test]
fn future_rows_change_literal_df_but_old_snapshot_keeps_native_bits_and_identifiers() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append(
        "old",
        &[
            row("alpha beta foo_bar", Some("memory"), 10),
            row("alpha alpha", None, 11),
            row("foo and bar continuation", None, 12),
        ],
    )
    .unwrap();
    let old_native = View::open(&log, scope(), AsOf::default()).unwrap();
    let texts = [
        "alpha",
        "foo_bar",
        "\"alpha beta\"",
        "\"beta\"",
        "key:memory",
        "foo",
    ];
    let native = super::super::Index::rebuild(&log).unwrap();
    let before: Vec<_> = texts
        .iter()
        .map(|t| {
            signature(
                native
                    .search_view(&log, &old_native, &query(t, AsOf::default()))
                    .unwrap(),
            )
        })
        .collect();
    drop(native);
    drop(log);
    let reader = f.reader(None);
    let old_paged = reader.view(scope(), AsOf::default()).unwrap();
    let mut cache = f.index(&reader);
    drop(reader);
    let mut log = f.log();
    let mut future = (0..60)
        .map(|i| row("beta beta future", None, 100 + i))
        .collect::<Vec<_>>();
    future.push(row("memory future foo_bar beta", Some("memory"), 200));
    log.append("future", &future).unwrap();
    let native = super::super::Index::rebuild(&log).unwrap();
    let after: Vec<_> = texts
        .iter()
        .map(|t| {
            signature(
                native
                    .search_view(&log, &old_native, &query(t, AsOf::default()))
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(before, after);
    drop(native);
    drop(log);
    let reader = f.reader(None);
    cache.refresh(&reader).unwrap();
    for (i, text) in texts.iter().enumerate() {
        assert_eq!(
            signature(
                cache
                    .search_view(&reader, &old_paged, &query(text, AsOf::default()))
                    .unwrap()
            ),
            after[i],
            "before merge {text}"
        );
    }
    cache.merge(&reader).unwrap();
    for (i, text) in texts.iter().enumerate() {
        assert_eq!(
            signature(
                cache
                    .search_view(&reader, &old_paged, &query(text, AsOf::default()))
                    .unwrap()
            ),
            after[i],
            "after merge {text}"
        );
    }
}

fn disk_policy(records: usize) -> DiskLimits {
    DiskLimits::new(records, 1 << 30, 1 << 30, 10000).unwrap()
}
fn disk_writer(f: &Fixture, records: usize) -> Writer {
    Writer::open_clock(
        &f.root.join("authority"),
        &KEY,
        identity(),
        &f.root.join("directory"),
        None,
        disk_policy(records),
        Arc::new(|| Ok(100)),
    )
    .unwrap()
}
#[test]
fn disk_writer_exact_retry_current_index_old_snapshot_and_native_rank_match() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 50);
    assert_eq!(writer.reader().unwrap().locator_count(), 0);
    let mut index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    let first = vec![
        row("alpha old foo_bar", Some("memory"), 10),
        row("alpha original duplicate", None, 11),
    ];
    let ack = writer.append_indexed(&mut index, "first", &first).unwrap();
    let old = writer
        .reader()
        .unwrap()
        .view(scope(), AsOf::default())
        .unwrap();
    let old_hits = signature(
        index
            .search_view(
                writer.reader().unwrap(),
                &old,
                &query("alpha", AsOf::default()),
            )
            .unwrap(),
    );
    let second = vec![
        row("alpha newest", Some("memory"), 20),
        row("alpha backdated", Some("memory"), 5),
    ];
    let next = writer.append("second", &second).unwrap();
    assert!(
        index
            .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
            .is_err()
    );
    index.refresh(writer.reader().unwrap()).unwrap();
    assert_eq!(
        writer.append_indexed(&mut index, "first", &first).unwrap(),
        ack
    );
    assert_eq!(writer.reader().unwrap().watermark().unwrap().records, 4);
    let mut changed = first.clone();
    changed[0].text.push('x');
    assert_eq!(writer.append("first", &changed), Err(Error::Conflict));
    assert_eq!(
        signature(
            index
                .search_view(
                    writer.reader().unwrap(),
                    &old,
                    &query("alpha", AsOf::default())
                )
                .unwrap()
        ),
        old_hits
    );
    let snapshot = writer
        .reader()
        .unwrap()
        .view(scope(), AsOf::default())
        .unwrap();
    assert_eq!(
        writer
            .reader()
            .unwrap()
            .get_key(&snapshot, "memory", 10000)
            .unwrap()
            .unwrap()
            .id,
        next.ids[0]
    );
    let actual: Vec<_> = ["alpha", "foo_bar", "\"alpha original\"", "key:memory", ""]
        .iter()
        .map(|text| {
            signature(
                index
                    .search(writer.reader().unwrap(), &query(text, AsOf::default()))
                    .unwrap(),
            )
        })
        .collect();
    let originals = writer
        .reader()
        .unwrap()
        .read_ordinals(
            &snapshot,
            0,
            Limits {
                entries: 1000,
                bytes: 1 << 20,
            },
        )
        .unwrap();
    assert_eq!(originals.len(), 4);
    assert_eq!(originals[0].text, first[0].text);
    assert!(
        writer
            .reader()
            .unwrap()
            .read_ordinals(
                &snapshot,
                0,
                Limits {
                    entries: 1000,
                    bytes: 8
                }
            )
            .is_err()
    );
    let past = writer
        .reader()
        .unwrap()
        .view(
            scope(),
            AsOf {
                known_at: ack.recorded_at[1],
                ..AsOf::default()
            },
        )
        .unwrap();
    assert_eq!(
        writer
            .reader()
            .unwrap()
            .read_ordinals(
                &past,
                0,
                Limits {
                    entries: 1000,
                    bytes: 1 << 20
                }
            )
            .unwrap()
            .len(),
        2
    );
    let minimum = writer.reader().unwrap().watermark().unwrap();
    drop(index);
    drop(writer);
    // Small new-path stores remain byte-format compatible with the bounded API.
    let log = f.log();
    let native = super::super::Index::rebuild(&log).unwrap();
    for (at, text) in ["alpha", "foo_bar", "\"alpha original\"", "key:memory", ""]
        .iter()
        .enumerate()
    {
        assert_eq!(
            signature(native.search(&log, &query(text, AsOf::default())).unwrap()),
            actual[at]
        );
    }
    drop(native);
    drop(log);
    let writer = Writer::open_disk(
        &f.root.join("authority"),
        &KEY,
        identity(),
        &f.root.join("directory"),
        Some(&minimum),
        disk_policy(50),
    )
    .unwrap();
    assert_eq!(
        writer.reader().unwrap().receipt("first").unwrap(),
        Some(ack)
    );
    let index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    assert_eq!(index.opened(), Opened::Loaded);
    assert_eq!(
        signature(
            index
                .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
                .unwrap()
        ),
        actual[0]
    );
    std::thread::scope(|threads| {
        for _ in 0..10 {
            let reader = writer.reader().unwrap();
            let snapshot = &snapshot;
            threads.spawn(move || {
                assert_eq!(
                    reader.get_id(snapshot, 10, 10000).unwrap().unwrap().text,
                    "alpha original duplicate"
                );
            });
        }
    });
}
#[test]
fn disk_writer_admission_does_not_expand_legacy_guards_and_retries_survive_full_store() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 2);
    let inputs = vec![
        row("bounded original", None, 1),
        row("bounded second", None, 2),
    ];
    assert_eq!(
        writer.append("too many", &[inputs.clone(), inputs.clone()].concat()),
        Err(Error::Bounds)
    );
    let ack = writer.append("full", &inputs).unwrap();
    assert_eq!(writer.append("full", &inputs).unwrap(), ack);
    assert_eq!(writer.append("overflow", &inputs[..1]), Err(Error::Bounds));
    assert_eq!(MAX_RECORDS, 100000);
    assert_eq!(MAX_STATE_BYTES, 128 << 20);
    drop(writer);
    assert!(
        Reader::open_disk_with_limits(
            &f.root.join("authority"),
            &KEY,
            identity(),
            &f.root.join("directory"),
            None,
            disk_policy(1)
        )
        .is_err()
    );
    let mut writer = disk_writer(&f, 2);
    assert_eq!(writer.append("full", &inputs).unwrap(), ack);
}
#[test]
fn disk_writer_unknown_ack_and_failed_search_publication_recover_exact_receipts() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 100);
    writer.hook = Some(Box::new(|phase| {
        if matches!(phase, authority::Phase::AfterStore) {
            panic!("synthetic lost ACK");
        }
    }));
    let original = vec![row("alpha exact after lost acknowledgement", None, 1)];
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || writer.append("uncertain", &original)
        ))
        .is_err()
    );
    assert!(writer.reader().is_err());
    drop(writer);
    let mut writer = disk_writer(&f, 100);
    let ack = writer
        .reader()
        .unwrap()
        .receipt("uncertain")
        .unwrap()
        .unwrap();
    assert_eq!(writer.append("uncertain", &original).unwrap(), ack);
    let cache = f.root.join("disk-index");
    let mut index = Index::open_disk(writer.reader().unwrap(), &cache).unwrap();
    fs::remove_file(cache.join("DISK_RECORD_INDEX")).unwrap();
    fs::create_dir(cache.join("DISK_RECORD_INDEX")).unwrap();
    let input = vec![row(
        "alpha committed despite disposable cache failure",
        None,
        2,
    )];
    assert!(
        writer
            .append_indexed(&mut index, "cache failure", &input)
            .is_err()
    );
    assert!(
        index
            .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
            .is_err()
    );
    let committed = writer
        .reader()
        .unwrap()
        .receipt("cache failure")
        .unwrap()
        .unwrap();
    fs::remove_dir(cache.join("DISK_RECORD_INDEX")).unwrap();
    drop(index);
    let mut index = Index::open_disk(writer.reader().unwrap(), &cache).unwrap();
    assert_eq!(
        writer
            .append_indexed(&mut index, "cache failure", &input)
            .unwrap(),
        committed
    );
    assert_eq!(
        index
            .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        2
    );
    drop(index);
    drop(writer);
    fs::remove_file(cache.join("DISK_RECORD_INDEX")).unwrap();
    fs::remove_file(f.root.join("directory/DIRECTORY")).unwrap();
    let writer = disk_writer(&f, 100);
    let index = Index::open_disk(writer.reader().unwrap(), &cache).unwrap();
    assert_eq!(
        writer.reader().unwrap().receipt("cache failure").unwrap(),
        Some(committed)
    );
    assert_eq!(
        index
            .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        2
    );
}

#[test]
#[ignore]
fn disk_writer_crash_child() {
    let f = Fixture {
        root: PathBuf::from(std::env::var("OOJURO_WRITER_CRASH_ROOT").unwrap()),
    };
    let phase = std::env::var("OOJURO_WRITER_CRASH_PHASE").unwrap();
    let mut writer = disk_writer(&f, 1000);
    let mut index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    if phase == "AfterStore" {
        writer.hook = Some(Box::new(|at| {
            if matches!(at, authority::Phase::AfterStore) {
                writer_boundary();
            }
        }));
    } else if let Some(phase) = phase.strip_prefix("index:") {
        let phase = phase.to_owned();
        index.hook_for_test(Box::new(move |at| {
            if at == phase {
                writer_boundary()
            }
        }));
    }
    if std::env::var_os("OOJURO_WRITER_GROUP").is_some() {
        let first = [row("alpha child exact", Some("memory"), 20)];
        let second = [row("alpha grouped independent", None, 21)];
        writer
            .append_group_indexed(
                &mut index,
                &[
                    Append {
                        request: "child",
                        inputs: &first,
                    },
                    Append {
                        request: "child-2",
                        inputs: &second,
                    },
                ],
            )
            .unwrap();
    } else {
        writer
            .append_indexed(
                &mut index,
                "child",
                &[row("alpha child exact", Some("memory"), 20)],
            )
            .unwrap();
    }
    panic!("writer boundary not reached");
}
fn writer_boundary() {
    use std::io::Write;
    println!("WRITER_BOUNDARY_READY");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park()
    }
}
#[test]
fn disk_writer_sigkill_actual_append_directory_and_index_publication_recovers_receipts() {
    writer_crash_matrix(false);
}
#[test]
fn disk_group_sigkill_independent_receipts_and_searchable_ack_boundaries() {
    writer_crash_matrix(true);
}
fn writer_crash_matrix(grouped: bool) {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    for phase in [
        "AfterStore",
        "directory:PackTemp",
        "directory:PackPublished",
        "directory:RootPagesDurable",
        "directory:RootTemp",
        "directory:RootPublished",
        "directory:RootDurable",
        "index:VisibilityTemp",
        "index:VisibilityPublished",
        "index:SegmentTemp",
        "index:SegmentPublished",
        "index:DirectoryPagesDurable",
        "index:ManifestTemp",
        "index:ManifestPublished",
        "index:ManifestDurable",
    ] {
        let f = Fixture::new();
        let mut writer = disk_writer(&f, 1000);
        let mut index =
            Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
        writer
            .append_indexed(
                &mut index,
                "parent",
                &[
                    row("alpha parent exact", Some("memory"), 10),
                    row("alpha independent", None, 11),
                ],
            )
            .unwrap();
        let old = writer
            .reader()
            .unwrap()
            .view(scope(), AsOf::default())
            .unwrap();
        let old_signature = signature(
            index
                .search_view(
                    writer.reader().unwrap(),
                    &old,
                    &query("alpha", AsOf::default()),
                )
                .unwrap(),
        );
        drop(index);
        drop(writer);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "record::paged::tests::disk_writer_crash_child",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env("OOJURO_WRITER_CRASH_ROOT", &f.root)
            .env("OOJURO_WRITER_CRASH_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(phase) = phase.strip_prefix("directory:") {
            command.env("OOJURO_DIRECTORY_CRASH_PHASE", phase);
        }
        if grouped {
            command.env("OOJURO_WRITER_GROUP", "1");
        }
        let mut child = command.spawn().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let listen = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if line.unwrap().contains("BOUNDARY_READY") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        let reached = rx.recv_timeout(Duration::from_secs(10));
        let _ = child.kill();
        let status = child.wait().unwrap();
        listen.join().unwrap();
        assert!(reached.is_ok(), "phase={phase}");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let mut writer = disk_writer(&f, 1000);
        let receipt = writer.reader().unwrap().receipt("child").unwrap().unwrap();
        assert_eq!(receipt.ids, vec![11]);
        if grouped {
            let second = writer
                .reader()
                .unwrap()
                .receipt("child-2")
                .unwrap()
                .unwrap();
            assert_eq!(second.ids, vec![12]);
            let first_input = [row("alpha child exact", Some("memory"), 20)];
            let second_input = [row("alpha grouped independent", None, 21)];
            assert_eq!(
                writer
                    .append_group(&[
                        Append {
                            request: "child-2",
                            inputs: &second_input
                        },
                        Append {
                            request: "child",
                            inputs: &first_input
                        }
                    ])
                    .unwrap(),
                vec![second, receipt.clone()]
            );
        }
        let mut index =
            Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
        assert_eq!(
            writer
                .append_indexed(
                    &mut index,
                    "child",
                    &[row("alpha child exact", Some("memory"), 20)]
                )
                .unwrap(),
            receipt
        );
        assert_eq!(
            signature(
                index
                    .search_view(
                        writer.reader().unwrap(),
                        &old,
                        &query("alpha", AsOf::default())
                    )
                    .unwrap()
            ),
            old_signature,
            "phase={phase}"
        );
        assert_eq!(
            index
                .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
                .unwrap()
                .hits
                .len(),
            if grouped { 3 } else { 2 }
        );
        let snapshot = writer
            .reader()
            .unwrap()
            .view(scope(), AsOf::default())
            .unwrap();
        let originals = writer
            .reader()
            .unwrap()
            .get_ids(&snapshot, &[11, 10, 11, 0, 999], 1 << 20)
            .unwrap();
        assert_eq!(originals[0].as_ref().unwrap().text, "alpha child exact");
        assert_eq!(originals[1].as_ref().unwrap().text, "alpha independent");
        assert_eq!(
            originals[0].as_ref().unwrap().id,
            originals[2].as_ref().unwrap().id
        );
        assert!(originals[3].is_none() && originals[4].is_none());
    }
}
#[test]
fn disk_writer_evicted_births_old_prefix_and_copied_origin_fork_are_checked() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 1000);
    writer
        .append("genesis", &[row("alpha original", Some("memory"), 0)])
        .unwrap();
    let old = writer
        .reader()
        .unwrap()
        .view(scope(), AsOf::default())
        .unwrap();
    for n in 1..90 {
        writer
            .append(
                &format!("generation-{n}"),
                &[row(&format!("alpha version {n}"), Some("memory"), n)],
            )
            .unwrap();
    }
    let latest = writer
        .reader()
        .unwrap()
        .view(scope(), AsOf::default())
        .unwrap();
    assert_eq!(
        writer
            .reader()
            .unwrap()
            .get_key(&old, "memory", 10000)
            .unwrap()
            .unwrap()
            .text,
        "alpha original"
    );
    assert_eq!(
        writer
            .reader()
            .unwrap()
            .get_key(&latest, "memory", 10000)
            .unwrap()
            .unwrap()
            .text,
        "alpha version 89"
    );
    assert!(
        writer
            .reader()
            .unwrap()
            .get_ids(&old, &[9, 98], 10000)
            .unwrap()[1]
            .is_none()
    );
    let future = writer.reader().unwrap().watermark().unwrap();
    drop(writer);
    let fork = f.root.join("fork");
    copy(&f.root.join("authority"), &fork);
    // A same-origin fork keeps the old prefix but must not adopt a later main snapshot.
    let mut fork_writer = Writer::open_clock(
        &fork,
        &KEY,
        identity(),
        &f.root.join("fork-directory"),
        None,
        disk_policy(1000),
        Arc::new(|| Ok(100)),
    )
    .unwrap();
    fork_writer
        .append("fork-tail", &[row("alpha fork branch", Some("memory"), 90)])
        .unwrap();
    drop(fork_writer);
    let mut writer = disk_writer(&f, 1000);
    writer
        .append("main-tail", &[row("alpha main branch", Some("memory"), 90)])
        .unwrap();
    let main = writer
        .reader()
        .unwrap()
        .view(scope(), AsOf::default())
        .unwrap();
    let main_min = writer.reader().unwrap().watermark().unwrap();
    drop(writer);
    assert!(
        Writer::open_disk(
            &fork,
            &KEY,
            identity(),
            &f.root.join("fork-directory"),
            Some(&main_min),
            disk_policy(1000)
        )
        .is_err()
    );
    let fork_writer = Writer::open_disk(
        &fork,
        &KEY,
        identity(),
        &f.root.join("fork-directory"),
        Some(&future),
        disk_policy(1000),
    )
    .unwrap();
    assert!(
        fork_writer
            .reader()
            .unwrap()
            .get_id(&main, 99, 10000)
            .is_err()
    );
    assert_eq!(
        fork_writer
            .reader()
            .unwrap()
            .get_key(&old, "memory", 10000)
            .unwrap()
            .unwrap()
            .text,
        "alpha original"
    );
}

#[test]
fn disk_visibility_metadata_matches_native_clocks_filters_versions_and_literal_bits() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 1000);
    let mut index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    for start in (0..120).step_by(5) {
        let inputs = (start..start + 5)
            .map(|n| {
                let mut value = row(
                    &format!("alpha beta foo_bar literal group{}", n % 4),
                    if n % 3 == 0 { Some("revision") } else { None },
                    (n % 71) as i64 - 5,
                );
                value.agent_id = format!("source{}", n % 3);
                value.kind = if n % 2 == 0 {
                    Kind::Input
                } else {
                    Kind::ToolResult
                };
                value.tool = Some(format!("tool{}", n % 2));
                value.status = Some(if n % 5 == 0 {
                    Status::Error
                } else {
                    Status::Ok
                });
                value.indexed = n % 7 != 0;
                value
            })
            .collect::<Vec<_>>();
        writer
            .append_indexed(&mut index, &format!("batch-{start}"), &inputs)
            .unwrap();
    }
    let mut queries = Vec::new();
    for text in [
        "alpha",
        "foo_bar",
        "foo_missing",
        "\"alpha beta\"",
        "\"literal group3\"",
        "key:revision",
        "",
        "absent",
    ] {
        for clock in [
            AsOf::default(),
            AsOf {
                valid_at: 25,
                ..AsOf::default()
            },
            AsOf {
                known_at: 150,
                ..AsOf::default()
            },
            AsOf {
                from: 15,
                valid_at: 40,
                known_at: 180,
            },
        ] {
            for filter in [
                Filter::default(),
                Filter {
                    agent_id: Some("source1".into()),
                    ..Filter::default()
                },
                Filter {
                    kind: Some(Kind::ToolResult),
                    tool: Some("tool1".into()),
                    status: Some(Status::Ok),
                    agent_id: None,
                },
            ] {
                queries.push((text.to_owned(), clock, filter));
            }
        }
    }
    let expected = queries
        .iter()
        .map(|(text, as_of, filter)| {
            let mut q = query(text, *as_of);
            q.filter = filter.clone();
            let expected = signature(index.search(writer.reader().unwrap(), &q).unwrap());
            let (profiled, work) = index.search_profiled(writer.reader().unwrap(), &q).unwrap();
            assert_eq!(signature(profiled), expected);
            assert_eq!(work.fallback_authority_rows, 0);
            expected
        })
        .collect::<Vec<_>>();
    index.merge(writer.reader().unwrap()).unwrap();
    for (at, (text, as_of, filter)) in queries.iter().enumerate() {
        let mut q = query(text, *as_of);
        q.filter = filter.clone();
        assert_eq!(
            signature(index.search(writer.reader().unwrap(), &q).unwrap()),
            expected[at],
            "after merge {text}"
        );
    }
    drop(index);
    drop(writer);
    let log = f.log();
    let native = super::super::Index::rebuild(&log).unwrap();
    for (at, (text, as_of, filter)) in queries.iter().enumerate() {
        let mut q = query(text, *as_of);
        q.filter = filter.clone();
        assert_eq!(
            signature(native.search(&log, &q).unwrap()),
            expected[at],
            "native {text}"
        );
    }
    drop(native);
    drop(log);
    let writer = disk_writer(&f, 1000);
    let index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    assert_eq!(index.opened(), Opened::Loaded);
    let page = fs::read_dir(f.root.join("disk-index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("vis-"))
        .unwrap();
    let sealed = fs::read(&page).unwrap();
    let mut corrupted = sealed.clone();
    let at = corrupted.len() / 2;
    corrupted[at] ^= 1;
    fs::write(&page, corrupted).unwrap();
    assert!(
        index
            .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
            .is_err()
    );
    drop(index);
    let index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    assert_eq!(index.opened(), Opened::Rebuilt);
    assert_eq!(
        signature(
            index
                .search(writer.reader().unwrap(), &query("alpha", AsOf::default()))
                .unwrap()
        ),
        expected[0]
    );
}

#[test]
fn disk_group_preserves_independent_retries_clocks_order_conflicts_and_legacy_format() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 1000);
    let seed = [row("alpha seed", None, 0)];
    let seed_receipt = writer.append("seed", &seed).unwrap();
    let first = [
        row("alpha first version", Some("memory"), 10),
        row("alpha independent", None, 11),
    ];
    let second = [row("alpha second version", Some("memory"), 20)];
    let mut index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    let receipts = writer
        .append_group_indexed(
            &mut index,
            &[
                Append {
                    request: "first",
                    inputs: &first,
                },
                Append {
                    request: "second",
                    inputs: &second,
                },
                Append {
                    request: "first",
                    inputs: &first,
                },
                Append {
                    request: "seed",
                    inputs: &seed,
                },
            ],
        )
        .unwrap();
    assert_eq!(receipts[0], receipts[2]);
    assert_eq!(receipts[3], seed_receipt);
    assert_eq!(receipts[0].ids, vec![10, 11]);
    assert_eq!(receipts[1].ids, vec![12]);
    assert_eq!(receipts[0].recorded_at, vec![101, 102]);
    assert_eq!(receipts[1].recorded_at, vec![103]);
    assert_eq!(receipts[0].watermark.sequence, 2);
    assert_eq!(receipts[1].watermark.sequence, 3);
    let before = writer.reader().unwrap().watermark().unwrap();
    let conflict = [row("different input", Some("memory"), 10)];
    assert_eq!(
        writer.append_group(&[
            Append {
                request: "fresh-before-conflict",
                inputs: &seed
            },
            Append {
                request: "first",
                inputs: &conflict
            }
        ]),
        Err(Error::Conflict)
    );
    assert!(
        writer
            .reader()
            .unwrap()
            .receipt("fresh-before-conflict")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        writer.reader().unwrap().watermark().unwrap().records,
        before.records
    );
    assert_eq!(writer.append_group(&[]), Err(Error::Bounds));
    assert_eq!(
        writer.append_group(&vec![
            Append {
                request: "repeated",
                inputs: &seed
            };
            1001
        ]),
        Err(Error::Bounds)
    );
    let mut invalid = seed.clone();
    invalid[0].agent_id.clear();
    assert_eq!(
        writer.append_group(&[
            Append {
                request: "fresh-before-invalid",
                inputs: &seed
            },
            Append {
                request: "invalid",
                inputs: &invalid
            }
        ]),
        Err(Error::Bounds)
    );
    assert!(
        writer
            .reader()
            .unwrap()
            .receipt("fresh-before-invalid")
            .unwrap()
            .is_none()
    );
    let mut q = query(
        "alpha",
        AsOf {
            known_at: 102,
            ..AsOf::default()
        },
    );
    let past = signature(index.search(writer.reader().unwrap(), &q).unwrap());
    q.as_of = AsOf::default();
    let current = signature(index.search(writer.reader().unwrap(), &q).unwrap());
    assert_eq!(past.len(), 3);
    assert_eq!(current.len(), 3);
    drop(index);
    drop(writer);
    let mut log = f.log();
    let native = super::super::Index::rebuild(&log).unwrap();
    q.as_of = AsOf {
        known_at: 102,
        ..AsOf::default()
    };
    assert_eq!(signature(native.search(&log, &q).unwrap()), past);
    q.as_of = AsOf::default();
    assert_eq!(signature(native.search(&log, &q).unwrap()), current);
    drop(native);
    assert_eq!(log.append("first", &first).unwrap(), receipts[0]);
    assert_eq!(log.append("second", &second).unwrap(), receipts[1]);
    drop(log);
    let mut writer = disk_writer(&f, 1000);
    assert_eq!(
        writer
            .append_group(&[
                Append {
                    request: "second",
                    inputs: &second
                },
                Append {
                    request: "first",
                    inputs: &first
                }
            ])
            .unwrap(),
        vec![receipts[1].clone(), receipts[0].clone()]
    );
}
#[test]
fn disk_group_encoded_capacity_failure_does_not_publish_a_prefix() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 1000);
    // Each individual input fits, but escaping makes the two fresh batches
    // exceed the shared 32 MiB group bound. Nothing may commit first.
    let first = [row(&"\\".repeat(9 << 20), None, 1)];
    let second = [row(&"\\".repeat(9 << 20), None, 2)];
    assert_eq!(
        writer.append_group(&[
            Append {
                request: "first",
                inputs: &first
            },
            Append {
                request: "second",
                inputs: &second
            }
        ]),
        Err(Error::Bounds)
    );
    assert_eq!(writer.reader().unwrap().watermark().unwrap().records, 0);
    assert!(writer.reader().unwrap().receipt("first").unwrap().is_none());
}

#[test]
fn disk_write_phase_profile_preserves_receipts_and_distinguishes_fresh_from_retries() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 1000);
    let mut index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    let first = [row("alpha first", None, 1)];
    let second = [row("alpha second", None, 2)];
    let requests = [
        Append {
            request: "first",
            inputs: &first,
        },
        Append {
            request: "second",
            inputs: &second,
        },
    ];
    let (receipts, profile) = writer
        .append_group_indexed_profiled(&mut index, &requests)
        .unwrap();
    assert_eq!(profile.submitted_rows, 2);
    assert_eq!(profile.rows_committed, 2);
    assert_eq!(profile.fresh_requests, 2);
    assert!(profile.source_encoded_bytes > 0);
    let (retry, retry_work) = writer
        .append_group_indexed_profiled(&mut index, &requests)
        .unwrap();
    assert_eq!(receipts, retry);
    assert_eq!(retry_work.fresh_requests, 0);
    assert_eq!(retry_work.rows_committed, 0);
    assert_eq!(retry_work.authority_append_ns, 0);
    assert_eq!(retry_work.locator_publish_ns, 0);
    assert_eq!(
        writer.reader().unwrap().receipt("first").unwrap(),
        Some(receipts[0].clone())
    );
}

#[test]
fn disk_word_query_uses_only_required_scoring_passes_without_changing_results() {
    let f = Fixture::new();
    let mut writer = disk_writer(&f, 1000);
    let mut index = Index::open_disk(writer.reader().unwrap(), &f.root.join("disk-index")).unwrap();
    for start in [0, 500] {
        let inputs = (start..start + 500)
            .map(|n| row("alpha foo_bar", None, n))
            .collect::<Vec<_>>();
        writer
            .append_indexed(&mut index, &format!("batch-{start}"), &inputs)
            .unwrap();
    }
    let q = query("alpha", AsOf::default());
    let (found, work) = index.search_profiled(writer.reader().unwrap(), &q).unwrap();
    assert_eq!(
        signature(found),
        signature(index.search(writer.reader().unwrap(), &q).unwrap())
    );
    assert_eq!(work.segment_loads, 2 * index.segment_count() as u64);
    assert_eq!(work.visibility_pages, 2 * index.segment_count() as u64);
    assert_eq!(work.rows_examined, 2000);
    assert_eq!(work.term_pass_ns, 0);
    assert_eq!(work.presence_pass_ns, 0);
    let literal = query("\"absent_literal\"", AsOf::default());
    let (_, literal_work) = index
        .search_profiled(writer.reader().unwrap(), &literal)
        .unwrap();
    assert!(literal_work.segment_loads > work.segment_loads);
}
