//! CLI round trips on a synthetic journal (no recorder running: every call
//! opens the journal directly and closes it).
mod common;
use common::Scratch;
use serde_json::Value;
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn oojuro(home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oojuro"));
    c.args(args)
        .current_dir(home)
        .env("OOJURO_HOME", home)
        .env("USER", "tester")
        .env_remove("OOJURO_WRITER")
        .env_remove("CLAUDECODE")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn ok_json(home: &Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let o = oojuro(home, &a, None);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&o.stdout)))
}

#[test]
fn cli_round_trips() {
    let s = Scratch::new("cli");
    let h = &s.0;
    // reads before anything exists: a clear error, nothing created
    let o = oojuro(h, &["search", "anything"], None);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no journal at"));
    assert!(!h.join("journal").exists());

    let a = ok_json(h, &["remember", "--key", "deploy", "deploys", "go", "through", "the", "orange", "gate"]);
    assert_eq!((a["writer"].as_str(), a["version"].as_u64()), (Some("human:tester"), Some(1)));
    let b = ok_json(
        h,
        &["remember", "--key", "deploy", "--writer", "human:tester", "deploys", "need", "a", "green", "build"],
    );
    assert_eq!(b["version"], 2);
    let o = oojuro(
        h,
        &["--json", "remember", "--key", "deploy", "-"],
        Some("deploys now use the release train\nsee runbook 7"),
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v3: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v3["version"], 3);
    let plain = ok_json(h, &["remember", "--writer", "agent:ci", "the flaky test is zebra_parser_roundtrip"]);
    assert_eq!(plain["writer"], "agent:ci");

    // search: newest version only, exact excerpt ranges
    let hits = ok_json(h, &["search", "deploys", "release", "train", "-k", "5"]);
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 1, "older versions are superseded: {hits:?}");
    assert_eq!(hits[0]["id"], v3["id"]);
    assert_eq!(hits[0]["key"], "deploy");
    let o = oojuro(h, &["search", "zebra_parser_roundtrip"], None);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("agent:ci") && out.contains("explicit"), "{out}");
    let hits = ok_json(h, &["search", "--writer", "human:tester", "zebra"]);
    assert!(hits.as_array().unwrap().is_empty());
    let hits = ok_json(h, &["search", "--kind", "explicit", "--excerpt-bytes", "12", "runbook"]);
    let e = &hits[0];
    let full = ok_json(h, &["get", &e["id"].to_string()]);
    let text = full["text"].as_str().unwrap();
    let (st, en) = (e["start"].as_u64().unwrap() as usize, e["end"].as_u64().unwrap() as usize);
    assert_eq!(&text[st..en], e["excerpt"].as_str().unwrap());
    assert!(e["excerpt"].as_str().unwrap().contains("runbook"));
    // a query on stdin
    let o = oojuro(h, &["--json", "search", "-"], Some("why is zebra_parser_roundtrip flaky"));
    assert!(o.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&o.stdout).unwrap()[0]["id"], plain["id"]);

    // get / history
    let g = ok_json(h, &["get", "--key", "deploy"]);
    assert_eq!(g["text"], "deploys now use the release train\nsee runbook 7");
    let g = ok_json(h, &["get", "--key", "deploy", "--as-of", a["ts"].as_str().unwrap()]);
    assert_eq!(g["id"], a["id"]);
    let hist = ok_json(h, &["history", "deploy"]);
    assert_eq!(hist.as_array().unwrap().iter().map(|x| x["seq"].as_u64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(oojuro(h, &["get", "999"], None).status.code(), Some(1));

    // range / export
    let all = ok_json(h, &["range"]);
    assert_eq!(all.as_array().unwrap().len(), 4);
    assert!(all.as_array().unwrap().iter().all(|r| r["agent_id"] == r["session"] && r.get("parent_id").is_none()));
    // --when on range, search and export, and periods
    assert_eq!(ok_json(h, &["range", "--when", "today"]).as_array().unwrap().len(), 4);
    assert!(ok_json(h, &["range", "--when", "yesterday"]).as_array().unwrap().is_empty());
    assert_eq!(ok_json(h, &["search", "zebra_parser_roundtrip", "--when", "past 2 hours"]).as_array().unwrap().len(), 1);
    let o = oojuro(h, &["range", "--when", "after \"zebra_parser_roundtrip\""], None);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stderr).contains("anchor"), "{o:?}");
    assert_eq!(oojuro(h, &["range", "--when", "purple elephant"], None).status.code(), Some(2));
    let p = ok_json(h, &["periods", "--by", "volume", "--per", "writer", "--when", "today"]);
    assert_eq!(p["periods"].as_array().unwrap().iter().map(|x| x["events"].as_u64().unwrap()).sum::<u64>(), 4);
    let o = oojuro(h, &["periods", "--by", "errors"], None);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("periods by errors"), "{o:?}");
    assert_eq!(oojuro(h, &["periods", "--by", "novelty"], None).status.code(), Some(2));
    assert_eq!(oojuro(h, &["config", "set", "timezone", "Mars/Olympus"], None).status.code(), Some(2));
    assert!(oojuro(h, &["config", "set", "timezone", "America/New_York"], None).status.success());
    assert_eq!(ok_json(h, &["range", "--when", "today"]).as_array().unwrap().len(), 4);
    let ci = ok_json(h, &["range", "--writer", "agent:ci"]);
    assert_eq!(ci.as_array().unwrap().len(), 1);
    let o = oojuro(h, &["export"], None);
    assert!(o.status.success());
    let lines: Vec<Value> =
        String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3]["text"], "the flaky test is zebra_parser_roundtrip");

    // stats
    let st = ok_json(h, &["stats"]);
    assert_eq!(st["records"], 4);
    assert_eq!(st["served_by"], "direct");
    assert_eq!(st["settings"]["inject"], false);

    // forget with proof (no terminal: no prompt)
    let f = ok_json(h, &["forget", "--key", "deploy"]);
    assert_eq!(f["forgotten"], 3);
    assert_eq!(f["proof"]["proven"], true);
    assert_eq!(common::grep_files(&h.join("journal"), b"orange gate"), 0);
    assert_eq!(oojuro(h, &["get", "--key", "deploy"], None).status.code(), Some(1));
    let o = oojuro(h, &["forget", "--id", "1", "--key", "x"], None);
    assert_eq!(o.status.code(), Some(2));

    // pause / resume / inject / config
    assert!(oojuro(h, &["pause"], None).status.success());
    assert_eq!(ok_json(h, &["config", "get", "paused"])["paused"]["value"], true);
    assert!(oojuro(h, &["resume"], None).status.success());
    assert_eq!(ok_json(h, &["config", "get", "paused"])["paused"]["value"], false);
    assert!(oojuro(h, &["inject", "on"], None).status.success());
    let g = ok_json(h, &["config", "get", "inject"]);
    assert_eq!(g["inject"]["value"], true);
    assert!(g["inject"]["source"].as_str().unwrap().ends_with("config.toml"));
    assert!(oojuro(h, &["config", "set", "top_k", "7", "--project"], None).status.success());
    assert!(h.join(".oojuro.toml").exists());
    assert!(oojuro(h, &["config", "set", "record", "off", "--agent", "codex:*"], None).status.success());
    let g = ok_json(h, &["config", "get"]);
    assert_eq!(g["top_k"]["value"], 7);
    assert!(g["top_k"]["source"].as_str().unwrap().ends_with(".oojuro.toml"));
    assert_eq!(ok_json(h, &["config", "get", "--agent", "codex:-x"])["record"]["value"], false);
    assert_eq!(ok_json(h, &["config", "get", "--agent", "human:tester"])["record"]["value"], true);
    assert_eq!(oojuro(h, &["config", "set", "top_k", "zero"], None).status.code(), Some(2));
    assert_eq!(oojuro(h, &["config", "set", "bogus", "1"], None).status.code(), Some(2));
    assert_eq!(oojuro(h, &["frobnicate"], None).status.code(), Some(2));
    let text = std::fs::read_to_string(h.join("config.toml")).unwrap();
    assert!(
        text.contains("paused = false") && text.contains("inject = true") && text.contains("[agents.\"codex:*\"]"),
        "{text}"
    );
}
