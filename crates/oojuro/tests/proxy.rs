//! The model API proxy with real `oojurod` processes and a local mock
//! upstream (no real model API): inside `oojurod tail` (one daemon), its
//! records reach the journal through the store's single writer and the
//! settings apply (`record = false`, `exclude`, a withheld call's result);
//! `oojurod proxy` on its own owns the journal when no recorder runs and
//! journals through a running recorder's socket otherwise.
mod common;
use common::Scratch;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

#[track_caller]
fn wait_until(limit: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < limit, "timed out after {limit:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Last user text of an Anthropic request.
fn last_text(req: &Value) -> String {
    let m = req["messages"].as_array().unwrap().last().unwrap();
    match &m["content"] {
        Value::String(s) => s.clone(),
        Value::Array(b) => b
            .iter()
            .rev()
            .find(|x| x["type"] == "text")
            .map(|x| x["text"].as_str().unwrap().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// A minimal HTTP/1.1 mock of the Anthropic Messages API (one request per connection).
fn mock_upstream() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let req: Value = serde_json::from_slice(&body).unwrap();
                let text = last_text(&req);
                let content = if text.contains("read notes") {
                    json!([{"type":"tool_use","id":"tu_notes","name":"Read","input":{"file_path":"/w/notes.txt"}}])
                } else if text.contains("read env") {
                    json!([{"type":"tool_use","id":"tu_env","name":"Read","input":{"file_path":"/w/.env"}}])
                } else {
                    json!([{"type":"text","text":format!("noted: {text}")}])
                };
                let resp = json!({"id":"msg_1","type":"message","role":"assistant","model":"mock-2","content":content,
                    "stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":4}})
                .to_string();
                let mut s = s;
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nrequest-id: req_m\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                    resp.len()
                );
            });
        }
    });
    port
}

/// POST to the proxy; (status, body).
fn post(port: u16, writer: &str, msgs: &[Value]) -> (u16, String) {
    post_as(port, writer, "", msgs)
}

const CLAUDE_UA: &str = "claude-cli/2.1.3 (external, cli)";
const CODEX_UA: &str = "codex_cli_rs/0.50.0 (Ubuntu 24.4.0; x86_64) xterm-256color";

/// [`post`] with a `User-Agent` (none when empty).
fn post_as(port: u16, writer: &str, ua: &str, msgs: &[Value]) -> (u16, String) {
    let body =
        json!({"model":"mock-2","max_tokens":100,"system":"You are the proxy test agent.","messages":msgs}).to_string();
    let ua = if ua.is_empty() { String::new() } else { format!("user-agent: {ua}\r\n") };
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\ncontent-type: application/json\r\nanthropic-version: 2023-06-01\r\nx-api-key: test-key-not-real\r\nx-oojuro-writer: {writer}\r\n{ua}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, out.split_once("\r\n\r\n").map(|x| x.1.to_string()).unwrap_or_default())
}

fn oojuro(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .args(args)
        .current_dir(home)
        .env("OOJURO_HOME", home)
        .env_remove("OOJURO_WRITER")
        .env_remove("CLAUDECODE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Every record of `writer` (through the recorder when one serves the journal).
fn export(home: &Path, writer: &str) -> Vec<Value> {
    let o = oojuro(home, &["export", "--writer", writer]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn daemon(home: &Path, args: &[&str], log: &str) -> Child {
    let log = std::fs::File::create(home.join(log)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_oojurod"))
        .args(args)
        .arg("--store")
        .arg(home.join("journal"))
        .env("OOJURO_HOME", home)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap()
}

fn stop(mut c: Child) {
    // SAFETY: plain signal to our own child.
    unsafe {
        libc::kill(c.id() as i32, libc::SIGTERM);
    }
    let start = Instant::now();
    while c.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = c.kill();
            panic!("daemon did not stop on SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(c.wait().unwrap().success(), "daemon exit status");
}

fn listening(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn user(content: Value) -> Value {
    json!({"role":"user","content":content})
}

fn texts<'a>(recs: &'a [Value], kind: &str) -> Vec<&'a str> {
    recs.iter().filter(|r| r["kind"] == kind).map(|r| r["text"].as_str().unwrap()).collect()
}

#[test]
fn proxy_inside_the_recorder_honors_settings() {
    let s = Scratch::new("proxy-rec");
    let home = &s.0;
    let logs = s.path("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let (mock, port) = (mock_upstream(), free_port());
    std::fs::write(
        home.join("config.toml"),
        format!(
            "exclude = [\".env\"]\n\n[proxy]\nenabled = true\nlisten = \"127.0.0.1:{port}\"\nanthropic_upstream = \"http://127.0.0.1:{mock}\"\n\n[agents.\"quiet-agent\"]\nrecord = false\n"
        ),
    )
    .unwrap();
    let rec = daemon(home, &["tail", "--poll-ms", "50", "--no-dedupe-stats", logs.to_str().unwrap()], "tail.log");
    wait_until(Duration::from_secs(30), || {
        oojuro::client::hello(&home.join("oojurod.sock")).is_some() && listening(port)
    });

    let w = "proxy-test";
    let mut h = vec![user(json!("remember the zebra protocol"))];
    let (st, body) = post(port, w, &h);
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("noted: remember the zebra protocol"));
    h.push(json!({"role":"assistant","content":[{"type":"text","text":"noted: remember the zebra protocol"}]}));
    h.push(user(json!("please read notes")));
    assert_eq!(post(port, w, &h).0, 200);
    h.push(json!({"role":"assistant","content":[{"type":"tool_use","id":"tu_notes","name":"Read","input":{"file_path":"/w/notes.txt"}}]}));
    h.push(user(json!([{"type":"tool_result","tool_use_id":"tu_notes","content":"notes body: quokka"},{"type":"text","text":"now read env"}])));
    assert_eq!(post(port, w, &h).0, 200);
    h.push(json!({"role":"assistant","content":[{"type":"tool_use","id":"tu_env","name":"Read","input":{"file_path":"/w/.env"}}]}));
    h.push(user(json!([{"type":"tool_result","tool_use_id":"tu_env","content":"API_TOKEN=abc123"},{"type":"text","text":"thanks"}])));
    assert_eq!(post(port, w, &h).0, 200);
    // record = false for this writer
    assert_eq!(post(port, "quiet-agent", &[user(json!("quiet plans for the heist"))]).0, 200);

    wait_until(Duration::from_secs(30), || export(home, w).iter().any(|r| r["text"] == "noted: thanks"));
    let recs = export(home, w);
    assert_eq!(
        texts(&recs, "user_input"),
        vec!["remember the zebra protocol", "please read notes", "now read env", "thanks"]
    );
    assert_eq!(texts(&recs, "system"), vec!["You are the proxy test agent."]);
    assert_eq!(texts(&recs, "assistant"), vec!["noted: remember the zebra protocol", "noted: thanks"]);
    assert_eq!(texts(&recs, "tool_call"), vec![r#"{"file_path":"/w/notes.txt"}"#]);
    let result = recs.iter().find(|r| r["kind"] == "tool_result").unwrap();
    assert_eq!((result["text"].as_str(), result["tool"].as_str()), (Some("notes body: quokka"), Some("Read")));
    // the excluded call and its result are not stored
    let all = serde_json::to_string(&recs).unwrap();
    assert!(!all.contains(".env") && !all.contains("API_TOKEN"), "{all}");
    assert!(recs.iter().all(|r| r["meta"]["via"] == "proxy" && r["writer"] == w));
    let out = recs.iter().find(|r| r["text"] == "noted: thanks").unwrap();
    assert_eq!(out["meta"]["model"], "mock-2");
    assert_eq!(out["meta"]["usage"], json!({"in": 3, "out": 4}));
    assert!(!all.contains("test-key-not-real"));
    // the quiet writer is not even registered
    let stats: Value = serde_json::from_slice(&oojuro(home, &["--json", "stats"]).stdout).unwrap();
    let writers: Vec<&str> =
        stats["writers"].as_array().unwrap().iter().map(|x| x["writer"].as_str().unwrap()).collect();
    assert_eq!(writers, vec![w]);
    // searchable through the recorder
    let o = oojuro(home, &["--json", "search", "zebra protocol"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("remember the zebra protocol"));
    stop(rec);
    let log = std::fs::read_to_string(home.join("tail.log")).unwrap();
    assert!(log.contains("proxy: listening"), "{log}");
    // the journal reopens with the proxy records (no source position needed)
    assert_eq!(export(home, w).len(), recs.len());
}

#[test]
fn standalone_proxy_owns_the_journal_or_uses_the_recorder() {
    let s = Scratch::new("proxy-alone");
    let home = &s.0;
    let logs = s.path("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let mock = mock_upstream();
    let up = format!("http://127.0.0.1:{mock}");
    // no recorder: the proxy owns the journal and serves its socket
    let p1 = free_port();
    let listen = format!("127.0.0.1:{p1}");
    let owner =
        daemon(home, &["proxy", "--listen", &listen, "--anthropic-upstream", &up, "--writer", "w-owner"], "p1.log");
    wait_until(Duration::from_secs(30), || {
        oojuro::client::hello(&home.join("oojurod.sock")).is_some() && listening(p1)
    });
    // an empty x-oojuro-writer falls back to the listener's name
    assert_eq!(post(p1, "", &[user(json!("owner mode question"))]).0, 200);
    // no tailer: Claude Code's calls are recorded too
    assert_eq!(post_as(p1, "w-owner-claude", CLAUDE_UA, &[user(json!("claude without a tailer"))]).0, 200);
    wait_until(Duration::from_secs(30), || export(home, "w-owner").len() >= 3);
    assert_eq!(texts(&export(home, "w-owner"), "assistant"), vec!["noted: owner mode question"]);
    wait_until(Duration::from_secs(30), || export(home, "w-owner-claude").len() >= 2);
    stop(owner);
    // a recorder tails Claude Code's transcripts (not Codex's): the standalone proxy journals
    // through its socket and leaves Claude Code to it
    let claude_dir = s.path("u/.claude/projects");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let rec = daemon(home, &["tail", "--poll-ms", "50", "--no-dedupe-stats", claude_dir.to_str().unwrap()], "tail.log");
    wait_until(Duration::from_secs(30), || oojuro::client::hello(&home.join("oojurod.sock")).is_some());
    let hello = oojuro::client::hello(&home.join("oojurod.sock")).unwrap();
    assert_eq!(hello["tailed"], json!(["claude-code"]));
    let p2 = free_port();
    let listen = format!("127.0.0.1:{p2}");
    let proxy = daemon(home, &["proxy", "--listen", &listen, "--anthropic-upstream", &up], "p2.log");
    wait_until(Duration::from_secs(30), || listening(p2));
    assert_eq!(post(p2, "w-socket", &[user(json!("socket mode question"))]).0, 200);
    let (st, body) = post_as(p2, "w-socket-claude", CLAUDE_UA, &[user(json!("left to the tailer"))]);
    assert_eq!(st, 200);
    assert!(body.contains("noted: left to the tailer"), "forwarded: {body}");
    assert_eq!(post_as(p2, "w-socket-codex", CODEX_UA, &[user(json!("codex is not tailed"))]).0, 200);
    wait_until(Duration::from_secs(30), || export(home, "w-socket").len() >= 3);
    wait_until(Duration::from_secs(30), || export(home, "w-socket-codex").len() >= 3);
    assert_eq!(texts(&export(home, "w-socket"), "user_input"), vec!["socket mode question"]);
    assert_eq!(texts(&export(home, "w-socket-codex"), "user_input"), vec!["codex is not tailed"]);
    assert!(export(home, "w-socket-claude").is_empty());
    stop(proxy);
    let log = std::fs::read_to_string(home.join("p2.log")).unwrap();
    assert!(log.contains("journaling through the recorder"), "{log}");
    assert!(log.contains(r#"transcripts tailed for ["claude-code"]"#), "{log}");
    assert!(log.contains(r#""skipped_tailed": 1"#), "{log}");
    stop(rec);
    // loopback only
    let o = Command::new(env!("CARGO_BIN_EXE_oojurod"))
        .args(["proxy", "--listen", "0.0.0.0:1", "--store"])
        .arg(home.join("journal"))
        .env("OOJURO_HOME", home)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("loopback"));
}

/// The transcript recorder wins: inside `oojurod tail`, the proxy leaves
/// agents whose transcripts the recorder reads (Claude Code here, from the
/// directory it follows) to it, whatever their writer header; other agents
/// are recorded; `record_tailed = true` records them all.
#[test]
fn transcript_recorder_wins_inside_the_daemon() {
    let s = Scratch::new("proxy-tailed");
    let home = &s.0;
    let claude_dir = s.path("u/.claude/projects");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let (mock, port) = (mock_upstream(), free_port());
    let config = |extra: &str| {
        format!(
            "[proxy]\nenabled = true\n{extra}listen = \"127.0.0.1:{port}\"\nanthropic_upstream = \"http://127.0.0.1:{mock}\"\n"
        )
    };
    std::fs::write(home.join("config.toml"), config("")).unwrap();
    let args = ["tail", "--poll-ms", "50", "--no-dedupe-stats", claude_dir.to_str().unwrap()];
    let rec = daemon(home, &args, "tail.log");
    let sock = home.join("oojurod.sock");
    wait_until(Duration::from_secs(30), || oojuro::client::hello(&sock).is_some() && listening(port));
    assert_eq!(oojuro::client::hello(&sock).unwrap()["tailed"], json!(["claude-code"]));
    let (st, body) = post_as(port, "chosen-writer", CLAUDE_UA, &[user(json!("claude through the proxy"))]);
    assert_eq!(st, 200);
    assert!(body.contains("noted: claude through the proxy"));
    assert_eq!(post_as(port, "codex-w", CODEX_UA, &[user(json!("codex through the proxy"))]).0, 200);
    assert_eq!(post_as(port, "sdk-w", "Anthropic/Python 0.60.0", &[user(json!("sdk through the proxy"))]).0, 200);
    wait_until(Duration::from_secs(30), || export(home, "codex-w").len() >= 3 && export(home, "sdk-w").len() >= 3);
    assert!(export(home, "chosen-writer").is_empty(), "the writer header does not override the ruling");
    let stats: Value = serde_json::from_slice(&oojuro(home, &["--json", "stats"]).stdout).unwrap();
    let writers: Vec<&str> =
        stats["writers"].as_array().unwrap().iter().map(|x| x["writer"].as_str().unwrap()).collect();
    assert_eq!(writers, vec!["codex-w", "sdk-w"]);
    stop(rec);
    let log = std::fs::read_to_string(home.join("tail.log")).unwrap();
    assert!(log.contains(r#"transcripts tailed for ["claude-code"]"#), "{log}");
    assert!(log.contains(r#""skipped_tailed": 1"#), "{log}");
    // record_tailed = true: Claude Code's calls are recorded as well
    std::fs::write(home.join("config.toml"), config("record_tailed = true\n")).unwrap();
    let rec = daemon(home, &args, "tail2.log");
    wait_until(Duration::from_secs(30), || oojuro::client::hello(&sock).is_some() && listening(port));
    assert_eq!(post_as(port, "chosen-writer", CLAUDE_UA, &[user(json!("claude recorded anyway"))]).0, 200);
    wait_until(Duration::from_secs(30), || export(home, "chosen-writer").len() >= 3);
    assert_eq!(texts(&export(home, "chosen-writer"), "user_input"), vec!["claude recorded anyway"]);
    stop(rec);
    let log = std::fs::read_to_string(home.join("tail2.log")).unwrap();
    assert!(log.contains("overridden by record_tailed") && log.contains(r#""skipped_tailed": 0"#), "{log}");
}
