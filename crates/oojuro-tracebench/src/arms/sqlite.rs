//! SQLite baseline arm (`sqlite`), embedded via rusqlite (bundled SQLite).
//!
//! One file in the run's work directory, WAL journal, `synchronous=FULL`
//! (every commit fsyncs the WAL). SQLite has one writer: every write goes
//! through one connection behind a mutex; reads use a pool of connections
//! (WAL readers run concurrently with the writer). Search is a contentless
//! FTS5 table (`contentless_delete=1`) with an `agent` column so the agent
//! filter is part of the full-text match, ranked by FTS5's `bm25`.
//! Settings and their reasons: `docs/TRACEBENCH_BASELINES.md`.
use super::Params;
use super::pg::{fold, query_words};
use crate::events::Event;
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::Value};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Page cache per connection (negative = KiB): 64 MiB.
const CACHE_KIB: i64 = 65_536;

pub struct Sqlite {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

const COLS: &str = "id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, \
                    ts_us, duration_us, tool, args, text";

const SCHEMA: &str = "
CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL,
  agent_id INTEGER NOT NULL,
  turn INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  parent INTEGER NOT NULL,
  kind INTEGER NOT NULL,
  status INTEGER NOT NULL,
  exit_code INTEGER,
  tokens_in INTEGER NOT NULL,
  tokens_out INTEGER NOT NULL,
  ts_us INTEGER NOT NULL,
  duration_us INTEGER NOT NULL,
  tool TEXT NOT NULL,
  args TEXT NOT NULL,
  text TEXT NOT NULL
) STRICT;
CREATE INDEX events_session_seq ON events (session_id, seq);
CREATE INDEX events_kind_ts ON events (kind, ts_us);
CREATE INDEX events_ts ON events (ts_us);
CREATE VIRTUAL TABLE events_fts USING fts5(
  agent, body, content='', contentless_delete=1, tokenize='porter unicode61'
);
";

fn pragmas(c: &Connection) -> Result<()> {
    c.busy_timeout(std::time::Duration::from_secs(600))?;
    c.execute_batch(&format!(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA cache_size = -{CACHE_KIB};
         PRAGMA temp_store = FILE;
         PRAGMA mmap_size = 0;
         PRAGMA wal_autocheckpoint = 1000;
         PRAGMA secure_delete = OFF;"
    ))?;
    Ok(())
}

/// Params: `existing=on` opens the store already in `work` (no schema
/// creation; used to time reopen and crash recovery).
pub fn open(work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let path = work.join("trace.db");
    let existing = params.flag("existing", false);
    if existing && !path.exists() {
        return Err(format!("no existing store at {}", path.display()).into());
    }
    let writer = Connection::open(&path)?;
    pragmas(&writer)?;
    if !existing {
        writer.execute_batch(SCHEMA)?;
    }
    Ok(Box::new(Sqlite { path, writer: Mutex::new(writer), readers: Mutex::new(Vec::new()) }))
}

fn row_of(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get::<_, i64>(0)? as u64,
        session: r.get::<_, i64>(1)? as u64,
        agent: r.get::<_, i64>(2)? as u32,
        turn: r.get::<_, i64>(3)? as u32,
        seq: r.get::<_, i64>(4)? as u32,
        parent: r.get::<_, i64>(5)? as u64,
        kind: r.get::<_, i64>(6)? as u8,
        status: r.get::<_, i64>(7)? as u8,
        exit_code: r.get::<_, Option<i64>>(8)?.map(|v| v as i32),
        tokens_in: r.get::<_, i64>(9)? as u32,
        tokens_out: r.get::<_, i64>(10)? as u32,
        ts_us: r.get(11)?,
        duration_us: r.get(12)?,
        tool: r.get(13)?,
        args: r.get(14)?,
        text: r.get(15)?,
    })
}

fn filter_sql(f: &Filter) -> (String, Vec<Value>) {
    let mut w = vec!["ts_us >= ?".to_string(), "ts_us < ?".to_string()];
    let mut p = vec![Value::Integer(f.from_us), Value::Integer(f.to_us)];
    if let Some(a) = f.agent {
        w.push("agent_id = ?".into());
        p.push(Value::Integer(a as i64));
    }
    if let Some(k) = f.kind {
        w.push("kind = ?".into());
        p.push(Value::Integer(k as i64));
    }
    if let Some(t) = &f.tool {
        w.push("tool = ?".into());
        p.push(Value::Text(t.clone()));
    }
    if let Some(s) = f.status {
        w.push("status = ?".into());
        p.push(Value::Integer(s as i64));
    }
    (w.join(" AND "), p)
}

/// FTS5 string literal.
fn fts_quote(w: &str) -> String {
    format!("\"{}\"", w.replace('"', "\"\""))
}

impl Sqlite {
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = self.readers.lock().unwrap().pop();
        let c = match pooled {
            Some(c) => c,
            None => {
                let c = Connection::open_with_flags(
                    &self.path,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
                )?;
                pragmas(&c)?;
                c.execute_batch("PRAGMA query_only = ON")?;
                c
            }
        };
        let r = f(&c);
        self.readers.lock().unwrap().push(c);
        r
    }

    /// Write the events (revisions already folded in) and apply `revs`, in
    /// the writer's current transaction.
    fn write_rows(c: &Connection, rows: &[Event], revs: &[Event]) -> Result<()> {
        let mut ins = c.prepare_cached(
            "INSERT INTO events (id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out,
                                 ts_us, duration_us, tool, args, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )?;
        let mut fts = c.prepare_cached("INSERT INTO events_fts (rowid, agent, body) VALUES (?1, ?2, ?3)")?;
        for e in rows {
            ins.execute(params![
                e.id as i64,
                e.session as i64,
                e.agent as i64,
                e.turn as i64,
                e.seq as i64,
                e.parent as i64,
                e.kind as i64,
                e.status as i64,
                e.exit_code.map(|v| v as i64),
                e.tokens_in as i64,
                e.tokens_out as i64,
                e.ts_us,
                e.duration_us,
                e.tool,
                e.args,
                e.text,
            ])?;
            if crate::events::indexed_kind(e.kind) {
                fts.execute(params![e.id as i64, format!("a{}", e.agent), e.text])?;
            }
        }
        if !revs.is_empty() {
            let mut upd = c.prepare_cached("UPDATE events SET status = ?2, exit_code = ?3, duration_us = ?4 WHERE id = ?1")?;
            for r in revs {
                upd.execute(params![r.revises as i64, r.status as i64, r.exit_code.map(|v| v as i64), r.duration_us])?;
            }
        }
        Ok(())
    }

    fn write_tx(&self, rows: &[Event], revs: &[Event]) -> Result<()> {
        let mut c = self.writer.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::write_rows(&tx, rows, revs)?;
        tx.commit()?;
        Ok(())
    }

    fn rows(&self, sql: &str, p: &[Value]) -> Result<Vec<Row>> {
        self.read(|c| {
            let mut st = c.prepare_cached(sql)?;
            let v = st.query_map(rusqlite::params_from_iter(p.iter()), row_of)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
    }

    /// Delete the rows selected by `cond` (and their FTS entries) in one
    /// transaction.
    fn delete_where(&self, cond: &str, v: i64) -> Result<()> {
        let mut c = self.writer.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            &format!("DELETE FROM events_fts WHERE rowid IN (SELECT id FROM events WHERE {cond} AND kind IN (1, 3, 4, 5))"),
            [v],
        )?;
        tx.execute(&format!("DELETE FROM events WHERE {cond}"), [v])?;
        tx.commit()?;
        Ok(())
    }
}

impl TraceStore for Sqlite {
    fn name(&self) -> String {
        format!("sqlite {} (WAL, synchronous=FULL, FTS5 contentless)", rusqlite::version())
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        let (rows, revs) = fold(batch.iter());
        self.write_tx(&rows, &revs)
    }

    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        let (rows, revs) = fold(batches.iter().flatten());
        self.write_tx(&rows, &revs)
    }

    fn finish_load(&self) -> Result<()> {
        let c = self.writer.lock().unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); ANALYZE;")?;
        Ok(())
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        let sql = format!("SELECT {COLS} FROM events WHERE id = ?1");
        Ok(self.rows(&sql, &[Value::Integer(id as i64)])?.into_iter().next())
    }

    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let len = end.saturating_sub(start).min(i64::MAX as usize) as i64;
        let from = start.min(i64::MAX as usize - 1) as i64 + 1;
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT substr(CAST(text AS BLOB), ?2, ?3) FROM events WHERE id = ?1")?;
            Ok(st
                .query_row(params![id as i64, from, len], |r| r.get::<_, Option<Vec<u8>>>(0))
                .optional()?
                .map(Option::unwrap_or_default))
        })
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let sql = format!("SELECT {COLS} FROM events WHERE session_id = ?1 ORDER BY seq");
        self.rows(&sql, &[Value::Integer(session as i64)])
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let sql = format!("SELECT {COLS} FROM events WHERE ts_us >= ?1 AND ts_us < ?2 ORDER BY ts_us, id LIMIT ?3");
        self.rows(&sql, &[Value::Integer(from_us), Value::Integer(to_us), Value::Integer(limit as i64)])
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let (w, mut p) = filter_sql(f);
        p.push(Value::Integer(limit as i64));
        let sql = format!("SELECT id FROM events WHERE {w} ORDER BY ts_us, id LIMIT ?");
        self.read(|c| {
            let mut st = c.prepare_cached(&sql)?;
            let v = st
                .query_map(rusqlite::params_from_iter(p.iter()), |r| r.get::<_, i64>(0))?
                .map(|x| x.map(|v| v as u64))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        let (w, p) = filter_sql(f);
        let sql = format!("SELECT count(*) FROM events WHERE {w}");
        self.read(|c| {
            let mut st = c.prepare_cached(&sql)?;
            Ok(st.query_row(rusqlite::params_from_iter(p.iter()), |r| r.get::<_, i64>(0))? as u64)
        })
    }

    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let words = query_words(query, char::is_alphanumeric, false, 64);
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let ors: Vec<String> = words.iter().map(|w| fts_quote(w)).collect();
        let m = format!("agent : {} AND body : ({})", fts_quote(&format!("a{agent}")), ors.join(" OR "));
        self.read(|c| {
            // bm25 weights: agent column 0 (a filter, not evidence), body 1.
            let mut st = c.prepare_cached(
                "SELECT e.id FROM events_fts f JOIN events e ON e.id = f.rowid
                 WHERE events_fts MATCH ?1 AND e.ts_us <= ?2
                 ORDER BY bm25(events_fts, 0.0, 1.0), e.ts_us DESC, e.id DESC LIMIT ?3",
            )?;
            let v = st
                .query_map(params![m, as_of_us, k as i64], |r| r.get::<_, i64>(0))?
                .map(|x| x.map(|v| v as u64))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
    }

    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        self.read(|c| {
            // length(CAST(text AS BLOB)) = bytes (length(text) counts characters).
            let mut st = c.prepare_cached(
                "SELECT count(*), coalesce(sum(tokens_in), 0), coalesce(sum(tokens_out), 0),
                        coalesce(sum(kind = 4), 0), coalesce(sum(kind = 4 AND status = 3), 0),
                        coalesce(sum(length(CAST(text AS BLOB))), 0)
                 FROM events WHERE session_id = ?1",
            )?;
            Ok(st.query_row([session as i64], |r| {
                Ok(SessionAgg {
                    events: r.get::<_, i64>(0)? as u64,
                    tokens_in: r.get::<_, i64>(1)? as u64,
                    tokens_out: r.get::<_, i64>(2)? as u64,
                    tool_calls: r.get::<_, i64>(3)? as u64,
                    tool_errors: r.get::<_, i64>(4)? as u64,
                    text_bytes: r.get::<_, i64>(5)? as u64,
                })
            })?)
        })
    }

    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        let mut v = self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT tool, count(*), coalesce(sum(status = 3), 0), coalesce(sum(duration_us), 0)
                 FROM events WHERE kind = 4 AND ts_us >= ?1 AND ts_us < ?2 GROUP BY tool",
            )?;
            let v = st
                .query_map([from_us, to_us], |r| {
                    Ok(ToolAgg {
                        tool: r.get(0)?,
                        calls: r.get::<_, i64>(1)? as u64,
                        errors: r.get::<_, i64>(2)? as u64,
                        duration_us: r.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })?;
        v.sort_by(|a, b| a.tool.as_bytes().cmp(b.tool.as_bytes()));
        Ok(v)
    }

    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT session_id, sum(tokens_in + tokens_out) AS t FROM events
                 WHERE ts_us >= ?1 AND ts_us < ?2 GROUP BY session_id ORDER BY t DESC, session_id ASC LIMIT ?3",
            )?;
            let v = st
                .query_map([from_us, to_us, n as i64], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
    }

    fn delete_session(&self, session: u64) -> Result<()> {
        self.delete_where("session_id = ?1", session as i64)
    }

    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.delete_where("ts_us < ?1", ts_us)
    }

    /// FTS5 `optimize` merges the index into one segment and drops deleted
    /// documents; `VACUUM` rebuilds the database file from live pages only
    /// (freelist pages with deleted text are gone); `wal_checkpoint(TRUNCATE)`
    /// before and after empties the WAL file (VACUUM writes through it).
    fn reclaim(&self) -> Result<()> {
        self.readers.lock().unwrap().clear();
        let c = self.writer.lock().unwrap();
        c.execute_batch(
            "INSERT INTO events_fts (events_fts) VALUES ('optimize');
             PRAGMA wal_checkpoint(TRUNCATE);
             VACUUM;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        Ok(())
    }

    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let sql = format!("SELECT {COLS} FROM events ORDER BY session_id, seq");
        self.read(|c| {
            let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                buf.clear();
                crate::export_line(&row_of(r)?, &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
            Ok((n, bytes))
        })
    }

    fn disk_bytes(&self) -> Result<u64> {
        Ok(self.data_paths()?.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum())
    }

    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::rss_anon())
    }

    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = Vec::new();
        if let Some(dir) = self.path.parent() {
            crate::sys::files_under(dir, &mut v);
        }
        Ok(v)
    }
}
