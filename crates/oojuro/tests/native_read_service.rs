#![cfg(target_os = "linux")]
//! Actual native subprocesses and private synthetic authorities; no user data or providers.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{PermissionsExt, symlink},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_oojuro-native");
const BIG: u64 = 9_007_199_254_741_001;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    socket: PathBuf,
}
fn write(path: &Path, data: &[u8]) {
    fs::write(path, data).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let parent = std::env::temp_dir().join(format!(
            "onread-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root: parent.join("root"),
            socket: parent.join("read.sock"),
            parent,
        }
    }
    fn command(&self, op: &str) -> Command {
        let mut c = Command::new(BIN);
        c.arg(op).arg("--root").arg(&self.root);
        c
    }
    fn record(&self) {
        assert!(
            self.command("record-setup")
                .args([
                    "--writer",
                    "synthetic-read",
                    "--owner",
                    "7",
                    "--adapter",
                    "synthetic",
                    "--first-id",
                    &BIG.to_string()
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    fn capture(&self) {
        let policy = self.parent.join("policy.json");
        write(&policy,&serde_json::to_vec(&json!({"version":1,"record":true,"paused":false,"record_thinking":true,"index_thinking":true,"exclude":[],"denied_tools":[]})).unwrap());
        assert!(
            self.command("setup")
                .args([
                    "--harness",
                    "claude-code",
                    "--writer",
                    "claude-code:-synthetic",
                    "--agent",
                    "7",
                    "--policy",
                    policy.to_str().unwrap()
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    fn append(&self) -> Value {
        let input = self.parent.join("input.json");
        let rows = json!([
            {"agent_id":"reported","parent_id":null,"valid_at":1,"kind":"explicit","tool":null,"status":null,"exit":null,"key":"memory","text":"synthetic older original oldonlyneedle","attrs":{},"indexed":true},
            {"agent_id":"reported","parent_id":null,"valid_at":2,"kind":"explicit","tool":null,"status":null,"exit":null,"key":"memory","text":"stored-only synthetic current","attrs":{},"indexed":false},
            {"agent_id":"reported","parent_id":null,"valid_at":3,"kind":"explicit","tool":null,"status":null,"exit":null,"key":null,"text":"visibleuniqueneedle synthetic original","attrs":{},"indexed":true}
        ]);
        write(&input, &serde_json::to_vec(&rows).unwrap());
        let out = self
            .command("record-append")
            .args([
                "--input",
                input.to_str().unwrap(),
                "--request",
                "seed",
                "--allow-unredacted-records",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn start(&self, namespace: &str) -> Server {
        let mut child = self
            .command("serve")
            .args([
                "--socket",
                self.socket.to_str().unwrap(),
                "--namespace",
                namespace,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.socket.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "server exited before binding"
            );
            assert!(Instant::now() < deadline, "server bind deadline");
            thread::sleep(Duration::from_millis(10));
        }
        Server { child }
    }
    fn read(&self, namespace: &str, request: Value) -> Output {
        let mut child = self
            .command("read")
            .args([
                "--socket",
                self.socket.to_str().unwrap(),
                "--namespace",
                namespace,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn value(&self, namespace: &str, request: Value) -> Value {
        let out = self.read(namespace, request);
        assert!(out.status.success(), "native read refused");
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
struct Server {
    child: Child,
}
impl Drop for Server {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(7);
        while self.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            assert!(e.file_type().unwrap().is_file());
            (
                e.file_name().to_string_lossy().to_string(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
#[test]
fn initialized_empty_namespaces_are_readable_but_missing_authority_never_bootstraps() {
    for namespace in ["record-v2", "capture-v1"] {
        let f = Fixture::new();
        if namespace == "record-v2" {
            f.record();
        } else {
            f.capture();
        }
        {
            let server = f.start(namespace);
            let stats = f.value(namespace, json!({"op":"stats"}));
            assert_eq!(
                stats[if namespace == "record-v2" {
                    "records"
                } else {
                    "receipts"
                }],
                0
            );
            assert!(
                f.value(namespace, json!({"op":"range"}))[if namespace == "record-v2" {
                    "records"
                } else {
                    "entries"
                }]
                .as_array()
                .unwrap()
                .is_empty()
            );
            drop(server);
            assert!(
                !f.socket.exists(),
                "owned socket not removed after graceful stop"
            );
        }
        fs::remove_dir_all(f.root.join("journal")).unwrap();
        let mut child = f
            .command("serve")
            .args([
                "--socket",
                f.socket.to_str().unwrap(),
                "--namespace",
                namespace,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() && !f.socket.exists() && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        let bootstrapped = f.socket.exists() || f.root.join("journal").exists();
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        assert!(!bootstrapped, "read startup bootstrapped missing authority");
        assert!(!status.success());
        assert!(!f.root.join("journal").exists());
        assert!(!f.socket.exists());
    }
}
#[test]
fn record_socket_and_actual_mcp_preserve_full_ids_originals_time_keys_and_protected_pages() {
    let f = Fixture::new();
    f.record();
    let ack = f.append();
    let server = f.start("record-v2");
    let before = snapshot(&f.root.join("journal"));
    let original = f.value("record-v2", json!({"op":"get","id":BIG}));
    assert_eq!(original["record"]["id"].as_u64(), Some(BIG));
    assert_eq!(
        original["record"]["text"],
        "synthetic older original oldonlyneedle"
    );
    assert_eq!(original["record"]["adapter"], "synthetic");
    assert_eq!(original["record"]["recorded_at"], ack["recorded_at"][0]);
    let current = f.value("record-v2", json!({"op":"get","key":"memory"}));
    assert_eq!(current["record"]["indexed"], false);
    assert_eq!(current["record"]["id"].as_u64(), Some(BIG + 1));
    assert!(
        f.value("record-v2", json!({"op":"search","query":"oldonlyneedle"}))["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        f.value("record-v2", json!({"op":"search","query":"stored-only"}))["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let historical = f.value(
        "record-v2",
        json!({"op":"get","key":"memory","known_at_us":ack["recorded_at"][0]}),
    );
    assert_eq!(historical["record"]["id"].as_u64(), Some(BIG));
    let page = f.value(
        "record-v2",
        json!({"op":"history","key":"memory","entries":1}),
    );
    assert_eq!(page["records"][0]["id"].as_u64(), Some(BIG));
    let cursor = page["next"].as_str().unwrap();
    let next = f.value(
        "record-v2",
        json!({"op":"range","cursor":cursor,"entries":1}),
    );
    assert_eq!(next["records"][0]["id"].as_u64(), Some(BIG + 1));
    assert!(
        !f.read(
            "record-v2",
            json!({"op":"range","cursor":cursor,"kind":"explicit"})
        )
        .status
        .success()
    );
    let stats = f.value("record-v2", json!({"op":"stats"}));
    assert_eq!(stats["records"], 2);
    assert_eq!(stats["indexed"], 1);
    assert_eq!(stats["stored_only"], 1);
    assert_eq!(
        f.value("record-v2", json!({"op":"stats","versions":"all"}))["records"],
        3
    );
    assert!(!f.read("capture-v1", json!({"op":"stats"})).status.success());
    assert!(
        !f.read("record-v2", json!({"op":"append","text":"not allowed"}))
            .status
            .success()
    );
    assert!(
        !f.read("record-v2", json!({"op":"get","id":BIG,"writer":"other"}))
            .status
            .success()
    );
    assert!(
        !f.read("record-v2", json!({"op":"get","id":BIG,"bytes":200}))
            .status
            .success()
    );
    let mut mcp = f
        .command("mcp")
        .args([
            "--socket",
            f.socket.to_str().unwrap(),
            "--namespace",
            "record-v2",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = mcp.stdin.take().unwrap();
        for request in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"synthetic","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get","arguments":{"id":BIG}}}),
        ] {
            serde_json::to_writer(&mut input, &request).unwrap();
            input
                .write_all(
                    b"
",
                )
                .unwrap();
        }
    }
    let output = mcp.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let messages = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice::<Value>(b).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["result"]["protocolVersion"], "2025-06-18");
    assert!(
        messages[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| !["remember", "forget", "capture"].contains(&t["name"].as_str().unwrap()))
    );
    assert_eq!(
        messages[2]["result"]["structuredContent"]["data"]["record"]["id"].as_u64(),
        Some(BIG)
    );
    assert_eq!(
        snapshot(&f.root.join("journal")),
        before,
        "requests changed authority files"
    );
    drop(server);
    assert!(!f.socket.exists());
    // No socket means no direct-open or legacy fallback, even with a valid existing root.
    assert!(!f.read("record-v2", json!({"op":"stats"})).status.success());
}
#[test]
fn capture_api_preserves_original_receipts_and_never_exposes_raw_or_descriptors() {
    let f = Fixture::new();
    f.capture();
    let source = f.parent.join(".claude/projects/-synthetic/session.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let mut line=serde_json::to_vec(&json!({"type":"user","timestamp":"2026-01-02T03:04:06Z","origin":{"kind":"human"},
        "raw_private_marker":"SYNTHETIC_RAW_ONLY_NOT_PARSED","message":{"content":"captureuniqueneedle synthetic"}})).unwrap();
    line.push(b'\n');
    write(&source, &line);
    let out = f
        .command("capture")
        .args([
            "--source",
            source.to_str().unwrap(),
            "--source-id",
            "34343434343434343434343434343434",
            "--allow-raw-source",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "capture refusal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ack: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = ack["event_ids"][0].as_u64().unwrap();
    let server = f.start("capture-v1");
    let before = snapshot(&f.root.join("journal"));
    let read = f.value("capture-v1", json!({"op":"get","id":id}));
    assert_eq!(read["receipt"]["output"]["event"]["id"].as_u64(), Some(id));
    assert_eq!(
        read["receipt"]["output"]["event"]["text"],
        "captureuniqueneedle synthetic"
    );
    let search = f.value(
        "capture-v1",
        json!({"op":"search","query":"captureuniqueneedle"}),
    );
    assert_eq!(search["hits"][0]["id"].as_u64(), Some(id));
    for result in [
        read,
        search,
        f.value("capture-v1", json!({"op":"range"})),
        f.value("capture-v1", json!({"op":"export"})),
    ] {
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains("SYNTHETIC_RAW_ONLY_NOT_PARSED"));
        assert!(!encoded.contains(source.to_str().unwrap()));
        assert!(!encoded.contains("descriptor"));
    }
    assert!(
        !f.read("capture-v1", json!({"op":"history","key":"invented"}))
            .status
            .success()
    );
    assert!(
        !f.read(
            "capture-v1",
            json!({"op":"restore_raw","out":f.parent.join("raw")})
        )
        .status
        .success()
    );
    assert!(!f.parent.join("raw").exists());
    assert_eq!(snapshot(&f.root.join("journal")), before);
    drop(server);
}
#[test]
fn private_socket_refusal_bounds_busy_and_absolute_partial_request_deadline() {
    let f = Fixture::new();
    f.record();
    let target = f.parent.join("unchanged");
    write(&target, b"synthetic protected file");
    symlink(&target, &f.socket).unwrap();
    assert!(
        !f.command("serve")
            .args([
                "--socket",
                f.socket.to_str().unwrap(),
                "--namespace",
                "record-v2"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        fs::symlink_metadata(&f.socket)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), b"synthetic protected file");
    fs::remove_file(&f.socket).unwrap();
    fs::set_permissions(&f.parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !f.command("serve")
            .args([
                "--socket",
                f.socket.to_str().unwrap(),
                "--namespace",
                "record-v2"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!f.socket.exists());
    fs::set_permissions(&f.parent, fs::Permissions::from_mode(0o700)).unwrap();
    let server = f.start("record-v2");
    let mut oversized = UnixStream::connect(&f.socket).unwrap();
    oversized
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    oversized.write_all(&vec![b'x'; (64 << 10) + 2]).unwrap();
    let mut line = String::new();
    BufReader::new(oversized).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["ok"], false);
    assert!(!line.contains("xxxx"));
    let mut peers = vec![];
    for _ in 0..8 {
        let mut s = UnixStream::connect(&f.socket).unwrap();
        s.write_all(b"{").unwrap();
        peers.push(s);
        thread::sleep(Duration::from_millis(20));
    }
    let mut busy = UnixStream::connect(&f.socket).unwrap();
    busy.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut line = String::new();
    BufReader::new(&mut busy).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["error"],
        "reader_busy"
    );
    let start = Instant::now();
    let mut byte = [0u8; 1];
    peers[0]
        .set_read_timeout(Some(Duration::from_secs(7)))
        .unwrap();
    assert_eq!(peers[0].read(&mut byte).unwrap(), 0);
    assert!(start.elapsed() < Duration::from_secs(7));
    drop(peers);
    assert_eq!(f.value("record-v2", json!({"op":"stats"}))["records"], 0);
    drop(server);
}
