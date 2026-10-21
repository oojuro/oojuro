//! "Index the compressed data": per-frame term sketches vs postings.
//!
//! One agent's indexed events (user, output, tool_call, tool_result) in
//! append order, packed into ~64 KiB frames (the store's random-access
//! unit). Every frame gets a Bloom filter of its distinct words. A query
//! (first 200 bytes of an observation, as in the turn loop) keeps its rarest
//! words (by document frequency); the candidate frames are those whose
//! filter holds any of them, and only those frames would be decompressed and
//! scored exactly. Reports, against an inverted index:
//! - sketch bits vs the information-theoretic size of a docid postings index
//!   (sum over words of df * (2 + log2(N/df)) bits, Elias-Fano-like) and vs
//!   the same with frame ids instead of docids
//! - candidate frames per query (what a sketch search must decode)
//! Usage: sketch_exp <base.tbe> <agent> [bits_per_term] [rare_terms]
use memmap2::Mmap;
use std::collections::{HashMap, HashSet};
use std::fs::File;

fn words(t: &str) -> impl Iterator<Item = String> + '_ {
    t.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 2).map(|w| w.to_lowercase())
}

fn h(w: &str, seed: u64) -> u64 {
    let x = blake3::hash(w.as_bytes());
    u64::from_le_bytes(x.as_bytes()[..8].try_into().unwrap()).wrapping_mul(seed | 1).rotate_left(seed as u32 % 63)
}

struct Bloom {
    bits: Vec<u64>,
    m: u64,
    k: u64,
}
impl Bloom {
    fn new(n: usize, bits_per: f64) -> Self {
        let m = ((n as f64 * bits_per).ceil() as u64).max(64);
        let k = ((bits_per * std::f64::consts::LN_2).round() as u64).max(1);
        Self { bits: vec![0; m.div_ceil(64) as usize], m, k }
    }
    fn add(&mut self, w: &str) {
        let (a, b) = (h(w, 1), h(w, 2));
        for i in 0..self.k {
            let x = a.wrapping_add(i.wrapping_mul(b)) % self.m;
            self.bits[(x / 64) as usize] |= 1 << (x % 64);
        }
    }
    fn has(&self, w: &str) -> bool {
        let (a, b) = (h(w, 1), h(w, 2));
        (0..self.k).all(|i| {
            let x = a.wrapping_add(i.wrapping_mul(b)) % self.m;
            self.bits[(x / 64) as usize] >> (x % 64) & 1 == 1
        })
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let map = unsafe { Mmap::map(&File::open(&a[1]).unwrap()).unwrap() };
    let agent: u32 = a[2].parse().unwrap();
    let bits_per: f64 = a.get(3).map_or(10.0, |s| s.parse().unwrap());
    let rare: usize = a.get(4).map_or(3, |s| s.parse().unwrap());
    // (text, ts, is_query_source) for the agent's indexed events in append order
    let mut docs: Vec<(String, i64)> = Vec::new();
    let mut queries: Vec<(String, i64, usize)> = Vec::new(); // (query, as_of, docs so far)
    let m = &map[..];
    let mut p = 8;
    while p < m.len() {
        let n = u32::from_le_bytes(m[p..p + 4].try_into().unwrap());
        p += 4;
        for k in 0..n {
            let ag = u32::from_le_bytes(m[p + 16..p + 20].try_into().unwrap());
            let rev = u64::from_le_bytes(m[p + 36..p + 44].try_into().unwrap());
            let kind = m[p + 44];
            let ts = i64::from_le_bytes(m[p + 58..p + 66].try_into().unwrap());
            let tl = u16::from_le_bytes(m[p + 74..p + 76].try_into().unwrap()) as usize;
            let al = u32::from_le_bytes(m[p + 76..p + 80].try_into().unwrap()) as usize;
            let xl = u32::from_le_bytes(m[p + 80..p + 84].try_into().unwrap()) as usize;
            let s = p + 84 + tl + al;
            let text = std::str::from_utf8(&m[s..s + xl]).unwrap();
            if ag == agent && rev == 0 && matches!(kind, 1 | 3 | 4 | 5) && !text.is_empty() {
                if k == 0 && matches!(kind, 1 | 5) {
                    let mut e = text.len().min(200);
                    while !text.is_char_boundary(e) {
                        e -= 1;
                    }
                    queries.push((text[..e].to_string(), ts - 1, docs.len()));
                }
                docs.push((text.to_string(), ts));
            }
            p = s + xl;
        }
    }
    // frames of ~64 KiB
    let mut frames: Vec<(usize, usize)> = Vec::new(); // doc range
    let (mut start, mut bytes) = (0, 0);
    for (i, (t, _)) in docs.iter().enumerate() {
        if bytes > 0 && bytes + t.len() > 64 << 10 {
            frames.push((start, i));
            start = i;
            bytes = 0;
        }
        bytes += t.len();
    }
    frames.push((start, docs.len()));
    // document and frame frequencies, sketches
    let mut df: HashMap<String, u64> = HashMap::new();
    let mut ff: HashMap<String, u64> = HashMap::new();
    let mut blooms = Vec::new();
    let mut sketch_bits = 0u64;
    for &(s, e) in &frames {
        let mut fw: HashSet<String> = HashSet::new();
        for (t, _) in &docs[s..e] {
            let dw: HashSet<String> = words(t).collect();
            for w in &dw {
                *df.entry(w.clone()).or_default() += 1;
            }
            fw.extend(dw);
        }
        let mut b = Bloom::new(fw.len(), bits_per);
        for w in &fw {
            b.add(w);
            *ff.entry(w.clone()).or_default() += 1;
        }
        sketch_bits += b.m;
        blooms.push(b);
    }
    let n = docs.len() as f64;
    let nf = frames.len() as f64;
    let ef = |d: u64, total: f64| d as f64 * (2.0 + (total / d as f64).log2().max(0.0));
    let postings_bits: f64 = df.values().map(|&d| ef(d, n)).sum();
    let frame_postings_bits: f64 = ff.values().map(|&d| ef(d, nf)).sum();
    let postings: u64 = df.values().sum();
    // queries: rarest `rare` words (by df) -> candidate frames (those up to as_of)
    let mut cand_total = 0usize;
    let mut true_total = 0usize;
    let mut considered = 0usize;
    let qs = &queries[queries.len().saturating_sub(2000)..];
    for (q, _, upto) in qs {
        let mut qw: Vec<String> = words(q).collect::<HashSet<_>>().into_iter().collect();
        qw.sort_by_key(|w| (df.get(w).copied().unwrap_or(0), w.clone()));
        let sel: Vec<&String> = qw.iter().filter(|w| df.contains_key(*w)).take(rare).collect();
        if sel.is_empty() {
            continue;
        }
        let last_frame = frames.partition_point(|f| f.0 < *upto);
        considered += last_frame;
        for (fi, b) in blooms.iter().enumerate().take(last_frame) {
            if sel.iter().any(|w| b.has(w)) {
                cand_total += 1;
                let (s, e) = frames[fi];
                if docs[s..e.min(*upto)].iter().any(|(t, _)| {
                    let dw: HashSet<String> = words(t).collect();
                    sel.iter().any(|w| dw.contains(*w))
                }) {
                    true_total += 1;
                }
            }
        }
    }
    let nq = qs.len() as f64;
    println!(
        "{{\"agent\":{agent},\"docs\":{},\"frames\":{},\"raw_bytes\":{},\"vocabulary\":{},\"postings\":{postings},\
\"postings_bound_bytes\":{:.0},\"frame_postings_bound_bytes\":{:.0},\"sketch_bytes\":{},\"bits_per_term\":{bits_per},\
\"rare_terms\":{rare},\"queries\":{},\"candidate_frames_mean\":{:.1},\"true_frames_mean\":{:.1},\"frames_visible_mean\":{:.1}}}",
        docs.len(),
        frames.len(),
        docs.iter().map(|d| d.0.len()).sum::<usize>(),
        df.len(),
        postings_bits / 8.0,
        frame_postings_bits / 8.0,
        sketch_bits / 8,
        qs.len(),
        cand_total as f64 / nq,
        true_total as f64 / nq,
        considered as f64 / nq
    );
}
