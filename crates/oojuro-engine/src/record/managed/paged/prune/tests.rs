use super::*;
use crate::audit::{Domain, Signer, Verifier};
use crate::record::managed::paged::driver::tests::{control, log};
use crate::record::managed::tests::{Fixture, identity, input};
fn limits() -> PruneLimits {
    PruneLimits {
        bytes: 1 << 30,
        rows: 1_000_000,
    }
}
#[test]
fn actual_streamed_rewrite_preserves_survivors_retries_and_signed_opaque_ids() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    let original = [
        input("removed-agent", "unique removed private phrase"),
        input("kept-agent", "surviving original"),
    ];
    let ack = l.append("original", &original).unwrap();
    let before = l.get(10, 4096).unwrap().unwrap();
    let proof = l
        .prune("erase", &Selection::Ids(vec![9]), limits())
        .unwrap();
    assert_eq!(proof.selected_count, 1);
    assert!(proof.all_owned_prior_namespaces_removed);
    proof.signed_receipt.verify_record_ids(&[9]).unwrap();
    proof
        .signed_receipt
        .verify(
            &Verifier::new([53; 32], Domain::NativeLifecycle, f.signer().public_key()),
            identity_binding(&[51; 32], &identity()).unwrap(),
        )
        .unwrap();
    assert!(l.get(9, 4096).unwrap().is_none());
    assert_eq!(
        json(&l.get(10, 4096).unwrap().unwrap(), 4096).unwrap(),
        json(&before, 4096).unwrap()
    );
    let replay = l.append("original", &original).unwrap();
    assert_eq!(replay.commit, ack.commit);
    assert_eq!(replay.removed_ids, vec![9]);
    assert!(replay.receipt_only);
    let tail = l.append("tail", &[input("tail", "new original")]).unwrap();
    assert_eq!(tail.commit.ids, vec![11]);
    let historical = l
        .prune("erase", &Selection::Ids(vec![9]), limits())
        .unwrap();
    assert!(historical.historical);
    assert_eq!(
        historical.signed_receipt.checkpoint,
        proof.signed_receipt.checkpoint
    );
    assert!(
        l.prune("erase", &Selection::Ids(vec![10]), limits())
            .is_err()
    );
}
#[test]
fn typed_shared_containers_and_metadata_prevent_false_local_absence() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c, true);
    let mut removed = input("removed", "");
    removed
        .attrs
        .insert("removed-field".into(), serde_json::json!({"": [""]}));
    let mut kept = input("kept", "safe");
    kept.attrs
        .insert("different-field".into(), serde_json::json!({"": [""]}));
    l.append("attrs", &[removed, kept]).unwrap();
    let p = l
        .prune("attr-prune", &Selection::Ids(vec![9]), limits())
        .unwrap();
    assert!(p.decoded_shared_occurrences > 0);
    assert!(!p.local_live_absence_verified);
}
#[test]
fn work_refusal_foreign_artifact_and_selection_conflict_never_publish_false_receipt() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c, true);
    l.append("one", &[input("removed", "first"), input("kept", "second")])
        .unwrap();
    let before = l.binding().unwrap();
    assert!(
        l.prune(
            "bounded",
            &Selection::Ids(vec![9]),
            PruneLimits { bytes: 1, rows: 1 }
        )
        .is_err()
    );
    assert_eq!(l.binding().unwrap(), before);
    assert!(l.get(9, 4096).unwrap().is_some());
    fs::write(
        l.directory.join("unrecognized"),
        b"private unmanaged residue",
    )
    .unwrap();
    assert!(
        l.prune("foreign", &Selection::Ids(vec![9]), limits())
            .is_err()
    );
    assert_eq!(l.binding().unwrap(), before);
    fs::remove_file(l.directory.join("unrecognized")).unwrap();
    l.prune("good", &Selection::Ids(vec![9]), limits()).unwrap();
    assert!(l.get(9, 4096).unwrap().is_none());
}
#[test]
fn orphan_and_partial_stages_are_retired_but_current_live_bytes_remain_exact() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c, true);
    l.append("one", &[input("removed", "one"), input("kept", "two")])
        .unwrap();
    let owner = l.control.lock().unwrap();
    let root = schema::load(&l.control.directory, owner.control_cipher(), &owner.state).unwrap();
    let old = data_directory(&l.directory, &root).unwrap();
    drop(owner);
    fs::write(
        old.join("packs")
            .join(format!("data-{}.stage", "ab".repeat(32))),
        b"partial old stage",
    )
    .unwrap();
    let p = l
        .prune("erase", &Selection::Ids(vec![9]), limits())
        .unwrap();
    assert!(p.all_owned_prior_namespaces_removed);
    assert!(!old.exists());
    assert_eq!(l.get(10, 4096).unwrap().unwrap().text, "two");
}
#[test]
#[ignore = "subprocess helper invoked by paged_sigkill_publication_and_cleanup"]
fn crash_worker() {
    let Ok(root) = std::env::var("OOJURO_PAGED_CRASH_ROOT") else {
        return;
    };
    let phase = std::env::var("OOJURO_PAGED_CRASH_PHASE").unwrap();
    let root = PathBuf::from(root);
    let signer = Signer::from_pkcs8(
        [53; 32],
        Domain::NativeLifecycle,
        &read_regular(&root.join("test-only-signing.pk8"), 4096).unwrap(),
    )
    .unwrap();
    let c = Arc::new(
        RetainedControl::open_paged(
            &root.join("paged-control"),
            &[52; 32],
            signer,
            [53; 32],
            FileWitness::open(&root.join("paged-witness")).unwrap(),
            identity_binding(&[51; 32], &identity()).unwrap(),
        )
        .unwrap(),
    );
    let mut l =
        ManagedPagedLog::open(&root.join("paged-data"), &[51; 32], identity(), c.clone()).unwrap();
    let stop = move || {
        fs::write(root.join("stopped"), b"ready").unwrap();
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
    };
    if phase == "control-object" {
        c.lock().unwrap().hook = Some(Arc::new(stop));
    } else {
        let wanted = match phase.as_str() {
            "stage" => Phase::AfterStage,
            "control" => Phase::AfterControl,
            "cleanup" => Phase::AfterCleanup,
            "ack" => Phase::BeforeAck,
            _ => panic!("invalid fixture phase"),
        };
        l.hook = Some(Arc::new(move |at| {
            if at == wanted {
                stop()
            }
        }));
    }
    l.prune("crash-prune", &Selection::Ids(vec![9]), limits())
        .unwrap();
    panic!("fixture should stop");
}
#[test]
fn paged_sigkill_publication_and_cleanup() {
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    for phase in ["stage", "control-object", "control", "cleanup", "ack"] {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c.clone(), true);
        let rows = [
            input("removed", "crash first original"),
            input("kept", "survives crash"),
        ];
        let ack = l.append("original", &rows).unwrap();
        drop(l);
        drop(c);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "record::managed::paged::prune::tests::crash_worker",
                "--nocapture",
            ])
            .env("OOJURO_PAGED_CRASH_ROOT", &f.root)
            .env("OOJURO_PAGED_CRASH_PHASE", phase)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f.root.join("stopped").exists() {
            if child.try_wait().unwrap().is_some() {
                panic!("fixture exited before boundary {phase}");
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("fixture phase timeout {phase}");
            }
            thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        let c = control(&f, false);
        let mut l = log(&f, c, false);
        if matches!(phase, "stage" | "control-object") {
            assert!(l.get(9, 4096).unwrap().is_some());
        } else {
            assert!(l.get(9, 4096).unwrap().is_none());
        }
        if matches!(phase, "control" | "cleanup") {
            assert!(matches!(
                l.prune("different", &Selection::Ids(vec![10]), limits()),
                Err(Error::Unavailable)
            ));
            assert!(l.append("blocked", &[input("other", "blocked")]).is_err());
        }
        l.prune("crash-prune", &Selection::Ids(vec![9]), limits())
            .unwrap();
        assert!(l.get(9, 4096).unwrap().is_none());
        let replay = l.append("original", &rows).unwrap();
        assert_eq!(replay.commit, ack.commit);
        assert!(replay.receipt_only);
        assert_eq!(l.get(10, 4096).unwrap().unwrap().text, "survives crash");
    }
}

#[test]
fn changed_retained_witness_before_local_receipt_return_is_not_acknowledged() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    l.append("original", &[input("remove", "selected")])
        .unwrap();
    let witness = c.witness_directory.join("CURRENT");
    l.hook = Some(Arc::new(move |at| {
        if at == Phase::BeforeAck {
            let mut bytes = fs::read(&witness).unwrap();
            bytes[0] ^= 1;
            fs::write(&witness, bytes).unwrap();
        }
    }));
    assert!(
        l.prune("remove", &Selection::Ids(vec![9]), PruneLimits::default())
            .is_err()
    );
}
