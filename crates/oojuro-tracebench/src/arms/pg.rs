//! Postgres 17 baseline arm (`pg`, `pg:sync=off`, `pg:part=day`).
//!
//! Server settings: `scripts/tracedb/pg.sh` (every one explicit); the why of
//! each setting and of the schema below: `docs/TRACEBENCH_BASELINES.md`.
//!
//! One row per event in `events`; a status revision is an UPDATE of the
//! call's row in the same transaction as the rest of its batch. Search is a
//! stored `tsvector` (generated, NULL for non-indexed kinds) under a
//! `btree_gin` GIN index on `(agent_id, tsv)`, ranked by `ts_rank` with an
//! OR `tsquery`.
//!
//! Params: `sync=off` sets `synchronous_commit=off` per session
//! (NON-DURABLE); `part=day` makes `events` range-partitioned by day on
//! `ts_us` so purges drop whole partitions; `reset=off` reuses a running
//! container (drops and recreates the schema instead of a fresh initdb);
//! `existing=on` starts the container on the data directory a previous run
//! left (`pg.sh start`, no initdb, no schema change: reopen and crash
//! recovery); `keep=on` leaves the container running when the store is
//! dropped (the `ycsb` suite stops it itself).
use super::Params;
use crate::events::{Event, ST_ERROR, TOOL_CALL};
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore};
use postgres::{
    Client, NoTls, Statement,
    binary_copy::BinaryCopyInWriter,
    error::SqlState,
    types::{IsNull, ToSql, Type, private::BytesMut, to_sql_checked},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

const CONTAINER: &str = "oojuro-tracedb-pg";
const DAY_US: i64 = 86_400_000_000;
/// Characters of `text` that go into the tsvector (Postgres caps a tsvector
/// at 1 MB of lexemes; 0.003% of base1m events are longer than this).
const TSV_PREFIX_CHARS: usize = 131_072;
/// Daily partitions pre-created for `part=day` (the default partition takes
/// anything outside): 2026-08-25 .. 2026-11-02 covers every tier's clock.
const PART_FIRST_DAY: i64 = 1_787_616_000_000_000 / DAY_US;
const PART_DAYS: i64 = 70;

// ---------------------------------------------------------------- shared helpers
// (used by the sqlite and ch arms too; kept here so mod.rs stays untouched)

/// Repo root (scripts and container data live under it).
pub(crate) fn repo_root() -> PathBuf {
    std::env::var_os("TRACEDB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")))
}

/// Run `scripts/tracedb/<script> <arg>`; returns stdout.
pub(crate) fn run_script(script: &str, arg: &str) -> Result<String> {
    let path = repo_root().join("scripts/tracedb").join(script);
    let out = Command::new("bash").arg(&path).arg(arg).output()?;
    if !out.status.success() {
        return Err(format!(
            "{} {arg} failed: {}{}",
            path.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Fold status revisions into their calls when the call is in the same set
/// (a batch, or a bulk-load chunk): returns (events to write with the latest
/// status applied, revisions whose call was written earlier). Only the last
/// revision per call is kept, in arrival order.
pub(crate) fn fold<'a>(events: impl Iterator<Item = &'a Event<'a>>) -> (Vec<Event<'a>>, Vec<Event<'a>>) {
    let mut rows: Vec<Event<'a>> = Vec::new();
    let mut pos: HashMap<u64, usize> = HashMap::new();
    let mut revs: Vec<Event<'a>> = Vec::new();
    let mut rev_pos: HashMap<u64, usize> = HashMap::new();
    for e in events {
        if e.revises == 0 {
            pos.insert(e.id, rows.len());
            rows.push(*e);
        } else if let Some(&i) = pos.get(&e.revises) {
            let r = &mut rows[i];
            r.status = e.status;
            r.exit_code = e.exit_code;
            r.duration_us = e.duration_us;
        } else if let Some(&i) = rev_pos.get(&e.revises) {
            revs[i] = *e;
        } else {
            rev_pos.insert(e.revises, revs.len());
            revs.push(*e);
        }
    }
    (rows, revs)
}

/// Query words for OR full-text search: maximal runs of characters for which
/// `word_char` holds, lowercased, deduplicated in order, at most `max`.
pub(crate) fn query_words(q: &str, word_char: impl Fn(char) -> bool, ascii_lower_only: bool, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in q.split(|c: char| !word_char(c)).filter(|w| !w.is_empty()) {
        let w = if ascii_lower_only { w.to_ascii_lowercase() } else { w.to_lowercase() };
        if !out.contains(&w) {
            out.push(w);
            if out.len() == max {
                break;
            }
        }
    }
    out
}

pub(crate) fn i32_of(v: u32, what: &str) -> Result<i32> {
    i32::try_from(v).map_err(|_| format!("{what} {v} does not fit int4").into())
}

/// Run a write once more if Postgres aborted it as a deadlock victim
/// (40P01; the transaction was rolled back, so repeating it is safe).
/// Sorted revision updates make this rare; batched multi-writer commits of
/// the `ycsb` suite are where it could happen.
fn retry_deadlock(mut f: impl FnMut() -> Result<()>) -> Result<()> {
    match f() {
        Err(e)
            if e.downcast_ref::<postgres::Error>().and_then(postgres::Error::code)
                == Some(&SqlState::T_R_DEADLOCK_DETECTED) =>
        {
            f()
        }
        r => r,
    }
}

// ---------------------------------------------------------------- the arm

/// `json` values go over the wire as their exact text (binary COPY and
/// parameters); `jsonb` would normalise whitespace and key order.
#[derive(Debug)]
struct Json<'a>(&'a str);

impl ToSql for Json<'_> {
    fn to_sql(&self, _ty: &Type, out: &mut BytesMut) -> std::result::Result<IsNull, crate::Error> {
        out.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }
    fn accepts(ty: &Type) -> bool {
        *ty == Type::JSON
    }
    to_sql_checked!();
}

struct Conn {
    client: Client,
    stmts: HashMap<String, Statement>,
}

impl Conn {
    fn prep(&mut self, sql: &str) -> Result<Statement> {
        if let Some(s) = self.stmts.get(sql) {
            return Ok(s.clone());
        }
        let s = self.client.prepare(sql)?;
        self.stmts.insert(sql.to_string(), s.clone());
        Ok(s)
    }
}

pub struct Pg {
    conninfo: String,
    sync_off: bool,
    part_day: bool,
    idle: Mutex<Vec<Conn>>,
    container_id: String,
    data_dir: PathBuf,
    keep: bool,
}

const COLS: &str = "id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, \
                    ts_us, duration_us, tool, coalesce(args::text, ''), text";

const INSERT_COLS: &str =
    "id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, ts_us, duration_us, tool, args, text";

/// One statement per append: insert the batch's events (unnest of typed
/// arrays: one prepared statement for any batch size) and apply its status
/// revisions to earlier calls. Single statement = single implicit
/// transaction = one durable commit.
const APPEND_SQL: &str = "WITH ins AS (
  INSERT INTO events (id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, ts_us, duration_us, tool, args, text)
  SELECT u.id, u.session_id, u.agent_id, u.turn, u.seq, u.parent, u.kind, u.status, u.exit_code, u.tokens_in, u.tokens_out,
         u.ts_us, u.duration_us, u.tool, nullif(u.args, '')::json, u.text
  FROM unnest($1::int8[], $2::int8[], $3::int4[], $4::int4[], $5::int4[], $6::int8[], $7::int2[], $8::int2[], $9::int4[],
              $10::int4[], $11::int4[], $12::int8[], $13::int8[], $14::text[], $15::text[], $16::text[])
       AS u(id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, ts_us, duration_us, tool, args, text)
), upd AS (
  UPDATE events e SET status = r.status, exit_code = r.exit_code, duration_us = r.duration_us
  FROM unnest($17::int8[], $18::int2[], $19::int4[], $20::int8[]) AS r(id, status, exit_code, duration_us)
  WHERE e.id = r.id
)
SELECT 1";

const UPDATE_SQL: &str = "UPDATE events e SET status = r.status, exit_code = r.exit_code, duration_us = r.duration_us
  FROM unnest($1::int8[], $2::int2[], $3::int4[], $4::int8[]) AS r(id, status, exit_code, duration_us)
  WHERE e.id = r.id";

fn row_of(r: &postgres::Row) -> Row {
    Row {
        id: r.get::<_, i64>(0) as u64,
        session: r.get::<_, i64>(1) as u64,
        agent: r.get::<_, i32>(2) as u32,
        turn: r.get::<_, i32>(3) as u32,
        seq: r.get::<_, i32>(4) as u32,
        parent: r.get::<_, i64>(5) as u64,
        kind: r.get::<_, i16>(6) as u8,
        status: r.get::<_, i16>(7) as u8,
        exit_code: r.get(8),
        tokens_in: r.get::<_, i32>(9) as u32,
        tokens_out: r.get::<_, i32>(10) as u32,
        ts_us: r.get(11),
        duration_us: r.get(12),
        tool: r.get(13),
        args: r.get(14),
        text: r.get(15),
    }
}

fn schema(part_day: bool) -> String {
    // Fixed-width columns first (no alignment padding), then the varlenas.
    let mut s = format!(
        "CREATE EXTENSION IF NOT EXISTS btree_gin;
DROP TABLE IF EXISTS events CASCADE;
CREATE TABLE events (
  id bigint NOT NULL,
  session_id bigint NOT NULL,
  parent bigint NOT NULL,
  ts_us bigint NOT NULL,
  duration_us bigint NOT NULL,
  agent_id integer NOT NULL,
  turn integer NOT NULL,
  seq integer NOT NULL,
  tokens_in integer NOT NULL,
  tokens_out integer NOT NULL,
  exit_code integer,
  kind smallint NOT NULL,
  status smallint NOT NULL,
  tool text NOT NULL,
  args json COMPRESSION lz4,
  text text COMPRESSION lz4 NOT NULL,
  tsv tsvector COMPRESSION lz4 GENERATED ALWAYS AS (
    CASE WHEN kind IN (1, 3, 4, 5) THEN to_tsvector('english'::regconfig, left(text, {TSV_PREFIX_CHARS})) END
  ) STORED,
  PRIMARY KEY ({pk})
) {part} WITH (fillfactor = 90);
CREATE INDEX events_session_seq ON events (session_id, seq);
CREATE INDEX events_kind_ts ON events (kind, ts_us);
CREATE INDEX events_ts_id ON events (ts_us, id);
CREATE INDEX events_search ON events USING gin (agent_id, tsv);
",
        pk = if part_day { "id, ts_us" } else { "id" },
        part = if part_day { "PARTITION BY RANGE (ts_us)" } else { "" },
    );
    if part_day {
        // fillfactor is a per-partition storage parameter.
        s = s.replace(") PARTITION BY RANGE (ts_us) WITH (fillfactor = 90);", ") PARTITION BY RANGE (ts_us);");
        for d in 0..PART_DAYS {
            let day = PART_FIRST_DAY + d;
            s.push_str(&format!(
                "CREATE TABLE events_d{day} PARTITION OF events FOR VALUES FROM ({}) TO ({}) WITH (fillfactor = 90);\n",
                day * DAY_US,
                (day + 1) * DAY_US
            ));
        }
        s.push_str("CREATE TABLE events_dflt PARTITION OF events DEFAULT WITH (fillfactor = 90);\n");
    }
    s
}

pub fn open(_work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let sync_off = !params.flag("sync", true);
    let part_day = params.get("part") == Some("day");
    let existing = params.flag("existing", false);
    if existing {
        run_script("pg.sh", "start")?;
    } else if params.flag("reset", true) {
        run_script("pg.sh", "reset")?;
    }
    let port = std::env::var("TRACEDB_PG_PORT").unwrap_or_else(|_| "56510".into());
    let conninfo = format!("host=127.0.0.1 port={port} user=tracebench password=tracebench dbname=tracebench");
    let container_id = crate::sys::container_id(CONTAINER).ok_or("pg container not running")?;
    let data_dir = PathBuf::from(run_script("pg.sh", "path")?);
    let keep = params.flag("keep", false);
    let pg = Pg { conninfo, sync_off, part_day, idle: Mutex::new(Vec::new()), container_id, data_dir, keep };
    if !existing {
        pg.with(|c| Ok(c.client.batch_execute(&schema(part_day))?))?;
    }
    Ok(Box::new(pg))
}

impl Pg {
    fn connect(&self) -> Result<Conn> {
        let mut client = Client::connect(&self.conninfo, NoTls)?;
        if self.sync_off {
            client.batch_execute("SET synchronous_commit = off")?;
        }
        Ok(Conn { client, stmts: HashMap::new() })
    }

    /// Run `f` on a pooled connection (one per concurrent caller).
    fn with<T>(&self, f: impl FnOnce(&mut Conn) -> Result<T>) -> Result<T> {
        let pooled = self.idle.lock().unwrap().pop();
        let mut c = match pooled {
            Some(c) => c,
            None => self.connect()?,
        };
        let r = f(&mut c);
        if !c.client.is_closed() {
            self.idle.lock().unwrap().push(c);
        }
        r
    }

    fn rows(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        self.with(|c| {
            let st = c.prep(sql)?;
            Ok(c.client.query(&st, params)?.iter().map(row_of).collect())
        })
    }

    /// Revision columns, sorted by the revised call's id: concurrent
    /// transactions that update several rows then lock them in the same
    /// order and cannot deadlock (`fold` leaves at most one revision per
    /// call, so the order does not change the result).
    fn revision_arrays(revs: &[Event]) -> (Vec<i64>, Vec<i16>, Vec<Option<i32>>, Vec<i64>) {
        let mut revs = revs.to_vec();
        revs.sort_by_key(|r| r.revises);
        (
            revs.iter().map(|r| r.revises as i64).collect(),
            revs.iter().map(|r| r.status as i16).collect(),
            revs.iter().map(|r| r.exit_code).collect(),
            revs.iter().map(|r| r.duration_us).collect(),
        )
    }
}

/// WHERE clause + parameters for a [`Filter`].
fn filter_sql(f: &Filter) -> (String, Vec<Box<dyn ToSql + Sync>>) {
    let mut w = vec!["ts_us >= $1".to_string(), "ts_us < $2".to_string()];
    let mut p: Vec<Box<dyn ToSql + Sync>> = vec![Box::new(f.from_us), Box::new(f.to_us)];
    if let Some(a) = f.agent {
        p.push(Box::new(a as i32));
        w.push(format!("agent_id = ${}", p.len()));
    }
    if let Some(k) = f.kind {
        p.push(Box::new(k as i16));
        w.push(format!("kind = ${}", p.len()));
    }
    if let Some(t) = &f.tool {
        p.push(Box::new(t.clone()));
        w.push(format!("tool = ${}", p.len()));
    }
    if let Some(s) = f.status {
        p.push(Box::new(s as i16));
        w.push(format!("status = ${}", p.len()));
    }
    (w.join(" AND "), p)
}

impl TraceStore for Pg {
    fn name(&self) -> String {
        let mut n = "pg17".to_string();
        if self.part_day {
            n.push_str(" part=day");
        }
        if self.sync_off {
            n.push_str(" synchronous_commit=off NON-DURABLE");
        }
        n.push_str(" [disk_bytes excl. WAL]");
        n
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        let (rows, revs) = fold(batch.iter());
        let ids: Vec<i64> = rows.iter().map(|e| e.id as i64).collect();
        let sessions: Vec<i64> = rows.iter().map(|e| e.session as i64).collect();
        let agents = rows.iter().map(|e| i32_of(e.agent, "agent")).collect::<Result<Vec<i32>>>()?;
        let turns = rows.iter().map(|e| i32_of(e.turn, "turn")).collect::<Result<Vec<i32>>>()?;
        let seqs = rows.iter().map(|e| i32_of(e.seq, "seq")).collect::<Result<Vec<i32>>>()?;
        let parents: Vec<i64> = rows.iter().map(|e| e.parent as i64).collect();
        let kinds: Vec<i16> = rows.iter().map(|e| e.kind as i16).collect();
        let statuses: Vec<i16> = rows.iter().map(|e| e.status as i16).collect();
        let exits: Vec<Option<i32>> = rows.iter().map(|e| e.exit_code).collect();
        let tin = rows.iter().map(|e| i32_of(e.tokens_in, "tokens_in")).collect::<Result<Vec<i32>>>()?;
        let tout = rows.iter().map(|e| i32_of(e.tokens_out, "tokens_out")).collect::<Result<Vec<i32>>>()?;
        let ts: Vec<i64> = rows.iter().map(|e| e.ts_us).collect();
        let dur: Vec<i64> = rows.iter().map(|e| e.duration_us).collect();
        let tools: Vec<&str> = rows.iter().map(|e| e.tool).collect();
        let args: Vec<&str> = rows.iter().map(|e| e.args).collect();
        let texts: Vec<&str> = rows.iter().map(|e| e.text).collect();
        let (rid, rst, rex, rdu) = Self::revision_arrays(&revs);
        retry_deadlock(|| {
            self.with(|c| {
                let st = c.prep(APPEND_SQL)?;
                c.client.execute(
                    &st,
                    &[
                        &ids, &sessions, &agents, &turns, &seqs, &parents, &kinds, &statuses, &exits, &tin, &tout,
                        &ts, &dur, &tools, &args, &texts, &rid, &rst, &rex, &rdu,
                    ],
                )?;
                Ok(())
            })
        })
    }

    /// One transaction per chunk: binary COPY of the chunk's events (with
    /// in-chunk revisions folded in) + one UPDATE for revisions of calls
    /// loaded by an earlier chunk.
    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        let (rows, revs) = fold(batches.iter().flatten());
        let (rid, rst, rex, rdu) = Self::revision_arrays(&revs);
        retry_deadlock(|| {
            self.with(|c| {
                let upd = c.prep(UPDATE_SQL)?;
                let mut tx = c.client.transaction()?;
                let sink = tx.copy_in(&format!("COPY events ({INSERT_COLS}) FROM STDIN (FORMAT binary)"))?;
                let types = [
                    Type::INT8,
                    Type::INT8,
                    Type::INT4,
                    Type::INT4,
                    Type::INT4,
                    Type::INT8,
                    Type::INT2,
                    Type::INT2,
                    Type::INT4,
                    Type::INT4,
                    Type::INT4,
                    Type::INT8,
                    Type::INT8,
                    Type::TEXT,
                    Type::JSON,
                    Type::TEXT,
                ];
                let mut w = BinaryCopyInWriter::new(sink, &types);
                for e in &rows {
                    let args = (!e.args.is_empty()).then_some(Json(e.args));
                    w.write(&[
                        &(e.id as i64),
                        &(e.session as i64),
                        &i32_of(e.agent, "agent")?,
                        &i32_of(e.turn, "turn")?,
                        &i32_of(e.seq, "seq")?,
                        &(e.parent as i64),
                        &(e.kind as i16),
                        &(e.status as i16),
                        &e.exit_code,
                        &i32_of(e.tokens_in, "tokens_in")?,
                        &i32_of(e.tokens_out, "tokens_out")?,
                        &e.ts_us,
                        &e.duration_us,
                        &e.tool,
                        &args,
                        &e.text,
                    ])?;
                }
                w.finish()?;
                if !rid.is_empty() {
                    tx.execute(&upd, &[&rid, &rst, &rex, &rdu])?;
                }
                tx.commit()?;
                Ok(())
            })
        })
    }

    fn finish_load(&self) -> Result<()> {
        self.with(|c| {
            // Settle the bulk load: visibility map + hint bits, GIN pending
            // list merged into the index, planner statistics, then a
            // checkpoint so the read suites do not pay for it.
            c.client.batch_execute("VACUUM (ANALYZE) events")?;
            c.client.batch_execute("CHECKPOINT")?;
            Ok(())
        })
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        let sql = format!("SELECT {COLS} FROM events WHERE id = $1");
        Ok(self.rows(&sql, &[&(id as i64)])?.into_iter().next())
    }

    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        // Byte slice (may cut a UTF-8 sequence), so slice the UTF-8 bytes.
        let from = (start.min(i32::MAX as usize - 1) + 1) as i32;
        let len = end.saturating_sub(start).min(i32::MAX as usize) as i32;
        self.with(|c| {
            let st = c.prep("SELECT substring(convert_to(text, 'UTF8') FROM $2 FOR $3) FROM events WHERE id = $1")?;
            Ok(c.client.query_opt(&st, &[&(id as i64), &from, &len])?.map(|r| r.get::<_, Vec<u8>>(0)))
        })
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let sql = format!("SELECT {COLS} FROM events WHERE session_id = $1 ORDER BY seq");
        self.rows(&sql, &[&(session as i64)])
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let sql = format!("SELECT {COLS} FROM events WHERE ts_us >= $1 AND ts_us < $2 ORDER BY ts_us, id LIMIT $3");
        self.rows(&sql, &[&from_us, &to_us, &(limit as i64)])
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let (w, mut p) = filter_sql(f);
        p.push(Box::new(limit as i64));
        let sql = format!("SELECT id FROM events WHERE {w} ORDER BY ts_us, id LIMIT ${}", p.len());
        self.with(|c| {
            let st = c.prep(&sql)?;
            let refs: Vec<&(dyn ToSql + Sync)> = p.iter().map(|b| b.as_ref()).collect();
            Ok(c.client.query(&st, &refs)?.iter().map(|r| r.get::<_, i64>(0) as u64).collect())
        })
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        let (w, p) = filter_sql(f);
        let sql = format!("SELECT count(*) FROM events WHERE {w}");
        self.with(|c| {
            let st = c.prep(&sql)?;
            let refs: Vec<&(dyn ToSql + Sync)> = p.iter().map(|b| b.as_ref()).collect();
            Ok(c.client.query_one(&st, &refs)?.get::<_, i64>(0) as u64)
        })
    }

    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let words = query_words(query, char::is_alphanumeric, false, 64);
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let tsq = words.join(" | ");
        self.with(|c| {
            let st = c.prep(
                "SELECT id FROM events, to_tsquery('english'::regconfig, $2) AS q
                 WHERE agent_id = $1 AND tsv @@ q AND ts_us <= $3
                 ORDER BY ts_rank(tsv, q) DESC, ts_us DESC, id DESC LIMIT $4",
            )?;
            Ok(c.client
                .query(&st, &[&(agent as i32), &tsq, &as_of_us, &(k as i64)])?
                .iter()
                .map(|r| r.get::<_, i64>(0) as u64)
                .collect())
        })
    }

    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        self.with(|c| {
            let st = c.prep(
                "SELECT count(*), coalesce(sum(tokens_in), 0)::int8, coalesce(sum(tokens_out), 0)::int8,
                        count(*) FILTER (WHERE kind = 4), count(*) FILTER (WHERE kind = 4 AND status = 3),
                        coalesce(sum(octet_length(text)), 0)::int8
                 FROM events WHERE session_id = $1",
            )?;
            let r = c.client.query_one(&st, &[&(session as i64)])?;
            Ok(SessionAgg {
                events: r.get::<_, i64>(0) as u64,
                tokens_in: r.get::<_, i64>(1) as u64,
                tokens_out: r.get::<_, i64>(2) as u64,
                tool_calls: r.get::<_, i64>(3) as u64,
                tool_errors: r.get::<_, i64>(4) as u64,
                text_bytes: r.get::<_, i64>(5) as u64,
            })
        })
    }

    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        debug_assert_eq!(TOOL_CALL, 4);
        debug_assert_eq!(ST_ERROR, 3);
        let mut v = self.with(|c| {
            let st = c.prep(
                "SELECT tool, count(*), count(*) FILTER (WHERE status = 3), coalesce(sum(duration_us), 0)::int8
                 FROM events WHERE kind = 4 AND ts_us >= $1 AND ts_us < $2 GROUP BY tool",
            )?;
            Ok(c.client
                .query(&st, &[&from_us, &to_us])?
                .iter()
                .map(|r| ToolAgg {
                    tool: r.get(0),
                    calls: r.get::<_, i64>(1) as u64,
                    errors: r.get::<_, i64>(2) as u64,
                    duration_us: r.get(3),
                })
                .collect::<Vec<_>>())
        })?;
        // Byte order (the server collation is en_US.utf8, not bytewise).
        v.sort_by(|a, b| a.tool.as_bytes().cmp(b.tool.as_bytes()));
        Ok(v)
    }

    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        self.with(|c| {
            let st = c.prep(
                "SELECT session_id, sum(tokens_in::int8 + tokens_out)::int8 AS t
                 FROM events WHERE ts_us >= $1 AND ts_us < $2
                 GROUP BY session_id ORDER BY t DESC, session_id ASC LIMIT $3",
            )?;
            Ok(c.client
                .query(&st, &[&from_us, &to_us, &(n as i64)])?
                .iter()
                .map(|r| (r.get::<_, i64>(0) as u64, r.get::<_, i64>(1) as u64))
                .collect())
        })
    }

    fn delete_session(&self, session: u64) -> Result<()> {
        self.with(|c| {
            let st = c.prep("DELETE FROM events WHERE session_id = $1")?;
            c.client.execute(&st, &[&(session as i64)])?;
            Ok(())
        })
    }

    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.with(|c| {
            let mut tx = c.client.transaction()?;
            if self.part_day {
                // Whole days below the cutoff: drop the partition (no dead
                // tuples, no VACUUM needed); the boundary day is a DELETE.
                let parts: Vec<(String, i64)> = tx
                    .query(
                        "SELECT c.relname, substring(c.relname FROM 9)::int8 FROM pg_inherits i
                         JOIN pg_class c ON c.oid = i.inhrelid
                         WHERE i.inhparent = 'events'::regclass AND c.relname LIKE 'events_d%' AND c.relname <> 'events_dflt'",
                        &[],
                    )?
                    .iter()
                    .map(|r| (r.get(0), r.get(1)))
                    .collect();
                for (name, day) in parts {
                    if (day + 1) * DAY_US <= ts_us {
                        tx.batch_execute(&format!("DROP TABLE {name}"))?;
                    }
                }
            }
            tx.execute("DELETE FROM events WHERE ts_us < $1", &[&ts_us])?;
            tx.commit()?;
            Ok(())
        })
    }

    /// VACUUM FULL rewrites the table, its TOAST table and every index into
    /// new files (only live rows) and the old files are unlinked at the next
    /// checkpoint. The WAL still holds the deleted rows' original inserts
    /// (and full-page images taken by the deletes): checkpoint, switch to a
    /// new segment and checkpoint again so every segment before the new redo
    /// point is removed (`wal_recycle=off` unlinks them instead of renaming
    /// them for reuse with the old bytes inside).
    fn reclaim(&self) -> Result<()> {
        self.with(|c| {
            c.client.batch_execute("VACUUM (FULL, ANALYZE) events")?;
            c.client.batch_execute("CHECKPOINT")?;
            c.client.simple_query("SELECT pg_switch_wal()")?;
            c.client.batch_execute("CHECKPOINT")?;
            Ok(())
        })
    }

    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        self.with(|c| {
            let mut tx = c.client.transaction()?;
            let portal = tx.bind(&format!("SELECT {COLS} FROM events ORDER BY session_id, seq"), &[])?;
            loop {
                let rows = tx.query_portal(&portal, 4096)?;
                if rows.is_empty() {
                    break;
                }
                for r in &rows {
                    buf.clear();
                    crate::export_line(&row_of(r), &mut buf);
                    out.write_all(&buf)?;
                    n += 1;
                    bytes += buf.len() as u64;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok((n, bytes))
    }

    fn disk_bytes(&self) -> Result<u64> {
        self.with(|c| {
            // pg_partition_tree lists nothing for a plain table.
            let sql = if self.part_day {
                "SELECT coalesce(sum(pg_total_relation_size(relid)), 0)::int8 FROM pg_partition_tree('events')"
            } else {
                "SELECT pg_total_relation_size('events')::int8"
            };
            let r = c.client.query_one(sql, &[])?;
            Ok(r.get::<_, i64>(0) as u64)
        })
    }

    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::cgroup_mem(&self.container_id))
    }

    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = Vec::new();
        crate::sys::files_under(&self.data_dir, &mut v);
        Ok(v)
    }
}

impl Drop for Pg {
    fn drop(&mut self) {
        self.idle.lock().unwrap().clear();
        if !self.keep && std::env::var_os("TRACEDB_KEEP_CONTAINER").is_none() {
            let _ = run_script("pg.sh", "stop");
        }
    }
}
