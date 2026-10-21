//! Embedded key-value arms (`redb`, `fjall`, `rocksdb`): one trace layout
//! over a small engine trait, so the three engines store the same keys and
//! answer every query with the same plan. Spec and settings:
//! `docs/TRACEBENCH_BASELINES.md` ("Embedded key-value stores").
//!
//! Keyspaces (a redb table, a fjall keyspace, a RocksDB column family each);
//! integers are big-endian so byte order is numeric order, `ts` is the i64
//! with its sign bit flipped:
//!
//! | keyspace | key | value |
//! |---|---|---|
//! | `meta`   | id | fixed fields + latest status + text length + tool |
//! | `body`   | id | args length, args, text |
//! | `sess`   | session, seq, id | - |
//! | `time`   | ts, id | - |
//! | `kind`   | kind, ts, id | - |
//! | `agent`  | agent, ts, id | - |
//! | `tool`   | tool length (u16), tool, ts, id | - |
//! | `status` | latest status, ts, id | - |
//!
//! Every secondary key ends in (ts, id), so a range of one index value is
//! already in the (ts, id) order `filter` and `time_range` return. A status
//! revision is a read-modify-write of the call's `meta` (status, exit code,
//! duration) and moves its `status` key, in the same atomic batch as the rest
//! of the turn; `body` is never rewritten. Concurrent revisions of the same
//! call serialise on a striped lock (the engines' own commit paths run
//! concurrently otherwise, so RocksDB's group commit still applies).
//!
//! No full-text search: `search` returns [`crate::Unsupported`].
use crate::events::{Event, ST_ERROR, ST_NONE, TOOL_CALL};
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ks {
    Meta = 0,
    Body = 1,
    Sess = 2,
    Time = 3,
    Kind = 4,
    Agent = 5,
    Tool = 6,
    Status = 7,
}

pub(crate) const KEYSPACES: [&str; 8] = ["meta", "body", "sess", "time", "kind", "agent", "tool", "status"];

/// One write of an atomic batch.
pub(crate) enum Op {
    Put(Ks, Vec<u8>, Vec<u8>),
    Del(Ks, Vec<u8>),
}

/// A consistent read view (a redb read transaction, a fjall or RocksDB
/// snapshot).
pub(crate) trait Reader {
    /// Calls `f` with the value of `key`; false if there is none.
    fn get(&self, ks: Ks, key: &[u8], f: &mut dyn FnMut(&[u8])) -> Result<bool>;
    /// Calls `f` with every (key, value) in `[from, to)` in key order, until
    /// it returns false.
    fn scan(&self, ks: Ks, from: &[u8], to: &[u8], f: &mut dyn FnMut(&[u8], &[u8]) -> bool) -> Result<()>;
}

/// What an engine provides; everything else is shared.
pub(crate) trait Engine: Send + Sync + 'static {
    fn name(&self) -> String;
    /// Run `f` on one consistent read view.
    fn read(&self, f: &mut dyn FnMut(&dyn Reader) -> Result<()>) -> Result<()>;
    /// Apply `ops` atomically; durable on return when `sync` (one durable
    /// commit), else durable after the next `settle`.
    fn write(&self, ops: Vec<Op>, sync: bool) -> Result<()>;
    /// End of a bulk load: everything written so far durable.
    fn settle(&self) -> Result<()>;
    /// Make deletes physical: compact so no live file holds deleted data.
    fn compact(&self) -> Result<()>;
    /// The store directory (every file of the store is under it).
    fn dir(&self) -> &Path;
    /// Engine-specific numbers recorded after a load (disk per keyspace).
    fn details(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

// ---------------------------------------------------------------- encoding

const META_FIXED: usize = 8 + 4 + 4 + 4 + 8 + 1 + 1 + 1 + 4 + 4 + 4 + 8 + 8 + 4;

fn ts_key(ts: i64) -> [u8; 8] {
    ((ts as u64) ^ (1 << 63)).to_be_bytes()
}

#[cfg(test)]
fn ts_of(k: &[u8]) -> i64 {
    (u64::from_be_bytes(k.try_into().unwrap()) ^ (1 << 63)) as i64
}

fn id_key(id: u64) -> [u8; 8] {
    id.to_be_bytes()
}

fn u64_at(b: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(b[p..p + 8].try_into().unwrap())
}
fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}

/// The fixed fields of a row (everything but tool, args and text).
#[derive(Clone, Debug)]
pub(crate) struct Meta {
    session: u64,
    agent: u32,
    turn: u32,
    seq: u32,
    parent: u64,
    kind: u8,
    status: u8,
    exit_code: Option<i32>,
    tokens_in: u32,
    tokens_out: u32,
    ts_us: i64,
    duration_us: i64,
    text_len: u32,
    tool: String,
}

impl Meta {
    fn of(e: &Event) -> Self {
        Self {
            session: e.session,
            agent: e.agent,
            turn: e.turn,
            seq: e.seq,
            parent: e.parent,
            kind: e.kind,
            status: e.status,
            exit_code: e.exit_code,
            tokens_in: e.tokens_in,
            tokens_out: e.tokens_out,
            ts_us: e.ts_us,
            duration_us: e.duration_us,
            text_len: e.text.len() as u32,
            tool: e.tool.to_string(),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(META_FIXED + self.tool.len());
        v.extend_from_slice(&self.session.to_be_bytes());
        v.extend_from_slice(&self.agent.to_be_bytes());
        v.extend_from_slice(&self.turn.to_be_bytes());
        v.extend_from_slice(&self.seq.to_be_bytes());
        v.extend_from_slice(&self.parent.to_be_bytes());
        v.push(self.kind);
        v.push(self.status);
        v.push(self.exit_code.is_some() as u8);
        v.extend_from_slice(&self.exit_code.unwrap_or(0).to_be_bytes());
        v.extend_from_slice(&self.tokens_in.to_be_bytes());
        v.extend_from_slice(&self.tokens_out.to_be_bytes());
        v.extend_from_slice(&self.ts_us.to_be_bytes());
        v.extend_from_slice(&self.duration_us.to_be_bytes());
        v.extend_from_slice(&self.text_len.to_be_bytes());
        v.extend_from_slice(self.tool.as_bytes());
        v
    }

    fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < META_FIXED {
            return Err("kv: short meta value".into());
        }
        Ok(Self {
            session: u64_at(b, 0),
            agent: u32_at(b, 8),
            turn: u32_at(b, 12),
            seq: u32_at(b, 16),
            parent: u64_at(b, 20),
            kind: b[28],
            status: b[29],
            exit_code: (b[30] != 0).then(|| i32::from_be_bytes(b[31..35].try_into().unwrap())),
            tokens_in: u32_at(b, 35),
            tokens_out: u32_at(b, 39),
            ts_us: u64_at(b, 43) as i64,
            duration_us: u64_at(b, 51) as i64,
            text_len: u32_at(b, 59),
            tool: String::from_utf8(b[META_FIXED..].to_vec())?,
        })
    }

    /// The secondary keys of a row with this meta.
    fn index_keys(&self, id: u64) -> [(Ks, Vec<u8>); 6] {
        let (ts, idk) = (ts_key(self.ts_us), id_key(id));
        let tail = |head: &[u8]| -> Vec<u8> {
            let mut k = Vec::with_capacity(head.len() + 16);
            k.extend_from_slice(head);
            k.extend_from_slice(&ts);
            k.extend_from_slice(&idk);
            k
        };
        let mut sess = Vec::with_capacity(20);
        sess.extend_from_slice(&self.session.to_be_bytes());
        sess.extend_from_slice(&self.seq.to_be_bytes());
        sess.extend_from_slice(&idk);
        [
            (Ks::Sess, sess),
            (Ks::Time, tail(&[])),
            (Ks::Kind, tail(&[self.kind])),
            (Ks::Agent, tail(&self.agent.to_be_bytes())),
            (Ks::Tool, tail(&tool_head(&self.tool))),
            (Ks::Status, status_key(self.status, self.ts_us, id)),
        ]
    }

    fn row(self, id: u64, args: String, text: String) -> Row {
        Row {
            id,
            session: self.session,
            agent: self.agent,
            turn: self.turn,
            seq: self.seq,
            parent: self.parent,
            kind: self.kind,
            status: self.status,
            exit_code: self.exit_code,
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            ts_us: self.ts_us,
            duration_us: self.duration_us,
            tool: self.tool,
            args,
            text,
        }
    }
}

fn tool_head(tool: &str) -> Vec<u8> {
    let t = tool.as_bytes();
    let mut h = Vec::with_capacity(2 + t.len());
    h.extend_from_slice(&(t.len().min(u16::MAX as usize) as u16).to_be_bytes());
    h.extend_from_slice(&t[..t.len().min(u16::MAX as usize)]);
    h
}

fn status_key(status: u8, ts: i64, id: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(17);
    k.push(status);
    k.extend_from_slice(&ts_key(ts));
    k.extend_from_slice(&id_key(id));
    k
}

fn encode_body(args: &str, text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + args.len() + text.len());
    v.extend_from_slice(&(args.len() as u32).to_be_bytes());
    v.extend_from_slice(args.as_bytes());
    v.extend_from_slice(text.as_bytes());
    v
}

/// (args, text) byte ranges inside a body value.
fn body_parts(b: &[u8]) -> Result<(&[u8], &[u8])> {
    if b.len() < 4 {
        return Err("kv: short body value".into());
    }
    let al = u32_at(b, 0) as usize;
    if b.len() < 4 + al {
        return Err("kv: body shorter than its args".into());
    }
    Ok((&b[4..4 + al], &b[4 + al..]))
}

/// The id at the end of a secondary key.
fn tail_id(k: &[u8]) -> u64 {
    u64_at(k, k.len() - 8)
}

/// The ts before the id at the end of a (.., ts, id) key.
#[cfg(test)]
fn tail_ts(k: &[u8]) -> i64 {
    ts_of(&k[k.len() - 16..k.len() - 8])
}

/// `head` + ts + id 0: the first key of `head` at or after `ts`.
fn bound(head: &[u8], ts: i64) -> Vec<u8> {
    let mut k = Vec::with_capacity(head.len() + 16);
    k.extend_from_slice(head);
    k.extend_from_slice(&ts_key(ts));
    k.extend_from_slice(&[0; 8]);
    k
}

/// Every key starting with `p`: `[p, p + 1)` (p is never all 0xff here).
fn prefix_range(p: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut end = p.to_vec();
    while let Some(last) = end.last_mut() {
        if *last == 0xff {
            end.pop();
        } else {
            *last += 1;
            break;
        }
    }
    (p.to_vec(), end)
}

// ---------------------------------------------------------------- reads on a view

fn get_meta(r: &dyn Reader, id: u64) -> Result<Option<Meta>> {
    let mut m = None;
    let mut err = None;
    r.get(Ks::Meta, &id_key(id), &mut |v| match Meta::decode(v) {
        Ok(x) => m = Some(x),
        Err(e) => err = Some(e),
    })?;
    match err {
        Some(e) => Err(e),
        None => Ok(m),
    }
}

fn get_row(r: &dyn Reader, id: u64) -> Result<Option<Row>> {
    let Some(m) = get_meta(r, id)? else { return Ok(None) };
    let mut parts: Option<Result<(String, String)>> = None;
    r.get(Ks::Body, &id_key(id), &mut |v| {
        parts = Some(body_parts(v).and_then(|(a, t)| {
            Ok((String::from_utf8(a.to_vec())?, String::from_utf8(t.to_vec())?))
        }));
    })?;
    match parts {
        Some(p) => {
            let (args, text) = p?;
            Ok(Some(m.row(id, args, text)))
        }
        None => Err(format!("kv: row {id} has meta but no body").into()),
    }
}

/// Ids in an index range, in key order, at most `limit`.
fn ids_in(r: &dyn Reader, ks: Ks, from: &[u8], to: &[u8], limit: usize) -> Result<Vec<u64>> {
    let mut v = Vec::new();
    if limit == 0 {
        return Ok(v);
    }
    r.scan(ks, from, to, &mut |k, _| {
        v.push(tail_id(k));
        v.len() < limit
    })?;
    Ok(v)
}

fn session_ids(r: &dyn Reader, session: u64) -> Result<Vec<u64>> {
    let (a, b) = prefix_range(&session.to_be_bytes());
    ids_in(r, Ks::Sess, &a, &b, usize::MAX)
}

/// The index range a filter scans: (keyspace, from, to, conditions the
/// range itself guarantees: agent, kind, tool, status).
struct Plan {
    ks: Ks,
    from: Vec<u8>,
    to: Vec<u8>,
    covers: [bool; 4],
}

/// Pick the narrowest index a filter can use: tool, then agent, then a
/// call status (pending/ok/error), then kind, then the status `none`, then
/// time alone. Each is a contiguous (ts, id)-ordered range.
fn plan(f: &Filter) -> Plan {
    let (from, to) = (f.from_us, f.to_us.max(f.from_us));
    let mk = |ks: Ks, head: Vec<u8>, i: usize| {
        let mut covers = [false; 4];
        if i < 4 {
            covers[i] = true;
        }
        Plan { ks, from: bound(&head, from), to: bound(&head, to), covers }
    };
    if let Some(t) = &f.tool {
        mk(Ks::Tool, tool_head(t), 2)
    } else if let Some(a) = f.agent {
        mk(Ks::Agent, a.to_be_bytes().to_vec(), 0)
    } else if let Some(s) = f.status.filter(|s| *s != ST_NONE) {
        mk(Ks::Status, vec![s], 3)
    } else if let Some(k) = f.kind {
        mk(Ks::Kind, vec![k], 1)
    } else if let Some(s) = f.status {
        mk(Ks::Status, vec![s], 3)
    } else {
        mk(Ks::Time, Vec::new(), 4)
    }
}

fn meta_matches(m: &Meta, f: &Filter) -> bool {
    f.agent.is_none_or(|a| m.agent == a)
        && f.kind.is_none_or(|k| m.kind == k)
        && f.tool.as_deref().is_none_or(|t| m.tool == t)
        && f.status.is_none_or(|s| m.status == s)
}

/// Matching ids in (ts, id) order, at most `limit`.
fn filter_ids(r: &dyn Reader, f: &Filter, limit: usize) -> Result<Vec<u64>> {
    let p = plan(f);
    let needs = [f.agent.is_some(), f.kind.is_some(), f.tool.is_some(), f.status.is_some()];
    let covered = needs.iter().zip(p.covers).all(|(n, c)| !n || c);
    if covered {
        return ids_in(r, p.ks, &p.from, &p.to, limit);
    }
    let mut out = Vec::new();
    let mut cands = Vec::new();
    // scan the index in pages so a `limit` stops early
    let mut from = p.from.clone();
    while out.len() < limit {
        cands.clear();
        let mut last: Option<Vec<u8>> = None;
        r.scan(p.ks, &from, &p.to, &mut |k, _| {
            cands.push(tail_id(k));
            if cands.len() == 4096 {
                last = Some(k.to_vec());
                return false;
            }
            true
        })?;
        for &id in &cands {
            if let Some(m) = get_meta(r, id)?
                && meta_matches(&m, f)
            {
                out.push(id);
                if out.len() == limit {
                    break;
                }
            }
        }
        match last {
            Some(mut k) => {
                k.push(0); // the next key after the last one seen
                from = k;
            }
            None => break,
        }
    }
    Ok(out)
}

fn count_matching(r: &dyn Reader, f: &Filter) -> Result<u64> {
    let p = plan(f);
    let needs = [f.agent.is_some(), f.kind.is_some(), f.tool.is_some(), f.status.is_some()];
    let covered = needs.iter().zip(p.covers).all(|(n, c)| !n || c);
    let mut n = 0u64;
    if covered {
        r.scan(p.ks, &p.from, &p.to, &mut |_, _| {
            n += 1;
            true
        })?;
        return Ok(n);
    }
    let ids = ids_in(r, p.ks, &p.from, &p.to, usize::MAX)?;
    for id in ids {
        if let Some(m) = get_meta(r, id)?
            && meta_matches(&m, f)
        {
            n += 1;
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------- the arm

/// Lock stripes for revisions (read-modify-write of a call's meta).
const STRIPES: usize = 4096;

pub(crate) struct KvStore<E: Engine> {
    pub(crate) e: E,
    stripes: Vec<Mutex<()>>,
    /// Revisions hold it shared; deletes exclusively (a revision must not
    /// write back a row a concurrent delete removed).
    gate: RwLock<()>,
}

impl<E: Engine> KvStore<E> {
    pub(crate) fn new(e: E) -> Self {
        Self { e, stripes: (0..STRIPES).map(|_| Mutex::new(())).collect(), gate: RwLock::new(()) }
    }

    fn view<T>(&self, f: impl FnOnce(&dyn Reader) -> Result<T>) -> Result<T> {
        let mut f = Some(f);
        let mut out = None;
        self.e.read(&mut |r| {
            out = Some((f.take().expect("read view runs once"))(r)?);
            Ok(())
        })?;
        out.ok_or_else(|| "kv: read view did not run".into())
    }

    fn put_row(ops: &mut Vec<Op>, e: &Event) {
        let m = Meta::of(e);
        for (ks, k) in m.index_keys(e.id) {
            ops.push(Op::Put(ks, k, Vec::new()));
        }
        ops.push(Op::Put(Ks::Meta, id_key(e.id).to_vec(), m.encode()));
        ops.push(Op::Put(Ks::Body, id_key(e.id).to_vec(), encode_body(e.args, e.text)));
    }

    /// Write `events` (revisions of calls in the same set folded in first)
    /// as one atomic batch; revisions of earlier calls read-modify-write the
    /// call's meta under its stripe locks.
    fn write_events<'a>(&self, events: impl Iterator<Item = &'a Event<'a>>, sync: bool) -> Result<()> {
        let (rows, revs) = super::pg::fold(events);
        let mut ops = Vec::with_capacity(rows.len() * 8 + revs.len() * 3);
        for e in &rows {
            Self::put_row(&mut ops, e);
        }
        if revs.is_empty() {
            return self.e.write(ops, sync);
        }
        let _gate = self.gate.read().unwrap();
        let mut stripes: Vec<usize> = revs.iter().map(|r| (r.revises as usize) % STRIPES).collect();
        stripes.sort_unstable();
        stripes.dedup();
        let _held: Vec<_> = stripes.iter().map(|&s| self.stripes[s].lock().unwrap()).collect();
        let metas: Vec<Option<Meta>> = self.view(|r| revs.iter().map(|rv| get_meta(r, rv.revises)).collect())?;
        for (rv, m) in revs.iter().zip(metas) {
            // a revision of a call that is gone (deleted) or unknown changes nothing
            let Some(mut m) = m else { continue };
            if m.status != rv.status {
                ops.push(Op::Del(Ks::Status, status_key(m.status, m.ts_us, rv.revises)));
                ops.push(Op::Put(Ks::Status, status_key(rv.status, m.ts_us, rv.revises), Vec::new()));
            }
            m.status = rv.status;
            m.exit_code = rv.exit_code;
            m.duration_us = rv.duration_us;
            ops.push(Op::Put(Ks::Meta, id_key(rv.revises).to_vec(), m.encode()));
        }
        self.e.write(ops, sync)
    }

    /// Delete the rows `ids` (meta, body, every index key), in durable
    /// batches of at most 16,384 rows.
    fn delete_ids(&self, ids: Vec<u64>) -> Result<()> {
        let _gate = self.gate.write().unwrap();
        for chunk in ids.chunks(16_384) {
            let metas: Vec<Option<Meta>> = self.view(|r| chunk.iter().map(|&id| get_meta(r, id)).collect())?;
            let mut ops = Vec::with_capacity(chunk.len() * 8);
            for (&id, m) in chunk.iter().zip(metas) {
                let Some(m) = m else { continue };
                for (ks, k) in m.index_keys(id) {
                    ops.push(Op::Del(ks, k));
                }
                ops.push(Op::Del(Ks::Meta, id_key(id).to_vec()));
                ops.push(Op::Del(Ks::Body, id_key(id).to_vec()));
            }
            if !ops.is_empty() {
                self.e.write(ops, true)?;
            }
        }
        Ok(())
    }

    fn rows_of(r: &dyn Reader, ids: &[u64]) -> Result<Vec<Row>> {
        let mut v = Vec::with_capacity(ids.len());
        for &id in ids {
            // a row deleted between the index scan and here is skipped
            if let Some(row) = get_row(r, id)? {
                v.push(row);
            }
        }
        Ok(v)
    }
}

impl<E: Engine> TraceStore for KvStore<E> {
    fn name(&self) -> String {
        self.e.name()
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        self.write_events(batch.iter(), true)
    }

    fn append_many(&self, batches: &[Vec<Event>]) -> Result<()> {
        self.write_events(batches.iter().flatten(), true)
    }

    /// One non-synced batch per chunk; `finish_load` makes it durable.
    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        self.write_events(batches.iter().flatten(), false)
    }

    fn finish_load(&self) -> Result<()> {
        self.e.settle()
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        self.view(|r| get_row(r, id))
    }

    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        self.view(|r| {
            let mut out: Option<Result<Vec<u8>>> = None;
            r.get(Ks::Body, &id_key(id), &mut |v| {
                out = Some(body_parts(v).map(|(_, t)| {
                    let e = end.min(t.len());
                    t[start.min(e)..e].to_vec()
                }));
            })?;
            out.transpose()
        })
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        self.view(|r| Self::rows_of(r, &session_ids(r, session)?))
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        if from_us >= to_us {
            return Ok(Vec::new());
        }
        self.view(|r| Self::rows_of(r, &ids_in(r, Ks::Time, &bound(&[], from_us), &bound(&[], to_us), limit)?))
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        if f.from_us >= f.to_us {
            return Ok(Vec::new());
        }
        self.view(|r| filter_ids(r, f, limit))
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        if f.from_us >= f.to_us {
            return Ok(0);
        }
        self.view(|r| count_matching(r, f))
    }

    fn search(&self, _agent: u32, _query: &str, _as_of_us: i64, _k: usize) -> Result<Vec<u64>> {
        crate::unsupported("search", "key-value store: no full-text index (by design of this arm)")
    }

    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        self.view(|r| {
            let mut a = SessionAgg::default();
            for id in session_ids(r, session)? {
                let Some(m) = get_meta(r, id)? else { continue };
                a.events += 1;
                a.tokens_in += m.tokens_in as u64;
                a.tokens_out += m.tokens_out as u64;
                a.text_bytes += m.text_len as u64;
                if m.kind == TOOL_CALL {
                    a.tool_calls += 1;
                    a.tool_errors += (m.status == ST_ERROR) as u64;
                }
            }
            Ok(a)
        })
    }

    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        if from_us >= to_us {
            return Ok(Vec::new());
        }
        self.view(|r| {
            let head = [TOOL_CALL];
            let ids = ids_in(r, Ks::Kind, &bound(&head, from_us), &bound(&head, to_us), usize::MAX)?;
            let mut m: BTreeMap<Vec<u8>, ToolAgg> = BTreeMap::new();
            for id in ids {
                let Some(x) = get_meta(r, id)? else { continue };
                let t = m
                    .entry(x.tool.as_bytes().to_vec())
                    .or_insert_with(|| ToolAgg { tool: x.tool.clone(), ..Default::default() });
                t.calls += 1;
                t.errors += (x.status == ST_ERROR) as u64;
                t.duration_us += x.duration_us;
            }
            Ok(m.into_values().collect())
        })
    }

    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        if from_us >= to_us {
            return Ok(Vec::new());
        }
        self.view(|r| {
            let ids = ids_in(r, Ks::Time, &bound(&[], from_us), &bound(&[], to_us), usize::MAX)?;
            let mut m: HashMap<u64, u64> = HashMap::new();
            for id in ids {
                let Some(x) = get_meta(r, id)? else { continue };
                *m.entry(x.session).or_default() += x.tokens_in as u64 + x.tokens_out as u64;
            }
            let mut v: Vec<(u64, u64)> = m.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v.truncate(n);
            Ok(v)
        })
    }

    fn delete_session(&self, session: u64) -> Result<()> {
        let ids = self.view(|r| session_ids(r, session))?;
        self.delete_ids(ids)
    }

    fn purge_before(&self, ts_us: i64) -> Result<()> {
        let ids = self.view(|r| ids_in(r, Ks::Time, &bound(&[], i64::MIN), &bound(&[], ts_us), usize::MAX))?;
        self.delete_ids(ids)
    }

    fn reclaim(&self) -> Result<()> {
        let _gate = self.gate.write().unwrap();
        self.e.compact()
    }

    /// The session index in key order is (session, seq) order.
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        self.view(|r| {
            let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
            let mut from = Vec::new();
            loop {
                // pages of 4,096 ids, so the scan never holds the whole store
                let mut ids = Vec::with_capacity(4096);
                let mut last = None;
                r.scan(Ks::Sess, &from, &[0xff; 21], &mut |k, _| {
                    ids.push(tail_id(k));
                    if ids.len() == 4096 {
                        last = Some(k.to_vec());
                        return false;
                    }
                    true
                })?;
                for row in Self::rows_of(r, &ids)? {
                    buf.clear();
                    crate::export_line(&row, &mut buf);
                    out.write_all(&buf)?;
                    n += 1;
                    bytes += buf.len() as u64;
                }
                match last {
                    Some(mut k) => {
                        k.push(0);
                        from = k;
                    }
                    None => break,
                }
            }
            Ok((n, bytes))
        })
    }

    /// Allocated bytes of every file of the store (`du`): fjall's journals
    /// are sparse 64 MiB files, RocksDB fallocates ahead; details report the
    /// logical size too.
    fn disk_bytes(&self) -> Result<u64> {
        Ok(crate::sys::dir_alloc_bytes(self.e.dir()))
    }

    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::rss_anon())
    }

    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = Vec::new();
        crate::sys::files_under(self.e.dir(), &mut v);
        Ok(v)
    }

    /// The engine's per-keyspace numbers, plus logical and allocated bytes
    /// of the store directory (`disk_bytes` is the allocated size).
    fn details(&self) -> serde_json::Value {
        let mut v = self.e.details();
        if !v.is_object() {
            v = serde_json::json!({});
        }
        v["dir_logical_bytes"] = serde_json::json!(crate::sys::dir_bytes(self.e.dir()));
        v["dir_allocated_bytes"] = serde_json::json!(crate::sys::dir_alloc_bytes(self.e.dir()));
        v
    }
}

/// Block cache / page cache size (MiB) shared by the three engines unless
/// `cache_mb=` says otherwise.
pub(crate) const DEFAULT_CACHE_MB: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_round_trips_and_keys_order() {
        let e = Event {
            id: 7,
            session: 3,
            agent: 2,
            turn: 1,
            seq: 4,
            parent: 6,
            revises: 0,
            kind: TOOL_CALL,
            status: 1,
            exit_code: Some(-3),
            tokens_in: 10,
            tokens_out: 11,
            ts_us: -5,
            duration_us: 9,
            tool: "bash",
            args: "{}",
            text: "héllo",
        };
        let m = Meta::of(&e);
        let d = Meta::decode(&m.encode()).unwrap();
        let row = d.row(7, "{}".into(), "héllo".into());
        assert_eq!(row, Row::from_event(&e));
        assert!(ts_key(-5) < ts_key(0) && ts_key(0) < ts_key(i64::MAX) && ts_key(i64::MIN) < ts_key(-5));
        assert_eq!(ts_of(&ts_key(-5)), -5);
        let k = &m.index_keys(7)[1].1;
        assert_eq!((tail_ts(k), tail_id(k)), (-5, 7));
        let (a, b) = prefix_range(&[1, 0xff]);
        assert_eq!((a, b), (vec![1, 0xff], vec![2]));
    }

    /// A small trace (sessions of calls revised one batch later, results,
    /// thinking) through an arm and the owning oracle: every deterministic
    /// read equal before and after deletes, purge, reclaim and a reopen.
    #[cfg(any(feature = "arm-redb", feature = "arm-fjall", feature = "arm-rocksdb"))]
    fn exercise(spec: &str) {
        use crate::arms::{self, oracle::OwnedOracle};
        use crate::events::{OUTPUT, ST_OK, ST_PENDING, THINKING, TOOL_RESULT};
        let dir = std::env::temp_dir().join(format!("tracebench-kv-test-{spec}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // leaked: the events borrow them for the whole test
        let texts: Vec<&'static str> =
            (0..64).map(|i| &*Box::leak(format!("text {i} {}", "é".repeat(i % 7)).into_boxed_str())).collect();
        let tools = ["bash", "edit", ""];
        let mut batches: Vec<Vec<Event>> = Vec::new();
        let mut id = 0u64;
        let mut open_calls: Vec<(u64, u64)> = Vec::new(); // (call id, session)
        for step in 0..120u64 {
            let session = 1 + step % 9;
            let mut b = Vec::new();
            let mut ev = |kind: u8, status: u8, revises: u64, tool: &'static str, text: &'static str, ts: i64| {
                id += 1;
                Event {
                    id,
                    session,
                    agent: (session % 4) as u32,
                    turn: step as u32,
                    seq: id as u32,
                    parent: 0,
                    revises,
                    kind,
                    status,
                    exit_code: (revises != 0).then_some((id % 3) as i32 - 1),
                    tokens_in: (id % 11) as u32,
                    tokens_out: (id % 5) as u32,
                    ts_us: ts,
                    duration_us: if revises != 0 { id as i64 * 10 } else { 0 },
                    tool,
                    args: if kind == TOOL_CALL { "{\"a\":1}" } else { "" },
                    text: if revises != 0 { "" } else { text },
                }
            };
            let ts = 1_000_000 + step as i64 * 3_600_000_000 / 7;
            // revise the calls of this session from earlier batches
            let mine: Vec<(u64, u64)> = open_calls.iter().copied().filter(|c| c.1 == session).collect();
            open_calls.retain(|c| c.1 != session);
            for (c, _) in mine {
                let st = if c % 4 == 0 { ST_ERROR } else { ST_OK };
                b.push(ev(TOOL_CALL, st, c, "", "", ts));
            }
            let t = texts[(step % 64) as usize];
            b.push(ev(TOOL_RESULT, ST_NONE, 0, tools[(step % 3) as usize], t, ts));
            b.push(ev(THINKING, ST_NONE, 0, "", t, ts + 1));
            b.push(ev(OUTPUT, ST_NONE, 0, "", t, ts + 1));
            let call = ev(TOOL_CALL, ST_PENDING, 0, tools[(step % 2) as usize], t, ts + 2);
            open_calls.push((call.id, session));
            b.push(call);
            // a same-batch revision of the call (folded by the arm)
            if step % 5 == 0 {
                let c = b.last().unwrap().id;
                b.push(ev(TOOL_CALL, ST_OK, c, "", "", ts + 3));
                open_calls.pop();
            }
            batches.push(b);
        }
        let store = arms::open(spec, &dir).unwrap();
        let oracle = OwnedOracle::new();
        store.load(&batches[..40]).unwrap();
        store.finish_load().unwrap();
        oracle.load(&batches[..40]).unwrap();
        for b in &batches[40..] {
            store.append(b).unwrap();
            oracle.append(b).unwrap();
        }
        let check = |store: &dyn TraceStore, what: &str| {
            let all = Filter { from_us: i64::MIN / 2, to_us: i64::MAX / 2, ..Default::default() };
            for i in 1..=id {
                assert_eq!(store.get(i).unwrap(), oracle.get(i).unwrap(), "{what}: get {i}");
                assert_eq!(store.get_range(i, 2, 9).unwrap(), oracle.get_range(i, 2, 9).unwrap(), "{what}: range {i}");
            }
            for s in 0..11 {
                assert_eq!(store.session(s).unwrap(), oracle.session(s).unwrap(), "{what}: session {s}");
                assert_eq!(store.agg_session(s).unwrap(), oracle.agg_session(s).unwrap(), "{what}: agg {s}");
            }
            let (lo, hi) = (1_000_000i64, 1_000_000 + 120 * 3_600_000_000 / 7);
            for (f, t) in [(lo, hi), (lo + 10_000_000_000, lo + 40_000_000_000), (hi, lo)] {
                assert_eq!(store.time_range(f, t, 37).unwrap(), oracle.time_range(f, t, 37).unwrap(), "{what}: time");
                assert_eq!(store.agg_tools(f, t).unwrap(), oracle.agg_tools(f, t).unwrap(), "{what}: tools");
                assert_eq!(store.top_sessions(f, t, 4).unwrap(), oracle.top_sessions(f, t, 4).unwrap(), "{what}: top");
            }
            let filters = [
                all.clone(),
                Filter { kind: Some(TOOL_CALL), ..all.clone() },
                Filter { kind: Some(TOOL_CALL), status: Some(ST_ERROR), tool: Some("bash".into()), ..all.clone() },
                Filter { agent: Some(1), kind: Some(TOOL_RESULT), from_us: lo + 5_000_000_000, to_us: hi, ..all.clone() },
                Filter { status: Some(ST_PENDING), ..all.clone() },
                Filter { status: Some(ST_NONE), agent: Some(2), ..all.clone() },
                Filter { tool: Some(String::new()), ..all.clone() },
            ];
            for f in &filters {
                assert_eq!(store.filter(f, 13).unwrap(), oracle.filter(f, 13).unwrap(), "{what}: filter {f:?}");
                assert_eq!(store.count(f).unwrap(), oracle.count(f).unwrap(), "{what}: count {f:?}");
            }
            let (mut a, mut b) = (Vec::new(), Vec::new());
            assert_eq!(store.export(&mut a).unwrap(), oracle.export(&mut b).unwrap(), "{what}: export");
            assert!(a == b, "{what}: export bytes");
        };
        check(store.as_ref(), "loaded");
        assert!(crate::unsupported_reason(&store.search(0, "text", i64::MAX, 5).unwrap_err()).is_some());
        store.delete_session(3).unwrap();
        oracle.delete_session(3).unwrap();
        store.purge_before(1_000_000 + 20 * 3_600_000_000 / 7).unwrap();
        oracle.purge_before(1_000_000 + 20 * 3_600_000_000 / 7).unwrap();
        check(store.as_ref(), "deleted");
        store.reclaim().unwrap();
        check(store.as_ref(), "reclaimed");
        drop(store);
        let store = arms::reopen(spec, &dir).unwrap();
        check(store.as_ref(), "reopened");
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "arm-redb")]
    #[test]
    fn redb_matches_the_oracle() {
        exercise("redb");
    }

    #[cfg(feature = "arm-fjall")]
    #[test]
    fn fjall_matches_the_oracle() {
        exercise("fjall");
    }

    #[cfg(feature = "arm-rocksdb")]
    #[test]
    fn rocksdb_matches_the_oracle() {
        exercise("rocksdb");
    }
}
