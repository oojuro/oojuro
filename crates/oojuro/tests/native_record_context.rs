#![cfg(target_os = "linux")]
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};
const BIN: &str = env!("CARGO_BIN_EXE_oojuro-native");
struct Fixture {
    root: PathBuf,
    parent: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let parent =
            std::env::temp_dir().join(format!("oojuro-context-cli-{}", std::process::id()));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let f = Self {
            root: parent.join("record"),
            parent,
        };
        assert!(
            f.call(
                "record-setup",
                &[
                    "--writer",
                    "synthetic-context",
                    "--owner",
                    "1",
                    "--adapter",
                    "synthetic"
                ]
            )
            .status
            .success()
        );
        let mut rows = vec![
            json!({"agent_id":"reported","parent_id":null,"valid_at":1,"kind":"explicit","tool":null,"status":null,"exit":null,"key":"memory","text":"uniqueneedle synthetic original record","attrs":{},"indexed":true}),
        ];
        for i in 2..20 {
            rows.push(json!({"agent_id":"reported","parent_id":null,"valid_at":i,"kind":"explicit","tool":null,"status":null,"exit":null,"key":null,"text":"unrelated other filler text","attrs":{},"indexed":true}));
        }
        let input = f.parent.join("input.json");
        fs::write(&input, serde_json::to_vec(&rows).unwrap()).unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            f.call(
                "record-append",
                &[
                    "--request",
                    "fixture",
                    "--input",
                    input.to_str().unwrap(),
                    "--allow-unredacted-records"
                ]
            )
            .status
            .success()
        );
        f
    }
    fn call(&self, command: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg(command)
            .arg("--root")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
#[test]
fn explicit_native_context_measures_whole_original_and_honestly_labels_estimate() {
    let f = Fixture::new();
    let missing = f.call(
        "record-context",
        &[
            "--query",
            "uniqueneedle",
            "--turn-start-us",
            "9223372036854775807",
        ],
    );
    assert!(!missing.status.success());
    let result = f.call(
        "record-context",
        &[
            "--query",
            "uniqueneedle",
            "--measure",
            "chars4-estimate",
            "--turn-start-us",
            "9223372036854775807",
            "--min-bits",
            "0",
            "--bytes",
            "65536",
            "--context-bytes",
            "8192",
        ],
    );
    assert!(
        result.status.success(),
        "fixed CLI error: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["hard_provider_token_cap"], false);
    assert_eq!(value["items"], 1);
    let context: Value = serde_json::from_str(value["context"].as_str().unwrap()).unwrap();
    assert_eq!(
        context["items"][0]["record"]["text"],
        "uniqueneedle synthetic original record"
    );
    assert!(
        context["trust"]
            .as_str()
            .unwrap()
            .contains("not instructions")
    );
    assert!(value["estimated_tokens"].as_u64().unwrap() <= 2500);
    let hook = f.call(
        "record-context",
        &[
            "--query",
            "uniqueneedle",
            "--measure",
            "chars4-estimate",
            "--turn-start-us",
            "9223372036854775807",
            "--min-bits",
            "0",
            "--format",
            "claude-hook",
        ],
    );
    assert!(hook.status.success());
    let value: Value = serde_json::from_slice(&hook.stdout).unwrap();
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert_eq!(value["hard_provider_token_cap"], false);
    let small = f.call(
        "record-context",
        &[
            "--query",
            "uniqueneedle",
            "--measure",
            "chars4-estimate",
            "--turn-start-us",
            "9223372036854775807",
            "--min-bits",
            "0",
            "--context-bytes",
            "32",
        ],
    );
    assert!(small.status.success());
    let value: Value = serde_json::from_slice(&small.stdout).unwrap();
    assert_eq!(value["items"], 0);
    assert!(value["context"].is_null());
    let original = f.call("record-get", &["--id", "1"]);
    assert!(original.status.success());
    let original: Value = serde_json::from_slice(&original.stdout).unwrap();
    let turn_start = original["recorded_at"].as_i64().unwrap() + 1;
    let future = f.parent.join("future.json");
    fs::write(
        &future,
        serde_json::to_vec(
            &json!([{"agent_id":"reported","parent_id":null,"valid_at":1,
        "kind":"explicit","tool":null,"status":null,"exit":null,"key":"memory",
        "text":"later correction replaces prior key","attrs":{},"indexed":true}]),
        )
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&future, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        f.call(
            "record-append",
            &[
                "--request",
                "later",
                "--input",
                future.to_str().unwrap(),
                "--allow-unredacted-records"
            ]
        )
        .status
        .success()
    );
    let prior = f.call(
        "record-context",
        &[
            "--query",
            "uniqueneedle",
            "--measure",
            "chars4-estimate",
            "--turn-start-us",
            &turn_start.to_string(),
            "--min-bits",
            "0",
        ],
    );
    assert!(prior.status.success());
    let prior: Value = serde_json::from_slice(&prior.stdout).unwrap();
    assert_eq!(
        prior["items"], 1,
        "pre-turn query must select the original key version before later correction"
    );
}
