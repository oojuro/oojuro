//! The recorder serving the journal while it writes (real processes):
//! concurrent reader processes and writer processes (an agent appending to
//! its log, CLI `remember` calls) never block and never see a torn record;
//! the recorder honors `record`, `pause`, `exclude` and the thinking
//! settings; stopping it hands the journal back to direct access.
mod common;
use common::{Scratch, append, thinking, tool_call, tool_result, user};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Recorder {
    child: Option<Child>,
    socket: PathBuf,
}

impl Recorder {
    fn start(home: &Path, logs: &Path) -> Self {
        let log = std::fs::File::create(home.join("daemon.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_oojurod"))
            .args(["tail", "--store"])
            .arg(home.join("journal"))
            .args(["--poll-ms", "50", "--no-dedupe-stats"])
            .arg(logs)
            .env("OOJURO_HOME", home)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let r = Recorder { child: Some(child), socket: home.join("oojurod.sock") };
        wait_until(Duration::from_secs(30), || oojuro::client::hello(&r.socket).is_some());
        r
    }

    /// SIGTERM, then wait: the recorder finishes its pass and removes the socket.
    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            // SAFETY: plain signal to our own child.
            unsafe {
                libc::kill(c.id() as i32, libc::SIGTERM);
            }
            let start = Instant::now();
            while c.try_wait().unwrap().is_none() {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = c.kill();
                    panic!("recorder did not stop on SIGTERM");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(c.wait().unwrap().success(), "recorder exit status");
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn wait_until(limit: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < limit, "timed out after {limit:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn oojuro(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .args(args)
        .current_dir(home)
        .env("OOJURO_HOME", home)
        .env("USER", "tester")
        .env_remove("OOJURO_WRITER")
        .env_remove("CLAUDECODE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(home: &Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let o = oojuro(home, &a);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// The texts the agent writes in turn `i` (prompt, tool input, tool output).
fn turn_texts(i: u32) -> (String, String, String) {
    let body: String = format!("payload{i} ").repeat((i as usize % 40 + 1) * 25);
    (
        format!("T{i} prompt: summarize payload batch {i}"),
        json!({ "command": format!("cat batch-{i}.txt") }).to_string(),
        format!("T{i} output begins\n{body}T{i} output ends"),
    )
}

#[test]
fn concurrent_readers_and_writers_never_block_or_tear() {
    const TURNS: u32 = 120;
    const KEYS: u32 = 3;
    const VERSIONS: u32 = 8;
    let s = Scratch::new("conc");
    let home = s.0.clone();
    let cwd = "/w/app";
    let file = home.join("logs/.claude/projects/-w-app/sess-1.jsonl");
    append(&file, &user("2026-10-21T09:00:00Z", cwd, "T0 start"));
    let mut rec = Recorder::start(&home, &home.join("logs"));

    // while the recorder runs, nothing else opens the journal
    let o = oojuro(&home, &["--direct", "stats"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("held by another process"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(json(&home, &["stats"])["served_by"].as_str().map(|x| x.starts_with("oojurod (pid")), Some(true));

    // every text that may legally be read back
    let mut legal: BTreeSet<String> = BTreeSet::from(["T0 start".to_string()]);
    for i in 1..=TURNS {
        let (a, b, c) = turn_texts(i);
        legal.extend([a, b, c]);
    }
    for k in 0..KEYS {
        for v in 1..=VERSIONS {
            legal.insert(format!("key k{k} version {v}: payload notes for k{k}"));
        }
    }
    for v in 1..=4 {
        legal.insert(format!("doomed version {v}: payload notes doomed-marker-{v}"));
    }
    let legal = Arc::new(legal);
    let done = Arc::new(AtomicBool::new(false));
    let worst = Arc::new(Mutex::new(Duration::ZERO));
    let calls = Arc::new(Mutex::new(0u32));

    // writer process 1: the agent appending to its transcript, in pieces
    let agent = {
        let file = file.clone();
        std::thread::spawn(move || {
            for i in 1..=TURNS {
                let ts = |s: u32| format!("2026-10-21T10:{:02}:{:02}Z", i / 60 % 60, (i + s) % 60);
                let (p, input, out) = turn_texts(i);
                let id = format!("toolu_{i}");
                let lines = format!(
                    "{}{}{}",
                    user(&ts(0), cwd, &p),
                    tool_call(&ts(1), cwd, &id, "Bash", serde_json::from_str(&input).unwrap()),
                    tool_result(&ts(2), cwd, &id, &out)
                );
                // split mid-line: the recorder must never read half a line
                let cut = lines.len() / 3;
                append(&file, &lines[..cut]);
                std::thread::sleep(Duration::from_millis(3));
                append(&file, &lines[cut..]);
                std::thread::sleep(Duration::from_millis(12));
            }
        })
    };
    // writer processes 2..: CLI remember (through the recorder, the single writer)
    let rememberers: Vec<_> = (0..KEYS)
        .map(|k| {
            let home = home.clone();
            std::thread::spawn(move || {
                for v in 1..=VERSIONS {
                    let text = format!("key k{k} version {v}: payload notes for k{k}");
                    let o = oojuro(&home, &["--json", "remember", "--key", &format!("k{k}"), &text]);
                    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
                    let r: Value = serde_json::from_slice(&o.stdout).unwrap();
                    assert_eq!(r["version"], v);
                }
            })
        })
        .collect();
    // reader processes: search, range, get, stats in loops
    let readers: Vec<_> = (0..4)
        .map(|n| {
            let (home, legal, done, worst, calls) =
                (home.clone(), legal.clone(), done.clone(), worst.clone(), calls.clone());
            std::thread::spawn(move || {
                let mut last_count = 0u64;
                let mut round = 0u32;
                while !done.load(Ordering::SeqCst) || round < 2 {
                    round += 1;
                    let t = Instant::now();
                    match (n + round) % 4 {
                        0 => {
                            let hits = json(
                                &home,
                                &["search", "payload", "batch", "notes", "-k", "40", "--excerpt-bytes", "65536"],
                            );
                            for h in hits.as_array().unwrap() {
                                let ex = h["excerpt"].as_str().unwrap();
                                assert!(legal.contains(ex), "torn or unknown search excerpt ({} bytes)", ex.len());
                                assert_eq!(h["start"], 0);
                            }
                        }
                        1 => {
                            for r in json(&home, &["range", "--limit", "100000"]).as_array().unwrap() {
                                let text = r["text"].as_str().unwrap();
                                assert!(
                                    legal.contains(text),
                                    "torn or unknown record #{} ({} bytes)",
                                    r["id"],
                                    text.len()
                                );
                            }
                        }
                        2 => {
                            let o = oojuro(&home, &["--json", "get", "--key", "k0"]);
                            if o.status.success() {
                                let r: Value = serde_json::from_slice(&o.stdout).unwrap();
                                assert!(legal.contains(r["text"].as_str().unwrap()));
                            } else {
                                assert_eq!(o.status.code(), Some(1), "{}", String::from_utf8_lossy(&o.stderr));
                            }
                        }
                        _ => {
                            // only the 4 forgotten records may ever leave the count
                            let c = json(&home, &["stats"])["records"].as_u64().unwrap();
                            assert!(c + 4 >= last_count, "record count went backwards: {last_count} -> {c}");
                            last_count = last_count.max(c);
                        }
                    }
                    let el = t.elapsed();
                    let mut w = worst.lock().unwrap();
                    *w = (*w).max(el);
                    *calls.lock().unwrap() += 1;
                }
            })
        })
        .collect();

    // a forget (reclaim + proof) through the recorder while readers and writers run
    let forgetter = {
        let home = home.clone();
        std::thread::spawn(move || {
            for v in 1..=4 {
                let text = format!("doomed version {v}: payload notes doomed-marker-{v}");
                assert!(oojuro(&home, &["remember", "--key", "doomed", &text]).status.success());
            }
            std::thread::sleep(Duration::from_millis(300));
            let f = json(&home, &["forget", "--key", "doomed"]);
            assert_eq!((f["forgotten"].as_u64(), f["proof"]["proven"].as_bool()), (Some(4), Some(true)), "{f}");
        })
    };
    agent.join().unwrap();
    for r in rememberers {
        r.join().unwrap();
    }
    forgetter.join().unwrap();
    let expected = 1 + 3 * TURNS as u64 + (KEYS * VERSIONS) as u64;
    wait_until(Duration::from_secs(60), || json(&home, &["stats"])["records"].as_u64() == Some(expected));
    done.store(true, Ordering::SeqCst);
    for r in readers {
        r.join().unwrap();
    }
    let worst = *worst.lock().unwrap();
    let calls = *calls.lock().unwrap();
    eprintln!("reader calls {calls}, slowest {worst:?}");
    assert!(calls >= 8);
    assert!(worst < Duration::from_secs(10), "a read blocked for {worst:?}");

    // exact contents through the recorder
    let all = json(&home, &["range", "--limit", "100000"]);
    let all = all.as_array().unwrap();
    assert_eq!(all.len() as u64, expected);
    let texts: BTreeSet<&str> = all.iter().map(|r| r["text"].as_str().unwrap()).collect();
    assert_eq!(texts.len(), legal.len() - 4, "every text exactly once, the forgotten ones gone");
    assert!(!texts.iter().any(|t| t.contains("doomed-marker")));
    for k in 0..KEYS {
        let h = json(&home, &["history", &format!("k{k}")]);
        assert_eq!(
            h.as_array().unwrap().iter().map(|x| x["seq"].as_u64().unwrap()).collect::<Vec<_>>(),
            (1..=VERSIONS as u64).collect::<Vec<_>>()
        );
        let g = json(&home, &["get", "--key", &format!("k{k}")]);
        assert_eq!(g["text"], format!("key k{k} version {VERSIONS}: payload notes for k{k}"));
    }
    let hits = json(&home, &["search", "payload", "notes", "--kind", "explicit", "-k", "50"]);
    assert_eq!(hits.as_array().unwrap().len() as u32, KEYS, "one (newest) version per key");
    // periods and --when are reads the recorder serves too
    let p = json(&home, &["periods", "--by", "volume", "--per", "writer", "--from", "2000-01-01", "-k", "50"]);
    let n: u64 = p["periods"].as_array().unwrap().iter().map(|x| x["events"].as_u64().unwrap()).sum();
    assert!(n > 0 && n <= expected, "{n} of {expected}");
    let w = json(&home, &["range", "--when", "since 2000-01-01", "--limit", "100000"]);
    assert_eq!(w.as_array().unwrap().len() as u64, expected);

    // a forget through the recorder, then stop it: direct access sees the same journal
    let f = json(&home, &["forget", "--key", "k1"]);
    assert_eq!((f["forgotten"].as_u64(), f["proof"]["proven"].as_bool()), (Some(VERSIONS as u64), Some(true)));
    rec.stop();
    assert!(!rec.socket.exists(), "socket removed on stop");
    let st = json(&home, &["stats"]);
    assert_eq!(st["served_by"], "direct");
    assert_eq!(st["records"].as_u64(), Some(expected - VERSIONS as u64));
    assert_eq!(common::grep_files(&home.join("journal"), b"version 3: payload notes for k1"), 0);
    for v in 1..=4 {
        assert_eq!(common::grep_files(&home.join("journal"), format!("doomed-marker-{v}").as_bytes()), 0);
    }
}

#[test]
fn recorder_honors_record_pause_exclude_and_thinking() {
    let s = Scratch::new("policy");
    let home = s.0.clone();
    let app = home.join("w/app");
    std::fs::create_dir_all(&app).unwrap();
    let cwd = app.to_str().unwrap().to_string();
    // global: *.env never recorded, the quiet project not at all, no thinking index for the app
    std::fs::write(
        home.join("config.toml"),
        "exclude = [\"*.env\"]\n\n[agents.\"claude-code:-w-quiet\"]\nrecord = false\n\n[agents.\"claude-code:-w-app\"]\nindex_thinking = false\n",
    )
    .unwrap();
    // project: its private/ directory is never recorded
    std::fs::write(app.join(".oojuro.toml"), "exclude = [\"private/**\"]\n").unwrap();
    let file = home.join("logs/.claude/projects/-w-app/sess-1.jsonl");
    let quiet = home.join("logs/.claude/projects/-w-quiet/sess-2.jsonl");
    let ts = "2026-10-21T10:00:00Z";
    append(&file, &user(ts, &cwd, "please check the config of the zebra service"));
    append(&file, &tool_call(ts, &cwd, "t1", "Read", json!({ "file_path": format!("{cwd}/.env") })));
    append(&file, &tool_result(ts, &cwd, "t1", "SERVICE_PASSWORD=hunter2-very-secret-value"));
    append(&file, &tool_call(ts, &cwd, "t2", "Read", json!({ "file_path": "private/diary.md" })));
    append(&file, &tool_result(ts, &cwd, "t2", "dear diary, zebra thoughts unique-diary-marker"));
    append(&file, &tool_call(ts, &cwd, "t3", "Read", json!({ "file_path": format!("{cwd}/src/main.rs") })));
    append(&file, &tool_result(ts, &cwd, "t3", "fn main() { zebra::run() }"));
    append(&file, &thinking(ts, &cwd, "pondering the zebra quietly unique-thought-marker"));
    append(&quiet, &user(ts, "/w/quiet", "quiet project secret plan unique-quiet-marker"));
    let mut rec = Recorder::start(&home, &home.join("logs"));
    wait_until(Duration::from_secs(30), || json(&home, &["stats"])["records"].as_u64() == Some(4));
    let all = json(&home, &["range"]);
    let texts: Vec<&str> = all.as_array().unwrap().iter().map(|r| r["text"].as_str().unwrap()).collect();
    assert_eq!(
        texts,
        vec![
            "please check the config of the zebra service",
            &json!({ "file_path": format!("{cwd}/src/main.rs") }).to_string(),
            "fn main() { zebra::run() }",
            "pondering the zebra quietly unique-thought-marker",
        ]
    );
    // thinking is stored but (index_thinking = false) not searchable
    let hits = json(&home, &["search", "unique-thought-marker"]);
    assert!(hits.as_array().unwrap().is_empty());
    assert_eq!(json(&home, &["search", "zebra", "service"])[0]["kind"], "user_input");

    // pause: what happens meanwhile is never recorded; explicit writes still work
    assert!(oojuro(&home, &["pause"]).status.success());
    std::thread::sleep(Duration::from_millis(200));
    append(&file, &user("2026-10-21T10:05:00Z", &cwd, "while paused unique-paused-marker"));
    assert!(oojuro(&home, &["remember", "explicit while paused"]).status.success());
    std::thread::sleep(Duration::from_millis(500));
    assert!(oojuro(&home, &["resume"]).status.success());
    std::thread::sleep(Duration::from_millis(200));
    append(&file, &user("2026-10-21T10:06:00Z", &cwd, "after resume"));
    wait_until(Duration::from_secs(30), || json(&home, &["stats"])["records"].as_u64() == Some(6));
    let st = json(&home, &["stats"]);
    let kinds: BTreeMap<String, u64> = serde_json::from_value(st["kinds"].clone()).unwrap();
    assert_eq!(kinds["user_input"], 2);
    assert_eq!(kinds["explicit"], 1);
    assert!(st["writers"].as_array().unwrap().iter().all(|w| w["writer"] != "claude-code:-w-quiet"));
    rec.stop();
    let log = std::fs::read_to_string(home.join("daemon.log")).unwrap();
    assert!(
        log.contains("excluded=2x")
            && log.contains("call_withheld=2x")
            && log.contains("paused=1x")
            && log.contains("record_off=1x"),
        "{log}"
    );
    for marker in ["hunter2-very-secret-value", "unique-diary-marker", "unique-quiet-marker", "unique-paused-marker"] {
        assert_eq!(common::grep_files(&home.join("journal"), marker.as_bytes()), 0, "{marker} was stored");
    }
}
