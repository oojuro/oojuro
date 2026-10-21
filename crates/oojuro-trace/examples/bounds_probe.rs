//! Section D bounds on this machine (timed: run under the bench lock).
//! 1. memory bandwidth (1 thread copy) — the ceiling for any decode
//! 2. zstd decode speed of 64 KiB frames (levels 3 / 19, with / without a
//!    dictionary) built from tier-1 text — the read path's codec
//! 3. search: one agent's events in a v4 store; its turn-loop queries
//!    (first 200 bytes of each tool result / user message, as of that turn)
//!    vs postings visited, index bits, and a brute-force scan of all its
//!    text (what search costs with no index at all)
//! Usage: bounds_probe <base.tbe> <scratch dir> <agent>
use memmap2::Mmap;
use oojuro_core::Record;
use oojuro_nine::{Nine, Options, Receipts};
use std::{fs::File, time::Instant};

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1]
}

struct Ev<'a> {
    id: u64,
    agent: u32,
    kind: u8,
    revises: u64,
    ts: i64,
    text: &'a str,
}

fn events(m: &[u8]) -> Vec<Vec<Ev<'_>>> {
    let mut out = Vec::new();
    let mut p = 8;
    while p < m.len() {
        let n = u32::from_le_bytes(m[p..p + 4].try_into().unwrap());
        p += 4;
        let mut b = Vec::new();
        for _ in 0..n {
            let g8 = |o: usize| u64::from_le_bytes(m[p + o..p + o + 8].try_into().unwrap());
            let tl = u16::from_le_bytes(m[p + 74..p + 76].try_into().unwrap()) as usize;
            let al = u32::from_le_bytes(m[p + 76..p + 80].try_into().unwrap()) as usize;
            let xl = u32::from_le_bytes(m[p + 80..p + 84].try_into().unwrap()) as usize;
            let s = p + 84 + tl + al;
            b.push(Ev {
                id: g8(0),
                agent: u32::from_le_bytes(m[p + 16..p + 20].try_into().unwrap()),
                kind: m[p + 44],
                revises: g8(36),
                ts: g8(58) as i64,
                text: std::str::from_utf8(&m[s..s + xl]).unwrap(),
            });
            p = s + xl;
        }
        out.push(b);
    }
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let map = unsafe { Mmap::map(&File::open(&a[1]).unwrap()).unwrap() };
    let scratch = std::path::PathBuf::from(&a[2]);
    let agent: u32 = a[3].parse().unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    // 1. memory bandwidth
    let src = vec![7u8; 256 << 20];
    let mut dst = vec![0u8; 256 << 20];
    let t = Instant::now();
    for _ in 0..4 {
        dst.copy_from_slice(&src);
        std::hint::black_box(&dst);
    }
    let bw = 4.0 * (256u64 << 20) as f64 / t.elapsed().as_secs_f64() / 1e9;
    println!("{{\"probe\":\"memcpy_1thread\",\"gb_per_s\":{bw:.2}}}");
    drop((src, dst));
    // 2. zstd frame decode
    let batches = events(&map);
    let mut text = Vec::new();
    'fill: for b in &batches {
        for e in b {
            text.extend_from_slice(e.text.as_bytes());
            if text.len() >= 64 << 20 {
                break 'fill;
            }
        }
    }
    let frames: Vec<&[u8]> = text.chunks(64 << 10).collect();
    let samples: Vec<&[u8]> = frames.iter().step_by(4).copied().collect();
    let sizes: Vec<usize> = samples.iter().map(|s| s.len()).collect();
    let dict = zstd::dict::from_continuous(&samples.concat(), &sizes, 512 << 10).unwrap();
    for level in [3, 19] {
        for use_dict in [false, true] {
            let cd = zstd::dict::EncoderDictionary::copy(&dict, level);
            let dd = zstd::dict::DecoderDictionary::copy(&dict);
            let comp: Vec<Vec<u8>> = frames
                .iter()
                .map(|f| {
                    if use_dict {
                        let mut c = zstd::bulk::Compressor::with_prepared_dictionary(&cd).unwrap();
                        c.compress(f).unwrap()
                    } else {
                        zstd::bulk::compress(f, level).unwrap()
                    }
                })
                .collect();
            let csize: usize = comp.iter().map(|c| c.len()).sum();
            let mut dec = if use_dict {
                zstd::bulk::Decompressor::with_prepared_dictionary(&dd).unwrap()
            } else {
                zstd::bulk::Decompressor::new().unwrap()
            };
            let t = Instant::now();
            let mut out = 0usize;
            for c in &comp {
                out += dec.decompress(c, 64 << 10).unwrap().len();
            }
            let s = t.elapsed().as_secs_f64();
            println!(
                "{{\"probe\":\"zstd_frame_decode\",\"level\":{level},\"dict\":{use_dict},\"ratio\":{:.4},\"mb_per_s\":{:.1},\"us_per_64k_frame\":{:.1},\"of_memcpy\":{:.4}}}",
                csize as f64 / out as f64,
                out as f64 / s / 1e6,
                s * 1e6 / comp.len() as f64,
                out as f64 / s / 1e9 / bw
            );
        }
    }
    drop(text);
    // 3. search on one agent's store
    let dir = scratch.join("agent");
    let opts = Options { receipts: Receipts::Off, query_threads: 1, ..Options::default() };
    let nine = Nine::open_with(&dir, opts).unwrap();
    let mut queries: Vec<(String, i64)> = Vec::new();
    let mut raw = 0usize;
    let mut all_text: Vec<&str> = Vec::new();
    let t = Instant::now();
    for b in &batches {
        if b[0].agent != agent {
            continue;
        }
        if matches!(b[0].kind, 1 | 5) && !b[0].text.is_empty() {
            let mut e = b[0].text.len().min(200);
            while !b[0].text.is_char_boundary(e) {
                e -= 1;
            }
            queries.push((b[0].text[..e].to_string(), b[0].ts - 1));
        }
        let recs: Vec<(Record, bool)> = b
            .iter()
            .map(|e| {
                raw += e.text.len();
                all_text.push(e.text);
                let id = e.id.to_string();
                let key = if e.revises != 0 { format!("r{}", e.revises) } else { id.clone() };
                (Record::new(&id, &key, e.text, e.ts), matches!(e.kind, 1 | 3 | 4 | 5) && e.revises == 0 && !e.text.is_empty())
            })
            .collect();
        nine.insert_batch_indexed(recs).unwrap();
    }
    nine.flush().unwrap();
    let build = t.elapsed().as_secs_f64();
    let stats = nine.stats().unwrap();
    let idx_bytes: u64 = nine
        .files()
        .unwrap()
        .iter()
        .filter(|p| p.extension().is_some_and(|x| x == "idx"))
        .map(|p| std::fs::metadata(p).unwrap().len())
        .sum();
    let bits_per_posting = idx_bytes as f64 * 8.0 / stats.indexed_postings.max(1) as f64;
    println!(
        "{{\"probe\":\"agent_store\",\"agent\":{agent},\"records\":{},\"raw_bytes\":{raw},\"index_bytes\":{idx_bytes},\"postings\":{},\"vocabulary\":{},\"index_bits_per_posting\":{bits_per_posting:.2},\"build_s\":{build:.1}}}",
        stats.records, stats.indexed_postings, stats.vocabulary
    );
    // replay the last 2,000 queries (as of their turn), timed
    let qs = &queries[queries.len().saturating_sub(2000)..];
    let (mut lat, mut visited, mut cands) = (Vec::new(), 0usize, 0usize);
    for (q, at) in qs {
        let t = Instant::now();
        let _ = nine.find(q, 0, *at, 20).unwrap();
        lat.push(t.elapsed().as_secs_f64() * 1e3);
        let m = nine.metrics();
        visited += m.posted_rows_visited;
        cands += m.candidate_records;
    }
    let n = qs.len() as f64;
    let mean_ms = lat.iter().sum::<f64>() / n;
    let post_bits = visited as f64 / n * bits_per_posting;
    println!(
        "{{\"probe\":\"search\",\"queries\":{},\"p50_ms\":{:.3},\"p99_ms\":{:.3},\"mean_ms\":{mean_ms:.3},\"postings_visited_mean\":{:.0},\"candidates_mean\":{:.0},\"postings_bits_mean\":{post_bits:.0},\"ns_per_posting\":{:.1},\"postings_bytes_at_memcpy_us\":{:.2}}}",
        qs.len(),
        pct(&mut lat.clone(), 0.5),
        pct(&mut lat, 0.99),
        visited as f64 / n,
        cands as f64 / n,
        mean_ms * 1e6 / (visited as f64 / n).max(1.0),
        post_bits / 8.0 / (bw * 1e9) * 1e6
    );
    // no index: scan all of the agent's text for the query's words (lower bound
    // of brute force: one memchr-class pass over the raw bytes per query)
    let joined: Vec<u8> = all_text.concat().into_bytes();
    let t = Instant::now();
    let mut hits = 0usize;
    for (q, _) in qs.iter().take(20) {
        let w = q.split_whitespace().find(|w| w.len() > 4).unwrap_or("error");
        hits += joined.windows(w.len()).filter(|x| *x == w.as_bytes()).count();
    }
    let scan_ms = t.elapsed().as_secs_f64() * 1e3 / 20.0;
    println!(
        "{{\"probe\":\"brute_force_scan\",\"raw_bytes\":{},\"ms_per_query_one_word\":{scan_ms:.1},\"hits\":{hits},\"memcpy_floor_ms\":{:.2}}}",
        joined.len(),
        joined.len() as f64 / (bw * 1e9) * 1e3
    );
    drop(nine);
    let _ = std::fs::remove_dir_all(&scratch);
}
