//! TraceBench runner: one arm, one suite, one run per invocation (the
//! driver script wraps each in its own memory-capped scope and the bench
//! lock). Appends one JSON line to `--out`.
//!
//! Suites
//! - `ingest`    durable turn-batch appends from W concurrent writers into an
//!               empty store (ingest-heavy mix).
//! - `reads`     bulk load, then point, session, scan, filter, aggregate and
//!               export operations (dashboard/analytics mix), then `purge`.
//! - `turnloop`  load all but the last fraction, then W agents replay the rest:
//!               search top-20, fetch 20 excerpts, append the turn (durable).
//! - `purge`     load, delete sessions, purge older than N days, reclaim, and
//!               scan the store files for deleted text (retention mix).
//! - `verify`    untimed: compares every deterministic answer with the oracle.
//! - `ycsb`      traditional-database suite: YCSB workloads A-F over the same
//!               trait (load, reopen, client-thread sweeps, durable and
//!               batched commits, writer sweep, SIGKILL crash recovery);
//!               `--ycsb-*` flags, see `oojuro_tracebench::ycsb` and the
//!               "Traditional suite (ycsb)" section of `docs/TRACEBENCH.md`.
use oojuro_tracebench::{
    Event, Filter, Result, TraceStore, arms,
    arms::oracle::Oracle,
    events::{self, Base, Tier},
    stats::Lat,
    sys, ycsb,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

struct Args {
    suite: String,
    arm: String,
    data: PathBuf,
    tier: u32,
    run: u32,
    work: PathBuf,
    out: PathBuf,
    writers: usize,
    ingest_batches: usize,
    turnloop_frac: f64,
    queries: usize,
    seed: u64,
    keep: bool,
    load_threads: usize,
    purge_days: i64,
    delete_sessions: usize,
    ycsb: ycsb::Config,
}

fn args() -> Args {
    let mut a = Args {
        suite: String::new(),
        arm: String::new(),
        data: PathBuf::new(),
        tier: 1,
        run: 0,
        work: PathBuf::new(),
        out: PathBuf::from("tracebench.jsonl"),
        writers: 16,
        ingest_batches: 0,
        turnloop_frac: 0.1,
        queries: 300,
        seed: 1,
        keep: false,
        load_threads: 4,
        purge_days: 7,
        delete_sessions: 100,
        ycsb: ycsb::Config::default(),
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    a.suite = v.first().cloned().unwrap_or_default();
    let mut i = 1;
    while i < v.len() {
        let val = || v.get(i + 1).cloned().unwrap_or_default();
        match v[i].as_str() {
            "--arm" => a.arm = val(),
            "--data" => a.data = val().into(),
            "--tier" => a.tier = val().parse().unwrap(),
            "--run" => a.run = val().parse().unwrap(),
            "--work" => a.work = val().into(),
            "--out" => a.out = val().into(),
            "--writers" => a.writers = val().parse().unwrap(),
            "--ingest-batches" => a.ingest_batches = val().parse().unwrap(),
            "--turnloop-frac" => a.turnloop_frac = val().parse().unwrap(),
            "--queries" => a.queries = val().parse().unwrap(),
            "--seed" => a.seed = val().parse().unwrap(),
            "--load-threads" => a.load_threads = val().parse().unwrap(),
            "--purge-days" => a.purge_days = val().parse().unwrap(),
            "--delete-sessions" => a.delete_sessions = val().parse().unwrap(),
            "--keep" => {
                a.keep = true;
                i += 1;
                continue;
            }
            "--ycsb-check" => {
                a.ycsb.check = true;
                i += 1;
                continue;
            }
            x => {
                if !a.ycsb.set(x, &val()) {
                    panic!("unknown flag {x}")
                }
            }
        }
        i += 2;
    }
    a.ycsb.seed = a.seed;
    a.ycsb.load_threads = a.load_threads;
    a.ycsb.data = a.data.clone();
    a.ycsb.tier = a.tier;
    a
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Operations an arm answered with `Unsupported` (op -> reason). Suites skip
/// them and report them under `unsupported` instead of failing the run.
#[derive(Default)]
struct Skips(BTreeMap<String, String>);

impl Skips {
    /// `Ok(Some(v))` on success, `Ok(None)` if the arm does not support the
    /// operation (recorded), the error otherwise.
    fn take<T>(&mut self, op: &str, r: Result<T>) -> Result<Option<T>> {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(e) => match oojuro_tracebench::unsupported_reason(&e) {
                Some(why) => {
                    self.0.entry(op.to_string()).or_insert(why);
                    Ok(None)
                }
                None => Err(e),
            },
        }
    }
    fn json(&self) -> Value {
        json!(self.0)
    }
}

const HOUR: i64 = 3_600_000_000;
const DAY: i64 = 24 * HOUR;

/// Bulk-load plan entries `range` with `threads` loaders (chunks of 256
/// batches, dealt by session so each session stays in order).
fn bulk_load(store: &dyn TraceStore, tier: &Tier, range: std::ops::Range<usize>, threads: usize) -> Result<Value> {
    let t0 = Instant::now();
    let threads = threads.max(1);
    let err: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|s| {
        for w in 0..threads {
            let range = range.clone();
            let err = &err;
            s.spawn(move || {
                let mut chunk: Vec<Vec<Event>> = Vec::new();
                for i in range {
                    if tier.plan[i].session as usize % threads != w {
                        continue;
                    }
                    let mut b = Vec::new();
                    tier.batch(i, &mut b);
                    chunk.push(b);
                    if chunk.len() == 256 {
                        if let Err(e) = store.load(&chunk) {
                            *err.lock().unwrap() = Some(e.to_string());
                            return;
                        }
                        chunk.clear();
                    }
                }
                if !chunk.is_empty() {
                    if let Err(e) = store.load(&chunk) {
                        *err.lock().unwrap() = Some(e.to_string());
                    }
                }
            });
        }
    });
    if let Some(e) = err.into_inner().unwrap() {
        return Err(e.into());
    }
    let loaded = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    store.finish_load()?;
    let settle = t1.elapsed().as_secs_f64();
    let events: u64 = tier.plan[range.clone()].iter().map(|e| e.n as u64).sum();
    Ok(json!({"batches": range.len(), "events": events, "load_s": loaded, "finish_s": settle,
              "events_per_s": events as f64 / (loaded + settle)}))
}

fn sampler(store: &Arc<dyn TraceStore>) -> sys::PeakSampler {
    let s = Arc::clone(store);
    sys::PeakSampler::start(Arc::new(move || s.mem_bytes().unwrap_or(0)))
}

/// Durable appends of plan entries `range` from `writers` threads; a writer
/// owns the sessions with `session % writers == w` and replays them in order.
fn ingest(store: &Arc<dyn TraceStore>, tier: &Tier, range: std::ops::Range<usize>, writers: usize) -> Result<Value> {
    let t0 = Instant::now();
    let lats: Vec<Lat> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let range = range.clone();
                let store = Arc::clone(store);
                s.spawn(move || {
                    let mut lat = Lat::default();
                    let mut b = Vec::new();
                    for i in range {
                        if tier.plan[i].session as usize % writers != w {
                            continue;
                        }
                        tier.batch(i, &mut b);
                        let t = Instant::now();
                        store.append(&b).expect("append");
                        lat.add(t);
                        lat.items += b.len() as u64;
                        lat.bytes += b.iter().map(|e| e.raw_bytes()).sum::<u64>();
                    }
                    lat
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let wall = t0.elapsed().as_secs_f64();
    let mut all = Lat::default();
    for l in &lats {
        all.merge(l);
    }
    Ok(all.summary(Some(wall)))
}

fn text_prefix(t: &str, n: usize) -> &str {
    let mut e = n.min(t.len());
    while !t.is_char_boundary(e) {
        e -= 1;
    }
    &t[..e]
}

fn turnloop(store: &Arc<dyn TraceStore>, tier: &Tier, range: std::ops::Range<usize>, writers: usize) -> Result<Value> {
    let t0 = Instant::now();
    let fresh_fail = AtomicUsize::new(0);
    let res: Vec<[Lat; 4]> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let range = range.clone();
                let store = Arc::clone(store);
                let fresh_fail = &fresh_fail;
                s.spawn(move || {
                    let mut l: [Lat; 4] = Default::default(); // search, fetch, append, turn
                    let mut b = Vec::new();
                    for i in range {
                        let pe = tier.plan[i];
                        if pe.session as usize % writers != w {
                            continue;
                        }
                        tier.batch(i, &mut b);
                        let turn = Instant::now();
                        let first = &b[0];
                        if matches!(first.kind, events::TOOL_RESULT | events::USER) && !first.text.is_empty() {
                            let q = text_prefix(first.text, 200);
                            let t = Instant::now();
                            let hits = store.search(pe.agent, q, pe.append_ts - 1, 20).expect("search");
                            l[0].add(t);
                            l[0].items += hits.len() as u64;
                            let t = Instant::now();
                            for id in &hits {
                                let x = store.get_range(*id, 0, 512).expect("get_range");
                                l[1].bytes += x.map_or(0, |v| v.len() as u64);
                            }
                            l[1].add(t);
                        }
                        let t = Instant::now();
                        store.append(&b).expect("append");
                        l[2].add(t);
                        l[3].add(turn);
                        // freshness gate: the turn's last original reads back now
                        if let Some(e) = b.iter().rev().find(|e| !e.is_revision()) {
                            match store.get(e.id) {
                                Ok(Some(r)) if r.text == e.text => {}
                                _ => {
                                    fresh_fail.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    l
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let wall = t0.elapsed().as_secs_f64();
    let mut m: [Lat; 4] = Default::default();
    for r in &res {
        for k in 0..4 {
            m[k].merge(&r[k]);
        }
    }
    Ok(json!({
        "search_top20": m[0].summary(None), "fetch_20_excerpts": m[1].summary(None),
        "append": m[2].summary(None), "turn": m[3].summary(Some(wall)),
        "freshness_failures": fresh_fail.load(Ordering::Relaxed),
    }))
}

/// Random query parameters drawn from the loaded tier (seeded).
struct Draw<'a> {
    tier: &'a Tier<'a>,
    rng: Rng,
    tools: Vec<String>,
}

impl<'a> Draw<'a> {
    fn new(tier: &'a Tier<'a>, seed: u64) -> Self {
        // the 12 most common tools (from a sample of batches)
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut b = Vec::new();
        for i in (0..tier.plan.len()).step_by(37) {
            tier.batch(i, &mut b);
            for e in &b {
                if e.kind == events::TOOL_CALL && !e.is_revision() {
                    *counts.entry(e.tool.to_string()).or_default() += 1;
                }
            }
        }
        let mut tools: Vec<(String, u64)> = counts.into_iter().collect();
        tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Self { tier, rng: Rng(seed), tools: tools.into_iter().take(12).map(|x| x.0).collect() }
    }
    fn id(&mut self) -> u64 {
        1 + self.rng.below(self.tier.events)
    }
    fn session(&mut self) -> u64 {
        1 + self.rng.below(self.tier.sessions())
    }
    fn window(&mut self, len: i64) -> (i64, i64) {
        let span = (self.tier.max_ts() - self.tier.min_ts() - len).max(1);
        let from = self.tier.min_ts() + self.rng.below(span as u64) as i64;
        (from, from + len)
    }
    fn filter(&mut self, shape: u32) -> Filter {
        let (from, to) = self.window(DAY);
        let mut f = Filter { from_us: from, to_us: to, ..Default::default() };
        match shape {
            0 => f.kind = Some(events::TOOL_CALL), // tool calls in a day
            1 => {
                // failed calls of one tool in a day
                f.kind = Some(events::TOOL_CALL);
                f.status = Some(events::ST_ERROR);
                f.tool = Some(self.tools[self.rng.below(self.tools.len() as u64) as usize].clone());
            }
            2 => {
                // one agent's tool results in a day
                f.kind = Some(events::TOOL_RESULT);
                f.agent = Some(self.rng.below(16) as u32);
            }
            _ => {
                // pending (never completed) calls over the whole window
                f.from_us = i64::MIN / 2;
                f.to_us = i64::MAX / 2;
                f.status = Some(events::ST_PENDING);
            }
        }
        f
    }
}

fn reads(store: &Arc<dyn TraceStore>, tier: &Tier, a: &Args) -> Result<Value> {
    let mut d = Draw::new(tier, a.seed);
    let q = a.queries;
    let mut out = serde_json::Map::new();
    let mut skips = Skips::default();
    // An operation the arm does not support is skipped at its first call and
    // reported as {"unsupported": reason} under its name.
    macro_rules! time_op {
        ($name:expr, $n:expr, $body:expr) => {{
            let name: String = $name.into();
            let mut l = Lat::default();
            let t0 = Instant::now();
            let mut skipped = false;
            for _ in 0..$n {
                let t = Instant::now();
                let r: Result<u64> = (|| -> Result<u64> {
                    let items: u64 = $body;
                    Ok(items)
                })();
                match skips.take(&name, r)? {
                    Some(items) => {
                        l.add(t);
                        l.items += items;
                    }
                    None => {
                        skipped = true;
                        break;
                    }
                }
            }
            if skipped {
                out.insert(name.clone(), json!({"unsupported": skips.0[&name]}));
            } else {
                out.insert(name, l.summary(Some(t0.elapsed().as_secs_f64())));
            }
        }};
    }
    time_op!("get", q, { store.get(d.id())?.is_some() as u64 });
    time_op!("get_range_512", q, {
        let id = d.id();
        store.get_range(id, 0, 512)?.map_or(0, |v| v.len() as u64)
    });
    time_op!("session", q, { store.session(d.session())?.len() as u64 });
    time_op!("agg_session", q, { store.agg_session(d.session())?.events });
    time_op!("time_range_1h_1000", q / 3, {
        let (f, t) = d.window(HOUR);
        store.time_range(f, t, 1000)?.len() as u64
    });
    for shape in 0..4u32 {
        let n = if shape == 3 { 5 } else { q / 3 };
        time_op!(format!("filter{shape}_100"), n, {
            let f = d.filter(shape);
            store.filter(&f, 100)?.len() as u64
        });
        time_op!(format!("count{shape}"), n, {
            let f = d.filter(shape);
            store.count(&f)?
        });
    }
    time_op!("agg_tools_1d", q / 3, {
        let (f, t) = d.window(DAY);
        store.agg_tools(f, t)?.len() as u64
    });
    time_op!("agg_tools_30d", 5, { store.agg_tools(i64::MIN / 2, i64::MAX / 2)?.len() as u64 });
    time_op!("top_sessions_1d_10", q / 3, {
        let (f, t) = d.window(DAY);
        store.top_sessions(f, t, 10)?.len() as u64
    });
    // concurrent dashboard throughput: 8 threads, mixed point + session + agg
    // + scan; an op the arm does not support (seen above) is left out of the
    // mix and listed, so the mix is labelled partial
    let mix: Vec<usize> = (0..4usize)
        .filter(|k| !skips.0.contains_key(["get", "session", "agg_session", "time_range_1h_1000"][*k]))
        .collect();
    let t0 = Instant::now();
    let lats: Vec<Lat> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..8u64)
            .map(|w| {
                let store = Arc::clone(store);
                let mix = &mix;
                s.spawn(move || {
                    let mut d = Draw::new(tier, 1000 + w);
                    let mut l = Lat::default();
                    if mix.is_empty() {
                        return l;
                    }
                    for k in 0..q {
                        let t = Instant::now();
                        match mix[k % mix.len()] {
                            0 => drop(store.get(d.id())),
                            1 => drop(store.session(d.session())),
                            2 => drop(store.agg_session(d.session())),
                            _ => {
                                let (f, t) = d.window(HOUR);
                                drop(store.time_range(f, t, 100))
                            }
                        }
                        l.add(t);
                    }
                    l
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut all = Lat::default();
    for l in &lats {
        all.merge(l);
    }
    let mut m = all.summary(Some(t0.elapsed().as_secs_f64()));
    if mix.len() < 4 {
        m["partial_mix"] = json!(mix.iter().map(|k| ["get", "session", "agg_session", "time_range"][*k]).collect::<Vec<_>>());
    }
    out.insert("mixed_8threads".into(), m);
    // export
    struct Counter(u64, blake3::Hasher);
    impl Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len() as u64;
            self.1.update(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Counter(0, blake3::Hasher::new());
    let t = Instant::now();
    match skips.take("export", store.export(&mut c))? {
        Some((n, bytes)) => {
            let w = t.elapsed().as_secs_f64();
            out.insert(
                "export".into(),
                json!({"events": n, "bytes": bytes, "wall_s": w, "mb_per_s": bytes as f64 / w / 1e6,
                       "blake3": c.1.finalize().to_hex().to_string()}),
            );
        }
        None => {
            out.insert("export".into(), json!({"unsupported": skips.0["export"]}));
        }
    }
    if !skips.0.is_empty() {
        out.insert("unsupported".into(), skips.json());
    }
    Ok(Value::Object(out))
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Pick `want` 48-byte strings from deleted texts that occur in no surviving
/// text, so finding one in the store files proves a leak.
fn needles(tier: &Tier, dead: &dyn Fn(&Event) -> bool, want: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng(seed);
    let mut cands: Vec<Vec<u8>> = Vec::new();
    let mut b = Vec::new();
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        for e in &b {
            if dead(e) && e.text.len() > 400 && rng.below(50) == 0 && cands.len() < want * 10 {
                let t = e.text.as_bytes();
                let off = rng.below((t.len() - 64) as u64) as usize;
                cands.push(t[off..off + 48].to_vec());
            }
        }
    }
    let mut alive_hit = vec![false; cands.len()];
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        for e in &b {
            if !dead(e) {
                for (k, c) in cands.iter().enumerate() {
                    if !alive_hit[k] && find(e.text.as_bytes(), c) {
                        alive_hit[k] = true;
                    }
                }
            }
        }
    }
    cands.into_iter().zip(alive_hit).filter(|(_, h)| !h).map(|(c, _)| c).take(want).collect()
}

/// Plaintext occurrences of any needle in the store's files: (hits, bytes
/// scanned, files with a hit and their hit counts).
fn scan_files(paths: &[PathBuf], needles: &[Vec<u8>]) -> (u64, u64, BTreeMap<String, u64>) {
    let (mut hits, mut bytes, mut files) = (0, 0, BTreeMap::new());
    for p in paths {
        let Ok(data) = std::fs::read(p) else { continue };
        bytes += data.len() as u64;
        for n in needles {
            if find(&data, n) {
                hits += 1;
                *files.entry(p.display().to_string()).or_insert(0u64) += 1;
            }
        }
    }
    (hits, bytes, files)
}

fn purge(store: &Arc<dyn TraceStore>, tier: &Tier, a: &Args) -> Result<Value> {
    let mut rng = Rng(a.seed ^ 0xD1E);
    let mut doomed: HashSet<u64> = HashSet::new();
    while doomed.len() < a.delete_sessions {
        doomed.insert(1 + rng.below(tier.sessions()));
    }
    let cutoff = tier.min_ts() + a.purge_days * DAY;
    let dead = |e: &Event| doomed.contains(&e.session) || e.ts_us < cutoff;
    // Unique needles exist only without replication (replicas repeat every
    // text), and finding them scans the tier twice: tier 1 only.
    let needle_set = if tier.replicas == 1 { needles(tier, &dead, 20, a.seed) } else { Vec::new() };
    let disk0 = store.disk_bytes()?;
    let mut skips = Skips::default();
    let mut del = Lat::default();
    let t0 = Instant::now();
    let mut ds: Vec<u64> = doomed.iter().copied().collect();
    ds.sort_unstable();
    for s in &ds {
        let t = Instant::now();
        if skips.take("delete_session", store.delete_session(*s))?.is_none() {
            break;
        }
        del.add(t);
    }
    let del_wall = t0.elapsed().as_secs_f64();
    let t = Instant::now();
    skips.take("purge_before", store.purge_before(cutoff))?;
    let purge_s = t.elapsed().as_secs_f64();
    let deleted = !skips.0.contains_key("delete_session");
    let purged = !skips.0.contains_key("purge_before");
    if !deleted && !purged {
        return Ok(json!({"unsupported": skips.json(), "disk_before": disk0}));
    }
    // invisibility checks, of what was deleted
    let mut visible = 0u64;
    if deleted {
        for s in ds.iter().take(20) {
            visible += store.session(*s)?.len() as u64;
        }
    }
    if purged {
        visible += store.count(&Filter { from_us: i64::MIN / 2, to_us: cutoff, ..Default::default() })?;
    }
    // the needles come from both kinds of deleted text: with either delete
    // skipped some of them are live text, so the scans are skipped
    let needle_set = if deleted && purged { needle_set } else { Vec::new() };
    let paths = store.data_paths()?;
    let (hits_before, scanned_before, _) = scan_files(&paths, &needle_set);
    let disk1 = store.disk_bytes()?;
    let t = Instant::now();
    let reclaimed = skips.take("reclaim", store.reclaim())?.is_some();
    let reclaim_s = t.elapsed().as_secs_f64();
    let disk2 = store.disk_bytes()?;
    let paths = store.data_paths()?;
    let (hits_after, scanned_after, hit_files) = scan_files(&paths, &needle_set);
    let deep_after = if needle_set.is_empty() || !reclaimed { None } else { store.deep_scan(&needle_set)? };
    let live = store.count(&Filter { from_us: i64::MIN / 2, to_us: i64::MAX / 2, ..Default::default() })?;
    let label = |op: &str| json!({"unsupported": skips.0.get(op)});
    Ok(json!({
        "delete_session": if deleted { del.summary(Some(del_wall)) } else { label("delete_session") },
        "purge_before_s": if purged { json!(purge_s) } else { label("purge_before") },
        "reclaim_s": if reclaimed { json!(reclaim_s) } else { label("reclaim") },
        "cutoff_days": a.purge_days, "deleted_sessions": if deleted { ds.len() } else { 0 },
        "visible_after_delete": visible,
        "disk_before": disk0, "disk_after_delete": disk1, "disk_after_reclaim": disk2, "live_events": live,
        "needles": needle_set.len(), "needle_files_hits_before_reclaim": hits_before,
        "needle_files_hits_after_reclaim": hits_after, "needle_decoded_hits_after_reclaim": deep_after,
        "needle_hit_files_after_reclaim": hit_files,
        "scanned_bytes_before": scanned_before,
        "scanned_bytes_after": scanned_after,
        "unsupported": skips.json(),
    }))
}

struct HashWriter(blake3::Hasher);
impl Write for HashWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.update(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Excerpt fetch latency on a loaded store: random 512-byte excerpts, and
/// the turn loop's read side (search top-20 for the first 200 bytes of a
/// random indexed event as of its time, then fetch the 20 excerpts).
fn fetch(store: &Arc<dyn TraceStore>, tier: &Tier, a: &Args) -> Result<Value> {
    let mut d = Draw::new(tier, a.seed ^ 0xfe7c);
    let mut one = Lat::default();
    let t0 = Instant::now();
    for _ in 0..2000 {
        let id = d.id();
        let t = Instant::now();
        let n = store.get_range(id, 0, 512)?.map_or(0, |v| v.len() as u64);
        one.add(t);
        one.bytes += n;
    }
    let single = one.summary(Some(t0.elapsed().as_secs_f64()));
    let mut skips = Skips::default();
    if skips.take("search", store.search(0, "probe", i64::MAX, 1))?.is_none() {
        let l = json!({"unsupported": skips.0["search"]});
        return Ok(json!({"excerpt_512": single, "search_top20": l, "fetch_top20": l, "fetch_each": l}));
    }
    let (mut search, mut top20, mut each) = (Lat::default(), Lat::default(), Lat::default());
    let mut b = Vec::new();
    let mut done = 0;
    while done < a.queries {
        let i = d.rng.below(tier.plan.len() as u64) as usize;
        tier.batch(i, &mut b);
        let Some(e) = b.iter().find(|e| events::indexed_kind(e.kind) && e.text.len() >= 50 && !e.is_revision()) else {
            continue;
        };
        let q: String = e.text.chars().take(200).collect();
        let t = Instant::now();
        let ids = store.search(e.agent, &q, e.ts_us, 20)?;
        search.add(t);
        let t = Instant::now();
        for id in &ids {
            let x = Instant::now();
            let n = store.get_range(*id, 0, 512)?.map_or(0, |v| v.len() as u64);
            each.add(x);
            each.bytes += n;
        }
        top20.add(t);
        top20.items += ids.len() as u64;
        done += 1;
    }
    Ok(json!({"excerpt_512": single, "search_top20": search.summary(None), "fetch_top20": top20.summary(None),
              "fetch_each": each.summary(None)}))
}

/// Untimed: every deterministic answer equals the oracle's (tier 1 only).
fn verify(store: &Arc<dyn TraceStore>, tier: &Tier, a: &Args) -> Result<Value> {
    let oracle = Oracle::new();
    let mut b = Vec::new();
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        oracle.add(&b);
    }
    let mut d = Draw::new(tier, a.seed ^ 77);
    let mut fails: Vec<String> = Vec::new();
    let mut checks = 0u64;
    let mut per_op: BTreeMap<String, u64> = BTreeMap::new();
    let mut check = |name: &str, ok: bool| {
        checks += 1;
        *per_op.entry(name.trim_end_matches(char::is_numeric).to_string()).or_default() += 1;
        if !ok && fails.len() < 50 {
            fails.push(name.to_string());
        }
    };
    // An operation the arm does not support is not a check: it is skipped
    // and listed under "unsupported" with the arm's reason.
    let mut skips = Skips::default();
    macro_rules! cmp {
        ($name:expr, $got:expr, $want:expr) => {{
            let name: &str = &$name;
            if let Some(v) = skips.take(name, $got)? {
                check(name, v == $want?);
            }
        }};
    }
    for _ in 0..200 {
        let id = d.id();
        cmp!("get", store.get(id), oracle.get(id));
        let (s, e) = (d.rng.below(300) as usize, d.rng.below(3000) as usize);
        cmp!("get_range", store.get_range(id, s, e), oracle.get_range(id, s, e));
        let s = d.session();
        cmp!("session", store.session(s), oracle.session(s));
        cmp!("agg_session", store.agg_session(s), oracle.agg_session(s));
    }
    for _ in 0..30 {
        let (f, t) = d.window(HOUR);
        cmp!("time_range", store.time_range(f, t, 1000), oracle.time_range(f, t, 1000));
        for shape in 0..4 {
            let flt = d.filter(shape);
            cmp!(format!("filter{shape}"), store.filter(&flt, 100), oracle.filter(&flt, 100));
            cmp!(format!("count{shape}"), store.count(&flt), oracle.count(&flt));
        }
        let (f, t) = d.window(DAY);
        cmp!("agg_tools", store.agg_tools(f, t), oracle.agg_tools(f, t));
        cmp!("top_sessions", store.top_sessions(f, t, 10), oracle.top_sessions(f, t, 10));
    }
    let (mut x, mut y) = (HashWriter(blake3::Hasher::new()), HashWriter(blake3::Hasher::new()));
    if let Some(ne) = skips.take("export", store.export(&mut x))? {
        let no = oracle.export(&mut y)?;
        check("export", ne == no && x.0.finalize() == y.0.finalize());
    }
    // The oracle mirrors only the deletes the arm performed.
    let s = d.session();
    if skips.take("delete_session", store.delete_session(s))?.is_some() {
        oracle.delete_session(s)?;
        cmp!("delete_session", store.session(s).map(|v| v.is_empty()), Ok::<bool, oojuro_tracebench::Error>(true));
    }
    let cut = tier.min_ts() + 3 * DAY;
    if skips.take("purge_before", store.purge_before(cut))?.is_some() {
        oracle.purge_before(cut)?;
    }
    for shape in 0..4 {
        let flt = d.filter(shape);
        cmp!(format!("count_after_purge{shape}"), store.count(&flt), oracle.count(&flt));
    }
    skips.take("reclaim", store.reclaim())?;
    for _ in 0..50 {
        let id = d.id();
        cmp!("get_after_reclaim", store.get(id), oracle.get(id));
        let s = d.session();
        cmp!("session_after_reclaim", store.session(s), oracle.session(s));
    }
    let all = Filter { from_us: i64::MIN / 2, to_us: i64::MAX / 2, ..Default::default() };
    cmp!("count_after_reclaim", store.count(&all), oracle.count(&all));
    Ok(json!({"checks": checks, "failures": fails, "checks_per_op": per_op, "unsupported": skips.json()}))
}

fn host() -> Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_default();
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    json!({"cpu": cpu, "threads": std::thread::available_parallelism().map_or(0, |n| n.get()),
           "loadavg": load.trim()})
}

fn write_rec(rec: &Value, out: &std::path::Path) -> Result<()> {
    let line = serde_json::to_string(rec)?;
    println!("{line}");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out)?;
    writeln!(f, "{line}")?;
    Ok(())
}

fn main() -> Result<()> {
    let a = args();
    let base = Base::open(&a.data)?;
    let tier = Tier::new(&base, a.tier);
    if a.suite == "ycsb-crash-child" {
        // internal: the writer process the ycsb crash test SIGKILLs; `--work`
        // is the exact store directory, stdout carries acknowledged ids
        return ycsb::crash_child(&a.arm, &a.work, &tier, &a.ycsb);
    }
    // the pid keeps concurrent invocations with the same arguments apart
    let work = a.work.join(format!(
        "{}-{}-r{}-p{}",
        a.arm.replace([':', ',', '='], "_"),
        a.suite,
        a.run,
        std::process::id()
    ));
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    let mut rec = json!({
        "suite": a.suite, "arm": a.arm, "run": a.run, "tier": a.tier,
        "events": tier.events, "batches": tier.plan.len(), "sessions": tier.sessions(),
        "raw_bytes": tier.raw_bytes(), "writers": a.writers, "host_before": host(),
        "bin": std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
        "utc_start": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
    });
    let started = Instant::now();
    if a.suite == "ycsb" {
        // opens, reopens and drops its stores itself
        rec["writers"] = json!(a.ycsb.writers);
        ycsb::run(&a.arm, &work, &tier, &a.ycsb, &mut rec)?;
        rec["wall_s"] = json!(started.elapsed().as_secs_f64());
        rec["host_after"] = host();
        write_rec(&rec, &a.out)?;
        if !a.keep {
            let _ = std::fs::remove_dir_all(&work);
        }
        return Ok(());
    }
    let store: Arc<dyn TraceStore> = Arc::from(arms::open(&a.arm, &work)?);
    rec["store"] = json!(store.name());
    let n = tier.plan.len();
    match a.suite.as_str() {
        "ingest" => {
            let m = if a.ingest_batches == 0 { n } else { a.ingest_batches.min(n) };
            let mem = sampler(&store);
            let r = ingest(&store, &tier, 0..m, a.writers)?;
            rec["mem_peak"] = json!(mem.finish());
            let mut b = Vec::new();
            let mut raw = 0u64;
            for i in 0..m {
                tier.batch(i, &mut b);
                raw += b.iter().map(|e| e.raw_bytes()).sum::<u64>();
            }
            if let Some(c) = store.sync_counters() {
                rec["sync_counters"] = c;
            }
            let disk = store.disk_bytes()?;
            rec["ingest"] = r;
            rec["ingested_raw_bytes"] = json!(raw);
            rec["disk_bytes"] = json!(disk);
            rec["bytes_per_raw"] = json!(disk as f64 / raw as f64);
        }
        "reads" | "purge" => {
            let mem = sampler(&store);
            rec["load"] = bulk_load(store.as_ref(), &tier, 0..n, a.load_threads)?;
            rec["mem_peak_load"] = json!(mem.finish());
            let disk = store.disk_bytes()?;
            rec["disk_bytes"] = json!(disk);
            rec["bytes_per_raw"] = json!(disk as f64 / tier.raw_bytes() as f64);
            rec["mem_after_load"] = json!(store.mem_bytes()?);
            rec["details_after_load"] = store.details();
            let mem = sampler(&store);
            if a.suite == "reads" {
                rec["reads"] = reads(&store, &tier, &a)?;
            }
            rec["mem_peak_queries"] = json!(mem.finish());
            // retention mix last: it deletes data (reads + purge share one load)
            rec["purge"] = purge(&store, &tier, &a)?;
        }
        "turnloop" => {
            // the per-turn loop is search-driven: an arm without search skips
            // it (probed on the empty store, before any load)
            let mut skips = Skips::default();
            if skips.take("search", store.search(0, "probe", i64::MAX, 1))?.is_none() {
                rec["turnloop"] = json!({"unsupported": skips.0["search"]});
            } else {
                let split = ((n as f64) * (1.0 - a.turnloop_frac)) as usize;
                rec["load"] = bulk_load(store.as_ref(), &tier, 0..split, a.load_threads)?;
                let mem = sampler(&store);
                rec["turnloop"] = turnloop(&store, &tier, split..n, a.writers)?;
                rec["mem_peak"] = json!(mem.finish());
                rec["disk_bytes"] = json!(store.disk_bytes()?);
            }
        }
        "size" => {
            // storage and memory only (untimed): bulk load, settle, measure
            let mem = sampler(&store);
            rec["load"] = bulk_load(store.as_ref(), &tier, 0..n, a.load_threads)?;
            rec["mem_peak_load"] = json!(mem.finish());
            let disk = store.disk_bytes()?;
            rec["disk_bytes"] = json!(disk);
            rec["bytes_per_raw"] = json!(disk as f64 / tier.raw_bytes() as f64);
            rec["mem_after_load"] = json!(store.mem_bytes()?);
            rec["details_after_load"] = store.details();
        }
        "fetch" => {
            // storage, then excerpt fetch latency (indicative; no lock)
            rec["load"] = bulk_load(store.as_ref(), &tier, 0..n, a.load_threads)?;
            let disk = store.disk_bytes()?;
            rec["disk_bytes"] = json!(disk);
            rec["bytes_per_raw"] = json!(disk as f64 / tier.raw_bytes() as f64);
            rec["details_after_load"] = store.details();
            rec["fetch"] = fetch(&store, &tier, &a)?;
        }
        "verify" => {
            rec["load"] = bulk_load(store.as_ref(), &tier, 0..n, a.load_threads)?;
            rec["verify"] = verify(&store, &tier, &a)?;
        }
        s => return Err(format!("unknown suite {s}").into()),
    }
    rec["wall_s"] = json!(started.elapsed().as_secs_f64());
    rec["host_after"] = host();
    write_rec(&rec, &a.out)?;
    drop(store);
    if !a.keep {
        let _ = std::fs::remove_dir_all(&work);
    }
    Ok(())
}
