//! OpenObserve 1.1.2 competitor arm (`openobserve`): a log store (WAL +
//! memtable, hourly parquet files, DataFusion SQL, tantivy full-text
//! index), one local-mode node in a container
//! (`scripts/tracedb/openobserve.sh`). Settings and their reasons:
//! `docs/TRACEBENCH_BASELINES.md` ("OpenObserve").
//!
//! One logs stream `events`: one record per event and one per status
//! revision (`revises` = the call's id); OpenObserve has no update. Rows
//! (get, session, time_range, export) fold the newest revision on the
//! client; filters, counts and aggregates on the latest status are one SQL
//! statement with a LEFT JOIN onto `first_value(.. ORDER BY id DESC)` per
//! call (the ClickHouse arm's argMax pattern). The stream is created with
//! its whole schema, and every record carries every field (no nulls: a
//! missing exit code is a sentinel). `_timestamp` = the event time, so
//! the hourly partitions prune time-bounded queries.
//!
//! Search: `match_all` (the stream's full-text key `body`, which holds the
//! text of indexed kinds only) OR-ed over the query words; OpenObserve has
//! no relevance function, so hits are ordered by how many query words they
//! contain (case-insensitive substring), then newest first.
//!
//! Durability: the server runs with `ZO_WAL_FSYNC_DISABLED=false`, so an
//! ingest request returns after its WAL write is fsynced; records are
//! searchable from the memtable at once. `finish_load` settles the store:
//! `PUT /node/flush` persists every memtable to parquet, and the node moves
//! the files into its storage once they are `ZO_MAX_FILE_RETENTION_TIME`
//! (600 s) old; the flushed files are not searchable until then, so it waits
//! until every record written is searchable again and no parquet is left in
//! the WAL directory (about 10 minutes; `settle=off` skips all of it).
//!
//! Unsupported: `delete_session` (no delete by query), `purge_before` (the
//! time-range delete is an asynchronous compactor job over whole files) and
//! `reclaim`.
//!
//! Recovery: after a restart (clean or SIGKILL) the node turns its WAL into
//! parquet files that stay unsearchable until it moves them into storage
//! (`ZO_MAX_FILE_RETENTION_TIME`, 600 s); opening an existing store waits
//! for that, so the ycsb open and crash-recovery times include it.
//!
//! Params: `existing=on`, `keep=on`, `reset=off` (as for the other docker
//! arms), `settle=off` (finish_load returns at once: the data stays in the
//! WAL and memtables, durable and searchable).
use super::Params;
use super::http::{Http, base64};
use super::pg::{fold, query_words, run_script};
use crate::events::{Event, ST_ERROR, TOOL_CALL, indexed_kind};
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore, unsupported};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const CONTAINER: &str = "oojuro-cmp-openobserve";
const STREAM: &str = "events";
/// The widest query range OpenObserve accepts (start 0 means "unset").
const FULL: (i64, i64) = (1, 253_402_300_799_999_999);
/// `exit_code` of an event without one (outside i32, exact in f64).
const NO_EXIT: i64 = -2_147_483_649;
/// Stay well below the ingest payload limit.
const MAX_INGEST_BYTES: usize = 8 << 20;
const BIG: usize = 100_000_000;

pub struct OpenObserve {
    http: Http,
    container_id: String,
    data_dir: PathBuf,
    keep: bool,
    settle: bool,
    /// Records written (events + revisions): what `finish_load` waits for.
    records: AtomicU64,
}

/// SQL string literal.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn record(e: &Event) -> Value {
    let (body, text) = if indexed_kind(e.kind) && e.revises == 0 { (e.text, "") } else { ("", e.text) };
    json!({
        "_timestamp": e.ts_us, "id": e.id, "session": e.session, "agent": e.agent, "turn": e.turn, "seq": e.seq,
        "parent": e.parent, "revises": e.revises, "kind": e.kind, "status": e.status,
        "exit_code": e.exit_code.map_or(NO_EXIT, |x| x as i64), "tokens_in": e.tokens_in, "tokens_out": e.tokens_out,
        "tok": e.tokens_in as u64 + e.tokens_out as u64, "ts": e.ts_us, "duration": e.duration_us,
        "tool": e.tool, "args": e.args, "body": body, "text": text,
    })
}

fn u(v: &Value, k: &str) -> u64 {
    v[k].as_u64().or_else(|| v[k].as_i64().map(|x| x as u64)).unwrap_or(0)
}

fn i(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn exit_of(v: &Value, k: &str) -> Option<i32> {
    match v[k].as_i64() {
        Some(NO_EXIT) | None => None,
        Some(x) => Some(x as i32),
    }
}

const COLS: &str = "id, session, agent, turn, seq, parent, revises, kind, status, exit_code, tokens_in, tokens_out, \
                    ts, duration, tool, args, body, text";

fn row_of(v: &Value) -> Row {
    let body = s(v, "body");
    Row {
        id: u(v, "id"),
        session: u(v, "session"),
        agent: u(v, "agent") as u32,
        turn: u(v, "turn") as u32,
        seq: u(v, "seq") as u32,
        parent: u(v, "parent"),
        kind: u(v, "kind") as u8,
        status: u(v, "status") as u8,
        exit_code: exit_of(v, "exit_code"),
        tokens_in: u(v, "tokens_in") as u32,
        tokens_out: u(v, "tokens_out") as u32,
        ts_us: i(v, "ts"),
        duration_us: i(v, "duration"),
        tool: s(v, "tool").to_string(),
        args: s(v, "args").to_string(),
        text: if body.is_empty() { s(v, "text") } else { body }.to_string(),
    }
}

/// call id -> (revision id, status, exit code, duration) of its newest revision.
type Latest = HashMap<u64, (u64, u8, Option<i32>, i64)>;

fn note_revision(m: &mut Latest, v: &Value) {
    let (call, id) = (u(v, "revises"), u(v, "id"));
    if m.get(&call).is_none_or(|old| old.0 < id) {
        m.insert(call, (id, u(v, "status") as u8, exit_of(v, "exit_code"), i(v, "duration")));
    }
}

fn apply(rows: &mut [Row], m: &Latest) {
    for r in rows {
        if let Some(&(_, st, ex, du)) = m.get(&r.id) {
            r.status = st;
            r.exit_code = ex;
            r.duration_us = du;
        }
    }
}

/// Conditions of a filter on events (not its status), columns prefixed `p`.
fn cond(f: &Filter, p: &str) -> String {
    let mut w = vec![format!("{p}revises = 0"), format!("{p}ts >= {}", f.from_us), format!("{p}ts < {}", f.to_us)];
    if let Some(a) = f.agent {
        w.push(format!("{p}agent = {a}"));
    }
    if let Some(k) = f.kind {
        w.push(format!("{p}kind = {k}"));
    }
    if let Some(t) = &f.tool {
        w.push(format!("{p}tool = {}", lit(t)));
    }
    w.join(" AND ")
}

/// Newest revision per call among the calls matching `calls` (unprefixed
/// conditions on events).
fn latest_sql(calls: &str) -> String {
    format!(
        "SELECT revises, first_value(status ORDER BY id DESC) AS st, first_value(exit_code ORDER BY id DESC) AS ex, \
         first_value(duration ORDER BY id DESC) AS du FROM \"{STREAM}\" \
         WHERE revises > 0 AND revises IN (SELECT id FROM \"{STREAM}\" WHERE kind = {TOOL_CALL} AND {calls}) GROUP BY revises"
    )
}

const LATEST_STATUS: &str = "(CASE WHEN l.revises IS NULL THEN e.status ELSE l.st END)";
const LATEST_DURATION: &str = "(CASE WHEN l.revises IS NULL THEN e.duration ELSE l.du END)";

pub fn open(_work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let existing = params.flag("existing", false);
    if existing {
        run_script("openobserve.sh", "start")?;
    } else if params.flag("reset", true) {
        run_script("openobserve.sh", "reset")?;
    }
    let port = std::env::var("TRACEDB_OO_PORT").unwrap_or_else(|_| "27540".into());
    let user = std::env::var("TRACEDB_OO_USER").unwrap_or_else(|_| "tracebench@example.com".into());
    let pass = std::env::var("TRACEDB_OO_PASSWORD").unwrap_or_else(|_| "Tracebench-local-0nly".into());
    let auth = format!("Authorization: Basic {}\r\n", base64(format!("{user}:{pass}").as_bytes()));
    let http = Http::new(format!("127.0.0.1:{port}"), auth);
    let container_id = crate::sys::container_id(CONTAINER).ok_or("openobserve container not running")?;
    let data_dir = PathBuf::from(run_script("openobserve.sh", "path")?);
    let oo = OpenObserve {
        http,
        container_id,
        data_dir,
        keep: params.flag("keep", false),
        settle: params.flag("settle", true),
        records: AtomicU64::new(0),
    };
    if existing {
        // After a restart the node replays its WAL into parquet files that
        // are not searchable until it moves them into storage (once they are
        // ZO_MAX_FILE_RETENTION_TIME old): acknowledged records would read
        // as missing. Open waits until nothing is pending and a query
        // answers (part of the measured open / recovery time, at most 20 min).
        let t0 = Instant::now();
        let n = loop {
            match oo.count_all() {
                Ok(n) if oo.wal_pending() == 0 => break n,
                Err(e) if t0.elapsed() > Duration::from_secs(1200) => return Err(e),
                _ if t0.elapsed() > Duration::from_secs(1200) => {
                    return Err(format!("openobserve: {} WAL files not moved 20 min after restart", oo.wal_pending()).into());
                }
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        oo.records.store(n, Ordering::Relaxed);
    } else {
        // the whole schema up front (queries on an empty stream need it);
        // full-text key `body`; bloom filters (and OpenObserve's secondary
        // index) on the point-lookup columns
        let ints = [
            "id", "session", "agent", "turn", "seq", "parent", "revises", "kind", "status", "exit_code", "tokens_in",
            "tokens_out", "tok", "ts", "duration",
        ];
        let mut fields: Vec<Value> = ints.iter().map(|n| json!({"name": n, "type": "Int64"})).collect();
        fields.extend(["tool", "args", "body", "text"].iter().map(|n| json!({"name": n, "type": "Utf8"})));
        let settings = json!({"fields": fields, "settings": {"full_text_search_keys": ["body"],
                              "bloom_filter_fields": ["id", "session", "revises"]}});
        oo.http.json("POST", &format!("/api/default/streams/{STREAM}?type=logs"), Some(&settings))?;
    }
    Ok(Box::new(oo))
}

impl OpenObserve {
    /// Rows of one SQL query over `_timestamp` in `[start, end)`.
    fn sql(&self, sql: &str, (start, end): (i64, i64), size: usize) -> Result<Vec<Value>> {
        let body = json!({"query": {"sql": sql, "start_time": start.max(1), "end_time": end.max(start.max(1) + 1),
                                    "from": 0, "size": size}});
        let mut v = self.http.json("POST", "/api/default/_search", Some(&body))?;
        match v["hits"].take() {
            Value::Array(a) => Ok(a),
            _ => Err(format!("openobserve: no hits in {}", sql).into()),
        }
    }

    fn scalar(&self, sql: &str, range: (i64, i64)) -> Result<u64> {
        Ok(self.sql(sql, range, 1)?.first().map_or(0, |h| u(h, "c")))
    }

    fn count_all(&self) -> Result<u64> {
        self.scalar(&format!("SELECT count(*) AS c FROM \"{STREAM}\""), FULL)
    }

    /// Files not yet in storage: raw WAL segments (memtables not persisted
    /// yet) and parquet waiting in the WAL directory for the move.
    fn wal_pending(&self) -> usize {
        let mut v = Vec::new();
        crate::sys::files_under(&self.data_dir.join("wal/logs"), &mut v);
        crate::sys::files_under(&self.data_dir.join("wal/files"), &mut v);
        v.iter().filter(|p| p.extension().is_some_and(|e| e == "parquet" || e == "wal")).count()
    }

    fn ingest(&self, events: &[Event]) -> Result<()> {
        let mut start = 0;
        while start < events.len() {
            let mut body = vec![b'['];
            let mut end = start;
            while end < events.len() && (end == start || body.len() < MAX_INGEST_BYTES) {
                if end > start {
                    body.push(b',');
                }
                serde_json::to_writer(&mut body, &record(&events[end]))?;
                end += 1;
            }
            body.push(b']');
            let target = format!("/api/default/{STREAM}/_json");
            let v = self.http.json_raw("POST", &target, "application/json", &body)?;
            let st = &v["status"][0];
            let n = (end - start) as u64;
            if u(st, "successful") != n || u(st, "failed") != 0 {
                return Err(format!("openobserve: ingested {} of {n} records: {v}", st["successful"]).into());
            }
            self.records.fetch_add(n, Ordering::Relaxed);
            start = end;
        }
        Ok(())
    }

    fn write<'a>(&self, events: impl Iterator<Item = &'a Event<'a>>) -> Result<()> {
        // a revision of a call in the same set is folded into the call
        let (mut rows, revs) = fold(events);
        rows.extend(revs);
        self.ingest(&rows)
    }

    /// Newest revisions of `calls`.
    fn latest(&self, calls: &[u64]) -> Result<Latest> {
        let mut m = Latest::new();
        for chunk in calls.chunks(1000) {
            let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(", ");
            let sql = format!("SELECT id, revises, status, exit_code, duration FROM \"{STREAM}\" WHERE revises IN ({ids})");
            for h in self.sql(&sql, FULL, BIG)? {
                note_revision(&mut m, &h);
            }
        }
        Ok(m)
    }

    /// Ids (or a count) of events matching `f`, the status condition on the
    /// latest status (a join onto the newest revision of each call).
    fn status_sql(f: &Filter, st: u8, what: &str, tail: &str) -> String {
        format!(
            "SELECT {what} FROM \"{STREAM}\" AS e LEFT JOIN ({}) AS l ON e.id = l.revises \
             WHERE {} AND {LATEST_STATUS} = {st} {tail}",
            latest_sql(&cond(f, "")),
            cond(f, "e.")
        )
    }

    /// Allocated bytes under `sub`.
    fn dir(&self, sub: &str) -> u64 {
        crate::sys::dir_alloc_bytes(&self.data_dir.join(sub))
    }
}

impl TraceStore for OpenObserve {
    fn name(&self) -> String {
        "openobserve 1.1.2 (local mode, WAL fsync per ingest request, match_all full text)".into()
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        self.write(batch.iter())
    }

    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        self.write(batches.iter().flatten())
    }

    /// Settle: persist every memtable to parquet (`PUT /node/flush`), then
    /// wait until the node has moved the files into storage and every record
    /// written is searchable again (at most 20 min).
    fn finish_load(&self) -> Result<()> {
        if !self.settle {
            return Ok(());
        }
        self.http.json("PUT", "/node/flush", None)?;
        let want = self.records.load(Ordering::Relaxed);
        let t0 = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let n = self.count_all()?;
            if n >= want && self.wal_pending() == 0 {
                return Ok(());
            }
            if t0.elapsed() > Duration::from_secs(1200) {
                return Err(format!(
                    "openobserve: {n} of {want} records searchable, {} files not moved, after 20 min",
                    self.wal_pending()
                )
                .into());
            }
        }
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        let sql = format!("SELECT {COLS} FROM \"{STREAM}\" WHERE id = {id} OR revises = {id}");
        let (mut row, mut m) = (None, Latest::new());
        for h in self.sql(&sql, FULL, BIG)? {
            if u(&h, "revises") == 0 {
                if u(&h, "id") == id {
                    row = Some(row_of(&h));
                }
            } else {
                note_revision(&mut m, &h);
            }
        }
        let mut rows: Vec<Row> = row.into_iter().collect();
        apply(&mut rows, &m);
        Ok(rows.pop())
    }

    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let sql = format!("SELECT body, text FROM \"{STREAM}\" WHERE id = {id} AND revises = 0");
        Ok(self.sql(&sql, FULL, 1)?.first().map(|h| {
            let b = s(h, "body");
            super::docjson::slice(if b.is_empty() { s(h, "text") } else { b }, start, end)
        }))
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let sql = format!("SELECT {COLS} FROM \"{STREAM}\" WHERE session = {session}");
        let (mut rows, mut m) = (Vec::new(), Latest::new());
        for h in self.sql(&sql, FULL, BIG)? {
            if u(&h, "revises") == 0 {
                rows.push(row_of(&h));
            } else {
                note_revision(&mut m, &h);
            }
        }
        apply(&mut rows, &m);
        rows.sort_by_key(|r| (r.seq, r.id));
        Ok(rows)
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        if from_us >= to_us || limit == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {COLS} FROM \"{STREAM}\" WHERE revises = 0 AND ts >= {from_us} AND ts < {to_us} ORDER BY ts, id LIMIT {limit}"
        );
        let mut rows: Vec<Row> = self.sql(&sql, (from_us, to_us), limit)?.iter().map(row_of).collect();
        let calls: Vec<u64> = rows.iter().filter(|r| r.kind == TOOL_CALL).map(|r| r.id).collect();
        if !calls.is_empty() {
            apply(&mut rows, &self.latest(&calls)?);
        }
        Ok(rows)
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        if f.from_us >= f.to_us || limit == 0 {
            return Ok(Vec::new());
        }
        let hits = match f.status {
            None => {
                let sql = format!("SELECT id FROM \"{STREAM}\" WHERE {} ORDER BY ts, id LIMIT {limit}", cond(f, ""));
                self.sql(&sql, (f.from_us, f.to_us), limit)?
            }
            Some(st) => {
                let sql = Self::status_sql(f, st, "e.id AS id", &format!("ORDER BY e.ts, e.id LIMIT {limit}"));
                self.sql(&sql, FULL, limit)?
            }
        };
        Ok(hits.iter().map(|h| u(h, "id")).collect())
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        if f.from_us >= f.to_us {
            return Ok(0);
        }
        match f.status {
            None => self.scalar(&format!("SELECT count(*) AS c FROM \"{STREAM}\" WHERE {}", cond(f, "")), (f.from_us, f.to_us)),
            Some(st) => self.scalar(&Self::status_sql(f, st, "count(*) AS c", ""), FULL),
        }
    }

    /// `match_all` over the full-text key (`body`), OR-ed per query word;
    /// ordered by the number of query words each hit contains, then newest.
    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let words = query_words(query, char::is_alphanumeric, false, 64);
        if words.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let any = words.iter().map(|w| format!("match_all({})", lit(w))).collect::<Vec<_>>().join(" OR ");
        let score = words
            .iter()
            .map(|w| format!("CASE WHEN str_match_ignore_case(body, {}) THEN 1 ELSE 0 END", lit(w)))
            .collect::<Vec<_>>()
            .join(" + ");
        let sql = format!(
            "SELECT id, ({score}) AS sc FROM \"{STREAM}\" WHERE agent = {agent} AND ts <= {as_of_us} AND ({any}) \
             ORDER BY sc DESC, ts DESC, id DESC LIMIT {k}"
        );
        Ok(self.sql(&sql, (FULL.0, as_of_us.saturating_add(1)), k)?.iter().map(|h| u(h, "id")).collect())
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
        if from_us >= to_us {
            return Ok(Vec::new());
        }
        let f = Filter { kind: Some(TOOL_CALL), from_us, to_us, ..Default::default() };
        let sql = format!(
            "SELECT e.tool AS tool, count(*) AS calls, sum(CASE WHEN {LATEST_STATUS} = {ST_ERROR} THEN 1 ELSE 0 END) AS errors, \
             sum({LATEST_DURATION}) AS dur FROM \"{STREAM}\" AS e LEFT JOIN ({}) AS l ON e.id = l.revises \
             WHERE {} GROUP BY e.tool",
            latest_sql(&cond(&f, "")),
            cond(&f, "e.")
        );
        let mut v: Vec<ToolAgg> = self
            .sql(&sql, FULL, BIG)?
            .iter()
            .map(|h| ToolAgg { tool: s(h, "tool").to_string(), calls: u(h, "calls"), errors: u(h, "errors"), duration_us: i(h, "dur") })
            .collect();
        v.sort_by(|a, b| a.tool.as_bytes().cmp(b.tool.as_bytes()));
        Ok(v)
    }

    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        if from_us >= to_us || n == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT session, sum(tok) AS t FROM \"{STREAM}\" WHERE revises = 0 AND ts >= {from_us} AND ts < {to_us} \
             GROUP BY session ORDER BY t DESC, session ASC LIMIT {n}"
        );
        Ok(self.sql(&sql, (from_us, to_us), n)?.iter().map(|h| (u(h, "session"), u(h, "t"))).collect())
    }

    fn delete_session(&self, _session: u64) -> Result<()> {
        unsupported("delete_session", "OpenObserve has no delete by query (only whole streams or time ranges)")
    }

    fn purge_before(&self, _ts_us: i64) -> Result<()> {
        unsupported(
            "purge_before",
            "the time-range delete (DELETE .../data_by_time_range) is an asynchronous compactor job over whole \
             files: not exact at the cutoff, not invisible on return",
        )
    }

    fn reclaim(&self) -> Result<()> {
        unsupported("reclaim", "no supported delete to make physical")
    }

    /// The newest revision of every call (one grouped query), then events
    /// in blocks of 256 sessions, each ordered by (session, seq).
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let mut m = Latest::new();
        let sql = format!(
            "SELECT revises, max(id) AS id, first_value(status ORDER BY id DESC) AS status, \
             first_value(exit_code ORDER BY id DESC) AS exit_code, first_value(duration ORDER BY id DESC) AS duration \
             FROM \"{STREAM}\" WHERE revises > 0 GROUP BY revises"
        );
        for h in self.sql(&sql, FULL, BIG)? {
            note_revision(&mut m, &h);
        }
        let max = self.scalar(&format!("SELECT max(session) AS c FROM \"{STREAM}\" WHERE revises = 0"), FULL)?;
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        let mut a = 0u64;
        while a <= max {
            let sql = format!(
                "SELECT {COLS} FROM \"{STREAM}\" WHERE revises = 0 AND session >= {a} AND session < {} ORDER BY session, seq",
                a + 256
            );
            for h in self.sql(&sql, FULL, BIG)? {
                let mut r = [row_of(&h)];
                apply(&mut r, &m);
                buf.clear();
                crate::export_line(&r[0], &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
            a += 256;
        }
        Ok((n, bytes))
    }

    /// Allocated bytes of the WAL (raw entries and flushed parquet), stream
    /// files and the SQLite metadata; result/file caches, temp files and the
    /// bundled GeoIP databases are not store data (reported in details).
    fn disk_bytes(&self) -> Result<u64> {
        Ok(self.dir("wal") + self.dir("stream") + self.dir("db"))
    }

    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::cgroup_mem(&self.container_id))
    }

    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = Vec::new();
        crate::sys::files_under(&self.data_dir, &mut v);
        Ok(v)
    }

    fn details(&self) -> Value {
        json!({
            "wal_bytes": self.dir("wal"), "stream_bytes": self.dir("stream"), "db_bytes": self.dir("db"),
            "cache_bytes": self.dir("cache"), "tmp_bytes": self.dir("tmp"),
            "data_dir_bytes": crate::sys::dir_alloc_bytes(&self.data_dir),
            "data_dir_logical_bytes": crate::sys::dir_bytes(&self.data_dir),
        })
    }
}

impl Drop for OpenObserve {
    fn drop(&mut self) {
        if !self.keep && std::env::var_os("TRACEDB_KEEP_CONTAINER").is_none() {
            let _ = run_script("openobserve.sh", "stop");
        }
    }
}
