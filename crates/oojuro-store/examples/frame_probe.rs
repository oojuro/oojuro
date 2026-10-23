//! Where a single-row read of a run file spends its time (docs/PERF_ROUND2.md, item 4).
//!
//! Usage: frame_probe RUN.src [--rows 2000] [--seed 1]
//! Build: cargo build --release -p oojuro-store --features bench-tools --example frame_probe
//!
//! Opens the run without a frame cache and times, for random rows: a ranged
//! read of the first byte (reads the frame, checks its CRC, decodes the meta
//! and the body only up to that byte), a ranged read of the whole row, and the
//! whole-row read the engine uses (`get`: the complete frame, whose digest is
//! then checked). Also times `blake3::hash` of a frame-sized buffer alone.
use oojuro_store::run::RunReader;
use std::time::Instant;

fn main() {
    let v: Vec<String> = std::env::args().skip(1).collect();
    let path = v.first().expect("RUN.src");
    let rows: usize = v.iter().position(|x| x == "--rows").map_or(2000, |i| v[i + 1].parse().unwrap());
    let mut s: u64 = v.iter().position(|x| x == "--seed").map_or(1, |i| v[i + 1].parse().unwrap());
    let starter = oojuro_store::starter::v1();
    let r = RunReader::open_with(std::path::Path::new(path), 0, Some(starter)).expect("open run");
    let n = r.len();
    let mut next = move || {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let ids: Vec<u64> = (0..rows).map(|_| next() % n).collect();
    let time = |f: &dyn Fn(u64)| -> f64 {
        let t = Instant::now();
        for &i in &ids {
            f(i);
        }
        t.elapsed().as_secs_f64() * 1e6 / ids.len() as f64
    };
    let head = time(&|i| {
        r.get_range(i, 0, 1).unwrap();
    });
    let range = time(&|i| {
        r.get_range(i, 0, usize::MAX).unwrap();
    });
    let get = time(&|i| {
        r.get(i).unwrap();
    });
    let scan = time(&|i| {
        r.scan(i, i + 1, &mut |_, b| {
            std::hint::black_box(b.len());
            Ok(())
        })
        .unwrap();
    });
    let buf = vec![7u8; 64 << 10];
    let t = Instant::now();
    for _ in 0..2000 {
        std::hint::black_box(blake3::hash(std::hint::black_box(&buf)));
    }
    let b3 = t.elapsed().as_secs_f64() * 1e6 / 2000.0;
    println!(
        "{}",
        serde_json::json!({"run": path, "rows_in_run": n, "raw_bytes": r.raw_bytes(), "file_bytes": r.file_bytes(),
            "us_per_read": {"first_byte": head, "whole_row_range": range, "get_point": get, "scan_whole_frame": scan}, "blake3_64KiB_us": b3,
            "loadavg": std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim()})
    );
}
