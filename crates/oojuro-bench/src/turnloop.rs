//! Turn-loop replay, the primary workload. Before each agent turn the turn
//! input searches the whole store for the top 20; after the turn its thinking,
//! tool call and tool result are appended as one durable batch and must be
//! searchable at the next turn. Labels come from the trajectory itself (see
//! scripts/bench/prepare.py and docs/BENCH_HARNESS.md); they never reach arms.
use crate::{
    ArmSpec, Doc, Result, SearchReq,
    metrics::{self, Qrels},
    scorecard::{Ctx, Runs, Scorecard},
    sys,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    time::Instant,
};

pub const BACKGROUND_T0: i64 = 1_700_000_000;
pub const BACKGROUND_GAP: i64 = 7_200;
pub const FAR_FUTURE: i64 = 4_000_000_000;

#[derive(Clone, Deserialize)]
pub struct Rec {
    pub id: String,
    pub kind: String,
    pub dt: i64,
    pub text: String,
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize)]
pub struct Step {
    pub dt: i64,
    pub query: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub relevant: BTreeMap<String, i64>,
    pub records: Vec<Rec>,
}

#[derive(Clone, Deserialize)]
pub struct Traj {
    pub traj: String,
    pub group: String,
    pub split: String,
    pub init: Vec<Rec>,
    pub steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Head {
    group: String,
    split: String,
}

pub struct Options {
    pub set: String,
    pub split: String,
    pub trajectories: usize,
    pub background: usize,
    pub k: usize,
    pub batch: usize,
    /// index (default) | drop: whether thinking records are appended at all.
    /// Store-only indexing is an arm concern: pass --config thinking=store.
    pub thinking: String,
    pub config: BTreeMap<String, String>,
    pub arms: Vec<ArmSpec>,
    pub out: PathBuf,
    /// Candidates requested per turn (>= k); metrics still use the top k.
    pub pool: usize,
    /// Write each labelled turn's candidate pool (run 0) to `<dump>.<label>`
    /// for offline re-ranking/fusion tuning. Labels are in the dump; the arm
    /// never sees them.
    pub dump: Option<PathBuf>,
}

/// (session, turn, kind, path) of a record, kept only when dumping pools.
type Meta = HashMap<String, (String, i64, String, String)>;

fn note(meta: &mut Option<Meta>, d: &Doc) {
    if let Some(m) = meta {
        let get = |k: &str| d.meta.get(k).cloned().unwrap_or_default();
        let turn = d.meta.get("turn").and_then(|t| t.parse().ok()).unwrap_or(-1);
        m.insert(d.id.clone(), (get("session"), turn, get("kind"), get("path")));
    }
}

fn path(ctx: &Ctx, set: &str) -> PathBuf {
    ctx.data.join("prepared").join("turnloop").join(format!("{set}.jsonl"))
}

fn doc(r: &Rec, start: i64, session: &str, suffix: &str, turn: Option<usize>) -> Doc {
    let mut meta = r.meta.clone();
    meta.insert("session".into(), session.into());
    meta.insert("kind".into(), r.kind.clone());
    if let Some(t) = turn {
        meta.insert("turn".into(), t.to_string());
    }
    Doc {
        id: format!("{}{suffix}", r.id),
        key: String::new(),
        text: r.text.clone(),
        valid_at: start + r.dt,
        recorded_at: start + r.dt,
        meta,
    }
}

/// Every record of a trajectory as background docs, in append order.
fn traj_docs(t: &Traj, start: i64, suffix: &str, thinking: bool) -> Vec<Doc> {
    let session = format!("{}{suffix}", t.traj);
    let mut out: Vec<Doc> = t.init.iter().map(|r| doc(r, start, &session, suffix, None)).collect();
    for (i, s) in t.steps.iter().enumerate() {
        out.extend(s.records.iter().filter(|r| thinking || r.kind != "thinking").map(|r| doc(r, start, &session, suffix, Some(i))));
    }
    out
}

fn digest(text: &str) -> u128 {
    u128::from_le_bytes(blake3::hash(text.as_bytes()).as_bytes()[..16].try_into().unwrap())
}

/// (valid_at, recorded_at, digest) of everything appended, for leak and
/// exactness checks on returned ids.
type Registry = HashMap<String, (i64, i64, u128)>;

pub fn run(ctx: &Ctx, o: &Options) -> Result<Scorecard> {
    let file = path(ctx, &o.set);
    let open = || -> Result<BufReader<File>> {
        Ok(BufReader::new(File::open(&file).map_err(|e| {
            format!("{}: {e} (run scripts/bench/prepare.py turnloop {})", file.display(), o.set)
        })?))
    };
    // Measured trajectories: the first N of the split, in the file's frozen order.
    let mut measured: Vec<Traj> = Vec::new();
    for line in open()?.lines() {
        let line = line?;
        let head: Head = serde_json::from_str(&line)?;
        if head.split == o.split || o.split == "all" {
            measured.push(serde_json::from_str(&line)?);
            if measured.len() == o.trajectories {
                break;
            }
        }
    }
    if measured.is_empty() {
        return Err(format!("no {} trajectories in {}", o.split, file.display()).into());
    }
    let groups: BTreeSet<String> = measured.iter().map(|t| t.group.clone()).collect();
    let thinking = o.thinking != "drop";
    let steps: usize = measured.iter().map(|t| t.steps.len()).sum();
    let labelled: usize = measured.iter().flat_map(|t| &t.steps).filter(|s| !s.relevant.is_empty()).count();
    let mut card = Scorecard::new("turnloop", &o.set, &o.split, ctx);
    for (k, v) in [
        ("k", json!(o.k)),
        ("trajectories", json!(measured.len())),
        ("steps", json!(steps)),
        ("labelled_steps", json!(labelled)),
        ("background_target_records", json!(o.background)),
        ("thinking", json!(o.thinking)),
        ("arm_config", json!(o.config)),
        ("cache", json!("warm: one process per run; prefill, then measured replay")),
        ("build_time_included", json!("prefill reported as prefill_s; per-turn overhead = turn search + turn append")),
        ("self_exclusion", json!("the turn input's own record is dropped from results before scoring; requested k+1")),
        ("returned_content", json!("search_get includes search, top-k source exclusion and original get for every returned hit; all originals checked byte-exact; native projection also computes excerpts")),
        ("pool", json!(o.pool.max(o.k))),
    ] {
        card.settings.insert(k.into(), v);
    }
    card.notes.push("Quality is averaged over labelled steps only; every step is timed.".into());
    let _lock = ctx.bench_lock()?;
    for spec in &o.arms {
        let mut runs = Runs::default();
        let mut info = Default::default();
        let mut first: Option<Vec<Vec<String>>> = None;
        let mut error = None;
        for run in 0..ctx.runs {
            let outcome = (|| -> Result<()> {
                let mut s = ctx.session_with(spec, run, "turnloop", &o.config)?;
                info = s.info.clone();
                let mut registry: Registry = HashMap::new();
                let mut meta: Option<Meta> = (run == 0 && o.dump.is_some()).then(HashMap::new);
                let mut dump = match (&o.dump, run) {
                    (Some(p), 0) => Some(std::io::BufWriter::new(File::create(
                        p.with_file_name(format!("{}.{}", p.file_name().and_then(|n| n.to_str()).unwrap_or("pools"), spec.label)),
                    )?)),
                    _ => None,
                };
                let mut raw_text = 0u64;
                // Prefill: background trajectories (other task groups), repeated with
                // id suffixes when the file holds fewer records than requested.
                let started = Instant::now();
                let mut pending: Vec<Doc> = Vec::new();
                let (mut appended, mut j, mut pass) = (0usize, 0i64, 0usize);
                'fill: while appended < o.background {
                    let before = appended;
                    for line in open()?.lines() {
                        let line = line?;
                        let head: Head = serde_json::from_str(&line)?;
                        if groups.contains(&head.group) {
                            continue;
                        }
                        let t: Traj = serde_json::from_str(&line)?;
                        let suffix = if pass == 0 { String::new() } else { format!("~r{pass}") };
                        for d in traj_docs(&t, BACKGROUND_T0 + j * BACKGROUND_GAP, &suffix, thinking) {
                            registry.insert(d.id.clone(), (d.valid_at, d.recorded_at, digest(&d.text)));
                            note(&mut meta, &d);
                            raw_text += d.text.len() as u64;
                            pending.push(d);
                            appended += 1;
                            if pending.len() == o.batch {
                                s.arm.append_batch(std::mem::take(&mut pending))?;
                            }
                            if appended >= o.background {
                                break 'fill;
                            }
                        }
                        j += 1;
                    }
                    if appended == before {
                        break; // nothing usable as background
                    }
                    pass += 1;
                }
                // Canaries: strong lexical matches that are invisible as of every
                // measured query, one from the future, one recorded too late.
                let measured_t0 = BACKGROUND_T0 + (j + 2) * BACKGROUND_GAP;
                for (m, t) in measured.iter().enumerate() {
                    let text = t.steps.get(1).or(t.steps.first()).map(|s| s.query.clone()).unwrap_or_default();
                    for (kind, valid, recorded) in [("future", FAR_FUTURE, FAR_FUTURE), ("unknown", measured_t0, FAR_FUTURE)] {
                        let d = Doc {
                            id: format!("canary:{kind}:{m}"),
                            key: String::new(),
                            text: text.clone(),
                            valid_at: valid,
                            recorded_at: recorded,
                            meta: BTreeMap::from([("kind".into(), "canary".into())]),
                        };
                        registry.insert(d.id.clone(), (d.valid_at, d.recorded_at, digest(&d.text)));
                            note(&mut meta, &d);
                        pending.push(d);
                    }
                }
                s.arm.append_batch(std::mem::take(&mut pending))?;
                let prefill = started.elapsed().as_secs_f64();
                runs.add("speed", "prefill_s", prefill);
                runs.add("speed", "prefill_records", appended as f64);
                runs.add("speed", "prefill_records_per_s", appended as f64 / prefill.max(1e-9));
                let steady = s.arm.memory();
                // Measured replay.
                let mut per_metric: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
                let (mut search_us, mut search_rt, mut append_us, mut append_rt, mut turn_us, mut content_us, mut content_turn_us) =
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                let mut returned_bytes = 0usize;
                let (mut self_hits, mut self_total, mut leaks, mut unknown, mut inexact, mut checked) = (0, 0, 0, 0, 0, 0);
                let mut readable = 0usize;
                let mut lists = Vec::new();
                for (m, t) in measured.iter().enumerate() {
                    let start = measured_t0 + m as i64 * 86_400;
                    let session = t.traj.clone();
                    let init: Vec<Doc> = t.init.iter().map(|r| doc(r, start, &session, "", None)).collect();
                    for d in &init {
                        registry.insert(d.id.clone(), (d.valid_at, d.recorded_at, digest(&d.text)));
                            note(&mut meta, &d);
                        raw_text += d.text.len() as u64;
                    }
                    s.arm.append_batch(init)?;
                    let mut mine: Vec<String> = t.init.iter().map(|r| r.id.clone()).collect();
                    for (i, step) in t.steps.iter().enumerate() {
                        let at = start + step.dt;
                        let query = SearchReq {
                            text: step.query.clone(),
                            valid_at: at,
                            known_at: at,
                            k: o.pool.max(o.k) + 1,
                            context: BTreeMap::from([
                                ("session".into(), session.clone()),
                                ("turn".into(), i.to_string()),
                                ("task".into(), t.group.clone()),
                            ]),
                        };
                        let started = Instant::now();
                        let (hits, ts) = s.arm.search(query)?;
                        let original_ids: Vec<_> = hits.iter().filter(|h| Some(&h.id) != step.source.as_ref()).take(o.k).map(|h| h.id.clone()).collect();
                        let originals = s.arm.get(original_ids.clone())?;
                        let search_get_us = started.elapsed().as_secs_f64() * 1e6;
                        if originals.len() != original_ids.len() { return Err("get returned a different number of originals".into()); }
                        for (id, text) in original_ids.iter().zip(originals) {
                            checked += 1;
                            returned_bytes += text.as_ref().map_or(0, String::len);
                            if !registry.contains_key(id) || text.map(|t| digest(&t)) != registry.get(id).map(|e| e.2) { inexact += 1; }
                        }
                        for h in &hits {
                            match registry.get(&h.id) {
                                None => unknown += 1,
                                Some((v, r, _)) if *v > at || *r > at || h.id.starts_with("canary:") => leaks += 1,
                                _ => {}
                            }
                        }
                        if let Some(src) = &step.source {
                            self_total += 1;
                            self_hits += usize::from(hits.iter().any(|h| &h.id == src));
                            let read = s.arm.get(vec![src.clone()])?;
                            let want = registry.get(src).map(|e| e.2);
                            readable += usize::from(read.into_iter().next().flatten().map(|t| digest(&t)) == want);
                        }
                        let pool: Vec<crate::Hit> =
                            hits.into_iter().filter(|h| Some(&h.id) != step.source.as_ref()).take(o.pool.max(o.k)).collect();
                        if let (Some(w), Some(m)) = (dump.as_mut(), meta.as_ref()) {
                            if !step.relevant.is_empty() {
                                use std::io::Write as _;
                                let rows: Vec<serde_json::Value> = pool
                                    .iter()
                                    .map(|h| {
                                        let (sess, turn, kind, path) = m.get(&h.id).cloned().unwrap_or_default();
                                        let valid = registry.get(&h.id).map_or(0, |e| e.0);
                                        json!([h.id, h.score, valid, sess, turn, kind, path])
                                    })
                                    .collect();
                                serde_json::to_writer(&mut *w, &json!({
                                    "traj": t.traj, "step": i, "at": at, "session": session, "turn": i,
                                    "query_bytes": step.query.len(), "relevant": step.relevant, "hits": rows,
                                }))?;
                                w.write_all(b"\n")?;
                            }
                        }
                        let ranked: Vec<String> = pool.into_iter().map(|h| h.id).take(o.k).collect();
                        if !step.relevant.is_empty() {
                            let rel: Qrels = step.relevant.iter().map(|(k, v)| (k.clone(), *v)).collect();
                            for (name, value) in [
                                ("ndcg@10", metrics::ndcg(&ranked, &rel, 10)),
                                ("ndcg@20", metrics::ndcg(&ranked, &rel, 20)),
                                ("recall@20", metrics::recall(&ranked, &rel, 20)),
                                ("mrr@10", metrics::mrr(&ranked, &rel, 10)),
                                ("hit@20", metrics::hit(&ranked, &rel, 20)),
                            ] {
                                per_metric.entry(name).or_default().push(value);
                            }
                        }
                        lists.push(ranked);
                        let batch: Vec<Doc> = step
                            .records
                            .iter()
                            .filter(|r| thinking || r.kind != "thinking")
                            .map(|r| doc(r, start, &session, "", Some(i)))
                            .collect();
                        for d in &batch {
                            registry.insert(d.id.clone(), (d.valid_at, d.recorded_at, digest(&d.text)));
                            note(&mut meta, &d);
                            raw_text += d.text.len() as u64;
                            mine.push(d.id.clone());
                        }
                        let ta = if batch.is_empty() { Default::default() } else { s.arm.append_batch(batch)? };
                        search_us.push(ts.arm_us as f64);
                        search_rt.push(ts.total_us as f64);
                        append_us.push(ta.arm_us as f64);
                        append_rt.push(ta.total_us as f64);
                        turn_us.push((ts.total_us + ta.total_us) as f64);
                        content_us.push(search_get_us);
                        content_turn_us.push(search_get_us + ta.total_us as f64);
                    }
                    // Every record of the trajectory reads back byte-identical.
                    for chunk in mine.chunks(256) {
                        let texts = s.arm.get(chunk.to_vec())?;
                        for (id, text) in chunk.iter().zip(texts) {
                            checked += 1;
                            if text.map(|t| digest(&t)) != registry.get(id).map(|e| e.2) {
                                inexact += 1;
                            }
                        }
                    }
                }
                for (name, values) in per_metric {
                    runs.add("quality", name, metrics::mean(&values));
                }
                runs.add("quality", "freshness_self_hit@21", self_hits as f64 / self_total.max(1) as f64);
                for (name, v) in [
                    ("search", &search_us),
                    ("search_roundtrip", &search_rt),
                    ("append", &append_us),
                    ("append_roundtrip", &append_rt),
                    ("turn_overhead", &turn_us),
                    ("search_get", &content_us),
                    ("turn_content_overhead", &content_turn_us),
                ] {
                    runs.add("speed", &format!("{name}_p50_us"), sys::percentile(v, 50.0));
                    runs.add("speed", &format!("{name}_p99_us"), sys::percentile(v, 99.0));
                }
                runs.add("speed", "store_records_end", registry.len() as f64);
                runs.add("speed", "returned_original_bytes", returned_bytes as f64);
                runs.gate("no_future_leak", leaks == 0 && unknown == 0, json!({"leaked_hits": leaks, "unknown_ids": unknown}));
                runs.gate("byte_exact", inexact == 0, json!({"checked": checked, "mismatched": inexact}));
                runs.gate(
                    "freshness",
                    readable == self_total,
                    json!({"readable": readable, "turns_with_source": self_total, "search_self_hits": self_hits,
                           "rule": "the previous turn's result (this turn's input) reads back exactly at the next turn; search visibility is the freshness_self_hit@21 metric"}),
                );
                match &first {
                    None => first = Some(lists),
                    Some(base) => {
                        let same = base.iter().zip(&lists).filter(|(a, b)| a == b).count();
                        runs.gate("determinism", same == base.len(), json!({"identical_rankings": same, "steps": base.len()}));
                    }
                }
                let (disk, end) = ctx.finish(s)?;
                runs.footprint(raw_text, disk, steady.as_ref(), end.as_ref());
                Ok(())
            })();
            if let Err(e) = outcome {
                eprintln!("{} run {run}: {e}", spec.label);
                error = Some(e.to_string());
                break;
            }
            eprintln!("turnloop {} {} run {run} done", o.set, spec.label);
        }
        card.arms.push(runs.into_result(spec, info, error));
    }
    card.save(&o.out)?;
    Ok(card)
}
