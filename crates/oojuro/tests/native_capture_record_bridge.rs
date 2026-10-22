//! Synthetic local capture -> real retained managed authority, without provider calls.
#![cfg(unix)]
use oojuro_capture::{
    Rec,
    ingest::{Decision, Policy},
    kind,
    local::{LocalCapture, Options},
};
use oojuro_engine::{
    audit::{Domain, Signer},
    capture_log::Identity,
    capture_record::{Limits, ingest},
    record::{
        self, AsOf, Filter, Scope,
        managed::{self, FileWitness, ManagedLog, RetainedControl},
        ranking::Pipeline,
    },
};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const WRITER: &str = "codex:-synthetic-bridge";
const CAPTURE_KEY: [u8; 32] = [71; 32];
const DATA_KEY: [u8; 32] = [72; 32];
const CONTROL_KEY: [u8; 32] = [73; 32];
const AUTHORITY: [u8; 32] = [74; 32];
const FIRST: u64 = 9_007_199_254_741_001;
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
                "native-bridge-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("progress"))
            .unwrap();
        fs::create_dir_all(root.join(".codex/sessions")).unwrap();
        Self {
            root,
            signing: Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref()
                .to_vec(),
        }
    }
    fn identity(&self) -> Identity {
        Identity {
            writer: WRITER.into(),
            agent: 7,
            first_id: 1000,
            adapter: "capture-record-bridge-v1".into(),
        }
    }
    fn control(&self) -> Arc<RetainedControl> {
        Arc::new(
            RetainedControl::open(
                &self.root.join("control"),
                &CONTROL_KEY,
                Signer::from_pkcs8(AUTHORITY, Domain::NativeLifecycle, &self.signing).unwrap(),
                AUTHORITY,
                FileWitness::open(&self.root.join("witness")).unwrap(),
                managed::identity_binding(&DATA_KEY, &self.identity()).unwrap(),
            )
            .unwrap(),
        )
    }
    fn target(&self, c: Arc<RetainedControl>) -> ManagedLog {
        if self.root.join("data").exists() {
            ManagedLog::open(&self.root.join("data"), &DATA_KEY, self.identity(), c).unwrap()
        } else {
            ManagedLog::create(&self.root.join("data"), &DATA_KEY, self.identity(), c).unwrap()
        }
    }
    fn capture(&self) -> LocalCapture {
        let mut options = Options::new(WRITER, 7, "synthetic-policy-v1");
        options.first_id = FIRST;
        options.recorded_at = || 123;
        LocalCapture::open(
            &self.root.join("capture"),
            &CAPTURE_KEY,
            options,
            Box::new(Strict),
        )
        .unwrap()
    }
    fn progress(&self) -> PathBuf {
        self.root.join("progress/CURRENT")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
struct Strict;
impl Policy for Strict {
    fn decide(&mut self, record: &Rec, _: &str) -> Decision {
        if record.kind == kind::THINKING {
            return Decision::Withhold("synthetic-thinking-off");
        }
        Decision::Store {
            indexed: !record.text.contains("STOREONLY") && kind::indexed(record.kind),
        }
    }
}
fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
fn output(text: &str) -> Vec<u8> {
    line(
        json!({"timestamp":"2026-01-02T03:04:06Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}}),
    )
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 7,
    }
}
#[test]
fn local_capture_bridge_reopen_preserves_originals_policy_redaction_and_real_known_time() {
    let f = Fixture::new();
    let file = f.root.join(".codex/sessions/rollout-synthetic.jsonl");
    let synthetic_token = format!("{}{}", "sk-ant-api03-", "syntheticAb0123".repeat(6));
    let bytes=[
        line(json!({"type":"session_meta","payload":{"cwd":"/synthetic/bridge","originator":"codex_cli_rs","base_instructions":{"text":""}}})),
        output("amber battery searchable"),
        output("STOREONLY battery hidden"),
        line(json!({"timestamp":"2026-01-02T03:04:07Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"WITHHELDONLYTHOUGHT"}]}})),
        output(&format!("synthetic credential {synthetic_token}")),
        output("amber battery searchable"),
    ].concat();
    fs::write(&file, &bytes).unwrap();
    let mut capture = f.capture();
    let captured = capture
        .capture_file([91; 16], &file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(captured.event_ids.len(), 4);
    let raw_before = capture
        .authority()
        .unwrap()
        .read_raw([91; 16], 0, bytes.len())
        .unwrap();
    assert_eq!(raw_before, bytes);
    let control = f.control();
    let mut target = f.target(control.clone());
    let mut mappings = vec![];
    let mut total_new = 0;
    let mut withheld = 0;
    loop {
        let report = ingest(
            capture.authority().unwrap(),
            &mut target,
            scope(),
            &f.progress(),
            Limits {
                receipts: 2,
                bytes: 32 << 20,
            },
        )
        .unwrap();
        total_new += report.new;
        withheld += report.withheld;
        mappings.extend(report.mappings);
        if !report.more {
            break;
        }
    }
    assert_eq!(total_new, 4);
    assert_eq!(withheld, 1);
    assert_eq!(mappings.len(), 4);
    for mapping in &mappings {
        let original = capture.get(mapping.source_id).unwrap().unwrap().0;
        let row = target.get(mapping.canonical_id, 1 << 20).unwrap().unwrap();
        assert_eq!(row.text, original.text);
        assert_eq!(row.indexed, original.indexed);
        assert_eq!(row.valid_at, original.ts_us);
        assert_eq!(row.recorded_at, mapping.recorded_at);
        assert_ne!(row.recorded_at, 123);
        assert!(row.key.is_none());
        assert_eq!(
            row.attrs["capture"]["reported"]["id"].as_u64(),
            Some(mapping.source_id)
        );
        assert_eq!(row.attrs["capture"]["policy_id"], "synthetic-policy-v1");
        assert!(!row.text.contains(&synthetic_token));
        assert!(!row.text.contains("WITHHELDONLYTHOUGHT"));
    }
    let saved: Vec<_> = mappings
        .iter()
        .map(|m| {
            serde_json::to_value(target.get(m.canonical_id, 1 << 20).unwrap().unwrap()).unwrap()
        })
        .collect();
    let before = target.binding().unwrap();
    let first_at = mappings[0].recorded_at;
    target
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            let hits = view.search_ranked("amber", 10, 1 << 20, &Pipeline::default())?;
            assert_eq!(hits.hits.len(), 2); // Canonical Record retains both eligible originals.
            assert!(
                view.search_ranked("STOREONLY", 10, 1 << 20, &Pipeline::default())?
                    .hits
                    .is_empty()
            );
            assert!(
                view.search_ranked("WITHHELDONLYTHOUGHT", 10, 1 << 20, &Pipeline::default())?
                    .hits
                    .is_empty()
            );
            assert!(
                view.search_ranked("key:missing", 10, 1 << 20, &Pipeline::default())?
                    .hits
                    .is_empty()
            );
            Ok(())
        })
        .unwrap();
    target
        .with_rank_view(
            scope(),
            AsOf {
                known_at: first_at - 1,
                ..AsOf::default()
            },
            Filter::default(),
            |view| {
                assert!(
                    view.search_ranked("", 10, 1 << 20, &Pipeline::default())?
                        .hits
                        .is_empty()
                );
                Ok(())
            },
        )
        .unwrap();
    let signature = |target: &ManagedLog| {
        target
            .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
                Ok(view
                    .search_ranked("amber", 10, 1 << 20, &Pipeline::default())?
                    .hits
                    .into_iter()
                    .map(|hit| {
                        (
                            hit.record.id,
                            hit.lexical_bits.to_bits(),
                            hit.final_bits.to_bits(),
                            hit.continued,
                            hit.record.attrs["capture"]["reported"]["id"]
                                .as_u64()
                                .unwrap(),
                        )
                    })
                    .collect::<Vec<_>>())
            })
            .unwrap()
    };
    let original_signature = signature(&target);
    assert_eq!(original_signature.len(), 2);
    drop(target);
    drop(control);
    drop(capture);
    let capture = f.capture();
    let control = f.control();
    let mut target = f.target(control);
    fs::remove_file(f.progress()).unwrap();
    let replay = ingest(
        capture.authority().unwrap(),
        &mut target,
        scope(),
        &f.progress(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!((replay.new, replay.already_committed), (0, 4));
    assert_eq!(target.binding().unwrap(), before);
    assert_eq!(signature(&target), original_signature);
    for (mapping, original) in mappings.iter().zip(saved) {
        assert_eq!(
            serde_json::to_value(target.get(mapping.canonical_id, 1 << 20).unwrap().unwrap())
                .unwrap(),
            original
        );
    }
    assert_eq!(
        capture
            .authority()
            .unwrap()
            .read_raw([91; 16], 0, bytes.len())
            .unwrap(),
        raw_before
    );
    assert!(
        fs::read(f.progress())
            .unwrap()
            .windows(WRITER.len())
            .all(|w| w != WRITER.as_bytes())
    );
}
#[test]
fn bridge_does_not_create_key_versions_or_replace_explicit_canonical_memory_history() {
    let f = Fixture::new();
    let file = f.root.join(".codex/sessions/rollout-synthetic.jsonl");
    fs::write(&file, [
        line(json!({"type":"session_meta","payload":{"cwd":"/synthetic/bridge","originator":"codex_cli_rs","base_instructions":{"text":""}}})),
        output("captured original"),
    ].concat()).unwrap();
    let mut capture = f.capture();
    capture
        .capture_file([92; 16], &file, |_| true)
        .unwrap()
        .unwrap();
    let mut target = f.target(f.control());
    let mut memory = record::Input {
        agent_id: "explicit-owner".into(),
        parent_id: None,
        valid_at: 1,
        kind: record::Kind::Explicit,
        tool: None,
        status: None,
        exit: None,
        key: Some("memory".into()),
        text: "old".into(),
        attrs: Default::default(),
        indexed: true,
    };
    let old = target.append("memory-old", &[memory.clone()]).unwrap();
    memory.text = "new".into();
    memory.valid_at = 2;
    let new = target.append("memory-new", &[memory]).unwrap();
    let report = ingest(
        capture.authority().unwrap(),
        &mut target,
        scope(),
        &f.progress(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(report.new, 1);
    target
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            let hits = view.search_ranked("memory", 10, 1 << 20, &Pipeline::default())?;
            assert_eq!(hits.hits[0].record.id, new.commit.ids[0]);
            assert_eq!(hits.hits[0].record.text, "new");
            Ok(())
        })
        .unwrap();
    assert_eq!(
        target
            .get(old.commit.ids[0], 1 << 20)
            .unwrap()
            .unwrap()
            .text,
        "old"
    );
}
