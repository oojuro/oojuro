#![cfg(target_os = "linux")]
//! Actual binary subprocesses, private synthetic roots and no user logs/providers.
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
const BIN: &str = env!("CARGO_BIN_EXE_oojuro-native");
const GENERATION: &str = "34343434343434343434343434343434";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    parent: PathBuf,
    home: PathBuf,
    source: PathBuf,
    policy: PathBuf,
    harness: &'static str,
}
fn policy_snapshot() -> Value {
    json!({"version":1,"record":true,"paused":false,"record_thinking":true,"index_thinking":true,"exclude":[],"denied_tools":[]})
}
fn private_write(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
impl Fixture {
    fn new(harness: &'static str) -> Self {
        let parent = std::env::temp_dir().join(format!(
            "oojuro-native-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let home = parent.join("native");
        let source = parent.join(match harness {
            "codex" => ".codex/rollout-synthetic.jsonl",
            "pi" => ".pi/agent/sessions/--synthetic--/session.jsonl",
            _ => ".claude/projects/-synthetic/session.jsonl",
        });
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let policy = parent.join("explicit-policy.json");
        private_write(&policy, &serde_json::to_vec(&policy_snapshot()).unwrap());
        Self {
            parent,
            home,
            source,
            policy,
            harness,
        }
    }
    fn call(&self, command: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg(command)
            .args(["--root", self.home.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap()
    }
    fn setup(&self) -> Output {
        self.call(
            "setup",
            &[
                "--harness",
                self.harness,
                "--writer",
                match self.harness {
                    "codex" => "codex:-synthetic",
                    "pi" => "pi:--synthetic--",
                    _ => "claude-code:-synthetic",
                },
                "--agent",
                "7",
                "--policy",
                self.policy.to_str().unwrap(),
            ],
        )
    }
    fn capture(&self) -> Output {
        self.call(
            "capture",
            &[
                "--source",
                self.source.to_str().unwrap(),
                "--source-id",
                GENERATION,
                "--allow-raw-source",
            ],
        )
    }
    fn message(&self, text: &str) -> Vec<u8> {
        line(if self.harness == "pi" {
            json!({"type":"message","id":format!("entry-{}",text.len()),"parentId":null,"timestamp":"2026-01-02T03:04:06Z","message":{"role":"user","content":text}})
        } else if self.harness == "codex" {
            json!({"type":"response_item","timestamp":"2026-01-02T03:04:06Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}})
        } else {
            json!({"type":"user","timestamp":"2026-01-02T03:04:06Z","origin":{"kind":"human"},"message":{"content":text}})
        })
    }
    fn initial(&self, text: &str) -> Vec<u8> {
        let mut bytes = if self.harness == "pi" {
            line(
                json!({"type":"session","version":3,"id":"synthetic-pi","cwd":"/synthetic","timestamp":"2026-01-02T03:04:06Z"}),
            )
        } else if self.harness == "codex" {
            line(
                json!({"type":"session_meta","payload":{"cwd":"/synthetic","originator":"codex_cli_rs","base_instructions":{"text":""}}}),
            )
        } else {
            vec![]
        };
        bytes.extend(self.message(text));
        bytes.extend(b"\xff\n");
        bytes
    }
    fn append(&self, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(&self.source)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    fn set_policy(&self, value: Value) {
        private_write(
            &self.home.join("policy.json"),
            &serde_json::to_vec(&value).unwrap(),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn refused(output: Output, code: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "failed command leaked output");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("oojuro-native: {code}\n")
    );
}

#[test]
fn actual_codex_claude_pi_setup_capture_reopen_search_receipt_export_and_exact_raw_restore() {
    for harness in ["codex", "claude-code", "pi"] {
        let f = Fixture::new(harness);
        assert_eq!(ok(f.setup())["status"], "initialized");
        assert_eq!(
            fs::metadata(&f.home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(f.home.join("key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let original = f.initial("battery EVIDENCEMARKER decision");
        fs::write(&f.source, &original).unwrap();
        let report = ok(f.capture());
        assert_eq!(report["raw_end"], original.len());
        let ids = report["event_ids"].clone();
        assert_eq!(ok(f.capture())["event_ids"], ids);
        let search = ok(f.call("search", &["--query", "battery"]));
        let id = search["hits"][0]["id"].as_u64().unwrap().to_string();
        let record = ok(f.call("get", &["--id", &id]));
        assert!(
            record["output"]["event"]["text"]
                .as_str()
                .unwrap()
                .contains("EVIDENCEMARKER")
        );
        let first = ok(f.call("timeline", &["--entries", "1"]));
        let cursor = first["next"].as_str().unwrap();
        let watermark = first["watermark"].clone();
        let appended = f.message("later APPENDMARKER battery");
        f.append(&appended);
        ok(f.capture());
        let continued = ok(f.call("export", &["--entries", "100", "--cursor", cursor]));
        assert_eq!(continued["watermark"], watermark);
        let old = serde_json::to_string(&continued).unwrap();
        assert!(!old.contains("APPENDMARKER"));
        let latest = ok(f.call("export", &[]));
        assert!(
            serde_json::to_string(&latest)
                .unwrap()
                .contains("APPENDMARKER")
        );
        let cache = f.parent.join("dedicated-cache");
        let cache = cache.to_str().unwrap();
        let first = ok(f.call("search", &["--query", "battery", "--cache", cache]));
        assert_eq!(first["cache"], "Rebuilt");
        let second = ok(f.call("search", &["--query", "battery", "--cache", cache]));
        assert_eq!(second["cache"], "Loaded");
        assert_eq!(first["hits"], second["hits"]);
        ok(f.call("cache-merge", &["--cache", cache]));
        let extension = f.message("CACHEEXTENSION battery");
        f.append(&extension);
        ok(f.capture());
        assert_eq!(
            ok(f.call("search", &["--query", "battery", "--cache", cache]))["cache"],
            "Extended"
        );
        let destination = f.parent.join("raw-export.jsonl");
        ok(f.call(
            "restore-raw",
            &[
                "--source-id",
                GENERATION,
                "--out",
                destination.to_str().unwrap(),
                "--allow-raw-export",
            ],
        ));
        assert_eq!(
            fs::read(&destination).unwrap(),
            [original, appended, extension].concat()
        );
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let stats = ok(f.call("stats", &[]));
        assert!(stats["stored"].as_u64().unwrap() > 0);
        assert_eq!(stats["malformed"], 1);
        fn sealed(path: &std::path::Path) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    sealed(&entry.path());
                } else {
                    assert!(
                        !fs::read(entry.path())
                            .unwrap()
                            .windows(b"EVIDENCEMARKER".len())
                            .any(|bytes| bytes == b"EVIDENCEMARKER"),
                        "journal plaintext marker"
                    );
                }
            }
        }
        sealed(&f.home.join("journal"));
    }
}

#[test]
fn explicit_raw_capture_and_export_consent_precede_source_or_destination_activity() {
    let f = Fixture::new("claude-code");
    ok(f.setup());
    fs::write(&f.source, f.initial("not captured")).unwrap();
    refused(
        f.call(
            "capture",
            &[
                "--source",
                f.source.to_str().unwrap(),
                "--source-id",
                GENERATION,
            ],
        ),
        "explicit_raw_consent_required",
    );
    let absent = f.parent.join("PRIVATESOURCEPATH");
    refused(
        f.call(
            "capture",
            &[
                "--source",
                absent.to_str().unwrap(),
                "--source-id",
                GENERATION,
            ],
        ),
        "explicit_raw_consent_required",
    );
    assert_eq!(ok(f.call("stats", &[]))["receipts"], 0);
    let out = f.parent.join("not-created");
    refused(
        f.call(
            "restore-raw",
            &["--source-id", GENERATION, "--out", out.to_str().unwrap()],
        ),
        "explicit_raw_consent_required",
    );
    assert!(!out.exists());
}

#[test]
fn setup_does_not_overwrite_keys_and_invalid_key_permissions_or_scope_fail_closed() {
    let f = Fixture::new("claude-code");
    ok(f.setup());
    let key_path = f.home.join("key");
    let original = fs::read(&key_path).unwrap();
    refused(f.setup(), "local_io_failed");
    assert!(fs::read(&key_path).unwrap() == original);
    fs::write(&f.source, f.initial("binding evidence")).unwrap();
    ok(f.capture());
    private_write(&key_path, &[11; 32]);
    refused(
        f.call("search", &["--query", "binding"]),
        "authority_unavailable",
    );
    private_write(&key_path, &original);
    assert!(
        !ok(f.call("search", &["--query", "binding"]))["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    refused(f.call("stats", &[]), "private_owner_permissions_required");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
    let backup = f.parent.join("synthetic-key");
    private_write(&backup, &original);
    fs::remove_file(&key_path).unwrap();
    symlink(&backup, &key_path).unwrap();
    refused(f.call("stats", &[]), "local_io_failed");
    fs::remove_file(&key_path).unwrap();
    private_write(&key_path, &original);
    let config_path = f.home.join("native.json");
    let bytes = fs::read(&config_path).unwrap();
    let mut config: Value = serde_json::from_slice(&bytes).unwrap();
    config["writer"] = json!("foreign-writer");
    private_write(&config_path, &serde_json::to_vec(&config).unwrap());
    refused(f.call("stats", &[]), "authority_unavailable");
    private_write(&config_path, &bytes);
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o755)).unwrap();
    refused(f.call("stats", &[]), "private_owner_permissions_required");
}

#[test]
fn frozen_policy_is_fail_closed_and_partial_lines_keep_their_original_policy_identity() {
    let f = Fixture::new("claude-code");
    ok(f.setup());
    let complete = f.message("partial evidence");
    fs::write(&f.source, &complete[..complete.len() - 1]).unwrap();
    let before = ok(f.capture());
    assert_eq!(before["parsed_end"], 0);
    let mut changed = policy_snapshot();
    changed["record_thinking"] = json!(false);
    f.set_policy(changed);
    f.append(b"\n");
    let error = f.capture();
    assert!(!error.status.success());
    assert!(error.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&error.stderr)
            .contains("incomplete line requires its original policy identity")
    );
    f.set_policy(policy_snapshot());
    assert_eq!(ok(f.capture())["event_ids"], json!([1]));
    private_write(&f.home.join("policy.json"), b"{private malformed snapshot}");
    refused(f.capture(), "invalid_policy");
    for key in ["record", "paused"] {
        let mut stopped = policy_snapshot();
        stopped[key] = json!(key == "paused");
        f.set_policy(stopped);
        fs::remove_file(&f.source).unwrap();
        refused(f.capture(), "recording_stopped");
        fs::write(&f.source, &complete).unwrap();
    }
}

#[test]
fn denied_thought_and_tool_results_stay_withheld_after_policy_relaxation_and_restart() {
    let f = Fixture::new("claude-code");
    let mut strict = policy_snapshot();
    strict["record_thinking"] = json!(false);
    strict["denied_tools"] = json!(["blocked_tool"]);
    private_write(&f.policy, &serde_json::to_vec(&strict).unwrap());
    ok(f.setup());
    let initial = line(
        json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"RAWTHOUGHTMARKER"},{"type":"tool_use","id":"denied","name":"blocked_tool","input":{}}]}}),
    );
    fs::write(&f.source, &initial).unwrap();
    assert!(ok(f.capture())["event_ids"].as_array().unwrap().is_empty());
    f.set_policy(policy_snapshot());
    let result = line(
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"denied","content":"RAWRESULTMARKER"}]}}),
    );
    f.append(&result);
    assert!(ok(f.capture())["event_ids"].as_array().unwrap().is_empty());
    assert!(
        ok(f.call("search", &["--query", "RAWTHOUGHTMARKER RAWRESULTMARKER"]))["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let exported = ok(f.call("export", &[]));
    let text = serde_json::to_string(&exported).unwrap();
    assert!(!text.contains("RAWTHOUGHTMARKER"));
    assert!(!text.contains("RAWRESULTMARKER"));
    assert!(text.contains("withheld"));
}

#[test]
fn receipt_output_bounds_and_tampered_cursors_return_no_partial_payload() {
    let f = Fixture::new("claude-code");
    ok(f.setup());
    fs::write(
        &f.source,
        [f.message(&"x".repeat(4096)), f.message("second")].concat(),
    )
    .unwrap();
    ok(f.capture());
    refused(f.call("export", &["--bytes", "512"]), "receipt_refused");
    let result = f.call("export", &["--bytes", "100000", "--entries", "1"]);
    assert!(result.stdout.len() <= 100000);
    let page = ok(result);
    let mut cursor = page["next"].as_str().unwrap().as_bytes().to_vec();
    cursor[0] = if cursor[0] == b'0' { b'1' } else { b'0' };
    let cursor = String::from_utf8(cursor).unwrap();
    refused(
        f.call("timeline", &["--cursor", &cursor]),
        "receipt_refused",
    );
    refused(
        f.call("get", &["--id", "1", "--bytes", "512"]),
        "receipt_refused",
    );
}

#[test]
fn malformed_claude_call_has_no_ack_and_never_leaks_source_path_or_private_body() {
    let f = Fixture::new("claude-code");
    ok(f.setup());
    let bytes = line(
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"blocked_tool"},{"type":"bash_tool_result","tool_use_id":"t","content":"PRIVATERESULTBODY"}]}}),
    );
    fs::write(&f.source, &bytes).unwrap();
    refused(f.capture(), "capture_refused: tool call input missing");
    assert_eq!(ok(f.call("stats", &[]))["receipts"], 0);
    assert_eq!(fs::read(&f.source).unwrap(), bytes);
}
