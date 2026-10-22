use super::*;
use crate::record::managed::tests::input;
#[test]
fn canonical_fingerprints_preserve_number_spelling_and_exact_existing_bytes() {
    let mut i = input("agent", "\"\\\0\t😀");
    i.attrs.insert("float".into(), serde_json::json!(-0.0));
    let inputs = [i];
    let key = [9; 32];
    let bytes = serde_json::to_vec(&inputs).unwrap();
    assert_eq!(
        fingerprint(&key, &inputs, 1).unwrap(),
        *blake3::keyed_hash(&key, &bytes).as_bytes()
    );
    assert_eq!(
        fingerprint(&key, &inputs, 2).unwrap(),
        fingerprint(&key, &inputs, 1).unwrap()
    );
    let row = Assigned {
        id: 7,
        recorded_at: -12,
        input: inputs[0].clone(),
    };
    assert_eq!(
        serde_json::to_vec(&row).unwrap(),
        serde_json::to_vec(&AssignedRef {
            id: 7,
            recorded_at: -12,
            input: &inputs[0]
        })
        .unwrap()
    );
    let mut changed = inputs.clone();
    changed[0]
        .attrs
        .insert("float".into(), serde_json::json!(0.0));
    assert_ne!(
        fingerprint(&key, &inputs, 2).unwrap(),
        fingerprint(&key, &changed, 2).unwrap()
    );
}
#[test]
fn lexical_preflight_handles_unicode_and_refuses_unbounded_allocation_shapes() {
    for bytes in [
        br#"{"x":"\u0000\u07ff\u0800\ud83d\ude00"}"#.as_slice(),
        br#"{"x":[null,true,-0.0]}"#.as_slice(),
    ] {
        preflight(bytes).unwrap();
    }
    for bytes in [
        br#"{"x":"\ud800x"}"#.as_slice(),
        br#"{"x":"\udc00"}"#.as_slice(),
        br#"{"x":"\uXXXX"}"#.as_slice(),
    ] {
        assert!(preflight(bytes).is_err());
    }
    assert!(preflight(format!("[{}]", "0,".repeat(13000)).as_bytes()).is_err());
    assert!(preflight(format!("{}0{}", "[".repeat(41), "]".repeat(41)).as_bytes()).is_err());
    assert!(preflight(format!("{{\"x\":{}}}", "1".repeat(65)).as_bytes()).is_err());
}

use crate::record::managed::tests::{Fixture, identity};
fn control(f: &Fixture, fresh: bool) -> Arc<RetainedControl> {
    let create = if fresh {
        RetainedControl::create_paged_streaming
    } else {
        RetainedControl::open_paged
    };
    Arc::new(
        create(
            &f.root.join("stream-control"),
            &[52; 32],
            f.signer(),
            [53; 32],
            FileWitness::open(&f.root.join("stream-witness")).unwrap(),
            identity_binding(&[51; 32], &identity()).unwrap(),
        )
        .unwrap(),
    )
}
fn log(f: &Fixture, c: Arc<RetainedControl>, fresh: bool) -> driver::ManagedPagedLog {
    let open = if fresh {
        driver::ManagedPagedLog::create_streaming
    } else {
        driver::ManagedPagedLog::open_streaming
    };
    open(&f.root.join("stream-data"), &[51; 32], identity(), c).unwrap()
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout1Anchor {
    format: u32,
    birth: Vec<u8>,
    generation: u64,
    key_version: [u8; 32],
    token: [u8; 32],
    ciphertext_digest: [u8; 32],
    bytes: u32,
    details: control::ReceiptDetails,
}
#[test]
fn anchor_pin_precedes_current_deny_and_publication_and_old_schema_refuses_it() {
    let f = Fixture::new();
    let c = control(&f, true);
    let l = log(&f, c.clone(), true);
    let mut owner = c.lock().unwrap();
    let anchor = owner.state.paged.as_ref().unwrap();
    assert!(serde_json::from_slice::<Layout1Anchor>(&serde_json::to_vec(anchor).unwrap()).is_err());
    let mut unsupported = owner.state.clone();
    unsupported
        .paged
        .as_mut()
        .unwrap()
        .layout
        .as_mut()
        .unwrap()
        .minimum_reader = 3;
    let current = fs::read(f.root.join("stream-witness/CURRENT")).unwrap();
    assert!(owner.advance(unsupported.clone()).is_err());
    assert_eq!(
        fs::read(f.root.join("stream-witness/CURRENT")).unwrap(),
        current
    );
    owner.state = unsupported;
    drop(owner);
    assert!(c.current_floor().is_err());
    assert!(c.deny().is_err());
    assert!(c.status().is_err());
    assert_eq!(
        fs::read(f.root.join("stream-witness/CURRENT")).unwrap(),
        current
    );
    drop(l);
    drop(c);
    let c = control(&f, false);
    let l = log(&f, c.clone(), false);
    drop(l);
    assert!(
        driver::ManagedPagedLog::open(
            &f.root.join("stream-data"),
            &[51; 32],
            identity(),
            c.clone()
        )
        .is_err()
    );
    c.deny().unwrap();
    c.cleanup_paged_data(&f.root.join("stream-data"), PruneLimits::default())
        .unwrap();
}
#[test]
fn layout_one_anchor_bytes_remain_absent_and_streaming_cannot_convert_it() {
    let f = Fixture::new();
    let c = super::super::driver::tests::control(&f, true);
    let owner = c.lock().unwrap();
    let a = owner.state.paged.as_ref().unwrap();
    let bytes = serde_json::to_vec(a).unwrap();
    let old: Layout1Anchor = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&old).unwrap(), bytes);
    drop(owner);
    assert!(
        driver::ManagedPagedLog::create_streaming(
            &f.root.join("stream-data"),
            &[51; 32],
            identity(),
            c.clone()
        )
        .is_err()
    );
    assert!(!f.root.join("stream-data").exists());
    let l = super::super::driver::tests::log(&f, c, true);
    drop(l);
}
#[test]
fn logical_batch_budget_counts_metadata_and_array_punctuation_exactly() {
    let key = [7; 32];
    let mut inputs = [input("one", ""), input("two", "")];
    let metadata = hash(&EmptyInputs(&inputs), &key, MAX_BATCH_BYTES)
        .unwrap()
        .1;
    inputs[0].text = "x".repeat(MAX_TEXT_BYTES);
    inputs[1].text = "y".repeat(MAX_BATCH_BYTES - metadata - MAX_TEXT_BYTES);
    fingerprint(&key, &inputs, 2).unwrap();
    inputs[1].text.push('y');
    assert!(matches!(fingerprint(&key, &inputs, 2), Err(Error::Bounds)));
}
#[test]
fn legal_worst_escaping_original_spans_packs_reopens_and_remains_prunable() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    let mut v = input("large", "");
    v.text = "\0".repeat(MAX_TEXT_BYTES);
    let inputs = [v];
    let ack = l.append("original", &inputs).unwrap();
    {
        let owner = c.lock().unwrap();
        let root = schema::load(&c.directory, owner.control_cipher(), &owner.state).unwrap();
        assert_eq!(root.format, 2);
        assert!(root.physical.as_ref().unwrap().file.count > 1);
        assert_eq!(
            root.physical.as_ref().unwrap().file.count,
            root.data.as_ref().unwrap().segment.count
        );
        let source = l.source(&owner, &root).unwrap();
        let got = driver::read_assigned(&source, &l.directory, 9)
            .unwrap()
            .unwrap();
        assert_eq!(got.recorded_at, ack.commit.recorded_at[0]);
        assert_eq!(got.input.text, inputs[0].text);
    }
    // The public encoded-JSON output bound is intentionally not relaxed.
    assert!(matches!(l.get(9, 4096), Err(Error::Bounds)));
    drop(l);
    let mut l = log(&f, c.clone(), false);
    let report = l
        .prune(
            "erase",
            &Selection::Ids(vec![9]),
            PruneLimits {
                bytes: 2 << 30,
                rows: 1_000_000,
            },
        )
        .unwrap();
    assert_eq!(report.selected_count, 1);
    report.signed_receipt.verify_record_ids(&[9]).unwrap();
    let retry = l.append("original", &inputs).unwrap();
    assert_eq!(retry.commit, ack.commit);
    assert!(retry.receipt_only);
    assert_eq!(retry.removed_ids, vec![9]);
    assert!(l.get(9, 4096).unwrap().is_none());
}
#[test]
fn streamed_survivor_rewrite_retains_negative_zero_and_chunk_boundaries() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    let mut big = input("survivor", "");
    big.text = "\"\\\0😀".repeat((CHUNK / 8) + 1);
    big.attrs
        .insert("minuszero".into(), serde_json::json!(-0.0));
    let small = input("remove", "selected unique small");
    let ack = l.append("batch", &[big.clone(), small]).unwrap();
    l.prune(
        "remove",
        &Selection::Ids(vec![10]),
        PruneLimits {
            bytes: 2 << 30,
            rows: 1_000_000,
        },
    )
    .unwrap();
    let owner = c.lock().unwrap();
    let root = schema::load(&c.directory, owner.control_cipher(), &owner.state).unwrap();
    let source = l.source(&owner, &root).unwrap();
    let got = driver::read_assigned(&source, &l.directory, 9)
        .unwrap()
        .unwrap();
    assert_eq!(got.recorded_at, ack.commit.recorded_at[0]);
    assert_eq!(
        digest(&got.input, &root.intent, 2).unwrap(),
        digest(&big, &root.intent, 2).unwrap()
    );
}

#[test]
fn verified_parts_refuse_truncation_reordering_and_changed_authority_between_chunks() {
    let f = Fixture::new();
    let c = control(&f, true);
    let mut l = log(&f, c.clone(), true);
    let mut v = input("parts", "");
    v.text = "x".repeat(CHUNK);
    l.append("original", &[v]).unwrap();
    let owner = c.lock().unwrap();
    let root = schema::load(&c.directory, owner.control_cipher(), &owner.state).unwrap();
    let source = l.source(&owner, &root).unwrap();
    let raw = source
        .lookup(
            schema::RootName::Record,
            &root.data.as_ref().unwrap().record,
            &9u64.to_be_bytes(),
        )
        .unwrap()
        .unwrap();
    let reference: RowRef = serde_json::from_slice(&raw).unwrap();
    let reader = Reader::new(
        &driver::data_directory(&l.directory, &root)
            .unwrap()
            .join("packs"),
        &source,
        CHUNK,
    )
    .unwrap();
    let mut truncated = reference.clone();
    truncated.bytes -= 1;
    assert!(decode(&reader, &truncated, 9, 2).is_err());
    let mut extra = reference.clone();
    extra.chunks += 1;
    assert!(decode(&reader, &extra, 9, 2).is_err());
    let mut moved = reference.clone();
    moved.first = u64::MAX;
    assert!(decode(&reader, &moved, 9, 2).is_err());
    let mut stream = parts(&reader, &reference);
    let mut first = vec![0; CHUNK];
    stream.read_exact(&mut first).unwrap();
    let path = f.root.join("stream-witness/CURRENT");
    let mut witness = fs::read(&path).unwrap();
    witness[0] ^= 1;
    fs::write(path, witness).unwrap();
    assert!(stream.read(&mut [0]).is_err());
}
