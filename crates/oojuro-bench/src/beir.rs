//! BEIR retrieval: bulk-load a corpus, answer every split query, score with
//! trec_eval-compatible metrics. Documents carry no time (valid_at = 0), so a
//! time-aware arm must be neutral here.
use crate::{
    ArmSpec, Doc, Result, SearchReq,
    metrics::{self, Qrels},
    scorecard::{Ctx, Runs, Scorecard},
    sys,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

pub struct Options {
    pub dataset: String,
    pub split: String,
    pub k: usize,
    pub batch: usize,
    pub arms: Vec<ArmSpec>,
    pub out: PathBuf,
    pub run_file: Option<PathBuf>,
    pub limit_queries: Option<usize>,
    /// Arm settings (`--config key=value`), as for the turn loop.
    pub config: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    title: String,
    text: String,
}

pub struct Beir {
    pub docs: Vec<Doc>,
    pub queries: BTreeMap<String, String>,
    pub qrels: BTreeMap<String, Qrels>,
}

pub fn repo_bench_dir() -> PathBuf {
    std::env::var("OOJURO_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench"))
}

/// Query ids of a frozen split ("test" = every labelled test query).
pub fn split_ids(dataset: &str, split: &str) -> Result<Option<BTreeSet<String>>> {
    if split == "test" || split == "all" {
        return Ok(None);
    }
    let path = repo_bench_dir().join("splits").join(format!("{dataset}.json"));
    let v: Value = serde_json::from_slice(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)?;
    let ids = v.get(split).and_then(Value::as_array).ok_or_else(|| format!("split {split:?} missing in {}", path.display()))?;
    Ok(Some(ids.iter().filter_map(Value::as_str).map(String::from).collect()))
}

pub fn load(ctx: &Ctx, dataset: &str) -> Result<Beir> {
    let short = dataset.strip_prefix("beir-").unwrap_or(dataset);
    let dir = ctx.data.join(dataset).join(short);
    let lines = |name: &str| -> Result<Vec<Line>> {
        let path = dir.join(name);
        let file = File::open(&path).map_err(|e| format!("{}: {e} (run scripts/bench/fetch.py {dataset})", path.display()))?;
        BufReader::new(file).lines().map(|l| Ok(serde_json::from_str(&l?)?)).collect()
    };
    let docs = lines("corpus.jsonl")?
        .into_iter()
        .map(|l| Doc {
            text: if l.title.is_empty() { l.text } else { format!("{}\n{}", l.title, l.text) },
            id: l.id,
            key: String::new(),
            valid_at: 0,
            recorded_at: 0,
            meta: BTreeMap::new(),
        })
        .collect();
    let queries = lines("queries.jsonl")?.into_iter().map(|l| (l.id, l.text)).collect();
    let mut qrels: BTreeMap<String, Qrels> = BTreeMap::new();
    for line in BufReader::new(File::open(dir.join("qrels/test.tsv"))?).lines().skip(1) {
        let line = line?;
        let f: Vec<_> = line.split('\t').collect();
        if f.len() != 3 {
            return Err(format!("bad qrels line {line:?}").into());
        }
        qrels.entry(f[0].into()).or_default().insert(f[1].into(), f[2].trim().parse()?);
    }
    Ok(Beir { docs, queries, qrels })
}

fn references(dataset: &str) -> Vec<Value> {
    let path = repo_bench_dir().join("references.json");
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { return Vec::new() };
    v.get("rows")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter(|r| r["dataset"] == dataset).cloned().collect())
        .unwrap_or_default()
}

pub fn run(ctx: &Ctx, o: &Options) -> Result<Scorecard> {
    let data = load(ctx, &o.dataset)?;
    let split = split_ids(&o.dataset, &o.split)?;
    let mut qids: Vec<&String> = data
        .qrels
        .iter()
        .filter(|(q, rel)| metrics::relevant_count(rel) > 0 && data.queries.contains_key(*q))
        .filter(|(q, _)| split.as_ref().is_none_or(|s| s.contains(*q)))
        .map(|(q, _)| q)
        .collect();
    if let Some(n) = o.limit_queries {
        qids.truncate(n);
    }
    let digests: HashMap<&str, blake3::Hash> = data.docs.iter().map(|d| (d.id.as_str(), blake3::hash(d.text.as_bytes()))).collect();
    let raw_text: u64 = data.docs.iter().map(|d| d.text.len() as u64).sum();
    let mut card = Scorecard::new("beir", &o.dataset, &o.split, ctx);
    card.settings.insert("k".into(), json!(o.k));
    card.settings.insert("queries".into(), json!(qids.len()));
    card.settings.insert("documents".into(), json!(data.docs.len()));
    card.settings.insert("batch_docs".into(), json!(o.batch));
    card.settings.insert("cache".into(), json!("warm: queries run in the build process right after the last append"));
    card.settings.insert("build_time_included".into(), json!("reported separately as build_s; not in query latency"));
    card.settings.insert("identical_ids".into(), json!("a hit whose id equals the query id is dropped before scoring (BEIR ignore_identical_ids)"));
    card.settings.insert("averaging".into(), json!("mean over every labelled split query; an empty result scores 0"));
    card.settings.insert("returned_content".into(), json!("search_get includes search, top-k ID filtering and byte-exact original get for every returned hit; IDs-only search remains separate"));
    card.references = references(&o.dataset);
    if o.split != "test" {
        card.notes.push("Published references use the full BEIR test split; this run uses a frozen subset.".into());
    }
    let _lock = ctx.bench_lock()?;
    for spec in &o.arms {
        let mut runs = Runs::default();
        let mut info = Default::default();
        let mut first: Option<Vec<Vec<String>>> = None;
        let mut error = None;
        for run in 0..ctx.runs {
            let outcome = (|| -> Result<()> {
                let mut s = ctx.session_with(spec, run, "beir", &o.config)?;
                info = s.info.clone();
                let started = Instant::now();
                for chunk in data.docs.chunks(o.batch) {
                    s.arm.append_batch(chunk.to_vec())?;
                }
                let build = started.elapsed().as_secs_f64();
                runs.add("speed", "build_s", build);
                runs.add("speed", "build_docs_per_s", data.docs.len() as f64 / build);
                let steady = s.arm.memory();
                let mut per_metric: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
                let (mut arm_us, mut total_us, mut content_us) = (Vec::new(), Vec::new(), Vec::new());
                let mut lists = Vec::with_capacity(qids.len());
                let mut run_lines = String::new();
                let mut checked = 0usize;
                let mut returned_bytes = 0usize;
                let mut bad = BTreeSet::new();
                for qid in &qids {
                    let query = SearchReq {
                        text: data.queries[*qid].clone(),
                        valid_at: 1,
                        known_at: 1,
                        k: o.k + 1,
                        context: BTreeMap::new(),
                    };
                    let started = Instant::now();
                    let (hits, t) = s.arm.search(query)?;
                    arm_us.push(t.arm_us as f64);
                    total_us.push(t.total_us as f64);
                    let ranked: Vec<String> = hits.into_iter().map(|h| h.id).filter(|id| id != *qid).take(o.k).collect();
                    let texts = s.arm.get(ranked.clone())?;
                    content_us.push(started.elapsed().as_secs_f64() * 1e6);
                    if texts.len() != ranked.len() { return Err("get returned a different number of originals".into()); }
                    for (id, text) in ranked.iter().zip(texts) {
                        checked += 1;
                        returned_bytes += text.as_ref().map_or(0, String::len);
                        if !digests.contains_key(id.as_str()) || text.as_ref().map(|t| blake3::hash(t.as_bytes())) != digests.get(id.as_str()).copied() {
                            bad.insert(id.clone());
                        }
                    }
                    for (name, value) in metrics::standard(&ranked, &data.qrels[*qid]) {
                        per_metric.entry(name).or_default().push(value);
                    }
                    if run == 0 && o.run_file.is_some() {
                        for (rank, id) in ranked.iter().enumerate() {
                            run_lines.push_str(&format!("{qid} Q0 {id} {} {} {}\n", rank + 1, o.k - rank, spec.label));
                        }
                    }
                    lists.push(ranked);
                }
                for (name, values) in per_metric {
                    runs.add("quality", name, metrics::mean(&values));
                }
                runs.add("speed", "search_p50_us", sys::percentile(&arm_us, 50.0));
                runs.add("speed", "search_p99_us", sys::percentile(&arm_us, 99.0));
                runs.add("speed", "search_roundtrip_p50_us", sys::percentile(&total_us, 50.0));
                runs.add("speed", "search_roundtrip_p99_us", sys::percentile(&total_us, 99.0));
                runs.add("speed", "qps_serial", qids.len() as f64 / (total_us.iter().sum::<f64>() / 1e6));
                runs.add("speed", "search_get_p50_us", sys::percentile(&content_us, 50.0));
                runs.add("speed", "search_get_p99_us", sys::percentile(&content_us, 99.0));
                runs.add("speed", "search_get_qps_serial", qids.len() as f64 / (content_us.iter().sum::<f64>() / 1e6));
                runs.add("speed", "returned_original_bytes", returned_bytes as f64);
                runs.gate("byte_exact", bad.is_empty(), json!({"checked": checked, "mismatched": bad.len(), "first": bad.first()}));
                match &first {
                    None => first = Some(lists),
                    Some(base) => {
                        let same = base.iter().zip(&lists).filter(|(a, b)| a == b).count();
                        runs.gate("determinism", same == base.len(), json!({"identical_rankings": same, "queries": base.len()}));
                    }
                }
                if let (0, Some(path)) = (run, &o.run_file) {
                    let path = path.with_file_name(format!(
                        "{}.{}",
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("run"),
                        spec.label
                    ));
                    File::create(&path)?.write_all(run_lines.as_bytes())?;
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
            eprintln!("beir {} {} run {run} done", o.dataset, spec.label);
        }
        card.arms.push(runs.into_result(spec, info, error));
    }
    card.save(&o.out)?;
    Ok(card)
}
