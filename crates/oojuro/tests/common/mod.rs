#![allow(dead_code)]
//! Shared test helpers: private scratch homes and synthetic agent logs.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// A fresh directory under `$TMPDIR`, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
        let p = base.join(format!("oj-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    pub fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn append(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = OpenOptions::new().create(true).append(true).open(p).unwrap();
    f.write_all(s.as_bytes()).unwrap();
    f.sync_all().unwrap();
}

/// A Claude Code user prompt line.
pub fn user(ts: &str, cwd: &str, text: &str) -> String {
    let v = serde_json::json!({
        "type": "user", "timestamp": ts, "cwd": cwd,
        "message": { "role": "user", "content": text }, "origin": { "kind": "human" }
    });
    format!("{v}\n")
}

/// A Claude Code tool call line.
pub fn tool_call(ts: &str, cwd: &str, id: &str, tool: &str, input: serde_json::Value) -> String {
    let v = serde_json::json!({
        "type": "assistant", "timestamp": ts, "cwd": cwd,
        "message": { "model": "m", "content": [{ "type": "tool_use", "id": id, "name": tool, "input": input }] }
    });
    format!("{v}\n")
}

/// A Claude Code tool result line.
pub fn tool_result(ts: &str, cwd: &str, id: &str, text: &str) -> String {
    let v = serde_json::json!({
        "type": "user", "timestamp": ts, "cwd": cwd,
        "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": text, "is_error": false }] }
    });
    format!("{v}\n")
}

/// A Claude Code thinking + text answer line.
pub fn thinking(ts: &str, cwd: &str, thought: &str) -> String {
    let v = serde_json::json!({
        "type": "assistant", "timestamp": ts, "cwd": cwd,
        "message": { "model": "m", "content": [{ "type": "thinking", "thinking": thought, "signature": "x" }] }
    });
    format!("{v}\n")
}

/// Count of files under `dir` whose bytes contain `needle`.
pub fn grep_files(dir: &Path, needle: &[u8]) -> usize {
    let mut hits = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(data) = fs::read(&p) {
                hits += data.windows(needle.len()).any(|w| w == needle) as usize;
            }
        }
    }
    hits
}
