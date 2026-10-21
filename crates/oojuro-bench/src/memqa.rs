//! Memory QA retrieval (LongMemEval; LoCoMo once its license is cleared).
//! Each question gets its own fresh store holding its haystack, one record
//! per chat turn at the session's real date; the question is asked as of its
//! own date. Retrieval only: no reader model, no answer generation.
use crate::{
    ArmSpec, Doc, Result, SearchReq,
    metrics::{self, Qrels},
    scorecard::{Ctx, Runs, Scorecard},
    sys,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    time::Instant,
};

#[derive(Deserialize)]
struct Record {
    id: String,
    session: String,
    valid_at: i64,
    text: String,
    #[serde(default)]
    meta: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Question {
    qid: String,
    #[serde(rename = "type")]
    kind: String,
    split: String,
    question: String,
    at: i64,
    records: Vec<Record>,
    relevant: BTreeMap<String, i64>,
    relevant_sessions: Vec<String>,
}

pub struct Options {
    pub set: String,
    pub split: String,
    pub k: usize,
    pub limit: Option<usize>,
    pub arms: Vec<ArmSpec>,
    pub out: PathBuf,
    /// Write each question's top-k pool (run 0) to `<dump>.<label>`.
    pub dump: Option<PathBuf>,
}

pub fn run(ctx: &Ctx, o: &Options) -> Result<Scorecard> {
    let path = ctx.data.join("prepared").join("memqa").join(format!("{}.jsonl", o.set));
    let reader = BufReader::new(
        File::open(&path).map_err(|e| format!("{}: {e} (run scripts/bench/prepare.py memqa {})", path.display(), o.set))?,
    );
    let mut questions = Vec::new();
    for line in reader.lines() {
        let q: Question = serde_json::from_str(&line?)?;
        if (q.split == o.split || o.split == "all") && !q.relevant.is_empty() {
            questions.push(q);
        }
        if o.limit.is_some_and(|n| questions.len() >= n) {
            break;
        }
    }
    let mut card = Scorecard::new("memqa", &o.set, &o.split, ctx);
    for (k, v) in [
        ("k", json!(o.k)),
        ("questions", json!(questions.len())),
        ("unit", json!("one record per chat turn, text = '<role>: <content>', valid_at = session date + turn index seconds")),
        ("cache", json!("warm: one process per run, a fresh store per question")),
        ("build_time_included", json!("per-question build reported as build_ms; not in search latency")),
        ("reader", json!("none: retrieval metrics only, no LLM")),
    ] {
        card.settings.insert(k.into(), v);
    }
    card.notes.push("Abstention questions (no evidence turns) are excluded. Session recall maps ranked turns to their sessions in rank order.".into());
    let _lock = ctx.bench_lock()?;
    for spec in &o.arms {
        let mut runs = Runs::default();
        let mut info = Default::default();
        let mut error = None;
        for run in 0..ctx.runs {
            let outcome = (|| -> Result<()> {
                let mut s = ctx.session(spec, run, "memqa")?;
                let mut dump = match (&o.dump, run) {
                    (Some(p), 0) => Some(std::io::BufWriter::new(File::create(
                        p.with_file_name(format!("{}.{}", p.file_name().and_then(|n| n.to_str()).unwrap_or("pools"), spec.label)),
                    )?)),
                    _ => None,
                };
                info = s.info.clone();
                let mut per_metric: BTreeMap<String, Vec<f64>> = BTreeMap::new();
                let mut by_type: BTreeMap<(String, &str), Vec<f64>> = BTreeMap::new();
                let (mut build_ms, mut search_us) = (Vec::new(), Vec::new());
                let mut raw_text = 0u64;
                for (n, q) in questions.iter().enumerate() {
                    let dir = s.store.join(format!("q{n}"));
                    s.arm.open(&dir, &BTreeMap::new())?;
                    let docs: Vec<Doc> = q
                        .records
                        .iter()
                        .map(|r| {
                            let mut meta = r.meta.clone();
                            meta.insert("session".into(), r.session.clone());
                            Doc { id: r.id.clone(), key: String::new(), text: r.text.clone(), valid_at: r.valid_at, recorded_at: r.valid_at, meta }
                        })
                        .collect();
                    raw_text += docs.iter().map(|d| d.text.len() as u64).sum::<u64>();
                    let started = Instant::now();
                    for chunk in docs.chunks(1000) {
                        s.arm.append_batch(chunk.to_vec())?;
                    }
                    build_ms.push(started.elapsed().as_secs_f64() * 1e3);
                    let (hits, t) = s.arm.search(SearchReq {
                        text: q.question.clone(),
                        valid_at: q.at,
                        known_at: q.at,
                        k: o.k,
                        context: BTreeMap::new(),
                    })?;
                    search_us.push(t.arm_us as f64);
                    if let Some(w) = dump.as_mut() {
                        use std::io::Write as _;
                        let valid: BTreeMap<&str, (i64, &str)> =
                            q.records.iter().map(|r| (r.id.as_str(), (r.valid_at, r.session.as_str()))).collect();
                        let rows: Vec<serde_json::Value> = hits
                            .iter()
                            .map(|h| {
                                let (v, sess) = valid.get(h.id.as_str()).copied().unwrap_or((0, ""));
                                json!([h.id, h.score, v, sess])
                            })
                            .collect();
                        serde_json::to_writer(&mut *w, &json!({
                            "qid": q.qid, "type": q.kind, "at": q.at, "relevant": q.relevant,
                            "relevant_sessions": q.relevant_sessions, "hits": rows,
                        }))?;
                        w.write_all(b"\n")?;
                    }
                    let ranked: Vec<String> = hits.into_iter().map(|h| h.id).collect();
                    let session_of: BTreeMap<&str, &str> = q.records.iter().map(|r| (r.id.as_str(), r.session.as_str())).collect();
                    let mut sessions = Vec::new();
                    for id in &ranked {
                        if let Some(sid) = session_of.get(id.as_str()) {
                            if !sessions.contains(sid) {
                                sessions.push(*sid);
                            }
                        }
                    }
                    let gold: BTreeSet<&str> = q.relevant_sessions.iter().map(String::as_str).collect();
                    let session_recall = |k: usize, all: bool| {
                        let found = sessions.iter().take(k).filter(|s| gold.contains(**s)).count();
                        if all { found as f64 / gold.len().max(1) as f64 } else { f64::from(u8::from(found > 0)) }
                    };
                    let rel: Qrels = q.relevant.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    for (name, value) in [
                        ("turn_ndcg@10", metrics::ndcg(&ranked, &rel, 10)),
                        ("turn_recall@10", metrics::recall(&ranked, &rel, 10)),
                        ("turn_recall@20", metrics::recall(&ranked, &rel, 20)),
                        ("turn_recall@50", metrics::recall(&ranked, &rel, 50)),
                        ("session_recall_all@5", session_recall(5, true)),
                        ("session_recall_all@10", session_recall(10, true)),
                        ("session_recall_any@5", session_recall(5, false)),
                    ] {
                        per_metric.entry(name.to_string()).or_default().push(value);
                        by_type.entry((q.kind.clone(), name)).or_default().push(value);
                    }
                    s.arm.close()?;
                    if !ctx.keep {
                        let _ = std::fs::remove_dir_all(&dir);
                    }
                }
                for (name, values) in per_metric {
                    runs.add("quality", &name, metrics::mean(&values));
                }
                for ((kind, name), values) in by_type {
                    runs.add_breakdown(&kind, name, metrics::mean(&values));
                }
                runs.add("speed", "build_ms_p50", sys::percentile(&build_ms, 50.0));
                runs.add("speed", "search_p50_us", sys::percentile(&search_us, 50.0));
                runs.add("speed", "search_p99_us", sys::percentile(&search_us, 99.0));
                runs.add("storage", "raw_text_bytes", raw_text as f64);
                if let Some(v) = s.arm.memory().and_then(|m| m.cgroup_peak) {
                    runs.add("memory", "peak_cgroup_bytes", v as f64);
                }
                s.arm.open(&s.store.join("final"), &BTreeMap::new())?;
                ctx.finish(s)?;
                Ok(())
            })();
            if let Err(e) = outcome {
                eprintln!("{} run {run}: {e}", spec.label);
                error = Some(e.to_string());
                break;
            }
            eprintln!("memqa {} {} run {run} done", o.set, spec.label);
        }
        card.arms.push(runs.into_result(spec, info, error));
    }
    card.save(&o.out)?;
    Ok(card)
}
