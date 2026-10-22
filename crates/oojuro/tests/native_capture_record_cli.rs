#![cfg(target_os = "linux")]
//! Actual CLI processes; private synthetic authorities, no provider/user data.
use oojuro_capture::{
    Rec,
    ingest::{Decision, Policy},
    local::{LocalCapture, Options},
};
use oojuro_engine::capture_log::CaptureLog;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_oojuro-native");
const WRITER: &str = "codex:-synthetic-cli-bridge";
const BIG: u64 = 9_007_199_254_741_001;
static NEXT: AtomicU64 = AtomicU64::new(0);
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn success(output: Output) -> Value {
    assert!(output.status.success(), "synthetic command refused");
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn refused(output: Output) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.starts_with("oojuro-native: "));
    assert!(!error.contains("/tmp/"));
    assert!(!error.contains("SYNTHETIC_VISIBLE"));
    assert!(!error.contains("RAWONLY_WITHHELD"));
}
fn line(value: Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(&value).unwrap();
    b.push(b'\n');
    b
}
struct Fixture {
    parent: PathBuf,
    source: PathBuf,
    operator: PathBuf,
    data: PathBuf,
    control: PathBuf,
    witness: PathBuf,
    progress: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let parent = std::env::temp_dir().join(format!(
            "ocli-bridge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let progress = parent.join("progress");
        fs::create_dir(&progress).unwrap();
        fs::set_permissions(&progress, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            source: parent.join("capture"),
            operator: parent.join("operator"),
            data: parent.join("data"),
            control: parent.join("control"),
            witness: parent.join("witness"),
            parent,
            progress,
        }
    }
    fn command(&self, op: &str, root: &Path) -> Command {
        let mut c = Command::new(BIN);
        c.arg(op).arg("--root").arg(root);
        c
    }
    fn setup(&self, writer: &str) {
        let policy = self.parent.join("policy.json");
        private(
            &policy,
            &serde_json::to_vec(&json!({"version":1,"record":true,"paused":false,
            "record_thinking":true,"index_thinking":false,"exclude":[],"denied_tools":["blocked"]}))
            .unwrap(),
        );
        success(
            self.command("setup", &self.source)
                .args([
                    "--harness",
                    "codex",
                    "--writer",
                    WRITER,
                    "--agent",
                    "7",
                    "--first-id",
                    &BIG.to_string(),
                    "--policy",
                    policy.to_str().unwrap(),
                ])
                .output()
                .unwrap(),
        );
        success(
            self.command("managed-setup", &self.operator)
                .args([
                    "--writer",
                    writer,
                    "--owner",
                    "7",
                    "--adapter",
                    "capture-bridge-cli-v1",
                    "--first-id",
                    "1000",
                    "--data-root",
                    self.data.to_str().unwrap(),
                    "--control-root",
                    self.control.to_str().unwrap(),
                    "--witness-root",
                    self.witness.to_str().unwrap(),
                ])
                .output()
                .unwrap(),
        );
    }
    fn capture(&self) {
        let file = self.parent.join(".codex/sessions/rollout-synthetic.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let body=[
            line(json!({"type":"session_meta","payload":{"cwd":"/synthetic/cli-bridge","originator":"codex_cli_rs","base_instructions":{"text":""}}})),
            line(json!({"timestamp":"2026-01-02T03:04:06Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"SYNTHETIC_VISIBLE amber"}]}})),
            line(json!({"timestamp":"2026-01-02T03:04:07Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"SYNTHETIC_STORED_ONLY thinking"}]}})),
            line(json!({"timestamp":"2026-01-02T03:04:08Z","type":"response_item","payload":{"type":"function_call","name":"blocked","call_id":"blocked-fixture","arguments":"RAWONLY_WITHHELD"}})),
            line(json!({"timestamp":"2026-01-02T03:04:09Z","type":"response_item","payload":{"type":"function_call_output","call_id":"blocked-fixture","output":"RAWONLY_WITHHELD_RESULT"}})),
            line(json!({"timestamp":"2026-01-02T03:04:10Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"SYNTHETIC_VISIBLE amber"}]}})),
        ].concat();
        private(&file, &body);
        success(
            self.command("capture", &self.source)
                .args([
                    "--source",
                    file.to_str().unwrap(),
                    "--source-id",
                    "91919191919191919191919191919191",
                    "--allow-raw-source",
                ])
                .output()
                .unwrap(),
        );
    }
    fn checkpoint(&self) -> PathBuf {
        self.progress.join("CURRENT")
    }
    fn bridge(&self) -> Command {
        let mut c = self.command("managed-ingest-capture", &self.operator);
        c.arg("--capture-root")
            .arg(&self.source)
            .arg("--checkpoint")
            .arg(self.checkpoint())
            .arg("--allow-capture-ingest");
        c
    }
    fn get(&self, id: u64) -> Value {
        success(
            self.command("managed-get", &self.operator)
                .args(["--id", &id.to_string()])
                .output()
                .unwrap(),
        )
    }
    fn status(&self) -> Value {
        success(
            self.command("managed-status", &self.operator)
                .output()
                .unwrap(),
        )["authority"]
            .clone()
    }
    fn prune(&self, id: u64) {
        let input = self.parent.join("selection.json");
        private(&input, &serde_json::to_vec(&json!({"Ids":[id]})).unwrap());
        success(
            self.command("managed-prune", &self.operator)
                .args([
                    "--operation",
                    "synthetic-prune",
                    "--input",
                    input.to_str().unwrap(),
                ])
                .output()
                .unwrap(),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    fn walk(root: &Path, base: &Path, out: &mut BTreeMap<PathBuf, [u8; 32]>) {
        for item in fs::read_dir(root).unwrap() {
            let item = item.unwrap();
            let path = item.path();
            let m = fs::symlink_metadata(&path).unwrap();
            if m.is_dir() {
                walk(&path, base, out);
            } else if m.is_file() {
                out.insert(
                    path.strip_prefix(base).unwrap().into(),
                    *blake3::hash(&fs::read(path).unwrap()).as_bytes(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn actual_cli_empty_then_policy_bound_continuation_reopen_exact_retry_and_pruned_receipt() {
    let f = Fixture::new();
    f.setup(WRITER);
    let empty = success(f.bridge().output().unwrap());
    assert_eq!(empty["report"]["scanned"], 0);
    assert_eq!(f.status()["retained_snapshot_records"], 0);
    f.capture();
    let mut mappings = vec![];
    let mut stored = 0;
    let mut withheld = 0;
    loop {
        let report = success(f.bridge().args(["--receipts", "1"]).output().unwrap());
        stored += report["report"]["new"].as_u64().unwrap();
        withheld += report["report"]["withheld"].as_u64().unwrap();
        mappings.extend(
            report["report"]["mappings"]
                .as_array()
                .unwrap()
                .iter()
                .cloned(),
        );
        if report["report"]["more"] == false {
            break;
        }
    }
    assert_eq!((stored, withheld), (3, 2));
    assert_eq!(mappings.len(), 3);
    assert_eq!(f.status()["retained_snapshot_records"], 3);
    let rows: Vec<_> = mappings
        .iter()
        .map(|m| f.get(m["canonical_id"].as_u64().unwrap()))
        .collect();
    assert_eq!(rows[0]["attrs"]["capture"]["reported"]["id"], BIG);
    assert_eq!(rows[1]["indexed"], false);
    assert_eq!(rows[0]["text"], rows[2]["text"]);
    for (m, row) in mappings.iter().zip(&rows) {
        assert_eq!(row["recorded_at"], m["recorded_at"]);
        assert_eq!(
            row["valid_at"],
            row["attrs"]["capture"]["reported"]["ts_us"]
        );
        assert!(row["key"].is_null());
        assert!(
            !serde_json::to_string(row)
                .unwrap()
                .contains("RAWONLY_WITHHELD")
        );
    }
    fs::remove_file(f.checkpoint()).unwrap();
    let replay = success(f.bridge().output().unwrap());
    assert_eq!(replay["report"]["new"], 0);
    assert_eq!(replay["report"]["already_committed"], 3);
    assert_eq!(replay["report"]["mappings"], json!(mappings));
    for (mapping, row) in mappings.iter().zip(rows) {
        assert_eq!(f.get(mapping["canonical_id"].as_u64().unwrap()), row);
    }
    f.prune(1000);
    fs::remove_file(f.checkpoint()).unwrap();
    let historical = success(f.bridge().output().unwrap());
    assert_eq!(historical["report"]["removed"], 1);
    assert_eq!(historical["report"]["mappings"][0]["receipt_only"], true);
    assert!(f.get(1000).is_null());
    assert_eq!(f.status()["retained_snapshot_records"], 2);
    success(f.command("managed-deny", &f.operator).output().unwrap());
    fs::remove_file(f.checkpoint()).unwrap();
    refused(f.bridge().output().unwrap());
    assert!(!f.checkpoint().exists());
}
#[test]
fn consent_precedes_inspection_and_missing_authorities_never_bootstrap() {
    let f = Fixture::new();
    let output = f
        .command("managed-ingest-capture", &f.operator)
        .arg("--capture-root")
        .arg(&f.source)
        .arg("--checkpoint")
        .arg(f.checkpoint())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("consent_required"));
    refused(output);
    assert!(!f.operator.exists());
    assert!(!f.source.exists());
    assert!(!f.checkpoint().exists());
    f.setup(WRITER);
    for path in [
        f.source.join("journal/MANIFEST"),
        f.control.join("MANIFEST"),
        f.witness.join("CURRENT"),
        f.data.join("ENROLLMENT"),
    ] {
        let held = path.with_extension("synthetic-held");
        fs::rename(&path, &held).unwrap();
        let before = snapshot(&f.parent);
        refused(f.bridge().output().unwrap());
        assert!(!path.exists());
        assert!(!f.checkpoint().exists());
        assert!(
            before == snapshot(&f.parent),
            "missing authority was mutated"
        );
        fs::rename(held, path).unwrap();
    }
    success(f.bridge().output().unwrap());
}
#[test]
fn checkpoint_layout_realpath_and_scope_refuse_without_appending() {
    let f = Fixture::new();
    f.setup(WRITER);
    f.capture();
    let before = f.status();
    let nested = f.source.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
    for checkpoint in [
        nested.join("CURRENT"),
        f.operator.join("CURRENT"),
        f.data.join("CURRENT"),
        f.control.join("CURRENT"),
        f.witness.join("other"),
        f.parent.join("CURRENT"),
    ] {
        refused(
            f.command("managed-ingest-capture", &f.operator)
                .arg("--capture-root")
                .arg(&f.source)
                .arg("--checkpoint")
                .arg(&checkpoint)
                .arg("--allow-capture-ingest")
                .output()
                .unwrap(),
        );
        assert!(!checkpoint.exists());
    }
    let alias = f.parent.join("alias");
    symlink(&f.parent, &alias).unwrap();
    refused(
        f.command("managed-ingest-capture", &f.operator)
            .arg("--capture-root")
            .arg(alias.join("capture"))
            .arg("--checkpoint")
            .arg(f.checkpoint())
            .arg("--allow-capture-ingest")
            .output()
            .unwrap(),
    );
    assert_eq!(f.status(), before);
    assert!(!f.checkpoint().exists());
    refused(f.bridge().args(["--receipts", "0"]).output().unwrap());
    refused(f.bridge().args(["--input-bytes", "0"]).output().unwrap());
    assert_eq!(f.status(), before);
    assert!(!f.checkpoint().exists());
    // No-output receipts can advance without spending the input byte budget.
    let partial = success(f.bridge().args(["--input-bytes", "1"]).output().unwrap());
    assert_eq!(partial["report"]["new"], 0);
    assert_eq!(partial["report"]["no_output"], 1);
    assert_eq!(partial["report"]["more"], true);
    let progress = fs::read(f.checkpoint()).unwrap();
    refused(f.bridge().args(["--input-bytes", "1"]).output().unwrap());
    assert!(
        fs::read(f.checkpoint()).unwrap() == progress,
        "oversized Stored emission advanced progress"
    );
    assert_eq!(f.status(), before);
    fs::remove_file(f.checkpoint()).unwrap();
    let wrong = Fixture::new();
    wrong.setup("different-target-writer");
    refused(
        wrong
            .command("managed-ingest-capture", &wrong.operator)
            .arg("--capture-root")
            .arg(&f.source)
            .arg("--checkpoint")
            .arg(wrong.checkpoint())
            .arg("--allow-capture-ingest")
            .output()
            .unwrap(),
    );
    assert_eq!(wrong.status()["retained_snapshot_records"], 0);
}
#[test]
fn actual_stdout_ack_loss_keeps_commits_and_same_checkpoint_resumes_without_duplicate() {
    let f = Fixture::new();
    f.setup(WRITER);
    f.capture();
    // Close the peer before spawning: deterministic lost stdout, no scheduling race.
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    let stdout: std::os::fd::OwnedFd = writer.into();
    let mut child = f
        .bridge()
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI exceeded synthetic bound");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    assert!(f.checkpoint().exists());
    assert_eq!(f.status()["retained_snapshot_records"], 3);
    let first = f.get(1000);
    let resumed = success(f.bridge().output().unwrap());
    assert_eq!(resumed["report"]["scanned"], 0);
    assert_eq!(resumed["report"]["new"], 0);
    assert_eq!(f.get(1000), first);
}
struct NoWrite;
impl Policy for NoWrite {
    fn decide(&mut self, _: &Rec, _: &str) -> Decision {
        Decision::Withhold("synthetic-read-only")
    }
}
#[test]
fn cli_replay_after_actual_capture_compaction_keeps_original_canonical_ids_and_times() {
    let f = Fixture::new();
    f.setup(WRITER);
    f.capture();
    let first = success(f.bridge().output().unwrap());
    let row = f.get(1000);
    let key: [u8; 32] = fs::read(f.source.join("key"))
        .unwrap()
        .try_into()
        .unwrap_or_else(|_| panic!("synthetic fixture key length")); // synthetic fixture only; never output.
    let mut options = Options::new(WRITER, 7, "synthetic-read-only");
    options.first_id = BIG;
    let capture =
        LocalCapture::open_existing(&f.source.join("journal"), &key, options, Box::new(NoWrite))
            .unwrap();
    let identity = capture.authority().unwrap().identity().clone();
    drop(capture);
    let mut source = CaptureLog::open_existing(&f.source.join("journal"), &key, identity).unwrap();
    source.compact().unwrap();
    drop(source);
    fs::remove_file(f.checkpoint()).unwrap();
    let replay = success(f.bridge().output().unwrap());
    assert_eq!(replay["report"]["new"], 0);
    assert_eq!(replay["report"]["already_committed"], 3);
    assert_eq!(replay["report"]["mappings"], first["report"]["mappings"]);
    assert_eq!(f.get(1000), row);
}
