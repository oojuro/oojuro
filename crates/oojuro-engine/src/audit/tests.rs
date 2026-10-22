use super::*;
use ring::rand::SystemRandom;
fn signer(domain: Domain) -> Signer {
    let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Signer::from_pkcs8([1; 32], domain, key.as_ref()).unwrap()
}
fn body(sequence: u64, events: u64, previous: [u8; 32], head: [u8; 32]) -> CheckpointBody {
    CheckpointBody {
        schema: 1,
        authority: [1; 32],
        domain: Domain::NativeLifecycle,
        scope: [2; 32],
        sequence,
        events,
        previous,
        head,
        issued_at: 500,
    }
}
fn event(sequence: u64, previous: [u8; 32]) -> AuditEntry {
    AuditEntry {
        previous,
        event: AuditEvent {
            sequence,
            recorded_at: 100,
            kind: EventKind::LogicalDenialPendingPermanent,
            scope: [3; 32],
            actor: [4; 32],
            records: 19,
            bytes: 1000,
            before: [5; 32],
            after: [6; 32],
        },
    }
}
fn fixture() -> (Signer, Verifier, AuditExport) {
    let signer = signer(Domain::NativeLifecycle);
    let verifier = Verifier::new([1; 32], Domain::NativeLifecycle, signer.public_key());
    let one = event(1, [0; 32]);
    let two = event(2, one.digest().unwrap());
    let first = signer
        .sign(body(1, 1, [0; 32], one.digest().unwrap()))
        .unwrap();
    let second = signer
        .sign(body(2, 2, first.digest().unwrap(), two.digest().unwrap()))
        .unwrap();
    let bundle = AuditExport {
        schema: 1,
        authority: [1; 32],
        domain: Domain::NativeLifecycle,
        scope: [2; 32],
        public_key: signer.public_key(),
        entries: vec![one, two],
        checkpoints: vec![first, second],
    };
    (signer, verifier, bundle)
}
fn tip(bundle: &AuditExport) -> RetainedFloor {
    bundle.checkpoints.last().unwrap().floor().unwrap()
}
#[test]
fn signed_content_free_export_roundtrip_and_witness() {
    let (_, verifier, bundle) = fixture();
    let floor = bundle.checkpoints[0].floor().unwrap();
    bundle.verify(&verifier, tip(&bundle), Some(floor)).unwrap();
    let bytes = bundle.encode().unwrap();
    let decoded = AuditExport::decode(&bytes).unwrap();
    assert_eq!(decoded, bundle);
    decoded
        .verify(&verifier, tip(&bundle), Some(floor))
        .unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("logical_denial_pending_permanent")
    );
    assert!(!std::str::from_utf8(&bytes).unwrap().contains("private_key"));
}
#[test]
fn every_signed_field_is_bound_and_unknown_versions_refused() {
    let (_, verifier, bundle) = fixture();
    let cp = &bundle.checkpoints[0];
    let mut variants = vec![];
    for field in 0..10 {
        let mut bad = cp.clone();
        match field {
            0 => bad.body.schema = 2,
            1 => bad.body.authority[0] ^= 1,
            2 => bad.body.domain = Domain::HostedRead,
            3 => bad.body.scope[0] ^= 1,
            4 => bad.body.sequence += 1,
            5 => bad.body.events += 1,
            6 => bad.body.previous[0] ^= 1,
            7 => bad.body.head[0] ^= 1,
            8 => bad.body.issued_at += 1,
            _ => bad.signature[0] ^= 1,
        }
        variants.push(bad);
    }
    for bad in variants {
        assert!(verifier.verify_checkpoint(&bad, [2; 32]).is_err());
    }
    let mut zero = cp.body.clone();
    zero.sequence = 0;
    assert!(zero.bytes().is_err());
    assert!(Signer::from_pkcs8([1; 32], Domain::NativeLifecycle, b"not a private key").is_err());
}
#[test]
fn sender_public_key_cannot_replace_pinned_trust_or_scope() {
    let (original, verifier, bundle) = fixture();
    let attacker = signer(Domain::NativeLifecycle);
    let mut forged = bundle.clone();
    forged.public_key = attacker.public_key();
    for cp in &mut forged.checkpoints {
        *cp = attacker.sign(cp.body.clone()).unwrap();
    }
    assert_eq!(
        forged.verify(&verifier, tip(&forged), None),
        Err(Error::Scope)
    );
    assert_eq!(
        verifier.verify_checkpoint(&bundle.checkpoints[0], [99; 32]),
        Err(Error::Scope)
    );
    let hosted = Verifier::new([1; 32], Domain::HostedRead, original.public_key());
    assert_eq!(
        hosted.verify_checkpoint(&bundle.checkpoints[0], [2; 32]),
        Err(Error::Scope)
    );
    let mut wrong = bundle.checkpoints[0].body.clone();
    wrong.domain = Domain::HostedRead;
    assert_eq!(original.sign(wrong), Err(Error::Scope));
}
#[test]
fn trusted_current_detects_suffix_truncation_and_floor_detects_alternate_longer_fork() {
    let (signer, verifier, bundle) = fixture();
    let mut shortened = bundle.clone();
    shortened.entries.pop();
    shortened.checkpoints.pop();
    assert_eq!(
        shortened.verify(&verifier, tip(&bundle), None),
        Err(Error::Witness)
    );
    // An honest signature is still not proof of the independently witnessed branch.
    let mut fork = bundle.clone();
    fork.entries[0].event.records += 1;
    fork.entries[1].previous = fork.entries[0].digest().unwrap();
    fork.checkpoints[0] = signer
        .sign(body(1, 1, [0; 32], fork.entries[0].digest().unwrap()))
        .unwrap();
    fork.checkpoints[1] = signer
        .sign(body(
            2,
            2,
            fork.checkpoints[0].digest().unwrap(),
            fork.entries[1].digest().unwrap(),
        ))
        .unwrap();
    fork.verify(&verifier, tip(&fork), None).unwrap();
    assert_eq!(
        fork.verify(
            &verifier,
            tip(&fork),
            Some(bundle.checkpoints[0].floor().unwrap())
        ),
        Err(Error::Witness)
    );
    // A floor on the old branch cannot be satisfied by a higher ordinal alone.
    let extra = event(3, fork.entries[1].digest().unwrap());
    fork.checkpoints.push(
        signer
            .sign(body(3, 3, tip(&fork).digest, extra.digest().unwrap()))
            .unwrap(),
    );
    fork.entries.push(extra);
    assert_eq!(
        fork.verify(&verifier, tip(&fork), Some(tip(&bundle))),
        Err(Error::Witness)
    );
}
#[test]
fn every_event_and_checkpoint_link_is_verified_without_sampling() {
    let (_, verifier, bundle) = fixture();
    let trusted = tip(&bundle);
    for field in 0..9 {
        let mut bad = bundle.clone();
        match field {
            0 => bad.entries[0].event.scope[0] ^= 1,
            1 => bad.entries[0].event.actor[0] ^= 1,
            2 => bad.entries[0].event.bytes += 1,
            3 => bad.entries[0].event.before[0] ^= 1,
            4 => bad.entries[0].event.after[0] ^= 1,
            5 => bad.entries[0].event.recorded_at += 1,
            6 => bad.entries[0].event.sequence += 1,
            7 => bad.entries[0].event.kind = EventKind::LiveRewriteVerified,
            _ => bad.entries[1].previous[0] ^= 1,
        }
        assert!(bad.verify(&verifier, trusted, None).is_err());
    }
    let mut missing = bundle.clone();
    missing.entries.remove(0);
    assert!(missing.verify(&verifier, trusted, None).is_err());
    let mut duplicate = bundle.clone();
    duplicate.entries.push(duplicate.entries[1].clone());
    assert!(duplicate.verify(&verifier, trusted, None).is_err());
    let mut skipped = bundle.clone();
    skipped.checkpoints.remove(0);
    assert!(skipped.verify(&verifier, trusted, None).is_err());
}
#[test]
fn authentic_but_incorrect_head_or_count_or_domain_is_refused() {
    let (signer, verifier, mut bundle) = fixture();
    bundle.checkpoints[1].body.head = [77; 32];
    bundle.checkpoints[1] = signer.sign(bundle.checkpoints[1].body.clone()).unwrap();
    assert_eq!(
        bundle.verify(&verifier, tip(&bundle), None),
        Err(Error::Chain)
    );
    let (signer, verifier, mut bundle) = fixture();
    bundle.checkpoints[1].body.events = u64::MAX;
    bundle.checkpoints[1] = signer.sign(bundle.checkpoints[1].body.clone()).unwrap();
    assert!(bundle.verify(&verifier, tip(&bundle), None).is_err());
    let (_, verifier, mut bundle) = fixture();
    bundle.entries[1].event.kind = EventKind::PayloadExport;
    assert_eq!(
        bundle.verify(&verifier, tip(&bundle), None),
        Err(Error::Chain)
    );
}
#[test]
fn hosted_opaque_ciphertext_heads_and_timer_checkpoints_use_same_signature_seam() {
    let signer = signer(Domain::HostedRead);
    let verifier = Verifier::new([1; 32], Domain::HostedRead, signer.public_key());
    let mut one = body(1, 3, [0; 32], [123; 32]);
    one.domain = Domain::HostedRead;
    let one = signer.sign(one).unwrap();
    let mut two = one.body.clone();
    two.sequence = 2;
    two.previous = one.digest().unwrap();
    two.issued_at += 1;
    let two = signer.sign(two).unwrap();
    let floor = one.floor().unwrap();
    let current = two.floor().unwrap();
    verifier.verify_checkpoint(&one, [2; 32]).unwrap();
    verifier
        .verify_chain(&[one, two.clone()], current, Some(floor))
        .unwrap();
    verifier
        .verify_chain(&[two.clone()], current, Some(current))
        .unwrap();
    assert!(verifier.verify_chain(&[two], current, None).is_err());
}
#[test]
fn strict_bounded_json_and_signature_lengths() {
    let (_, verifier, bundle) = fixture();
    let mut value = serde_json::to_value(&bundle).unwrap();
    value["private_context"] = serde_json::json!("forbidden synthetic field");
    assert_eq!(
        AuditExport::decode(&serde_json::to_vec(&value).unwrap()),
        Err(Error::Invalid)
    );
    assert_eq!(
        AuditExport::decode(&vec![b' '; MAX_EXPORT_BYTES + 1]),
        Err(Error::Bounds)
    );
    let mut big = bundle.clone();
    big.entries = vec![bundle.entries[0].clone(); MAX_ENTRIES + 1];
    assert_eq!(big.encode(), Err(Error::Bounds));
    big.entries.pop();
    assert_eq!(big.encode(), Err(Error::Bounds)); // encoded byte bound, not only entry count
    let mut short = bundle.checkpoints[0].clone();
    short.signature.pop();
    assert_eq!(
        verifier.verify_checkpoint(&short, [2; 32]),
        Err(Error::Signature)
    );
    let mut long = bundle.checkpoints[0].clone();
    long.signature.push(0);
    assert_eq!(
        verifier.verify_checkpoint(&long, [2; 32]),
        Err(Error::Signature)
    );
}
#[test]
fn uncheckpointed_tail_and_reordered_checkpoints_are_refused() {
    let (_, verifier, mut bundle) = fixture();
    let trusted = tip(&bundle);
    bundle
        .entries
        .push(event(3, bundle.entries[1].digest().unwrap()));
    assert_eq!(bundle.verify(&verifier, trusted, None), Err(Error::Chain));
    bundle.entries.pop();
    bundle.checkpoints.reverse();
    assert!(bundle.verify(&verifier, trusted, None).is_err());
}
