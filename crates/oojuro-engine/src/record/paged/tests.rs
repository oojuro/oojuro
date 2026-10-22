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
