//! Records that come from no log file: Claude Code hooks (`oojuro hook`)
//! and the OpenTelemetry receiver (`docs/CAPTURE.md`).
//!
//! They are appended by the journal's single writer (the recorder, or the
//! calling process when none runs) the way the log tailer appends what it
//! parses: the same record kinds, the recording policy (`record`, pause,
//! `exclude`, thinking), redaction, and the marks for Oojuro's own echo
//! (calls to Oojuro, their results, injected context: stored, never
//! indexed). Each record names where it came from (`via`), which also tells
//! a restarting recorder that it has no source position.
//!
//! **Store once.** A record with a `dedupe` key is stored only the first
//! time its `(writer, key)` is seen: a tool call reported by both the
//! pre- and post-tool hooks, or a chat message an instrumented framework
//! sends again with the whole history on every call. The keys seen (64-bit
//! hashes, with the id of the record stored) are kept per journal in
//! `append-seen.bin`, bounded to the newest [`SEEN_CAP`]. Forgetting a
//! record does not drop its key, so a re-sent history never brings a
//! forgotten message back.
//!
//! The cache belongs to the open store (it lives in the journal's [`Reader`]
//! state and is loaded on the store's first append), so journals of
//! different stores never share it or wait for each other.
use crate::{Result, journal::Owner};
use oojuro_capture::{
    Rec, hash64,
    ingest::Decision,
    kind, marker,
    redact::{self},
    status,
    time::now_us,
};
use oojuro_trace::EventIn;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// One record from no log file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewRecord {
    pub writer: String,
    /// A capture kind: `user_input`, `assistant`, `thinking`, `tool_call`,
    /// `tool_result` or `system` (not `explicit`).
    pub kind: String,
    /// Valid time, µs since the epoch (0: now).
    #[serde(default)]
    pub ts_us: i64,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// Records with the same session key share a session; its hash is the
    /// session id (for Claude Code: the transcript path, as the log tailer
    /// hashes it).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// Session key of the agent instance that spawned this one, hashed like
    /// `session`. Empty: none, or (Claude Code) derived from a subagent
    /// transcript path in `session`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_session: String,
    /// Links a tool result to its call (stored as `call`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    /// `ok` or `error` (tool results).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// The agent's working directory (per-project settings).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// The path settings' `exclude` patterns match (e.g. the transcript).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Where it came from: `hook`, `otel`, ...
    pub via: String,
    /// Store once per `(writer, dedupe)`; empty: always store.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dedupe: String,
    /// MCP server or tool namespace of a tool call, when known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// More metadata (sender, model, token usage, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
}

/// What an append did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Appended {
    /// Ids of the records stored, in request order.
    pub ids: Vec<u64>,
    /// Skipped: stored before under the same dedupe key.
    pub deduped: usize,
    /// Not stored because of the settings (pause, `record = false`, `exclude`, ...).
    pub withheld: usize,
    /// Skipped: no text.
    pub empty: usize,
    /// Secrets masked before storing.
    pub redacted: u64,
}

/// Dedupe keys kept per journal (the newest; 16 bytes each on disk).
pub const SEEN_CAP: usize = 1 << 18;
pub const SEEN_FILE: &str = "append-seen.bin";

/// Dedupe keys seen, with the id of the record stored under each.
pub(crate) struct Seen {
    path: PathBuf,
    map: HashMap<u64, u64>,
    order: VecDeque<u64>,
}

impl Seen {
    fn load(store: &Path) -> Result<Self> {
        let path = store.join(SEEN_FILE);
        let mut s = Seen { path, map: HashMap::new(), order: VecDeque::new() };
        let bytes = match fs::read(&s.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(s),
            Err(e) => return Err(e.into()),
        };
        let n = bytes.len() / 16;
        for c in bytes[..n * 16].as_chunks::<16>().0 {
            let h = u64::from_le_bytes(c[..8].try_into().unwrap());
            let id = u64::from_le_bytes(c[8..].try_into().unwrap());
            s.insert(h, id);
        }
        if n > 2 * SEEN_CAP || bytes.len() % 16 != 0 {
            s.rewrite()?; // keep the file bounded (and drop a torn tail)
        }
        Ok(s)
    }

    fn insert(&mut self, h: u64, id: u64) {
        if self.map.insert(h, id).is_none() {
            self.order.push_back(h);
            while self.order.len() > SEEN_CAP {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    fn rewrite(&self) -> Result<()> {
        let mut buf = Vec::with_capacity(self.order.len() * 16);
        for h in &self.order {
            buf.extend_from_slice(&h.to_le_bytes());
            buf.extend_from_slice(&self.map[h].to_le_bytes());
        }
        let tmp = self.path.with_extension("bin.tmp");
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Add keys after their records are durable (a crash in between can
    /// only store a record twice, never lose one).
    fn add(&mut self, keys: &[(u64, u64)]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(keys.len() * 16);
        for (h, id) in keys {
            self.insert(*h, *id);
            buf.extend_from_slice(&h.to_le_bytes());
            buf.extend_from_slice(&id.to_le_bytes());
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(&buf)?;
        Ok(())
    }
}

/// One store's cache, shared by the clones of its journal's reader. The
/// store has one owning writer, so the lock is never contended; it only
/// lets the writer's `&Reader` reach the cache.
pub(crate) type SeenCache = Arc<Mutex<Option<Seen>>>;

/// Session id of a session key.
pub fn session_id(key: &str) -> u64 {
    hash64(key)
}

fn dedupe_hash(writer: &str, key: &str) -> u64 {
    hash64(&format!("oojuro:dedupe\0{writer}\0{key}"))
}

/// A record ready to append.
struct Ready {
    req: usize,
    agent: u32,
    rec: Rec,
    args: serde_json::Map<String, serde_json::Value>,
    indexed: bool,
    dedupe: Option<u64>,
    call_id: String,
}

impl Owner<'_> {
    /// Append records that come from no log file (see the module docs).
    pub fn append(&mut self, records: &[NewRecord]) -> Result<Appended> {
        self.cap.refresh_policy();
        let cache = Arc::clone(&self.reader.seen);
        let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(Seen::load(&self.cap.dir)?);
        }
        self.append_with(records, g.as_mut().unwrap())
    }

    fn append_with(&mut self, records: &[NewRecord], seen: &mut Seen) -> Result<Appended> {
        let mut out = Appended::default();
        let now = now_us();
        let mut rc = redact::Counts::new();
        let mut lookup_calls: HashSet<(&str, &str)> = HashSet::new();
        let mut withheld_calls: HashSet<(&str, &str)> = HashSet::new();
        // tool calls of this request, stored now or before: (writer, call id) -> id (0: not yet known)
        let mut call_ids: HashMap<(String, String), u64> = HashMap::new();
        let mut ready: Vec<Ready> = Vec::new();
        for (i, r) in records.iter().enumerate() {
            crate::paths::check_writer(&r.writer)?;
            let k = kind::code(&r.kind)
                .filter(|k| *k != kind::EXPLICIT)
                .ok_or_else(|| format!("record {i}: unknown kind {:?} (a capture kind, not explicit)", r.kind))?;
            if r.via.is_empty() || r.via.len() > 64 {
                return Err(format!("record {i}: `via` must name the source (1-64 bytes)").into());
            }
            if r.text.trim().is_empty() {
                out.empty += 1;
                continue;
            }
            let call = (r.writer.as_str(), r.call_id.as_str());
            let mut meta: Vec<(&'static str, String)> = Vec::new();
            if k == kind::TOOL_CALL && marker::is_oojuro_call(&r.tool, &r.namespace, &r.text) {
                marker::mark(&mut meta, marker::CALL);
                if !r.call_id.is_empty() {
                    lookup_calls.insert(call);
                }
            }
            if k == kind::TOOL_RESULT && !r.call_id.is_empty() && lookup_calls.contains(&call) {
                marker::mark(&mut meta, marker::RESULT);
            }
            if marker::has_context(&r.text) {
                marker::mark(&mut meta, marker::CONTEXT);
            }
            let session_key = if r.session.is_empty() { format!("{}:{}", r.via, r.writer) } else { r.session.clone() };
            let mut rec = Rec {
                writer: r.writer.clone(),
                ts_us: if r.ts_us == 0 { now } else { r.ts_us },
                kind: k,
                tool: r.tool.clone(),
                cwd: r.cwd.clone(),
                text: r.text.clone(),
                meta,
                status: match r.status.as_str() {
                    "ok" => status::OK,
                    "error" => status::ERROR,
                    _ => status::NONE,
                },
                session: session_id(&session_key),
                parent_session: if r.parent_session.is_empty() {
                    oojuro_capture::claude::parent_session(Path::new(&session_key))
                } else {
                    session_id(&r.parent_session)
                },
                call_id: r.call_id.clone(),
                ..Rec::default()
            };
            let mut d = self.cap.decide(&rec, &r.source);
            if k == kind::TOOL_RESULT && !r.call_id.is_empty() && withheld_calls.contains(&call) {
                d = Decision::Withhold("call_withheld");
            }
            let indexed = match d {
                Decision::Withhold(_) => {
                    if k == kind::TOOL_CALL && !r.call_id.is_empty() {
                        withheld_calls.insert(call);
                    }
                    out.withheld += 1;
                    continue;
                }
                Decision::Store { indexed } => indexed,
            };
            let dedupe = (!r.dedupe.is_empty()).then(|| dedupe_hash(&r.writer, &r.dedupe));
            if let Some(h) = dedupe
                && let Some(id) = seen.map.get(&h)
            {
                if k == kind::TOOL_CALL && !r.call_id.is_empty() {
                    call_ids.insert((r.writer.clone(), r.call_id.clone()), *id);
                }
                out.deduped += 1;
                continue;
            }
            // the same key twice in one request: once
            if let Some(h) = dedupe
                && ready.iter().any(|x| x.dedupe == Some(h))
            {
                out.deduped += 1;
                continue;
            }
            rec.text = self.cap.redactor().redact(&rec.text, &mut rc);
            let mut args = serde_json::Map::new();
            for (k, v) in &r.meta {
                args.insert(k.clone(), v.clone().into());
            }
            for (k, v) in &rec.meta {
                args.insert((*k).into(), v.clone().into());
            }
            args.insert("via".into(), r.via.clone().into());
            args.insert("rt".into(), now.into());
            if !r.call_id.is_empty() {
                args.insert("call".into(), r.call_id.clone().into());
                if k == kind::TOOL_CALL {
                    call_ids.entry((r.writer.clone(), r.call_id.clone())).or_insert(0);
                }
            }
            let agent = self.cap.writer_store(&r.writer)?;
            ready.push(Ready { req: i, agent, rec, args, indexed, dedupe, call_id: r.call_id.clone() });
        }
        self.sync_writers();
        // ids: grouped by writer store (one durable append each), request order within
        let mut order: Vec<usize> = (0..ready.len()).collect();
        order.sort_by_key(|&j| ready[j].agent);
        let mut ids = vec![0u64; ready.len()];
        for (id, &j) in (self.cap.next_id()..).zip(order.iter()) {
            ids[j] = id;
        }
        for (j, x) in ready.iter().enumerate() {
            if x.rec.kind == kind::TOOL_CALL && !x.call_id.is_empty() {
                call_ids.insert((x.rec.writer.clone(), x.call_id.clone()), ids[j]);
            }
        }
        // session position (seq, turn) after what the session holds
        let mut pos: HashMap<u64, (u32, u32)> = HashMap::new();
        let mut parents = vec![0u64; ready.len()];
        for (j, x) in ready.iter_mut().enumerate() {
            let p = pos.entry(x.rec.session).or_insert_with(|| {
                let info = self.cap.db.session_info(x.rec.session);
                let seq = info.iter().map(|e| e.seq + 1).max().unwrap_or(0);
                let turn = info.iter().map(|e| e.turn).max().unwrap_or(0);
                (seq, turn)
            });
            if x.rec.kind == kind::USER_INPUT {
                p.1 += 1;
            }
            x.rec.seq = p.0;
            x.rec.turn = p.1;
            p.0 += 1;
            if x.rec.kind == kind::TOOL_RESULT && !x.call_id.is_empty() {
                parents[j] = match call_ids.get(&(x.rec.writer.clone(), x.call_id.clone())) {
                    Some(id) if *id > 0 => *id,
                    _ => self.find_call(x.rec.session, x.agent, &x.call_id),
                };
            }
        }
        let args: Vec<String> = ready.iter().map(|x| serde_json::Value::Object(x.args.clone()).to_string()).collect();
        let mut start = 0;
        while start < order.len() {
            let agent = ready[order[start]].agent;
            let end = order[start..].iter().position(|&j| ready[j].agent != agent).map_or(order.len(), |p| start + p);
            let evs: Vec<EventIn> = order[start..end]
                .iter()
                .map(|&j| {
                    let x = &ready[j];
                    EventIn {
                        id: ids[j],
                        session: x.rec.session,
                        parent_session: x.rec.parent_session,
                        agent: x.agent,
                        turn: x.rec.turn,
                        seq: x.rec.seq,
                        parent: parents[j],
                        revises: 0,
                        kind: x.rec.kind,
                        status: x.rec.status,
                        exit_code: None,
                        tokens_in: 0,
                        tokens_out: 0,
                        ts_us: x.rec.ts_us,
                        duration_us: 0,
                        tool: &x.rec.tool,
                        args: &args[j],
                        text: &x.rec.text,
                        indexed: x.indexed,
                    }
                })
                .collect();
            self.cap.append_external(&evs)?;
            start = end;
        }
        if !ready.is_empty() {
            self.cap.save_if_dirty()?;
        }
        let keys: Vec<(u64, u64)> =
            ready.iter().enumerate().filter_map(|(j, x)| x.dedupe.map(|h| (h, ids[j]))).collect();
        seen.add(&keys)?;
        let mut by_req: Vec<(usize, u64)> = ready.iter().enumerate().map(|(j, x)| (x.req, ids[j])).collect();
        by_req.sort_unstable();
        out.ids = by_req.into_iter().map(|x| x.1).collect();
        out.redacted = rc.values().sum();
        Ok(out)
    }

    /// The id of the newest tool call with this call id in a session (or 0).
    fn find_call(&self, session: u64, agent: u32, call_id: &str) -> u64 {
        let mut calls: Vec<u64> = self
            .cap
            .db
            .session_info(session)
            .into_iter()
            .filter(|e| e.kind == kind::TOOL_CALL && e.agent == agent)
            .map(|e| e.id)
            .collect();
        calls.sort_unstable();
        let tail: Vec<u64> = calls.iter().rev().take(64).copied().collect();
        let Ok(evs) = self.cap.db.get_many(&tail) else { return 0 };
        for e in evs.into_iter().flatten() {
            let v: serde_json::Value = serde_json::from_str(&e.args).unwrap_or_default();
            if v.get("call").and_then(|c| c.as_str()) == Some(call_id) {
                return e.id;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use std::sync::mpsc;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
        base.join(format!("oj-seen-{name}-{}-{nanos}", std::process::id()))
    }

    fn one(text: &str, key: &str) -> NewRecord {
        NewRecord {
            writer: "w".into(),
            kind: "tool_result".into(),
            text: text.into(),
            via: "hook".into(),
            dedupe: key.into(),
            ..NewRecord::default()
        }
    }

    /// A store's append never touches another store's cache lock: with
    /// store A mid-append (its lock held) store B still completes.
    #[test]
    fn a_store_does_not_wait_for_another_stores_cache() {
        let (da, db) = (scratch("a"), scratch("b"));
        let ja = Journal::open(&da).unwrap();
        let mut jb = Journal::open(&db).unwrap();
        let held = Arc::clone(&ja.reader().seen);
        let _a_is_appending = held.lock().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                let out = jb.owner().append(&[one("hello", "k1")]).unwrap();
                tx.send(out.ids.len()).unwrap();
            });
            let n = rx.recv_timeout(Duration::from_secs(60)).expect("store B waited for store A's cache");
            assert_eq!(n, 1);
        });
        drop(_a_is_appending);
        drop(ja);
        drop(jb);
        let _ = fs::remove_dir_all(&da);
        let _ = fs::remove_dir_all(&db);
    }
}
