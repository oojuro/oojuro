//! Train a starter dictionary from the archive rows (engine-encoded records,
//! exactly what flush runs compress) of trace stores built from PUBLIC,
//! permissively licensed data only. Prints the sources it read.
//!
//! Usage: train_starter <out.zdict> <dict_kib> <store_dir>...
use oojuro_store::run::RunReader;
use std::path::{Path, PathBuf};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "src") {
                out.push(path);
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let kib: usize = a[1].parse().unwrap();
    let mut runs = Vec::new();
    for d in &a[2..] {
        walk(Path::new(d), &mut runs);
    }
    runs.sort();
    let mut rows: Vec<Vec<u8>> = Vec::new();
    let mut bytes = 0usize;
    for r in &runs {
        let run = RunReader::open(r, 4 << 20).expect("run (built without a shared dictionary)");
        run.scan(0, run.len(), &mut |_, b| {
            bytes += b.len();
            rows.push(b.to_vec());
            Ok(())
        })
        .unwrap();
    }
    let dict = oojuro_store::store::train_dict(&rows, kib << 10).expect("dictionary");
    std::fs::write(&a[0], &dict).unwrap();
    println!(
        "{{\"runs\": {}, \"rows\": {}, \"row_bytes\": {bytes}, \"dict_bytes\": {}, \"blake3\": \"{}\"}}",
        runs.len(),
        rows.len(),
        dict.len(),
        blake3::hash(&dict).to_hex()
    );
}
