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
            "ocli-paged-{}-{}",
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
            self.command("managed-paged-setup", &self.operator)
                .args([
                    "--writer",
                    writer,
                    "--owner",
                    "7",
                    "--adapter",
                    "capture-paged-bridge-cli-v1",
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
        let mut c = self.command("managed-paged-ingest-capture", &self.operator);
        c.arg("--capture-root")
            .arg(&self.source)
            .arg("--checkpoint")
            .arg(self.checkpoint())
            .arg("--allow-capture-ingest");
        c
    }
    fn get(&self, id: u64) -> Value {
        success(
            self.command("managed-paged-get", &self.operator)
                .args(["--id", &id.to_string()])
                .output()
                .unwrap(),
        )
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
fn actual_paged_cli_capture_restart_policy_compaction_and_exact_first_ingestion() {
    let f = Fixture::new();
    f.setup(WRITER);
    assert!(!f.operator.join("managed.json").exists());
    assert!(f.operator.join("managed-paged.json").exists());
    assert!(!f.control.join("MANIFEST").exists());
    let empty = success(f.bridge().output().unwrap());
    assert_eq!(empty["report"]["scanned"], 0);
    f.capture();
    let mut mappings = Vec::new();
    let mut withheld = 0;
    loop {
        let r = success(f.bridge().args(["--receipts", "1"]).output().unwrap());
        assert!(
            !String::from_utf8(serde_json::to_vec(&r).unwrap())
                .unwrap()
                .contains("SYNTHETIC_VISIBLE")
        );
        withheld += r["report"]["withheld"].as_u64().unwrap();
        mappings.extend(r["report"]["mappings"].as_array().unwrap().iter().cloned());
        if !r["report"]["more"].as_bool().unwrap() {
            break;
        }
    }
    assert_eq!(withheld, 2);
    assert_eq!(mappings.len(), 3);
    let originals: Vec<_> = (1000..1003).map(|id| f.get(id)).collect();
    assert_eq!(originals[0]["text"], "SYNTHETIC_VISIBLE amber");
    assert_eq!(originals[1]["indexed"], false);
    assert_eq!(originals[2]["text"], originals[0]["text"]);
    assert_eq!(originals[0]["attrs"]["capture"]["reported"]["id"], BIG);
    assert_eq!(originals[0]["valid_at"], 1767323046000000i64);
    assert!(
        originals[0]["recorded_at"].as_i64().unwrap() > originals[0]["valid_at"].as_i64().unwrap()
    );
    fs::remove_file(f.checkpoint()).unwrap();
    let retry = success(f.bridge().output().unwrap());
    assert_eq!(retry["report"]["new"], 0);
    assert_eq!(retry["report"]["already_committed"], 3);
    assert_eq!(retry["report"]["mappings"], json!(mappings));
    let key: [u8; 32] = fs::read(f.source.join("key"))
        .unwrap()
        .try_into()
        .unwrap_or_else(|_| panic!("synthetic key length"));
    struct ReadOnly;
    impl Policy for ReadOnly {
        fn decide(&mut self, _: &Rec, _: &str) -> Decision {
            Decision::Withhold("synthetic-read-only")
        }
    }
    let mut options = Options::new(WRITER, 7, "synthetic-read-only");
    options.first_id = BIG;
    let capture =
        LocalCapture::open_existing(&f.source.join("journal"), &key, options, Box::new(ReadOnly))
            .unwrap();
    let identity = capture.authority().unwrap().identity().clone();
    drop(capture);
    let mut capture = CaptureLog::open_existing(&f.source.join("journal"), &key, identity).unwrap();
    capture.compact().unwrap();
    drop(capture);
    fs::remove_file(f.checkpoint()).unwrap();
    let after = success(f.bridge().output().unwrap());
    assert_eq!(after["report"]["new"], 0);
    assert_eq!(after["report"]["mappings"], retry["report"]["mappings"]);
    for (i, original) in originals.iter().enumerate() {
        assert_eq!(&f.get(1000 + i as u64), original);
    }
}
#[test]
fn paged_selector_consent_missing_authorities_and_unsafe_paths_never_bootstrap() {
    let missing = Fixture::new();
    let no = missing
        .command("managed-paged-ingest-capture", &missing.operator)
        .arg("--capture-root")
        .arg(&missing.source)
        .arg("--checkpoint")
        .arg(missing.checkpoint())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&no.stderr).contains("consent_required"));
    assert!(!missing.operator.exists() && !missing.source.exists());
    let f = Fixture::new();
    f.setup(WRITER);
    for file in [
        f.source.join("journal/MANIFEST"),
        f.control.join("PAGED_CONTROL"),
        f.data.join("ENROLLMENT"),
        f.witness.join("CURRENT"),
    ] {
        let temp = file.with_extension("held");
        fs::rename(&file, &temp).unwrap();
        let before = snapshot(&f.parent);
        refused(f.bridge().output().unwrap());
        assert!(!file.exists());
        assert_eq!(snapshot(&f.parent), before);
        fs::rename(&temp, &file).unwrap();
    }
    for parent in [
        &f.source,
        &f.operator,
        &f.data,
        &f.control,
        &f.witness,
        &f.parent,
    ] {
        let checkpoint = parent.join("forbidden-checkpoint");
        refused(
            f.command("managed-paged-ingest-capture", &f.operator)
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
    symlink(&f.progress, &alias).unwrap();
    refused(
        f.command("managed-paged-ingest-capture", &f.operator)
            .arg("--capture-root")
            .arg(&f.source)
            .arg("--checkpoint")
            .arg(alias.join("checkpoint"))
            .arg("--allow-capture-ingest")
            .output()
            .unwrap(),
    );
    refused(
        f.command("managed-get", &f.operator)
            .args(["--id", "1000"])
            .output()
            .unwrap(),
    );
    // A file rename cannot turn the actual paged authority into a snapshot.
    fs::copy(
        f.operator.join("managed-paged.json"),
        f.operator.join("managed.json"),
    )
    .unwrap();
    refused(
        f.command("managed-get", &f.operator)
            .args(["--id", "1000"])
            .output()
            .unwrap(),
    );
    let wrong = Fixture::new();
    wrong.setup("different-writer");
    wrong.capture();
    refused(wrong.bridge().output().unwrap());
    assert!(!wrong.checkpoint().exists());
    assert!(f.get(1000).is_null());
    // Explicit setup never re-enrolls or resets the existing target.
    let before = snapshot(&f.parent);
    refused(
        f.command("managed-paged-setup", &f.operator)
            .arg("--data-root")
            .arg(&f.data)
            .arg("--control-root")
            .arg(&f.control)
            .arg("--witness-root")
            .arg(&f.witness)
            .args(["--writer", WRITER, "--owner", "7", "--adapter", "refused"])
            .output()
            .unwrap(),
    );
    assert_eq!(snapshot(&f.parent), before);
}
#[test]
fn actual_paged_stdout_ack_loss_and_oversized_emission_resume_without_duplicate() {
    let f = Fixture::new();
    f.setup(WRITER);
    f.capture();
    refused(f.bridge().args(["--input-bytes", "0"]).output().unwrap());
    assert!(!f.checkpoint().exists());
    let partial = success(f.bridge().args(["--input-bytes", "1"]).output().unwrap());
    assert_eq!(partial["report"]["new"], 0);
    assert_eq!(partial["report"]["more"], true);
    let before = fs::read(f.checkpoint()).unwrap();
    refused(f.bridge().args(["--input-bytes", "1"]).output().unwrap());
    assert_eq!(fs::read(f.checkpoint()).unwrap(), before);
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let output: OwnedFd = writer.into();
    let mut child = f
        .bridge()
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("synthetic child timeout");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let original = f.get(1000);
    let retry = success(f.bridge().output().unwrap());
    assert_eq!(retry["report"]["scanned"], 0);
    assert_eq!(retry["report"]["new"], 0);
    assert_eq!(f.get(1000), original);
    assert!(f.get(1003).is_null());
}
