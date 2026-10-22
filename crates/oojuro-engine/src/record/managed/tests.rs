use super::*;
use crate::audit::{Domain, Signer, Verifier};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use std::sync::atomic::{AtomicU64, Ordering};
const DATA_KEY: [u8; 32] = [51; 32];
const CONTROL_KEY: [u8; 32] = [52; 32];
const AUTHORITY: [u8; 32] = [53; 32];
struct Fixture {
    root: PathBuf,
    pkcs8: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::current_dir()
            .unwrap()
            .join(".scratch")
            .join(format!(
                "managed-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap()
            .as_ref()
            .to_vec();
        write_new(&root.join("test-only-signing.pk8"), &pkcs8).unwrap();
        Self { root, pkcs8 }
    }
    fn signer(&self) -> Signer {
        Signer::from_pkcs8(AUTHORITY, Domain::NativeLifecycle, &self.pkcs8).unwrap()
    }
    fn control(&self) -> Arc<RetainedControl> {
        Arc::new(
            RetainedControl::open(
                &self.root.join("control"),
                &CONTROL_KEY,
                self.signer(),
                AUTHORITY,
                FileWitness::open(&self.root.join("witness")).unwrap(),
                identity_binding(&DATA_KEY, &identity()).unwrap(),
            )
            .unwrap(),
        )
    }
    fn log(&self, control: Arc<RetainedControl>) -> ManagedLog {
        if self.root.join("data").exists() {
            ManagedLog::open(&self.root.join("data"), &DATA_KEY, identity(), control).unwrap()
        } else {
            ManagedLog::create(&self.root.join("data"), &DATA_KEY, identity(), control).unwrap()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn identity() -> Identity {
    Identity {
        writer: "synthetic-managed-owner".into(),
        agent: 7,
        first_id: 9,
        adapter: "explicit-direct-fixture".into(),
    }
}
fn input(agent: &str, text: &str) -> Input {
    Input {
        agent_id: agent.into(),
        parent_id: None,
        valid_at: -123,
        kind: Kind::Explicit,
        tool: None,
        status: None,
        exit: None,
        key: None,
        text: text.into(),
        attrs: BTreeMap::new(),
        indexed: true,
    }
}
fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for item in fs::read_dir(source).unwrap() {
        let item = item.unwrap();
        let to = destination.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_tree(&item.path(), &to);
        } else {
            fs::copy(item.path(), to).unwrap();
        }
    }
}
#[test]
fn managed_exact_sparse_ids_times_retries_lineage_and_reopen() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    let rows = [
        input("synthetic-amber-agent", "synthetic amber private phrase"),
        input("synthetic-cobalt-agent", "synthetic cobalt private phrase"),
        input("synthetic-silver-agent", "synthetic silver private phrase"),
    ];
    let ack = log.append("batch", &rows).unwrap();
    let original = log.get(11, 4096).unwrap().unwrap();
    let binding = log.binding().unwrap();
    let (cache_binding, cache) = log.derived_directory().unwrap();
    assert_eq!(binding, cache_binding);
    fs::write(
        cache.join("RANK_TABLE"),
        log.cipher.seal_dict(b"test-cache", rows[1].text.as_bytes()),
    )
    .unwrap();
    let proof = log
        .prune("erase-cobalt", &Selection::Ids(vec![10]))
        .unwrap();
    assert!(proof.all_owned_prior_generations_removed);
    assert_eq!(proof.decoded_shared_occurrences, 0);
    assert!(!cache.exists());
    assert!(log.get(10, 4096).unwrap().is_none());
    let survivor = log.get(11, 4096).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(original).unwrap(),
        serde_json::to_value(survivor).unwrap()
    );
    assert!(log.validate_binding(&binding).is_err());
    let retry = log.append("batch", &rows).unwrap();
    assert_eq!(retry.commit, ack.commit);
    assert!(retry.receipt_only);
    assert_eq!(retry.removed_ids, vec![10]);
    let next = log
        .append(
            "next",
            &[input(
                "synthetic-gold-agent",
                "synthetic golden remaining record",
            )],
        )
        .unwrap();
    assert_eq!(next.commit.ids, vec![12]);
    let old_proof = log
        .prune("erase-cobalt", &Selection::Ids(vec![10]))
        .unwrap();
    assert!(old_proof.historical);
    assert!(!old_proof.local_live_absence_verified);
    drop(log);
    drop(control);
    let control = f.control();
    let mut reopened = f.log(control);
    assert!(reopened.get(10, 4096).unwrap().is_none());
    assert_eq!(reopened.append("batch", &rows).unwrap().commit, ack.commit);
    assert_eq!(
        reopened.get(11, 4096).unwrap().unwrap().recorded_at,
        ack.commit.recorded_at[2]
    );
    assert_eq!(reopened.get(9, 1).err(), Some(Error::Bounds));
    assert_eq!(
        reopened.finalize_permanent_destruction(),
        Err(Error::Authority)
    );
}
#[test]
fn denial_fences_already_open_owner_and_old_data_restore_but_not_copied_key() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "one",
        &[input(
            "synthetic-ruby-agent",
            "synthetic ruby kept in backup",
        )],
    )
    .unwrap();
    let backup = f.root.join("backup-data");
    copy_tree(&f.root.join("data"), &backup);
    let generation = log.snapshot.generation;
    let cp = control.deny().unwrap();
    Verifier::new(AUTHORITY, Domain::NativeLifecycle, f.signer().public_key())
        .verify_checkpoint(
            &cp.checkpoint,
            identity_binding(&DATA_KEY, &identity()).unwrap(),
        )
        .unwrap();
    assert!(log.get(9, 4096).is_err());
    assert!(log.binding().is_err());
    assert!(log.derived_directory().is_err());
    assert!(log.append("two", &[input("other", "new content")]).is_err());
    drop(log);
    assert!(ManagedLog::open(&backup, &DATA_KEY, identity(), control.clone()).is_err());
    // This successful primitive decrypt proves why a logical receipt cannot claim key destruction.
    let primitive = open_store(
        &backup.join(format!("g-{generation}")),
        Arc::new(FrameCipher::new(&DATA_KEY)),
    )
    .unwrap();
    assert!(!primitive.get(0).unwrap().is_empty());
}
#[test]
fn writer_cleanup_is_local_only_and_signed_pending_permanent() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "one",
        &[input(
            "synthetic-onyx-agent",
            "synthetic onyx erased locally",
        )],
    )
    .unwrap();
    let cp = log.deny_writer().unwrap();
    assert!(
        root_entries(&f.root.join("data"))
            .unwrap()
            .iter()
            .all(|p| !p.file_name().unwrap().to_str().unwrap().starts_with("g-"))
    );
    assert!(log.get(9, 4096).is_err());
    assert_eq!(log.finalize_permanent_destruction(), Err(Error::Authority));
    assert_eq!(
        cp.checkpoint.floor().unwrap(),
        control.current_floor().unwrap()
    );
}
#[test]
fn shared_short_strings_are_reported_not_falsely_proved_absent() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    log.append(
        "shared",
        &[
            input("shared-agent", "x"),
            input("shared-agent", "prefix x suffix"),
        ],
    )
    .unwrap();
    let report = log.prune("erase-one", &Selection::Ids(vec![9])).unwrap();
    assert!(report.decoded_shared_occurrences > 0);
    assert!(report.encoded_occurrences > 0); // one-byte scan is not silently excluded
    assert!(!report.local_live_absence_verified);
    assert!(log.get(9, 4096).unwrap().is_none());
    assert_eq!(log.get(10, 4096).unwrap().unwrap().text, "prefix x suffix");
}
#[test]
fn changed_retry_selector_and_unowned_files_fail_closed() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    let row = input("synthetic-indigo-agent", "synthetic indigo original");
    log.append("one", std::slice::from_ref(&row)).unwrap();
    assert_eq!(
        log.append("one", &[input("different", "changed")]).err(),
        Some(Error::Conflict)
    );
    let unowned = f.root.join("data/unowned.txt");
    fs::write(&unowned, b"synthetic unowned residue").unwrap();
    assert!(log.prune("erase", &Selection::Ids(vec![9])).is_err());
    assert!(log.get(9, 4096).unwrap().is_some());
    assert!(unowned.exists());
    fs::remove_file(unowned).unwrap();
    log.prune("erase", &Selection::Ids(vec![9])).unwrap();
    assert_eq!(
        log.prune("erase", &Selection::Agent("different".into()))
            .err(),
        Some(Error::Conflict)
    );
}
#[test]
fn old_control_restore_and_missing_witness_cannot_bootstrap() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "one",
        &[input("synthetic-jade-agent", "synthetic jade content")],
    )
    .unwrap();
    let backup = f.root.join("old-control");
    // A closed immutable copy can be captured while the authority owns its advisory lock.
    copy_tree(&f.root.join("control"), &backup);
    log.prune("erase", &Selection::Ids(vec![9])).unwrap();
    drop(log);
    drop(control);
    fs::remove_dir_all(f.root.join("control")).unwrap();
    copy_tree(&backup, &f.root.join("control"));
    assert!(
        RetainedControl::open(
            &f.root.join("control"),
            &CONTROL_KEY,
            f.signer(),
            AUTHORITY,
            FileWitness::open(&f.root.join("witness")).unwrap(),
            identity_binding(&DATA_KEY, &identity()).unwrap()
        )
        .is_err()
    );
    fs::remove_file(f.root.join("witness/CURRENT")).unwrap();
    assert!(
        RetainedControl::open(
            &f.root.join("control"),
            &CONTROL_KEY,
            f.signer(),
            AUTHORITY,
            FileWitness::open(&f.root.join("witness")).unwrap(),
            identity_binding(&DATA_KEY, &identity()).unwrap()
        )
        .is_err()
    );
}
#[test]
fn ciphertext_corruption_wrong_owner_and_second_data_owner_refuse() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "one",
        &[input("synthetic-pearl-agent", "synthetic pearl original")],
    )
    .unwrap();
    assert!(
        ManagedLog::open(&f.root.join("data"), &DATA_KEY, identity(), control.clone()).is_err()
    );
    let mut wrong = identity();
    wrong.writer = "foreign".into();
    assert!(
        ManagedLog::open(
            &f.root.join("foreign-data"),
            &DATA_KEY,
            wrong,
            control.clone()
        )
        .is_err()
    );
    let generation = log.snapshot.generation;
    drop(log);
    let hot = store_files(&f.root.join(format!("data/g-{generation}")))
        .unwrap()
        .into_iter()
        .find(|p| p.extension().is_some_and(|s| s == "ojl"))
        .unwrap();
    let mut bytes = fs::read(&hot).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(hot, bytes).unwrap();
    assert!(ManagedLog::open(&f.root.join("data"), &DATA_KEY, identity(), control).is_err());
}
#[test]
fn retention_uses_original_recorded_clock_not_claimed_valid_time() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    let one = log
        .append(
            "one",
            &[input("synthetic-topaz-agent", "synthetic topaz old")],
        )
        .unwrap();
    let two = log
        .append(
            "two",
            &[input("synthetic-opal-agent", "synthetic opal recent")],
        )
        .unwrap();
    log.prune(
        "retention",
        &Selection::RecordedBefore(two.commit.recorded_at[0]),
    )
    .unwrap();
    assert!(log.get(one.commit.ids[0], 4096).unwrap().is_none());
    assert!(log.get(two.commit.ids[0], 4096).unwrap().is_some());
}

#[test]
#[ignore = "subprocess helper invoked only by managed_sigkill_publication_boundaries"]
fn crash_worker() {
    let Ok(root) = std::env::var("OOJURO_MANAGED_CRASH_ROOT") else {
        return;
    };
    let phase = std::env::var("OOJURO_MANAGED_CRASH_PHASE").unwrap();
    let root = PathBuf::from(root);
    let pkcs8 = read_regular(&root.join("test-only-signing.pk8"), 4096).unwrap();
    let fixture = Fixture {
        root: root.clone(),
        pkcs8,
    };
    let control = fixture.control();
    let mut log = fixture.log(control.clone());
    let marker = root.join("stopped");
    let stop = move || {
        fs::write(&marker, b"ready").unwrap();
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
    };
    if phase == "control-append" {
        control.lock().unwrap().hook = Some(Arc::new(stop));
    } else {
        let wanted = match phase.as_str() {
            "stage" => Phase::AfterStage,
            "control" => Phase::AfterControl,
            "cleanup" => Phase::AfterCleanup,
            "ack" => Phase::BeforeAck,
            _ => panic!("invalid synthetic phase"),
        };
        log.hook = Some(Arc::new(move |at| {
            if at == wanted {
                stop();
            }
        }));
    }
    log.prune("crash-prune", &Selection::Ids(vec![9])).unwrap();
    panic!("worker should have been stopped");
}
#[test]
fn managed_sigkill_publication_boundaries() {
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    for phase in ["stage", "control-append", "control", "cleanup", "ack"] {
        let f = Fixture::new();
        let control = f.control();
        let mut log = f.log(control.clone());
        let input = input(
            "synthetic-crash-owner",
            "synthetic crash original kept only if uncommitted",
        );
        let ack = log
            .append("original", std::slice::from_ref(&input))
            .unwrap();
        drop(log);
        drop(control);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "record::managed::tests::crash_worker",
                "--nocapture",
            ])
            .env("OOJURO_MANAGED_CRASH_ROOT", &f.root)
            .env("OOJURO_MANAGED_CRASH_PHASE", phase)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f.root.join("stopped").exists() {
            if child.try_wait().unwrap().is_some() {
                panic!("synthetic worker exited before boundary");
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("synthetic crash boundary timed out");
            }
            thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap(); // Unix Child::kill sends SIGKILL to the actually stopped process.
        let status = child.wait().unwrap();
        assert!(!status.success());
        let control = f.control();
        let mut log = f.log(control);
        if phase == "stage" {
            assert!(log.get(9, 4096).unwrap().is_some());
        } else {
            assert!(log.get(9, 4096).unwrap().is_none());
        }
        if phase == "control" {
            assert_eq!(
                log.prune("different-pending-prune", &Selection::Ids(vec![9]))
                    .err(),
                Some(Error::Unavailable)
            );
        }
        let proof = log.prune("crash-prune", &Selection::Ids(vec![9])).unwrap();
        assert!(proof.all_owned_prior_generations_removed);
        assert!(
            log.prune("allowed-after-recovery", &Selection::Ids(vec![9]))
                .is_ok()
        );
        assert!(log.get(9, 4096).unwrap().is_none());
        let replay = log
            .append("original", std::slice::from_ref(&input))
            .unwrap();
        assert_eq!(replay.commit, ack.commit);
        assert!(replay.receipt_only);
    }
}
#[test]
fn overflow_and_generation_cache_registry_fail_before_false_ack() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.snapshot.next_id = u64::MAX;
    assert!(log.append("overflow", &[input("agent", "value")]).is_err());
    // Restore the untouched durable snapshot, proving failed assignment didn't commit.
    drop(log);
    let mut log = f.log(control);
    assert_eq!(
        log.append(
            "one",
            &[input("synthetic-cache-owner", "synthetic cache original")]
        )
        .unwrap()
        .commit
        .ids,
        vec![9]
    );
    let (_, directory) = log.derived_directory().unwrap();
    fs::write(
        directory.join("unregistered-file"),
        b"synthetic private residue",
    )
    .unwrap();
    assert!(log.prune("erase", &Selection::Ids(vec![9])).is_err());
    assert!(directory.join("unregistered-file").exists());
    // Logical removal already committed, but no verified cleanup receipt escaped.
    assert!(log.get(9, 4096).unwrap().is_none());
    assert!(log.append("blocked", &[input("other", "another")]).is_err());
    fs::remove_file(directory.join("unregistered-file")).unwrap();
    assert!(
        log.prune("erase", &Selection::Ids(vec![9]))
            .unwrap()
            .all_owned_prior_generations_removed
    );
}

#[test]
fn public_lifecycle_receipt_binds_counts_ids_scope_range_and_cipher_commitment() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    let one = log
        .append(
            "one",
            &[input(
                "synthetic-bounded-owner",
                "synthetic bounded private payload",
            )],
        )
        .unwrap();
    let report = log
        .prune(
            "retention",
            &Selection::RecordedBefore(one.commit.recorded_at[0] + 1),
        )
        .unwrap();
    let receipt = &report.signed_receipt;
    let verifier = Verifier::new(AUTHORITY, Domain::NativeLifecycle, f.signer().public_key());
    let scope = identity_binding(&DATA_KEY, &identity()).unwrap();
    receipt.verify(&verifier, scope).unwrap();
    receipt.verify_record_ids(&report.removed_ids).unwrap();
    assert!(receipt.verify_record_ids(&[10]).is_err());
    assert!(receipt.verify(&verifier, [0; 32]).is_err());
    for field in 0..8 {
        let mut bad = receipt.clone();
        match field {
            0 => bad.event.records += 1,
            1 => bad.event.bytes += 1,
            2 => bad.event.before[0] ^= 1,
            3 => bad.control_ciphertext_digest[0] ^= 1,
            4 => bad.details.recorded_before = Some(0),
            5 => bad.details.record_ids = Some([1; 32]),
            6 => bad.details.decoded_occurrences += 1,
            _ => bad.details.encoded_occurrences += 1,
        }
        assert!(bad.verify(&verifier, scope).is_err());
    }
    let decoded = LifecycleReceipt::decode(&receipt.encode().unwrap()).unwrap();
    decoded.verify(&verifier, scope).unwrap();
    assert_eq!(
        decoded.details.recorded_before,
        Some(one.commit.recorded_at[0] + 1)
    );
}

#[test]
fn root_marker_is_inside_complete_live_byte_scan_scope() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    log.append(
        "root-marker-overlap",
        &[input("unique-test-marker-agent", "oojuro")],
    )
    .unwrap();
    let proof = log
        .prune("erase-marker-overlap", &Selection::Ids(vec![9]))
        .unwrap();
    assert!(proof.encoded_occurrences > 0);
    assert_eq!(
        proof.encoded_occurrences,
        proof.signed_receipt.details.encoded_occurrences
    );
    assert!(!proof.local_live_absence_verified);
}

#[test]
fn data_and_retained_control_encryption_keys_must_be_distinct() {
    let f = Fixture::new();
    let control = Arc::new(
        RetainedControl::open(
            &f.root.join("control"),
            &DATA_KEY,
            f.signer(),
            AUTHORITY,
            FileWitness::open(&f.root.join("witness")).unwrap(),
            identity_binding(&DATA_KEY, &identity()).unwrap(),
        )
        .unwrap(),
    );
    assert!(matches!(
        ManagedLog::create(&f.root.join("data"), &DATA_KEY, identity(), control),
        Err(Error::Scope)
    ));
}

#[test]
fn typed_attribute_payload_residue_prevents_false_local_absence() {
    for payload in [
        serde_json::json!(123456789),
        serde_json::json!([false, [true, null, 987654321]]),
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({}),
    ] {
        let f = Fixture::new();
        let mut log = f.log(f.control());
        let mut removed = input("removed-typed-metadata-agent", "");
        removed
            .attrs
            .insert("removed-only-field".into(), payload.clone());
        let mut survivor = input("kept-typed-metadata-agent", "safe");
        survivor
            .attrs
            .insert("different-kept-field".into(), payload);
        log.append("typed-attributes", &[removed, survivor])
            .unwrap();
        let report = log.prune("remove-typed", &Selection::Ids(vec![9])).unwrap();
        assert!(
            report.decoded_shared_occurrences > 0,
            "typed shared payload was skipped"
        );
        assert!(!report.local_live_absence_verified);
    }
}

#[test]
fn existing_or_denied_data_cannot_reenroll_under_a_fresh_control_root() {
    for denied in [false, true] {
        let f = Fixture::new();
        let original = f.control();
        let mut log = f.log(original.clone());
        log.append(
            "original-root",
            &[input(
                "synthetic-enrolled-owner",
                "synthetic original acknowledged data",
            )],
        )
        .unwrap();
        if denied {
            log.deny_writer().unwrap();
        }
        drop(log);
        let fresh = Arc::new(
            RetainedControl::open(
                &f.root.join("wrong-control"),
                &CONTROL_KEY,
                f.signer(),
                AUTHORITY,
                FileWitness::open(&f.root.join("wrong-witness")).unwrap(),
                identity_binding(&DATA_KEY, &identity()).unwrap(),
            )
            .unwrap(),
        );
        assert!(
            ManagedLog::open(&f.root.join("data"), &DATA_KEY, identity(), fresh).is_err(),
            "existing data was silently reenrolled under a fresh authority"
        );
        if !denied {
            assert_eq!(
                f.log(original).get(9, 4096).unwrap().unwrap().text,
                "synthetic original acknowledged data"
            );
        }
    }
}

#[test]
fn missing_enrollment_never_bootstraps_existing_data_or_empty_marker() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "original",
        &[input(
            "synthetic-missing-enrollment",
            "synthetic immutable original",
        )],
    )
    .unwrap();
    drop(log);
    fs::remove_file(f.root.join("data/ENROLLMENT")).unwrap();
    assert!(
        ManagedLog::open(&f.root.join("data"), &DATA_KEY, identity(), control.clone()).is_err()
    );
    assert!(ManagedLog::create(&f.root.join("data"), &DATA_KEY, identity(), control).is_err());

    let g = Fixture::new();
    let control = g.control();
    directory_create(&g.root.join("partial-data")).unwrap();
    write_new(&g.root.join("partial-data/MANAGED_FORMAT"), FORMAT).unwrap();
    assert!(
        ManagedLog::open(&g.root.join("partial-data"), &DATA_KEY, identity(), control).is_err()
    );
}
