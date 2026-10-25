use ojo::events::{AppendIntent, Journal};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ojo-events-cli-{:032x}",
            u128::from_le_bytes(ojo::request_id().unwrap())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ojo"));
        command.current_dir(&self.0).args(args);
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn engine_assigns_time_and_binary_cursor_without_application_keys() {
    let s = Scratch::new();
    s.ok(&["keygen", "key"]);
    s.ok(&["init", "events.juro", "--key-file", "key"]);
    let original = b"\xff\0sensor sample\r\n";
    let mut child = s
        .command(&["append", "events.juro", "-", "--key-file", "key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(original).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cursor = receipt["cursor"].as_str().unwrap();
    assert_eq!(cursor.len(), 80);
    assert!(receipt["accepted_at"].as_i64().unwrap() > 0);
    assert_eq!(
        s.ok(&["read", "events.juro", cursor, "--key-file", "key"])
            .stdout,
        original
    );
    s.ok(&[
        "read",
        "events.juro",
        cursor,
        "--key-file",
        "key",
        "--output",
        "restored",
    ]);
    assert_eq!(fs::read(s.0.join("restored")).unwrap(), original);
    assert!(
        !s.run(&[
            "read",
            "events.juro",
            cursor,
            "--key-file",
            "key",
            "--output",
            "restored"
        ])
        .status
        .success()
    );
    let page: Value =
        serde_json::from_slice(&s.ok(&["scan", "events.juro", "--key-file", "key"]).stdout)
            .unwrap();
    assert_eq!(page["events"][0]["cursor"], cursor);
    assert_eq!(page["events"][0]["accepted_at"], receipt["accepted_at"]);
    assert_eq!(page["page"]["cursor"], cursor);
    assert_eq!(page["page"]["has_more"], false);
    assert!(!s.run(&["read", "events.juro", cursor]).status.success());
    // An explicitly selected legacy filename stays at that path. Detection is
    // based on its unchanged header/profile, not the canonical new suffix.
    s.ok(&["export", "events.juro", "legacy.ojo", "--key-file", "key"]);
    let legacy = fs::read(s.0.join("legacy.ojo")).unwrap();
    assert_eq!(fs::read(s.0.join("events.juro")).unwrap(), legacy);
    assert_eq!(&legacy[..8], b"OJOFILE\0");
    assert_eq!(
        s.ok(&["read", "legacy.ojo", cursor, "--key-file", "key"])
            .stdout,
        original
    );
    assert!(!s.0.join("legacy.juro").exists());
    #[cfg(feature = "archive")]
    {
        s.ok(&[
            "archive",
            "events.juro",
            "archive.juro",
            "--key-file",
            "key",
        ]);
        assert_eq!(
            s.ok(&["read", "archive.juro", cursor, "--key-file", "key"])
                .stdout,
            original
        );
        s.ok(&[
            "archive",
            "legacy.ojo",
            "legacy-archive.juro",
            "--key-file",
            "key",
        ]);
        assert_eq!(fs::read(s.0.join("legacy.ojo")).unwrap(), legacy);
        assert_eq!(
            s.ok(&["read", "legacy-archive.juro", cursor, "--key-file", "key"])
                .stdout,
            original
        );
    }
}

#[test]
fn lost_stdout_ack_retries_retained_intent_and_bad_input_never_recovers_tail() {
    let s = Scratch::new();
    s.ok(&["init", "events.juro"]);
    let data = b"retained exact original\0\xff";
    fs::write(s.0.join("value"), data).unwrap();
    let args = ["append", "events.juro", "value", "--intent", "retry"];
    let mut child = s
        .command(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take()); // break the ACK channel after submission
    assert!(!child.wait().unwrap().success());
    let intent =
        AppendIntent::from_bytes(fs::read(s.0.join("retry")).unwrap().try_into().unwrap()).unwrap();
    let mut reader = Journal::open(s.0.join("events.juro"), None, false).unwrap();
    assert_eq!(reader.len(), 1);
    reader.check_intent(&intent, data).unwrap();
    drop(reader);
    let before = fs::read(s.0.join("events.juro")).unwrap();
    let first: Value = serde_json::from_slice(&s.ok(&args).stdout).unwrap();
    let second: Value = serde_json::from_slice(&s.ok(&args).stdout).unwrap();
    assert_eq!(first["cursor"], second["cursor"]);
    assert_eq!(first["accepted_at"], second["accepted_at"]);
    assert_eq!(fs::read(s.0.join("events.juro")).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(s.0.join("retry"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    fs::OpenOptions::new()
        .append(true)
        .open(s.0.join("events.juro"))
        .unwrap()
        .write_all(b"torn")
        .unwrap();
    let torn = fs::read(s.0.join("events.juro")).unwrap();
    fs::write(s.0.join("value"), b"different").unwrap();
    assert!(!s.run(&args).status.success());
    assert_eq!(fs::read(s.0.join("events.juro")).unwrap(), torn);
    let kv = s.0.join("kv.juro");
    drop(ojo::Database::create(&kv, None).unwrap());
    assert!(!s.run(&["append", "kv.juro", "value"]).status.success());
    assert_eq!(fs::metadata(kv).unwrap().len(), 64);
    fs::write(s.0.join("value"), data).unwrap();
    s.ok(&args); // valid exact retry may recover the verified incomplete tail
    assert_eq!(fs::read(s.0.join("events.juro")).unwrap(), before);
}

#[test]
fn time_ranges_append_pages_and_foreign_cursors_remain_distinct() {
    let s = Scratch::new();
    let vectors: Value =
        serde_json::from_str(include_str!("../../../spec/events/1/vectors.json")).unwrap();
    let vector = &vectors["valid"][1];
    let text = vector["file_hex"].as_str().unwrap();
    let raw = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect::<Vec<_>>();
    fs::write(s.0.join("events.juro"), raw).unwrap();
    let first: Value =
        serde_json::from_slice(&s.ok(&["scan", "events.juro", "--limit", "2"]).stdout).unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), 2);
    assert_eq!(first["page"]["has_more"], true);
    let cursor = first["page"]["cursor"].as_str().unwrap();
    let last: Value =
        serde_json::from_slice(&s.ok(&["scan", "events.juro", "--after", cursor]).stdout).unwrap();
    assert_eq!(last["events"].as_array().unwrap().len(), 1);
    assert_eq!(last["events"][0]["data_hex"], "");
    let time: Value = serde_json::from_slice(
        &s.ok(&["scan", "events.juro", "--from", "-3", "--to", "17"])
            .stdout,
    )
    .unwrap();
    assert_eq!(time["events"].as_array().unwrap().len(), 1);
    assert_eq!(time["events"][0]["accepted_at"], -3);
    assert_eq!(time["events"][0]["data_hex"], "00ff0d0a");
    s.ok(&["init", "foreign.juro"]);
    assert!(
        !s.run(&["scan", "foreign.juro", "--after", cursor])
            .status
            .success()
    );
    assert!(
        !s.run(&["scan", "events.juro", "--limit", "0"])
            .status
            .success()
    );
    assert!(
        !s.run(&["scan", "events.juro", "--from", "17", "--to", "17"])
            .status
            .success()
    );
}

#[test]
fn follow_releases_snapshot_lock_so_another_cli_can_append() {
    let s = Scratch::new();
    s.ok(&["init", "events.juro"]);
    fs::write(s.0.join("value"), b"arrived after follow").unwrap();
    let mut follower = s
        .command(&["follow", "events.juro", "--wait-ms", "3000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(75));
    assert!(follower.try_wait().unwrap().is_none());
    let deadline = Instant::now() + Duration::from_secs(2);
    let receipt = loop {
        let output = s.run(&["append", "events.juro", "value"]);
        if output.status.success() {
            break serde_json::from_slice::<Value>(&output.stdout).unwrap();
        }
        assert!(String::from_utf8_lossy(&output.stderr).contains("Locked"));
        assert!(
            Instant::now() < deadline,
            "follow held its shared lock while waiting"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let result = follower.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let page: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["events"][0]["cursor"], receipt["cursor"]);
    let cursor = page["page"]["cursor"].as_str().unwrap();
    let caught_up: Value = serde_json::from_slice(
        &s.ok(&["follow", "events.juro", "--after", cursor, "--wait-ms", "0"])
            .stdout,
    )
    .unwrap();
    assert!(caught_up["events"].as_array().unwrap().is_empty());
    assert_eq!(caught_up["page"]["cursor"], cursor);
}
