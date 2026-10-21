//! ClickHouse baseline arm (`ch`): the Langfuse-style analytics trace store.
//!
//! Server: `scripts/tracedb/ch.sh`; schema and query settings here; reasons
//! for each in `docs/TRACEBENCH_BASELINES.md`. Talks HTTP/1.1 (keep-alive
//! pool, hand-written: std::net only) with RowBinary in both directions.
//!
//! Layout
//! - `ingest` (Null engine) receives every batch as one async insert
//!   (`async_insert=1, wait_for_async_insert=1`: the server batches small
//!   inserts from all clients into one part per flush and acks each client
//!   after its flush is written; with `fsync_after_insert=1` and
//!   `fsync_part_directory=1` the part is on disk when the client returns).
//! - `events_mv` routes original events into `events` (MergeTree, ORDER BY
//!   (session_id, ts_us, id), PARTITION BY day, ZSTD text, bloom filter on
//!   id, set index on agent, text index on text).
//! - `call_status_mv` routes status revisions into `call_status`
//!   (ReplacingMergeTree(rev) ORDER BY id). Every read takes the latest
//!   revision per call with `argMax(.., rev)` (correct before and after
//!   background merges, no FINAL) and LEFT JOINs it onto the call.
//!
//! Search: the text index (`hasAnyTokens` = OR over tokens) narrows to
//! matching rows; ClickHouse has no relevance function, so hits are ordered
//! by the number of distinct query tokens they contain, then newest first.
use super::Params;
use super::pg::{fold, run_script};
use crate::events::Event;
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore};
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

const CONTAINER: &str = "oojuro-tracedb-ch";
const DAY_US: i64 = 86_400_000_000;

// ---------------------------------------------------------------- HTTP

struct Http {
    addr: String,
    idle: Mutex<Vec<(BufReader<TcpStream>, Instant)>>,
}

enum BodyMode {
    Length(u64),
    Chunked { left: u64, done: bool, first: bool },
    Eof,
}

/// A response body (de-chunked) over a pooled connection.
struct Body<'a> {
    r: &'a mut BufReader<TcpStream>,
    mode: BodyMode,
}

fn read_line(r: &mut BufReader<TcpStream>) -> io::Result<String> {
    let mut s = String::new();
    if r.read_line(&mut s)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

impl Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.mode {
            BodyMode::Length(left) => {
                if *left == 0 {
                    return Ok(0);
                }
                let want = buf.len().min(*left as usize);
                let n = self.r.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short body"));
                }
                *left -= n as u64;
                Ok(n)
            }
            BodyMode::Eof => self.r.read(buf),
            BodyMode::Chunked { left, done, first } => loop {
                if *done {
                    return Ok(0);
                }
                if *left > 0 {
                    let want = buf.len().min(*left as usize);
                    let n = self.r.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short chunk"));
                    }
                    *left -= n as u64;
                    return Ok(n);
                }
                if !*first {
                    // CRLF after the previous chunk's data
                    let l = read_line(self.r)?;
                    if !l.is_empty() {
                        return Err(io::Error::other(format!("bad chunk terminator {l:?}")));
                    }
                }
                *first = false;
                let l = read_line(self.r)?;
                let size = l.split(';').next().unwrap_or("").trim();
                let n = u64::from_str_radix(size, 16).map_err(|_| io::Error::other(format!("bad chunk size {l:?}")))?;
                if n == 0 {
                    // trailers until the empty line
                    while !read_line(self.r)?.is_empty() {}
                    *done = true;
                    return Ok(0);
                }
                *left = n;
            },
        }
    }
}

fn url_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

impl Http {
    fn conn(&self) -> io::Result<BufReader<TcpStream>> {
        loop {
            let c = self.idle.lock().unwrap().pop();
            match c {
                // The server closes idle keep-alive connections after 60 s.
                Some((c, t)) if t.elapsed() < Duration::from_secs(30) => return Ok(c),
                Some(_) => continue,
                None => {
                    let s = TcpStream::connect(&self.addr)?;
                    s.set_nodelay(true)?;
                    return Ok(BufReader::with_capacity(1 << 16, s));
                }
            }
        }
    }

    /// POST `body` to `/?{query_string}`; `f` consumes the response body.
    /// Non-200 answers become errors carrying the server's message.
    fn post<T>(&self, qs: &str, body: &[u8], f: impl FnOnce(&mut dyn BufRead) -> Result<T>) -> Result<T> {
        let mut c = self.conn()?;
        let head = format!(
            "POST /?{qs} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        let sent = (|| -> io::Result<()> {
            let s = c.get_mut();
            s.write_all(head.as_bytes())?;
            s.write_all(body)?;
            s.flush()
        })();
        if let Err(e) = sent {
            // A pooled connection the server closed while idle: retry once on
            // a fresh one (nothing reached the server).
            let fresh = TcpStream::connect(&self.addr)?;
            fresh.set_nodelay(true)?;
            c = BufReader::with_capacity(1 << 16, fresh);
            let s = c.get_mut();
            s.write_all(head.as_bytes()).map_err(|_| e)?;
            s.write_all(body)?;
            s.flush()?;
        }
        let status = read_line(&mut c)?;
        let code: u32 = status.split_whitespace().nth(1).and_then(|x| x.parse().ok()).ok_or("bad status line")?;
        let (mut len, mut chunked, mut close) = (None, false, false);
        loop {
            let l = read_line(&mut c)?;
            if l.is_empty() {
                break;
            }
            let (k, v) = l.split_once(':').unwrap_or((&l, ""));
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "content-length" => len = v.parse::<u64>().ok(),
                "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
                "connection" => close = v.eq_ignore_ascii_case("close"),
                _ => {}
            }
        }
        let mode = if chunked {
            BodyMode::Chunked { left: 0, done: false, first: true }
        } else if let Some(n) = len {
            BodyMode::Length(n)
        } else {
            close = true;
            BodyMode::Eof
        };
        let mut body = BufReader::with_capacity(1 << 16, Body { r: &mut c, mode });
        if code != 200 {
            let mut msg = String::new();
            body.read_to_string(&mut msg).ok();
            return Err(format!("clickhouse HTTP {code}: {}", msg.trim()).into());
        }
        let out = f(&mut body)?;
        // Drain whatever the consumer left so the connection can be reused.
        io::copy(&mut body, &mut io::sink())?;
        drop(body);
        if !close {
            self.idle.lock().unwrap().push((c, Instant::now()));
        }
        Ok(out)
    }

    /// Run a statement (DDL, mutation, ...) and return the body as text.
    fn exec(&self, sql: &str) -> Result<String> {
        self.exec_with("", sql)
    }

    fn exec_with(&self, settings: &str, sql: &str) -> Result<String> {
        self.post(settings, sql.as_bytes(), |b| {
            let mut s = String::new();
            b.read_to_string(&mut s)?;
            Ok(s)
        })
    }

    /// SELECT in RowBinary; `f` parses one row.
    fn select<T>(&self, sql: &str, settings: &str, mut f: impl FnMut(&mut Rb) -> Result<T>) -> Result<Vec<T>> {
        let q = format!("{sql} FORMAT RowBinary");
        self.post(settings, q.as_bytes(), |b| {
            let mut rb = Rb { r: b };
            let mut v = Vec::new();
            while !rb.eof()? {
                v.push(f(&mut rb)?);
            }
            Ok(v)
        })
    }
}

// ---------------------------------------------------------------- RowBinary

struct Rb<'a> {
    r: &'a mut dyn BufRead,
}

impl Rb<'_> {
    fn eof(&mut self) -> io::Result<bool> {
        Ok(self.r.fill_buf()?.is_empty())
    }
    fn bytes<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.r.read_exact(&mut b)?;
        Ok(b)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.bytes()?))
    }
    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.bytes()?))
    }
    fn opt_i32(&mut self) -> io::Result<Option<i32>> {
        Ok(if self.u8()? != 0 { None } else { Some(self.i32()?) })
    }
    fn varint(&mut self) -> io::Result<u64> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn blob(&mut self) -> io::Result<Vec<u8>> {
        let n = self.varint()? as usize;
        let mut v = vec![0u8; n];
        self.r.read_exact(&mut v)?;
        Ok(v)
    }
    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8(self.blob()?)?)
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// One `ingest` row (column order of INGEST_COLS).
fn put_event(out: &mut Vec<u8>, e: &Event) {
    out.extend_from_slice(&e.id.to_le_bytes());
    out.extend_from_slice(&e.session.to_le_bytes());
    out.extend_from_slice(&e.agent.to_le_bytes());
    out.extend_from_slice(&e.turn.to_le_bytes());
    out.extend_from_slice(&e.seq.to_le_bytes());
    out.extend_from_slice(&e.parent.to_le_bytes());
    out.push(e.kind);
    out.push(e.status);
    match e.exit_code {
        Some(x) => {
            out.push(0);
            out.extend_from_slice(&x.to_le_bytes());
        }
        None => out.push(1),
    }
    out.extend_from_slice(&e.tokens_in.to_le_bytes());
    out.extend_from_slice(&e.tokens_out.to_le_bytes());
    out.extend_from_slice(&e.ts_us.to_le_bytes());
    out.extend_from_slice(&e.duration_us.to_le_bytes());
    put_str(out, e.tool);
    put_str(out, e.args);
    put_str(out, e.text);
    out.extend_from_slice(&e.revises.to_le_bytes());
}

/// Read one row in ROW_COLS order.
fn get_row(rb: &mut Rb) -> Result<Row> {
    Ok(Row {
        id: rb.u64()?,
        session: rb.u64()?,
        agent: rb.u32()?,
        turn: rb.u32()?,
        seq: rb.u32()?,
        parent: rb.u64()?,
        kind: rb.u8()?,
        status: rb.u8()?,
        exit_code: rb.opt_i32()?,
        tokens_in: rb.u32()?,
        tokens_out: rb.u32()?,
        ts_us: rb.i64()?,
        duration_us: rb.i64()?,
        tool: rb.string()?,
        args: rb.string()?,
        text: rb.string()?,
    })
}

// ---------------------------------------------------------------- schema

/// Deletes are lightweight updates (patch parts); `_force` fails loudly
/// instead of silently falling back to a mutation. `lightweight_deletes_sync
/// = 2`: the statement returns once the patch is written.
const DELETE_SETTINGS: &str = "lightweight_delete_mode=lightweight_update_force&lightweight_deletes_sync=2";

/// Durability: fsync every inserted part and its directory, and every merge
/// result. Prompt physical deletes: merged-away / mutated / dropped parts
/// leave the disk 10 s after they stop being active (the default 480 s
/// window exists to survive crashes before an un-fsynced merge result is on
/// disk; merges are fsynced here), and the cleaner polls each second instead
/// of backing off to 300 s when idle. Block number/offset columns enable
/// lightweight updates, which DELETE uses (patch parts, see delete_session).
const TABLE_SETTINGS: &str = "fsync_after_insert = 1, fsync_part_directory = 1, \
     enable_block_number_column = 1, enable_block_offset_column = 1, \
     min_compressed_bytes_to_fsync_after_merge = 1, min_rows_to_fsync_after_merge = 1, \
     old_parts_lifetime = 10, cleanup_delay_period = 1, max_cleanup_delay_period = 2, cleanup_delay_period_random_add = 0";

fn schema() -> Vec<String> {
    vec![
        format!(
            "CREATE TABLE events (
  id UInt64 CODEC(Delta, ZSTD(1)),
  session_id UInt64 CODEC(Delta, ZSTD(1)),
  agent_id UInt32 CODEC(ZSTD(1)),
  turn UInt32 CODEC(Delta, ZSTD(1)),
  seq UInt32 CODEC(Delta, ZSTD(1)),
  parent UInt64 CODEC(Delta, ZSTD(1)),
  kind UInt8 CODEC(ZSTD(1)),
  status UInt8 CODEC(ZSTD(1)),
  exit_code Nullable(Int32) CODEC(ZSTD(1)),
  tokens_in UInt32 CODEC(T64, ZSTD(1)),
  tokens_out UInt32 CODEC(T64, ZSTD(1)),
  ts_us Int64 CODEC(Delta, ZSTD(1)),
  duration_us Int64 CODEC(T64, ZSTD(1)),
  tool LowCardinality(String) CODEC(ZSTD(1)),
  args String CODEC(ZSTD(3)),
  text String CODEC(ZSTD(3)),
  INDEX idx_id id TYPE bloom_filter(0.001) GRANULARITY 1,
  INDEX idx_agent agent_id TYPE set(16) GRANULARITY 1,
  INDEX idx_text text TYPE text(tokenizer = splitByNonAlpha, preprocessor = lower(text))
) ENGINE = MergeTree
PARTITION BY intDiv(ts_us, {DAY_US})
ORDER BY (session_id, ts_us, id)
SETTINGS {TABLE_SETTINGS}"
        ),
        format!(
            "CREATE TABLE call_status (
  id UInt64 CODEC(Delta, ZSTD(1)),
  session_id UInt64 CODEC(ZSTD(1)),
  status UInt8 CODEC(ZSTD(1)),
  exit_code Nullable(Int32) CODEC(ZSTD(1)),
  duration_us Int64 CODEC(T64, ZSTD(1)),
  rev UInt64 CODEC(Delta, ZSTD(1))
) ENGINE = ReplacingMergeTree(rev)
ORDER BY id
SETTINGS {TABLE_SETTINGS}"
        ),
        "CREATE TABLE ingest (
  id UInt64, session_id UInt64, agent_id UInt32, turn UInt32, seq UInt32, parent UInt64, kind UInt8, status UInt8,
  exit_code Nullable(Int32), tokens_in UInt32, tokens_out UInt32, ts_us Int64, duration_us Int64,
  tool String, args String, text String, revises UInt64
) ENGINE = Null"
            .into(),
        "CREATE MATERIALIZED VIEW events_mv TO events AS
  SELECT id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, ts_us,
         duration_us, tool, args, text
  FROM ingest WHERE revises = 0"
            .into(),
        "CREATE MATERIALIZED VIEW call_status_mv TO call_status AS
  SELECT revises AS id, session_id, status, exit_code, duration_us, id AS rev
  FROM ingest WHERE revises != 0"
            .into(),
    ]
}

/// Columns of a Row with the latest status applied (`e` = events, `s` =
/// latest status per call or no match: s.id = 0).
const ROW_COLS: &str = "e.id, e.session_id, e.agent_id, e.turn, e.seq, e.parent, e.kind, \
     if(s.id = 0, e.status, tupleElement(s.t, 1)), if(s.id = 0, e.exit_code, tupleElement(s.t, 2)), \
     e.tokens_in, e.tokens_out, e.ts_us, if(s.id = 0, e.duration_us, tupleElement(s.t, 3)), \
     toString(e.tool), e.args, e.text";

/// Latest status of the calls among the events matching `cond`.
fn latest_status(cond: &str) -> String {
    format!(
        "(SELECT id, argMax(tuple(status, exit_code, duration_us), rev) AS t FROM call_status
          WHERE id IN (SELECT id FROM events WHERE ({cond}) AND kind = 4) GROUP BY id)"
    )
}

/// Full rows matching `cond`, ordered and limited inside (`inner`), with the
/// latest status joined on; `outer` re-establishes the order after the join.
fn rows_sql(cond: &str, inner: &str, outer: &str) -> String {
    format!(
        "SELECT {ROW_COLS} FROM (SELECT * FROM events WHERE {cond} {inner}) AS e
         LEFT JOIN {} AS s ON e.id = s.id {outer}",
        latest_status(cond)
    )
}

/// SQL string literal (ClickHouse escaping).
fn lit(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\'' => o.push_str("\\'"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\0' => o.push_str("\\0"),
            c => o.push(c),
        }
    }
    o.push('\'');
    o
}

fn filter_cond(f: &Filter) -> String {
    let mut w = vec![format!("ts_us >= {}", f.from_us), format!("ts_us < {}", f.to_us)];
    if let Some(a) = f.agent {
        w.push(format!("agent_id = {a}"));
    }
    if let Some(k) = f.kind {
        w.push(format!("kind = {k}"));
    }
    if let Some(t) = &f.tool {
        w.push(format!("tool = {}", lit(t)));
    }
    w.join(" AND ")
}

/// Ids (with ts) of events matching `f`, the status condition evaluated on
/// the latest status.
fn filter_ids_sql(f: &Filter) -> String {
    let cond = filter_cond(f);
    match f.status {
        None => format!("SELECT id, ts_us FROM events WHERE {cond}"),
        Some(st) => format!(
            "SELECT e.id AS id, e.ts_us AS ts_us FROM (SELECT id, ts_us, status FROM events WHERE {cond}) AS e
             LEFT JOIN {} AS s ON e.id = s.id
             WHERE if(s.id = 0, e.status, tupleElement(s.t, 1)) = {st}",
            latest_status(&cond)
        ),
    }
}

pub struct Ch {
    http: Http,
    container_id: String,
    data_dir: PathBuf,
    keep: bool,
}

/// Params: `reset=off` reuses a running container (drops and recreates the
/// tables); `existing=on` starts the container on the data directory a
/// previous run left (`ch.sh start`, tables kept: reopen and crash recovery);
/// `keep=on` leaves the container running when the store is dropped.
pub fn open(_work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let existing = params.flag("existing", false);
    if existing {
        run_script("ch.sh", "start")?;
    } else if params.flag("reset", true) {
        run_script("ch.sh", "reset")?;
    }
    let port = std::env::var("TRACEDB_CH_HTTP_PORT").unwrap_or_else(|_| "56520".into());
    let http = Http { addr: format!("127.0.0.1:{port}"), idle: Mutex::new(Vec::new()) };
    let container_id = crate::sys::container_id(CONTAINER).ok_or("ch container not running")?;
    let data_dir = PathBuf::from(run_script("ch.sh", "path")?);
    if !existing {
        for t in ["call_status_mv", "events_mv", "ingest", "call_status", "events"] {
            http.exec(&format!("DROP TABLE IF EXISTS {t} SYNC"))?;
        }
        for s in schema() {
            http.exec(&s)?;
        }
    }
    Ok(Box::new(Ch { http, container_id, data_dir, keep: params.flag("keep", false) }))
}

impl Ch {
    fn insert(&self, events: &[Event], async_insert: bool) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut body = Vec::with_capacity(events.iter().map(|e| e.raw_bytes() as usize + 96).sum());
        for e in events {
            put_event(&mut body, e);
        }
        let q = url_encode("INSERT INTO ingest FORMAT RowBinary");
        let qs = if async_insert {
            format!("query={q}&async_insert=1&wait_for_async_insert=1")
        } else {
            format!("query={q}&async_insert=0")
        };
        self.http.post(&qs, &body, |_| Ok(()))
    }

    fn rows(&self, sql: &str) -> Result<Vec<Row>> {
        self.http.select(sql, "", get_row)
    }

    fn scalar_u64(&self, sql: &str) -> Result<u64> {
        Ok(self.http.select(sql, "", |rb| Ok(rb.u64()?))?.into_iter().next().unwrap_or(0))
    }

    /// Parts that are no longer active (merged away, mutated, dropped) and
    /// still on disk.
    fn outdated_parts(&self) -> Result<u64> {
        self.scalar_u64(
            "SELECT toUInt64(count()) FROM system.parts WHERE database = currentDatabase()
             AND table IN ('events', 'call_status') AND NOT active",
        )
    }

    fn wait_until(&self, what: &str, max: Duration, mut done: impl FnMut() -> Result<bool>) -> Result<()> {
        let t0 = Instant::now();
        while !done()? {
            if t0.elapsed() > max {
                return Err(format!("clickhouse: timed out waiting for {what}").into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(())
    }
}

impl TraceStore for Ch {
    fn name(&self) -> String {
        "clickhouse 26.9 (MergeTree + async inserts, fsync on)".into()
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        // Same-batch revisions are folded into their call; the rest ride in
        // the same insert and land in call_status via the view.
        let (mut rows, revs) = fold(batch.iter());
        rows.extend(revs);
        self.insert(&rows, true)
    }

    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        let (mut rows, revs) = fold(batches.iter().flatten());
        rows.extend(revs);
        self.insert(&rows, false)
    }

    /// Settle: wait until background merges of the bulk load are done (the
    /// state a long-running store is in), at most 15 minutes.
    fn finish_load(&self) -> Result<()> {
        let mut quiet = 0;
        self.wait_until("merges to settle", Duration::from_secs(900), || {
            let n = self.scalar_u64("SELECT toUInt64(count()) FROM system.merges WHERE database = currentDatabase()")?;
            quiet = if n == 0 { quiet + 1 } else { 0 };
            Ok(quiet >= 4)
        })
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        Ok(self.rows(&rows_sql(&format!("id = {id}"), "", ""))?.into_iter().next())
    }

    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let len = end.saturating_sub(start);
        let sql = format!("SELECT substring(text, {}, {len}) FROM events WHERE id = {id}", start as u64 + 1);
        Ok(self.http.select(&sql, "", |rb| Ok(rb.blob()?))?.into_iter().next())
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        self.rows(&rows_sql(&format!("session_id = {session}"), "", "ORDER BY e.seq"))
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let ord = format!("ORDER BY ts_us, id LIMIT {limit}");
        self.rows(&rows_sql(&format!("ts_us >= {from_us} AND ts_us < {to_us}"), &ord, "ORDER BY e.ts_us, e.id"))
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let sql = format!("SELECT id FROM ({}) ORDER BY ts_us, id LIMIT {limit}", filter_ids_sql(f));
        self.http.select(&sql, "", |rb| Ok(rb.u64()?))
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        self.scalar_u64(&format!("SELECT toUInt64(count()) FROM ({})", filter_ids_sql(f)))
    }

    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        // splitByNonAlpha: tokens are runs of ASCII alphanumerics and
        // non-ASCII bytes; the index lowercases ASCII (preprocessor lower()).
        let words = super::pg::query_words(query, |c| !c.is_ascii() || c.is_ascii_alphanumeric(), true, 64);
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let arr = words.iter().map(|w| lit(w)).collect::<Vec<_>>().join(", ");
        let score = words.iter().map(|w| format!("hasTokenCaseInsensitive(text, {})", lit(w))).collect::<Vec<_>>().join(" + ");
        let sql = format!(
            "SELECT id FROM events
             WHERE agent_id = {agent} AND kind IN (1, 3, 4, 5) AND ts_us <= {as_of_us} AND hasAnyTokens(text, [{arr}])
             ORDER BY ({score}) DESC, ts_us DESC, id DESC LIMIT {k}"
        );
        self.http.select(&sql, "", |rb| Ok(rb.u64()?))
    }

    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        let cond = format!("session_id = {session}");
        let sql = format!(
            "SELECT toUInt64(count()), toUInt64(sum(e.tokens_in)), toUInt64(sum(e.tokens_out)),
                    toUInt64(countIf(e.kind = 4)),
                    toUInt64(countIf(e.kind = 4 AND if(s.id = 0, e.status, tupleElement(s.t, 1)) = 3)),
                    toUInt64(sum(length(e.text)))
             FROM (SELECT id, kind, status, tokens_in, tokens_out, text FROM events WHERE {cond}) AS e
             LEFT JOIN {} AS s ON e.id = s.id",
            latest_status(&cond)
        );
        let v = self.http.select(&sql, "", |rb| {
            Ok(SessionAgg {
                events: rb.u64()?,
                tokens_in: rb.u64()?,
                tokens_out: rb.u64()?,
                tool_calls: rb.u64()?,
                tool_errors: rb.u64()?,
                text_bytes: rb.u64()?,
            })
        })?;
        Ok(v.into_iter().next().unwrap_or_default())
    }

    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        let cond = format!("kind = 4 AND ts_us >= {from_us} AND ts_us < {to_us}");
        let sql = format!(
            "SELECT toString(tool), toUInt64(count()), toUInt64(countIf(st = 3)), toInt64(sum(du)) FROM (
               SELECT e.tool AS tool, if(s.id = 0, e.status, tupleElement(s.t, 1)) AS st,
                      if(s.id = 0, e.duration_us, tupleElement(s.t, 3)) AS du
               FROM (SELECT id, tool, status, duration_us FROM events WHERE {cond}) AS e
               LEFT JOIN {} AS s ON e.id = s.id)
             GROUP BY tool",
            latest_status(&cond)
        );
        let mut v = self.http.select(&sql, "", |rb| {
            Ok(ToolAgg { tool: rb.string()?, calls: rb.u64()?, errors: rb.u64()?, duration_us: rb.i64()? })
        })?;
        v.sort_by(|a, b| a.tool.as_bytes().cmp(b.tool.as_bytes()));
        Ok(v)
    }

    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        let sql = format!(
            "SELECT session_id, toUInt64(sum(toUInt64(tokens_in) + tokens_out)) AS t FROM events
             WHERE ts_us >= {from_us} AND ts_us < {to_us}
             GROUP BY session_id ORDER BY t DESC, session_id ASC LIMIT {n}"
        );
        self.http.select(&sql, "", |rb| Ok((rb.u64()?, rb.u64()?)))
    }

    /// Lightweight DELETE as a lightweight update: writes a patch part that
    /// masks the rows (`_row_exists = 0`), applied on every read. The default
    /// mode (`alter_update`) runs a mutation over every part instead: ~35 s
    /// per session on the smoke tier under this box's load. The text stays in
    /// the parts until `reclaim`.
    fn delete_session(&self, session: u64) -> Result<()> {
        self.http.exec_with(DELETE_SETTINGS, &format!("DELETE FROM call_status WHERE session_id = {session}"))?;
        self.http.exec_with(DELETE_SETTINGS, &format!("DELETE FROM events WHERE session_id = {session}"))?;
        Ok(())
    }

    /// Whole days below the cutoff: DROP PARTITION; the boundary day: a
    /// lightweight DELETE. Status rows of purged calls go first.
    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.http.exec_with(
            DELETE_SETTINGS,
            &format!("DELETE FROM call_status WHERE id IN (SELECT id FROM events WHERE ts_us < {ts_us} AND kind = 4)"),
        )?;
        let parts = self.http.select(
            "SELECT DISTINCT partition_id FROM system.parts WHERE database = currentDatabase() AND table = 'events' AND active",
            "",
            |rb| rb.string(),
        )?;
        for p in parts {
            // Patch parts (lightweight deletes) have their own partition ids.
            let Ok(day) = p.parse::<i64>() else { continue };
            if (day + 1) * DAY_US <= ts_us {
                self.http.exec(&format!("ALTER TABLE events DROP PARTITION ID {}", lit(&p)))?;
            }
        }
        self.http.exec_with(DELETE_SETTINGS, &format!("DELETE FROM events WHERE ts_us < {ts_us}"))?;
        Ok(())
    }

    /// Materialise the delete patches into the parts (APPLY PATCHES), rewrite
    /// every part that has masked rows without them (APPLY DELETED MASK),
    /// then wait until the cleaner has removed the superseded parts from disk
    /// (old_parts_lifetime = 10 s).
    fn reclaim(&self) -> Result<()> {
        for t in ["events", "call_status"] {
            self.http.exec_with("mutations_sync=2", &format!("ALTER TABLE {t} APPLY PATCHES"))?;
            self.http.exec_with("mutations_sync=2", &format!("ALTER TABLE {t} APPLY DELETED MASK"))?;
        }
        self.wait_until("outdated parts removal", Duration::from_secs(600), || Ok(self.outdated_parts()? == 0))
    }

    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let status: HashMap<u64, (u8, Option<i32>, i64)> = self
            .http
            .select(
                "SELECT id, tupleElement(t, 1), tupleElement(t, 2), tupleElement(t, 3) FROM (
                   SELECT id, argMax(tuple(status, exit_code, duration_us), rev) AS t FROM call_status GROUP BY id)",
                "",
                |rb| Ok((rb.u64()?, (rb.u8()?, rb.opt_i32()?, rb.i64()?))),
            )?
            .into_iter()
            .collect();
        let sql = "SELECT id, session_id, agent_id, turn, seq, parent, kind, status, exit_code, tokens_in, tokens_out, ts_us,
                          duration_us, toString(tool), args, text
                   FROM events ORDER BY session_id, seq FORMAT RowBinary";
        // Spill the sort to disk instead of failing inside the 3 GB cap.
        let settings = "max_bytes_before_external_sort=536870912";
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        self.http.post(settings, sql.as_bytes(), |b| {
            let mut rb = Rb { r: b };
            while !rb.eof()? {
                let mut r = get_row(&mut rb)?;
                if let Some(&(st, ex, du)) = status.get(&r.id) {
                    r.status = st;
                    r.exit_code = ex;
                    r.duration_us = du;
                }
                buf.clear();
                crate::export_line(&r, &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
            Ok(())
        })?;
        Ok((n, bytes))
    }

    fn disk_bytes(&self) -> Result<u64> {
        self.scalar_u64(
            "SELECT toUInt64(sum(bytes_on_disk)) FROM system.parts WHERE database = currentDatabase()
             AND table IN ('events', 'call_status') AND active",
        )
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

impl Drop for Ch {
    fn drop(&mut self) {
        self.http.idle.lock().unwrap().clear();
        if !self.keep && std::env::var_os("TRACEDB_KEEP_CONTAINER").is_none() {
            let _ = run_script("ch.sh", "stop");
        }
    }
}
