//! Crypto-shredding storage cost (outside-the-obvious idea, track/tracedb).
//!
//! If every session's text is encrypted under its own key, deleting a
//! session = destroying one key: instant, provable erasure with no rewrite.
//! The price: ciphertext of different sessions cannot share a frame, a
//! dedupe chunk or a content-trained dictionary (each would hold another
//! session's text). This measures that price in bytes.
//!
//! Sessions are split in two halves by hash. Half H1 plays a public corpus
//! (a dictionary trained on it holds no H2 text); every variant is measured
//! on H2 only:
//! - shared-session: one run (zstd 19, 64 KiB frames, 1 KiB dedupe, 512 KiB
//!   dictionary trained on H2 itself), sessions contiguous (today's archive)
//! - shared-append: the same in append order (agents interleaved)
//! - iso-frames: per-session 64 KiB frames, zstd 19, no dictionary
//! - iso-frames+pubdict: per-session 64 KiB frames, dictionary trained on H1
//! - iso-session+pubdict: one frame per session, dictionary trained on H1
//! Isolated variants add 28 bytes per frame (96-bit nonce + 128-bit tag).
//! Usage: shred_exp <base.tbe> <scratch dir>
use memmap2::Mmap;
use oojuro_store::run::{RunOptions, write_run};
use std::{collections::BTreeMap, fs::File, time::Instant};

const HEAD: usize = 82;

struct Ev<'a> {
    session: u64,
    seq: u32,
    blob: [&'a [u8]; 3],
}

fn events(m: &[u8]) -> Vec<Ev<'_>> {
    let mut out = Vec::new();
    let mut p = 8;
    while p < m.len() {
        let n = u32::from_le_bytes(m[p..p + 4].try_into().unwrap());
        p += 4;
        for _ in 0..n {
            let session = u64::from_le_bytes(m[p + 8..p + 16].try_into().unwrap());
            let seq = u32::from_le_bytes(m[p + 24..p + 28].try_into().unwrap());
            let tl = u16::from_le_bytes(m[p + 74..p + 76].try_into().unwrap()) as usize;
            let al = u32::from_le_bytes(m[p + 76..p + 80].try_into().unwrap()) as usize;
            let xl = u32::from_le_bytes(m[p + 80..p + 84].try_into().unwrap()) as usize;
            let s = p + HEAD + 2;
            out.push(Ev { session, seq, blob: [&m[s..s + tl], &m[s + tl..s + tl + al], &m[s + tl + al..s + tl + al + xl]] });
            p = s + tl + al + xl;
        }
    }
    out
}

fn half(session: u64) -> bool {
    blake3::hash(&session.to_le_bytes()).as_bytes()[0] & 1 == 1
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let f = File::open(&a[1]).unwrap();
    let map = unsafe { Mmap::map(&f).unwrap() };
    let scratch = std::path::PathBuf::from(&a[2]);
    std::fs::create_dir_all(&scratch).unwrap();
    let evs = events(&map);
    let row = |e: &Ev| -> Vec<u8> { e.blob.concat() };
    let mut by_session: BTreeMap<u64, Vec<&Ev>> = BTreeMap::new();
    let (mut h1, mut h2_append) = (Vec::new(), Vec::new());
    for e in &evs {
        if half(e.session) {
            by_session.entry(e.session).or_default().push(e);
            h2_append.push(row(e));
        } else if h1.len() < 400_000 {
            h1.push(row(e));
        }
    }
    for v in by_session.values_mut() {
        v.sort_by_key(|e| e.seq);
    }
    let raw: usize = h2_append.iter().map(|r| r.len()).sum();
    let sessions = by_session.len();
    eprintln!("H2: {sessions} sessions, {} events, {raw} bytes", h2_append.len());
    let report = |name: &str, bytes: usize, frames: usize, t: Instant| {
        println!(
            "{{\"variant\":\"{name}\",\"raw\":{raw},\"bytes\":{bytes},\"frames\":{frames},\"bytes_per_raw\":{:.5},\"seconds\":{:.1},\"sessions\":{sessions}}}",
            bytes as f64 / raw as f64,
            t.elapsed().as_secs_f64()
        )
    };
    // shared domains (today's archive run format)
    let opts = RunOptions { level: 19, threads: 4, ..RunOptions::default() };
    for (name, rows) in [
        ("shared-session", by_session.values().flatten().map(|e| row(e)).collect::<Vec<_>>()),
        ("shared-append", h2_append.clone()),
    ] {
        let t = Instant::now();
        let p = scratch.join(name);
        let _ = std::fs::remove_file(&p);
        let info = write_run(&p, &mut rows.into_iter(), &opts).unwrap();
        report(name, info.file_bytes as usize, 0, t);
        let _ = std::fs::remove_file(&p);
    }
    // a "public" dictionary trained on H1 only
    let t = Instant::now();
    let dict = oojuro_store::store::train_dict(&h1, 512 << 10).expect("dict");
    eprintln!("public dict {} bytes in {:.1}s", dict.len(), t.elapsed().as_secs_f64());
    let cdict = zstd::dict::EncoderDictionary::copy(&dict, 19);
    // isolated: frames never cross sessions
    for (name, frame_cap, use_dict) in [
        ("iso-frames", 64usize << 10, false),
        ("iso-frames+pubdict", 64 << 10, true),
        ("iso-session+pubdict", usize::MAX, true),
    ] {
        let t = Instant::now();
        let (mut bytes, mut frames) = (0usize, 0usize);
        for evs in by_session.values() {
            let mut buf: Vec<u8> = Vec::new();
            let mut flush = |buf: &mut Vec<u8>| {
                if buf.is_empty() {
                    return;
                }
                let c = if use_dict {
                    let mut enc = zstd::stream::Encoder::with_prepared_dictionary(Vec::new(), &cdict).unwrap();
                    std::io::Write::write_all(&mut enc, buf).unwrap();
                    enc.finish().unwrap()
                } else {
                    zstd::bulk::compress(buf, 19).unwrap()
                };
                bytes += c.len() + 28;
                frames += 1;
                buf.clear();
            };
            for e in evs {
                let r = row(e);
                if !buf.is_empty() && buf.len() + r.len() > frame_cap {
                    flush(&mut buf);
                }
                buf.extend_from_slice(&r);
            }
            flush(&mut buf);
        }
        report(name, bytes, frames, t);
    }
}
