//! Exact-duplicate potential of a TraceBench stream, per agent store
//! (dedupe never crosses agents): share of events and raw bytes whose
//! (tool, args, text) or text alone already occurred earlier in the same
//! agent, by kind and by minimum text size.
//!
//! Usage: dup_probe <events.tbe>
use oojuro_tracebench::events::{Base, KIND_NAMES, Tier};
use std::collections::{HashMap, HashSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("events file");
    let base = Base::open(path.as_ref())?;
    let tier = Tier::new(&base, 1);
    let mut seen_full: HashMap<u32, HashSet<[u8; 16]>> = HashMap::new();
    let mut seen_text: HashMap<u32, HashSet<[u8; 16]>> = HashMap::new();
    // per kind: events, raw, dup_full events, dup_full raw, dup_text events, dup_text raw
    let mut k = [[0u64; 6]; 6];
    // dup_full by minimum text+args size
    let mins = [0usize, 32, 128, 512, 4096];
    let mut by_min = vec![[0u64; 2]; mins.len()];
    let mut b = Vec::new();
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        for e in &b {
            if e.is_revision() {
                continue;
            }
            let raw = e.raw_bytes();
            let mut h = blake3::Hasher::new();
            h.update(&(e.tool.len() as u64).to_le_bytes());
            h.update(e.tool.as_bytes());
            h.update(&(e.args.len() as u64).to_le_bytes());
            h.update(e.args.as_bytes());
            h.update(e.text.as_bytes());
            let full: [u8; 16] = h.finalize().as_bytes()[..16].try_into().unwrap();
            let text: [u8; 16] = blake3::hash(e.text.as_bytes()).as_bytes()[..16].try_into().unwrap();
            let kk = &mut k[e.kind as usize];
            kk[0] += 1;
            kk[1] += raw;
            let df = !seen_full.entry(e.agent).or_default().insert(full);
            let dt = !e.text.is_empty() && !seen_text.entry(e.agent).or_default().insert(text);
            if df {
                kk[2] += 1;
                kk[3] += raw;
                for (m, v) in mins.iter().zip(by_min.iter_mut()) {
                    if e.text.len() + e.args.len() >= *m {
                        v[0] += 1;
                        v[1] += raw;
                    }
                }
            }
            if dt {
                kk[4] += 1;
                kk[5] += e.text.len() as u64;
            }
        }
    }
    let (te, tr): (u64, u64) = (k.iter().map(|x| x[0]).sum(), k.iter().map(|x| x[1]).sum());
    println!("events {te} raw {tr}");
    println!("kind          events      raw MB   dup(tool,args,text) ev%  raw%   dup(text) ev%  text%");
    for (i, kk) in k.iter().enumerate() {
        if kk[0] == 0 {
            continue;
        }
        println!(
            "{:<12} {:>8} {:>10.1}   {:>6.1} {:>6.1}   {:>6.1} {:>6.1}",
            KIND_NAMES[i],
            kk[0],
            kk[1] as f64 / 1e6,
            100.0 * kk[2] as f64 / kk[0] as f64,
            100.0 * kk[3] as f64 / kk[1].max(1) as f64,
            100.0 * kk[4] as f64 / kk[0] as f64,
            100.0 * kk[5] as f64 / kk[1].max(1) as f64
        );
    }
    let (de, dr): (u64, u64) = (k.iter().map(|x| x[2]).sum(), k.iter().map(|x| x[3]).sum());
    println!("all          dup events {:.2}%  dup raw {:.2}%", 100.0 * de as f64 / te as f64, 100.0 * dr as f64 / tr as f64);
    for (m, v) in mins.iter().zip(by_min.iter()) {
        println!("  min {m:>5} B: dup events {:.2}%  dup raw {:.2}%", 100.0 * v[0] as f64 / te as f64, 100.0 * v[1] as f64 / tr as f64);
    }
    Ok(())
}
