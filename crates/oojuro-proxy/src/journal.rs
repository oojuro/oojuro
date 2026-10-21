//! The journal worker: one thread, fed by a bounded queue, never on the
//! agent's path. Per finished call it decodes and parses both bodies, keeps
//! the messages the writer's store does not hold yet ([`crate::seen`]),
//! masks secrets and writes them through the [`Sink`] as one batch.
//!
//! **Message identity** (what "stored once" means): a 128-bit hash of the
//! scope, kind, tool name, tool call id, the text (a tool call's input in
//! canonical JSON) and the occurrence number of that same content earlier
//! in the same call. Scope: `system` records (system prompt, tool
//! definitions, developer messages, harness context) per writer; every
//! other record per writer and conversation (`x-oojuro-session`, or the
//! session in Claude Code's `metadata.user_id`; without one, the writer).
//! So a history sent again matches itself, a repeated "yes" later in the
//! conversation is a new message, and a system prompt is stored once per
//! writer. Outputs get the identity they will have when the agent sends
//! them back in the next call's history, so they are stored once, from the
//! response (with model, usage and timing).
use crate::{
    Batch, C, DirectRec, Sink, VIA,
    decode::decode,
    extract::{self, Item, Route},
    forward::Shared,
    seen::Seen,
    tee::{Captured, End},
};
use oojuro_capture::{ParseCounts, hash64, kind, redact::Redactor, status};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    sync::{Arc, mpsc::Receiver},
};

/// One finished (or failed, or cut off) call on a recorded route.
pub struct Job {
    pub route: Route,
    pub writer: String,
    pub session: Option<String>,
    pub cwd: Option<String>,
    pub started_us: i64,
    pub ended_us: i64,
    pub req: Option<Captured>,
    pub req_encoding: Option<String>,
    /// `None`: no response from upstream.
    pub status: Option<u16>,
    pub resp: Option<Captured>,
    pub resp_type: String,
    pub resp_encoding: Option<String>,
    pub request_id: Option<String>,
    /// The proxy's own view of a failure (upstream unreachable).
    pub error: Option<String>,
}

impl Job {
    pub fn bytes(&self) -> usize {
        self.req.as_ref().map_or(0, |c| c.bytes) + self.resp.as_ref().map_or(0, |c| c.bytes)
    }
}

/// Error texts kept in the journal are cut here.
const MAX_ERROR_TEXT: usize = 64 << 10;

pub(crate) fn run(rx: Receiver<Job>, mut sink: Box<dyn Sink>, mut seen: Seen, shared: Arc<Shared>) {
    let red = Redactor::new();
    while let Ok(job) = rx.recv() {
        let n = job.bytes();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process(&job, &mut *sink, &mut seen, &red, &shared);
        }));
        if r.is_err() {
            shared.stats.inc(C::ParseErrors);
        }
        shared.release(n);
    }
    let _ = seen.flush();
}

/// 128-bit identity of `it` (see the module docs).
pub fn identity(scope: &[u8; 32], it: &Item, occurrence: u32) -> u128 {
    let mut h = blake3::Hasher::new();
    h.update(scope);
    h.update(&[it.kind]);
    h.update(it.tool.as_bytes());
    h.update(&[0]);
    h.update(it.call_id.as_bytes());
    h.update(&[0]);
    h.update(&occurrence.to_le_bytes());
    h.update(it.canon.as_deref().unwrap_or(&it.text).as_bytes());
    u128::from_le_bytes(h.finalize().as_bytes()[..16].try_into().unwrap())
}

fn scope(parts: &[&str]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(&[0]);
    }
    *h.finalize().as_bytes()
}

/// Identities and the seen-set for one call.
struct Admit<'a> {
    wscope: [u8; 32],
    cscope: [u8; 32],
    wkey: u64,
    occurrences: HashMap<u128, u32>,
    fresh: Vec<u128>,
    seq: u32,
    seen: &'a mut Seen,
    sh: &'a Shared,
}

impl Admit<'_> {
    /// The record's position in the call when it is new to the writer's store.
    fn admit(&mut self, it: &Item) -> Option<u32> {
        let sc = if it.kind == kind::SYSTEM { &self.wscope } else { &self.cscope };
        let n = self.occurrences.entry(identity(sc, it, u32::MAX)).or_insert(0);
        let id = identity(sc, it, *n);
        *n += 1;
        let seq = self.seq;
        self.seq += 1;
        if !self.seen.check_insert(self.wkey, id) {
            self.sh.stats.inc(C::RecordsDup);
            return None;
        }
        self.sh.stats.inc(C::RecordsNew);
        self.fresh.push(id);
        Some(seq)
    }
}

fn body(c: &Captured, enc: Option<&str>, max: usize) -> Result<Vec<u8>, String> {
    decode(c.concat(), enc, max)
}

fn error_text(status: Option<u16>, body: &str) -> String {
    let mut t = match status {
        Some(s) => format!("HTTP {s}: {body}"),
        None => body.to_string(),
    };
    if t.len() > MAX_ERROR_TEXT {
        let mut e = MAX_ERROR_TEXT;
        while !t.is_char_boundary(e) {
            e -= 1;
        }
        t.truncate(e);
        t.push_str(" [cut]");
    }
    t
}

fn process(job: &Job, sink: &mut dyn Sink, seen: &mut Seen, red: &Redactor, sh: &Shared) {
    let st = &sh.stats;
    let max = sh.max_decoded();
    let mut pc = ParseCounts::default();
    // what the agent sent: only a request body that arrived whole
    let req = match &job.req {
        Some(c) if c.truncated => {
            st.inc(C::DroppedTooLarge);
            None
        }
        Some(c) if c.end == End::Complete => {
            match body(c, job.req_encoding.as_deref(), max).and_then(|b| extract::request(job.route, &b, &mut pc)) {
                Ok(r) => Some(r),
                Err(_) => {
                    st.inc(C::ParseErrors);
                    None
                }
            }
        }
        _ => None,
    };
    let turn = req.as_ref().and_then(|r| r.items.last()).map_or(0, |i| i.turn);
    // what came back
    let mut out: Option<extract::Response> = None;
    let mut errors: Vec<(String, Option<u16>)> = Vec::new();
    if let Some(e) = &job.error {
        errors.push((e.clone(), None));
    }
    if let (Some(code), Some(c)) = (job.status, &job.resp) {
        if c.truncated {
            st.inc(C::DroppedTooLarge);
        } else if code >= 400 {
            let b = body(c, job.resp_encoding.as_deref(), max).unwrap_or_else(|e| e.into_bytes());
            errors.push((String::from_utf8_lossy(&b).into_owned(), Some(code)));
        } else if code < 300 {
            let sse = job.resp_type.starts_with("text/event-stream");
            match body(c, job.resp_encoding.as_deref(), max)
                .and_then(|b| extract::response(job.route, &b, sse, turn, &mut pc))
            {
                Ok(r) => {
                    for e in &r.errors {
                        errors.push((e.clone(), None));
                    }
                    out = Some(r);
                }
                Err(_) => st.inc(C::ParseErrors),
            }
        }
        if let End::Error(e) = &c.end {
            errors.push((format!("upstream stream broke off: {e}"), None));
        }
    }
    let cut = job.resp.as_ref().map(|c| c.end.clone());
    let writer = &job.writer;
    let session_key = job.session.clone().or_else(|| req.as_ref().and_then(|r| r.session.clone()));
    let wkey = hash64(writer);
    let model = out
        .as_ref()
        .map(|o| o.model.clone())
        .filter(|m| !m.is_empty())
        .or_else(|| req.as_ref().map(|r| r.model.clone()))
        .unwrap_or_default();
    let session = hash64(&format!("proxy\0{writer}\0{}", session_key.as_deref().unwrap_or("")));
    let source = format!("proxy:{}", job.route.name());

    // identities in conversation order: inputs, then outputs
    let mut base = Map::new();
    base.insert("api".into(), job.route.name().into());
    if !model.is_empty() {
        base.insert("model".into(), model.clone().into());
    }
    let mut a = Admit {
        wscope: scope(&["w", writer]),
        cscope: scope(&["c", writer, session_key.as_deref().unwrap_or("")]),
        wkey,
        occurrences: HashMap::new(),
        fresh: Vec::new(),
        seq: 0,
        seen,
        sh,
    };
    let rec = |it: &Item, ts: i64, seq: u32| DirectRec {
        writer: writer.clone(),
        ts_us: ts,
        kind: it.kind,
        tool: it.tool.clone(),
        text: it.text.clone(),
        meta: base
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .chain(it.meta.iter().map(|(k, v)| ((*k).to_string(), v.clone())))
            .collect(),
        status: it.status,
        session,
        seq,
        turn: it.turn,
        call_id: it.call_id.clone(),
        cwd: job.cwd.clone().unwrap_or_default(),
        ..DirectRec::default()
    };
    let mut recs: Vec<DirectRec> = Vec::new();
    if let Some(r) = &req {
        for it in &r.items {
            if let Some(seq) = a.admit(it) {
                recs.push(rec(it, job.started_us, seq));
            }
        }
    }
    let duration = job.ended_us - job.started_us;
    let usage = out.as_ref().map(|o| o.usage.clone()).unwrap_or_default();
    let usage_meta = |d: &mut DirectRec| {
        if !usage.is_empty() {
            d.meta.insert("usage".into(), Value::Object(usage.clone()));
        }
        d.tokens_in = usage.get("in").and_then(Value::as_u64).unwrap_or(0).min(u32::MAX as u64) as u32;
        d.tokens_out = usage.get("out").and_then(Value::as_u64).unwrap_or(0).min(u32::MAX as u64) as u32;
    };
    let mut first_out = true;
    if let Some(o) = &out {
        for it in &o.items {
            let Some(seq) = a.admit(it) else { continue };
            let mut d = rec(it, job.ended_us, seq);
            d.duration_us = duration;
            if let Some(rid) = &job.request_id {
                d.meta.insert("rid".into(), rid.clone().into());
            }
            if !o.stop.is_empty() {
                d.meta.insert("stop".into(), o.stop.clone().into());
            }
            match &cut {
                Some(End::Dropped) => {
                    d.meta.insert("partial".into(), "client_disconnect".into());
                }
                Some(End::Error(_)) => {
                    d.meta.insert("partial".into(), "upstream_error".into());
                }
                _ => {}
            }
            if first_out {
                usage_meta(&mut d);
                first_out = false;
            }
            recs.push(d);
        }
    }
    let first_error_seq = a.seq;
    let fresh = std::mem::take(&mut a.fresh);
    drop(a);
    // errors are events: always stored
    for (seq, (e, code)) in (first_error_seq..).zip(&errors) {
        let mut meta = base.clone();
        meta.insert("sub".into(), "upstream_error".into());
        if let Some(c) = code {
            meta.insert("http".into(), (*c).into());
        }
        if let Some(rid) = &job.request_id {
            meta.insert("rid".into(), rid.clone().into());
        }
        let mut d = DirectRec {
            writer: writer.clone(),
            ts_us: job.ended_us,
            kind: kind::SYSTEM,
            text: error_text(*code, e),
            meta: meta.into_iter().collect(),
            status: status::ERROR,
            session,
            seq,
            turn,
            duration_us: duration,
            cwd: job.cwd.clone().unwrap_or_default(),
            ..DirectRec::default()
        };
        if first_out {
            usage_meta(&mut d);
            first_out = false;
        }
        recs.push(d);
    }
    if recs.is_empty() {
        return;
    }
    // secrets are masked before anything leaves the proxy
    let mut rc = oojuro_capture::redact::Counts::new();
    for d in &mut recs {
        d.text = red.redact(&d.text, &mut rc);
    }
    st.add(C::Redactions, rc.values().sum());
    let n = recs.len() as u64;
    match sink.write(Batch { via: VIA.into(), source, records: recs }) {
        Ok(_) => {
            st.inc(C::BatchesWritten);
            st.add(C::RecordsWritten, n);
            let _ = seen.ack(wkey, &fresh);
        }
        Err(_) => {
            st.inc(C::DroppedJournal);
            st.add(C::RecordsDropped, n);
            seen.forget(wkey, &fresh); // stored when the agent sends them again
        }
    }
}
