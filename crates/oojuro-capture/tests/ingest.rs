//! Import, tail, partial lines and crash recovery on synthetic logs.
use oojuro_capture::{
    ingest::{Capture, Options},
    kind,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("oojuro-capture-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn append(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = OpenOptions::new().create(true).append(true).open(p).unwrap();
    f.write_all(s.as_bytes()).unwrap();
    f.sync_all().unwrap();
}

fn user(i: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-01-02T03:04:{:02}Z\",\"message\":{{\"role\":\"user\",\"content\":\"step {i}: run the tests\"}},\"origin\":{{\"kind\":\"human\"}}}}\n",
        i % 60
    )
}
fn call(i: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"2026-01-02T03:05:{:02}Z\",\"message\":{{\"model\":\"m\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"toolu_{i}\",\"name\":\"Bash\",\"input\":{{\"command\":\"cargo test --step {i}\"}}}}]}}}}\n",
        i % 60
    )
}
fn result(i: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-01-02T03:06:{:02}Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_{i}\",\"content\":\"ok {i}\",\"is_error\":false}}]}}}}\n",
        i % 60
    )
}
fn turn(i: u32) -> String {
    format!("{}{}{}", user(i), call(i), result(i))
}

fn opts(save_every: Duration) -> Options {
    Options { save_every, batch_bytes: 512, ..Options::default() }
}

#[test]
fn import_partial_lines_tail_and_restart_without_duplicates() {
    let s = Scratch::new("tail");
    let logs = s.0.join("logs/.claude/projects/-h-demo");
    let file = logs.join("sess-1.jsonl");
    let store = s.0.join("store");
    let dirs = vec![s.0.join("logs")];
    // two turns plus half of a third line
    let third = user(3);
    append(&file, &format!("{}{}", turn(1), turn(2)));
    append(&file, &third[..20]);
    {
        let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
        cap.poll(&dirs).unwrap();
        assert_eq!(cap.stats.records, 6);
        assert_eq!(cap.db.len(), 6);
        assert_eq!(cap.pending_tail_bytes(), 20, "the partial line waits");
        // the rest of the line arrives, then more
        append(&file, &third[20..]);
        append(&file, &format!("{}{}", call(3), result(3)));
        cap.poll(&dirs).unwrap();
        assert_eq!(cap.db.len(), 9);
        assert_eq!(cap.pending_tail_bytes(), 0);
        // the result links to its call and inherits its tool name
        let res = cap.db.get(9).unwrap().unwrap();
        assert_eq!(res.kind, kind::TOOL_RESULT);
        assert_eq!(res.parent, 8);
        assert_eq!(res.tool, "Bash");
        assert_eq!(res.text, "ok 3");
        let u = cap.db.get(7).unwrap().unwrap();
        assert_eq!((u.kind, u.turn), (kind::USER_INPUT, 3));
        assert!(u.args.contains("\"from\":\"human\""));
        cap.finish().unwrap();
    }
    // restart: nothing new, nothing duplicated
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    assert_eq!(cap.stats.recovered, 0);
    assert_eq!(cap.poll(&dirs).unwrap(), 0);
    assert_eq!(cap.db.len(), 9);
    // a new file of another agent shows up while following
    let codex = s.0.join("logs/codex/2026/01/02/rollout-2026-01-02T03-04-05-x.jsonl");
    append(&codex, "{\"timestamp\":\"2026-01-02T03:04:05Z\",\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/h/demo\",\"originator\":\"codex-tui\",\"source\":\"cli\",\"base_instructions\":{\"text\":\"sys\"}}}\n");
    append(&codex, "{\"timestamp\":\"2026-01-02T03:04:06Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n");
    cap.poll(&dirs).unwrap();
    assert_eq!(cap.db.len(), 11);
    assert_eq!(cap.state.writers.keys().cloned().collect::<Vec<_>>(), ["claude-code:-h-demo", "codex:-h-demo"]);
}

#[test]
fn crash_after_commit_before_state_save_neither_duplicates_nor_loses() {
    let s = Scratch::new("crash");
    let file = s.0.join("logs/.claude/projects/-h-demo/sess-1.jsonl");
    let store = s.0.join("store");
    let dirs = vec![s.0.join("logs")];
    append(&file, &(1..=5).map(turn).collect::<String>());
    {
        // never saves after commits: the state on disk still says offset 0
        let mut cap = Capture::open(&store, opts(Duration::from_secs(3600))).unwrap();
        cap.poll(&dirs).unwrap();
        assert_eq!(cap.db.len(), 15);
        assert!(cap.stats.commits > 1, "small batches: several commits");
        // dropped here without a final save = a crash after the appends
    }
    let saved = oojuro_capture::state::State::load(&store).unwrap();
    assert_eq!(saved.files.values().next().unwrap().offset, 0);
    assert_eq!(saved.next_id, 1);
    append(&file, &turn(6));
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    assert_eq!(cap.stats.recovered, 15);
    cap.poll(&dirs).unwrap();
    assert_eq!(cap.stats.skipped_existing, 15);
    assert_eq!(cap.stats.records, 3, "only the new turn");
    assert_eq!(cap.db.len(), 18);
    // ids stay dense, sequence and turns continue, call links survive the restart
    for id in 1..=18u64 {
        let e = cap.db.get(id).unwrap().unwrap();
        assert_eq!(e.seq as u64, id - 1);
        assert_eq!(e.turn as u64, (id - 1) / 3 + 1);
        if e.kind == kind::TOOL_RESULT {
            assert_eq!(e.parent, id - 1);
        }
    }
    cap.finish().unwrap();
    let saved = oojuro_capture::state::State::load(&store).unwrap();
    assert_eq!(saved.next_id, 19);
    assert_eq!(saved.files.values().next().unwrap().offset, fs::metadata(&file).unwrap().len());
}

#[test]
fn tail_follows_a_growing_file() {
    let s = Scratch::new("follow");
    let file = s.0.join("logs/.claude/projects/-h-demo/sess-1.jsonl");
    let store = s.0.join("store");
    let dirs = vec![s.0.join("logs")];
    append(&file, &turn(1));
    let writer = {
        let file = file.clone();
        std::thread::spawn(move || {
            for i in 2..=6 {
                std::thread::sleep(Duration::from_millis(60));
                let t = turn(i);
                // write each turn in two pieces, splitting a line
                append(&file, &t[..t.len() / 2]);
                std::thread::sleep(Duration::from_millis(20));
                append(&file, &t[t.len() / 2..]);
            }
        })
    };
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    let start = std::time::Instant::now();
    cap.tail(&dirs, Duration::from_millis(15), Duration::from_millis(100), |c, _| {
        c.db.len() >= 18 || start.elapsed() > Duration::from_secs(20)
    })
    .unwrap();
    writer.join().unwrap();
    assert_eq!(cap.db.len(), 18);
    assert_eq!(cap.stats.parse.bad_lines, 0, "split lines are never parsed half-written");
}

/// Withholds by a rule: tool calls naming `secret`, and every record of a
/// writer ending in `-off`.
struct TestPolicy;
impl oojuro_capture::ingest::Policy for TestPolicy {
    fn decide(&mut self, rec: &oojuro_capture::Rec, _source: &str) -> oojuro_capture::ingest::Decision {
        use oojuro_capture::ingest::Decision;
        if rec.writer.ends_with("-off") {
            return Decision::Withhold("record_off");
        }
        if rec.kind == kind::TOOL_CALL && rec.text.contains("secret") {
            return Decision::Withhold("excluded");
        }
        Decision::Store { indexed: rec.kind != kind::THINKING }
    }
}

#[test]
fn policy_withholds_records_and_the_results_of_withheld_calls() {
    let s = Scratch::new("policy");
    let file = s.0.join("logs/.claude/projects/-h-demo/sess-1.jsonl");
    let off = s.0.join("logs/.claude/projects/-h-x-off/sess-2.jsonl");
    let store = s.0.join("store");
    let dirs = vec![s.0.join("logs")];
    let secret_call = call(2).replace("cargo test --step 2", "cat secret.env");
    append(&file, &format!("{}{}{}{}", turn(1), user(2), secret_call, result(2)));
    append(&off, &turn(9));
    {
        let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
        cap.set_policy(Box::new(TestPolicy));
        cap.poll(&dirs).unwrap();
        // turn 1 (3 records) + user 2; the secret call and its result are withheld; the -off writer is never registered
        assert_eq!(cap.db.len(), 4);
        assert_eq!(cap.state.writers.keys().cloned().collect::<Vec<_>>(), ["claude-code:-h-demo"]);
        assert_eq!(cap.stats.withheld["excluded"].0, 1);
        assert_eq!(cap.stats.withheld["call_withheld"].0, 1);
        assert_eq!(cap.stats.withheld["record_off"].0, 3);
        assert_eq!((1..=4u64).map(|id| cap.db.get(id).unwrap().unwrap().kind).collect::<Vec<_>>(), [1, 4, 5, 1]);
        assert!(cap.db.get(5).unwrap().is_none(), "withheld records take no id");
        cap.finish().unwrap();
    }
    // a withheld call's result that arrives after a restart is withheld too
    let late = s.0.join("logs/.claude/projects/-h-demo/sess-3.jsonl");
    append(&late, &secret_call.replace("toolu_2", "toolu_7"));
    {
        let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
        cap.set_policy(Box::new(TestPolicy));
        cap.poll(&dirs).unwrap();
        cap.finish().unwrap();
    }
    append(&late, &result(7));
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    cap.set_policy(Box::new(TestPolicy));
    cap.poll(&dirs).unwrap();
    assert_eq!(cap.db.len(), 4);
    assert_eq!(cap.stats.withheld["call_withheld"].0, 1);
}

#[test]
fn explicit_records_share_the_id_space_and_survive_a_crash() {
    let s = Scratch::new("explicit");
    let file = s.0.join("logs/.claude/projects/-h-demo/sess-1.jsonl");
    let store = s.0.join("store");
    let dirs = vec![s.0.join("logs")];
    append(&file, &turn(1));
    {
        let mut cap = Capture::open(&store, opts(Duration::from_secs(3600))).unwrap();
        // one owner per store
        assert!(Capture::open(&store, opts(Duration::ZERO)).is_err(), "a second owner opened the store");
        cap.poll(&dirs).unwrap();
        let agent = cap.writer_store("human:test").unwrap();
        let id = cap.next_id();
        assert_eq!(id, 4);
        let ev = |id: u64, kind_: u8| oojuro_trace::EventIn {
            id,
            session: 77,
            parent_session: 0,
            agent,
            turn: 0,
            seq: 1,
            parent: 0,
            revises: 0,
            kind: kind_,
            status: 0,
            exit_code: None,
            tokens_in: 0,
            tokens_out: 0,
            ts_us: 5,
            duration_us: 0,
            tool: "",
            args: "{\"rt\":5}",
            text: "remember the zebra",
            indexed: true,
        };
        assert!(cap.append_explicit(&[ev(id + 1, kind::EXPLICIT)]).is_err(), "ids must be the next ones");
        assert!(cap.append_explicit(&[ev(id, kind::ASSISTANT)]).is_err(), "kind must be explicit");
        cap.append_explicit(&[ev(id, kind::EXPLICIT)]).unwrap();
        assert_eq!(cap.next_id(), 5);
        // crash: the state on disk still says next id 1
    }
    append(&file, &turn(2));
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    // registering the writer saved the state (next id 4): the explicit
    // record is the one past it, and it has no source position
    assert_eq!(cap.stats.recovered, 1);
    cap.poll(&dirs).unwrap();
    assert_eq!(cap.db.len(), 7);
    assert_eq!(cap.db.get(4).unwrap().unwrap().text, "remember the zebra");
    assert_eq!(cap.db.get(5).unwrap().unwrap().kind, kind::USER_INPUT);
    assert_eq!(cap.db.search(cap.state.writers["human:test"], "zebra", i64::MAX / 2, 5).unwrap(), vec![4]);
}

fn direct(writer: &str, k: u8, text: &str, call_id: &str) -> oojuro_capture::DirectRec {
    oojuro_capture::DirectRec {
        writer: writer.into(),
        ts_us: 1_000,
        kind: k,
        tool: if k == kind::TOOL_CALL { "Read".into() } else { String::new() },
        text: text.into(),
        call_id: call_id.into(),
        meta: [("model".to_string(), serde_json::json!("m-1"))].into_iter().collect(),
        tokens_in: 7,
        ..Default::default()
    }
}

#[test]
fn direct_records_policy_call_links_redaction_and_crash_recovery() {
    let s = Scratch::new("direct");
    let file = s.0.join("logs/.claude/projects/-h-demo/sess-1.jsonl");
    let store = s.0.join("store");
    let dirs = vec![s.0.join("logs")];
    append(&file, &turn(1));
    // built at run time so no key-shaped literal sits in the source
    let key = ["sk-ant-", "api03-Zq8vT3xLm2Pk9Rw4Yb7Nc1Hd6Jf0Gs5Ue2Ia8Ko3Lp7"].concat();
    {
        let mut cap = Capture::open(&store, opts(Duration::from_secs(3600))).unwrap();
        cap.set_policy(Box::new(TestPolicy));
        cap.poll(&dirs).unwrap();
        cap.save().unwrap(); // next id 4 on disk
        let w = "proxy:demo";
        let rep = cap
            .append_direct(
                "proxy",
                "proxy:anthropic.messages",
                vec![
                    direct(w, kind::USER_INPUT, &format!("my key {key}"), ""),
                    direct(w, kind::TOOL_CALL, "{\"path\":\"notes.txt\"}", "c1"),
                    direct(w, kind::TOOL_CALL, "{\"path\":\"secret.env\"}", "c2"),
                ],
            )
            .unwrap();
        assert_eq!((rep.stored, rep.withheld, rep.first_id), (2, 1, 4));
        // results arrive with the next call: linked to their calls; a withheld call's result is withheld
        let rep = cap
            .append_direct(
                "proxy",
                "proxy:anthropic.messages",
                vec![direct(w, kind::TOOL_RESULT, "notes", "c1"), direct(w, kind::TOOL_RESULT, "API_TOKEN=x", "c2")],
            )
            .unwrap();
        assert_eq!((rep.stored, rep.withheld), (1, 1));
        assert_eq!(cap.stats.withheld["call_withheld"].0, 1);
        let r = cap.db.get(6).unwrap().unwrap();
        assert_eq!((r.kind, r.parent, r.tool.as_str()), (kind::TOOL_RESULT, 5, "Read"));
        // a writer whose records are all withheld is not registered; mixed writers are refused
        cap.append_direct("proxy", "p", vec![direct("x-off", kind::USER_INPUT, "hi", "")]).unwrap();
        assert!(!cap.state.writers.contains_key("x-off"));
        let two = vec![direct("a", kind::USER_INPUT, "1", ""), direct("b", kind::USER_INPUT, "2", "")];
        assert!(cap.append_direct("proxy", "p", two).is_err());
        assert!(cap.append_direct("proxy", "p", vec![direct("a", kind::EXPLICIT, "1", "")]).is_err());
        assert_eq!(cap.next_id(), 7);
        // crash: the state on disk still says next id 4
    }
    append(&file, &turn(2));
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    assert_eq!(cap.stats.recovered, 3, "proxy records past the saved state are recovered");
    cap.poll(&dirs).unwrap();
    assert_eq!(cap.db.len(), 9);
    let u = cap.db.get(4).unwrap().unwrap();
    assert!(u.text.starts_with("my key [REDACTED:anthropic:"), "{}", u.text);
    assert_eq!(u.tokens_in, 7);
    let args: serde_json::Value = serde_json::from_str(&u.args).unwrap();
    assert_eq!((args["via"].as_str(), args["model"].as_str()), (Some("proxy"), Some("m-1")));
    assert!(args.get("src").is_none());
    assert_eq!(cap.db.get(7).unwrap().unwrap().kind, kind::USER_INPUT);
}

#[test]
fn direct_records_mark_oojuro_echo_as_stored_but_not_indexed() {
    let s = Scratch::new("direct-echo");
    let mut cap = Capture::open(&s.0.join("store"), opts(Duration::ZERO)).unwrap();
    let w = "proxy:demo";
    let mut lookup = direct(w, kind::TOOL_CALL, "{\"query\":\"zebra\"}", "c9");
    lookup.tool = "mcp__oojuro__search".into();
    let recs = vec![
        direct(w, kind::USER_INPUT, "zebra stripes on the whiteboard", ""),
        lookup,
        direct(w, kind::TOOL_RESULT, "zebra: 3 hits", "c9"),
        direct(w, kind::USER_INPUT, "<oojuro-context n=\"1\">\nzebra notes\n</oojuro-context>", ""),
    ];
    let rep = cap.append_direct("proxy", "proxy:anthropic.messages", recs).unwrap();
    assert_eq!(rep.stored, 4, "the echo is stored: the agent saw it");
    let marks: Vec<Option<String>> = (0..4)
        .map(|i| {
            let r = cap.db.get(rep.first_id + i).unwrap().unwrap();
            let a: serde_json::Value = serde_json::from_str(&r.args).unwrap();
            a["oojuro"].as_str().map(str::to_string)
        })
        .collect();
    assert_eq!(marks, [None, Some("call".into()), Some("result".into()), Some("context".into())]);
    let agent = cap.state.writers[w];
    let hits = cap.db.search(agent, "zebra", i64::MAX, 10).unwrap();
    assert_eq!(hits, vec![rep.first_id], "only the ordinary record is searchable");
}

#[test]
fn every_agent_instance_names_its_parent() {
    let s = Scratch::new("parents");
    let logs = s.0.join("logs");
    let main = logs.join(".claude/projects/-h-demo/sess-1.jsonl");
    let sub = logs.join(".claude/projects/-h-demo/sess-1/subagents/agent-a7.jsonl");
    append(&main, &turn(1));
    append(&sub, &turn(2));
    // Codex: the child rollout names its parent thread; the parent file sorts after it
    let day = logs.join(".codex/sessions/2026/01/02");
    let parent = day.join("rollout-2026-01-02T03-04-05-019a-parent.jsonl");
    let child = day.join("rollout-2026-01-02T03-04-04-019b-child.jsonl");
    let meta = |id: &str, source: &str| {
        format!(
            "{{\"timestamp\":\"2026-01-02T03:04:05Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/h/w\",\"originator\":\"codex-tui\",\"source\":{source},\"base_instructions\":{{\"text\":\"sys\"}}}}}}\n"
        )
    };
    let msg = "{\"timestamp\":\"2026-01-02T03:04:06Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"do it\"}]}}\n";
    append(&parent, &format!("{}{msg}", meta("019a-parent", "\"cli\"")));
    append(
        &child,
        &format!("{}{msg}", meta("019b-child", r#"{"subagent":{"thread_spawn":{"parent_thread_id":"019a-parent","depth":1}}}"#)),
    );
    let store = s.0.join("store");
    let mut cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    cap.poll(&[logs.clone()]).unwrap();
    let h = |p: &Path| oojuro_capture::hash64(&p.to_string_lossy());
    assert_eq!(cap.db.parent_of(h(&sub)), Some(h(&main)));
    assert_eq!(cap.db.parent_of(h(&main)), None);
    assert_eq!(cap.db.parent_of(h(&child)), Some(h(&parent)));
    assert_eq!(cap.db.parent_of(h(&parent)), None);
    // after a restart (the parent link is in every stored record of the instance)
    drop(cap);
    let cap = Capture::open(&store, opts(Duration::ZERO)).unwrap();
    assert_eq!(cap.db.parent_of(h(&sub)), Some(h(&main)));
    assert_eq!(cap.db.parent_of(h(&child)), Some(h(&parent)));
}
