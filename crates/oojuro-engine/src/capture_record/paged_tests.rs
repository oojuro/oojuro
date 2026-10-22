use super::*;
use crate::record::{
    Versions,
    managed::{ManagedPagedLog, PruneLimits, ReadLimits},
};
impl Fixture {
    fn paged_control(&self) -> Arc<RetainedControl> {
        let witness = FileWitness::open(&self.root.join("paged-witness")).unwrap();
        let open = if self.root.join("paged-control/PAGED_CONTROL").exists() {
            RetainedControl::open_paged
        } else {
            RetainedControl::create_paged
        };
        Arc::new(
            open(
                &self.root.join("paged-control"),
                &CONTROL_KEY,
                Signer::from_pkcs8(AUTHORITY, Domain::NativeLifecycle, &self.signing).unwrap(),
                AUTHORITY,
                witness,
                managed::identity_binding(&DATA_KEY, &identity("capture-record-paged-v1", 900))
                    .unwrap(),
            )
            .unwrap(),
        )
    }
    fn paged_target(&self, control: Arc<RetainedControl>) -> ManagedPagedLog {
        let open = if self.root.join("paged-data").exists() {
            ManagedPagedLog::open
        } else {
            ManagedPagedLog::create
        };
        open(
            &self.root.join("paged-data"),
            &DATA_KEY,
            identity("capture-record-paged-v1", 900),
            control,
        )
        .unwrap()
    }
}
fn paged_run(
    s: &CaptureLog,
    d: &mut ManagedPagedLog,
    checkpoint: &Path,
    after: &mut dyn FnMut() -> Result<()>,
) -> Result<Report> {
    run_target(
        s,
        Target::Paged(d),
        scope(),
        checkpoint,
        Limits::default(),
        after,
    )
}
#[test]
fn paged_exact_replay_first_clock_real_pages_and_pruned_receipt_survive_reopen() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.paged_control();
    let mut d = f.paged_target(c.clone());
    let first = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), one()).unwrap();
    assert_eq!((first.new, first.more), (1, true));
    let original = d.get(900, 1 << 20).unwrap().unwrap();
    assert_eq!(original.valid_at, -500);
    assert!(original.recorded_at > 123);
    assert_eq!(original.attrs["capture"]["reported"]["id"], 41);
    let rest = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!(
        (rest.new, rest.withheld, rest.no_output, rest.more),
        (1, 1, 2, false)
    );
    assert!(!d.get(901, 1 << 20).unwrap().unwrap().indexed);
    let limits = ReadLimits {
        entries: 1,
        ..ReadLimits::default()
    };
    let page = d
        .read(
            scope(),
            AsOf::default(),
            &Filter::default(),
            Versions::All,
            None,
            limits,
        )
        .unwrap();
    assert_eq!(page.records[0].id, 900);
    let next = d
        .read(
            scope(),
            AsOf::default(),
            &Filter::default(),
            Versions::All,
            page.next.as_deref(),
            limits,
        )
        .unwrap();
    assert_eq!(next.records[0].id, 901);
    let historical = d
        .read(
            scope(),
            AsOf {
                known_at: original.recorded_at - 1,
                ..AsOf::default()
            },
            &Filter::default(),
            Versions::All,
            None,
            limits,
        )
        .unwrap();
    assert!(historical.records.is_empty());
    fs::remove_file(f.checkpoint()).unwrap();
    let replay = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 2));
    assert_eq!(replay.mappings[0].recorded_at, original.recorded_at);
    d.prune(
        "paged-bridge-prune",
        &Selection::Ids(vec![900]),
        PruneLimits::default(),
    )
    .unwrap();
    fs::remove_file(f.checkpoint()).unwrap();
    drop(d);
    drop(c);
    drop(s);
    let s = f.source();
    let mut d = f.paged_target(f.paged_control());
    let pruned = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!(
        (pruned.new, pruned.already_committed, pruned.removed),
        (0, 2, 1)
    );
    assert_eq!(pruned.mappings[0].canonical_id, 900);
    assert_eq!(pruned.mappings[0].recorded_at, original.recorded_at);
    assert!(pruned.mappings[0].receipt_only);
    assert!(d.get(900, 1 << 20).unwrap().is_none());
}
#[test]
fn paged_original_emission_after_real_compaction_and_payload_conflict() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let mut d = f.paged_target(f.paged_control());
    let first = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    let before = d.binding().unwrap();
    s.compact().unwrap();
    drop(s);
    let s = f.source();
    fs::remove_file(f.checkpoint()).unwrap();
    let replay = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), Limits::default()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 2));
    for (a, b) in first.mappings.iter().zip(&replay.mappings) {
        assert_eq!(
            (a.canonical_id, a.recorded_at),
            (b.canonical_id, b.recorded_at)
        );
    }
    let other = Fixture::new();
    let mut changed = other.source();
    capture_text(&mut changed, "synthetic changed same logical emission");
    assert_eq!(
        ingest_paged(&changed, &mut d, scope(), &other.checkpoint(), one()).err(),
        Some(Error::Target(record::Error::Conflict))
    );
    assert_eq!(d.binding().unwrap(), before);
    assert!(!other.checkpoint().exists());
}
#[test]
fn paged_scope_current_authority_and_cross_layout_checkpoint_are_genuine() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.paged_control();
    let mut d = f.paged_target(c.clone());
    assert_eq!(
        d.scoped_binding(Scope {
            writer: "other",
            owner: 7
        })
        .err(),
        Some(record::Error::Scope)
    );
    assert_eq!(
        d.scoped_binding(Scope {
            writer: WRITER,
            owner: 8
        })
        .err(),
        Some(record::Error::Scope)
    );
    let mut snapshot = f.target(f.control());
    ingest(&s, &mut snapshot, scope(), &f.checkpoint(), one()).unwrap();
    let before = d.binding().unwrap();
    assert_eq!(
        ingest_paged(&s, &mut d, scope(), &f.checkpoint(), one()).err(),
        Some(Error::Authority)
    );
    assert_eq!(d.binding().unwrap(), before);
    fs::remove_file(f.checkpoint()).unwrap();
    ingest_paged(&s, &mut d, scope(), &f.checkpoint(), one()).unwrap();
    assert_eq!(
        ingest(&s, &mut snapshot, scope(), &f.checkpoint(), one()).err(),
        Some(Error::Authority)
    );
    c.deny().unwrap();
    assert!(d.scoped_binding(scope()).is_err());
    assert!(ingest_paged(&s, &mut d, scope(), &f.checkpoint(), one()).is_err());
}
#[test]
fn paged_denial_after_append_and_unknown_ack_never_bless_progress() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let c = f.paged_control();
    let mut d = f.paged_target(c.clone());
    assert!(
        paged_run(&s, &mut d, &f.checkpoint(), &mut || {
            c.deny().unwrap();
            Ok(())
        })
        .is_err()
    );
    assert!(!f.checkpoint().exists());
    let other = Fixture::new();
    let mut s = other.source();
    capture(&mut s);
    let c = other.paged_control();
    let mut d = other.paged_target(c.clone());
    assert_eq!(
        paged_run(&s, &mut d, &other.checkpoint(), &mut || Err(
            Error::Checkpoint
        ))
        .err(),
        Some(Error::Checkpoint)
    );
    let original = d.get(900, 1 << 20).unwrap().unwrap();
    let pin = d.binding().unwrap();
    drop(d);
    drop(c);
    let mut d = other.paged_target(other.paged_control());
    let replay = ingest_paged(&s, &mut d, scope(), &other.checkpoint(), one()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 1));
    assert_eq!(d.binding().unwrap(), pin);
    assert_eq!(replay.mappings[0].recorded_at, original.recorded_at);
}
#[test]
fn paged_oversized_first_and_wrong_target_do_not_advance_or_append() {
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    let mut d = f.paged_target(f.paged_control());
    let before = d.binding().unwrap();
    assert_eq!(
        ingest_paged(
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
    ingest_paged(&s, &mut d, scope(), &f.checkpoint(), one()).unwrap();
    let other = Fixture::new();
    let mut wrong = other.paged_target(other.paged_control());
    let before = wrong.binding().unwrap();
    assert_eq!(
        ingest_paged(&s, &mut wrong, scope(), &f.checkpoint(), one()).err(),
        Some(Error::Authority)
    );
    assert_eq!(wrong.binding().unwrap(), before);
}
#[test]
fn actual_paged_sigkill_after_append_replays_original_first_clock() {
    const ENV: &str = "OOJURO_SYNTHETIC_PAGED_BRIDGE_CRASH_ROOT";
    if let Some(root) = std::env::var_os(ENV) {
        let root = PathBuf::from(root);
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("bridge-{}-", unsafe { libc::getppid() }))
        );
        let signing = fs::read(root.join("synthetic-signing.pk8")).unwrap();
        let f = std::mem::ManuallyDrop::new(Fixture { root, signing });
        let s = f.source();
        let mut d = f.paged_target(f.paged_control());
        let _ = paged_run(&s, &mut d, &f.checkpoint(), &mut || {
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            panic!("synthetic kill failed");
        });
        panic!("synthetic crash boundary not reached");
    }
    let f = Fixture::new();
    let mut s = f.source();
    capture(&mut s);
    drop(s);
    let d = f.paged_target(f.paged_control());
    drop(d);
    let path = f.root.join("synthetic-signing.pk8");
    let mut key = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    key.write_all(&f.signing).unwrap();
    key.sync_all().unwrap();
    drop(key);
    let mut child=std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact","capture_record::tests::paged::actual_paged_sigkill_after_append_replays_original_first_clock"])
        .env(ENV,&f.root).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("synthetic child timeout");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!f.checkpoint().exists());
    let s = f.source();
    let mut d = f.paged_target(f.paged_control());
    let original = d.get(900, 1 << 20).unwrap().unwrap();
    let replay = ingest_paged(&s, &mut d, scope(), &f.checkpoint(), one()).unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 1));
    assert_eq!(
        (
            replay.mappings[0].canonical_id,
            replay.mappings[0].recorded_at
        ),
        (original.id, original.recorded_at)
    );
    assert!(d.get(901, 1 << 20).unwrap().is_none());
}
