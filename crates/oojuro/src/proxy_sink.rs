//! Where the model API proxy's batches go (`docs/CAPTURE.md`, "Model API
//! proxy"): the recorder's own write loop when the proxy runs inside
//! `oojurod tail` ([`OwnerSink`]), or a running recorder's socket when it
//! runs as its own process ([`SocketSink`]). Either way the store's single
//! writer appends them, applying the settings and redacting again.
//!
//! Which agents the proxy leaves to the transcript recorder (the transcript
//! recorder wins, design decision 2026-10-21): [`tailed_formats`] for the
//! directories a recorder follows, [`tailed_in_hello`] for a running one.
use crate::{client, proto::Request, server::WriteJob};
use oojuro_capture::Format;
use oojuro_proxy::{Batch, Sink};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

/// Agents whose transcripts a recorder following `dirs` reads: a format
/// whose transcript root is at, below or above one of the directories
/// ([`Format::tailed_from`]), or of a transcript already registered under
/// one of them (`files`: path and format).
pub fn tailed_formats<'a>(dirs: &[PathBuf], files: impl IntoIterator<Item = (&'a str, Format)>) -> Vec<Format> {
    let mut out: Vec<Format> = Format::ALL.into_iter().filter(|f| dirs.iter().any(|d| f.tailed_from(d))).collect();
    for (path, f) in files {
        if !out.contains(&f) && dirs.iter().any(|d| Path::new(path).starts_with(d)) {
            out.push(f);
        }
    }
    out.sort_by_key(|f| Format::ALL.iter().position(|x| x == f));
    out
}

/// The agents a running recorder tails, from its hello: its `tailed`
/// list, else (an older recorder) derived from its `watch` directories.
pub fn tailed_in_hello(h: &Value) -> Vec<Format> {
    if let Some(names) = h.get("tailed").and_then(Value::as_array) {
        return names.iter().filter_map(Value::as_str).filter_map(Format::from_name).collect();
    }
    let dirs: Vec<PathBuf> = h
        .get("watch")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .collect();
    tailed_formats(&dirs, [])
}

/// Format names (`claude-code`, `codex`).
pub fn names(formats: &[Format]) -> Vec<&'static str> {
    formats.iter().map(|f| f.name()).collect()
}

/// How long the proxy's journal worker waits for the store owner. While it
/// waits, new calls queue up (bounded) and then drop; the agent never waits.
pub const SINK_TIMEOUT: Duration = Duration::from_secs(300);

fn request(b: Batch) -> Request {
    Request::Ingest { via: b.via, source: b.source, records: b.records }
}

fn stored(v: &serde_json::Value) -> u64 {
    v["stored"].as_u64().unwrap_or(0)
}

/// Batches into the recorder's own write loop (same process).
pub struct OwnerSink {
    pub writes: mpsc::Sender<WriteJob>,
    pub timeout: Duration,
}

impl Sink for OwnerSink {
    fn write(&mut self, b: Batch) -> Result<u64, String> {
        let (tx, rx) = mpsc::channel();
        self.writes.send(WriteJob { req: request(b), reply: tx }).map_err(|_| "the recorder stopped".to_string())?;
        match rx.recv_timeout(self.timeout) {
            Ok(Ok(v)) => Ok(stored(&v)),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the recorder did not answer in time".into()),
        }
    }
}

/// Batches to a recorder serving the journal on its socket.
pub struct SocketSink {
    pub socket: PathBuf,
}

impl Sink for SocketSink {
    fn write(&mut self, b: Batch) -> Result<u64, String> {
        let v = client::call(&self.socket, &request(b), SINK_TIMEOUT).map_err(|e| e.to_string())?;
        Ok(stored(&v))
    }
}
