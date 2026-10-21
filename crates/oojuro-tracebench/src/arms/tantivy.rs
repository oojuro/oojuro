//! Tantivy arm (`tantivy`, `tantivy:commit=periodic`): the embedded Rust
//! search library (0.27), one index in the run's work directory.
//!
//! One document per event. Text of indexed kinds (user, output, tool call,
//! tool result) goes into `text` (indexed with the `en_stem` tokenizer:
//! Tantivy's default English pipeline, simple tokenizer + lowercaser + long
//! token filter + English stemmer; frequencies, no positions; stored); text
//! of thinking and system events goes into `raw` (stored only). Numeric
//! fields are fast fields where a query sorts, filters or aggregates on them.
//!
//! Durability: `append` is one `IndexWriter::run` (deletes + adds) and one
//! `commit()` per turn batch under the single writer, which fsyncs the new
//! segment files, the directory and `meta.json` before it returns; the
//! reader is reloaded before `append` returns (read-your-writes).
//! `commit=periodic` (NON-DURABLE, not read-your-writes) only adds the batch
//! and commits every `commit_ms` (1000) from a background thread.
//!
//! A status revision is Tantivy's update: delete the call's document by its
//! `id` term and add it again with the new status, in the same commit.
//! Settings and their reasons: `docs/TRACEBENCH_BASELINES.md`.
use super::Params;
use super::pg::{fold, query_words};
use crate::events::{Event, ST_ERROR, TOOL_CALL, indexed_kind};
use crate::{Filter, Result, Row, SessionAgg, ToolAgg, TraceStore};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
    ops::Bound,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tantivy::{
    DocAddress, DocId, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, SegmentReader, TantivyDocument, Term,
    collector::{Count, DocSetCollector, TopDocs},
    columnar::{Column, StrColumn},
    indexer::UserOperation,
    query::{AllQuery, BooleanQuery, ConstScoreQuery, Occur, Query, RangeQuery, TermQuery},
    schema::{
        FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
    },
};

/// Indexing threads and total arena of the writer (split across threads).
const WRITER_THREADS: usize = 4;
const WRITER_ARENA: usize = 256 << 20;

#[derive(Clone, Copy)]
struct Fields {
    id: Field,
    session: Field,
    agent: Field,
    turn: Field,
    seq: Field,
    parent: Field,
    kind: Field,
    status: Field,
    exit_code: Field,
    tokens_in: Field,
    tokens_out: Field,
    ts: Field,
    duration: Field,
    tool: Field,
    args: Field,
    text: Field,
    raw: Field,
    text_len: Field,
}

fn schema() -> Schema {
    let mut s = Schema::builder();
    s.add_u64_field("id", INDEXED | FAST | STORED);
    s.add_u64_field("session", INDEXED | FAST | STORED);
    s.add_u64_field("agent", INDEXED | FAST | STORED);
    s.add_u64_field("turn", STORED);
    s.add_u64_field("seq", FAST | STORED);
    s.add_u64_field("parent", STORED);
    s.add_u64_field("kind", INDEXED | FAST | STORED);
    s.add_u64_field("status", INDEXED | FAST | STORED);
    s.add_i64_field("exit_code", STORED);
    s.add_u64_field("tokens_in", FAST | STORED);
    s.add_u64_field("tokens_out", FAST | STORED);
    s.add_i64_field("ts", INDEXED | FAST | STORED);
    s.add_i64_field("duration", FAST | STORED);
    s.add_text_field("tool", STRING | FAST | STORED);
    s.add_text_field("args", STORED);
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default().set_tokenizer("en_stem").set_index_option(IndexRecordOption::WithFreqs),
        )
        .set_stored();
    s.add_text_field("text", text);
    s.add_text_field("raw", STORED);
    s.add_u64_field("text_len", FAST);
    s.build()
}

fn fields(s: &Schema) -> Result<Fields> {
    let f = |n: &str| s.get_field(n);
    Ok(Fields {
        id: f("id")?,
        session: f("session")?,
        agent: f("agent")?,
        turn: f("turn")?,
        seq: f("seq")?,
        parent: f("parent")?,
        kind: f("kind")?,
        status: f("status")?,
        exit_code: f("exit_code")?,
        tokens_in: f("tokens_in")?,
        tokens_out: f("tokens_out")?,
        ts: f("ts")?,
        duration: f("duration")?,
        tool: f("tool")?,
        args: f("args")?,
        text: f("text")?,
        raw: f("raw")?,
        text_len: f("text_len")?,
    })
}

impl Fields {
    fn doc(&self, r: &Row) -> TantivyDocument {
        let mut d = TantivyDocument::default();
        d.add_u64(self.id, r.id);
        d.add_u64(self.session, r.session);
        d.add_u64(self.agent, r.agent as u64);
        d.add_u64(self.turn, r.turn as u64);
        d.add_u64(self.seq, r.seq as u64);
        d.add_u64(self.parent, r.parent);
        d.add_u64(self.kind, r.kind as u64);
        d.add_u64(self.status, r.status as u64);
        if let Some(x) = r.exit_code {
            d.add_i64(self.exit_code, x as i64);
        }
        d.add_u64(self.tokens_in, r.tokens_in as u64);
        d.add_u64(self.tokens_out, r.tokens_out as u64);
        d.add_i64(self.ts, r.ts_us);
        d.add_i64(self.duration, r.duration_us);
        d.add_text(self.tool, &r.tool);
        d.add_text(self.args, &r.args);
        d.add_text(if indexed_kind(r.kind) { self.text } else { self.raw }, &r.text);
        d.add_u64(self.text_len, r.text.len() as u64);
        d
    }

    fn row(&self, d: &TantivyDocument) -> Row {
        let u = |f: Field| d.get_first(f).and_then(|v| v.as_u64()).unwrap_or(0);
        let i = |f: Field| d.get_first(f).and_then(|v| v.as_i64());
        let s = |f: Field| d.get_first(f).and_then(|v| v.as_str()).map(str::to_owned);
        Row {
            id: u(self.id),
            session: u(self.session),
            agent: u(self.agent) as u32,
            turn: u(self.turn) as u32,
            seq: u(self.seq) as u32,
            parent: u(self.parent),
            kind: u(self.kind) as u8,
            status: u(self.status) as u8,
            exit_code: i(self.exit_code).map(|x| x as i32),
            tokens_in: u(self.tokens_in) as u32,
            tokens_out: u(self.tokens_out) as u32,
            ts_us: i(self.ts).unwrap_or(0),
            duration_us: i(self.duration).unwrap_or(0),
            tool: s(self.tool).unwrap_or_default(),
            args: s(self.args).unwrap_or_default(),
            text: s(self.text).or_else(|| s(self.raw)).unwrap_or_default(),
        }
    }

    fn u64_term(&self, f: Field, v: u64) -> Box<dyn Query> {
        Box::new(TermQuery::new(Term::from_field_u64(f, v), IndexRecordOption::Basic))
    }
}

/// Fast-field columns of one segment (what aggregates read).
struct Cols {
    session: Column<u64>,
    seq: Column<u64>,
    kind: Column<u64>,
    status: Column<u64>,
    tokens_in: Column<u64>,
    tokens_out: Column<u64>,
    duration: Column<i64>,
    text_len: Column<u64>,
    tool: Option<StrColumn>,
}

impl Cols {
    fn open(r: &SegmentReader) -> Result<Self> {
        let ff = r.fast_fields();
        Ok(Self {
            session: ff.u64("session")?,
            seq: ff.u64("seq")?,
            kind: ff.u64("kind")?,
            status: ff.u64("status")?,
            tokens_in: ff.u64("tokens_in")?,
            tokens_out: ff.u64("tokens_out")?,
            duration: ff.i64("duration")?,
            text_len: ff.u64("text_len")?,
            tool: ff.str("tool")?,
        })
    }
    fn tool(&self, doc: DocId, out: &mut String) -> Result<()> {
        out.clear();
        if let Some(c) = &self.tool
            && let Some(ord) = c.term_ords(doc).next()
        {
            c.ord_to_str(ord, out)?;
        }
        Ok(())
    }
}

/// Writer state. `pending` holds rows added but not committed yet
/// (`commit=periodic` only) so a revision can find its call.
struct W {
    writer: IndexWriter,
    pending: HashMap<u64, Row>,
    dirty: bool,
}

struct Inner {
    dir: PathBuf,
    index: Index,
    f: Fields,
    reader: IndexReader,
    w: Mutex<Option<W>>,
    commits: AtomicU64,
}

/// The periodic committer: stop flag + wakeup, and its thread.
type Committer = (Arc<(Mutex<bool>, Condvar)>, JoinHandle<()>);

pub struct Tantivy {
    inner: Arc<Inner>,
    periodic: Option<Committer>,
    commit_ms: u64,
}

/// Params: `commit=periodic` (NON-DURABLE: commit from a background thread
/// every `commit_ms`, default 1000), `existing=on` (open the index already in
/// `work`).
pub fn open(work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let dir = work.join("tantivy");
    let existing = params.flag("existing", false);
    let index = if existing {
        Index::open_in_dir(&dir)?
    } else {
        std::fs::create_dir_all(&dir)?;
        Index::create_in_dir(&dir, schema())?
    };
    let f = fields(&index.schema())?;
    let writer = index.writer_with_num_threads(WRITER_THREADS, WRITER_ARENA)?;
    let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
    let inner = Arc::new(Inner {
        dir,
        index,
        f,
        reader,
        w: Mutex::new(Some(W { writer, pending: HashMap::new(), dirty: false })),
        commits: AtomicU64::new(0),
    });
    let commit_ms = params.num("commit_ms", 1000u64);
    let periodic = match params.get("commit") {
        None | Some("batch") => None,
        Some("periodic") => {
            let stop = Arc::new((Mutex::new(false), Condvar::new()));
            let (s, i) = (Arc::clone(&stop), Arc::clone(&inner));
            let h = std::thread::spawn(move || {
                let (lock, cv) = &*s;
                let mut stopped = lock.lock().unwrap();
                while !*stopped {
                    stopped = cv.wait_timeout(stopped, Duration::from_millis(commit_ms)).unwrap().0;
                    if let Err(e) = i.commit(false) {
                        eprintln!("tantivy periodic commit failed: {e}");
                    }
                }
            });
            Some((stop, h))
        }
        Some(x) => return Err(format!("tantivy: unknown commit={x} (batch|periodic)").into()),
    };
    Ok(Box::new(Tantivy { inner, periodic, commit_ms }))
}

impl Inner {
    fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    /// Commit (if anything is uncommitted or `force`), then make it visible.
    fn commit(&self, force: bool) -> Result<()> {
        let mut g = self.w.lock().unwrap();
        let w = g.as_mut().ok_or("tantivy writer closed")?;
        if !w.dirty && !force {
            return Ok(());
        }
        w.writer.commit()?;
        w.pending.clear();
        w.dirty = false;
        self.commits.fetch_add(1, Ordering::Relaxed);
        self.reader.reload()?;
        Ok(())
    }

    fn lookup(&self, s: &Searcher, id: u64) -> Result<Option<(DocAddress, TantivyDocument)>> {
        let q = TermQuery::new(Term::from_field_u64(self.f.id, id), IndexRecordOption::Basic);
        let top = s.search(&q, &TopDocs::with_limit(1).order_by_score())?;
        Ok(match top.first() {
            Some((_, a)) => Some((*a, s.doc(*a)?)),
            None => None,
        })
    }

    /// Add `rows` and apply `revs` (revisions of calls not in `rows`) as one
    /// operation group; commit it unless `periodic`.
    fn write(&self, rows: &[Event], revs: &[Event], periodic: bool) -> Result<()> {
        let mut g = self.w.lock().unwrap();
        let w = g.as_mut().ok_or("tantivy writer closed")?;
        let s = self.searcher();
        let mut ops: Vec<UserOperation> = Vec::with_capacity(rows.len() + 2 * revs.len());
        let mut updated: Vec<Row> = Vec::new();
        for r in revs {
            let call = match w.pending.get(&r.revises) {
                Some(row) => Some(row.clone()),
                None => self.lookup(&s, r.revises)?.map(|(_, d)| self.f.row(&d)),
            };
            // A revision of a deleted (purged) call has nothing to update.
            let Some(mut call) = call else { continue };
            call.status = r.status;
            call.exit_code = r.exit_code;
            call.duration_us = r.duration_us;
            ops.push(UserOperation::Delete(Term::from_field_u64(self.f.id, call.id)));
            ops.push(UserOperation::Add(self.f.doc(&call)));
            updated.push(call);
        }
        for e in rows {
            ops.push(UserOperation::Add(self.f.doc(&Row::from_event(e))));
        }
        w.writer.run(ops)?;
        if periodic {
            for r in rows {
                w.pending.insert(r.id, Row::from_event(r));
            }
            for r in updated {
                w.pending.insert(r.id, r);
            }
            w.dirty = true;
        } else {
            w.writer.commit()?;
            self.commits.fetch_add(1, Ordering::Relaxed);
            self.reader.reload()?;
        }
        Ok(())
    }

    fn filter_query(&self, f: &Filter) -> BooleanQuery {
        let mut must: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(self.f.ts, f.from_us)),
                Bound::Excluded(Term::from_field_i64(self.f.ts, f.to_us)),
            )),
        )];
        if let Some(a) = f.agent {
            must.push((Occur::Must, self.f.u64_term(self.f.agent, a as u64)));
        }
        if let Some(k) = f.kind {
            must.push((Occur::Must, self.f.u64_term(self.f.kind, k as u64)));
        }
        if let Some(t) = &f.tool {
            must.push((
                Occur::Must,
                Box::new(TermQuery::new(Term::from_field_text(self.f.tool, t), IndexRecordOption::Basic)),
            ));
        }
        if let Some(st) = f.status {
            must.push((Occur::Must, self.f.u64_term(self.f.status, st as u64)));
        }
        BooleanQuery::new(must)
    }

    /// Top `limit` matches of `q` in (ts, id) order.
    fn by_time(&self, s: &Searcher, q: &dyn Query, limit: usize) -> Result<Vec<DocAddress>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let top = TopDocs::with_limit(limit).order_by(|seg: &SegmentReader| {
            let ff = seg.fast_fields();
            let ts = ff.i64("ts").expect("ts fast field");
            let id = ff.u64("id").expect("id fast field");
            move |doc: DocId| Reverse((ts.first(doc).unwrap_or(0), id.first(doc).unwrap_or(0)))
        });
        Ok(s.search(q, &top)?.into_iter().map(|(_, a)| a).collect())
    }

    /// Every matching document, grouped by segment.
    fn docs(&self, s: &Searcher, q: &dyn Query) -> Result<BTreeMap<u32, Vec<DocId>>> {
        let mut by: BTreeMap<u32, Vec<DocId>> = BTreeMap::new();
        for a in s.search(q, &DocSetCollector)? {
            by.entry(a.segment_ord).or_default().push(a.doc_id);
        }
        for v in by.values_mut() {
            v.sort_unstable();
        }
        Ok(by)
    }
}

impl Tantivy {
    fn periodic(&self) -> bool {
        self.periodic.is_some()
    }
}

impl Drop for Tantivy {
    fn drop(&mut self) {
        if let Some((stop, h)) = self.periodic.take() {
            *stop.0.lock().unwrap() = true;
            stop.1.notify_all();
            let _ = h.join();
        }
        if let Some(w) = self.inner.w.lock().unwrap().take() {
            let _ = w.writer.wait_merging_threads();
        }
    }
}

impl TraceStore for Tantivy {
    fn name(&self) -> String {
        if self.periodic() {
            format!("tantivy 0.27.0 (en_stem BM25, commit every {} ms: NON-DURABLE)", self.commit_ms)
        } else {
            "tantivy 0.27.0 (en_stem BM25, commit per batch)".into()
        }
    }

    fn append(&self, batch: &[Event]) -> Result<()> {
        let (rows, revs) = fold(batch.iter());
        self.inner.write(&rows, &revs, self.periodic())
    }

    /// One operation group and one commit per chunk of turn batches.
    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        let (rows, revs) = fold(batches.iter().flatten());
        self.inner.write(&rows, &revs, false)
    }

    fn finish_load(&self) -> Result<()> {
        self.inner.commit(false)
    }

    fn get(&self, id: u64) -> Result<Option<Row>> {
        let s = self.inner.searcher();
        Ok(self.inner.lookup(&s, id)?.map(|(_, d)| self.inner.f.row(&d)))
    }

    /// The stored document is read (its doc-store block decompressed) and
    /// the bytes are cut client-side; Tantivy has no partial field reads.
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.get(id)?.map(|r| {
            let t = r.text.as_bytes();
            let e = end.min(t.len());
            t[start.min(e)..e].to_vec()
        }))
    }

    fn session(&self, session: u64) -> Result<Vec<Row>> {
        let s = self.inner.searcher();
        let q = TermQuery::new(Term::from_field_u64(self.inner.f.session, session), IndexRecordOption::Basic);
        let mut v = Vec::new();
        for (seg, docs) in self.inner.docs(&s, &q)? {
            for d in docs {
                v.push(self.inner.f.row(&s.doc(DocAddress::new(seg, d))?));
            }
        }
        v.sort_by_key(|r| r.seq);
        Ok(v)
    }

    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        let s = self.inner.searcher();
        let q = self.inner.filter_query(&Filter { from_us, to_us, ..Default::default() });
        self.inner.by_time(&s, &q, limit)?.into_iter().map(|a| Ok(self.inner.f.row(&s.doc(a)?))).collect()
    }

    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        let s = self.inner.searcher();
        let q = self.inner.filter_query(f);
        let mut out = Vec::new();
        let mut cols: HashMap<u32, Column<u64>> = HashMap::new();
        for a in self.inner.by_time(&s, &q, limit)? {
            let c = match cols.entry(a.segment_ord) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(s.segment_reader(a.segment_ord).fast_fields().u64("id")?)
                }
            };
            out.push(c.first(a.doc_id).unwrap_or(0));
        }
        Ok(out)
    }

    fn count(&self, f: &Filter) -> Result<u64> {
        let s = self.inner.searcher();
        Ok(s.search(&self.inner.filter_query(f), &Count)? as u64)
    }

    /// BM25 over `text` (OR of the query's `en_stem` terms), restricted to
    /// the agent and `ts <= as_of` by zero-scored filter clauses; top-k with
    /// Tantivy's block-max pruning, then ties in the k ordered newest first.
    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        let f = self.inner.f;
        let mut analyzer = self.inner.index.tokenizer_for_field(f.text)?;
        let mut terms: Vec<String> = Vec::new();
        for w in query_words(query, char::is_alphanumeric, false, 64) {
            let mut ts = analyzer.token_stream(&w);
            while ts.advance() {
                let t = ts.token().text.clone();
                if !terms.contains(&t) {
                    terms.push(t);
                }
            }
        }
        if terms.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let should: Vec<(Occur, Box<dyn Query>)> = terms
            .iter()
            .map(|t| {
                let q: Box<dyn Query> =
                    Box::new(TermQuery::new(Term::from_field_text(f.text, t), IndexRecordOption::WithFreqs));
                (Occur::Should, q)
            })
            .collect();
        let q = BooleanQuery::new(vec![
            (Occur::Must, Box::new(BooleanQuery::new(should))),
            (Occur::Must, Box::new(ConstScoreQuery::new(f.u64_term(f.agent, agent as u64), 0.0))),
            (
                Occur::Must,
                Box::new(ConstScoreQuery::new(
                    Box::new(RangeQuery::new(Bound::Unbounded, Bound::Included(Term::from_field_i64(f.ts, as_of_us)))),
                    0.0,
                )),
            ),
        ]);
        let s = self.inner.searcher();
        let top = s.search(&q, &TopDocs::with_limit(k).order_by_score())?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, a) in top {
            let ff = s.segment_reader(a.segment_ord).fast_fields();
            let ts = ff.i64("ts")?.first(a.doc_id).unwrap_or(0);
            let id = ff.u64("id")?.first(a.doc_id).unwrap_or(0);
            hits.push((score, ts, id));
        }
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
        Ok(hits.into_iter().map(|h| h.2).collect())
    }

    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        let s = self.inner.searcher();
        let q = TermQuery::new(Term::from_field_u64(self.inner.f.session, session), IndexRecordOption::Basic);
        let mut a = SessionAgg::default();
        for (seg, docs) in self.inner.docs(&s, &q)? {
            let c = Cols::open(s.segment_reader(seg))?;
            for d in docs {
                a.events += 1;
                a.tokens_in += c.tokens_in.first(d).unwrap_or(0);
                a.tokens_out += c.tokens_out.first(d).unwrap_or(0);
                a.text_bytes += c.text_len.first(d).unwrap_or(0);
                if c.kind.first(d) == Some(TOOL_CALL as u64) {
                    a.tool_calls += 1;
                    a.tool_errors += (c.status.first(d) == Some(ST_ERROR as u64)) as u64;
                }
            }
        }
        Ok(a)
    }

    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        let s = self.inner.searcher();
        let q = self.inner.filter_query(&Filter { kind: Some(TOOL_CALL), from_us, to_us, ..Default::default() });
        let mut m: BTreeMap<Vec<u8>, ToolAgg> = BTreeMap::new();
        let mut tool = String::new();
        for (seg, docs) in self.inner.docs(&s, &q)? {
            let c = Cols::open(s.segment_reader(seg))?;
            for d in docs {
                c.tool(d, &mut tool)?;
                let e = m
                    .entry(tool.as_bytes().to_vec())
                    .or_insert_with(|| ToolAgg { tool: tool.clone(), ..Default::default() });
                e.calls += 1;
                e.errors += (c.status.first(d) == Some(ST_ERROR as u64)) as u64;
                e.duration_us += c.duration.first(d).unwrap_or(0);
            }
        }
        Ok(m.into_values().collect())
    }

    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        let s = self.inner.searcher();
        let q = self.inner.filter_query(&Filter { from_us, to_us, ..Default::default() });
        let mut m: HashMap<u64, u64> = HashMap::new();
        for (seg, docs) in self.inner.docs(&s, &q)? {
            let c = Cols::open(s.segment_reader(seg))?;
            for d in docs {
                *m.entry(c.session.first(d).unwrap_or(0)).or_default() +=
                    c.tokens_in.first(d).unwrap_or(0) + c.tokens_out.first(d).unwrap_or(0);
            }
        }
        let mut v: Vec<(u64, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        Ok(v)
    }

    fn delete_session(&self, session: u64) -> Result<()> {
        self.delete(Box::new(TermQuery::new(
            Term::from_field_u64(self.inner.f.session, session),
            IndexRecordOption::Basic,
        )))
    }

    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.delete(Box::new(RangeQuery::new(
            Bound::Unbounded,
            Bound::Excluded(Term::from_field_i64(self.inner.f.ts, ts_us)),
        )))
    }

    /// Wait for in-flight merges (a new writer replaces the old one), merge
    /// every segment into one (deleted documents are dropped, the doc store
    /// and postings are rewritten from live documents), reload the reader and
    /// garbage-collect every file no longer referenced by `meta.json`.
    fn reclaim(&self) -> Result<()> {
        let mut g = self.inner.w.lock().unwrap();
        let old = g.take().ok_or("tantivy writer closed")?;
        if old.dirty {
            *g = Some(old);
            return Err("tantivy reclaim with uncommitted documents".into());
        }
        old.writer.wait_merging_threads()?;
        let mut writer = self.inner.index.writer_with_num_threads(WRITER_THREADS, WRITER_ARENA)?;
        let ids = self.inner.index.searchable_segment_ids()?;
        if ids.len() > 1 || self.inner.searcher().segment_readers().iter().any(|r| r.has_deletes()) {
            writer.merge(&ids).wait()?;
        }
        self.inner.reader.reload()?;
        writer.garbage_collect_files().wait()?;
        *g = Some(W { writer, pending: HashMap::new(), dirty: false });
        Ok(())
    }

    /// Every live document in (session, seq) order: keys from fast fields,
    /// sorted, then each stored document.
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let s = self.inner.searcher();
        let mut keys: Vec<(u64, u64, DocAddress)> = Vec::new();
        for (seg, docs) in self.inner.docs(&s, &AllQuery)? {
            let c = Cols::open(s.segment_reader(seg))?;
            for d in docs {
                keys.push((c.session.first(d).unwrap_or(0), c.seq.first(d).unwrap_or(0), DocAddress::new(seg, d)));
            }
        }
        keys.sort_unstable_by_key(|k| (k.0, k.1));
        let (mut n, mut bytes, mut buf) = (0u64, 0u64, Vec::new());
        for (_, _, a) in keys {
            buf.clear();
            crate::export_line(&self.inner.f.row(&s.doc(a)?), &mut buf);
            out.write_all(&buf)?;
            n += 1;
            bytes += buf.len() as u64;
        }
        Ok((n, bytes))
    }

    fn disk_bytes(&self) -> Result<u64> {
        Ok(crate::sys::dir_bytes(&self.inner.dir))
    }

    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::rss_anon())
    }

    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = Vec::new();
        crate::sys::files_under(&self.inner.dir, &mut v);
        Ok(v)
    }

    fn details(&self) -> serde_json::Value {
        let s = self.inner.searcher();
        serde_json::json!({
            "segments": s.segment_readers().len(),
            "docs": s.num_docs(),
            "commits": self.inner.commits.load(Ordering::Relaxed),
        })
    }

    fn sync_counters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"commits": self.inner.commits.load(Ordering::Relaxed)}))
    }
}

impl Tantivy {
    /// Delete matching documents and commit (durable, invisible on return);
    /// in `commit=periodic` mode the delete still commits at once.
    fn delete(&self, q: Box<dyn Query>) -> Result<()> {
        let mut g = self.inner.w.lock().unwrap();
        let w = g.as_mut().ok_or("tantivy writer closed")?;
        w.writer.delete_query(q)?;
        w.writer.commit()?;
        w.pending.clear();
        w.dirty = false;
        self.inner.commits.fetch_add(1, Ordering::Relaxed);
        self.inner.reader.reload()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{OUTPUT, ST_ERROR, ST_PENDING, THINKING, TOOL_CALL, TOOL_RESULT, USER};

    #[allow(clippy::too_many_arguments)]
    fn ev<'a>(id: u64, seq: u32, kind: u8, status: u8, ts: i64, tool: &'a str, text: &'a str) -> Event<'a> {
        Event {
            id,
            session: 9,
            agent: 1,
            turn: seq,
            seq,
            parent: 0,
            revises: 0,
            kind,
            status,
            exit_code: None,
            tokens_in: 3,
            tokens_out: 4,
            ts_us: ts,
            duration_us: 0,
            tool,
            args: "",
            text,
        }
    }

    fn store(spec: &str) -> (PathBuf, Box<dyn TraceStore>) {
        let dir = std::env::temp_dir().join(format!("tracebench-tantivy-{}-{}", std::process::id(), spec.len()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = crate::arms::open(spec, &dir).unwrap();
        (dir, s)
    }

    #[test]
    fn append_revise_search_delete_reclaim() {
        let (dir, s) = store("tantivy");
        s.append(&[
            ev(1, 0, USER, 0, 100, "", "Please run the failing tests"),
            ev(2, 1, THINKING, 0, 110, "", "secret plan about tests"),
            ev(3, 2, TOOL_CALL, ST_PENDING, 120, "bash", "pytest -x tests"),
        ])
        .unwrap();
        let mut rev = ev(5, 0, TOOL_CALL, ST_ERROR, 140, "bash", "");
        rev.revises = 3;
        rev.exit_code = Some(1);
        rev.duration_us = 77;
        s.append(&[ev(4, 3, TOOL_RESULT, 0, 140, "bash", "2 tests failed"), rev, ev(6, 4, OUTPUT, 0, 150, "", "fixed")])
            .unwrap();
        // the revision is applied and is not an event
        let call = s.get(3).unwrap().unwrap();
        assert_eq!((call.status, call.exit_code, call.duration_us), (ST_ERROR, Some(1), 77));
        assert_eq!(call.text, "pytest -x tests");
        assert!(s.get(5).unwrap().is_none());
        assert_eq!(s.session(9).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 6]);
        assert_eq!(s.get(2).unwrap().unwrap().text, "secret plan about tests");
        assert_eq!(s.get_range(4, 2, 7).unwrap().unwrap(), b"tests");
        // stemmed OR search over indexed kinds only, as of a time, one agent
        let hits = s.search(1, "running test", 1000, 10).unwrap();
        assert!(hits.contains(&1) && hits.contains(&3) && hits.contains(&4) && !hits.contains(&2), "{hits:?}");
        assert!(!s.search(1, "running test", 130, 10).unwrap().contains(&4));
        assert!(s.search(2, "test", 1000, 10).unwrap().is_empty());
        let f = Filter { kind: Some(TOOL_CALL), status: Some(ST_ERROR), from_us: 0, to_us: 1000, ..Default::default() };
        assert_eq!(s.filter(&f, 10).unwrap(), vec![3]);
        assert_eq!(s.time_range(110, 150, 2).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 3]);
        let a = s.agg_session(9).unwrap();
        assert_eq!((a.events, a.tool_calls, a.tool_errors, a.tokens_in), (5, 1, 1, 15));
        assert_eq!(s.agg_tools(0, 1000).unwrap()[0].duration_us, 77);
        let mut out = Vec::new();
        assert_eq!(s.export(&mut out).unwrap().0, 5);
        s.purge_before(115).unwrap();
        assert_eq!(s.count(&Filter { from_us: 0, to_us: 1000, ..Default::default() }).unwrap(), 3);
        s.delete_session(9).unwrap();
        s.reclaim().unwrap();
        assert!(s.session(9).unwrap().is_empty());
        assert_eq!(s.export(&mut out).unwrap().0, 0);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn periodic_commit_is_not_read_your_writes_but_lands() {
        let (dir, s) = store("tantivy:commit=periodic,commit_ms=50");
        s.append(&[ev(1, 0, USER, 0, 100, "", "hello")]).unwrap();
        let t0 = std::time::Instant::now();
        while s.get(1).unwrap().is_none() {
            assert!(t0.elapsed() < Duration::from_secs(10), "periodic commit never landed");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
