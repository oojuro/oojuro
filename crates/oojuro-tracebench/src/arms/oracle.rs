//! In-memory full-scan reference arm. Not a contender: it defines the
//! expected answer of every deterministic operation (search excepted) and
//! the runner checks the other arms against it on sampled queries.
use crate::events::{Event, ST_ERROR, TOOL_CALL};
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore};
use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

struct Slot<'b> {
    ev: Event<'b>,
    status: u8,
    exit_code: Option<i32>,
    duration_us: i64,
    live: bool,
}

#[derive(Default)]
struct Inner<'b> {
    slots: HashMap<u64, Slot<'b>>,
    sessions: HashMap<u64, Vec<u64>>,
}

pub struct Oracle<'b> {
    inner: RwLock<Inner<'b>>,
}

impl<'b> Oracle<'b> {
    pub fn new() -> Self {
        Self { inner: RwLock::new(Inner::default()) }
    }

    /// The oracle borrows text from the mapped tier, so it takes `Event<'b>`.
    pub fn add(&self, batch: &[Event<'b>]) {
        let mut g = self.inner.write().unwrap();
        for e in batch {
            if e.revises != 0 {
                if let Some(s) = g.slots.get_mut(&e.revises) {
                    s.status = e.status;
                    s.exit_code = e.exit_code;
                    s.duration_us = e.duration_us;
                }
                continue;
            }
            g.sessions.entry(e.session).or_default().push(e.id);
            g.slots.insert(
                e.id,
                Slot { ev: *e, status: e.status, exit_code: e.exit_code, duration_us: e.duration_us, live: true },
            );
        }
    }

    fn row(s: &Slot) -> Row {
        let mut r = Row::from_event(&s.ev);
        r.status = s.status;
        r.exit_code = s.exit_code;
        r.duration_us = s.duration_us;
        r
    }

    fn matches(s: &Slot, f: &Filter) -> bool {
        s.live
            && s.ev.ts_us >= f.from_us
            && s.ev.ts_us < f.to_us
            && f.agent.is_none_or(|a| s.ev.agent == a)
            && f.kind.is_none_or(|k| s.ev.kind == k)
            && f.tool.as_deref().is_none_or(|t| s.ev.tool == t)
            && f.status.is_none_or(|st| s.status == st)
    }
}

impl Default for Oracle<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceStore for Oracle<'_> {
    fn name(&self) -> String {
        "oracle".into()
    }
    fn append(&self, _batch: &[Event]) -> Result<()> {
        Err("the oracle is fed with Oracle::add".into())
    }
    fn get(&self, id: u64) -> Result<Option<Row>> {
        let g = self.inner.read().unwrap();
        Ok(g.slots.get(&id).filter(|s| s.live).map(Self::row))
    }
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let g = self.inner.read().unwrap();
        Ok(g.slots.get(&id).filter(|s| s.live).map(|s| {
            let t = s.ev.text.as_bytes();
            let e = end.min(t.len());
            t[start.min(e)..e].to_vec()
        }))
    }
    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let g = self.inner.read().unwrap();
        let mut v: Vec<Row> = g
            .sessions
            .get(&session)
            .into_iter()
            .flatten()
            .filter_map(|id| g.slots.get(id).filter(|s| s.live).map(Self::row))
            .collect();
        v.sort_by_key(|r| r.seq);
        Ok(v)
    }
    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let g = self.inner.read().unwrap();
        let mut v: Vec<&Slot> =
            g.slots.values().filter(|s| s.live && s.ev.ts_us >= from_us && s.ev.ts_us < to_us).collect();
        v.sort_by_key(|s| (s.ev.ts_us, s.ev.id));
        Ok(v.into_iter().take(limit).map(Self::row).collect())
    }
    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let g = self.inner.read().unwrap();
        let mut v: Vec<(i64, u64)> =
            g.slots.values().filter(|s| Self::matches(s, f)).map(|s| (s.ev.ts_us, s.ev.id)).collect();
        v.sort_unstable();
        Ok(v.into_iter().take(limit).map(|x| x.1).collect())
    }
    fn count(&self, f: &Filter) -> Result<u64> {
        let g = self.inner.read().unwrap();
        Ok(g.slots.values().filter(|s| Self::matches(s, f)).count() as u64)
    }
    fn search(&self, _agent: u32, _query: &str, _as_of_us: i64, _k: usize) -> Result<Vec<u64>> {
        Ok(Vec::new())
    }
    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        let mut a = SessionAgg::default();
        for r in self.session(session)? {
            a.events += 1;
            a.tokens_in += r.tokens_in as u64;
            a.tokens_out += r.tokens_out as u64;
            a.text_bytes += r.text.len() as u64;
            if r.kind == TOOL_CALL {
                a.tool_calls += 1;
                a.tool_errors += (r.status == ST_ERROR) as u64;
            }
        }
        Ok(a)
    }
    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        let g = self.inner.read().unwrap();
        let mut m: BTreeMap<&str, ToolAgg> = BTreeMap::new();
        for s in g.slots.values() {
            if s.live && s.ev.kind == TOOL_CALL && s.ev.ts_us >= from_us && s.ev.ts_us < to_us {
                let t = m.entry(s.ev.tool).or_insert_with(|| ToolAgg { tool: s.ev.tool.into(), ..Default::default() });
                t.calls += 1;
                t.errors += (s.status == ST_ERROR) as u64;
                t.duration_us += s.duration_us;
            }
        }
        Ok(m.into_values().collect())
    }
    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        let g = self.inner.read().unwrap();
        let mut m: HashMap<u64, u64> = HashMap::new();
        for s in g.slots.values() {
            if s.live && s.ev.ts_us >= from_us && s.ev.ts_us < to_us {
                *m.entry(s.ev.session).or_default() += s.ev.tokens_in as u64 + s.ev.tokens_out as u64;
            }
        }
        let mut v: Vec<(u64, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        Ok(v)
    }
    fn delete_session(&self, session: u64) -> Result<()> {
        let mut g = self.inner.write().unwrap();
        let ids = g.sessions.get(&session).cloned().unwrap_or_default();
        for id in ids {
            if let Some(s) = g.slots.get_mut(&id) {
                s.live = false;
            }
        }
        Ok(())
    }
    fn purge_before(&self, ts_us: i64) -> Result<()> {
        let mut g = self.inner.write().unwrap();
        for s in g.slots.values_mut() {
            if s.ev.ts_us < ts_us {
                s.live = false;
            }
        }
        Ok(())
    }
    fn reclaim(&self) -> Result<()> {
        Ok(())
    }
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let g = self.inner.read().unwrap();
        let mut sessions: Vec<u64> = g.sessions.keys().copied().collect();
        sessions.sort_unstable();
        drop(g);
        let (mut n, mut bytes, mut buf) = (0, 0, Vec::new());
        for s in sessions {
            for r in self.session(s)? {
                buf.clear();
                crate::export_line(&r, &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
        }
        Ok((n, bytes))
    }
    fn disk_bytes(&self) -> Result<u64> {
        Ok(0)
    }
    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::rss_anon())
    }
    fn data_paths(&self) -> Result<Vec<std::path::PathBuf>> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------- owning reference arm

#[derive(Default)]
struct OwnedInner {
    /// id -> (row with the latest status, live)
    rows: HashMap<u64, (Row, bool)>,
    sessions: HashMap<u64, Vec<u64>>,
    by_ts: std::collections::BTreeSet<(i64, u64)>,
}

/// The in-memory reference as an arm (`--arm oracle`): the same answers as
/// [`Oracle`], fed through the trait (`append`, `append_many`, `load`) with
/// its own copy of every text, so it works for data the runner generates
/// (the `ycsb` suite). `Oracle` itself borrows the mapped tier and stays the
/// reference of `verify`. Not a contender: no disk, no durability.
#[derive(Default)]
pub struct OwnedOracle {
    inner: RwLock<OwnedInner>,
}

impl OwnedOracle {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&self, events: &[Event]) {
        let mut g = self.inner.write().unwrap();
        for e in events {
            if e.revises != 0 {
                if let Some((r, _)) = g.rows.get_mut(&e.revises) {
                    r.apply(e);
                }
                continue;
            }
            g.sessions.entry(e.session).or_default().push(e.id);
            g.by_ts.insert((e.ts_us, e.id));
            g.rows.insert(e.id, (Row::from_event(e), true));
        }
    }

    fn matches(r: &Row, f: &Filter) -> bool {
        r.ts_us >= f.from_us
            && r.ts_us < f.to_us
            && f.agent.is_none_or(|a| r.agent == a)
            && f.kind.is_none_or(|k| r.kind == k)
            && f.tool.as_deref().is_none_or(|t| r.tool == t)
            && f.status.is_none_or(|st| r.status == st)
    }

    fn live_rows(g: &OwnedInner) -> impl Iterator<Item = &Row> {
        g.rows.values().filter(|(_, l)| *l).map(|(r, _)| r)
    }
}

impl TraceStore for OwnedOracle {
    fn name(&self) -> String {
        "oracle (in-memory reference, owning)".into()
    }
    fn append(&self, batch: &[Event]) -> Result<()> {
        self.add(batch);
        Ok(())
    }
    fn append_many(&self, batches: &[Vec<Event>]) -> Result<()> {
        for b in batches {
            self.add(b);
        }
        Ok(())
    }
    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        self.append_many(batches)
    }
    fn get(&self, id: u64) -> Result<Option<Row>> {
        let g = self.inner.read().unwrap();
        Ok(g.rows.get(&id).filter(|(_, l)| *l).map(|(r, _)| r.clone()))
    }
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let g = self.inner.read().unwrap();
        Ok(g.rows.get(&id).filter(|(_, l)| *l).map(|(r, _)| {
            let t = r.text.as_bytes();
            let e = end.min(t.len());
            t[start.min(e)..e].to_vec()
        }))
    }
    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let g = self.inner.read().unwrap();
        let mut v: Vec<Row> = g
            .sessions
            .get(&session)
            .into_iter()
            .flatten()
            .filter_map(|id| g.rows.get(id).filter(|(_, l)| *l).map(|(r, _)| r.clone()))
            .collect();
        v.sort_by_key(|r| r.seq);
        Ok(v)
    }
    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let g = self.inner.read().unwrap();
        if from_us >= to_us {
            return Ok(Vec::new());
        }
        Ok(g.by_ts
            .range((from_us, 0)..(to_us, 0))
            .filter_map(|(_, id)| g.rows.get(id).filter(|(_, l)| *l).map(|(r, _)| r.clone()))
            .take(limit)
            .collect())
    }
    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let g = self.inner.read().unwrap();
        let mut v: Vec<(i64, u64)> =
            Self::live_rows(&g).filter(|r| Self::matches(r, f)).map(|r| (r.ts_us, r.id)).collect();
        v.sort_unstable();
        Ok(v.into_iter().take(limit).map(|x| x.1).collect())
    }
    fn count(&self, f: &Filter) -> Result<u64> {
        let g = self.inner.read().unwrap();
        Ok(Self::live_rows(&g).filter(|r| Self::matches(r, f)).count() as u64)
    }
    fn search(&self, _agent: u32, _query: &str, _as_of_us: i64, _k: usize) -> Result<Vec<u64>> {
        Ok(Vec::new())
    }
    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        let mut a = SessionAgg::default();
        for r in self.session(session)? {
            a.events += 1;
            a.tokens_in += r.tokens_in as u64;
            a.tokens_out += r.tokens_out as u64;
            a.text_bytes += r.text.len() as u64;
            if r.kind == TOOL_CALL {
                a.tool_calls += 1;
                a.tool_errors += (r.status == ST_ERROR) as u64;
            }
        }
        Ok(a)
    }
    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        let g = self.inner.read().unwrap();
        let mut m: BTreeMap<&str, ToolAgg> = BTreeMap::new();
        for r in Self::live_rows(&g) {
            if r.kind == TOOL_CALL && r.ts_us >= from_us && r.ts_us < to_us {
                let t = m.entry(&r.tool).or_insert_with(|| ToolAgg { tool: r.tool.clone(), ..Default::default() });
                t.calls += 1;
                t.errors += (r.status == ST_ERROR) as u64;
                t.duration_us += r.duration_us;
            }
        }
        Ok(m.into_values().collect())
    }
    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        let g = self.inner.read().unwrap();
        let mut m: HashMap<u64, u64> = HashMap::new();
        for r in Self::live_rows(&g) {
            if r.ts_us >= from_us && r.ts_us < to_us {
                *m.entry(r.session).or_default() += r.tokens_in as u64 + r.tokens_out as u64;
            }
        }
        let mut v: Vec<(u64, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        Ok(v)
    }
    fn delete_session(&self, session: u64) -> Result<()> {
        let mut g = self.inner.write().unwrap();
        let ids = g.sessions.get(&session).cloned().unwrap_or_default();
        for id in ids {
            if let Some((_, l)) = g.rows.get_mut(&id) {
                *l = false;
            }
        }
        Ok(())
    }
    fn purge_before(&self, ts_us: i64) -> Result<()> {
        let mut g = self.inner.write().unwrap();
        for (r, l) in g.rows.values_mut() {
            if r.ts_us < ts_us {
                *l = false;
            }
        }
        Ok(())
    }
    fn reclaim(&self) -> Result<()> {
        Ok(())
    }
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let g = self.inner.read().unwrap();
        let mut sessions: Vec<u64> = g.sessions.keys().copied().collect();
        sessions.sort_unstable();
        drop(g);
        let (mut n, mut bytes, mut buf) = (0, 0, Vec::new());
        for s in sessions {
            for r in self.session(s)? {
                buf.clear();
                crate::export_line(&r, &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
        }
        Ok((n, bytes))
    }
    fn disk_bytes(&self) -> Result<u64> {
        Ok(0)
    }
    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::rss_anon())
    }
    fn data_paths(&self) -> Result<Vec<std::path::PathBuf>> {
        Ok(Vec::new())
    }
}
