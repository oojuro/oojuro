use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ojo-cli-{}",
            u128::from_le_bytes(ojo::request_id().unwrap())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ojo"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
const FIRST: &str = "01000000000000000000000000000000";
const SECOND: &str = "02000000000000000000000000000000";

#[test]
fn encrypted_binary_originals_retry_export_and_history() {
    let s = Scratch::new();
    s.ok(&["keygen", "key"]);
    s.ok(&["create", "db.ojo", "--key-file", "key"]);
    let original = b"\0\xffexact\r\n\"bytes\"\0";
    fs::write(s.0.join("value"), original).unwrap();
    let args = [
        "put",
        "db.ojo",
        "00ff",
        "value",
        "--request",
        FIRST,
        "--key-file",
        "key",
    ];
    let first: Value = serde_json::from_slice(&s.ok(&args).stdout).unwrap();
    let before = fs::read(s.0.join("db.ojo")).unwrap();
    let retry: Value = serde_json::from_slice(&s.ok(&args).stdout).unwrap();
    assert_eq!(first["checkpoint"], retry["checkpoint"]);
    assert_eq!(fs::read(s.0.join("db.ojo")).unwrap(), before);
    assert_eq!(
        s.ok(&["get", "db.ojo", "00ff", "--key-file", "key"]).stdout,
        original
    );
    assert!(!s.run(&["get", "db.ojo", "00ff"]).status.success());
    fs::write(s.0.join("value"), b"changed").unwrap();
    assert!(!s.run(&args).status.success());
    assert_eq!(fs::read(s.0.join("db.ojo")).unwrap(), before);
    s.ok(&["export", "db.ojo", "copy.ojo", "--key-file", "key"]);
    assert_eq!(fs::read(s.0.join("copy.ojo")).unwrap(), before);
    assert!(
        !s.run(&["export", "db.ojo", "copy.ojo", "--key-file", "key"])
            .status
            .success()
    );
    assert_eq!(fs::read(s.0.join("copy.ojo")).unwrap(), before);
    assert!(
        !fs::read_dir(&s.0).unwrap().any(|p| p
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
    s.ok(&[
        "delete",
        "db.ojo",
        "00ff",
        "--request",
        SECOND,
        "--key-file",
        "key",
    ]);
    assert!(
        !s.run(&["get", "db.ojo", "00ff", "--key-file", "key"])
            .status
            .success()
    );
    let dump = String::from_utf8(s.ok(&["dump", "db.ojo", "--key-file", "key"]).stdout).unwrap();
    let rows = dump
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["key_hex"], "00ff");
    assert_eq!(rows[0]["value_hex"], "00ff65786163740d0a2262797465732200");
    assert!(rows[1]["value_hex"].is_null());
    s.ok(&["verify", "db.ojo", "--key-file", "key"]);
}

#[test]
fn incomplete_tail_recovery_and_complete_corruption_are_distinct() {
    use std::io::Write;
    let s = Scratch::new();
    s.ok(&["create", "db.ojo"]);
    fs::write(s.0.join("value"), b"original").unwrap();
    s.ok(&["put", "db.ojo", "61", "value", "--request", FIRST]);
    let committed = fs::read(s.0.join("db.ojo")).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(s.0.join("db.ojo"))
        .unwrap()
        .write_all(b"torn")
        .unwrap();
    let torn = fs::read(s.0.join("db.ojo")).unwrap();
    assert!(!s.run(&["verify", "db.ojo"]).status.success());
    assert!(!s.run(&["export", "db.ojo", "bad.ojo"]).status.success());
    assert!(!s.0.join("bad.ojo").exists());
    assert!(
        !s.run(&["put", "db.ojo", "61", "value", "--request", "invalid"])
            .status
            .success()
    );
    assert!(
        !s.run(&["put", "db.ojo", "", "value", "--request", SECOND])
            .status
            .success()
    );
    assert_eq!(fs::read(s.0.join("db.ojo")).unwrap(), torn);
    let inspected: Value = serde_json::from_slice(&s.ok(&["inspect", "db.ojo"]).stdout).unwrap();
    assert_eq!(inspected["incomplete_tail_bytes"], 4);
    s.ok(&["recover", "db.ojo"]);
    assert_eq!(fs::read(s.0.join("db.ojo")).unwrap(), committed);
    let mut corrupt = committed;
    corrupt[64 + 96 + 16 + 1] ^= 1;
    fs::write(s.0.join("db.ojo"), &corrupt).unwrap();
    assert!(!s.run(&["recover", "db.ojo"]).status.success());
    assert_eq!(fs::read(s.0.join("db.ojo")).unwrap(), corrupt);
}

#[test]
fn key_and_output_files_are_private_and_existing_files_survive() {
    let s = Scratch::new();
    s.ok(&["keygen", "key"]);
    let before = fs::read(s.0.join("key")).unwrap();
    assert_eq!(before.len(), 32);
    assert!(!s.run(&["keygen", "key"]).status.success());
    assert_eq!(fs::read(s.0.join("key")).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(s.0.join("key")).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(s.0.join("key"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            !s.run(&["create", "db.ojo", "--key-file", "key"])
                .status
                .success()
        );
        assert!(!s.0.join("db.ojo").exists());
    }
}

#[cfg(feature = "archive")]
#[test]
fn archive_is_separate_and_round_trips_encrypted_originals() {
    let s = Scratch::new();
    s.ok(&["keygen", "key"]);
    s.ok(&["create", "db.ojo", "--key-file", "key"]);
    let value = vec![b'x'; 10000];
    fs::write(s.0.join("value"), &value).unwrap();
    s.ok(&[
        "put",
        "db.ojo",
        "61",
        "value",
        "--request",
        FIRST,
        "--key-file",
        "key",
    ]);
    let source = fs::read(s.0.join("db.ojo")).unwrap();
    s.ok(&["archive", "db.ojo", "archive.ojo", "--key-file", "key"]);
    assert_eq!(fs::read(s.0.join("db.ojo")).unwrap(), source);
    assert!(fs::metadata(s.0.join("archive.ojo")).unwrap().len() < (source.len() as u64));
    assert_eq!(
        s.ok(&["get", "archive.ojo", "61", "--key-file", "key"])
            .stdout,
        value
    );
    s.ok(&["verify", "archive.ojo", "--key-file", "key"]);
}
