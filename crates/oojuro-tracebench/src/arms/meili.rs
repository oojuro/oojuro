//! Meilisearch arm (`meili`, `meili:wait=poll`): Meilisearch 1.55 in a
//! container (`scripts/tracedb/meili.sh`), one index `events`, primary key
//! `id`, talked to over HTTP/JSON.
//!
//! Writes are asynchronous tasks. `append` sends the turn batch as one
//! `PUT /indexes/events/documents` (add-or-update: new events in full, a
//! status revision as a partial document `{id, status, exit_code,
//! duration}` merged into its call) and returns only when that task has
//! **succeeded**: the scheduler (which auto-batches the tasks enqueued by all
//! writers) has committed the LMDB write transaction, which fsyncs. So
//! `append` is durable and read-your-writes on return. Waiting uses the
//! experimental `/tasks/stream` route (server-sent task updates, no polling
//! delay); `wait=poll` polls `GET /tasks/{uid}` every `poll_ms` (5) instead.
//!
//! Search: the native ranking rules (words, typo, proximity, attribute,
//! sort, exactness) over `text` only, filtered by `agent = A AND ts <= T`,
//! ties by `ts:desc, id:desc` (the sort rule). Meilisearch keeps only the
//! first 10 words of a query (not configurable) and its default matching
//! strategy (`last`) drops words from the end of the query until enough
//! documents match: not pure OR. There is no stemming.
//!
//! Aggregates (`agg_session`, `agg_tools`, `top_sessions`) are unsupported:
//! Meilisearch has facet counts but no sums.
//! Settings and their reasons: `docs/TRACEBENCH_BASELINES.md`.
use super::Params;
use super::docjson;
use super::http::Http;
use super::pg::{fold, query_words, run_script};
use crate::events::Event;
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore, unsupported};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const CONTAINER: &str = "oojuro-cmp-meili";
const INDEX: &str = "events";
/// Master key of the loopback-only benchmark server (production mode needs
/// one; it protects nothing).
const KEY: &str = "oojuro-tracebench-local-key";
/// Upper bound of `limit`/`offset` and of exhaustive hit counts.
const MAX_TOTAL_HITS: u64 = 1_000_000_000;
/// More events than any session holds (base1m's largest has a few thousand).
const SESSION_LIMIT: u64 = 100_000;
/// Tasks (compaction of a large index, a big delete) may take a while.
const TASK_TIMEOUT: Duration = Duration::from_secs(4 * 3600);

/// Task completion: terminal statuses reported by the event stream, plus a
/// polling fallback.
struct Tasks {
    http: Arc<Http>,
    done: Mutex<HashMap<u64, Value>>,
    cv: Condvar,
    poll: Duration,
}

impl Tasks {
    fn terminal(v: &Value) -> bool {
        matches!(v.get("status").and_then(Value::as_str), Some("succeeded" | "failed" | "canceled"))
    }

    fn wait(&self, uid: u64) -> Result<Value> {
        let t0 = Instant::now();
        let mut g = self.done.lock().unwrap();
        loop {
            if let Some(v) = g.remove(&uid) {
                return Self::check(uid, v);
            }
            let (ng, to) = self.cv.wait_timeout(g, self.poll).unwrap();
            g = ng;
            if to.timed_out() {
                drop(g);
                let v = self.http.json("GET", &format!("/tasks/{uid}"), None)?;
                if Self::terminal(&v) {
                    self.done.lock().unwrap().remove(&uid);
                    return Self::check(uid, v);
                }
                if t0.elapsed() > TASK_TIMEOUT {
                    return Err(format!("meili: task {uid} not finished after {TASK_TIMEOUT:?}").into());
                }
                g = self.done.lock().unwrap();
            }
        }
    }

    fn check(uid: u64, v: Value) -> Result<Value> {
        match v.get("status").and_then(Value::as_str) {
            Some("succeeded") => Ok(v),
            _ => Err(format!("meili: task {uid} did not succeed: {v}").into()),
        }
    }
}

/// Reads `/tasks/stream` (one SSE `data:` line per task status change) and
/// records terminal ones; reconnects until stopped.
fn stream_tasks(tasks: Arc<Tasks>, stop: Arc<AtomicBool>, conn: Arc<Mutex<Option<TcpStream>>>) {
    while !stop.load(Ordering::Relaxed) {
        let r = (|| -> Result<()> {
            let (code, handle, body) = tasks.http.stream("/tasks/stream?statuses=succeeded,failed,canceled")?;
            if code != 200 {
                return Err(format!("tasks stream HTTP {code}").into());
            }
            *conn.lock().unwrap() = Some(handle);
            for line in BufReader::new(body).lines() {
                let line = line?;
                let Some(data) = line.strip_prefix("data:") else { continue };
                let v: Value = serde_json::from_str(data.trim())?;
                if let Some(uid) = v.get("uid").and_then(Value::as_u64)
                    && Tasks::terminal(&v)
                {
                    tasks.done.lock().unwrap().insert(uid, v);
                    tasks.cv.notify_all();
                }
            }
            Ok(())
        })();
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Err(e) = r {
            eprintln!("meili tasks stream: {e} (reconnecting)");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The task-stream reader: stop flag, its connection (to shut it down) and
/// its thread.
type Stream = (Arc<AtomicBool>, Arc<Mutex<Option<TcpStream>>>, JoinHandle<()>);

pub struct Meili {
    http: Arc<Http>,
    tasks: Arc<Tasks>,
    stream: Option<Stream>,
    container_id: String,
    data_dir: PathBuf,
    keep: bool,
    poll_mode: bool,
}

/// Index settings (the reasons are in `docs/TRACEBENCH_BASELINES.md`).
fn settings() -> Value {
    let rule = |attrs: &[&str], comparison: bool| {
        json!({"attributePatterns": attrs, "features": {"facetSearch": false,
               "filter": {"equality": true, "comparison": comparison}}})
    };
    json!({
        "searchableAttributes": ["text"],
        "displayedAttributes": ["*"],
        "filterableAttributes": [rule(&["agent", "kind", "status", "tool"], false), rule(&["ts", "session", "seq"], true)],
        "sortableAttributes": ["ts", "id", "session", "seq"],
        "pagination": {"maxTotalHits": MAX_TOTAL_HITS},
    })
}

/// Params: `wait=poll` (poll task status every `poll_ms` instead of the
/// task stream), `existing=on` (start on the data a previous run left),
/// `keep=on` (leave the container running on drop), `reset=off` (reuse a
/// running container: delete and recreate the index).
pub fn open(_work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let existing = params.flag("existing", false);
    if existing {
        run_script("meili.sh", "start")?;
    } else if params.flag("reset", true) {
        run_script("meili.sh", "reset")?;
    }
    let port = std::env::var("TRACEDB_MEILI_PORT").unwrap_or_else(|_| "56580".into());
    let http = Arc::new(Http::new(format!("127.0.0.1:{port}"), format!("Authorization: Bearer {KEY}\r\n")));
    let poll_mode = params.get("wait") == Some("poll");
    let poll = Duration::from_millis(if poll_mode { params.num("poll_ms", 5u64) } else { 100 });
    let tasks = Arc::new(Tasks { http: Arc::clone(&http), done: Mutex::new(HashMap::new()), cv: Condvar::new(), poll });
    let stream = if poll_mode {
        None
    } else {
        http.json("PATCH", "/experimental-features", Some(&json!({"tasksStreamingRoute": true})))?;
        let stop = Arc::new(AtomicBool::new(false));
        let conn = Arc::new(Mutex::new(None));
        let (t, s, c) = (Arc::clone(&tasks), Arc::clone(&stop), Arc::clone(&conn));
        let h = std::thread::spawn(move || stream_tasks(t, s, c));
        Some((stop, conn, h))
    };
    let container_id = crate::sys::container_id(CONTAINER).ok_or("meili container not running")?;
    let data_dir = PathBuf::from(run_script("meili.sh", "path")?);
    let m = Meili { http, tasks, stream, container_id, data_dir, keep: params.flag("keep", false), poll_mode };
    if !existing {
        let (code, _) = m.http.call("GET", &format!("/indexes/{INDEX}"), "application/json", b"")?;
        if code == 200 {
            m.task("DELETE", &format!("/indexes/{INDEX}"), None)?;
        }
        m.task("POST", "/indexes", Some(&json!({"uid": INDEX, "primaryKey": "id"})))?;
        m.task("PATCH", &format!("/indexes/{INDEX}/settings"), Some(&settings()))?;
    }
    Ok(Box::new(m))
}

/// A Meilisearch filter expression for a TraceBench filter.
fn filter_expr(f: &Filter) -> String {
    let mut w: Vec<String> = Vec::new();
    let (lo, hi) = docjson::time_bounds(f.from_us, f.to_us);
    if let Some(lo) = lo {
        w.push(format!("ts >= {lo}"));
    }
    if let Some(hi) = hi {
        w.push(format!("ts < {hi}"));
    }
    if let Some(a) = f.agent {
        w.push(format!("agent = {a}"));
    }
    if let Some(k) = f.kind {
        w.push(format!("kind = {k}"));
    }
    if let Some(t) = &f.tool {
        w.push(format!("tool = {}", quote(t)));
    }
    if let Some(s) = f.status {
        w.push(format!("status = {s}"));
    }
    w.join(" AND ")
}

/// A string literal of the filter language.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Meili {
    /// Enqueue a task and wait until it has succeeded.
    fn task(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let r = self.http.json(method, path, body)?;
        self.wait(&r)
    }

    fn wait(&self, enqueued: &Value) -> Result<Value> {
        let uid = enqueued.get("taskUid").and_then(Value::as_u64).ok_or_else(|| format!("meili: no taskUid in {enqueued}"))?;
        self.tasks.wait(uid)
    }

    /// One add-or-update task: full documents for `rows`, partial status
    /// documents for `revs`.
    fn write(&self, rows: &[Event], revs: &[Event]) -> Result<()> {
        if rows.is_empty() && revs.is_empty() {
            return Ok(());
        }
        let mut body = Vec::with_capacity(rows.iter().map(|e| e.raw_bytes() as usize + 256).sum());
        for e in rows {
            serde_json::to_writer(&mut body, &docjson::doc(&Row::from_event(e), true))?;
            body.push(b'\n');
        }
        for r in revs {
            let mut d = docjson::status_fields(r.status, r.exit_code, r.duration_us);
            d.insert("id".into(), json!(r.revises));
            serde_json::to_writer(&mut body, &d)?;
            body.push(b'\n');
        }
        let r = self.http.json_raw(
            "PUT",
            &format!("/indexes/{INDEX}/documents?primaryKey=id"),
            "application/x-ndjson",
            &body,
        )?;
        self.wait(&r)?;
        Ok(())
    }

    fn search_raw(&self, body: &Value) -> Result<Value> {
        self.http.json("POST", &format!("/indexes/{INDEX}/search"), Some(body))
    }

    fn hits(v: &Value) -> &[Value] {
        v.get("hits").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
    }

    fn ids(v: &Value) -> Result<Vec<u64>> {
        Self::hits(v).iter().map(|h| h.get("id").and_then(Value::as_u64).ok_or_else(|| "hit without id".into())).collect()
    }

    fn rows(v: &Value) -> Result<Vec<Row>> {
        Self::hits(v).iter().map(|h| docjson::row(h, None)).collect()
    }

    fn get_doc(&self, id: u64, fields: Option<&str>) -> Result<Option<Value>> {
        let q = fields.map(|f| format!("?fields={f}")).unwrap_or_default();
        let (code, body) = self.http.call("GET", &format!("/indexes/{INDEX}/documents/{id}{q}"), "application/json", b"")?;
        match code {
            200 => Ok(Some(serde_json::from_slice(&body)?)),
            404 => Ok(None),
            c => Err(format!("meili get {id}: HTTP {c}: {}", String::from_utf8_lossy(&body)).into()),
        }
    }

    fn delete_by(&self, filter: &str) -> Result<()> {
        self.task("POST", &format!("/indexes/{INDEX}/documents/delete"), Some(&json!({"filter": filter})))?;
        Ok(())
    }
}

impl TraceStore for Meili {
    fn name(&self) -> String {
        format!(
            "meilisearch 1.55.0 (LMDB, append waits for task success{})",
            if self.poll_mode { ", polled" } else { ", task stream" }
        )
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        let (rows, revs) = fold(batch.iter());
        self.write(&rows, &revs)
    }

    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        let (rows, revs) = fold(batches.iter().flatten());
        self.write(&rows, &revs)
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        self.get_doc(id, None)?.map(|v| docjson::row(&v, None)).transpose()
    }

    /// The document's text field is fetched and cut client-side (no partial
    /// field reads).
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.get_doc(id, Some("text,raw"))?.map(|v| {
            let t = v.get("text").or_else(|| v.get("raw")).and_then(Value::as_str).unwrap_or_default();
            docjson::slice(t, start, end)
        }))
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let v = self.search_raw(&json!({"q": "", "filter": format!("session = {session}"), "sort": ["seq:asc"],
                                        "limit": SESSION_LIMIT}))?;
        Self::rows(&v)
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let f = filter_expr(&Filter { from_us, to_us, ..Default::default() });
        let v = self.search_raw(&json!({"q": "", "filter": f, "sort": ["ts:asc", "id:asc"], "limit": limit}))?;
        Self::rows(&v)
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let v = self.search_raw(&json!({"q": "", "filter": filter_expr(f), "sort": ["ts:asc", "id:asc"],
                                        "limit": limit, "attributesToRetrieve": ["id"]}))?;
        Self::ids(&v)
    }

    /// Exhaustive count: page mode (`hitsPerPage`) reports `totalHits`.
    fn count(&self, f: &Filter) -> Result<u64> {
        let v = self.search_raw(&json!({"q": "", "filter": filter_expr(f), "hitsPerPage": 0, "page": 1}))?;
        v.get("totalHits").and_then(Value::as_u64).ok_or_else(|| format!("meili count: no totalHits in {v}").into())
    }

    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let words = query_words(query, char::is_alphanumeric, false, 64);
        if words.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let v = self.search_raw(&json!({
            "q": words.join(" "), "filter": format!("agent = {agent} AND ts <= {as_of_us}"),
            "sort": ["ts:desc", "id:desc"], "limit": k, "attributesToRetrieve": ["id"],
        }))?;
        Self::ids(&v)
    }

    fn agg_session(&self, _session: u64) -> Result<SessionAgg> {
        unsupported("agg_session", "Meilisearch has no aggregation beyond facet counts (no sums)")
    }

    fn agg_tools(&self, _from_us: i64, _to_us: i64) -> Result<Vec<ToolAgg>> {
        unsupported("agg_tools", "Meilisearch has no aggregation beyond facet counts (no sums)")
    }

    fn top_sessions(&self, _from_us: i64, _to_us: i64, _n: usize) -> Result<Vec<(u64, u64)>> {
        unsupported("top_sessions", "Meilisearch has no aggregation beyond facet counts (no sums, no group-by)")
    }

    fn delete_session(&self, session: u64) -> Result<()> {
        self.delete_by(&format!("session = {session}"))
    }

    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.delete_by(&format!("ts < {ts_us}"))
    }

    /// Index compaction (`POST /indexes/events/compact`): the index's LMDB
    /// file is copied with compaction (live pages only, free pages holding
    /// deleted documents are not copied) and replaces the original.
    fn reclaim(&self) -> Result<()> {
        self.task("POST", &format!("/indexes/{INDEX}/compact"), None)?;
        Ok(())
    }

    /// Keyset pagination in (session, seq) order: each page is a sorted
    /// placeholder search after the last (session, seq) seen.
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        let mut after: Option<(u64, u32)> = None;
        loop {
            let mut body = json!({"q": "", "sort": ["session:asc", "seq:asc"], "limit": 1000});
            if let Some((s, q)) = after {
                body["filter"] = json!(format!("session > {s} OR (session = {s} AND seq > {q})"));
            }
            let rows = Self::rows(&self.search_raw(&body)?)?;
            let Some(last) = rows.last() else { break };
            after = Some((last.session, last.seq));
            for r in &rows {
                buf.clear();
                crate::export_line(r, &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
        }
        Ok((n, bytes))
    }

    fn disk_bytes(&self) -> Result<u64> {
        Ok(crate::sys::dir_bytes(&self.data_dir.join("data.ms")))
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
        self.http.json("GET", "/stats", None).unwrap_or(Value::Null)
    }
}

impl Drop for Meili {
    fn drop(&mut self) {
        if let Some((stop, conn, h)) = self.stream.take() {
            stop.store(true, Ordering::Relaxed);
            if let Some(c) = conn.lock().unwrap().take() {
                let _ = c.shutdown(Shutdown::Both);
            }
            let _ = h.join();
        }
        self.http.clear();
        if !self.keep && std::env::var_os("TRACEDB_KEEP_CONTAINER").is_none() {
            let _ = run_script("meili.sh", "stop");
        }
    }
}
