//! Rebuild a journal store from an export, with the recorder's store options,
//! to study its on-disk size (the index layout study).
//!
//! Usage:
//!   replay_export --export FILE.jsonl --dir NEW_DIR [--thinking index|store]
//!                 [--bulk N] [--kinds FILE] [--out FILE.jsonl] [--label L]
//!   replay_export --rec FILE.rec --dir NEW_DIR ...      (OJCORP01 corpus)
//!
//! `--export` reads `oojuro export` JSONL (any order): records are sorted by
//! id; writers become agent stores in order of first appearance (as the
//! recorder registers them); `args` is the compact JSON of `meta` (what the
//! recorder stored); `indexed = kind::indexed(kind) && !meta.oojuro` (the
//! recorder's default policy). Status and parent links are not in an export
//! and are left empty (columns only; the search index does not see them).
//! Records are appended in id order, one batch per run of consecutive
//! records of one writer and session (`--bulk N`: N such batches per
//! `append_many` call, one sync each; same rows per store).
//!
//! `--rec` reads an OJCORP01 corpus (crates/oojuro-store/src/corpus.rs) into
//! one agent store, one batch per (session, turn), ids 1.., a synthetic
//! clock of 1 ms per record; corpus kinds map to trace kinds (answer ->
//! assistant, doc -> user input).
//!
//! `--thinking store` stores thinking records without indexing them.
//! `--drop-meta k1,k2` leaves those keys out of `args` (e.g. `src,rt`, the
//! per-record provenance that makes every record's args unique, to measure
//! what record dedupe would do without it).
//! `--kinds FILE` writes one byte per event id (kind code; 255 = no event),
//! for `index_probe --kinds`. Prints `disk_breakdown()` after the load and a
//! flush (merges that ran during the load included), then after settling
//! merges, as one JSON line.
use oojuro::journal::store_options;
use oojuro_capture::kind;
use oojuro_trace::{EventIn, TraceDb};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    io::{BufRead, Read, Write},
    os::unix::fs::FileExt,
    path::PathBuf,
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn flag(v: &[String], name: &str) -> Option<String> {
    v.iter().position(|x| x == name).and_then(|i| v.get(i + 1).cloned())
}

#[derive(Deserialize)]
struct IdOnly {
    id: u64,
}

#[derive(Deserialize)]
struct Exported {
    id: u64,
    writer: String,
    kind: String,
    seq: u32,
    ts_us: i64,
    session: String,
    turn: u32,
    #[serde(default)]
    tool: String,
    #[serde(default)]
    meta: serde_json::Value,
    text: String,
}

/// One event, owned.
struct Ev {
    id: u64,
    agent: u32,
    session: u64,
    turn: u32,
    seq: u32,
    kind: u8,
    ts_us: i64,
    tool: String,
    args: String,
    text: String,
    indexed: bool,
}

fn event(e: &Ev) -> EventIn<'_> {
    EventIn {
        id: e.id,
        session: e.session,
        agent: e.agent,
        turn: e.turn,
        seq: e.seq,
        parent: 0,
        revises: 0,
        kind: e.kind,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: e.ts_us,
        duration_us: 0,
        tool: &e.tool,
        args: &e.args,
        text: &e.text,
        indexed: e.indexed,
    }
}

/// Events in append order, produced lazily (the export is ~1.3 GB).
trait Source {
    fn next(&mut self) -> Result<Option<Ev>>;
}

struct ExportSource {
    file: std::fs::File,
    order: Vec<(u64, u64, u32)>,
    at: usize,
    agents: HashMap<String, u32>,
    store_thinking: bool,
    drop_meta: Vec<String>,
    buf: Vec<u8>,
}

impl ExportSource {
    fn open(path: &PathBuf, store_thinking: bool, drop_meta: Vec<String>) -> Result<Self> {
        let f = std::fs::File::open(path)?;
        let mut r = std::io::BufReader::with_capacity(1 << 20, &f);
        let mut order = Vec::new();
        let mut off = 0u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = r.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            let id: IdOnly = serde_json::from_slice(&line)?;
            order.push((id.id, off, n as u32));
            off += n as u64;
        }
        order.sort_unstable();
        Ok(Self { file: std::fs::File::open(path)?, order, at: 0, agents: HashMap::new(), store_thinking, drop_meta, buf: Vec::new() })
    }
}

impl Source for ExportSource {
    fn next(&mut self) -> Result<Option<Ev>> {
        let Some(&(_, off, len)) = self.order.get(self.at) else { return Ok(None) };
        self.at += 1;
        self.buf.resize(len as usize, 0);
        self.file.read_exact_at(&mut self.buf, off)?;
        let mut r: Exported = serde_json::from_slice(&self.buf)?;
        if let serde_json::Value::Object(m) = &mut r.meta {
            for k in &self.drop_meta {
                m.remove(k);
            }
        }
        let n = self.agents.len() as u32;
        let agent = *self.agents.entry(r.writer).or_insert(n + 1);
        let k = kind::code(&r.kind).ok_or_else(|| format!("unknown kind {:?}", r.kind))?;
        let from_oojuro = r.meta.get("oojuro").is_some();
        let args = match &r.meta {
            serde_json::Value::Object(m) if m.is_empty() => String::new(),
            serde_json::Value::Null => String::new(),
            m => m.to_string(),
        };
        let indexed = kind::indexed(k) && !from_oojuro && !(self.store_thinking && k == kind::THINKING);
        Ok(Some(Ev {
            id: r.id,
            agent,
            session: u64::from_str_radix(&r.session, 16)?,
            turn: r.turn,
            seq: r.seq,
            kind: k,
            ts_us: r.ts_us,
            tool: r.tool,
            args,
            text: r.text,
            indexed,
        }))
    }
}

struct RecSource {
    data: Vec<u8>,
    at: usize,
    id: u64,
    seq: HashMap<u32, u32>,
    store_thinking: bool,
}

impl Source for RecSource {
    fn next(&mut self) -> Result<Option<Ev>> {
        let d = &self.data;
        if self.at >= d.len() {
            return Ok(None);
        }
        let at = self.at;
        let ck = d[at];
        let session = u32::from_le_bytes(d[at + 1..at + 5].try_into()?);
        let turn = u32::from_le_bytes(d[at + 5..at + 9].try_into()?);
        let len = u32::from_le_bytes(d[at + 9..at + 13].try_into()?) as usize;
        let text = String::from_utf8_lossy(&d[at + 13..at + 13 + len]).into_owned();
        self.at = at + 13 + len;
        // corpus kinds: system user thinking tool_call tool_result answer doc
        let k = [kind::SYSTEM, kind::USER_INPUT, kind::THINKING, kind::TOOL_CALL, kind::TOOL_RESULT, kind::ASSISTANT, kind::USER_INPUT]
            .get(ck as usize)
            .copied()
            .ok_or("unknown corpus kind")?;
        self.id += 1;
        let s = self.seq.entry(session).or_default();
        *s += 1;
        Ok(Some(Ev {
            id: self.id,
            agent: 1,
            session: u64::from(session),
            turn,
            seq: *s,
            kind: k,
            ts_us: 1_700_000_000_000_000 + self.id as i64 * 1000,
            tool: String::new(),
            args: String::new(),
            text,
            indexed: kind::indexed(k) && !(self.store_thinking && k == kind::THINKING),
        }))
    }
}

fn breakdown(db: &TraceDb) -> serde_json::Value {
    let b = db.disk_breakdown();
    let m: serde_json::Map<String, serde_json::Value> = b.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let mut v = serde_json::Value::Object(m);
    v["total"] = json!(b.iter().map(|x| x.1).sum::<u64>());
    v
}

fn main() -> Result<()> {
    let v: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(flag(&v, "--dir").ok_or("--dir")?);
    if dir.exists() {
        return Err(format!("{} exists", dir.display()).into());
    }
    let store_thinking = flag(&v, "--thinking").as_deref() == Some("store");
    let bulk: usize = flag(&v, "--bulk").map_or(Ok(0), |s| s.parse())?;
    let t0 = Instant::now();
    let (mut src, input): (Box<dyn Source>, String) = if let Some(p) = flag(&v, "--export") {
        let drop_meta = flag(&v, "--drop-meta").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
        (Box::new(ExportSource::open(&PathBuf::from(&p), store_thinking, drop_meta)?), p)
    } else if let Some(p) = flag(&v, "--rec") {
        let mut data = Vec::new();
        std::fs::File::open(&p)?.read_to_end(&mut data)?;
        if data.len() < 8 || &data[..8] != b"OJCORP01" {
            return Err("not an OJCORP01 corpus".into());
        }
        (Box::new(RecSource { data, at: 8, id: 0, seq: HashMap::new(), store_thinking }), p)
    } else {
        return Err("--export FILE or --rec FILE".into());
    };
    let opts = store_options().trace;
    let db = TraceDb::open(&dir, opts)?;
    let mut kinds: Vec<u8> = Vec::new();
    let (mut events, mut batches, mut text_bytes, mut indexed_bytes) = (0u64, 0u64, 0u64, 0u64);
    let mut by_kind = [(0u64, 0u64); 8];
    let mut pending: Vec<Vec<Ev>> = Vec::new();
    let mut cur: Vec<Ev> = Vec::new();
    let flush_batches = |pending: &mut Vec<Vec<Ev>>, force: bool| -> Result<()> {
        if pending.is_empty() || (!force && bulk > 0 && pending.len() < bulk) {
            return Ok(());
        }
        if bulk == 0 {
            for b in pending.iter() {
                let evs: Vec<EventIn> = b.iter().map(event).collect();
                db.append(&evs)?;
            }
        } else {
            let evs: Vec<Vec<EventIn>> = pending.iter().map(|b| b.iter().map(event).collect()).collect();
            db.append_many(&evs)?;
        }
        pending.clear();
        Ok(())
    };
    while let Some(e) = src.next()? {
        events += 1;
        text_bytes += e.text.len() as u64;
        if e.indexed {
            indexed_bytes += e.text.len() as u64;
        }
        let kc = (e.kind as usize).min(7);
        by_kind[kc].0 += 1;
        by_kind[kc].1 += e.text.len() as u64;
        let id = e.id as usize;
        if kinds.len() <= id {
            kinds.resize(id + 1, 255);
        }
        kinds[id] = e.kind;
        if cur.first().is_some_and(|f| f.agent != e.agent || f.session != e.session) {
            pending.push(std::mem::take(&mut cur));
            batches += 1;
            flush_batches(&mut pending, false)?;
        }
        cur.push(e);
    }
    if !cur.is_empty() {
        pending.push(cur);
        batches += 1;
    }
    flush_batches(&mut pending, true)?;
    let load_s = t0.elapsed().as_secs_f64();
    db.flush()?;
    let before = breakdown(&db);
    let t1 = Instant::now();
    let merges = db.settle()?;
    db.flush()?;
    let after = breakdown(&db);
    let settle_s = t1.elapsed().as_secs_f64();
    if let Some(k) = flag(&v, "--kinds") {
        std::fs::write(k, &kinds)?;
    }
    let names = ["system", "user_input", "thinking", "assistant", "tool_call", "tool_result", "explicit", "other"];
    let kinds_json: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .zip(by_kind.iter())
        .filter(|(_, c)| c.0 > 0)
        .map(|(n, c)| (n.to_string(), json!({"events": c.0, "text_bytes": c.1})))
        .collect();
    let (stored, dups) = db.dedupe_stats();
    let rec = json!({
        "label": flag(&v, "--label").unwrap_or_default(), "input": input, "dir": dir,
        "thinking": if store_thinking { "store" } else { "index" }, "bulk": bulk, "drop_meta": flag(&v, "--drop-meta"),
        "events": events, "batches": batches, "live_events": db.len(), "text_bytes": text_bytes,
        "indexed_text_bytes": indexed_bytes, "kinds": kinds_json,
        "stored_records": db.stored_records(), "dedupe": [stored, dups],
        "before_settle": before, "after_settle": after, "merges_at_settle": merges,
        "load_s": load_s, "settle_s": settle_s,
        "loadavg": std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim(),
    });
    println!("{rec}");
    if let Some(o) = flag(&v, "--out") {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(o)?;
        writeln!(f, "{rec}")?;
    }
    Ok(())
}
