//! Bytes per raw byte by record kind: per agent store, the rows of one kind
//! (engine-record shaped, append order, exact repeats removed as record
//! dedupe would) written as one run, zstd 19 + own dictionary (archive
//! tier) and zstd 3 + the starter dictionary (flush tier). Sums over agents.
//!
//! Usage: kind_probe <events.tbe>
use oojuro_store::run::{RunOptions, write_run};
use oojuro_tracebench::events::{Base, Event, KIND_NAMES, Tier};
use std::collections::{HashMap, HashSet};

fn row(e: &Event) -> Vec<u8> {
    let ex = e.exit_code.map(|x| x.to_string()).unwrap_or_default();
    let meta = format!("{},{},{},{},{},{},{},{},{},{},{}", e.session, e.turn, e.seq, e.parent, e.kind, e.status, ex, e.tokens_in, e.tokens_out, e.duration_us, e.revises);
    let mut r = format!("{}\x01e\x02{}\x01t\x02{}\x01a\x02{}\x01", e.id, meta, e.tool, e.args).into_bytes();
    r.extend_from_slice(e.text.as_bytes());
    r
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let base = Base::open(path.as_ref()).unwrap();
    let tier = Tier::new(&base, 1);
    let mut rows: HashMap<(u8, u32), Vec<Vec<u8>>> = HashMap::new();
    let mut raw: HashMap<u8, (u64, u64)> = HashMap::new();
    let mut seen: HashSet<(u32, [u8; 32])> = HashSet::new();
    let mut b = Vec::new();
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        for e in &b {
            if e.is_revision() {
                continue;
            }
            let r = raw.entry(e.kind).or_default();
            let n = (e.text.len() + e.tool.len() + e.args.len()) as u64;
            r.0 += n;
            let h = *blake3::hash(format!("{}\0{}\0{}", e.tool, e.args, e.text).as_bytes()).as_bytes();
            if seen.insert((e.agent, h)) {
                r.1 += n;
                rows.entry((e.kind, e.agent)).or_default().push(row(e));
            }
        }
    }
    let tmp = std::env::var("TMPDIR").unwrap_or("/tmp".into());
    let p = std::path::PathBuf::from(tmp).join(format!("kind-probe-{}.run", std::process::id()));
    let starter = oojuro_store::starter::v1().clone();
    let mut out: HashMap<u8, (u64, u64)> = HashMap::new();
    let mut keys: Vec<_> = rows.keys().copied().collect();
    keys.sort();
    for k in keys {
        for (j, opts) in [
            RunOptions { threads: 2, ..RunOptions::default() },
            RunOptions { level: 3, dict_bytes: 0, shared_dict: Some(starter.clone()), threads: 2, ..RunOptions::default() },
        ]
        .into_iter()
        .enumerate()
        {
            let _ = std::fs::remove_file(&p);
            let info = write_run(&p, &mut rows[&k].iter().cloned(), &opts).unwrap();
            let o = out.entry(k.0).or_default();
            if j == 0 { o.0 += info.file_bytes } else { o.1 += info.file_bytes }
        }
    }
    let _ = std::fs::remove_file(&p);
    println!("kind          raw MB  after record dedupe  z19+own/raw  z3+starter/raw");
    for k in 0..6u8 {
        let Some(&(r, u)) = raw.get(&k) else { continue };
        let o = out.get(&k).copied().unwrap_or_default();
        println!("{:<12} {:>8.1} {:>12.1} MB {:>12.4} {:>14.4}", KIND_NAMES[k as usize], r as f64 / 1e6, u as f64 / 1e6, o.0 as f64 / r as f64, o.1 as f64 / r as f64);
    }
}
