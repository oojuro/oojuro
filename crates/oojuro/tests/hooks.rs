//! `oojuro hook <event>` as Claude Code runs it (real processes, temporary
//! homes, synthetic data): each event recorded or skipped, the direct path
//! and the recorder path, no duplicates with the log tailer, injection on
//! and off within `top_k`, `min_bits` and the budget, Oojuro's own echo
//! never injected again, and a broken journal never failing the agent.
mod common;
use common::{Scratch, append, tool_call, tool_result, user};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const PROJECT: &str = "-w-demo";
const WRITER: &str = "claude-code:-w-demo";

struct Recorder {
    child: Option<Child>,
}

impl Recorder {
    fn start(home: &Path, logs: &Path) -> Self {
        let log = std::fs::File::create(home.join("daemon.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_oojurod"))
            .args(["tail", "--poll-ms", "50", "--no-dedupe-stats"])
            .arg(logs)
            .env("OOJURO_HOME", home)
            .env("HOME", home)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let socket = home.join("oojurod.sock");
        let start = Instant::now();
        while oojuro::client::hello(&socket).is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "recorder did not start: {}",
                std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Recorder { child: Some(child) }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            // SAFETY: plain signal to our own child.
            unsafe {
                libc::kill(c.id() as i32, libc::SIGTERM);
            }
            let start = Instant::now();
            while c.try_wait().unwrap().is_none() && start.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

struct Hook {
    stdout: String,
    stderr: String,
    code: i32,
}

/// Run `oojuro [pre...] hook EVENT` with `input` on stdin, in a private home.
fn hook_with(home: &Path, pre: &[&str], event: &str, input: &str) -> Hook {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .args(pre)
        .args(["hook", event])
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .env_remove("OOJURO_WRITER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    Hook {
        stdout: String::from_utf8(o.stdout).unwrap(),
        stderr: String::from_utf8(o.stderr).unwrap(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn hook(home: &Path, event: &str, payload: &Value) -> Hook {
    let h = hook_with(home, &[], event, &payload.to_string());
    assert_eq!(h.code, 0, "{event}: {}", h.stderr);
    h
}

fn oojuro(home: &Path, args: &[&str]) -> Value {
    let o = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .arg("--json")
        .args(args)
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .env("USER", "tester")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// `oojuro config set KEY VALUE --agent WRITER` (in the private home).
fn set(home: &Path, key: &str, value: &str) {
    let o = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .args(["config", "set", key, value, "--agent", WRITER])
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn transcript(root: &Path, session: &str) -> PathBuf {
    root.join(".claude/projects").join(PROJECT).join(format!("{session}.jsonl"))
}

/// A hook payload with the common fields.
fn payload(t: &Path, event: &str, extra: Value) -> Value {
    let mut v = json!({
        "session_id": "s1", "transcript_path": t, "cwd": "/w/demo", "permission_mode": "default",
        "hook_event_name": event,
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn records(home: &Path) -> Vec<Value> {
    oojuro(home, &["range", "--limit", "1000"]).as_array().unwrap().clone()
}

fn kinds(rs: &[Value]) -> Vec<String> {
    rs.iter().map(|r| r["kind"].as_str().unwrap().to_string()).collect()
}

fn injected_ids(out: &str) -> Vec<u64> {
    if out.is_empty() {
        return Vec::new();
    }
    let v: Value = serde_json::from_str(out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    ctx.lines()
        .filter(|l| l.starts_with('['))
        .map(|l| l.split('#').nth(1).unwrap().split(' ').next().unwrap().parse().unwrap())
        .collect()
}

fn context(out: &str) -> String {
    let v: Value = serde_json::from_str(out).unwrap();
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
}

#[test]
fn every_event_is_recorded_once_on_the_direct_path() {
    let s = Scratch::new("hook-direct");
    let home = &s.0;
    let t = transcript(home, "s1");
    let token = format!("ghp_{}", "aB3".repeat(12));
    // SessionStart records nothing (and creates no journal)
    let h = hook(home, "SessionStart", &payload(&t, "SessionStart", json!({ "source": "startup", "model": "m" })));
    assert_eq!((h.stdout.as_str(), h.stderr.as_str()), ("", ""));
    assert!(!home.join("journal").exists());
    hook(
        home,
        "UserPromptSubmit",
        &payload(&t, "UserPromptSubmit", json!({ "prompt": "how do zebras sleep", "prompt_id": "p1" })),
    );
    let bash = json!({ "command": format!("git push https://x:{token}@github.com/o/r"), "description": "push" });
    hook(
        home,
        "PreToolUse",
        &payload(&t, "PreToolUse", json!({ "tool_name": "Bash", "tool_input": bash, "tool_use_id": "t1" })),
    );
    let post = payload(
        &t,
        "PostToolUse",
        json!({ "tool_name": "Bash", "tool_input": bash, "tool_use_id": "t1",
                "tool_response": { "stdout": "zebras sleep standing up", "stderr": "", "interrupted": false }, "duration_ms": 12 }),
    );
    hook(home, "PostToolUse", &post);
    hook(home, "PostToolUse", &post); // installed twice: still once
    hook(
        home,
        "PostToolUseFailure",
        &payload(
            &t,
            "PostToolUseFailure",
            json!({ "tool_name": "Read", "tool_input": { "file_path": "/w/demo/missing.txt" }, "tool_use_id": "t2",
                    "error": "File does not exist.", "is_interrupt": false }),
        ),
    );
    hook(
        home,
        "Stop",
        &payload(
            &t,
            "Stop",
            json!({ "stop_hook_active": false, "prompt_id": "p1", "last_assistant_message": "Zebras sleep standing." }),
        ),
    );
    let sub = t.with_extension("").join("subagents/agent-a1.jsonl");
    hook(
        home,
        "SubagentStop",
        &payload(
            &t,
            "SubagentStop",
            json!({ "agent_id": "a1", "agent_type": "Explore", "agent_transcript_path": sub,
                    "last_assistant_message": "Found the zebra notes." }),
        ),
    );
    // the prompt again (same prompt id): once
    hook(
        home,
        "UserPromptSubmit",
        &payload(&t, "UserPromptSubmit", json!({ "prompt": "how do zebras sleep", "prompt_id": "p1" })),
    );

    let rs = records(home);
    assert_eq!(
        kinds(&rs),
        ["user_input", "tool_call", "tool_result", "tool_call", "tool_result", "assistant", "assistant"],
        "{rs:#?}"
    );
    assert!(rs.iter().all(|r| r["writer"] == WRITER && r["meta"]["via"] == "hook"));
    let main = rs[0]["session"].clone();
    assert!(rs[..6].iter().all(|r| r["session"] == main), "one session for the main transcript");
    assert_ne!(rs[6]["session"], main, "the subagent's own session");
    assert_eq!(rs[0]["meta"]["from"], "human");
    assert_eq!(rs.iter().map(|r| r["seq"].as_u64().unwrap()).take(6).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5]);
    assert_eq!(rs[0]["turn"], 1);
    let call = rs[1]["text"].as_str().unwrap();
    assert!(!call.contains(&token) && call.contains("[REDACTED:github:"), "redacted: {call}");
    assert!(call.starts_with(r#"{"command":"#), "verbatim key order: {call}");
    assert_eq!(rs[2]["text"], "zebras sleep standing up");
    assert_eq!((rs[2]["tool"].as_str(), rs[2]["meta"]["call"].as_str()), (Some("Bash"), Some("t1")));
    assert_eq!(rs[4]["text"], "File does not exist.");
    assert_eq!(rs[5]["text"], "Zebras sleep standing.");
    // recorded records are searchable at once
    let hits = oojuro(home, &["search", "zebras sleep standing", "--writer", WRITER]);
    let hit = hits.as_array().unwrap().iter().find(|h| h["id"] == rs[5]["id"]).unwrap();
    // hits carry the session and turn the injection's recency prior reads, keyed like the hook's session
    let key = format!("{:016x}", oojuro::append::session_id(t.to_str().unwrap()));
    assert_eq!((hit["session"].as_str(), hit["turn"].as_u64()), (Some(key.as_str()), Some(1)));
    assert_eq!(rs[5]["session"].as_str(), Some(key.as_str()));
}

#[test]
fn the_recorder_path_records_only_what_no_tailer_reads() {
    let s = Scratch::new("hr");
    let home = &s.0;
    let logs = home.join(".claude/projects");
    let t = transcript(home, "s1");
    append(&t, &user("2026-01-02T03:04:05Z", "/w/demo", "tailer sees this prompt"));
    let _r = Recorder::start(home, &logs);
    let deadline = Instant::now() + Duration::from_secs(20);
    while records(home).is_empty() {
        assert!(Instant::now() < deadline, "tailer did not record");
        std::thread::sleep(Duration::from_millis(50));
    }
    // the tailer reads this transcript: the hook adds nothing
    let h = hook(
        home,
        "UserPromptSubmit",
        &payload(&t, "UserPromptSubmit", json!({ "prompt": "tailer sees this prompt" })),
    );
    assert_eq!((h.stdout.as_str(), h.stderr.as_str()), ("", ""));
    hook(
        home,
        "PostToolUse",
        &payload(
            &t,
            "PostToolUse",
            json!({ "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "t1", "tool_response": "a.txt" }),
        ),
    );
    assert_eq!(records(home).len(), 1, "only the tailer's record");
    // a transcript elsewhere (no tailer reads it): recorded through the recorder
    let other = home.join("remote/projects").join(PROJECT).join("s2.jsonl");
    hook(
        home,
        "UserPromptSubmit",
        &payload(&other, "UserPromptSubmit", json!({ "prompt": "remote prompt about llamas" })),
    );
    hook(
        home,
        "PostToolUse",
        &payload(
            &other,
            "PostToolUse",
            json!({ "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "t9", "tool_response": "llamas.txt" }),
        ),
    );
    let rs = records(home);
    assert_eq!(kinds(&rs), ["user_input", "user_input", "tool_call", "tool_result"]);
    assert_eq!(rs[1]["meta"]["via"], "hook");
    assert!(rs.iter().all(|r| r["writer"] == WRITER));
    let stats = oojuro(home, &["stats"]);
    assert!(stats["served_by"].as_str().unwrap().starts_with("oojurod (pid"));
}

/// An earlier session of the agent (tailer-captured, old timestamps), with
/// Oojuro's own echo in it: a memory lookup and its result, an injected
/// context attachment, and a prompt that carries an injection (as a proxy
/// would see it).
fn seed_history(home: &Path) -> (PathBuf, Vec<u64>) {
    let logs = home.join("old-logs/projects");
    let f = logs.join(PROJECT).join("old.jsonl");
    let ts = "2026-01-02T03:04:05Z";
    // background: ordinary work on other topics
    let words = [
        "kernel", "compiler", "garden", "invoice", "router", "pasta", "violin", "glacier", "pixel", "harbor", "ledger",
        "sensor", "canvas", "tunnel", "orchid", "budget", "rocket", "parser", "window", "cache", "socket", "lambda",
    ];
    for i in 0..80usize {
        let n = words.len();
        let (a, b, c) = (words[i % n], words[(i * 7 + 3) % n], words[(i * 13 + 5) % n]);
        let text = match i % 3 {
            0 => format!("ran the {a} migration for the {b} {c} release"),
            1 => format!("how do we deploy the {a} service after the {b} {c} test"),
            _ => format!("checked the {a} {b} steps and the {c} notes"),
        };
        append(&f, &user(ts, "/w/demo", &text));
    }
    append(&f, &user(ts, "/w/demo", "where are the zebra migration steps"));
    append(&f, &tool_result(ts, "/w/demo", "x0", "the zebra migration deploys with ./migrate.sh once the VPN is up"));
    append(&f, &tool_call(ts, "/w/demo", "x1", "Bash", json!({ "command": "oojuro search zebra migration deploy" })));
    append(&f, &tool_result(ts, "/w/demo", "x1", "#7 21.0 bits zebra migration deploy notes (echo)"));
    let inj = "Earlier records:\n<oojuro-context v=\"1\" items=\"1\">\n[1] #7 zebra migration deploy (old injection)\n</oojuro-context>";
    let attach = json!({ "type": "attachment", "timestamp": ts, "attachment": { "type": "hook_additional_context", "content": [inj] },
                         "rendered": [{ "content": format!("<system-reminder>\n{inj}\n</system-reminder>") }] });
    append(&f, &format!("{attach}\n"));
    append(&f, &user(ts, "/w/demo", &format!("{inj}\nzebra migration deploy, again")));
    let o = Command::new(env!("CARGO_BIN_EXE_oojurod"))
        .args(["import", "--no-dedupe-stats"])
        .arg(&logs)
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rs = records(home);
    let rs = &rs[80..];
    let genuine: Vec<u64> =
        rs.iter().filter(|r| r["meta"].get("oojuro").is_none()).map(|r| r["id"].as_u64().unwrap()).collect();
    let marks: Vec<&str> = rs.iter().map(|r| r["meta"]["oojuro"].as_str().unwrap_or("-")).collect();
    assert_eq!(marks, ["-", "-", "call", "result", "context", "context"], "{rs:#?}");
    (logs, genuine)
}

#[test]
fn injection_is_off_by_default_and_respects_the_settings() {
    let s = Scratch::new("hook-inject");
    let home = &s.0;
    let (_, genuine) = seed_history(home);
    let t = transcript(home, "s1");
    let prompt = |p: &str| payload(&t, "UserPromptSubmit", json!({ "prompt": p }));
    let q = "how do we deploy the zebra migration";
    // off by default (dogfood): nothing on stdout
    let h = hook(home, "UserPromptSubmit", &prompt(q));
    assert_eq!((h.stdout.as_str(), h.stderr.as_str()), ("", ""));
    // on for this agent only (bits grow with the store: this one is tiny)
    set(home, "inject", "true");
    set(home, "min_bits", "1");
    let h = hook(home, "UserPromptSubmit", &prompt(q));
    assert_eq!(h.stderr, "");
    let ids = injected_ids(&h.stdout);
    assert!(!ids.is_empty() && ids.iter().all(|i| genuine.contains(i)), "only genuine records: {ids:?} of {genuine:?}");
    let ctx = context(&h.stdout);
    assert!(ctx.contains("./migrate.sh") && !ctx.contains("(echo)") && !ctx.contains("old injection"), "{ctx}");
    // another agent: still off
    let other = payload(&home.join(".claude/projects/-w-other/s.jsonl"), "UserPromptSubmit", json!({ "prompt": q }));
    assert_eq!(hook(home, "UserPromptSubmit", &other).stdout, "");
    // min_bits above every score: nothing
    set(home, "min_bits", "10000");
    assert_eq!(hook(home, "UserPromptSubmit", &prompt(q)).stdout, "");
    // (the prompts recorded meanwhile make these words common: lower the bar)
    set(home, "min_bits", "0");
    // top_k and the token budget
    set(home, "top_k", "1");
    let h = hook(home, "UserPromptSubmit", &prompt(q));
    assert_eq!(injected_ids(&h.stdout).len(), 1, "{} {}", h.stdout, h.stderr);
    set(home, "top_k", "20");
    set(home, "budget_tokens", "120");
    let out = hook(home, "UserPromptSubmit", &prompt(q)).stdout;
    assert!(out.is_empty() || context(&out).chars().count() <= 480, "{out}");
    set(home, "budget_tokens", "2500");
    // a prompt with nothing relevant: nothing injected
    assert_eq!(hook(home, "UserPromptSubmit", &prompt("qwxz plmo")).stdout, "");
}

#[test]
fn a_re_recorded_injection_is_never_injected_again() {
    let s = Scratch::new("hook-echo");
    let home = &s.0;
    let (logs, genuine) = seed_history(home);
    set(home, "inject", "true");
    set(home, "min_bits", "0");
    let t = transcript(home, "s1");
    let q = "zebra migration deploy steps";
    let h = hook(home, "UserPromptSubmit", &payload(&t, "UserPromptSubmit", json!({ "prompt": q })));
    let first = injected_ids(&h.stdout);
    assert!(!first.is_empty());
    let ctx = context(&h.stdout);
    // the hook recorded what it injected (no tailer reads this transcript): marked
    let rs = records(home);
    let own = rs.iter().find(|r| r["meta"]["sub"] == "hook_additional_context" && r["meta"]["via"] == "hook").unwrap();
    assert_eq!(own["meta"]["oojuro"], "context");
    // ... and the injection comes back through the tailer, as Claude Code
    // writes it, and inside a prompt (as a model proxy would see it)
    let f = logs.join(PROJECT).join("old.jsonl");
    let ts = "2026-01-03T03:04:05Z";
    let attach = json!({ "type": "attachment", "timestamp": ts, "attachment": { "type": "hook_additional_context", "content": [ctx] },
                         "rendered": [{ "content": format!("<system-reminder>\nUserPromptSubmit hook additional context: {ctx}\n</system-reminder>") }] });
    append(&f, &format!("{attach}\n"));
    append(&f, &user(ts, "/w/demo", &format!("{ctx}\nzebra migration deploy steps")));
    let o = Command::new(env!("CARGO_BIN_EXE_oojurod"))
        .args(["import", "--no-dedupe-stats"])
        .arg(&logs)
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rs = records(home);
    let echoes: Vec<u64> =
        rs.iter().filter(|r| r["meta"]["oojuro"] == "context").map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(echoes.len(), 5, "old attachment + old prompt + own record + new attachment + new prompt");
    // the same prompt again: the same genuine records, never an injection
    for _ in 0..2 {
        let h = hook(home, "UserPromptSubmit", &payload(&t, "UserPromptSubmit", json!({ "prompt": q })));
        let ids = injected_ids(&h.stdout);
        assert!(!ids.is_empty() && ids.iter().all(|i| genuine.contains(i)), "{ids:?} vs echoes {echoes:?}");
        assert_eq!(ids, first);
    }
    // explicit search does not find the echo either
    let hits = oojuro(home, &["search", "zebra migration deploy", "-k", "50"]);
    assert!(hits.as_array().unwrap().iter().all(|h| !echoes.contains(&h["id"].as_u64().unwrap())));
}

#[test]
fn injection_and_recording_through_the_recorder() {
    let s = Scratch::new("hri");
    let home = &s.0;
    seed_history(home);
    set(home, "inject", "true");
    set(home, "min_bits", "1");
    // the recorder tails other directories: the hook records and injects through it
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let _r = Recorder::start(home, &elsewhere);
    let t = transcript(home, "s1");
    let h = hook(
        home,
        "UserPromptSubmit",
        &payload(&t, "UserPromptSubmit", json!({ "prompt": "deploy the zebra migration" })),
    );
    assert!(!injected_ids(&h.stdout).is_empty(), "{}", h.stderr);
    let rs = records(home);
    let mine: Vec<&Value> = rs.iter().filter(|r| r["meta"]["via"] == "hook").collect();
    assert_eq!(kinds(&mine.into_iter().cloned().collect::<Vec<_>>()), ["user_input", "system"]);
}

#[test]
fn a_broken_journal_never_fails_the_agent() {
    let s = Scratch::new("hb");
    let home = &s.0;
    let t = transcript(home, "s1");
    let p = payload(
        &t,
        "PostToolUse",
        json!({ "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "t1", "tool_response": "x" }),
    );
    // the journal path is a file
    std::fs::write(home.join("journal"), b"not a journal").unwrap();
    let h = hook_with(home, &[], "PostToolUse", &p.to_string());
    assert_eq!((h.code, h.stdout.as_str()), (0, ""));
    assert!(h.stderr.contains("oojuro hook PostToolUse: record:"), "{}", h.stderr);
    // with injection on as well
    std::fs::write(home.join("config.toml"), "inject = true\n").unwrap();
    let up = payload(&t, "UserPromptSubmit", json!({ "prompt": "anything" }));
    let h = hook_with(home, &[], "UserPromptSubmit", &up.to_string());
    assert_eq!((h.code, h.stdout.as_str()), (0, ""));
    assert!(!h.stderr.is_empty());
    // settings that do not parse, input that is not JSON, unknown events, bad options
    std::fs::remove_file(home.join("journal")).unwrap();
    std::fs::write(home.join("config.toml"), "inject = [oops\n").unwrap();
    let h = hook_with(home, &[], "UserPromptSubmit", &up.to_string());
    assert_eq!((h.code, h.stdout.as_str()), (0, ""));
    assert!(h.stderr.contains("settings"), "{}", h.stderr);
    for (pre, ev, input) in
        [(&[][..], "PreToolUse", "{not json"), (&[][..], "NoSuchEvent", "{}"), (&["--store"][..], "PreToolUse", "{}")]
    {
        let h = hook_with(home, pre, ev, input);
        assert_eq!((h.code, h.stdout.as_str()), (0, ""), "{pre:?} {ev}");
        assert!(!h.stderr.is_empty());
    }
    // a corrupt journal state
    std::fs::remove_file(home.join("config.toml")).unwrap();
    std::fs::create_dir_all(home.join("journal")).unwrap();
    std::fs::write(home.join("journal/capture-state.json"), b"{broken").unwrap();
    let h = hook_with(home, &[], "PostToolUse", &p.to_string());
    assert_eq!((h.code, h.stdout.as_str()), (0, ""));
    assert!(!h.stderr.is_empty());
    // a journal held by a recorder that does not serve its socket: bounded wait, then give up
    std::fs::remove_dir_all(home.join("journal")).unwrap();
    let holder = Command::new(env!("CARGO_BIN_EXE_oojurod"))
        .args(["tail", "--no-socket", "--poll-ms", "50"])
        .arg(home.join("nothing"))
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = Recorder { child: Some(holder) };
    let start = Instant::now();
    while !home.join("journal/LOCK").exists() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    let t0 = Instant::now();
    let h = hook_with(home, &[], "PostToolUse", &p.to_string());
    assert_eq!((h.code, h.stdout.as_str()), (0, ""));
    assert!(h.stderr.contains("another process"), "{}", h.stderr);
    assert!(t0.elapsed() < Duration::from_secs(10), "bounded: {:?}", t0.elapsed());
}

#[test]
fn hooks_print_shows_the_settings_entry_without_writing_anything() {
    let s = Scratch::new("hook-print");
    let home = &s.0;
    let o = Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .args(["--home"])
        .arg(home)
        .args(["hooks", "print"])
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(o.status.success());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let cmd = v["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.ends_with(&format!("--home {} hook UserPromptSubmit", home.display())), "{cmd}");
    assert_eq!(v["hooks"]["SessionStart"][0]["matcher"], "compact");
    for ev in ["SessionStart", "PreToolUse", "PostToolUse", "PostToolUseFailure", "Stop", "SubagentStop"] {
        assert_eq!(v["hooks"][ev][0]["hooks"][0]["async"], true, "{ev}");
    }
    assert_eq!(std::fs::read_dir(home).unwrap().count(), 0, "nothing written");
}

/// A Claude Code compaction: the boundary entry and the summary after it.
fn compaction(ts: &str, summary: &str) -> String {
    let b = json!({ "type": "system", "subtype": "compact_boundary", "content": "Conversation compacted", "timestamp": ts,
                    "cwd": "/w/demo", "compactMetadata": { "trigger": "auto", "preTokens": 150000 } });
    let s = json!({ "type": "user", "timestamp": ts, "cwd": "/w/demo", "isCompactSummary": true,
                    "message": { "role": "user", "content": summary } });
    format!("{b}\n{s}\n")
}

fn import(home: &Path, dirs: &[&Path]) {
    let o = Command::new(env!("CARGO_BIN_EXE_oojurod"))
        .args(["import", "--no-dedupe-stats"])
        .args(dirs)
        .env("OOJURO_HOME", home)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn id_of(home: &Path, text: &str) -> u64 {
    let rs = records(home);
    rs.iter().find(|r| r["text"].as_str().is_some_and(|t| t.contains(text))).unwrap()["id"].as_u64().unwrap()
}

#[test]
fn what_is_still_in_the_context_window_is_never_injected() {
    let s = Scratch::new("hctx");
    let home = &s.0;
    let (old_logs, _) = seed_history(home); // another, older session of the same agent (S0)
    let live = home.join(".claude/projects");
    let t = transcript(home, "s1"); // the prompt's session (S1)
    let s0 = old_logs.join(PROJECT).join("old.jsonl");
    let ts = "2026-02-01T10:00:00Z";
    for i in 0..3 {
        append(&t, &user(ts, "/w/demo", &format!("warm-up question {i} about llamas")));
    }
    append(&t, &tool_result(ts, "/w/demo", "p1", "the zebra migration ledger: alpha"));
    append(&s0, &tool_result(ts, "/w/demo", "q1", "the zebra migration ledger: bravo bravo zebra ledger"));
    import(home, &[&old_logs, &live]);
    set(home, "inject", "true");
    set(home, "min_bits", "0");
    set(home, "recency", "0");
    let q = "zebra migration ledger";
    let ask = |prompt: &str| {
        injected_ids(
            &hook(home, "UserPromptSubmit", &payload(&t, "UserPromptSubmit", json!({ "prompt": prompt }))).stdout,
        )
    };
    let (alpha, bravo) = (id_of(home, "ledger: alpha"), id_of(home, "ledger: bravo"));
    // never compacted: the whole session is in context, none of it is injected
    let ids = ask(q);
    assert!(ids.contains(&bravo) && !ids.contains(&alpha), "{ids:?}");
    // compacted: before the boundary is out of context (eligible), after it is not
    let ts2 = "2026-02-01T11:00:00Z";
    append(&t, &compaction(ts2, "Summary: the zebra migration ledger was discussed"));
    append(&t, &user(ts2, "/w/demo", "after compaction: zebra migration ledger charlie"));
    append(&s0, &compaction(ts2, "Summary of the old session"));
    append(&s0, &user(ts2, "/w/demo", "old session after its compaction: zebra migration ledger delta"));
    import(home, &[&old_logs, &live]);
    let (charlie, delta) = (id_of(home, "ledger charlie"), id_of(home, "ledger delta"));
    let ids = ask(q);
    assert!(ids.contains(&alpha), "pre-compaction records are eligible: {ids:?}");
    assert!(!ids.contains(&charlie), "post-compaction records are in context: {ids:?}");
    assert!(
        ids.contains(&delta) && ids.contains(&bravo),
        "other sessions are unaffected by this session's window: {ids:?}"
    );
    // recency: this session's pre-compaction records first, then other sessions
    let pos = |ids: &[u64], x: u64| ids.iter().position(|i| *i == x).unwrap();
    assert!(pos(&ids, bravo) < pos(&ids, alpha), "text bits alone: {ids:?}");
    set(home, "recency", "50");
    let ids = ask(q);
    assert_eq!(ids[0], alpha, "{ids:?}");
    assert!(!ids.contains(&charlie));
    // explicit search sees everything (no context to leave out)
    let hits = oojuro(home, &["search", q, "-k", "50"]);
    assert!(hits.as_array().unwrap().iter().any(|h| h["id"].as_u64() == Some(charlie)));
}

#[test]
fn a_compaction_reported_by_the_hook_opens_the_window_for_untailed_sessions() {
    let s = Scratch::new("hcmp");
    let home = &s.0;
    seed_history(home);
    set(home, "inject", "true");
    set(home, "min_bits", "0");
    let t = transcript(home, "s2"); // no tailer reads it: the hook records it
    let prompt =
        |p: &str| hook(home, "UserPromptSubmit", &payload(&t, "UserPromptSubmit", json!({ "prompt": p }))).stdout;
    prompt("the zebra migration ledger lives in hangar nine");
    std::thread::sleep(std::time::Duration::from_millis(2100)); // past the current-turn guard
    let earlier = id_of(home, "hangar nine");
    assert!(
        !injected_ids(&prompt("where is the zebra migration ledger hangar")).contains(&earlier),
        "still in context"
    );
    let h = hook(home, "SessionStart", &payload(&t, "SessionStart", json!({ "source": "compact" })));
    assert_eq!((h.stdout.as_str(), h.stderr.as_str()), ("", ""));
    let rs = records(home);
    assert!(rs.iter().any(|r| r["meta"]["sub"] == "compact_boundary" && r["meta"]["via"] == "hook"));
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert!(
        injected_ids(&prompt("which hangar has the zebra migration ledger")).contains(&earlier),
        "out of context now"
    );
    // other sources record nothing
    hook(home, "SessionStart", &payload(&t, "SessionStart", json!({ "source": "resume" })));
    assert_eq!(records(home).iter().filter(|r| r["meta"]["sub"] == "compact_boundary").count(), 1);
}
