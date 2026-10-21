//! Exploration-only BEIR runner for the lexical track. Final comparisons use the
//! shared harness; this writes TREC runs for scripts/lexical_beir_eval.py.
//!
//! beir_explore DATASET_DIR SPLIT STORE_DIR RUN_PREFIX [CONFIG...]
//!
//! The store is built once (batched inserts) and reused. Each CONFIG is
//! `BACKGROUND:MU` with BACKGROUND `candidates` (v3) or `view`; the default
//! is `candidates:128`. Each writes RUN_PREFIX-BACKGROUND-MU.run.
use oojuro_core::Record;
use oojuro_nine::{Background, Nine, Options, Receipts};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
    time::Instant,
};

#[derive(Deserialize)]
struct Text {
    #[serde(rename = "_id")]
    id: String,
    text: String,
    #[serde(default)]
    title: String,
}

fn lines<T: for<'a> Deserialize<'a>>(path: &Path) -> Vec<T> {
    BufReader::new(File::open(path).expect("open"))
        .lines()
        .map(|l| serde_json::from_str(&l.expect("line")).expect("json"))
        .collect()
}

fn main() -> oojuro_core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dataset = Path::new(&args[1]);
    let split = &args[2];
    let store = Path::new(&args[3]);
    let prefix = &args[4];
    let configs: Vec<String> = if args.len() > 5 {
        args[5..].to_vec()
    } else {
        vec!["candidates:128".into()]
    };
    let depth = 101;
    let wanted: BTreeSet<String> = BufReader::new(File::open(
        dataset.join("qrels").join(format!("{split}.tsv")),
    )?)
    .lines()
    .skip(1)
    .map(|l| l.expect("qrel").split('\t').next().expect("qid").to_string())
    .collect();
    let queries: Vec<Text> = lines::<Text>(&dataset.join("queries.jsonl"))
        .into_iter()
        .filter(|q| wanted.contains(&q.id))
        .collect();
    if !store.join("manifest.nine").exists() {
        let corpus: Vec<Text> = lines(&dataset.join("corpus.jsonl"));
        let started = Instant::now();
        let db = Nine::open(store)?;
        for chunk in corpus.chunks(512) {
            db.insert_batch(
                chunk
                    .iter()
                    .map(|d| Record::new(&d.id, &d.id, &format!("{}\n{}", d.title, d.text), 1))
                    .collect(),
            )?;
        }
        db.flush()?;
        eprintln!(
            "built {} documents in {:.1}s",
            corpus.len(),
            started.elapsed().as_secs_f64()
        );
    }
    for config in configs {
        let (background, mu) = config.split_once(':').ok_or("CONFIG is BACKGROUND:MU")?;
        let options = Options {
            background: match background {
                "candidates" => Background::Candidates,
                "view" => Background::View,
                _ => return Err("background must be candidates or view".into()),
            },
            mu: mu.parse()?,
            receipts: Receipts::Off,
            ..Options::default()
        };
        let db = Nine::open_with(store, options)?;
        let mut times = Vec::with_capacity(queries.len());
        let mut run = std::io::BufWriter::new(File::create(format!("{prefix}-{background}-{mu}.run"))?);
        for q in &queries {
            let t = Instant::now();
            let found = db.find(&q.text, i64::MIN, 1, depth)?;
            times.push(t.elapsed().as_secs_f64() * 1e3);
            for (rank, m) in found.matches.iter().enumerate() {
                writeln!(run, "{} Q0 {} {} {} nine", q.id, m.id, rank + 1, m.score_bits)?;
            }
        }
        run.flush()?;
        times.sort_by(f64::total_cmp);
        let pct = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize];
        println!(
            "{}",
            serde_json::json!({"config": config, "queries": queries.len(),
                "search_ms_p50": pct(0.5), "search_ms_p99": pct(0.99)})
        );
    }
    Ok(())
}
