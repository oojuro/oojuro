//! Quickwit 0.9.2 competitor arm (`quickwit`): a log-search engine (tantivy
//! splits on local storage, file-backed metastore, ingest v2 WAL), one node
//! in a container (`scripts/tracedb/quickwit.sh`). Settings and their
//! reasons: `docs/TRACEBENCH_BASELINES.md` ("Quickwit").
//!
//! One index `events`; one document per event and one per status revision
//! (`revises` = the call's id): Quickwit has no update, so every read that
//! returns rows (get, session, time_range, export) also fetches the
//! revisions of the calls it returns and applies the newest (largest id) on
//! the client. The text of indexed kinds (user, output, tool_call,
//! tool_result) is the BM25 field `body` (tokenizer `default`, term
//! frequencies and field norms); thinking and system text is stored,
//! unindexed, as `text`.
//!
//! Durability and read-your-writes: `append` ingests with `commit=force`
//! and returns once the split holding the batch is published (searchable),
//! which Quickwit's local storage fsyncs (file, then directory) together
//! with the metastore. Bulk-load chunks use `commit=auto`; `finish_load`
//! waits until every document written is searchable.
//!
//! Unsupported (explicit errors, never approximations): filters and counts
//! on the latest status and `agg_tools` (they need the latest revision of
//! every call in the range: a join the engine does not have); deletes,
//! purge and reclaim (Quickwit's delete API runs asynchronously on mature
//! splits only, 48 h by default, so nothing is invisible on return).
//!
//! Params: `existing=on` (start on the data a previous run left), `keep=on`
//! (leave the container running when dropped), `reset=off` (reuse a running
//! container, recreating the index).
use super::Params;
use super::http::Http;
use super::pg::{fold, query_words, run_script};
use crate::events::{Event, TOOL_CALL, indexed_kind};
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore, unsupported};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const CONTAINER: &str = "oojuro-cmp-quickwit";
const INDEX: &str = "events";
/// Quickwit's cap on hits per request (and on `start_offset`).
const PAGE: usize = 10_000;
/// Stay below the 10 MiB ingest payload limit.
const MAX_INGEST_BYTES: usize = 8 << 20;

pub struct Quickwit {
    http: Http,
    container_id: String,
    data_dir: PathBuf,
    keep: bool,
    /// Documents written (events + revisions); `finish_load` waits until
    /// this many are searchable.
    docs: AtomicU64,
}

fn index_config() -> Value {
    let num = |name: &str, ty: &str, fast: bool, indexed: bool| {
        json!({"name": name, "type": ty, "fast": fast, "indexed": indexed, "stored": true})
    };
    json!({
        "version": "0.9",
        "index_id": INDEX,
        "doc_mapping": {
            "mode": "strict",
            "field_mappings": [
                num("id", "u64", true, true),
                num("session", "u64", true, true),
                num("agent", "u64", true, true),
                num("turn", "u64", false, false),
                num("seq", "u64", true, false),
                num("parent", "u64", false, false),
                num("revises", "u64", true, true),
                num("kind", "u64", true, true),
                num("status", "u64", true, true),
                num("exit_code", "i64", false, false),
                num("tokens_in", "u64", false, false),
                num("tokens_out", "u64", false, false),
                {"name": "tok", "type": "u64", "fast": true, "indexed": false, "stored": false},
                num("ts", "i64", true, true),
                {"name": "t", "type": "datetime", "input_formats": ["rfc3339"], "fast": true,
                 "fast_precision": "microseconds", "indexed": false, "stored": false},
                num("duration", "i64", false, false),
                {"name": "tool", "type": "text", "tokenizer": "raw", "fast": true, "record": "basic"},
                {"name": "args", "type": "text", "indexed": false},
                {"name": "body", "type": "text", "tokenizer": "default", "record": "freq", "fieldnorms": true},
                {"name": "text", "type": "text", "indexed": false},
            ],
            "timestamp_field": "t",
        },
        "indexing_settings": {
            // the indexer's in-memory budget, inside the 3 GiB container
            "resources": {"heap_size": "1GB"},
        },
        "search_settings": {"default_search_fields": ["body"]},
    })
}

/// `us` (µs since the epoch) as RFC 3339 with microseconds, for the
/// timestamp field (split pruning). None outside years 1970..9999.
fn rfc3339(us: i64) -> Option<String> {
    if !(0..253_402_300_800_000_000).contains(&us) {
        return None;
    }
    let (secs, frac) = (us / 1_000_000, us % 1_000_000);
    let (days, sod) = (secs / 86_400, secs % 86_400);
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{frac:06}Z", sod / 3600, sod % 3600 / 60, sod % 60))
}

fn doc(e: &Event) -> Value {
    let mut d = json!({
        "id": e.id, "session": e.session, "agent": e.agent, "turn": e.turn, "seq": e.seq, "parent": e.parent,
        "revises": e.revises, "kind": e.kind, "status": e.status, "tokens_in": e.tokens_in,
        "tokens_out": e.tokens_out, "tok": e.tokens_in as u64 + e.tokens_out as u64, "ts": e.ts_us,
        "duration": e.duration_us, "tool": e.tool,
    });
    if let Some(t) = rfc3339(e.ts_us) {
        d["t"] = json!(t);
    }
    if let Some(x) = e.exit_code {
        d["exit_code"] = json!(x);
    }
    if !e.args.is_empty() {
        d["args"] = json!(e.args);
    }
    if !e.text.is_empty() {
        d[if indexed_kind(e.kind) && e.revises == 0 { "body" } else { "text" }] = json!(e.text);
    }
    d
}

fn u(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

fn row_of(s: &Value) -> Row {
    let text = s["body"].as_str().or_else(|| s["text"].as_str()).unwrap_or("");
    Row {
        id: u(s, "id"),
        session: u(s, "session"),
        agent: u(s, "agent") as u32,
        turn: u(s, "turn") as u32,
        seq: u(s, "seq") as u32,
        parent: u(s, "parent"),
        kind: u(s, "kind") as u8,
        status: u(s, "status") as u8,
        exit_code: s["exit_code"].as_i64().map(|x| x as i32),
        tokens_in: u(s, "tokens_in") as u32,
        tokens_out: u(s, "tokens_out") as u32,
        ts_us: s["ts"].as_i64().unwrap_or(0),
        duration_us: s["duration"].as_i64().unwrap_or(0),
        tool: s["tool"].as_str().unwrap_or("").to_string(),
        args: s["args"].as_str().unwrap_or("").to_string(),
        text: text.to_string(),
    }
}

/// The newest revision per call: call id -> (revision id, status, exit, duration).
type Latest = HashMap<u64, (u64, u8, Option<i32>, i64)>;

fn note_revision(m: &mut Latest, s: &Value) {
    let (call, id) = (u(s, "revises"), u(s, "id"));
    let v = (id, u(s, "status") as u8, s["exit_code"].as_i64().map(|x| x as i32), s["duration"].as_i64().unwrap_or(0));
    match m.get(&call) {
        Some(old) if old.0 >= id => {}
        _ => {
            m.insert(call, v);
        }
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

const REV_FIELDS: &str = "id,revises,status,exit_code,duration";

/// Filter clauses: the event time range (exact on `ts`, plus the timestamp
/// field `t` when representable, so Quickwit can prune splits by time).
fn time_clauses(from: i64, to: i64) -> Vec<Value> {
    let mut v = vec![json!({"range": {"ts": {"gte": from, "lt": to}}})];
    if let (Some(a), Some(b)) = (rfc3339(from.saturating_sub(1_000_000)), rfc3339(to.saturating_add(1_000_000))) {
        v.push(json!({"range": {"t": {"gte": a, "lte": b}}}));
    }
    v
}

fn filter_query(f: &Filter) -> Value {
    let mut c = time_clauses(f.from_us, f.to_us);
    c.push(json!({"term": {"revises": 0}}));
    if let Some(a) = f.agent {
        c.push(json!({"term": {"agent": a}}));
    }
    if let Some(k) = f.kind {
        c.push(json!({"term": {"kind": k}}));
    }
    if let Some(t) = &f.tool {
        c.push(json!({"term": {"tool": t}}));
    }
    json!({"bool": {"filter": c}})
}

const STATUS_WHY: &str = "needs the latest revision of every matching call (append-only engine, no join; \
                          resolving it client-side means fetching every candidate document)";

pub fn open(_work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let existing = params.flag("existing", false);
    if existing {
        run_script("quickwit.sh", "start")?;
    } else if params.flag("reset", true) {
        run_script("quickwit.sh", "reset")?;
    }
    let port = std::env::var("TRACEDB_QW_PORT").unwrap_or_else(|_| "27280".into());
    let http = Http::new(format!("127.0.0.1:{port}"), String::new());
    let container_id = crate::sys::container_id(CONTAINER).ok_or("quickwit container not running")?;
    let data_dir = PathBuf::from(run_script("quickwit.sh", "path")?);
    let qw = Quickwit { http, container_id, data_dir, keep: params.flag("keep", false), docs: AtomicU64::new(0) };
    if existing {
        // after a restart the REST API answers before the searcher has
        // joined the cluster: wait until a search succeeds (part of the
        // measured open time)
        let t0 = Instant::now();
        let n = loop {
            match qw.count_all() {
                Ok(n) => break n,
                Err(e) if t0.elapsed() > Duration::from_secs(300) => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        qw.docs.store(n, Ordering::Relaxed);
    } else {
        let (code, body) = qw.http.call("DELETE", &format!("/api/v1/indexes/{INDEX}"), "application/json", b"")?;
        if !(200..300).contains(&code) && code != 404 {
            return Err(format!("quickwit: delete index: {code} {}", String::from_utf8_lossy(&body)).into());
        }
        qw.http.json("POST", "/api/v1/indexes", Some(&index_config()))?;
    }
    Ok(Box::new(qw))
}

impl Quickwit {
    /// Elasticsearch-compatible search; `source` = the `_source` fields to
    /// return ("" = all stored fields).
    fn es(&self, body: &Value, source: &str) -> Result<Value> {
        let mut target = format!("/api/v1/_elastic/{INDEX}/_search");
        if !source.is_empty() {
            target.push_str(&format!("?_source_includes={}", super::http::url_encode(source)));
        }
        self.http.json("POST", &target, Some(body))
    }

    fn hits(v: &Value) -> &[Value] {
        v["hits"]["hits"].as_array().map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every hit of `query` in `sort` order (search_after paging), at most
    /// `limit`; `f` sees each hit's `_source`.
    fn each(&self, query: &Value, sort: &Value, source: &str, limit: usize, f: &mut dyn FnMut(&Value)) -> Result<()> {
        let mut after: Option<Value> = None;
        let mut left = limit;
        while left > 0 {
            let size = left.min(PAGE);
            let mut body = json!({"query": query, "sort": sort, "size": size});
            if let Some(a) = &after {
                body["search_after"] = a.clone();
            }
            let v = self.es(&body, source)?;
            let hits = Self::hits(&v);
            for h in hits {
                f(&h["_source"]);
            }
            if hits.len() < size {
                break;
            }
            left -= hits.len();
            after = hits.last().map(|h| h["sort"].clone());
        }
        Ok(())
    }

    fn count_query(&self, query: &Value) -> Result<u64> {
        let v = self.es(&json!({"query": query, "size": 0, "track_total_hits": true}), "")?;
        v["hits"]["total"]["value"].as_u64().ok_or_else(|| format!("quickwit: no total in {v}").into())
    }

    fn count_all(&self) -> Result<u64> {
        let v = self.http.json("POST", &format!("/api/v1/{INDEX}/search"), Some(&json!({"query": "*", "max_hits": 0})))?;
        Ok(v["num_hits"].as_u64().unwrap_or(0))
    }

    /// Newest revisions of `calls`.
    fn latest(&self, calls: &[u64]) -> Result<Latest> {
        let mut m = Latest::new();
        for chunk in calls.chunks(1000) {
            let q = json!({"terms": {"revises": chunk}});
            self.each(&q, &json!([{"revises": "asc"}, {"id": "asc"}]), REV_FIELDS, usize::MAX, &mut |s| {
                note_revision(&mut m, s)
            })?;
        }
        Ok(m)
    }

    /// Rows with the latest status applied.
    fn finish_rows(&self, mut rows: Vec<Row>) -> Result<Vec<Row>> {
        let calls: Vec<u64> = rows.iter().filter(|r| r.kind == TOOL_CALL).map(|r| r.id).collect();
        if !calls.is_empty() {
            apply(&mut rows, &self.latest(&calls)?);
        }
        Ok(rows)
    }

    /// Ingest NDJSON documents; `force`: commit=force (durable and
    /// searchable on return). Overload answers (429/503, e.g. while shards
    /// open or scale) are retried with backoff for up to 10 minutes.
    fn ingest(&self, docs: &[Value], force: bool) -> Result<()> {
        let mut start = 0;
        while start < docs.len() {
            let mut body = Vec::new();
            let mut end = start;
            while end < docs.len() && (end == start || body.len() < MAX_INGEST_BYTES) {
                serde_json::to_writer(&mut body, &docs[end])?;
                body.push(b'\n');
                end += 1;
            }
            let target = format!("/api/v1/{INDEX}/ingest?commit={}", if force { "force" } else { "auto" });
            let t0 = Instant::now();
            let mut wait = Duration::from_millis(50);
            loop {
                let (code, resp) = self.http.call("POST", &target, "application/x-ndjson", &body)?;
                if (200..300).contains(&code) {
                    let v: Value = serde_json::from_slice(&resp)?;
                    let n = (end - start) as u64;
                    if v["num_ingested_docs"].as_u64() != Some(n) {
                        return Err(format!("quickwit: ingested {} of {n} documents: {v}", v["num_ingested_docs"]).into());
                    }
                    break;
                }
                if !matches!(code, 429 | 503) || t0.elapsed() > Duration::from_secs(600) {
                    return Err(format!("quickwit ingest: HTTP {code}: {}", String::from_utf8_lossy(&resp)).into());
                }
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(2));
            }
            self.docs.fetch_add((end - start) as u64, Ordering::Relaxed);
            start = end;
        }
        Ok(())
    }

    fn write<'a>(&self, events: impl Iterator<Item = &'a Event<'a>>, force: bool) -> Result<()> {
        // a revision of a call in the same set is folded into the call (what
        // a bulk loader does; the stored result is the same)
        let (rows, revs) = fold(events);
        let docs: Vec<Value> = rows.iter().chain(revs.iter()).map(doc).collect();
        self.ingest(&docs, force)
    }

    /// Allocated bytes under `sub` (the WAL files are preallocated, sparse
    /// 128 MiB files: their logical size says nothing).
    fn dir_size(&self, sub: &str) -> u64 {
        crate::sys::dir_alloc_bytes(&self.data_dir.join(sub))
    }
}

impl TraceStore for Quickwit {
    fn name(&self) -> String {
        "quickwit 0.9.2 (ingest commit=force per append, BM25 on body)".into()
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        self.write(batch.iter(), true)
    }

    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        self.write(batches.iter().flatten(), false)
    }

    /// Wait until every document written is searchable (the auto commit
    /// runs every `commit_timeout_secs`, 60 s by default), at most 15 min.
    fn finish_load(&self) -> Result<()> {
        let want = self.docs.load(Ordering::Relaxed);
        let t0 = Instant::now();
        loop {
            let n = self.count_all()?;
            if n >= want {
                return Ok(());
            }
            if t0.elapsed() > Duration::from_secs(900) {
                return Err(format!("quickwit: {n} of {want} documents searchable after 15 min").into());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        let q = json!({"bool": {"should": [{"term": {"id": id}}, {"term": {"revises": id}}], "minimum_should_match": 1}});
        let (mut row, mut m) = (None, Latest::new());
        self.each(&q, &json!([{"id": "asc"}]), "", usize::MAX, &mut |s| {
            if u(s, "revises") == 0 {
                if u(s, "id") == id {
                    row = Some(row_of(s));
                }
            } else {
                note_revision(&mut m, s);
            }
        })?;
        let mut rows: Vec<Row> = row.into_iter().collect();
        apply(&mut rows, &m);
        Ok(rows.pop())
    }

    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        let q = json!({"bool": {"filter": [{"term": {"id": id}}, {"term": {"revises": 0}}]}});
        let v = self.es(&json!({"query": q, "size": 1}), "body,text")?;
        Ok(Self::hits(&v).first().map(|h| {
            let s = &h["_source"];
            super::docjson::slice(s["body"].as_str().or_else(|| s["text"].as_str()).unwrap_or(""), start, end)
        }))
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let (mut rows, mut m) = (Vec::new(), Latest::new());
        let q = json!({"term": {"session": session}});
        self.each(&q, &json!([{"seq": "asc"}, {"id": "asc"}]), "", usize::MAX, &mut |s| {
            if u(s, "revises") == 0 {
                rows.push(row_of(s));
            } else {
                note_revision(&mut m, s);
            }
        })?;
        apply(&mut rows, &m);
        rows.sort_by_key(|r| (r.seq, r.id));
        Ok(rows)
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        if from_us >= to_us || limit == 0 {
            return Ok(Vec::new());
        }
        let mut c = time_clauses(from_us, to_us);
        c.push(json!({"term": {"revises": 0}}));
        let mut rows = Vec::new();
        self.each(&json!({"bool": {"filter": c}}), &json!([{"ts": "asc"}, {"id": "asc"}]), "", limit, &mut |s| {
            rows.push(row_of(s))
        })?;
        self.finish_rows(rows)
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        if f.status.is_some() {
            return unsupported("filter by latest status", STATUS_WHY);
        }
        if f.from_us >= f.to_us || limit == 0 {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        self.each(&filter_query(f), &json!([{"ts": "asc"}, {"id": "asc"}]), "id", limit, &mut |s| ids.push(u(s, "id")))?;
        Ok(ids)
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        if f.status.is_some() {
            return unsupported("count by latest status", STATUS_WHY);
        }
        if f.from_us >= f.to_us {
            return Ok(0);
        }
        self.count_query(&filter_query(f))
    }

    /// BM25 over `body` (OR of the query words), the agent's events with
    /// ts <= as_of; ties newest first.
    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let words = query_words(query, char::is_alphanumeric, false, 64);
        if words.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let mut filter = vec![json!({"term": {"agent": agent}}), json!({"range": {"ts": {"lte": as_of_us}}})];
        if let Some(t) = rfc3339(as_of_us.saturating_add(1_000_000)) {
            filter.push(json!({"range": {"t": {"lte": t}}}));
        }
        let body = json!({
            "query": {"bool": {"filter": filter, "must": [{"match": {"body": words.join(" ")}}]}},
            "sort": [{"_score": {"order": "desc"}}, {"ts": {"order": "desc"}}],
            "size": k.min(PAGE),
        });
        let v = self.es(&body, "id")?;
        Ok(Self::hits(&v).iter().map(|h| u(&h["_source"], "id")).collect())
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
                a.tool_errors += (r.status == crate::events::ST_ERROR) as u64;
            }
        }
        Ok(a)
    }

    fn agg_tools(&self, _from_us: i64, _to_us: i64) -> Result<Vec<ToolAgg>> {
        unsupported("agg_tools", STATUS_WHY)
    }

    /// Terms aggregation on `session` with the sum of `tok` (tokens in +
    /// out), every session of the window returned (exactness checked), then
    /// ordered on the client (sum desc, session asc).
    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        if from_us >= to_us {
            return Ok(Vec::new());
        }
        let mut c = time_clauses(from_us, to_us);
        c.push(json!({"term": {"revises": 0}}));
        let body = json!({
            "query": {"bool": {"filter": c}}, "size": 0,
            "aggs": {"s": {"terms": {"field": "session", "size": 65_000, "shard_size": 65_000},
                           "aggs": {"t": {"sum": {"field": "tok"}}}}},
        });
        let v = self.es(&body, "")?;
        let agg = &v["aggregations"]["s"];
        if agg["sum_other_doc_count"].as_u64().unwrap_or(0) != 0 {
            return Err("quickwit: top_sessions window has more than 65,000 sessions (inexact)".into());
        }
        let mut out: Vec<(u64, u64)> = agg["buckets"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|b| (b["key"].as_f64().unwrap_or(0.0) as u64, b["t"]["value"].as_f64().unwrap_or(0.0).round() as u64))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.truncate(n);
        Ok(out)
    }

    fn delete_session(&self, _session: u64) -> Result<()> {
        unsupported(
            "delete_session",
            "Quickwit delete tasks run asynchronously and only on mature splits (maturation_period 48 h by default): \
             nothing is invisible on return",
        )
    }

    fn purge_before(&self, _ts_us: i64) -> Result<()> {
        unsupported(
            "purge_before",
            "Quickwit retention drops whole splits on a schedule; a delete task by time is asynchronous (see delete_session)",
        )
    }

    fn reclaim(&self) -> Result<()> {
        unsupported("reclaim", "no supported delete to make physical")
    }

    /// Every revision first (call -> newest), then every event in
    /// (session, seq) order, both by search_after paging.
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let mut m = Latest::new();
        self.each(
            &json!({"range": {"revises": {"gt": 0}}}),
            &json!([{"revises": "asc"}, {"id": "asc"}]),
            REV_FIELDS,
            usize::MAX,
            &mut |s| note_revision(&mut m, s),
        )?;
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        let mut err: Option<std::io::Error> = None;
        self.each(
            &json!({"term": {"revises": 0}}),
            &json!([{"session": "asc"}, {"seq": "asc"}]),
            "",
            usize::MAX,
            &mut |s| {
                if err.is_some() {
                    return;
                }
                let mut r = [row_of(s)];
                apply(&mut r, &m);
                buf.clear();
                crate::export_line(&r[0], &mut buf);
                if let Err(e) = out.write_all(&buf) {
                    err = Some(e);
                }
                n += 1;
                bytes += buf.len() as u64;
            },
        )?;
        if let Some(e) = err {
            return Err(e.into());
        }
        Ok((n, bytes))
    }

    /// Allocated bytes of the splits and metastore (`indexes/`), the ingest
    /// WAL (`wal/`) and the legacy ingest queue (`queues/`); the indexer's
    /// split cache (copies of recent splits kept for merges) is excluded and
    /// reported in details.
    fn disk_bytes(&self) -> Result<u64> {
        Ok(self.dir_size("indexes") + self.dir_size("wal") + self.dir_size("queues"))
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
        let splits = self
            .http
            .call("GET", &format!("/api/v1/indexes/{INDEX}/splits?split_states=Published"), "application/json", b"")
            .ok()
            .and_then(|(_, body)| serde_json::from_slice::<Value>(&body).ok())
            .and_then(|v| v["splits"].as_array().map(Vec::len));
        json!({
            "indexes_bytes": self.dir_size("indexes"), "wal_bytes": self.dir_size("wal"),
            "queues_bytes": self.dir_size("queues"),
            "indexer_split_cache_bytes": self.dir_size("indexer-split-cache"),
            "data_dir_bytes": crate::sys::dir_alloc_bytes(&self.data_dir),
            "data_dir_logical_bytes": crate::sys::dir_bytes(&self.data_dir),
            "published_splits": splits, "documents": self.count_all().ok(),
        })
    }
}

impl Drop for Quickwit {
    fn drop(&mut self) {
        if !self.keep && std::env::var_os("TRACEDB_KEEP_CONTAINER").is_none() {
            let _ = run_script("quickwit.sh", "stop");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc3339_formats_microseconds() {
        assert_eq!(super::rfc3339(0).unwrap(), "1970-01-01T00:00:00.000000Z");
        assert_eq!(super::rfc3339(1_788_220_800_000_001).unwrap(), "2026-09-01T00:00:00.000001Z");
        assert_eq!(super::rfc3339(951_782_400_000_000 + 86_399_999_999).unwrap(), "2000-02-29T23:59:59.999999Z");
        assert!(super::rfc3339(-1).is_none());
    }
}
