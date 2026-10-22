use super::*;
use crate::{
    lexical::{Index, Query},
    receipts::{Limits, Range, ReceiptView, Scope},
};
use serde_json::json;
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
const KEY: [u8; 32] = [73; 32];
const SOURCE: Source = [11; 16];
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    data: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oojuro-message-engine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            data: root.join("message-authority"),
            root,
        }
    }
    fn create(&self) -> MessageLog {
        let mut log = MessageLog::create(&self.data, &KEY, identity()).unwrap();
        log.register(
            SOURCE,
            b"reported-source-generation",
            br#"{"version":1,"calls":[]}"#,
        )
        .unwrap();
        log
    }
    fn open(&self) -> MessageLog {
        MessageLog::open(&self.data, &KEY, identity()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn identity() -> Identity {
    Identity {
        writer: "synthetic-writer".into(),
        agent: 7,
        first_id: 41,
        adapter: "synthetic-message-body-1".into(),
    }
}
fn scope() -> Scope<'static> {
    Scope {
        writer: "synthetic-writer",
        agent: 7,
    }
}
fn request(id: &str, seq: u64) -> Request {
    Request {
        id: id.into(),
        sequence: seq,
        metadata: br#"{"codec":"opaque-binary","boundary":"PRIVATE_PROTOCOL_INTENT"}"#.to_vec(),
    }
}
fn event(id: u64, start: u64, end: u64, indexed: bool, text: &str, emission: u32) -> Event {
    Event {
        id,
        session: 9,
        parent_session: 0,
        agent: 7,
        turn: 1,
        seq: 1,
        parent: 0,
        revises: 0,
        kind: 1,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: 123,
        duration_us: 0,
        tool: String::new(),
        args: "{\"reported\":true}".into(),
        text: text.into(),
        indexed,
        line_start: start,
        line_end: end,
        emission,
    }
}
fn batch(
    log: &MessageLog,
    id: &str,
    sequence: u64,
    raw: &[u8],
    stored: Option<(&str, bool)>,
) -> MessageBatch {
    let progress = log.progress(SOURCE).unwrap();
    let start = progress.raw_end;
    let end = start + raw.len() as u64;
    let next = log.authority().unwrap().next_id();
    let events = stored
        .map(|(text, indexed)| vec![event(next, start, end, indexed, text, 0)])
        .unwrap_or_default();
    let emissions = events
        .iter()
        .map(|e| Decision::Stored {
            id: e.id,
            indexed: e.indexed,
        })
        .collect();
    MessageBatch {
        raw: raw.to_vec(),
        parsed: ParsedMessage {
            source: SOURCE,
            raw_start: start,
            request: request(id, sequence),
            outcome: Outcome {
                raw_end: end,
                parsed_end: end,
                first_id: events.first().map(|e| e.id),
                stored: events.len() as u64,
                next_id: next + events.len() as u64,
            },
            policy: MessagePolicy {
                policy_id: "original-policy".into(),
                raw_authorization: "explicit-body-consent".into(),
                messages: vec![MessageUnit {
                    start,
                    end,
                    kind: MessageKind::Parsed,
                    emissions,
                }],
            },
            context: br#"{"version":1,"completed":[],"denied":["PRIVATE_DENIAL_CONTEXT"]}"#
                .to_vec(),
            events,
        },
    }
}
fn commit(log: &mut MessageLog, b: MessageBatch) -> MessageAck {
    log.commit(b).unwrap()
}
fn query(log: &MessageLog, text: &str) -> Vec<crate::lexical::Hit> {
    let authority = log.authority().unwrap();
    Index::rebuild(authority)
        .unwrap()
        .search(&Query {
            writer: "synthetic-writer",
            agent: 7,
            text,
            from: i64::MIN,
            to: i64::MAX,
            k: 10,
        })
        .unwrap()
}

#[test]
fn binary_pretty_json_sse_exact_bytes_original_intent_and_authorized_native_query() {
    let f = Fixture::new();
    let mut log = f.create();
    let bodies = [
        vec![0, 255, 10, 0, 128],
        b"{\n \"resourceSpans\": []\n}".to_vec(),
        b"event: message\ndata: {\"text\":\"hello\"}\n\n".to_vec(),
    ];
    let mut joined: Vec<u8> = vec![];
    let mut original = None;
    for (n, body) in bodies.iter().enumerate() {
        let b = batch(
            &log,
            &format!("request-{n}"),
            n as u64 + 1,
            body,
            Some(("battery native", n == 0)),
        );
        if n == 0 {
            original = Some((b.parsed.request.clone(), body.clone()));
        }
        let ack = commit(&mut log, b);
        assert!(!ack.already_committed);
        assert_eq!(ack.outcome.first_id, Some(41 + n as u64));
        joined.extend(body);
    }
    assert_eq!(
        query(&log, "battery").len(),
        1,
        "stored-only messages must not become content hits"
    );
    let (original_request, raw) = original.unwrap();
    let receipt = log
        .lookup(SOURCE, 0, &original_request, &raw)
        .unwrap()
        .unwrap();
    assert!(receipt.already_committed);
    assert_eq!(receipt.outcome.first_id, Some(41));
    assert_eq!(log.position(SOURCE, "request-0").unwrap().sequence, 3);
    let mut changed = original_request.clone();
    changed.metadata = br#"{"codec":"other"}"#.to_vec();
    assert!(log.lookup(SOURCE, 0, &changed, &raw).is_err());
    assert!(
        log.lookup(SOURCE, 0, &original_request, b"different")
            .is_err()
    );
    drop(log);
    let mut log = f.open();
    assert_eq!(log.read_raw(SOURCE, 0, joined.len()).unwrap(), joined);
    assert_eq!(query(&log, "battery").len(), 1);
    // Original lookup/ACK ignores changed current policy/context candidates.
    let mut retry = batch(&log, "request-0", 1, &raw, None);
    retry.parsed.raw_start = 0;
    retry.parsed.request = original_request;
    retry.parsed.context.clear();
    retry.parsed.policy.policy_id.clear();
    assert_eq!(log.commit(retry).unwrap(), receipt);
    assert_eq!(log.authority().unwrap().next_id(), 44);
}

#[test]
fn empty_body_requires_explicit_unit_and_preserves_sequence_metadata_and_exact_ack() {
    let f = Fixture::new();
    let mut log = f.create();
    let b = batch(&log, "empty-one", 1, b"", None);
    let first = commit(&mut log, b);
    let mut second = batch(&log, "empty-two", 2, b"", None);
    second.parsed.request.metadata = br#"{"phase":"explicit-empty-response"}"#.to_vec();
    commit(&mut log, second);
    assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
    assert_eq!(log.position(SOURCE, "empty-one").unwrap().sequence, 2);
    drop(log);
    let log = f.open();
    let replay = log
        .lookup(SOURCE, 0, &request("empty-one", 1), b"")
        .unwrap()
        .unwrap();
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(replay.sequence, 1);
    let view = MessageReceiptView::open(&log, scope(), Range::default()).unwrap();
    let stats = view.stats(&log, 4096).unwrap();
    assert_eq!(stats.parsed_units_without_output, 2);
    assert_eq!(
        view.page(
            &log,
            Limits {
                entries: 10,
                bytes: 8192
            }
        )
        .unwrap()
        .entries[0]
            .byte_range,
        crate::receipts::Span { start: 0, end: 0 }
    );
}

#[test]
fn message_receipts_label_bytes_protect_policy_context_and_bind_cursor_snapshot_scope() {
    let f = Fixture::new();
    let mut log = f.create();
    let mut b = batch(
        &log,
        "mixed",
        1,
        b"\0binary\nrequest\n",
        Some(("private parsed battery", true)),
    );
    b.parsed.policy.messages[0]
        .emissions
        .push(Decision::Withheld {
            reason: "call_withheld".into(),
        });
    commit(&mut log, b);
    let view = MessageReceiptView::open(&log, scope(), Range::default()).unwrap();
    let first = view
        .page(
            &log,
            Limits {
                entries: 1,
                bytes: 8192,
            },
        )
        .unwrap();
    let cursor = first.next.clone().unwrap();
    let encoded = view
        .export(
            &log,
            Limits {
                entries: 10,
                bytes: 16384,
            },
        )
        .unwrap();
    let text = String::from_utf8(encoded.clone()).unwrap();
    assert!(!text.contains("PRIVATE_PROTOCOL_INTENT"));
    assert!(!text.contains("PRIVATE_DENIAL_CONTEXT"));
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(value["framing"], "messages_v1");
    assert!(value["entries"][0].get("line").is_none());
    assert!(value["entries"][0].get("byte_range").is_some());
    assert!(
        log.authority().unwrap().batches().is_err(),
        "normalized spans are not a public line API"
    );
    assert!(ReceiptView::open(log.authority().unwrap(), scope(), Range::default()).is_err());
    assert!(
        ReceiptView::resume(log.authority().unwrap(), scope(), Range::default(), &cursor).is_err()
    );
    assert!(
        MessageReceiptView::open(
            &log,
            Scope {
                writer: "foreign",
                agent: 7
            },
            Range::default()
        )
        .is_err()
    );
    let b = batch(&log, "later", 2, b"later", Some(("later-only", true)));
    commit(&mut log, b);
    let resumed = MessageReceiptView::resume(&log, scope(), Range::default(), &cursor).unwrap();
    let page = resumed
        .page(
            &log,
            Limits {
                entries: 10,
                bytes: 16384,
            },
        )
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert!(page.next.is_none(), "later append stays hidden");
    assert!(view.get(&log, 42, 8192).unwrap().is_none());
    assert!(
        view.export(
            &log,
            Limits {
                entries: 1,
                bytes: 1
            }
        )
        .is_err()
    );
    assert!(
        MessageReceiptView::resume(
            &log,
            scope(),
            Range {
                start: 1,
                end: u64::MAX
            },
            &cursor
        )
        .is_err()
    );
    let other = Fixture::new();
    let other_log = other.create();
    assert!(MessageReceiptView::resume(&other_log, scope(), Range::default(), &cursor).is_err());
}

#[test]
fn gaps_overlaps_empty_emissions_policy_ids_capacity_and_changed_requests_never_ack() {
    let f = Fixture::new();
    let mut log = f.create();
    for case in 0..12 {
        let mut b = batch(&log, "candidate", 1, b"body", Some(("derived", true)));
        match case {
            0 => b.parsed.policy.messages[0].start = 1,
            1 => b.parsed.policy.messages[0].end = 3,
            2 => b
                .parsed
                .policy
                .messages
                .push(b.parsed.policy.messages[0].clone()),
            3 => b.parsed.events[0].indexed = false,
            4 => b.parsed.events[0].id += 1,
            5 => b.parsed.outcome.next_id += 1,
            6 => b.parsed.context = vec![],
            7 => b.parsed.policy.policy_id.clear(),
            8 => b.parsed.policy.raw_authorization.clear(),
            9 => b.parsed.request.sequence = 2,
            10 => b.parsed.request.metadata = vec![b'x'; MAX_REQUEST_METADATA + 1],
            11 => b.parsed.policy.messages[0].kind = MessageKind::Malformed,
            _ => unreachable!(),
        }
        assert!(log.commit(b).is_err(), "case {case}");
        assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
        assert_eq!(log.authority().unwrap().next_id(), 41);
    }
    let b = batch(&log, "large", 1, &vec![0; MAX_BLOCK_BYTES + 1], None);
    assert!(log.commit(b).is_err());
    let mut b = batch(&log, "empty-event", 1, b"", Some(("no raw span", true)));
    assert!(log.commit(b).is_err());
    b = batch(&log, "units", 1, b"", None);
    b.parsed.policy.messages = vec![b.parsed.policy.messages[0].clone(); MAX_UNITS + 1];
    assert!(log.commit(b).is_err());
    assert!(log.register(SOURCE, b"source-fork", b"{}").is_err());
    let b = batch(&log, "good", 1, b"body", None);
    commit(&mut log, b);
    let b = batch(&log, "good", 1, b"changed", None);
    assert!(log.commit(b).is_err());
    assert_eq!(log.progress(SOURCE).unwrap().raw_end, 4);
}

#[test]
fn fresh_marker_mode_key_identity_and_missing_enrollment_are_not_interchangeable() {
    let f = Fixture::new();
    assert!(MessageLog::open(&f.data, &KEY, identity()).is_err());
    assert!(!f.data.exists());
    let log = f.create();
    let scoped_id = log.inner.cipher.key_id();
    assert!(
        log.inner.ingest_binding().is_err(),
        "message mode cannot enter the Lines bridge"
    );
    drop(log);
    assert_ne!(scoped_id, FrameCipher::new(&KEY).key_id());
    assert!(MessageLog::create(&f.data, &KEY, identity()).is_err());
    assert!(CaptureLog::open(&f.data, &KEY, identity()).is_err());
    let mut other = identity();
    other.adapter = "other-adapter".into();
    assert!(MessageLog::open(&f.data, &KEY, other).is_err());
    let marker = fs::read(f.data.join(MARKER)).unwrap();
    assert!(MessageLog::open(&f.data, &[74; 32], identity()).is_err());
    fs::write(f.data.join(MARKER), b"oojuro-capture-1\n").unwrap();
    assert!(MessageLog::open(&f.data, &KEY, identity()).is_err());
    assert!(CaptureLog::open(&f.data, &KEY, identity()).is_err());
    fs::write(f.data.join(MARKER), &marker).unwrap();
    fs::remove_file(f.data.join(MARKER)).unwrap();
    assert!(MessageLog::open(&f.data, &KEY, identity()).is_err());
    assert!(!f.data.join(MARKER).exists());
    fs::write(f.data.join(MARKER), &marker).unwrap();
    fs::rename(f.data.join("MANIFEST"), f.data.join("MISSING-MANIFEST")).unwrap();
    assert!(MessageLog::open(&f.data, &KEY, identity()).is_err());
    assert!(!f.data.join("MANIFEST").exists());
    let line = f.root.join("line-authority");
    let old = CaptureLog::open(&line, &KEY, identity()).unwrap();
    drop(old);
    fs::write(line.join(MARKER), FORMAT).unwrap();
    assert!(
        MessageLog::open(&line, &KEY, identity()).is_err(),
        "marker swap cannot change AEAD purpose"
    );
    let partial = f.root.join("partial");
    fs::create_dir(&partial).unwrap();
    fs::write(partial.join(MARKER), FORMAT).unwrap();
    assert!(MessageLog::open(&partial, &KEY, identity()).is_err());
    assert!(MessageLog::create(&partial, &KEY, identity()).is_err());
    assert!(!partial.join("MANIFEST").exists());
}

#[test]
fn wrong_tag_and_incomplete_required_schema_are_rejected_on_real_reopen() {
    for kind in 0..7 {
        let f = Fixture::new();
        let mut log = f.create();
        let b = batch(&log, "bad", 1, b"raw", None);
        let mut object = serde_json::to_value(&b.parsed).unwrap();
        if kind < 5 {
            object
                .as_object_mut()
                .unwrap()
                .remove(["request", "context", "policy", "outcome", "events"][kind]);
        } else if kind == 5 {
            object["unexpected"] = json!(true);
        }
        let metadata = serde_json::to_vec(&object).unwrap();
        let mut carrier = vec![if kind == 6 { 2 } else { 3 }];
        carrier.extend((metadata.len() as u32).to_le_bytes());
        carrier.extend(metadata);
        carrier.extend(b.raw);
        log.inner.write(&carrier).unwrap();
        drop(log);
        assert!(MessageLog::open(&f.data, &KEY, identity()).is_err());
    }
}

#[test]
fn scoped_compression_and_legacy_line_header_carrier_schema_remain_intact() {
    let f = Fixture::new();
    let mut log = f.create();
    let repetitive = b"raw-sensitive-redundancy".repeat(5000);
    let b = batch(&log, "compression", 1, &repetitive, None);
    commit(&mut log, b);
    drop(log);
    let mut directories = vec![f.data.clone()];
    let mut size = 0u64;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let ty = entry.file_type().unwrap();
            if ty.is_dir() {
                directories.push(entry.path());
            } else if ty.is_file() {
                size += entry.metadata().unwrap().len();
            } else {
                panic!("unexpected fixture file type");
            }
        }
    }
    assert!(size > 0);
    assert!(
        size < repetitive.len() as u64 / 2,
        "message carrier must reach compress-before-encrypt Store"
    );
    let line = f.root.join("line");
    let old = CaptureLog::open(&line, &KEY, identity()).unwrap();
    drop(old);
    let store = Store::open(
        &line,
        Options {
            cipher: Some(Arc::new(FrameCipher::new(&KEY))),
            require_sealed: true,
            sync: true,
            compact_threads: 1,
            ..Options::default()
        },
    )
    .unwrap();
    let mut expected = super::super::FORMAT.to_vec();
    expected.extend(serde_json::to_vec(&identity()).unwrap());
    assert_eq!(
        store.get(0).unwrap(),
        expected,
        "legacy header bytes remain exact"
    );
}

#[test]
fn actual_sigkill_before_write_after_durable_write_before_ack_and_after_ack() {
    for scenario in ["before-write", "after-write", "before-ack", "after-ack"] {
        let f = Fixture::new();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "capture_log::messages::tests::crash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("OOJURO_MESSAGE_TEST_ROOT", &f.root)
            .env("OOJURO_MESSAGE_TEST_PHASE", scenario)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let signal = f.root.join("READY");
        let until = Instant::now() + Duration::from_secs(15);
        while !signal.exists() && Instant::now() < until {
            assert!(
                child.try_wait().unwrap().is_none(),
                "owned child exited before commit seam"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(signal.exists());
        child.kill().unwrap();
        child.wait().unwrap();
        let log = f.open();
        let existing = log
            .lookup(SOURCE, 0, &request("crash-intent", 1), b"\xff\nbinary-body")
            .unwrap();
        if scenario == "before-write" {
            assert!(existing.is_none());
            assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
        } else {
            assert_eq!(existing.unwrap().outcome.first_id, Some(41));
            assert_eq!(log.read_raw(SOURCE, 0, 13).unwrap(), b"\xff\nbinary-body");
            assert_eq!(query(&log, "battery").len(), 1);
        }
    }
}
#[test]
#[ignore = "owned child fixture only"]
fn crash_child() {
    let Ok(root) = std::env::var("OOJURO_MESSAGE_TEST_ROOT") else {
        return;
    };
    let phase = std::env::var("OOJURO_MESSAGE_TEST_PHASE").unwrap();
    let data = PathBuf::from(&root).join("message-authority");
    let mut log = MessageLog::create(&data, &KEY, identity()).unwrap();
    log.register(SOURCE, b"reported-source-generation", b"{}")
        .unwrap();
    let signal = PathBuf::from(root).join("READY");
    let selected = match phase.as_str() {
        "before-write" => Some(Phase::BeforeWrite),
        "after-write" => Some(Phase::AfterWrite),
        "before-ack" => Some(Phase::BeforeAck),
        _ => None,
    };
    let marked = signal.clone();
    log.hook = Some(Box::new(move |at| {
        if selected == Some(at) {
            fs::write(&marked, b"paused").unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }));
    let b = batch(
        &log,
        "crash-intent",
        1,
        b"\xff\nbinary-body",
        Some(("battery crash fixture", true)),
    );
    let ack = log.commit(b).unwrap();
    assert!(!ack.already_committed);
    fs::write(signal, b"acknowledged").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn metadata_serialization_capacity_refuses_before_commit_and_compaction_preserves_original_intent()
{
    let f = Fixture::new();
    let mut log = f.create();
    let mut large = batch(&log, "large-metadata", 1, b"raw", Some(("bounded", true)));
    large.parsed.events[0].text = "\0".repeat(MAX_BATCH_BYTES / 6 + 1);
    assert!(log.commit(large).is_err());
    assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
    let b = batch(
        &log,
        "original-before-compact",
        1,
        b"raw\xff\nbody",
        Some(("battery compact", true)),
    );
    let request = b.parsed.request.clone();
    let raw = b.raw.clone();
    let ack = commit(&mut log, b);
    log.compact().unwrap();
    drop(log);
    let log = f.open();
    let original = log.lookup(SOURCE, 0, &request, &raw).unwrap().unwrap();
    assert_eq!(original.outcome, ack.outcome);
    assert_eq!(log.read_raw(SOURCE, 0, raw.len()).unwrap(), raw);
    assert_eq!(query(&log, "battery").len(), 1);
}

#[test]
fn existing_empty_sealed_store_is_partial_enrollment_and_cannot_be_initialized_by_open() {
    let f = Fixture::new();
    fs::create_dir(&f.data).unwrap();
    fs::write(f.data.join(MARKER), FORMAT).unwrap();
    let options = || Options {
        cipher: Some(Arc::new(cipher(&KEY, &identity()).unwrap())),
        require_sealed: true,
        sync: true,
        compact_threads: 1,
        ..Options::default()
    };
    let store = Store::open(&f.data, options()).unwrap();
    assert!(store.is_empty());
    drop(store);
    let manifest = fs::read(f.data.join("MANIFEST")).unwrap();
    assert!(MessageLog::open(&f.data, &KEY, identity()).is_err());
    let store = Store::open(&f.data, options()).unwrap();
    assert!(
        store.is_empty(),
        "read-open must never enroll a missing header"
    );
    assert_eq!(fs::read(f.data.join("MANIFEST")).unwrap(), manifest);
}
