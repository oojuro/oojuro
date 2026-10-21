//! Qdrant arm (`qdrant`): Qdrant 1.19 in a container
//! (`scripts/tracedb/qdrant.sh`), one collection `events` over HTTP/JSON.
//!
//! **No vectors.** The collection is created with an empty vector config
//! (`"vectors": {}`, supported since 1.x for payload-only points); every
//! event is a point whose id is the event id and whose payload holds the
//! fields. Text of indexed kinds goes into `text`, which has a full-text
//! payload index (word tokenizer, lowercase, English Snowball stemmer,
//! English stop words); thinking and system text goes into `raw` (no index).
//! Integer and keyword payload indexes cover agent, session, kind, status,
//! tool and ts (range, principal).
//!
//! Durability: every write is sent with `wait=true`, which makes Qdrant
//! flush (msync) its write-ahead log before applying the operation and
//! answer after it is applied: durable and read-your-writes on return. A
//! turn batch is one `points/batch` request: the upsert of its events plus a
//! `set_payload` per status revision of an earlier call. Qdrant has no
//! multi-operation transactions: each operation is its own WAL record.
//!
//! Search: `match: {text_any: ...}` (OR over the query's tokens after the
//! index's tokenizer) with the agent and `ts <= as_of` conditions. **No
//! relevance ranking**: a full-text payload condition is a filter; hits are
//! returned newest first (`order_by ts desc`). BM25 in Qdrant needs sparse
//! vectors, which this arm does not use.
//!
//! Aggregates (`agg_session`, `agg_tools`, `top_sessions`) are unsupported:
//! Qdrant has facet counts but no sums.
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
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CONTAINER: &str = "oojuro-cmp-qdrant";
const C: &str = "/collections/events";
/// More events than any session holds.
const SESSION_LIMIT: u64 = 100_000;
/// Split a write request beyond this many body bytes (the server's default
/// request limit is 32 MB).
const MAX_BODY: usize = 16 << 20;

pub struct Qdrant {
    http: Http,
    container_id: String,
    data_dir: PathBuf,
    keep: bool,
}

/// Payload indexes (field, schema).
fn indexes() -> Vec<(&'static str, Value)> {
    let lookup = json!({"type": "integer", "lookup": true, "range": false});
    vec![
        ("agent", lookup.clone()),
        ("session", lookup.clone()),
        ("kind", lookup.clone()),
        ("status", lookup),
        ("tool", json!({"type": "keyword"})),
        ("ts", json!({"type": "integer", "lookup": false, "range": true, "is_principal": true})),
        (
            "text",
            json!({"type": "text", "tokenizer": "word", "lowercase": true,
                   "stemmer": {"type": "snowball", "language": "english"}, "stopwords": "english"}),
        ),
    ]
}

/// Params: `existing=on` (start on the storage a previous run left),
/// `keep=on` (leave the container running on drop), `reset=off` (reuse a
/// running container: drop and recreate the collection).
pub fn open(_work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let existing = params.flag("existing", false);
    if existing {
        run_script("qdrant.sh", "start")?;
    } else if params.flag("reset", true) {
        run_script("qdrant.sh", "reset")?;
    }
    let port = std::env::var("TRACEDB_QDRANT_PORT").unwrap_or_else(|_| "56590".into());
    let http = Http::new(format!("127.0.0.1:{port}"), String::new());
    let container_id = crate::sys::container_id(CONTAINER).ok_or("qdrant container not running")?;
    let data_dir = PathBuf::from(run_script("qdrant.sh", "path")?);
    let q = Qdrant { http, container_id, data_dir, keep: params.flag("keep", false) };
    if !existing {
        let (code, _) = q.http.call("GET", C, "application/json", b"")?;
        if code == 200 {
            q.http.json("DELETE", C, None)?;
        }
        q.http.json("PUT", C, Some(&json!({"vectors": {}})))?;
        for (field, schema) in indexes() {
            q.http.json("PUT", &format!("{C}/index?wait=true"), Some(&json!({"field_name": field, "field_schema": schema})))?;
        }
    }
    Ok(Box::new(q))
}

fn filter_json(f: &Filter) -> Value {
    let mut must: Vec<Value> = Vec::new();
    let (lo, hi) = docjson::time_bounds(f.from_us, f.to_us);
    if lo.is_some() || hi.is_some() {
        let mut r = serde_json::Map::new();
        if let Some(lo) = lo {
            r.insert("gte".into(), json!(lo));
        }
        if let Some(hi) = hi {
            r.insert("lt".into(), json!(hi));
        }
        must.push(json!({"key": "ts", "range": r}));
    }
    if let Some(a) = f.agent {
        must.push(json!({"key": "agent", "match": {"value": a}}));
    }
    if let Some(k) = f.kind {
        must.push(json!({"key": "kind", "match": {"value": k}}));
    }
    if let Some(t) = &f.tool {
        must.push(json!({"key": "tool", "match": {"value": t}}));
    }
    if let Some(s) = f.status {
        must.push(json!({"key": "status", "match": {"value": s}}));
    }
    json!({"must": must})
}

fn point(e: &Event) -> Value {
    json!({"id": e.id, "vector": {}, "payload": docjson::doc(&Row::from_event(e), false)})
}

fn pid(p: &Value) -> Result<u64> {
    p.get("id").and_then(Value::as_u64).ok_or_else(|| format!("qdrant: point without integer id: {p}").into())
}

fn prow(p: &Value) -> Result<Row> {
    docjson::row(p.get("payload").ok_or("qdrant: point without payload")?, Some(pid(p)?))
}

impl Qdrant {
    fn result(v: Value) -> Value {
        v.get("result").cloned().unwrap_or(Value::Null)
    }

    /// One `points/batch?wait=true` request per ~16 MB of upserts; the
    /// revisions ride in the last one.
    fn write(&self, rows: &[Event], revs: &[Event]) -> Result<()> {
        let mut groups: Vec<Vec<Value>> = vec![Vec::new()];
        let mut size = 0usize;
        for e in rows {
            let sz = e.raw_bytes() as usize + 256;
            if size + sz > MAX_BODY && !groups.last().unwrap().is_empty() {
                groups.push(Vec::new());
                size = 0;
            }
            size += sz;
            groups.last_mut().unwrap().push(point(e));
        }
        let n = groups.len();
        for (i, pts) in groups.into_iter().enumerate() {
            let mut ops: Vec<Value> = Vec::new();
            if !pts.is_empty() {
                ops.push(json!({"upsert": {"points": pts}}));
            }
            if i + 1 == n {
                for r in revs {
                    let p = docjson::status_fields(r.status, r.exit_code, r.duration_us);
                    ops.push(json!({"set_payload": {"payload": p, "points": [r.revises]}}));
                }
            }
            if !ops.is_empty() {
                self.http.json("POST", &format!("{C}/points/batch?wait=true"), Some(&json!({"operations": ops})))?;
            }
        }
        Ok(())
    }

    /// Scroll one page; returns (points, next offset).
    fn scroll(&self, body: &Value) -> Result<(Vec<Value>, Option<Value>)> {
        let r = Self::result(self.http.json("POST", &format!("{C}/points/scroll"), Some(body))?);
        let pts = r.get("points").and_then(Value::as_array).cloned().unwrap_or_default();
        let next = r.get("next_page_offset").filter(|v| !v.is_null()).cloned();
        Ok((pts, next))
    }

    /// Up to `limit` matching points in exact (ts, id) order: `order_by ts`
    /// (ties at the cut are completed by an equality query, ordered by id).
    fn by_time(&self, filter: &Value, limit: usize, payload: &Value) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ts = |p: &Value| {
            p.get("order_value").and_then(Value::as_i64).or_else(|| p.pointer("/payload/ts").and_then(Value::as_i64))
        };
        let (pts, _) = self.scroll(&json!({"filter": filter, "limit": limit, "with_payload": payload,
                                          "order_by": {"key": "ts", "direction": "asc"}}))?;
        if pts.len() < limit {
            let mut v = pts;
            v.sort_by_key(|p| (ts(p), pid(p).unwrap_or(0)));
            return Ok(v);
        }
        let last = ts(pts.last().unwrap()).ok_or("qdrant: no order_value")?;
        let mut out: Vec<Value> = pts.into_iter().filter(|p| ts(p) < Some(last)).collect();
        out.sort_by_key(|p| (ts(p), pid(p).unwrap_or(0)));
        let mut f = filter.clone();
        f["must"].as_array_mut().unwrap().push(json!({"key": "ts", "range": {"gte": last, "lte": last}}));
        let mut ties: Vec<Value> = Vec::new();
        let mut offset: Option<Value> = None;
        loop {
            let mut body = json!({"filter": f, "limit": 10_000, "with_payload": payload});
            if let Some(o) = offset {
                body["offset"] = o;
            }
            let (pts, next) = self.scroll(&body)?;
            ties.extend(pts);
            match next {
                Some(n) => offset = Some(n),
                None => break,
            }
        }
        ties.sort_by_key(|p| pid(p).unwrap_or(0));
        let room = limit - out.len();
        out.extend(ties.into_iter().take(room));
        Ok(out)
    }

    fn delete_by(&self, filter: Value) -> Result<()> {
        self.http.json("POST", &format!("{C}/points/delete?wait=true"), Some(&json!({"filter": filter})))?;
        Ok(())
    }

    /// No optimisation queued or running and the collection green.
    fn idle(&self) -> Result<bool> {
        let o = Self::result(self.http.json("GET", &format!("{C}/optimizations"), None)?);
        let queued = o.pointer("/summary/queued_optimizations").and_then(Value::as_u64).unwrap_or(1);
        let running = o.get("running").and_then(Value::as_array).map_or(1, Vec::len);
        let c = Self::result(self.http.json("GET", C, None)?);
        let green = c.get("status").and_then(Value::as_str) == Some("green");
        Ok(queued == 0 && running == 0 && green)
    }
}

impl TraceStore for Qdrant {
    fn name(&self) -> String {
        "qdrant 1.19.2 (payload-only points, full-text payload index, wait=true)".into()
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        let (rows, revs) = fold(batch.iter());
        self.write(&rows, &revs)
    }

    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        let (rows, revs) = fold(batches.iter().flatten());
        self.write(&rows, &revs)
    }

    /// Wait until the optimizers have settled the loaded segments.
    fn finish_load(&self) -> Result<()> {
        let t0 = Instant::now();
        let mut calm = 0;
        while calm < 2 {
            calm = if self.idle()? { calm + 1 } else { 0 };
            if t0.elapsed() > Duration::from_secs(1800) {
                return Err("qdrant: optimizers did not settle in 30 min".into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(())
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        let (code, body) = self.http.call("GET", &format!("{C}/points/{id}"), "application/json", b"")?;
        match code {
            200 => Ok(Some(prow(&Self::result(serde_json::from_slice(&body)?))?)),
            404 => Ok(None),
            c => Err(format!("qdrant get {id}: HTTP {c}: {}", String::from_utf8_lossy(&body)).into()),
        }
    }

    /// The point's text payload is fetched and cut client-side.
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let r = Self::result(self.http.json(
            "POST",
            &format!("{C}/points"),
            Some(&json!({"ids": [id], "with_payload": {"include": ["text", "raw"]}})),
        )?);
        Ok(r.as_array().and_then(|a| a.first()).map(|p| {
            let t = p
                .pointer("/payload/text")
                .or_else(|| p.pointer("/payload/raw"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            docjson::slice(t, start, end)
        }))
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let f = json!({"must": [{"key": "session", "match": {"value": session}}]});
        let (pts, _) = self.scroll(&json!({"filter": f, "limit": SESSION_LIMIT, "with_payload": true}))?;
        let mut v = pts.iter().map(prow).collect::<Result<Vec<_>>>()?;
        v.sort_by_key(|r| r.seq);
        Ok(v)
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let f = filter_json(&Filter { from_us, to_us, ..Default::default() });
        self.by_time(&f, limit, &json!(true))?.iter().map(prow).collect()
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        self.by_time(&filter_json(f), limit, &json!(false))?.iter().map(pid).collect()
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        let r = Self::result(
            self.http.json("POST", &format!("{C}/points/count"), Some(&json!({"filter": filter_json(f), "exact": true})))?,
        );
        r.get("count").and_then(Value::as_u64).ok_or_else(|| format!("qdrant count: {r}").into())
    }

    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let words = query_words(query, char::is_alphanumeric, false, 64);
        if words.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let f = json!({"must": [
            {"key": "agent", "match": {"value": agent}},
            {"key": "ts", "range": {"lte": as_of_us}},
            {"key": "text", "match": {"text_any": words.join(" ")}},
        ]});
        let (pts, _) = self.scroll(&json!({"filter": f, "limit": k, "with_payload": false,
                                          "order_by": {"key": "ts", "direction": "desc"}}))?;
        pts.iter().map(pid).collect()
    }

    fn agg_session(&self, _session: u64) -> Result<SessionAgg> {
        unsupported("agg_session", "Qdrant has facet counts but no aggregation (no sums)")
    }

    fn agg_tools(&self, _from_us: i64, _to_us: i64) -> Result<Vec<ToolAgg>> {
        unsupported("agg_tools", "Qdrant has facet counts but no aggregation (no sums)")
    }

    fn top_sessions(&self, _from_us: i64, _to_us: i64, _n: usize) -> Result<Vec<(u64, u64)>> {
        unsupported("top_sessions", "Qdrant has facet counts but no aggregation (no sums, no group-by)")
    }

    fn delete_session(&self, session: u64) -> Result<()> {
        self.delete_by(json!({"must": [{"key": "session", "match": {"value": session}}]}))
    }

    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.delete_by(json!({"must": [{"key": "ts", "range": {"lt": ts_us}}]}))
    }

    /// Qdrant has no compaction call: deleted points are only marked until
    /// the vacuum optimizer rebuilds their segment. The vacuum thresholds are
    /// lowered to their minimum (any deleted point, segments of >= 100
    /// points), the optimizers are waited for, and the defaults restored.
    /// The write-ahead log (open 32 MB segment + 1 retained closed segment)
    /// still holds the most recent operations, deleted texts included.
    fn reclaim(&self) -> Result<()> {
        let set = |t: f64, n: u64| {
            self.http.json(
                "PATCH",
                C,
                Some(&json!({"optimizers_config": {"deleted_threshold": t, "vacuum_min_vector_number": n}})),
            )
        };
        set(0.0, 100)?;
        std::thread::sleep(Duration::from_millis(500));
        let r = self.finish_load();
        set(0.2, 1000)?;
        r
    }

    /// Two passes: scroll every point's (session, seq), sort, then fetch the
    /// points by id in that order.
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let mut keys: Vec<(u64, u64, u64)> = Vec::new();
        let mut offset: Option<Value> = None;
        loop {
            let mut body = json!({"limit": 10_000, "with_payload": {"include": ["session", "seq"]}});
            if let Some(o) = offset {
                body["offset"] = o;
            }
            let (pts, next) = self.scroll(&body)?;
            for p in &pts {
                let s = p.pointer("/payload/session").and_then(Value::as_u64).unwrap_or(0);
                let q = p.pointer("/payload/seq").and_then(Value::as_u64).unwrap_or(0);
                keys.push((s, q, pid(p)?));
            }
            match next {
                Some(n) => offset = Some(n),
                None => break,
            }
        }
        keys.sort_unstable();
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        for chunk in keys.chunks(500) {
            let ids: Vec<u64> = chunk.iter().map(|k| k.2).collect();
            let r = Self::result(
                self.http.json("POST", &format!("{C}/points"), Some(&json!({"ids": ids, "with_payload": true})))?,
            );
            let mut by: HashMap<u64, Row> = HashMap::new();
            for p in r.as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let row = prow(p)?;
                by.insert(row.id, row);
            }
            for id in ids {
                let Some(row) = by.get(&id) else { continue };
                buf.clear();
                crate::export_line(row, &mut buf);
                out.write_all(&buf)?;
                n += 1;
                bytes += buf.len() as u64;
            }
        }
        Ok((n, bytes))
    }

    /// Allocated bytes: Qdrant preallocates sparse 32 MB page files (payload
    /// storage, text index, WAL), so the apparent size overstates the disk
    /// used (`details` reports both).
    fn disk_bytes(&self) -> Result<u64> {
        Ok(crate::sys::dir_alloc_bytes(&self.data_dir))
    }

    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::cgroup_mem(&self.container_id))
    }

    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = Vec::new();
        crate::sys::files_under(&self.data_dir, &mut v);
        Ok(v)
    }

    /// Apparent and allocated bytes, the write-ahead log's apparent size
    /// (preallocated 32 MB segments), and the collection info.
    fn details(&self) -> Value {
        let mut wal = 0u64;
        let mut files = Vec::new();
        crate::sys::files_under(&self.data_dir, &mut files);
        for f in &files {
            if f.components().any(|c| c.as_os_str() == "wal") {
                wal += std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            }
        }
        let info = self.http.json("GET", C, None).map(Self::result).unwrap_or(Value::Null);
        json!({"wal_apparent_bytes": wal, "apparent_bytes": crate::sys::dir_bytes(&self.data_dir),
               "allocated_bytes": crate::sys::dir_alloc_bytes(&self.data_dir), "collection": info})
    }
}

impl Drop for Qdrant {
    fn drop(&mut self) {
        self.http.clear();
        if !self.keep && std::env::var_os("TRACEDB_KEEP_CONTAINER").is_none() {
            let _ = run_script("qdrant.sh", "stop");
        }
    }
}
