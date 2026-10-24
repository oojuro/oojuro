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
const KEY: [u8; 32] = [41; 32];
const WRITER: &str = "record-synthetic-owner";
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
                "record-time-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn open(&self) -> RecordLog {
        RecordLog::open(&self.root.join("authority"), &KEY, identity()).unwrap()
    }
    fn clock(&self, n: i64) -> RecordLog {
        RecordLog::open_clock(
            &self.root.join("authority"),
            &KEY,
            identity(),
            Arc::new(move || Ok(n)),
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
        adapter: "direct-record-v1".into(),
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
        agent_id: "reported-instance".into(),
        parent_id: Some("reported-parent".into()),
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
fn view(log: &RecordLog) -> View {
    View::open(log, scope(), AsOf::default()).unwrap()
}
#[test]
fn record_time_ack_reopen_and_original_typed_retry() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    let row = input("battery electrode", Some("memory"), -50);
    let ack = log.append("request-1", std::slice::from_ref(&row)).unwrap();
    assert_eq!(ack.ids, vec![9]);
    assert_eq!(ack.recorded_at, vec![100]);
    drop(log);
    let mut log = f.clock(-100);
    assert_eq!(
        log.append("request-1", std::slice::from_ref(&row)).unwrap(),
        ack
    );
    let r = view(&log).get_key(&log, "memory", 4096).unwrap().unwrap();
    assert_eq!(r.id, 9);
    assert_eq!(r.valid_at, -50);
    assert_eq!(r.recorded_at, 100);
    assert_eq!(r.agent_id, "reported-instance");
    let next = log
        .append(
            "request-2",
            &[input("battery changed", Some("memory"), -60)],
        )
        .unwrap();
    assert_eq!(next.recorded_at, vec![101]);
    assert_eq!(
        view(&log)
            .get_key(&log, "memory", 4096)
            .unwrap()
            .unwrap()
            .id,
        9
    );
    let index = Index::rebuild(&log).unwrap();
    assert_eq!(
        index
            .search(
                &log,
                &Query {
                    scope: scope(),
                    as_of: AsOf::default(),
                    text: "battery",
                    k: 10,
                    filter: Filter::default(),
                    max_bytes: 1 << 20
                }
            )
            .unwrap()
            .hits[0]
            .record
            .id,
        9
    );
}

fn query<'a>(text: &'a str, as_of: AsOf) -> Query<'a> {
    Query {
        scope: scope(),
        as_of,
        text,
        k: 100,
        filter: Filter::default(),
        max_bytes: 1 << 20,
    }
}
#[test]
fn record_time_original_intents_and_complete_json_are_required() {
    let f = Fixture::new();
    let mut log = f.clock(10);
    let row = input("original battery", Some("key"), 5);
    let ack = log.append("same", std::slice::from_ref(&row)).unwrap();
    let mut changes = vec![];
    let mut x = row.clone();
    x.valid_at += 1;
    changes.push(x);
    let mut x = row.clone();
    x.indexed = false;
    changes.push(x);
    let mut x = row.clone();
    x.key = None;
    changes.push(x);
    let mut x = row.clone();
    x.attrs
        .insert("source".into(), serde_json::json!(["reported", 1]));
    changes.push(x);
    let mut x = row.clone();
    x.agent_id = "other-instance".into();
    changes.push(x);
    let mut x = row.clone();
    x.kind = Kind::ToolResult;
    changes.push(x);
    let mut x = row.clone();
    x.status = Some(Status::Error);
    changes.push(x);
    for x in changes {
        assert_eq!(log.append("same", &[x]), Err(Error::Conflict));
    }
    assert_eq!(log.append("same", std::slice::from_ref(&row)).unwrap(), ack);
    let mut value = serde_json::to_value(&row).unwrap();
    value.as_object_mut().unwrap().remove("key");
    assert!(serde_json::from_value::<Input>(value).is_err());
    let mut x = row.clone();
    x.attrs.insert(
        "large".into(),
        serde_json::json!("x".repeat(MAX_ATTR_BYTES)),
    );
    assert_eq!(log.append("large", &[x]), Err(Error::Bounds));
    let mut nested = serde_json::json!(null);
    for _ in 0..34 {
        nested = serde_json::json!([nested]);
    }
    let mut x = row.clone();
    x.attrs.insert("deep".into(), nested);
    assert_eq!(log.append("deep", &[x]), Err(Error::Bounds));
    let mut x = row.clone();
    x.attrs
        .insert("escaped".into(), serde_json::json!("\n".repeat(2500)));
    assert_eq!(log.append("escaped", &[x]), Err(Error::Bounds));
    let mut x = row;
    x.text = "\u{1}".repeat(6 << 20);
    assert_eq!(log.append("escaped-frame", &[x]), Err(Error::Bounds));
    assert_eq!(
        view(&log)
            .range(&log, Versions::All, Filter::default(), Limits::default())
            .unwrap()
            .records
            .len(),
        1
    );
}
#[test]
fn record_time_overflow_never_commits_and_retry_does_not_read_clock() {
    let f = Fixture::new();
    let mut log = f.clock(i64::MAX);
    let row = input("battery", None, 0);
    let ack = log.append("last-time", std::slice::from_ref(&row)).unwrap();
    assert_eq!(ack.recorded_at, vec![i64::MAX]);
    assert_eq!(
        log.append("overflow", std::slice::from_ref(&row)),
        Err(Error::Bounds)
    );
    drop(log);
    let mut log = RecordLog::open_clock(
        &f.root.join("authority"),
        &KEY,
        identity(),
        Arc::new(|| Err(Error::Unavailable)),
    )
    .unwrap();
    assert_eq!(log.append("last-time", &[row]).unwrap(), ack);
    let g = Fixture::new();
    let mut id = identity();
    id.first_id = u64::MAX;
    let mut log = RecordLog::open(&g.root.join("authority"), &KEY, id).unwrap();
    assert_eq!(
        log.append("overflow", &[input("battery", None, 0)]),
        Err(Error::Bounds)
    );
    assert!(
        view(&log)
            .range(&log, Versions::All, Filter::default(), Limits::default())
            .unwrap()
            .records
            .is_empty()
    );
}
#[test]
fn record_time_key_winner_precedes_lower_and_metadata_filters() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    let a = input("battery old", Some("key"), 10);
    let mut b = input("battery newest", Some("key"), 30);
    b.kind = Kind::ToolResult;
    b.tool = Some("Read".into());
    b.indexed = false;
    let c = input("battery late-backdate", Some("key"), 20);
    log.append(
        "batch",
        &[
            a,
            b,
            c,
            input("battery unversioned", None, 10),
            input("battery unversioned", None, 10),
        ],
    )
    .unwrap();
    let now = view(&log);
    assert_eq!(now.get_key(&log, "key", 4096).unwrap().unwrap().id, 10);
    assert_eq!(
        now.history(&log, "key", Limits::default())
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![9, 11, 10]
    );
    let index = Index::rebuild(&log).unwrap();
    let all = index.search(&log, &query("", AsOf::default())).unwrap();
    assert_eq!(
        all.hits.iter().map(|h| h.record.id).collect::<Vec<_>>(),
        vec![10, 12, 13]
    );
    let mut q = query("battery", AsOf::default());
    q.filter.kind = Some(Kind::Explicit);
    let hits = index.search(&log, &q).unwrap();
    assert_eq!(
        hits.hits.iter().map(|h| h.record.id).collect::<Vec<_>>(),
        vec![12, 13]
    );
    assert!(
        index
            .search(
                &log,
                &Query {
                    text: "key:key",
                    ..q
                }
            )
            .unwrap()
            .hits
            .is_empty()
    );
    let at = AsOf {
        valid_at: 25,
        ..AsOf::default()
    };
    assert_eq!(
        View::open(&log, scope(), at)
            .unwrap()
            .get_key(&log, "key", 4096)
            .unwrap()
            .unwrap()
            .id,
        11
    );
    let known = AsOf {
        known_at: 100,
        ..AsOf::default()
    };
    assert_eq!(
        View::open(&log, scope(), known)
            .unwrap()
            .get_key(&log, "key", 4096)
            .unwrap()
            .unwrap()
            .id,
        9
    );
    let lower = AsOf {
        from: 31,
        ..AsOf::default()
    };
    assert!(
        View::open(&log, scope(), lower)
            .unwrap()
            .get_key(&log, "key", 4096)
            .unwrap()
            .is_none()
    );
    let old = AsOf {
        valid_at: 20,
        known_at: 101,
        ..AsOf::default()
    };
    assert_eq!(
        View::open(&log, scope(), old)
            .unwrap()
            .get_key(&log, "key", 4096)
            .unwrap()
            .unwrap()
            .id,
        9
    );
}
#[test]
fn record_time_snapshot_pages_scope_fork_reopen_and_output_bounds() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    log.append(
        "first",
        &[
            input("battery one", None, 1),
            input("battery two", None, 2),
            input("battery three", None, 3),
        ],
    )
    .unwrap();
    let old = view(&log);
    let page = old
        .range(
            &log,
            Versions::All,
            Filter::default(),
            Limits {
                entries: 1,
                bytes: 4096,
            },
        )
        .unwrap();
    let token = page.next.unwrap();
    assert_eq!(page.records[0].id, 9);
    let mut index = Index::rebuild(&log).unwrap();
    log.append("later", &[input("battery later", None, 4)])
        .unwrap();
    assert!(matches!(
        index.search(&log, &query("battery", AsOf::default())),
        Err(Error::Unavailable)
    ));
    index.refresh(&log).unwrap();
    assert_eq!(
        index
            .search_view(&log, &old, &query("battery", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        3
    );
    assert_eq!(
        old.range(&log, Versions::All, Filter::default(), Limits::default())
            .unwrap()
            .records
            .len(),
        3
    );
    assert!(matches!(
        View::open(
            &log,
            Scope {
                writer: "other",
                owner: 1
            },
            AsOf::default()
        ),
        Err(Error::Scope)
    ));
    assert!(matches!(
        View::open(
            &log,
            Scope {
                writer: WRITER,
                owner: 2
            },
            AsOf::default()
        ),
        Err(Error::Scope)
    ));
    assert!(matches!(old.get_id(&log, u64::MAX, 3), Err(Error::Bounds)));
    assert!(old.get_id(&log, u64::MAX, 4).unwrap().is_none());
    let exact = old
        .range(&log, Versions::All, Filter::default(), Limits::default())
        .unwrap();
    let bytes = serde_json::to_vec(&exact).unwrap().len();
    assert_eq!(
        old.export(
            &log,
            Versions::All,
            Filter::default(),
            Limits {
                entries: 100,
                bytes
            }
        )
        .unwrap()
        .len(),
        bytes
    );
    assert!(matches!(
        old.range(
            &log,
            Versions::All,
            Filter::default(),
            Limits {
                entries: 1,
                bytes: 100
            }
        ),
        Err(Error::Bounds)
    ));
    assert!(matches!(
        index.search(
            &log,
            &Query {
                max_bytes: 20,
                ..query("battery", AsOf::default())
            }
        ),
        Err(Error::Bounds)
    ));
    drop(log);
    let log = f.open();
    let next = View::continue_page(
        &log,
        scope(),
        &token,
        Limits {
            entries: 1,
            bytes: 4096,
        },
    )
    .unwrap();
    assert_eq!(next.records[0].id, 10);
    let last = View::continue_page(&log, scope(), &next.next.unwrap(), Limits::default()).unwrap();
    assert_eq!(
        last.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![11]
    );
    assert!(last.next.is_none());
    let mut changed = token.into_bytes();
    changed[60] = if changed[60] == b'0' { b'1' } else { b'0' };
    assert!(
        View::continue_page(
            &log,
            scope(),
            std::str::from_utf8(&changed).unwrap(),
            Limits::default()
        )
        .is_err()
    );
    let g = Fixture::new();
    let mut other = g.clock(100);
    other
        .append("first", &[input("battery fork", None, 1)])
        .unwrap();
    assert!(matches!(old.get_id(&other, 9, 4096), Err(Error::Authority)));
    assert!(
        index
            .search(&other, &query("battery", AsOf::default()))
            .is_err()
    );
}
#[test]
fn record_time_namespace_is_explicit_and_disk_is_sealed() {
    let f = Fixture::new();
    let mut log = f.open();
    let mut row = input(
        "private synthetic battery payload",
        Some("private synthetic key"),
        42,
    );
    row.attrs.insert(
        "private synthetic attr".into(),
        serde_json::json!("private synthetic value"),
    );
    log.append("private synthetic request", &[row]).unwrap();
    drop(log);
    fn files(p: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = vec![];
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                out.extend(files(&e.path()));
            } else {
                out.push((e.path(), fs::read(e.path()).unwrap()));
            }
        }
        out
    }
    let before = files(&f.root.join("authority"));
    for (_, bytes) in &before {
        for text in [
            "private synthetic battery payload",
            "private synthetic key",
            "private synthetic value",
            "private synthetic request",
        ] {
            assert!(!bytes.windows(text.len()).any(|w| w == text.as_bytes()));
        }
    }
    assert!(
        crate::capture_log::CaptureLog::open(&f.root.join("authority"), &KEY, identity()).is_err()
    );
    assert_eq!(before, files(&f.root.join("authority")));
    let g = Fixture::new();
    let legacy =
        crate::capture_log::CaptureLog::open(&g.root.join("authority"), &KEY, identity()).unwrap();
    drop(legacy);
    let before = files(&g.root.join("authority"));
    assert!(RecordLog::open(&g.root.join("authority"), &KEY, identity()).is_err());
    assert_eq!(before, files(&g.root.join("authority")));
    assert!(RecordLog::open(&f.root.join("authority"), &[40; 32], identity()).is_err());
}
#[test]
fn record_time_crash_child() {
    let Ok(root) = std::env::var("OOJURO_RECORD_TEST_ROOT") else {
        return;
    };
    let phase = std::env::var("OOJURO_RECORD_TEST_PHASE").unwrap();
    let mut log = RecordLog::open_clock(
        &PathBuf::from(&root).join("authority"),
        &KEY,
        identity(),
        Arc::new(|| Ok(500)),
    )
    .unwrap();
    if phase == "after-store" {
        log.hook = Some(Box::new(|p| {
            if matches!(p, authority::Phase::AfterStore) {
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
            }
        }));
    }
    let ack = log
        .append(
            "crash-request",
            &[input("battery crash original", Some("crash-key"), -700)],
        )
        .unwrap();
    fs::write(
        PathBuf::from(root).join("ack.json"),
        serde_json::to_vec(&ack).unwrap(),
    )
    .unwrap();
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
}
#[test]
fn record_time_sigkill_before_and_after_ack_recovers_original_times_ids_and_queries() {
    use std::os::unix::process::ExitStatusExt;
    for phase in ["after-store", "after-ack"] {
        let f = Fixture::new();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "record::tests::record_time_crash_child",
                "--nocapture",
            ])
            .env("OOJURO_RECORD_TEST_ROOT", &f.root)
            .env("OOJURO_RECORD_TEST_PHASE", phase)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(child.signal(), Some(libc::SIGKILL));
        let mut log = f.clock(-100);
        let ack = log
            .append(
                "crash-request",
                &[input("battery crash original", Some("crash-key"), -700)],
            )
            .unwrap();
        assert_eq!(ack.ids, vec![9]);
        assert_eq!(ack.recorded_at, vec![500]);
        if phase == "after-ack" {
            let original: Commit =
                serde_json::from_slice(&fs::read(f.root.join("ack.json")).unwrap()).unwrap();
            assert_eq!(original, ack);
        }
        assert_eq!(
            log.append(
                "crash-request",
                &[input("battery crash original", Some("crash-key"), -700)]
            )
            .unwrap(),
            ack
        );
        let v = view(&log);
        assert_eq!(v.get_id(&log, 9, 4096).unwrap().unwrap().recorded_at, 500);
        assert_eq!(v.get_key(&log, "crash-key", 4096).unwrap().unwrap().id, 9);
        assert_eq!(
            v.history(&log, "crash-key", Limits::default())
                .unwrap()
                .records
                .len(),
            1
        );
        assert!(
            !v.export(&log, Versions::All, Filter::default(), Limits::default())
                .unwrap()
                .is_empty()
        );
        let before = View::open(
            &log,
            scope(),
            AsOf {
                known_at: 499,
                ..AsOf::default()
            },
        )
        .unwrap();
        assert!(before.get_id(&log, 9, 4096).unwrap().is_none());
        let index = Index::rebuild(&log).unwrap();
        assert_eq!(
            index
                .search(&log, &query("battery", AsOf::default()))
                .unwrap()
                .hits[0]
                .record
                .id,
            9
        );
    }
}
fn oracle_rows(log: &RecordLog) -> Vec<(oojuro_core::Record, bool)> {
    log.records
        .iter()
        .map(|r| {
            let mut row = oojuro_core::Record::new(
                &r.id.to_string(),
                r.input.key.as_deref().unwrap_or(""),
                &r.input.text,
                r.input.valid_at,
            );
            if row.key.is_empty() {
                row.key = format!("unkeyed:{}", r.id);
            }
            row.recorded_at = r.recorded_at;
            (row, r.input.indexed)
        })
        .collect()
}
fn compare_oracle(log: &RecordLog, index: &Index, oracle: &oojuro_nine::Nine, cuts: &[AsOf]) {
    for &as_of in cuts {
        for text in [
            "",
            "battery",
            "battery electrode",
            "battery battery",
            "lookup_needle battery",
            "absent_name battery",
            "\"battery electrode\"",
            "\"the\"",
            "key:versioned",
            "versioned",
            "key:absent",
        ] {
            for k in [1, 5, 100] {
                let mut q = query(text, as_of);
                q.k = k;
                let got = index
                    .search(log, &q)
                    .unwrap()
                    .hits
                    .into_iter()
                    .map(|h| {
                        (
                            h.record.id.to_string(),
                            h.bits.to_bits(),
                            h.continued,
                            h.record.valid_at,
                        )
                    })
                    .collect::<Vec<_>>();
                let mut q = oojuro_core::Query::new(text, as_of.valid_at);
                q.known_at = as_of.known_at;
                q.k = k;
                let want = oracle
                    .search_refs(&q, as_of.from)
                    .unwrap()
                    .matches
                    .into_iter()
                    .map(|h| (h.id, h.score_bits.to_bits(), h.continued, h.time))
                    .collect::<Vec<_>>();
                assert_eq!(got, want, "text={text:?} k={k} cut={as_of:?}");
            }
        }
    }
}
#[test]
fn record_time_direct_nine_bitwise_version_time_and_background_parity() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    let mut rows = vec![
        input("lookup_needle battery electrode", Some("versioned"), 20),
        input("battery battery electrode", Some("versioned"), 40),
        input("battery backdated", Some("versioned"), 30),
        input("battery electrode", None, 20),
        input("battery electrode", None, 20),
        input("the", None, 40),
    ];
    let mut x = input("unindexed battery", Some("stored-only"), 40);
    x.indexed = false;
    rows.push(x);
    log.append("small", &rows).unwrap();
    let index = Index::rebuild(&log).unwrap();
    let oracle = oojuro_nine::Nine::open(&f.root.join("oracle")).unwrap();
    oracle.insert_batch_indexed(oracle_rows(&log)).unwrap();
    compare_oracle(
        &log,
        &index,
        &oracle,
        &[
            AsOf::default(),
            AsOf {
                valid_at: 30,
                ..AsOf::default()
            },
            AsOf {
                known_at: 100,
                ..AsOf::default()
            },
            AsOf {
                from: 25,
                ..AsOf::default()
            },
            AsOf {
                valid_at: 10,
                ..AsOf::default()
            },
        ],
    );
}
#[test]
fn record_time_direct_nine_floor_pruning_and_long_id_ties() {
    let f = Fixture::new();
    let mut id = identity();
    id.first_id = 1_000_000_000_000_000_009;
    let mut log =
        RecordLog::open_clock(&f.root.join("authority"), &KEY, id, Arc::new(|| Ok(100))).unwrap();
    let mut rows = vec![];
    for i in 0..1200 {
        let text = if i < 4 {
            "lookup_needle battery electrode".into()
        } else if i % 3 == 0 {
            "battery electrode ".repeat(20)
        } else {
            format!("battery material{i} substrate ").repeat(15)
        };
        let mut row = input(&text, Some(&format!("key{i}")), (i % 7) as i64);
        if i % 17 == 0 {
            row.indexed = false;
        }
        rows.push(row);
    }
    for (i, chunk) in rows.chunks(500).enumerate() {
        log.append(&format!("large-{i}"), chunk).unwrap();
    }
    let index = Index::rebuild(&log).unwrap();
    let oracle = oojuro_nine::Nine::open(&f.root.join("oracle")).unwrap();
    oracle.insert_batch_indexed(oracle_rows(&log)).unwrap();
    compare_oracle(
        &log,
        &index,
        &oracle,
        &[
            AsOf::default(),
            AsOf {
                valid_at: 2,
                ..AsOf::default()
            },
            AsOf {
                from: 4,
                ..AsOf::default()
            },
            AsOf {
                known_at: 150,
                ..AsOf::default()
            },
        ],
    );
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}
#[test]
fn record_time_sealed_but_incomplete_or_extended_receipts_fail_closed_on_reopen() {
    use oojuro_store::{NewRecord, Options, Store, crypto::FrameCipher};
    let source = Fixture::new();
    let mut log = source.clock(100);
    log.append("first", &[input("battery", Some("key"), 1)])
        .unwrap();
    drop(log);
    let before = source.root.join("before-second");
    copy_dir(&source.root.join("authority"), &before);
    let mut log = source.clock(101);
    log.append("second", &[input("battery changed", Some("key"), 2)])
        .unwrap();
    drop(log);
    let options = || Options {
        cipher: Some(Arc::new(FrameCipher::new(&KEY))),
        require_sealed: true,
        sync: true,
        compact_threads: 1,
        ..Options::default()
    };
    let store = Store::open(&source.root.join("authority"), options()).unwrap();
    let good = store.get(2).unwrap();
    drop(store);
    for mutation in [
        "missing-clock",
        "missing-key",
        "unknown-field",
        "bad-version",
        "wrong-id",
        "bad-receipt",
        "changed-clock",
        "changed-input",
    ] {
        let f = Fixture::new();
        copy_dir(&before, &f.root.join("authority"));
        let mut store = Store::open(&f.root.join("authority"), options()).unwrap();
        let mut batch: serde_json::Value = serde_json::from_slice(&good).unwrap();
        match mutation {
            "missing-clock" => {
                batch["assigned"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("recorded_at");
            }
            "missing-key" => {
                batch["assigned"][0]["input"]
                    .as_object_mut()
                    .unwrap()
                    .remove("key");
            }
            "unknown-field" => {
                batch["assigned"][0]["input"]["future_visibility"] = serde_json::json!(false);
            }
            "bad-version" => batch["schema"] = serde_json::json!(999),
            "wrong-id" => batch["assigned"][0]["id"] = serde_json::json!(999),
            "bad-receipt" => {
                let v = batch["watermark"]["commitment"][0].as_u64().unwrap();
                batch["watermark"]["commitment"][0] = serde_json::json!(v ^ 1);
            }
            "changed-clock" => batch["assigned"][0]["recorded_at"] = serde_json::json!(102),
            "changed-input" => {
                batch["assigned"][0]["input"]["text"] =
                    serde_json::json!("changed battery preimage")
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&batch).unwrap();
        assert_eq!(
            store
                .append(&[NewRecord {
                    kind: 7,
                    valid_at: 0,
                    recorded_at: 0,
                    key: b"",
                    attrs: b"",
                    text: &bytes
                }])
                .unwrap(),
            2
        );
        drop(store);
        assert!(
            RecordLog::open(&f.root.join("authority"), &KEY, identity()).is_err(),
            "{mutation}"
        );
    }
}
#[test]
fn record_time_known_prefix_detects_same_origin_rollback_and_failed_projection() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    log.append("first", &[input("battery original", None, 1)])
        .unwrap();
    drop(log);
    let rollback = f.root.join("rollback");
    copy_dir(&f.root.join("authority"), &rollback);
    let mut log = f.clock(100);
    let mut index = Index::rebuild(&log).unwrap();
    log.hook = Some(Box::new(|p| {
        if matches!(p, authority::Phase::AfterStore) {
            panic!("synthetic durable append boundary")
        }
    }));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log.append("second", &[input("battery second", None, 2)])
    }));
    assert!(outcome.is_err());
    assert!(matches!(view_result(&log), Err(Error::Unavailable)));
    assert!(matches!(index.refresh(&log), Err(Error::Unavailable)));
    drop(log);
    let log = f.open();
    let current = view(&log);
    assert_eq!(
        current
            .range(&log, Versions::All, Filter::default(), Limits::default())
            .unwrap()
            .records
            .len(),
        2
    );
    assert!(
        index
            .search(&log, &query("battery", AsOf::default()))
            .is_err()
    );
    index.refresh(&log).unwrap();
    assert_eq!(
        index
            .search(&log, &query("battery", AsOf::default()))
            .unwrap()
            .hits
            .len(),
        2
    );
    let old = RecordLog::open(&rollback, &KEY, identity()).unwrap();
    assert!(matches!(
        current.get_id(&old, 9, 4096),
        Err(Error::Authority)
    ));
    assert!(
        index
            .search(&old, &query("battery", AsOf::default()))
            .is_err()
    );
}
fn view_result(log: &RecordLog) -> Result<View> {
    View::open(log, scope(), AsOf::default())
}

#[test]
fn record_time_stored_only_never_reenters_content_phrase_or_identifier_anchor() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    let old = input("the !!!", Some("versioned"), 10);
    let mut hidden = input("the !!! lookup_needle", Some("versioned"), 20);
    hidden.indexed = false;
    let mut only = input("the !!!", Some("stored-only"), 30);
    only.indexed = false;
    log.append(
        "eligibility",
        &[old, hidden, only, input("lookup needle battery", None, 40)],
    )
    .unwrap();
    let index = Index::rebuild(&log).unwrap();
    assert!(
        index
            .search(&log, &query("\"the\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(
        index
            .search(&log, &query("\"!!!\"", AsOf::default()))
            .unwrap()
            .hits
            .is_empty()
    );
    let ids = index
        .search(&log, &query("", AsOf::default()))
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.record.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![12, 11, 10]);
    for (key, id) in [("versioned", 10), ("stored-only", 11)] {
        assert_eq!(
            index
                .search(&log, &query(&format!("key:{key}"), AsOf::default()))
                .unwrap()
                .hits[0]
                .record
                .id,
            id
        );
        assert_eq!(view(&log).get_key(&log, key, 4096).unwrap().unwrap().id, id);
    }
    let hits = index
        .search(&log, &query("lookup_needle battery", AsOf::default()))
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, 12);
    assert!(
        !hits[0].continued,
        "stored-only identifier must not anchor the view"
    );
    let before = AsOf {
        valid_at: 15,
        ..AsOf::default()
    };
    assert_eq!(
        index.search(&log, &query("\"the\"", before)).unwrap().hits[0]
            .record
            .id,
        9
    );
}

#[test]
fn record_time_outputs_keep_header_bound_adapter_through_reopen() {
    let f=Fixture::new();let mut log=f.clock(100);
    let mut row=input("battery provenance",Some("key"),20);
    row.attrs.insert("adapter".into(),serde_json::json!("reported-override"));
    let ack=log.append("adapter-original",&[row]).unwrap();drop(log);
    let log=f.clock(100);let v=view(&log);let expected=identity().adapter;
    assert_eq!(v.get_id(&log,ack.ids[0],4096).unwrap().unwrap().adapter,expected);
    assert_eq!(v.get_key(&log,"key",4096).unwrap().unwrap().adapter,expected);
    assert_eq!(v.history(&log,"key",Limits::default()).unwrap().records[0].adapter,expected);
    let index=Index::rebuild(&log).unwrap();let found=index.search(&log,&query("battery",AsOf::default())).unwrap();
    assert_eq!(found.hits[0].record.adapter,expected);
    assert_eq!(found.hits[0].record.attrs["adapter"],serde_json::json!("reported-override"));
    let exported=v.export(&log,Versions::All,Filter::default(),Limits::default()).unwrap();
    let parsed:serde_json::Value=serde_json::from_slice(&exported).unwrap();
    assert_eq!(parsed["records"][0]["adapter"],serde_json::json!(expected));
    drop(log);let mut changed=identity();changed.adapter="different-bound-source".into();
    assert!(RecordLog::open(&f.root.join("authority"),&KEY,changed).is_err());
}

#[test]
fn counted_json_preserves_exact_escaped_wire_bounds() {
    let values = [
        serde_json::json!(null),
        serde_json::json!({"a": ["quote\"slash\\line\n", "λ🦀\u{0000}", "", "é"], "b": [0, -1, 1.25, true]}),
        serde_json::json!({"text": "λ\"\\\n".repeat(1024)}),
    ];
    for value in values {
        let expected = serde_json::to_vec(&value).unwrap();
        for limit in [0, 1, expected.len() - 1, expected.len(), expected.len() + 1] {
            assert_eq!(
                json(&value, limit).map(|bytes| bytes.len()),
                json_size(&value, limit)
            );
        }
        assert_eq!(json_size(&value, expected.len()).unwrap(), expected.len());
    }
}

#[test]
fn exact_id_lookup_matches_snapshot_scan_with_nondefault_ids_and_two_clocks() {
    let f = Fixture::new();
    let mut log = f.clock(100);
    log.append("first", &[input("first", None, 10), input("second", None, 20)])
        .unwrap();
    let old = view(&log);
    log.append("next", &[input("third", None, 30), input("fourth", None, 40)])
        .unwrap();
    let cases = [
        AsOf::default(),
        AsOf { from: 20, valid_at: 30, ..AsOf::default() },
        AsOf { known_at: 100, ..AsOf::default() },
        AsOf { known_at: 99, ..AsOf::default() },
    ];
    for selected in std::iter::once(old).chain(cases.into_iter().map(|as_of| {
        View::open(&log, scope(), as_of).unwrap()
    })) {
        let end = selected.watermark().records as usize;
        let at = selected.as_of();
        for id in [0, 8, 9, 10, 11, 12, 13, u64::MAX] {
            let expected = log.records[..end]
                .iter()
                .find(|row| row.id == id && row.input.valid_at >= at.from
                    && row.input.valid_at <= at.valid_at && row.recorded_at <= at.known_at)
                .map(|row| row.record(WRITER, &identity().adapter));
            assert_eq!(selected.get_id(&log, id, 4096).unwrap(), expected);
        }
    }
    let selected = view(&log);
    let row = selected.get_id(&log, 12, 4096).unwrap();
    let bytes = serde_json::to_vec(&row).unwrap().len();
    assert_eq!(selected.get_id(&log, 12, bytes).unwrap(), row);
    assert_eq!(selected.get_id(&log, 12, bytes - 1), Err(Error::Bounds));
    let other = Fixture::new();
    assert!(selected.get_id(&other.clock(100), 12, 4096).is_err());
}

#[test]
fn exact_id_lookup_preserves_large_header_id_and_reopen() {
    let f = Fixture::new();
    let identity = Identity { first_id: u64::MAX - 4, ..identity() };
    let mut log = RecordLog::open(&f.root.join("authority"), &KEY, identity.clone()).unwrap();
    let ack = log.append("large-id", &[input("large first battery", None, 10),
        input("large second battery", None, 20)]).unwrap();
    let originals: Vec<_> = ack.ids.iter().map(|id| view(&log).get_id(&log, *id, 4096).unwrap()).collect();
    let index = Index::rebuild(&log).unwrap();
    let searched = index.search(&log, &query("battery", AsOf::default())).unwrap();
    for hit in searched.hits {
        assert_eq!(Some(hit.record.clone()), originals.iter().flatten().find(|row| row.id == hit.record.id).cloned());
    }
    drop(log);
    let log = RecordLog::open(&f.root.join("authority"), &KEY, identity).unwrap();
    for (id, original) in ack.ids.iter().zip(originals) {
        assert_eq!(view(&log).get_id(&log, *id, 4096).unwrap(), original);
    }
    for absent in [0, u64::MAX - 5, u64::MAX - 2, u64::MAX] {
        assert!(view(&log).get_id(&log, absent, 4096).unwrap().is_none());
    }
}
