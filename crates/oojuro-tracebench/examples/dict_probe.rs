//! Starter dictionary policy, measured with the store's own run writer.
//!
//! Training rows: a TBEV0001 file (`train`), only sessions whose hash falls
//! in the training quarter (`hash % 4 == 0`), so the same file can be
//! evaluated on the other three quarters. Evaluation: per agent store of
//! `eval` (other quarters only when eval == train), the agent's rows in
//! append order, prefixes of 1, 4, 16 MiB and the whole store (at most
//! 64 MiB), written as one
//! run each way:
//!   z3        zstd 3, no dictionary          (flush runs today)
//!   z3+S      zstd 3, starter dictionary     (shared, not stored in the run)
//!   z19+own   zstd 19, own trained dictionary (merged runs today; embedded)
//!   z19+S     zstd 19, starter dictionary
//! Rows look like the trace layer's engine rows (id, metadata line, tool,
//! args, text). Bytes per raw text byte, summed over agents.
//!
//! Usage: dict_probe <train.tbe> <eval.tbe> <dict_kib> [write_dict_path]
use oojuro_store::run::{RunOptions, SharedDict, write_run};
use oojuro_tracebench::events::{Base, Event, Tier};
use std::collections::HashMap;

fn quarter(session: u64) -> u64 {
    let h = blake3::hash(format!("dict|{session}").as_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap()) % 4
}

fn row(e: &Event) -> (Vec<u8>, usize) {
    let ex = e.exit_code.map(|x| x.to_string()).unwrap_or_default();
    let meta = format!(
        "{},{},{},{},{},{},{},{},{},{},{}",
        e.session, e.turn, e.seq, e.parent, e.kind, e.status, ex, e.tokens_in, e.tokens_out, e.duration_us, e.revises
    );
    let mut r = Vec::with_capacity(e.text.len() + 64);
    r.extend_from_slice(format!("{}\x01e\x02{}\x01t\x02{}\x01a\x02{}\x01", e.id, meta, e.tool, e.args).as_bytes());
    r.extend_from_slice(e.text.as_bytes());
    (r, e.text.len() + e.tool.len() + e.args.len())
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (train_path, eval_path) = (&a[0], &a[1]);
    let dict_kib: usize = a[2].parse().unwrap();
    let train = Base::open(train_path.as_ref()).unwrap();
    let tier = Tier::new(&train, 1);
    let mut b = Vec::new();
    let mut samples: Vec<Vec<u8>> = Vec::new();
    for i in 0..tier.plan.len() {
        if quarter(tier.plan[i].session) != 0 {
            continue;
        }
        tier.batch(i, &mut b);
        for e in &b {
            samples.push(row(e).0);
        }
    }
    let dict = oojuro_store::store::train_dict(&samples, dict_kib << 10).expect("dictionary");
    drop(samples);
    if let Some(p) = a.get(3) {
        std::fs::write(p, &dict).unwrap();
    }
    let shared = SharedDict::new(&dict);
    let same = train_path == eval_path;
    let eval = Base::open(eval_path.as_ref()).unwrap();
    let et = Tier::new(&eval, 1);
    let mut agents: HashMap<u32, (Vec<Vec<u8>>, Vec<usize>)> = HashMap::new();
    for i in 0..et.plan.len() {
        if same && quarter(et.plan[i].session) == 0 {
            continue;
        }
        et.batch(i, &mut b);
        for e in &b {
            let (r, raw) = row(e);
            let x = agents.entry(e.agent).or_default();
            x.0.push(r);
            x.1.push(raw);
        }
    }
    let tmp = std::env::var("TMPDIR").unwrap_or("/tmp".into());
    let path = std::path::PathBuf::from(tmp).join(format!("dict-probe-{}.run", std::process::id()));
    let variants: [(&str, i32, bool, bool); 4] = [("z3", 3, false, false), ("z3+S", 3, false, true), ("z19+own", 19, true, false), ("z19+S", 19, false, true)];
    // the last one: the whole store, capped at 64 MiB
    let prefixes = [1usize << 20, 4 << 20, 16 << 20, usize::MAX];
    let cap_whole = 64usize << 20;
    let mut tot = vec![[0u64; 2]; prefixes.len() * variants.len()];
    let mut ids: Vec<&u32> = agents.keys().collect();
    ids.sort();
    for id in ids {
        let (rows, raws) = &agents[id];
        for (pi, &cap) in prefixes.iter().enumerate() {
            let mut n = 0;
            let mut acc = 0usize;
            let lim = cap.min(cap_whole);
            while n < raws.len() && acc < lim {
                acc += raws[n];
                n += 1;
            }
            if cap != usize::MAX && acc < cap {
                continue; // agent smaller than this prefix
            }
            for (vi, (_, level, own, starter)) in variants.iter().enumerate() {
                let opts = RunOptions {
                    level: *level,
                    dict_bytes: if *own { 512 << 10 } else { 0 },
                    shared_dict: starter.then(|| shared.clone()),
                    threads: 2,
                    ..RunOptions::default()
                };
                let _ = std::fs::remove_file(&path);
                let info = write_run(&path, &mut rows[..n].iter().cloned(), &opts).unwrap();
                let t = &mut tot[pi * variants.len() + vi];
                t[0] += info.file_bytes;
                t[1] += acc as u64;
            }
        }
    }
    let _ = std::fs::remove_file(&path);
    let mut out = serde_json::Map::new();
    out.insert("dict_bytes".into(), dict.len().into());
    for (pi, p) in prefixes.iter().enumerate() {
        let name = if *p == usize::MAX { "whole_le64MiB".to_string() } else { format!("{}MiB", p >> 20) };
        let mut m = serde_json::Map::new();
        for (vi, v) in variants.iter().enumerate() {
            let t = tot[pi * variants.len() + vi];
            if t[1] > 0 {
                m.insert(v.0.into(), format!("{:.4}", t[0] as f64 / t[1] as f64).into());
            }
        }
        out.insert(name, serde_json::Value::Object(m));
    }
    println!("{}", serde_json::Value::Object(out));
}
