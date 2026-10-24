#![cfg(target_os = "linux")]
//! Real opt-in binary processes against synthetic private typed Record inputs.
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
    input: PathBuf,
}
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let parent = std::env::temp_dir().join(format!(
            "oojuro-record-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root: parent.join("native"),
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
    fn setup(&self) -> Value {
        ok(self.call(
            "record-setup",
            &[
                "--writer",
                "synthetic-writer",
                "--owner",
                "7",
                "--adapter",
                "reported-sdk",
                "--first-id",
                "41",
            ],
        ))
    }
    fn write(&self, rows: Value) {
        private(&self.input, &serde_json::to_vec(&rows).unwrap());
    }
    fn append(&self, request: &str) -> Output {
        self.call(
            "record-append",
            &[
                "--input",
                self.input.to_str().unwrap(),
                "--request",
                request,
                "--allow-unredacted-records",
            ],
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
fn row(text: &str, key: Option<&str>, valid: i64) -> Value {
    json!({"agent_id":"reported-instance","parent_id":null,"valid_at":valid,"kind":"explicit","tool":null,"status":null,"exit":null,"key":key,"text":text,"attrs":{},"indexed":true})
}
fn ok(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}
fn refused(o: Output, code: &str) {
    assert!(!o.status.success());
    assert!(o.stdout.is_empty(), "refusal leaked stdout");
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!("oojuro-native: {code}\n")
    );
}
fn ids(v: &Value) -> Vec<u64> {
    v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect()
}

fn append_jsonl(f: &Fixture, request: &str) -> Output {
    f.call(
        "record-append",
        &[
            "--input",
            f.input.to_str().unwrap(),
            "--input-format",
            "jsonl",
            "--request",
            request,
            "--allow-unredacted-records",
        ],
    )
}

#[test]
fn jsonl_exact_originals_reopen_and_cross_format_retry() {
    let f = Fixture::new();
    f.setup();
    let mut first = row("battery λ\r\nsecond line\u{0}終", Some("memory"), 10);
    first["attrs"] = json!({"source_id": "synthetic-1", "nested": {"n": 7}});
    let second = row("battery duplicate", None, 20);
    let third = row("battery duplicate", None, 20);
    let rows = [first, second, third];
    let mut input = Vec::new();
    for (i, value) in rows.iter().enumerate() {
        input.extend(serde_json::to_vec(value).unwrap());
        if i < 2 {
            input.extend_from_slice(b"\r\n");
        }
    }
    private(&f.input, &input);
    let ack = ok(append_jsonl(&f, "jsonl-original-operation"));
    assert_eq!(ack["ids"], json!([41, 42, 43]));
    assert_eq!(fs::read(&f.input).unwrap(), input);
    assert_eq!(ok(append_jsonl(&f, "jsonl-original-operation")), ack);
    // Representation and LF/CRLF/final-newline choices are not new typed intent.
    private(&f.input, &[input.as_slice(), b"\n"].concat());
    assert_eq!(ok(append_jsonl(&f, "jsonl-original-operation")), ack);
    f.write(json!(rows));
    assert_eq!(ok(f.append("jsonl-original-operation")), ack);
    for (i, original) in rows.iter().enumerate() {
        let stored = ok(f.call("record-get", &["--id", &(41 + i).to_string()]));
        for (key, value) in original.as_object().unwrap() {
            assert_eq!(&stored[key], value, "field {key}");
        }
        assert_eq!(stored["recorded_at"], ack["recorded_at"][i]);
    }
    assert_eq!(
        ids(&ok(f.call("record-export", &["--versions", "all"]))),
        vec![41, 42, 43]
    );
    assert_eq!(
        ok(f.call("record-search", &["--query", "battery"]))["hits"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let changed = row("changed typed intent", None, 20);
    private(&f.input, &serde_json::to_vec(&changed).unwrap());
    refused(
        append_jsonl(&f, "jsonl-original-operation"),
        "record_refused: conflict",
    );
}

#[test]
fn jsonl_invalid_later_rows_bounds_and_permissions_never_append_a_prefix() {
    let f = Fixture::new();
    f.setup();
    let original = row("PRIVATE-JSONL-ORIGINAL", None, 10);
    let line = serde_json::to_vec(&original).unwrap();
    let mut unknown = original.clone();
    unknown["unexpected"] = json!(true);
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("indexed");
    let bad_rows = [
        b"not-json".to_vec(),
        b"\n".to_vec(),
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&missing).unwrap(),
        b"\xff".to_vec(),
        serde_json::to_vec_pretty(&original).unwrap(),
    ];
    for bad in bad_rows {
        private(&f.input, &[line.as_slice(), b"\n", &bad].concat());
        refused(append_jsonl(&f, "bad-later-row"), "invalid_config");
    }
    private(&f.input, b"");
    refused(append_jsonl(&f, "empty"), "record_refused: bounds");
    let mut too_many = Vec::new();
    for _ in 0..1001 {
        too_many.extend_from_slice(&line);
        too_many.push(b'\n');
    }
    private(&f.input, &too_many);
    refused(append_jsonl(&f, "too-many"), "output_or_input_bound");
    private(&f.input, &line);
    fs::set_permissions(&f.input, fs::Permissions::from_mode(0o644)).unwrap();
    refused(
        append_jsonl(&f, "public-input"),
        "private_owner_permissions_required",
    );
    // Consent still precedes looking at even a nonexistent JSONL source.
    let absent = f.parent.join("PRIVATE-MISSING.jsonl");
    refused(
        f.call(
            "record-append",
            &[
                "--input",
                absent.to_str().unwrap(),
                "--input-format",
                "jsonl",
                "--request",
                "consent",
            ],
        ),
        "explicit_unredacted_record_consent_required",
    );
    refused(
        f.call(
            "record-append",
            &[
                "--input",
                absent.to_str().unwrap(),
                "--input-format",
                "csv",
                "--request",
                "format",
                "--allow-unredacted-records",
            ],
        ),
        "invalid_arguments",
    );
    assert!(ids(&ok(f.call("record-range", &["--versions", "all"]))).is_empty());
    private(&f.input, &too_many[..1000 * (line.len() + 1)]);
    let maximum = ok(append_jsonl(&f, "exactly-1000"));
    assert_eq!(maximum["ids"].as_array().unwrap().len(), 1000);
    assert_eq!(maximum["ids"][999], 1040);
    private(&f.input, &line);
    assert_eq!(
        ok(append_jsonl(&f, "valid-after-refusals"))["ids"],
        json!([1041])
    );
}

#[test]
fn actual_record_setup_append_reopen_exact_retry_bitemporal_versions_and_provenance() {
    let f = Fixture::new();
    assert_eq!(f.setup()["namespace"], "oojuro-record-2");
    assert_eq!(
        fs::metadata(&f.root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in ["key", "record.json"] {
        assert_eq!(
            fs::metadata(f.root.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut first = row("battery old", Some("memory"), 10);
    first["attrs"] = json!({"adapter":"untrusted-override"});
    f.write(json!([
        first,
        row("battery newest", Some("memory"), 30),
        row("battery late backdate", Some("memory"), 20),
        row("battery duplicate", None, 25),
        row("battery duplicate", None, 25)
    ]));
    let ack = ok(f.append("original-exact-batch"));
    assert_eq!(ack["ids"], json!([41, 42, 43, 44, 45]));
    assert_eq!(ok(f.append("original-exact-batch")), ack);
    let times = ack["recorded_at"].as_array().unwrap();
    for pair in times.windows(2) {
        assert!(pair[0].as_i64().unwrap() < pair[1].as_i64().unwrap());
    }
    assert_eq!(ok(f.call("record-get", &["--key", "memory"]))["id"], 42);
    let first = ok(f.call("record-get", &["--id", "41"]));
    assert_eq!(first["adapter"], "reported-sdk");
    assert_eq!(first["attrs"]["adapter"], "untrusted-override");
    assert_eq!(first["agent_id"], "reported-instance");
    assert_eq!(first["writer"], "synthetic-writer");
    assert_eq!(
        ok(f.call("record-get", &["--key", "memory", "--valid-at-us", "25"]))["id"],
        43
    );
    let known = times[0].as_i64().unwrap().to_string();
    assert_eq!(
        ok(f.call("record-get", &["--key", "memory", "--known-at-us", &known]))["id"],
        41
    );
    assert!(ok(f.call("record-get", &["--key", "memory", "--from-us", "31"])).is_null());
    assert_eq!(
        ids(&ok(f.call("record-history", &["--key", "memory"]))),
        vec![41, 43, 42]
    );
    assert_eq!(
        ids(&ok(f.call("record-range", &["--versions", "current"]))),
        vec![44, 45, 42]
    );
    assert_eq!(
        ids(&ok(f.call("record-export", &["--versions", "all"]))),
        vec![41, 43, 44, 45, 42]
    );
    let hits = ok(f.call("record-search", &["--query", "battery"]));
    let found = hits["hits"].as_array().unwrap();
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .all(|h| h["record"]["adapter"] == "reported-sdk")
    );
    assert_eq!(
        found
            .iter()
            .filter(|h| h["record"]["text"] == "battery duplicate")
            .count(),
        2
    );
    let old = ok(f.call(
        "record-search",
        &["--query", "battery", "--known-at-us", &known],
    ));
    assert_eq!(old["hits"].as_array().unwrap().len(), 1);
}

#[test]
fn record_consent_private_input_and_exact_intent_refusals_preserve_authority() {
    let f = Fixture::new();
    f.setup();
    let rows = json!([row("PRIVATE-BODY", Some("memory"), 10)]);
    f.write(rows.clone());
    refused(
        f.call(
            "record-append",
            &["--input", f.input.to_str().unwrap(), "--request", "consent"],
        ),
        "explicit_unredacted_record_consent_required",
    );
    assert!(ids(&ok(f.call("record-range", &["--versions", "all"]))).is_empty());
    let absent = f.parent.join("PRIVATE-MISSING-PATH");
    refused(
        f.call(
            "record-append",
            &["--input", absent.to_str().unwrap(), "--request", "consent"],
        ),
        "explicit_unredacted_record_consent_required",
    );
    let ack = ok(f.append("same-intent"));
    let mut changed = rows.clone();
    changed[0]["valid_at"] = json!(11);
    f.write(changed);
    refused(f.append("same-intent"), "record_refused: conflict");
    for field in ["key", "indexed"] {
        let mut incomplete = rows.clone();
        incomplete[0].as_object_mut().unwrap().remove(field);
        f.write(incomplete);
        refused(f.append("malformed"), "invalid_config");
    }
    let mut extra = rows.clone();
    extra[0]["unexpected"] = json!(true);
    f.write(extra);
    refused(f.append("extra"), "invalid_config");
    f.write(rows);
    assert_eq!(ok(f.append("same-intent")), ack);
    fs::set_permissions(&f.input, fs::Permissions::from_mode(0o644)).unwrap();
    refused(
        f.append("permissions"),
        "private_owner_permissions_required",
    );
    fs::set_permissions(&f.input, fs::Permissions::from_mode(0o600)).unwrap();
    let link = f.parent.join("link.json");
    symlink(&f.input, &link).unwrap();
    refused(
        f.call(
            "record-append",
            &[
                "--input",
                link.to_str().unwrap(),
                "--request",
                "symlink",
                "--allow-unredacted-records",
            ],
        ),
        "local_io_failed",
    );
    assert_eq!(
        ids(&ok(f.call("record-range", &["--versions", "all"]))),
        vec![41]
    );
}

#[test]
fn record_scoped_protected_roots_never_convert_capture_or_replace_keys() {
    let f = Fixture::new();
    f.setup();
    let key = fs::read(f.root.join("key")).unwrap();
    refused(
        f.call(
            "record-setup",
            &["--writer", "new", "--owner", "1", "--adapter", "new"],
        ),
        "local_io_failed",
    );
    assert!(fs::read(f.root.join("key")).unwrap() == key);
    let policy = f.parent.join("policy.json");
    private(&policy,br#"{"version":1,"record":true,"paused":false,"record_thinking":true,"index_thinking":true,"exclude":[],"denied_tools":[]}"#);
    refused(
        f.call(
            "setup",
            &[
                "--writer",
                "capture",
                "--agent",
                "1",
                "--harness",
                "codex",
                "--policy",
                policy.to_str().unwrap(),
            ],
        ),
        "local_io_failed",
    );
    assert!(!f.root.join("native.json").exists());
    refused(f.call("get", &["--id", "41"]), "local_io_failed");
    let cfgpath = f.root.join("record.json");
    let saved = fs::read(&cfgpath).unwrap();
    let mut cfg: Value = serde_json::from_slice(&saved).unwrap();
    cfg["adapter"] = json!("forged-source");
    private(&cfgpath, &serde_json::to_vec(&cfg).unwrap());
    refused(
        f.call("record-get", &["--id", "41"]),
        "record_refused: authority",
    );
    private(&cfgpath, &saved);
    private(&f.root.join("key"), &[19; 32]);
    refused(
        f.call("record-range", &["--versions", "all"]),
        "record_refused: unavailable",
    );
    private(&f.root.join("key"), &key);
    fs::set_permissions(&cfgpath, fs::Permissions::from_mode(0o644)).unwrap();
    refused(
        f.call("record-get", &["--id", "41"]),
        "private_owner_permissions_required",
    );
    fs::set_permissions(&cfgpath, fs::Permissions::from_mode(0o600)).unwrap();
    let capture = Fixture::new();
    ok(capture.call(
        "setup",
        &[
            "--writer",
            "capture",
            "--agent",
            "1",
            "--harness",
            "codex",
            "--policy",
            policy.to_str().unwrap(),
        ],
    ));
    let before = fs::read(capture.root.join("key")).unwrap();
    refused(
        capture.call(
            "record-setup",
            &["--writer", "new", "--owner", "1", "--adapter", "new"],
        ),
        "local_io_failed",
    );
    assert!(fs::read(capture.root.join("key")).unwrap() == before);
    assert!(!capture.root.join("record.json").exists());
    refused(
        capture.call("record-range", &["--versions", "all"]),
        "local_io_failed",
    );
}

#[test]
fn record_cursor_carries_original_query_and_prefix_across_append_and_reopen() {
    let f = Fixture::new();
    f.setup();
    f.write(json!([
        row("battery first", None, 10),
        row("battery second", None, 20),
        row("battery third", None, 30)
    ]));
    ok(f.append("initial"));
    let page = ok(f.call(
        "record-range",
        &[
            "--versions",
            "all",
            "--from-us",
            "15",
            "--kind",
            "explicit",
            "--entries",
            "1",
        ],
    ));
    assert_eq!(ids(&page), vec![42]);
    let cursor = page["next"].as_str().unwrap();
    let mark = page["watermark"].clone();
    f.write(json!([row("APPENDED-AFTER-CURSOR", None, 25)]));
    ok(f.append("later"));
    let continued = ok(f.call("record-continue", &["--cursor", cursor]));
    assert_eq!(ids(&continued), vec![43]);
    assert_eq!(continued["watermark"], mark);
    assert!(continued["next"].is_null());
    refused(
        f.call("record-continue", &["--cursor", cursor, "--from-us", "0"]),
        "invalid_arguments",
    );
    refused(
        f.call("record-continue", &["--cursor", cursor, "--key", "another"]),
        "invalid_arguments",
    );
    let mut broken = cursor.as_bytes().to_vec();
    broken[10] = if broken[10] == b'0' { b'1' } else { b'0' };
    refused(
        f.call(
            "record-continue",
            &["--cursor", std::str::from_utf8(&broken).unwrap()],
        ),
        "record_refused: authority",
    );
    let other = Fixture::new();
    other.setup();
    refused(
        other.call("record-continue", &["--cursor", cursor]),
        "record_refused: authority",
    );
    assert!(ids(&ok(f.call("record-export", &["--versions", "all"]))).contains(&44));
}

#[test]
fn record_visibility_filters_and_nonindexed_literals_match_real_engine_semantics() {
    let f = Fixture::new();
    f.setup();
    let old = row("old content !!! lookup_needle", Some("key"), 10);
    let mut hidden = row("hidden content !!! lookup_needle", Some("key"), 20);
    hidden["indexed"] = json!(false);
    hidden["kind"] = json!("tool_result");
    hidden["tool"] = json!("Read");
    hidden["status"] = json!("error");
    hidden["agent_id"] = json!("other-instance");
    let mut visible = row("lookup needle battery", None, 30);
    visible["attrs"] = json!({"not_indexed":"ATTR-ONLY-SUFFIX"});
    f.write(json!([old, hidden, visible]));
    ok(f.append("visibility"));
    assert!(
        ok(f.call("record-search", &["--query", "\"!!!\""]))["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        ok(f.call("record-search", &["--query", "ATTR-ONLY-SUFFIX"]))["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(ok(f.call("record-get", &["--key", "key"]))["id"], 42);
    assert_eq!(
        ids(&ok(f.call(
            "record-range",
            &["--versions", "current", "--kind", "explicit"]
        ))),
        vec![43]
    );
    let old = ok(f.call("record-search", &["--query", "old", "--valid-at-us", "15"]));
    assert_eq!(old["hits"][0]["record"]["id"], 41);
    let typed = ok(f.call(
        "record-range",
        &[
            "--versions",
            "current",
            "--kind",
            "tool_result",
            "--tool",
            "Read",
            "--status",
            "error",
            "--reported-agent",
            "other-instance",
        ],
    ));
    assert_eq!(ids(&typed), vec![42]);
    assert_eq!(
        ids(&ok(f.call(
            "record-range",
            &["--versions", "all", "--reported-agent", "reported-instance"]
        ))),
        vec![41, 43]
    );
    let found = ok(f.call("record-search", &["--query", "lookup_needle battery"]));
    assert_eq!(found["hits"].as_array().unwrap().len(), 1);
    assert_eq!(found["hits"][0]["record"]["id"], 43);
    assert_eq!(found["hits"][0]["continued"], false);
}

#[test]
fn record_bounded_errors_and_sealed_contents_never_leak_private_inputs_or_paths() {
    let f = Fixture::new();
    f.setup();
    let original = json!([row(
        &"PRIVATE-PAYLOAD".repeat(300),
        Some("PRIVATE-LOGICAL-KEY"),
        10
    )]);
    f.write(original.clone());
    let unchanged = fs::read(&f.input).unwrap();
    ok(f.append("PRIVATE-REQUEST"));
    assert_eq!(fs::read(&f.input).unwrap(), unchanged);
    refused(
        f.call("record-get", &["--id", "41", "--bytes", "256"]),
        "record_refused: bounds",
    );
    refused(
        f.call("record-export", &["--versions", "all", "--bytes", "256"]),
        "record_refused: bounds",
    );
    refused(
        f.call("record-search", &["--query", "PAYLOAD", "--bytes", "256"]),
        "record_refused: bounds",
    );
    refused(
        f.call("record-get", &["--id", "999", "--bytes", "4"]),
        "record_refused: bounds",
    );
    let null = f.call("record-get", &["--id", "999", "--bytes", "5"]);
    assert_eq!(null.stdout, b"null\n");
    ok(null);
    let out = f.call("record-export", &["--versions", "all", "--bytes", "16000"]);
    assert!(out.stdout.len() <= 16000);
    ok(out);
    private(&f.input, b"{PRIVATE-MALFORMED-BODY");
    refused(f.append("bad"), "invalid_config");
    fn sealed(dir: &Path) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                sealed(&entry.path());
            } else {
                let bytes = fs::read(entry.path()).unwrap();
                for marker in [
                    b"PRIVATE-PAYLOAD".as_slice(),
                    b"PRIVATE-LOGICAL-KEY",
                    b"PRIVATE-REQUEST",
                ] {
                    assert!(!bytes.windows(marker.len()).any(|b| b == marker));
                }
            }
        }
    }
    sealed(&f.root.join("journal"));
    fs::rename(f.root.join("journal"), f.root.join("held-journal")).unwrap();
    refused(
        f.call("record-range", &["--versions", "all"]),
        "authority_unavailable",
    );
    assert!(!f.root.join("journal").exists());
}
