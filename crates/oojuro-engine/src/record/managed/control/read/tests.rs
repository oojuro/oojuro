use super::*;
use crate::record::managed::tests::{Fixture, identity, input};
fn limits(entries: usize) -> Limits {
    Limits {
        entries,
        bytes: 1 << 20,
    }
}
fn verifier(f: &Fixture) -> Verifier {
    Verifier::new([53; 32], Domain::NativeLifecycle, f.signer().public_key())
}
#[test]
fn retained_status_survives_denial_and_names_snapshot_counts_honestly() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "two",
        &[input("synthetic-a", "one"), input("synthetic-b", "two")],
    )
    .unwrap();
    let before = control.status().unwrap();
    assert_eq!(
        before.identity,
        identity_binding(&[51; 32], &identity()).unwrap()
    );
    assert_eq!(before.status, Status::Active);
    assert_eq!(before.retained_snapshot_records, 2);
    assert!(before.retained_snapshot_bytes > 0);
    assert!(!before.local_cleanup_completed);
    control.deny().unwrap();
    let denied = control.status().unwrap();
    assert_eq!(denied.origin, before.origin);
    assert_eq!(denied.status, Status::DeniedPendingPermanent);
    assert_eq!(denied.retained_snapshot_records, 2); // retained evidence, not live data
    assert!(!denied.local_cleanup_completed);
    assert!(log.get(9, 1 << 20).is_err());
    log.deny_writer().unwrap();
    let cleaned = control.status().unwrap();
    assert!(cleaned.local_cleanup_completed);
    assert_eq!(cleaned.status, Status::DeniedPendingPermanent);
    assert_eq!(cleaned.retained_snapshot_records, 2);
    assert!(log.finalize_permanent_destruction().is_err());
}
#[test]
fn signed_pages_include_checkpoint_only_links_and_exact_terminal_witness() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "one",
        &[input(
            "synthetic-only-source",
            "synthetic-only-private-text",
        )],
    )
    .unwrap();
    log.prune("remove", &Selection::Ids(vec![9])).unwrap();
    let status = control.status().unwrap();
    let verifier = verifier(&f);
    let mut start = 0;
    let mut previous = [0; 32];
    let mut saw_checkpoint_only = false;
    let mut saw_lifecycle = false;
    loop {
        let page = control
            .export_audit(status.current_floor, start, limits(1))
            .unwrap();
        assert_eq!(page.identity, status.identity);
        assert_eq!(page.start, start);
        assert_eq!(page.current, status.current_floor);
        assert_eq!(page.entries.len(), 1);
        let bytes = json(&page, 1 << 20).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-only-private-text"));
        assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-only-source"));
        let entry = &page.entries[0];
        entry.verify(&verifier, status.identity).unwrap();
        assert_eq!(entry.checkpoint.body.sequence, start + 1);
        assert_eq!(entry.checkpoint.body.previous, previous);
        previous = entry.checkpoint.digest().unwrap();
        if entry.event.is_none() {
            saw_checkpoint_only = true;
            let mut changed = entry.clone();
            changed.ciphertext_digest[0] ^= 1;
            assert!(changed.verify(&verifier, status.identity).is_err());
            let mut changed = entry.clone();
            changed.details.decoded_occurrences = 1;
            assert!(changed.verify(&verifier, status.identity).is_err());
        } else {
            saw_lifecycle = true;
            let mut changed = entry.clone();
            changed.event.as_mut().unwrap().records += 1;
            assert!(changed.verify(&verifier, status.identity).is_err());
        }
        assert!(entry.verify(&verifier, [0; 32]).is_err());
        match page.next {
            Some(next) => {
                assert_eq!(next, start + 1);
                start = next;
            }
            None => {
                assert_eq!(entry.checkpoint.floor().unwrap(), status.current_floor);
                break;
            }
        }
    }
    assert!(saw_checkpoint_only && saw_lifecycle);
}
#[test]
fn page_bounds_exact_encoded_size_and_changed_tip_fail_closed() {
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    let old = control.status().unwrap().current_floor;
    let first = control.export_audit(old, 0, limits(1)).unwrap();
    let exact = json(&first, 1 << 20).unwrap().len();
    assert_eq!(
        json(
            &control
                .export_audit(
                    old,
                    0,
                    Limits {
                        entries: 1,
                        bytes: exact
                    }
                )
                .unwrap(),
            exact
        )
        .unwrap()
        .len(),
        exact
    );
    assert!(
        control
            .export_audit(
                old,
                0,
                Limits {
                    entries: 1,
                    bytes: exact - 1
                }
            )
            .is_err()
    );
    for invalid in [
        Limits {
            entries: 0,
            bytes: 10000,
        },
        Limits {
            entries: 1001,
            bytes: 10000,
        },
        Limits {
            entries: 1,
            bytes: 0,
        },
        Limits {
            entries: 1,
            bytes: MAX_AUDIT_PAGE + 1,
        },
    ] {
        assert!(control.export_audit(old, 0, invalid).is_err());
    }
    assert!(control.export_audit(old, old.sequence, limits(1)).is_err());
    assert!(control.export_audit(old, u64::MAX, limits(1)).is_err());
    log.append("advance", &[input("synthetic-a", "new current")])
        .unwrap();
    assert!(matches!(
        control.export_audit(old, 0, limits(1)),
        Err(Error::Conflict)
    ));
    let current = control.status().unwrap().current_floor;
    assert!(control.export_audit(current, 0, limits(1)).is_ok());
    fs::write(f.root.join("witness/CURRENT"), b"invalid retained witness").unwrap();
    assert!(control.status().is_err());
    assert!(control.export_audit(current, 0, limits(1)).is_err());
}
