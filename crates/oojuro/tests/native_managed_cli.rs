#![cfg(target_os = "linux")]
//! Synthetic real-process stories; all key material stays in private throwaway directories.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
const BIN: &str = env!("CARGO_BIN_EXE_oojuro-native");
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    data: PathBuf,
    control: PathBuf,
    witness: PathBuf,
    input: PathBuf,
}
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let parent = std::env::temp_dir().join(format!(
            "oojuro-managed-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root: parent.join("operator"),
            data: parent.join("payload"),
            control: parent.join("control"),
            witness: parent.join("independent-witness"),
            input: parent.join("PRIVATE-INPUT.json"),
            parent,
        }
    }
    fn call(&self, command: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg(command)
            .args(["--root", self.root.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap()
    }
    fn setup(&self) -> Output {
        self.call(
            "managed-setup",
            &[
                "--data-root",
                self.data.to_str().unwrap(),
                "--control-root",
                self.control.to_str().unwrap(),
                "--witness-root",
                self.witness.to_str().unwrap(),
                "--writer",
                "synthetic-writer",
                "--owner",
                "7",
                "--adapter",
                "caller-sdk",
                "--first-id",
                "41",
            ],
        )
    }
    fn input(&self, value: Value) {
        private(&self.input, &serde_json::to_vec(&value).unwrap());
    }
    fn append(&self, request: &str) -> Output {
        self.call(
            "managed-append",
            &[
                "--request",
                request,
                "--input",
                self.input.to_str().unwrap(),
                "--allow-unredacted-records",
            ],
        )
    }
    fn prune(&self, operation: &str, selection: Value) -> Value {
        self.input(selection);
        ok(self.call(
            "managed-prune",
            &[
                "--operation",
                operation,
                "--input",
                self.input.to_str().unwrap(),
            ],
        ))
    }
    fn export(&self) -> (PathBuf, Vec<Value>) {
        let mut start = 0u64;
        let mut floor = None;
        let mut lines = Vec::new();
        let mut pages = vec![];
        loop {
            let from = start.to_string();
            let encoded;
            let mut args = vec!["--start", from.as_str(), "--entries", "1"];
            if let Some(value) = &floor {
                encoded = serde_json::to_string(value).unwrap();
                args.extend(["--floor", encoded.as_str()]);
            }
            let output = self.call("managed-export", &args);
            let page = ok_output(&output);
            lines.extend(output.stdout);
            floor = Some(page["current"].clone());
            let next = page["next"].as_u64();
            pages.push(page);
            if let Some(next) = next {
                start = next
            } else {
                break;
            }
        }
        let path = self.parent.join("AUDIT-NO-CONTENT.jsonl");
        private(&path, &lines);
        (path, pages)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
fn ok_output(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn ok(output: Output) -> Value {
    ok_output(&output)
}
fn refused(output: Output) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "refusal leaked output");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.starts_with("oojuro-native: "));
    assert!(!error.contains("/tmp/"), "private path leaked");
    assert!(!error.contains("PRIVATE-INPUT"), "private path leaked");
}
fn row(text: &str, key: Option<&str>, agent: &str, valid_at: i64) -> Value {
    json!({"agent_id":agent,"parent_id":null,"valid_at":valid_at,"kind":"explicit","tool":null,"status":null,
        "exit":null,"key":key,"text":text,"attrs":{},"indexed":true})
}
fn status(f: &Fixture) -> Value {
    ok(f.call("managed-status", &[]))["authority"].clone()
}
fn write_pages(path: &Path, pages: &[Value]) {
    let mut bytes = Vec::new();
    for page in pages {
        bytes.extend(serde_json::to_vec(page).unwrap());
        bytes.push(b'\n');
    }
    private(path, &bytes);
}

#[test]
fn actual_setup_append_reopen_original_retry_prune_and_historical_ack() {
    let f = Fixture::new();
    assert_eq!(ok(f.setup())["namespace"], "oojuro-managed-record-3");
    for root in [&f.root, &f.data, &f.control, &f.witness] {
        assert_eq!(
            fs::metadata(root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    for name in ["managed.json", "data-key", "control-key", "signing.pk8"] {
        assert_eq!(
            fs::metadata(f.root.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let a = row("SYNTHETIC_SELECTED_ALPHA", Some("memory"), "a", 10);
    let b = row("SYNTHETIC_SURVIVOR_BETA", None, "b", 20);
    let original = json!([a, b]);
    f.input(original.clone());
    let ack = ok(f.append("exact-request"));
    assert_eq!(ack["commit"]["ids"], json!([41, 42]));
    assert_eq!(ok(f.append("exact-request")), ack);
    let found = ok(f.call("managed-get", &["--id", "41"]));
    assert_eq!(found["adapter"], "caller-sdk");
    assert_eq!(found["agent_id"], "a");
    assert_eq!(found["text"], "SYNTHETIC_SELECTED_ALPHA");
    let report = f.prune("prune-one", json!({"Ids":[41]}));
    assert_eq!(report["removed_ids"], json!([41]));
    assert!(
        report["all_owned_prior_generations_removed"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(report["permanent_erasure"], "unsupported_caller_owned_keys");
    assert!(ok(f.call("managed-get", &["--id", "41"])).is_null());
    assert_eq!(ok(f.call("managed-get", &["--id", "42"]))["id"], 42);
    f.input(original);
    let retry = ok(f.append("exact-request"));
    assert_eq!(retry["commit"], ack["commit"]);
    assert_eq!(retry["receipt_only"], true);
    assert_eq!(retry["removed_ids"], json!([41]));
    f.input(json!([row("SYNTHETIC_THIRD_GAMMA", None, "b", 30)]));
    assert_eq!(ok(f.append("next-request"))["commit"]["ids"], json!([43]));
    let past = f.prune("prune-one", json!({"Ids":[41]}));
    assert_eq!(past["historical"], true);
    assert_eq!(past["local_live_absence_verified"], false);
    f.input(json!([row("changed payload", None, "b", 30)]));
    refused(f.append("exact-request"));
    assert_eq!(status(&f)["retained_snapshot_records"], 2);
}

#[test]
fn actual_agent_key_and_engine_recorded_cutoff_selections() {
    let f = Fixture::new();
    ok(f.setup());
    f.input(json!([
        row("agent-only", None, "remove-me", i64::MAX),
        row("key-version1", Some("versions"), "keep", 10),
        row("key-version2", Some("versions"), "keep", 20),
        row("late-valid-time", None, "keep", i64::MIN)
    ]));
    let ack = ok(f.append("selection-input"));
    assert_eq!(
        f.prune("agent-op", json!({"Agent":"remove-me"}))["removed_ids"],
        json!([41])
    );
    assert_eq!(
        f.prune("key-op", json!({"Key":"versions"}))["removed_ids"],
        json!([42, 43])
    );
    let cutoff = ack["commit"]["recorded_at"][3]
        .as_i64()
        .unwrap()
        .checked_add(1)
        .unwrap();
    let report = f.prune("retention-op", json!({"RecordedBefore":cutoff}));
    assert_eq!(report["removed_ids"], json!([44]));
    assert_eq!(report["receipt"]["details"]["recorded_before"], cutoff);
    assert_eq!(status(&f)["retained_snapshot_records"], 0);
}

#[test]
fn actual_audit_pages_verify_current_chain_reject_mutation_truncation_and_stale_resume() {
    let f = Fixture::new();
    ok(f.setup());
    f.input(json!([row(
        "PRIVATE_PAYLOAD_NOT_AUDIT",
        None,
        "reported",
        1
    )]));
    ok(f.append("original"));
    f.prune("prune", json!({"Ids":[41]}));
    let (path, pages) = f.export();
    assert!(pages.len() > 3);
    let bytes = fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_PAYLOAD_NOT_AUDIT"));
    let result = ok(f.call("managed-verify", &["--input", path.to_str().unwrap()]));
    assert_eq!(result["status"], "verified");
    assert_eq!(result["pages"], pages.len());
    refused(f.call("managed-export", &["--bytes", "16777217"]));
    refused(f.call(
        "managed-verify",
        &["--input", path.to_str().unwrap(), "--max-input-bytes", "1"],
    ));
    let original = bytes;
    let mut mutated = pages.clone();
    let first = mutated[1]["entries"][0]["ciphertext_digest"][0]
        .as_u64()
        .unwrap();
    mutated[1]["entries"][0]["ciphertext_digest"][0] = json!(first ^ 1);
    write_pages(&path, &mutated);
    refused(f.call("managed-verify", &["--input", path.to_str().unwrap()]));
    write_pages(&path, &pages[..pages.len() - 1]);
    refused(f.call("managed-verify", &["--input", path.to_str().unwrap()]));
    let mut reordered = pages.clone();
    reordered.swap(0, 1);
    write_pages(&path, &reordered);
    refused(f.call("managed-verify", &["--input", path.to_str().unwrap()]));
    private(&path, &original);
    let old_floor = serde_json::to_string(&pages[0]["current"]).unwrap();
    f.input(json!([row("new current generation", None, "reported", 2)]));
    ok(f.append("next"));
    refused(f.call("managed-export", &["--start", "1", "--floor", &old_floor]));
    refused(f.call("managed-export", &["--start", "1"]));
    refused(f.call("managed-verify", &["--input", path.to_str().unwrap()]));
}

#[test]
fn logical_denial_is_current_and_pending_permanent_not_a_destroyed_key_claim() {
    let f = Fixture::new();
    ok(f.setup());
    f.input(json!([row(
        "SYNTHETIC_BACKUP_PAYLOAD",
        None,
        "reported",
        1
    )]));
    ok(f.append("before-deny"));
    let before = status(&f);
    let denial = ok(f.call("managed-deny", &[]));
    assert_eq!(denial["authority"]["status"], "denied_pending_permanent");
    assert_eq!(
        denial["authority"]["retained_snapshot_records"],
        before["retained_snapshot_records"]
    );
    assert_eq!(denial["authority"]["local_cleanup_completed"], false);
    assert_eq!(denial["permanent_erasure"], "unsupported_caller_owned_keys");
    assert!(f.root.join("data-key").exists());
    assert!(f.data.exists());
    assert_eq!(ok(f.call("managed-deny", &[])), denial);
    refused(f.call("managed-open", &[]));
    refused(f.call("managed-get", &["--id", "41"]));
    refused(f.append("after-deny"));
    fs::remove_dir_all(&f.data).unwrap();
    assert_eq!(status(&f)["status"], "denied_pending_permanent");
    let (path, _) = f.export();
    assert_eq!(
        ok(f.call("managed-verify", &["--input", path.to_str().unwrap()]))["status"],
        "verified"
    );
    assert!(
        !f.data.exists(),
        "status/export must not recreate unavailable payload storage"
    );
}

#[test]
fn setup_refuses_existing_or_overlapping_namespace_without_reset() {
    let f = Fixture::new();
    fs::create_dir(&f.data).unwrap();
    refused(f.setup());
    assert!(!f.root.exists());
    assert!(!f.control.exists());
    assert!(!f.witness.exists());
    fs::remove_dir(&f.data).unwrap();
    let same = f.root.to_str().unwrap();
    refused(f.call(
        "managed-setup",
        &[
            "--data-root",
            same,
            "--control-root",
            f.control.to_str().unwrap(),
            "--witness-root",
            f.witness.to_str().unwrap(),
            "--writer",
            "w",
            "--owner",
            "1",
            "--adapter",
            "sdk",
        ],
    ));
    assert!(!f.root.exists());
    ok(f.setup());
    let cfg = fs::read(f.root.join("managed.json")).unwrap();
    refused(f.setup());
    assert_eq!(fs::read(f.root.join("managed.json")).unwrap(), cfg);
    refused(f.call(
        "record-setup",
        &["--writer", "w", "--owner", "1", "--adapter", "sdk"],
    ));
    assert_eq!(fs::read(f.root.join("managed.json")).unwrap(), cfg);
    let partial = Fixture::new();
    fs::create_dir(&partial.root).unwrap();
    refused(partial.setup());
    assert!(!partial.data.exists());
}

#[test]
fn consent_precedes_inspection_and_private_inputs_keys_paths_remain_protected() {
    let f = Fixture::new();
    let denied = f.call(
        "managed-append",
        &["--input", f.input.to_str().unwrap(), "--request", "r"],
    );
    assert!(!denied.status.success());
    assert_eq!(
        String::from_utf8_lossy(&denied.stderr),
        "oojuro-native: explicit_unredacted_record_consent_required\n"
    );
    assert!(!f.root.exists());
    ok(f.setup());
    f.input(json!([row("NEVER_IN_ERROR_OUTPUT", None, "reported", 1)]));
    fs::set_permissions(&f.input, fs::Permissions::from_mode(0o644)).unwrap();
    refused(f.append("public-input"));
    fs::set_permissions(&f.input, fs::Permissions::from_mode(0o600)).unwrap();
    let link = f.parent.join("input-link");
    symlink(&f.input, &link).unwrap();
    refused(f.call(
        "managed-append",
        &[
            "--input",
            link.to_str().unwrap(),
            "--request",
            "link",
            "--allow-unredacted-records",
        ],
    ));
    let original = fs::read(f.root.join("data-key")).unwrap();
    private(&f.root.join("data-key"), &[7; 32]);
    refused(f.call("managed-status", &[]));
    private(&f.root.join("data-key"), &original);
    let proof = status(&f);
    let output = serde_json::to_string(&proof).unwrap();
    assert!(!output.contains(&serde_json::to_string(&original).unwrap()));
    fs::set_permissions(
        f.root.join("managed.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    refused(f.call("managed-open", &[]));
}

#[test]
fn missing_or_rolled_back_witness_refuses_without_bootstrap_and_output_bounds_hold() {
    let f = Fixture::new();
    ok(f.setup());
    let witness = fs::read(f.witness.join("CURRENT")).unwrap();
    f.input(json!([row(
        &"synthetic-payload".repeat(400),
        None,
        "reported",
        1
    )]));
    ok(f.append("a"));
    refused(f.call("managed-get", &["--id", "41", "--bytes", "4096"]));
    private(&f.witness.join("CURRENT"), &witness);
    // RetainedControl can recover a signed append beyond an ancestor witness by protocol.
    // A missing witness is different and must never recreate an empty authority.
    fs::remove_file(f.witness.join("CURRENT")).unwrap();
    refused(f.call("managed-status", &[]));
    assert!(!f.witness.join("CURRENT").exists());
    let gone = f.parent.join("retained-control-away");
    fs::rename(&f.control, &gone).unwrap();
    refused(f.call("managed-open", &[]));
    assert!(!f.control.exists());
    assert!(!f.witness.join("CURRENT").exists());
}

fn copy_private_tree(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let dest = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_private_tree(&item.path(), &dest)
        } else {
            assert!(item.file_type().unwrap().is_file());
            private(&dest, &fs::read(item.path()).unwrap());
        }
    }
}
#[test]
fn current_retained_authority_refuses_old_data_restore_without_reenrollment() {
    let f = Fixture::new();
    ok(f.setup());
    f.input(json!([row(
        "SYNTHETIC_RESTORE_SELECTED",
        None,
        "reported",
        1
    )]));
    ok(f.append("acknowledged-original"));
    let backup = f.parent.join("PAYLOAD-ONLY-BACKUP");
    copy_private_tree(&f.data, &backup);
    f.prune("prune-before-restore", json!({"Ids":[41]}));
    let current = fs::read(f.witness.join("CURRENT")).unwrap();
    fs::remove_dir_all(&f.data).unwrap();
    copy_private_tree(&backup, &f.data);
    refused(f.call("managed-open", &[]));
    refused(f.call("managed-get", &["--id", "41"]));
    refused(f.setup());
    assert_eq!(fs::read(f.witness.join("CURRENT")).unwrap(), current);
    assert_eq!(status(&f)["retained_snapshot_records"], 0);
    let (path, _) = f.export();
    ok(f.call("managed-verify", &["--input", path.to_str().unwrap()]));
}

#[test]
fn oversized_protected_config_is_refused_before_creating_any_authority() {
    let f = Fixture::new();
    let mut parent = f.parent.clone();
    for _ in 0..16 {
        // Backslashes are valid Linux filename bytes and expand when JSON-encoded.
        // The filesystem paths stay below PATH_MAX while the three config paths exceed16KiB.
        parent.push("d\\".repeat(120));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let root = parent.join("operator");
    let data = parent.join("payload");
    let control = parent.join("control");
    let witness = parent.join("witness");
    let output = Command::new(BIN)
        .arg("managed-setup")
        .args([
            "--root",
            root.to_str().unwrap(),
            "--data-root",
            data.to_str().unwrap(),
            "--control-root",
            control.to_str().unwrap(),
            "--witness-root",
            witness.to_str().unwrap(),
            "--writer",
            &"w".repeat(256),
            "--owner",
            "1",
            "--adapter",
            &"a".repeat(128),
        ])
        .output()
        .unwrap();
    refused(output);
    assert!(!root.exists());
    assert!(!data.exists());
    assert!(!control.exists());
    assert!(!witness.exists());
}
