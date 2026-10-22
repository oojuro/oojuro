use super::super::driver::tests::{control, log};
use super::*;
use crate::audit::{Domain, Verifier};
use crate::record::managed::tests::{Fixture, input};
fn verifier(f: &Fixture) -> Verifier {
    Verifier::new([53; 32], Domain::NativeLifecycle, f.signer().public_key())
}
#[test]
fn exact_signed_id_pages_stream_and_fail_closed_on_skip_double_tamper_and_tip_change() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    l.append(
        "original",
        &[
            input("remove", "one"),
            input("keep", "safe"),
            input("remove", "three"),
            input("remove", "four"),
        ],
    )
    .unwrap();
    let report = l
        .prune(
            "remove",
            &Selection::Agent("remove".into()),
            PruneLimits::default(),
        )
        .unwrap();
    let tip = c.status().unwrap().current_floor;
    let limits = ReadLimits {
        entries: 1,
        ..ReadLimits::default()
    };
    let mut stream = report
        .signed_receipt
        .record_ids_verifier(&verifier(&f), c.status().unwrap().identity)
        .unwrap();
    let mut next = Some(0);
    let mut ids = Vec::new();
    while let Some(start) = next {
        let page = c.pruned_ids(tip, "remove", start, limits).unwrap();
        assert_eq!(page.identity, c.status().unwrap().identity);
        for id in page.ids {
            stream.push(id).unwrap();
            ids.push(id);
        }
        next = page.next;
    }
    assert_eq!(ids, vec![9, 11, 12]);
    stream.finish().unwrap();
    report.signed_receipt.verify_record_ids(&ids).unwrap();
    assert!(
        report
            .signed_receipt
            .record_ids_verifier(&verifier(&f), [99; 32])
            .is_err()
    );
    let wrong = Verifier::new(
        [53; 32],
        Domain::NativeLifecycle,
        Fixture::new().signer().public_key(),
    );
    assert!(
        report
            .signed_receipt
            .record_ids_verifier(&wrong, c.status().unwrap().identity)
            .is_err()
    );
    for bad in [
        vec![9, 12],
        vec![9, 11, 13],
        vec![9, 9, 11, 12],
        vec![11, 9, 12],
        vec![9, 11, 12, 13],
    ] {
        let mut stream = report
            .signed_receipt
            .record_ids_verifier(&verifier(&f), c.status().unwrap().identity)
            .unwrap();
        for id in bad {
            let _ = stream.push(id);
        }
        assert!(stream.finish().is_err());
    }
    let mut tampered = report.signed_receipt.clone();
    tampered.details.record_ids = Some([0; 32]);
    assert!(
        tampered
            .record_ids_verifier(&verifier(&f), c.status().unwrap().identity)
            .is_err()
    );
    assert!(c.pruned_ids(tip, "unknown", 0, limits).is_err());
    assert!(c.pruned_ids(tip, "remove", 14, limits).is_err());
    assert!(
        c.pruned_ids(tip, "remove", 0, ReadLimits { bytes: 1, ..limits })
            .is_err()
    );
    l.append("tail", &[input("keep", "tail")]).unwrap();
    assert!(c.pruned_ids(tip, "remove", 0, limits).is_err());
    let tip = c.status().unwrap().current_floor;
    let p = c
        .pruned_ids(tip, "remove", 0, ReadLimits::default())
        .unwrap();
    assert_eq!(p.ids, ids);
}
#[test]
fn membership_export_survives_current_denial_and_data_removal_without_a_data_key() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    l.append(
        "original",
        &[input("remove", "selected"), input("keep", "survivor")],
    )
    .unwrap();
    let report = l
        .prune("remove", &Selection::Ids(vec![9]), PruneLimits::default())
        .unwrap();
    drop(l);
    c.deny().unwrap();
    c.cleanup_paged_data(&f.root.join("paged-data"), PruneLimits::default())
        .unwrap();
    let current = c.status().unwrap().current_floor;
    let page = c
        .pruned_ids(current, "remove", 0, ReadLimits::default())
        .unwrap();
    assert_eq!(page.ids, vec![9]);
    assert_eq!(
        page.signed_receipt.checkpoint,
        report.signed_receipt.checkpoint
    );
    let mut stream = page
        .signed_receipt
        .record_ids_verifier(&verifier(&f), page.identity)
        .unwrap();
    stream.push(9).unwrap();
    stream.finish().unwrap();
}
