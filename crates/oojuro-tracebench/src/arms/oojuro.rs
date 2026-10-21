//! Oojuro arm: `oojuro_trace::TraceDb` in process (embedded, like SQLite).
//! Params: `dedupe=on|off` (1 KiB content-defined chunks), `dict=on|off`,
//! `fast=3` / `heavy=19` (zstd levels for flushed / merged runs),
//! `flush_mb=16` (per-agent fresh segment), `qthreads=1`, `fanout=8`,
//! `cols=sealed|hot` (compressed column blocks, default sealed),
//! `reclaim=erase|rewrite` (rewrite only segments with dead rows, or whole agent stores),
//! `deferred=on` (experimental: ack before sync; NOT the durable default),
//! `archive=on` (major compaction of every agent store after a bulk load),
//! `existing=on` (open the store already in the work directory),
//! `log=shared` (one write log under all agent stores: concurrent turns of
//! different agents share one sync; still durable per turn; default
//! `log=private`, a journal per agent), `log_mb=32` (shared log file size),
//! `log_prealloc=on|off` (shared log files zero-filled ahead of use, so a
//! sync flushes data only; default on), `log_max_mb=256` (shared log size
//! that triggers flushing the stores that pin its oldest file).
//! `records=dedupe|off` (store a repeated (tool, args, text) once per agent
//! store, default dedupe), `window=N` (distinct texts remembered per agent),
//! `starter=on|off` (flush runs and heavy runs under 4 MiB use the shipped
//! starter dictionary, default on),
//! `maxq=65536` (longest accepted search query in bytes),
//! `exact_entropy=on` (score every candidate for the routing entropy, the
//! engine default; off: entropy of the top k, same ids).
use super::Params;
use crate::{Event, Filter, Result, Row, SessionAgg, ToolAgg, TraceStore, events};
use oojuro_trace::{EventIn, EventOut, Options, TraceDb};
use std::path::{Path, PathBuf};

pub struct Oojuro {
    db: TraceDb,
    dir: PathBuf,
    label: String,
    /// Major-compact every agent store at the end of a bulk load.
    archive: bool,
}

pub fn open(work: &Path, p: &Params) -> Result<Box<dyn TraceStore>> {
    let opts = Options {
        archive_fast_level: p.num("fast", 3),
        archive_heavy_level: p.num("heavy", 19),
        dedupe_chunk: if p.flag("dedupe", true) { 1024 } else { 0 },
        dict: p.flag("dict", true),
        flush_text_bytes: p.num::<usize>("flush_mb", 16) << 20,
        merge_fanout: p.num("fanout", 8),
        query_threads: p.num("qthreads", 1),
        seal_columns: p.get("cols") != Some("hot"),
        erase_segments: p.get("reclaim") != Some("rewrite"),
        deferred_sync: p.flag("deferred", false),
        shared_merges: p.flag("shared_merges", true),
        column_cache_bytes: p.num::<usize>("colcache_mb", 128) << 20,
        max_query_bytes: p.num("maxq", 65_536),
        exact_entropy: p.flag("exact_entropy", false),
        shared_log: match p.get("log") {
            Some("private") => false,
            None | Some("shared") => true,
            Some(x) => return Err(format!("unknown oojuro log mode {x} (private|shared)").into()),
        },
        log_segment_bytes: p.num::<u64>("log_mb", 32) << 20,
        log_preallocate: p.flag("log_prealloc", true),
        log_max_bytes: p.num::<u64>("log_max_mb", 256) << 20,
        dedupe_records: p.get("records") != Some("off"),
        dedupe_window: p.num("window", 1 << 18),
        starter_dict: p.flag("starter", true),
    };
    let label = format!(
        "oojuro-trace v1 (nine v4 per agent{}, zstd {}/{} runs, dedupe {}, dict {}, flush {} MiB, cols {}, reclaim {}, records {}{}{})",
        match (opts.shared_log, opts.log_preallocate) {
            (false, _) => "",
            (true, true) => ", one shared write log (preallocated)",
            (true, false) => ", one shared write log (appending)",
        },
        opts.archive_fast_level,
        opts.archive_heavy_level,
        opts.dedupe_chunk,
        opts.dict,
        opts.flush_text_bytes >> 20,
        if opts.seal_columns { "sealed" } else { "hot" },
        if opts.erase_segments { "erase" } else { "rewrite" },
        if opts.dedupe_records { "dedupe" } else { "all" },
        if opts.starter_dict { ", starter dictionary" } else { "" },
        if opts.deferred_sync { ", DEFERRED SYNC (not durable per turn)" } else { "" }
    );
    let dir = work.join("store");
    // `existing=on` (reopen / crash recovery): the store must already exist;
    // TraceDb::open itself replays whatever is there either way.
    if p.flag("existing", false) && !dir.exists() {
        return Err(format!("no existing store at {}", dir.display()).into());
    }
    let archive = p.flag("archive", false);
    let label = if archive { format!("{label} + major compaction after load") } else { label };
    Ok(Box::new(Oojuro { db: TraceDb::open(&dir, opts)?, dir, label, archive }))
}

fn to_in<'a>(e: &Event<'a>) -> EventIn<'a> {
    EventIn {
        id: e.id,
        session: e.session,
        agent: e.agent,
        turn: e.turn,
        seq: e.seq,
        parent: e.parent,
        revises: e.revises,
        kind: e.kind,
        status: e.status,
        exit_code: e.exit_code,
        tokens_in: e.tokens_in,
        tokens_out: e.tokens_out,
        ts_us: e.ts_us,
        duration_us: e.duration_us,
        tool: e.tool,
        args: e.args,
        text: e.text,
        indexed: events::indexed_kind(e.kind),
    }
}

fn row(o: EventOut) -> Row {
    Row {
        id: o.id,
        session: o.session,
        agent: o.agent,
        turn: o.turn,
        seq: o.seq,
        parent: o.parent,
        kind: o.kind,
        status: o.status,
        exit_code: o.exit_code,
        tokens_in: o.tokens_in,
        tokens_out: o.tokens_out,
        ts_us: o.ts_us,
        duration_us: o.duration_us,
        tool: o.tool,
        args: o.args,
        text: o.text,
    }
}

fn filt(f: &Filter) -> oojuro_trace::Filter {
    oojuro_trace::Filter {
        agent: f.agent,
        kind: f.kind,
        tool: f.tool.clone(),
        status: f.status,
        from_us: f.from_us,
        to_us: f.to_us,
    }
}

impl TraceStore for Oojuro {
    fn name(&self) -> String {
        self.label.clone()
    }
    fn append(&self, batch: &[Event]) -> Result<()> {
        let v: Vec<EventIn> = batch.iter().map(to_in).collect();
        self.db.append(&v)
    }
    fn load(&self, batches: &[Vec<Event>]) -> Result<()> {
        // Bulk path: the chunk's turn batches become one durable journal batch
        // per agent (like COPY in one transaction).
        let v: Vec<Vec<EventIn>> = batches.iter().map(|b| b.iter().map(to_in).collect()).collect();
        self.db.append_many(&v)
    }
    fn append_many(&self, batches: &[Vec<Event>]) -> Result<()> {
        // One durable commit per agent store the batches touch (an agent's
        // store is the unit of atomicity, also when `log=shared` makes the
        // commits of several agents share one sync).
        let v: Vec<Vec<EventIn>> = batches.iter().map(|b| b.iter().map(to_in).collect()).collect();
        self.db.append_many(&v)
    }
    fn finish_load(&self) -> Result<()> {
        if self.archive {
            self.db.compact_archive(4)?;
        }
        self.db.flush()
    }
    fn get(&self, id: u64) -> Result<Option<Row>> {
        Ok(self.db.get(id)?.map(row))
    }
    fn get_range(&self, id: u64, start: usize, end: usize) -> Result<Option<Vec<u8>>> {
        self.db.text_range(id, start, end)
    }
    fn session(&self, session: u64) -> Result<Vec<Row>> {
        Ok(self.db.session(session)?.into_iter().map(row).collect())
    }
    fn time_range(&self, from_us: i64, to_us: i64, limit: usize) -> Result<Vec<Row>> {
        Ok(self.db.time_range(from_us, to_us, limit)?.into_iter().map(row).collect())
    }
    fn filter(&self, f: &Filter, limit: usize) -> Result<Vec<u64>> {
        self.db.filter(&filt(f), limit)
    }
    fn count(&self, f: &Filter) -> Result<u64> {
        self.db.count(&filt(f))
    }
    fn search(&self, agent: u32, query: &str, as_of_us: i64, k: usize) -> Result<Vec<u64>> {
        self.db.search(agent, query, as_of_us, k)
    }
    fn agg_session(&self, session: u64) -> Result<SessionAgg> {
        let (events, tokens_in, tokens_out, tool_calls, tool_errors, text_bytes) = self.db.agg_session(session);
        Ok(SessionAgg { events, tokens_in, tokens_out, tool_calls, tool_errors, text_bytes })
    }
    fn agg_tools(&self, from_us: i64, to_us: i64) -> Result<Vec<ToolAgg>> {
        Ok(self
            .db
            .agg_tools(from_us, to_us)
            .into_iter()
            .map(|(tool, calls, errors, duration_us)| ToolAgg { tool, calls, errors, duration_us })
            .collect())
    }
    fn top_sessions(&self, from_us: i64, to_us: i64, n: usize) -> Result<Vec<(u64, u64)>> {
        Ok(self.db.top_sessions(from_us, to_us, n))
    }
    fn delete_session(&self, session: u64) -> Result<()> {
        self.db.delete_session(session)
    }
    fn purge_before(&self, ts_us: i64) -> Result<()> {
        self.db.purge_before(ts_us)
    }
    fn reclaim(&self) -> Result<()> {
        self.db.reclaim()
    }
    fn export(&self, out: &mut dyn std::io::Write) -> Result<(u64, u64)> {
        let mut buf = Vec::new();
        let mut bytes = 0u64;
        let n = self.db.export_rows(|e| {
            buf.clear();
            crate::export_line(&row(e.clone()), &mut buf);
            out.write_all(&buf)?;
            bytes += buf.len() as u64;
            Ok(())
        })?;
        Ok((n, bytes))
    }
    fn disk_bytes(&self) -> Result<u64> {
        Ok(self.db.disk_bytes())
    }
    fn mem_bytes(&self) -> Result<u64> {
        Ok(crate::sys::rss_anon())
    }
    fn details(&self) -> serde_json::Value {
        let mut v = serde_json::Map::new();
        for (k, n) in self.db.disk_breakdown() {
            v.insert(format!("disk_{k}"), n.into());
        }
        let (sealed, hot, loc) = self.db.column_bytes();
        v.insert("columns_sealed_bytes".into(), sealed.into());
        v.insert("columns_hot_bytes".into(), hot.into());
        v.insert("columns_location_bytes".into(), loc.into());
        let (window, dups) = self.db.dedupe_stats();
        v.insert("stored_records".into(), self.db.stored_records().into());
        v.insert("dup_slots".into(), dups.into());
        v.insert("dedupe_window".into(), window.into());
        v.insert("rss_anon_before_trim".into(), crate::sys::rss_anon().into());
        oojuro_trace::trim_heap();
        v.insert("rss_anon_after_trim".into(), crate::sys::rss_anon().into());
        serde_json::Value::Object(v)
    }
    fn sync_counters(&self) -> Option<serde_json::Value> {
        let mut v = serde_json::json!({ "journal_syncs": oojuro_trace::journal_syncs() });
        if let Some(st) = self.db.shared_log_stats() {
            v["shared_log_rounds"] = st.rounds.into();
            v["shared_log_batches"] = st.batches.into();
        }
        Some(v)
    }
    fn deep_scan(&self, needles: &[Vec<u8>]) -> Result<Option<u64>> {
        Ok(Some(self.db.scan_decoded(needles)?))
    }
    fn data_paths(&self) -> Result<Vec<PathBuf>> {
        let _ = &self.dir;
        Ok(self.db.files())
    }
}
