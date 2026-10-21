//! searchprobe: per-turn search latency and exactness probe (docs/PERF_SEARCH.md).
//!
//! `build` bulk-loads a prefix of a tier through `TraceDb` (per agent, or every
//! agent folded into one store with `--single`), flushes, settles merges and
//! writes `probe.json`. `run` opens that store read-only in spirit (no appends)
//! and times `Nine::find(query, 0, as_of, k)` on the turn-loop queries that
//! follow the loaded prefix, at a chosen query size, dumping top-k ids and
//! score bits so two builds can be compared bit for bit with `compare`.
//!
//! Query sizes: `--size 0` is the turnloop suite's query (first 200 bytes of
//! the turn's first event). `--size N` is exactly N bytes (cut at a char
//! boundary): the turn's first event, then the same agent's earlier events,
//! newest first, joined by newlines (an agent's recent context as the query).
//!
//! `run --max-df F` times the query-side simulation of an index that stops
//! terms whose df exceeds F of the agent store's indexed rows (df read from
//! the store's segment dictionaries; the same word rule as the bench arm
//! `nine-maxdf:F`). `kinds --data X [--tier N] --out F`
//! writes one byte per event id (kind code, 255 = none) for `index_probe`.
use oojuro_trace::{EventIn, Options, TraceDb};
use oojuro_tracebench::{
    Result,
    events::{self, Base, Event, Tier},
    stats::Lat,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

fn flag(v: &[String], name: &str) -> Option<String> {
    v.iter().position(|x| x == name).and_then(|i| v.get(i + 1).cloned())
}

fn has(v: &[String], name: &str) -> bool {
    v.iter().any(|x| x == name)
}

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}
unsafe extern "C" {
    fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
}

/// CPU time of the calling thread in ns (Linux CLOCK_THREAD_CPUTIME_ID): a
/// load-robust companion to wall time for single-threaded queries.
fn thread_cpu_ns() -> u64 {
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: valid out pointer; clock id 3 is CLOCK_THREAD_CPUTIME_ID.
    unsafe { clock_gettime(3, &mut t) };
    t.sec as u64 * 1_000_000_000 + t.nsec as u64
}

/// CPU time of the whole process (all threads) in ns: with --qthreads > 1
/// and --threads 1 its per-query delta is the query's total work.
fn process_cpu_ns() -> u64 {
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: valid out pointer; clock id 2 is CLOCK_PROCESS_CPUTIME_ID.
    unsafe { clock_gettime(2, &mut t) };
    t.sec as u64 * 1_000_000_000 + t.nsec as u64
}

/// Minor page faults of this process so far (/proc/self/stat field 10).
fn minor_faults() -> u64 {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| s.rsplit_once(')').map(|(_, r)| r.to_string()))
        .and_then(|r| r.split_whitespace().nth(7).and_then(|x| x.parse().ok()))
        .unwrap_or(0)
}

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim().to_string()
}

fn to_in<'a>(e: &Event<'a>, single: bool) -> EventIn<'a> {
    EventIn {
        id: e.id,
        session: e.session,
        parent_session: 0,
        agent: if single { 0 } else { e.agent },
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

fn opts(maxq: usize) -> Options {
    Options { max_query_bytes: maxq, ..Options::default() }
}

/// `--no-dedupe` at build (kept in probe.json): every event is its own
/// record, so a replicated tier keeps all its rows.
fn with_dedupe(mut o: Options, dedupe: bool) -> Options {
    o.dedupe_records = dedupe;
    o
}

fn opts_threads(maxq: usize, threads: usize, exact_entropy: bool) -> Options {
    Options { query_threads: threads, exact_entropy, ..opts(maxq) }
}

fn build(v: &[String]) -> Result<()> {
    let data = PathBuf::from(flag(v, "--data").ok_or("--data")?);
    let tier_n: u32 = flag(v, "--tier").map_or(Ok(1), |s| s.parse())?;
    let single = has(v, "--single");
    let dir = PathBuf::from(flag(v, "--dir").ok_or("--dir")?);
    let threads: usize = flag(v, "--load-threads").map_or(Ok(4), |s| s.parse())?;
    let base = Base::open(&data)?;
    let tier = Tier::new(&base, tier_n);
    let n = tier.plan.len();
    // Cut: the first plan entry not loaded. --records N loads the shortest
    // prefix holding at least N events; otherwise --frac F holds back F.
    let cut = match flag(v, "--records") {
        Some(r) => {
            let want: u64 = r.parse()?;
            let mut got = 0u64;
            let mut i = 0;
            while i < n && got < want {
                got += tier.plan[i].n as u64;
                i += 1;
            }
            i
        }
        None => {
            let frac: f64 = flag(v, "--frac").map_or(Ok(0.1), |s| s.parse())?;
            ((n as f64) * (1.0 - frac)) as usize
        }
    };
    if dir.exists() {
        return Err(format!("{} exists", dir.display()).into());
    }
    let t0 = Instant::now();
    let dedupe = !has(v, "--no-dedupe");
    let db = TraceDb::open(&dir, with_dedupe(opts(65_536), dedupe))?;
    let err: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|s| {
        for w in 0..threads {
            let (tier, db, err) = (&tier, &db, &err);
            s.spawn(move || {
                let mut chunk: Vec<Vec<EventIn>> = Vec::new();
                let mut b = Vec::new();
                for i in 0..cut {
                    if tier.plan[i].session as usize % threads != w {
                        continue;
                    }
                    tier.batch(i, &mut b);
                    chunk.push(b.iter().map(|e| to_in(e, single)).collect());
                    if chunk.len() == 256 {
                        if let Err(e) = db.append_many(&chunk) {
                            *err.lock().unwrap() = Some(e.to_string());
                            return;
                        }
                        chunk.clear();
                    }
                }
                if !chunk.is_empty() {
                    if let Err(e) = db.append_many(&chunk) {
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
    db.flush()?;
    let merged = db.settle()?;
    db.flush()?;
    let records = db.len();
    drop(db);
    let info = json!({"data": data, "tier": tier_n, "single": single, "cut": cut, "plan": n, "dedupe": dedupe,
                      "records": records, "load_s": loaded, "merges": merged,
                      "wall_s": t0.elapsed().as_secs_f64(), "loadavg": loadavg()});
    std::fs::write(dir.join("probe.json"), serde_json::to_vec_pretty(&info)?)?;
    println!("{info}");
    Ok(())
}

fn text_prefix(t: &str, n: usize) -> &str {
    let mut e = n.min(t.len());
    while !t.is_char_boundary(e) {
        e -= 1;
    }
    &t[..e]
}

struct Q {
    plan: usize,
    agent: u32,
    as_of: i64,
    text: String,
}

/// The turn-loop queries after the loaded prefix: batches whose first event
/// is a non-empty tool result or user message (as `tracebench turnloop`).
fn queries(tier: &Tier, cut: usize, single: bool, size: usize, limit: usize) -> Vec<Q> {
    let n = tier.plan.len();
    let mut b = Vec::new();
    let mut turns = Vec::new();
    for i in cut..n {
        tier.batch(i, &mut b);
        let f = &b[0];
        if matches!(f.kind, events::TOOL_RESULT | events::USER) && !f.text.is_empty() {
            turns.push(i);
        }
    }
    let pick: Vec<usize> = if limit == 0 || limit >= turns.len() {
        turns
    } else {
        (0..limit).map(|k| turns[k * turns.len() / limit]).collect()
    };
    // Per original agent, plan indices in order (for the history walk).
    let mut by_agent: std::collections::HashMap<u32, Vec<usize>> = Default::default();
    if size > 0 {
        for (i, pe) in tier.plan.iter().enumerate() {
            by_agent.entry(pe.agent).or_default().push(i);
        }
    }
    pick.into_iter()
        .map(|i| {
            let pe = tier.plan[i];
            tier.batch(i, &mut b);
            let text = if size == 0 {
                text_prefix(b[0].text, 200).to_string()
            } else {
                let mut s = String::with_capacity(size + 64);
                s.push_str(b[0].text);
                let list = &by_agent[&pe.agent];
                let mut at = list.partition_point(|x| *x < i);
                let mut hb = Vec::new();
                while s.len() < size && at > 0 {
                    at -= 1;
                    tier.batch(list[at], &mut hb);
                    for e in hb.iter().rev() {
                        if !e.text.is_empty() {
                            s.push('\n');
                            s.push_str(e.text);
                        }
                        if s.len() >= size {
                            break;
                        }
                    }
                }
                text_prefix(&s, size).to_string()
            };
            Q { plan: i, agent: if single { 0 } else { pe.agent }, as_of: pe.append_ts - 1, text }
        })
        .collect()
}

/// Term -> df summed over an agent store's segment dictionaries, and its
/// indexed rows (rows without the stored-only flag).
fn store_df(agent_dir: &Path) -> Result<(std::collections::HashMap<String, u64>, u64)> {
    fn varint(b: &[u8], p: &mut usize) -> u64 {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let x = b[*p];
            *p += 1;
            v |= u64::from(x & 0x7f) << s;
            if x < 0x80 {
                return v;
            }
            s += 7;
        }
    }
    let u64_at = |b: &[u8], p: usize| u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
    let u32_at = |b: &[u8], p: usize| u32::from_le_bytes(b[p..p + 4].try_into().unwrap());
    let mut df = std::collections::HashMap::new();
    let mut indexed = 0u64;
    for e in std::fs::read_dir(agent_dir)? {
        let p = e?.path();
        if p.extension().is_none_or(|x| x != "idx") {
            continue;
        }
        let map = std::fs::read(&p)?;
        let rows = u32_at(&map, 20) as usize;
        let sec: Vec<usize> = (0..9).map(|i| u64_at(&map, 32 + 8 * i) as usize).collect();
        indexed += (0..rows).filter(|r| u32_at(&map, sec[0] + 40 * r + 28) & 1 == 0).count() as u64;
        let (s_dict, s_dindex, s_end) = (sec[6], sec[7], sec[8]);
        let blocks = (s_end - s_dindex) / 8;
        for b in 0..blocks {
            let start = s_dict + u64_at(&map, s_dindex + 8 * b) as usize;
            let end = if b + 1 < blocks { s_dict + u64_at(&map, s_dindex + 8 * (b + 1)) as usize } else { s_dindex };
            let mut q = start;
            varint(&map, &mut q);
            let mut prev: Vec<u8> = Vec::new();
            while q < end {
                let shared = varint(&map, &mut q) as usize;
                let n = varint(&map, &mut q) as usize;
                let mut term = prev[..shared].to_vec();
                term.extend_from_slice(&map[q..q + n]);
                q += n;
                let d = varint(&map, &mut q);
                varint(&map, &mut q);
                varint(&map, &mut q);
                *df.entry(String::from_utf8(term.clone())?).or_default() += d;
                prev = term;
            }
        }
    }
    Ok((df, indexed))
}

/// Blank out query words whose normalized term has df above `limit` (the
/// rule of oojuro-bench `arms/nine_maxdf.rs`: stop words stay, at least one
/// non-stop word stays, quoted literal queries are not rewritten).
fn drop_frequent_words(text: &str, df: &dyn Fn(&str) -> u64, limit: f64) -> (String, usize, bool) {
    let t = text.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return (text.to_string(), 0, false);
    }
    let mut words: Vec<(usize, usize, String, bool)> = Vec::new();
    let mut start: Option<usize> = None;
    let push = |s: usize, e: usize, words: &mut Vec<(usize, usize, String, bool)>| {
        let w = &text[s..e];
        if !w.bytes().any(|b| b != b'_') {
            return;
        }
        let lower = w.to_lowercase();
        let stop = lower.len() <= 6 && oojuro_nine::QUERY_STOP_WORDS.contains(&lower.as_str());
        words.push((s, e, oojuro_nine::query_term(&lower), stop));
    };
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            push(s, i, &mut words);
        }
    }
    if let Some(s) = start {
        push(s, text.len(), &mut words);
    }
    let mut dfs: std::collections::HashMap<&str, u64> = Default::default();
    for w in &words {
        if !w.3 {
            dfs.entry(w.2.as_str()).or_insert_with(|| df(&w.2));
        }
    }
    if dfs.is_empty() {
        return (text.to_string(), 0, false);
    }
    let mut drop: Vec<&str> = dfs.iter().filter(|(_, d)| **d as f64 > limit).map(|(t, _)| *t).collect();
    let all_frequent = drop.len() == dfs.len();
    if all_frequent {
        let keep = dfs.iter().min_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0))).map(|x| *x.0).unwrap();
        drop.retain(|t| *t != keep);
    }
    if drop.is_empty() {
        return (text.to_string(), 0, all_frequent);
    }
    let mut out = text.as_bytes().to_vec();
    let mut n = 0;
    for w in &words {
        if !w.3 && drop.contains(&w.2.as_str()) {
            out[w.0..w.1].fill(b' ');
            n += 1;
        }
    }
    (String::from_utf8(out).expect("blanked whole chars"), n, all_frequent)
}

fn kinds(v: &[String]) -> Result<()> {
    let data = PathBuf::from(flag(v, "--data").ok_or("--data")?);
    let tier_n: u32 = flag(v, "--tier").map_or(Ok(1), |s| s.parse())?;
    let out = PathBuf::from(flag(v, "--out").ok_or("--out")?);
    let base = Base::open(&data)?;
    let tier = Tier::new(&base, tier_n);
    let mut k: Vec<u8> = Vec::new();
    let mut b = Vec::new();
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        for e in &b {
            let id = e.id as usize;
            if k.len() <= id {
                k.resize(id + 1, 255);
            }
            k[id] = e.kind;
        }
    }
    std::fs::write(out, &k)?;
    Ok(())
}

fn run(v: &[String]) -> Result<()> {
    let dir = PathBuf::from(flag(v, "--dir").ok_or("--dir")?);
    let info: Value = serde_json::from_slice(&std::fs::read(dir.join("probe.json"))?)?;
    let size: usize = flag(v, "--size").map_or(Ok(0), |s| s.parse())?;
    let limit: usize = flag(v, "--queries").map_or(Ok(0), |s| s.parse())?;
    let threads: usize = flag(v, "--threads").map_or(Ok(1), |s| s.parse())?;
    let k: usize = flag(v, "--k").map_or(Ok(20), |s| s.parse())?;
    let rounds: usize = flag(v, "--rounds").map_or(Ok(1), |s| s.parse())?;
    let label = flag(v, "--label").unwrap_or_default();
    let out = flag(v, "--out").map(PathBuf::from);
    let dump = flag(v, "--dump").map(PathBuf::from);
    // Per query: plan index, cpu ns, wall ns, terms, candidates, postings, scored.
    let work_out = flag(v, "--work").map(PathBuf::from);
    let data = PathBuf::from(info["data"].as_str().ok_or("probe.json data")?);
    let base = Base::open(&data)?;
    let tier = Tier::new(&base, info["tier"].as_u64().ok_or("tier")? as u32);
    let single = info["single"].as_bool().unwrap_or(false);
    let cut = info["cut"].as_u64().ok_or("cut")? as usize;
    let mut qs = queries(&tier, cut, single, size, limit);
    // --max-df F: blank query words of terms with df > F of the store's indexed rows
    let max_df: Option<f64> = flag(v, "--max-df").map(|s| s.parse()).transpose()?;
    let (mut dropped, mut all_frequent) = (0usize, 0usize);
    if let Some(f) = max_df {
        let mut stores: std::collections::HashMap<u32, (std::collections::HashMap<String, u64>, u64)> = Default::default();
        for q in qs.iter_mut() {
            if !stores.contains_key(&q.agent) {
                stores.insert(q.agent, store_df(&dir.join(format!("agent-{:05}", q.agent)))?);
            }
            let (df, rows) = &stores[&q.agent];
            let (t, n, all) = drop_frequent_words(&q.text, &|t: &str| df.get(t).copied().unwrap_or(0), f * *rows as f64);
            q.text = t;
            dropped += n;
            all_frequent += all as usize;
        }
    }
    let qbytes: usize = qs.iter().map(|q| q.text.len()).sum();
    let qthreads: usize = flag(v, "--qthreads").map_or(Ok(1), |s| s.parse())?;
    // --entropy all: score every candidate (the engine default); topk: the
    // TraceDb default (same ids and scores, entropy of the top k).
    let exact_entropy = flag(v, "--entropy").as_deref() == Some("all");
    let dedupe = info["dedupe"].as_bool().unwrap_or(true);
    let db = TraceDb::open(&dir, with_dedupe(opts_threads(size.max(65_536), qthreads, exact_entropy), dedupe))?;
    // Engine rows (events plus status revisions) per store.
    let mut rows = Vec::new();
    for agent in 0..16u32 {
        if let Some(nine) = db.engine(agent)? {
            rows.push(nine.len());
        }
    }
    let load_before = loadavg();
    oojuro_trace::profile_reset();
    let mut summaries = Vec::new();
    let mut lines: Vec<Option<String>> = (0..qs.len()).map(|_| None).collect();
    let mut per_query: Vec<String> = Vec::new();
    let mut work = [0u64; 5];
    for round in 0..rounds {
        let faults = minor_faults();
        let next = AtomicUsize::new(0);
        let res: Mutex<Vec<(usize, u64, u64, u64, String, [u64; 5])>> = Mutex::new(Vec::new());
        let t0 = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(q) = qs.get(i) else { break };
                        let nine = db.engine(q.agent).expect("open").expect("agent store");
                        nine.clear_query_cache();
                        let c = thread_cpu_ns();
                        let pc = process_cpu_ns();
                        let t = Instant::now();
                        let m = nine.find(&q.text, 0, q.as_of, k).expect("search");
                        let ns = t.elapsed().as_nanos() as u64;
                        let cpu = thread_cpu_ns() - c;
                        let pcpu = process_cpu_ns() - pc;
                        let mt = nine.metrics();
                        let ids: Vec<&str> = m.matches.iter().map(|x| x.id.as_str()).collect();
                        let bits: Vec<String> =
                            m.matches.iter().map(|x| format!("{:016x}", x.score_bits.to_bits())).collect();
                        let line = format!(
                            "{} {} {} {} {:016x}",
                            q.plan,
                            q.agent,
                            ids.join(","),
                            bits.join(","),
                            m.routing_entropy_bits.to_bits()
                        );
                        let w = [
                            mt.query_terms as u64,
                            mt.candidate_records as u64,
                            mt.posted_rows_visited as u64,
                            mt.scored_records as u64,
                            mt.background_pad.ceil() as u64,
                        ];
                        mine.push((i, ns, cpu, pcpu, line, w));
                    }
                    res.lock().unwrap().extend(mine);
                });
            }
        });
        let wall = t0.elapsed().as_secs_f64();
        let mut lat = Lat::default();
        let mut cpu = Lat::default();
        let mut pcpu = Lat::default();
        work = [0; 5];
        for (i, ns, c, pc, line, w) in res.into_inner().unwrap() {
            lat.add_ns(ns);
            cpu.add_ns(c);
            pcpu.add_ns(pc);
            for j in 0..4 {
                work[j] += w[j];
            }
            if round == 0 {
                per_query.push(format!("{} {} {} {} {} {} {} {}", qs[i].plan, c, ns, w[0], w[1], w[2], w[3], w[4]));
                lines[i] = Some(line);
            } else if lines[i].as_deref() != Some(line.as_str()) {
                return Err(format!("round {round}: query {i} differs between rounds").into());
            }
        }
        let mut s = lat.summary(Some(wall));
        s["round"] = json!(round);
        s["cpu"] = cpu.summary(None);
        if threads <= 1 {
            s["process_cpu"] = pcpu.summary(None);
        }
        s["minor_faults_per_query"] = json!((minor_faults() - faults) as f64 / qs.len().max(1) as f64);
        summaries.push(s);
    }
    let nq = qs.len().max(1) as f64;
    let rec = json!({
        "label": label, "dir": dir, "records": info["records"], "single": single, "size": size,
        "stores": rows.len(), "rows_total": rows.iter().sum::<usize>(), "rows_max": rows.iter().max(),
        "queries": qs.len(), "mean_query_bytes": qbytes as f64 / nq, "threads": threads, "k": k,
        "query_threads": qthreads, "exact_entropy": exact_entropy, "max_df": max_df,
        "dropped_words_per_query": dropped as f64 / qs.len().max(1) as f64, "all_frequent_queries": all_frequent,
        "mean_terms": work[0] as f64 / nq, "mean_candidates": work[1] as f64 / nq,
        "mean_postings": work[2] as f64 / nq, "mean_scored": work[3] as f64 / nq,
        "rounds": summaries, "loadavg_before": load_before, "loadavg_after": loadavg(),
        "bin": std::env::current_exe().ok().map(|p| p.display().to_string()),
    });
    let mut rec = rec;
    let prof = oojuro_trace::profile_snapshot();
    if !prof.is_empty() {
        // Per query: phases in ms, counters (n_*) as counts.
        let per = (qs.len().max(1) * rounds.max(1)) as f64;
        let m: serde_json::Map<String, Value> = prof
            .into_iter()
            .map(|(n, v)| (n.to_string(), json!(v as f64 / per / if n.starts_with("n_") { 1.0 } else { 1e6 })))
            .collect();
        rec["profile_per_query"] = Value::Object(m);
    }
    println!("{rec}");
    if let Some(o) = out {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(o)?;
        writeln!(f, "{rec}")?;
    }
    if let Some(wf) = work_out {
        std::fs::write(wf, per_query.join("\n") + "\n")?;
    }
    if let Some(d) = dump {
        let mut f = std::io::BufWriter::new(std::fs::File::create(d)?);
        for l in lines.into_iter().flatten() {
            writeln!(f, "{l}")?;
        }
    }
    Ok(())
}

/// Compare two dumps line by line: ids, score bits and entropy bits.
fn compare(v: &[String]) -> Result<()> {
    let read = |p: &str| -> Result<Vec<String>> {
        Ok(std::io::BufReader::new(std::fs::File::open(Path::new(p))?).lines().collect::<std::io::Result<_>>()?)
    };
    let a = read(v.get(1).ok_or("compare A B")?)?;
    let b = read(v.get(2).ok_or("compare A B")?)?;
    if a.len() != b.len() {
        return Err(format!("{} vs {} lines", a.len(), b.len()).into());
    }
    let (mut same, mut ids_same, mut entropy_diff) = (0, 0, 0);
    let mut shown = 0;
    for (x, y) in a.iter().zip(&b) {
        if x == y {
            same += 1;
            ids_same += 1;
            continue;
        }
        let fx: Vec<&str> = x.split(' ').collect();
        let fy: Vec<&str> = y.split(' ').collect();
        if fx[..fx.len() - 1] == fy[..fy.len() - 1] {
            entropy_diff += 1;
        }
        if fx.get(2) == fy.get(2) {
            ids_same += 1;
        }
        if shown < 5 {
            println!("- {x}\n+ {y}");
            shown += 1;
        }
    }
    println!(
        "{}",
        json!({"lines": a.len(), "identical": same, "same_ids": ids_same,
               "only_entropy_differs": entropy_diff})
    );
    if same != a.len() {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    let v: Vec<String> = std::env::args().skip(1).collect();
    match v.first().map(String::as_str) {
        Some("build") => build(&v),
        Some("run") => run(&v),
        Some("compare") => compare(&v),
        Some("kinds") => kinds(&v),
        _ => Err("usage: searchprobe build|run|compare|kinds ...".into()),
    }
}
