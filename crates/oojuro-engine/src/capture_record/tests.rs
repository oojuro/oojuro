use super::*;
use crate::{
    audit::{Domain, Signer},
    capture_log::{Batch, Line, LineKind, Outcome, ParsedBatch, PolicyReceipt},
    record::managed::{self, FileWitness, RetainedControl, Selection},
};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const SOURCE_KEY: [u8; 32] = [61; 32];
const DATA_KEY: [u8; 32] = [62; 32];
const CONTROL_KEY: [u8; 32] = [63; 32];
const AUTHORITY: [u8; 32] = [64; 32];
const WRITER: &str = "synthetic-capture-owner";
fn identity(adapter: &str, first_id: u64) -> Identity {
    Identity {
        writer: WRITER.into(),
        agent: 7,
        first_id,
        adapter: adapter.into(),
    }
}
struct Fixture {
    root: PathBuf,
    signing: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::current_dir()
            .unwrap()
            .join(".scratch")
            .join(format!(
                "bridge-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("progress"))
            .unwrap();
        Self {
            root,
            signing: Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref()
                .to_vec(),
        }
    }
    fn source(&self) -> CaptureLog {
        CaptureLog::open(
            &self.root.join("capture"),
            &SOURCE_KEY,
            identity("synthetic-capture1", 41),
        )
        .unwrap()
    }
    fn control(&self) -> Arc<RetainedControl> {
        Arc::new(
            RetainedControl::open(
                &self.root.join("control"),
                &CONTROL_KEY,
                Signer::from_pkcs8(AUTHORITY, Domain::NativeLifecycle, &self.signing).unwrap(),
                AUTHORITY,
                FileWitness::open(&self.root.join("witness")).unwrap(),
                managed::identity_binding(&DATA_KEY, &identity("capture-record-bridge-v1", 900))
                    .unwrap(),
            )
            .unwrap(),
        )
    }
    fn target(&self, c: Arc<RetainedControl>) -> ManagedLog {
        if self.root.join("data").exists() {
            ManagedLog::open(
                &self.root.join("data"),
                &DATA_KEY,
                identity("capture-record-bridge-v1", 900),
                c,
            )
            .unwrap()
        } else {
            ManagedLog::create(
                &self.root.join("data"),
                &DATA_KEY,
                identity("capture-record-bridge-v1", 900),
                c,
            )
            .unwrap()
        }
    }
    fn checkpoint(&self) -> PathBuf {
        self.root.join("progress/CURRENT")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 7,
    }
}
fn event(id: u64, start: u64, end: u64, indexed: bool, text: &str) -> Event {
    Event {
        id,
        session: 11,
        parent_session: 0,
        agent: 7,
        turn: 2,
        seq: id as u32,
        parent: 0,
        revises: 0,
        kind: 3,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: -500,
        duration_us: 0,
        tool: String::new(),
        args: r#"{"rt":123,"reported":"synthetic"}"#.into(),
        text: text.into(),
        indexed,
        line_start: start,
        line_end: end,
        emission: 0,
    }
}
fn capture(log: &mut CaptureLog) {
    capture_text(log, "synthetic amber visible");
}
fn capture_text(log: &mut CaptureLog, text: &str) {
    let source = [81; 16];
    log.register(source, b"private synthetic descriptor", b"{}")
        .unwrap();
    let raw = b"allowed\nwithheld\nstored\n\nbroken\n";
    let rows = vec![
        event(41, 0, 8, true, text),
        event(42, 17, 24, false, "synthetic hidden stored"),
    ];
    let lines = vec![
        Line {
            start: 0,
            end: 8,
            kind: LineKind::Parsed,
            emissions: vec![Decision::Stored {
                id: 41,
                indexed: true,
            }],
        },
        Line {
            start: 8,
            end: 17,
            kind: LineKind::Parsed,
            emissions: vec![Decision::Withheld {
                reason: "synthetic-policy".into(),
            }],
        },
        Line {
            start: 17,
            end: 24,
            kind: LineKind::Parsed,
            emissions: vec![Decision::Stored {
                id: 42,
                indexed: false,
            }],
        },
        Line {
            start: 24,
            end: 25,
            kind: LineKind::Blank,
            emissions: vec![],
        },
        Line {
            start: 25,
            end: 32,
            kind: LineKind::Malformed,
            emissions: vec![],
        },
    ];
    log.commit(Batch {
        raw: raw.to_vec(),
        parsed: ParsedBatch {
            source,
            raw_start: 0,
            parsed_before: 0,
            outcome: Outcome {
                raw_end: raw.len() as u64,
                parsed_end: raw.len() as u64,
                first_id: Some(41),
                stored: 2,
                next_id: 43,
            },
            policy: PolicyReceipt {
                policy_id: "synthetic-policy-v1".into(),
                raw_authorization: "explicit-local-only".into(),
                lines,
            },
            context: b"{}".to_vec(),
            events: rows,
        },
    })
    .unwrap();
}
fn one() -> Limits {
    Limits {
        receipts: 1,
        bytes: 32 << 20,
    }
}
#[test]
fn bounded_progress_loss_replays_original_ids_times_and_pruned_receipts_without_resurrection() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.control();
    let mut d = f.target(c.clone());
    let first = ingest(&s, &mut d, scope(), &f.checkpoint(), one()).unwrap();
    assert_eq!((first.scanned, first.new, first.more), (1, 1, true));
    let id = first.mappings[0].canonical_id;
    let at = first.mappings[0].recorded_at;
    assert!(at > 123);
    let row = d.get(id, 1 << 20).unwrap().unwrap();
    assert_eq!(row.valid_at, -500);
    assert!(row.key.is_none());
    assert_eq!(row.attrs["capture"]["reported"]["id"], 41);
    assert_eq!(
        row.attrs["capture"]["reported"]["args"],
        r#"{"rt":123,"reported":"synthetic"}"#
    );
    let rewound = fs::read(f.checkpoint()).unwrap();
    let rest = ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!(
        (rest.withheld, rest.no_output, rest.new, rest.more),
        (1, 2, 1, false)
    );
    assert!(!d.get(901, 1 << 20).unwrap().unwrap().indexed);
    fs::remove_file(f.checkpoint()).unwrap();
    let replay = ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 2));
    assert_eq!(replay.mappings[0].canonical_id, id);
    assert_eq!(replay.mappings[0].recorded_at, at);
    d.prune("synthetic-prune", &Selection::Ids(vec![id]))
        .unwrap();
    fs::remove_file(f.checkpoint()).unwrap();
    let removed = ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!(
        (removed.new, removed.already_committed, removed.removed),
        (0, 2, 1)
    );
    assert!(removed.mappings[0].receipt_only);
    assert!(d.get(id, 1 << 20).unwrap().is_none());
    drop(d);
    drop(c);
    drop(s);
    let s = f.source();
    let c = f.control();
    let mut d = f.target(c);
    fs::write(f.checkpoint(), rewound).unwrap();
    let after = ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!((after.new, after.already_committed), (0, 1));
    assert!(d.get(id, 1 << 20).unwrap().is_none());
}
#[test]
fn unknown_ack_before_progress_survives_reopen_without_second_append_or_clock() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.control();
    let mut d = f.target(c.clone());
    assert_eq!(
        run(
            &s,
            &mut d,
            scope(),
            &f.checkpoint(),
            Limits::default(),
            &mut || Err(Error::Checkpoint)
        )
        .err(),
        Some(Error::Checkpoint)
    );
    assert!(!f.checkpoint().exists());
    let original = d.get(900, 1 << 20).unwrap().unwrap();
    let binding = d.binding().unwrap();
    drop(d);
    drop(c);
    drop(s);
    let s = f.source();
    let c = f.control();
    let mut d = f.target(c);
    let replay = ingest(&s, &mut d, scope(), &f.checkpoint(), one()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 1));
    assert_eq!(d.binding().unwrap(), binding);
    assert_eq!(
        serde_json::to_value(d.get(900, 1 << 20).unwrap().unwrap()).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}
#[test]
fn ancestor_prefix_checkpoint_allows_new_capture_appends_but_rejects_wrong_target_and_corruption() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.control();
    let mut d = f.target(c);
    ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    let source = [82; 16];
    s.register(source, b"next source", b"{}").unwrap();
    let e = event(43, 0, 5, true, "synthetic next");
    s.commit(Batch {
        raw: b"next\n".to_vec(),
        parsed: ParsedBatch {
            source,
            raw_start: 0,
            parsed_before: 0,
            outcome: Outcome {
                raw_end: 5,
                parsed_end: 5,
                first_id: Some(43),
                stored: 1,
                next_id: 44,
            },
            policy: PolicyReceipt {
                policy_id: "p2".into(),
                raw_authorization: "local-only".into(),
                lines: vec![Line {
                    start: 0,
                    end: 5,
                    kind: LineKind::Parsed,
                    emissions: vec![Decision::Stored {
                        id: 43,
                        indexed: true,
                    }],
                }],
            },
            context: b"{}".to_vec(),
            events: vec![e],
        },
    })
    .unwrap();
    let next = ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!((next.scanned, next.new), (1, 1));
    let other = Fixture::new();
    let c = other.control();
    let mut wrong = other.target(c);
    assert_eq!(
        ingest(&s, &mut wrong, scope(), &f.checkpoint(), Limits::default()).err(),
        Some(Error::Authority)
    );
    let mut bytes = fs::read(f.checkpoint()).unwrap();
    bytes[30] ^= 1;
    fs::write(f.checkpoint(), bytes).unwrap();
    assert_eq!(
        ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).err(),
        Some(Error::Checkpoint)
    );
}
#[test]
fn oversized_first_and_unsafe_checkpoint_leave_no_progress_or_target_append() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.control();
    let mut d = f.target(c);
    let before = d.binding().unwrap();
    assert_eq!(
        ingest(
            &s,
            &mut d,
            scope(),
            &f.checkpoint(),
            Limits {
                receipts: 1,
                bytes: 1
            }
        )
        .err(),
        Some(Error::Bounds)
    );
    assert_eq!(d.binding().unwrap(), before);
    assert!(!f.checkpoint().exists());
    fs::set_permissions(f.root.join("progress"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).err(),
        Some(Error::Checkpoint)
    );
    assert_eq!(d.binding().unwrap(), before);
}
#[test]
fn denial_after_committed_append_refuses_progress_and_later_bridge() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.control();
    let mut d = f.target(c.clone());
    assert!(
        run(&s, &mut d, scope(), &f.checkpoint(), one(), &mut || {
            c.deny().unwrap();
            Ok(())
        })
        .is_err()
    );
    assert!(!f.checkpoint().exists());
    assert!(ingest(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).is_err());
}
#[test]
fn all_kind_status_codes_and_oversized_metadata_are_explicit() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let batch = &s.batches().unwrap()[0];
    let kinds = [
        Kind::System,
        Kind::Input,
        Kind::Thinking,
        Kind::Output,
        Kind::ToolCall,
        Kind::ToolResult,
        Kind::Explicit,
    ];
    for (code, kind) in kinds.into_iter().enumerate() {
        let mut e = batch.events[0].clone();
        e.kind = code as u8;
        for (status, mapped) in [(0, None), (2, Some(Status::Ok)), (3, Some(Status::Error))] {
            e.status = status;
            let row = input(s.identity(), batch, &e).unwrap();
            assert_eq!(row.kind, kind);
            assert_eq!(row.status, mapped);
        }
    }
    let mut e = batch.events[0].clone();
    e.status = 1;
    assert_eq!(input(s.identity(), batch, &e).err(), Some(Error::Source));
    e.status = 0;
    e.args = "synthetic".repeat(1024);
    assert_eq!(input(s.identity(), batch, &e).err(), Some(Error::Bounds));
}

#[test]
fn actual_process_kill_after_append_replays_first_canonical_ingestion() {
    const ENV: &str = "OOJURO_SYNTHETIC_BRIDGE_CRASH_ROOT";
    if let Some(root) = std::env::var_os(ENV) {
        let root = PathBuf::from(root);
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("bridge-{}-", unsafe { libc::getppid() }))
        );
        let signing = fs::read(root.join("synthetic-signing.pk8")).unwrap();
        // The parent alone owns cleanup, including when a child assertion fails.
        let f = std::mem::ManuallyDrop::new(Fixture { root, signing });
        let source = f.source();
        let mut destination = f.target(f.control());
        let _ = run(
            &source,
            &mut destination,
            scope(),
            &f.checkpoint(),
            one(),
            &mut || {
                // Real SIGKILL after the destination fsynced, before any progress save.
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
                panic!("synthetic process kill did not terminate");
            },
        );
        panic!("synthetic crash boundary not reached");
    }
    let f = Fixture::new();
    let mut source = f.source();
    capture(&mut source);
    drop(source);
    let destination = f.target(f.control());
    drop(destination);
    let mut signing = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(f.root.join("synthetic-signing.pk8"))
        .unwrap();
    signing.write_all(&f.signing).unwrap();
    signing.sync_all().unwrap();
    drop(signing);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "capture_record::tests::actual_process_kill_after_append_replays_first_canonical_ingestion"])
        .env(ENV, &f.root)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("synthetic child exceeded bound before reaching crash boundary");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!f.checkpoint().exists());
    let source = f.source();
    let mut destination = f.target(f.control());
    let original = destination.get(900, 1 << 20).unwrap().unwrap();
    let replay = ingest(&source, &mut destination, scope(), &f.checkpoint(), one()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 1));
    assert_eq!(replay.mappings[0].canonical_id, original.id);
    assert_eq!(replay.mappings[0].recorded_at, original.recorded_at);
    assert!(destination.get(901, 1 << 20).unwrap().is_none());
}

#[test]
fn compaction_and_missing_progress_preserve_durable_emission_identity() {
    let f = Fixture::new();
    let mut source = f.source();
    capture(&mut source);
    let mut destination = f.target(f.control());
    let original = ingest(
        &source,
        &mut destination,
        scope(),
        &f.checkpoint(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(original.new, 2);
    let before = destination.binding().unwrap();
    fs::remove_file(f.checkpoint()).unwrap();
    source.compact().unwrap();
    drop(source);
    let source = f.source();
    let replay = ingest(
        &source,
        &mut destination,
        scope(),
        &f.checkpoint(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 2));
    assert_eq!(destination.binding().unwrap(), before);
    for (first, retried) in original.mappings.iter().zip(&replay.mappings) {
        assert_eq!(
            (first.canonical_id, first.recorded_at),
            (retried.canonical_id, retried.recorded_at)
        );
    }
}

#[test]
fn reused_key_identity_and_generation_are_same_source_and_changed_payload_conflicts() {
    let first = Fixture::new();
    let mut source = first.source();
    capture(&mut source);
    let mut destination = first.target(first.control());
    ingest(
        &source,
        &mut destination,
        scope(),
        &first.checkpoint(),
        one(),
    )
    .unwrap();
    let before = destination.binding().unwrap();
    let other = Fixture::new();
    let mut reused = other.source();
    capture_text(&mut reused, "synthetic changed same logical emission");
    assert_eq!(
        source.ingest_binding().unwrap(),
        reused.ingest_binding().unwrap()
    );
    assert_eq!(
        ingest(
            &reused,
            &mut destination,
            scope(),
            &other.checkpoint(),
            one()
        )
        .err(),
        Some(Error::Target(record::Error::Conflict))
    );
    assert_eq!(destination.binding().unwrap(), before);
    assert!(!other.checkpoint().exists());
}

#[path = "paged_tests.rs"]
mod paged;
