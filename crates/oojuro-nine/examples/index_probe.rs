//! Where the bytes of v4 index segments go, and what other postings
//! encodings would cost on the same lists (sizes only; nothing is written).
//!
//! Usage: index_probe <dir>...   (every `seg-*.idx` below the directories)
//!
//! Encodings, each list padded to whole bytes:
//! - actual: the bytes in the files (version 1 LEB128 or version 2 Rice)
//! - leb:  LEB128 (row gap, tf) pairs (segment version 1)
//! - for:  blocks of 128: one byte of widths, then (gap - 1) and (tf - 1)
//!         bit-packed at the block's widths; a short last block likewise
//! - ef:   Elias-Fano rows (universe = segment rows) + Elias-gamma tf
//! - ef+w: Elias-Fano rows + (tf - 1) at one bit width per list
//! - bound: log2 C(U, n) per list + empirical entropy of tf over all postings
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
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
fn bits(v: u64) -> u64 {
    64 - v.leading_zeros() as u64
}
fn log2_binom(u: u64, n: u64) -> f64 {
    // ln C(u, n) via lgamma-free Stirling sums on the smaller side
    let n = n.min(u - n);
    let mut s = 0f64;
    for i in 0..n {
        s += ((u - i) as f64).log2() - ((i + 1) as f64).log2();
    }
    s
}

#[derive(Default, Debug)]
struct Tot {
    segs: u64,
    rows: u64,
    file: u64,
    sec: [u64; 8],
    postings: u64,
    lists: u64,
    actual: u64,
    leb: u64,
    forb: u64,
    ef: u64,
    efw: u64,
    bound: f64,
    tf_hist: HashMap<u64, u64>,
    df1: u64,
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "idx") {
                out.push(path);
            }
        }
    }
}

fn probe(map: &[u8], t: &mut Tot) {
    let version = u32_at(map, 8);
    let rows = u32_at(map, 20) as u64;
    let mut sec = [0usize; 9];
    for (i, s) in sec.iter_mut().enumerate() {
        *s = u64_at(map, 32 + 8 * i) as usize;
    }
    t.segs += 1;
    t.rows += rows;
    t.file += map.len() as u64;
    for i in 0..8 {
        t.sec[i] += (sec[i + 1] - sec[i]) as u64;
    }
    let (s_post, s_dict, s_dindex, s_end) = (sec[5], sec[6], sec[7], sec[8]);
    let blocks = (s_end - s_dindex) / 8;
    let u = rows.max(1);
    for b in 0..blocks {
        let start = s_dict + u64_at(map, s_dindex + 8 * b) as usize;
        let end = if b + 1 < blocks { s_dict + u64_at(map, s_dindex + 8 * (b + 1)) as usize } else { s_dindex };
        let mut p = start;
        let mut off = s_post + varint(map, &mut p) as usize;
        while p < end {
            let _shared = varint(map, &mut p);
            let n = varint(map, &mut p) as usize;
            p += n;
            let df = varint(map, &mut p);
            let _cf = varint(map, &mut p);
            let len = varint(map, &mut p) as usize;
            let list = &map[off..off + len];
            off += len;
            let (mut row, mut first) = (0u64, true);
            let mut rs = Vec::with_capacity(df as usize);
            let mut tfs = Vec::with_capacity(df as usize);
            if version == 1 {
                let mut q = 0;
                while q < list.len() {
                    let gap = varint(list, &mut q);
                    let tf = varint(list, &mut q);
                    row = if first { gap } else { row + gap };
                    first = false;
                    rs.push(row);
                    tfs.push(tf);
                }
            } else {
                // Rice gaps (k = floor(log2(rows / df))) + gamma tf, LSB first
                let qd = u / df.max(1);
                let k = if qd <= 1 { 0 } else { 63 - qd.leading_zeros() as u64 };
                let mut pos = 0usize;
                let bit = |pos: &mut usize| -> u64 {
                    let b = (list[*pos / 8] >> (*pos % 8)) & 1;
                    *pos += 1;
                    u64::from(b)
                };
                for _ in 0..df {
                    let mut q = 0u64;
                    while bit(&mut pos) == 0 {
                        q += 1;
                    }
                    let mut r = 0u64;
                    for i in 0..k {
                        r |= bit(&mut pos) << i;
                    }
                    let gap = (q << k) | r;
                    let mut z = 0u64;
                    while bit(&mut pos) == 0 {
                        z += 1;
                    }
                    let mut tf = 1u64 << z;
                    for i in 0..z {
                        tf |= bit(&mut pos) << i;
                    }
                    row = if first { gap } else { row + gap + 1 };
                    first = false;
                    rs.push(row);
                    tfs.push(tf);
                }
            }
            assert_eq!(rs.len() as u64, df);
            t.lists += 1;
            t.postings += df;
            t.df1 += (df == 1) as u64;
            t.actual += len as u64;
            let mut leb = Vec::new();
            let mut prev = 0u64;
            for (i, (&r, &tf)) in rs.iter().zip(&tfs).enumerate() {
                for mut v in [if i == 0 { r } else { r - prev }, tf] {
                    while v >= 0x80 {
                        leb.push(0);
                        v >>= 7;
                    }
                    leb.push(0);
                }
                prev = r;
            }
            t.leb += leb.len() as u64;
            for &x in &tfs {
                *t.tf_hist.entry(x.min(64)).or_default() += 1;
            }
            // FOR blocks of 128
            let mut fb = 0u64;
            let mut prev: Option<u64> = None;
            let gaps: Vec<u64> = rs
                .iter()
                .map(|&r| {
                    let g = match prev {
                        None => r,
                        Some(p) => r - p - 1,
                    };
                    prev = Some(r);
                    g
                })
                .collect();
            for (cg, ct) in gaps.chunks(128).zip(tfs.chunks(128)) {
                let wg = cg.iter().map(|&g| bits(g)).max().unwrap();
                let wt = ct.iter().map(|&x| bits(x - 1)).max().unwrap();
                fb += 8 + cg.len() as u64 * (wg + wt);
            }
            t.forb += fb.div_ceil(8);
            // Elias-Fano rows
            let n = df;
            let l = if u > n { bits(u / n) - 1 } else { 0 };
            let ef_rows = n * l + n + (u >> l) + 1;
            let gamma: u64 = tfs.iter().map(|&x| 2 * (bits(x) - 1) + 1).sum();
            let wt = tfs.iter().map(|&x| bits(x - 1)).max().unwrap();
            t.ef += (ef_rows + gamma).div_ceil(8);
            t.efw += (ef_rows + 8 + n * wt).div_ceil(8);
            t.bound += log2_binom(u, n.min(u));
        }
    }
}

fn main() {
    let mut files = Vec::new();
    for d in std::env::args().skip(1) {
        walk(Path::new(&d), &mut files);
    }
    let mut t = Tot::default();
    for f in &files {
        let map = std::fs::read(f).unwrap();
        probe(&map, &mut t);
    }
    let total_tf: u64 = t.tf_hist.values().sum();
    let h_tf: f64 = t
        .tf_hist
        .values()
        .map(|&c| {
            let p = c as f64 / total_tf as f64;
            -p * p.log2() * c as f64 / p
        })
        .sum::<f64>()
        / 1.0;
    let names = ["rows", "ids", "keys", "time", "multi", "postings", "dict", "dindex"];
    println!("segments {} rows {} file bytes {}", t.segs, t.rows, t.file);
    for (n, b) in names.iter().zip(t.sec.iter()) {
        println!("  {n:<9} {b:>12} ({:.1}%, {:.1} B/row)", 100.0 * *b as f64 / t.file as f64, *b as f64 / t.rows as f64);
    }
    println!("lists {} (df=1: {}), postings {}", t.lists, t.df1, t.postings);
    let p = t.postings as f64;
    for (n, b) in [("actual", t.actual as f64), ("leb", t.leb as f64), ("for128", t.forb as f64), ("ef+gamma", t.ef as f64), ("ef+width", t.efw as f64)] {
        println!("  {n:<9} {:>12.0} B  {:.2} bits/posting", b, 8.0 * b / p);
    }
    println!("  bound     {:>12.0} B  {:.2} bits/posting (rows {:.2} + tf {:.2})", (t.bound + h_tf) / 8.0, (t.bound + h_tf) / p, t.bound / p, h_tf / p);
    let mut h: Vec<_> = t.tf_hist.iter().collect();
    h.sort();
    println!("  tf: {:?}", h.iter().take(6).map(|(k, v)| (**k, format!("{:.1}%", 100.0 * **v as f64 / total_tf as f64))).collect::<Vec<_>>());
}
