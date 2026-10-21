//! Gate tests for immutable run files: exact rows and ranges, scans, dedupe
//! inside a run, determinism, and corruption / truncation detection.

use oojuro_store::run::{RunOptions, RunReader, write_run};
use oojuro_store::util::Rng;
use oojuro_store::Error;
use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let p = base.join(format!("oojuro-run-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Rows like encoded agent records: repeated file bodies, JSON, short rows, empties.
fn rows(n: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng::new(seed);
    let file: Vec<u8> = (0..5000).map(|i| b"fn main() { let x = 1; }\n"[i % 25]).collect();
    (0..n)
        .map(|i| match rng.below(5) {
            0 => Vec::new(),
            1 => format!("{{\"name\":\"tool_{}\",\"arguments\":{{\"path\":\"src/{}.rs\"}}}}", i % 7, rng.below(50)).into_bytes(),
            2 => [format!("$ cat src/{}.rs\n", i % 9).into_bytes(), file.clone()].concat(),
            3 => (0..rng.below(3000)).map(|_| b'a' + rng.below(26) as u8).collect(),
            _ => "ünïcode ✓ 日本 \0 tab\t end".as_bytes().to_vec(),
        })
        .collect()
}

fn opts() -> RunOptions {
    RunOptions { frame_bytes: 8 << 10, dict_bytes: 16 << 10, dict_sample: 64 << 10, dedupe_chunk: 512, threads: 3, ..RunOptions::default() }
}

#[test]
fn rows_ranges_and_scans_are_exact() {
    let path = temp("exact");
    let src = rows(2000, 1);
    let info = write_run(&path, &mut src.clone().into_iter(), &opts()).unwrap();
    assert_eq!(info.rows, 2000);
    assert_eq!(info.raw_bytes, src.iter().map(|r| r.len() as u64).sum::<u64>());
    assert!(info.file_bytes * 3 < info.raw_bytes, "repetitive rows should compress: {info:?}");
    let run = RunReader::open(&path, 1 << 20).unwrap();
    assert_eq!(run.len(), 2000);
    for (i, r) in src.iter().enumerate() {
        assert_eq!(blake3::hash(&run.get(i as u64).unwrap()), blake3::hash(r), "row {i}");
        let (a, b) = (r.len() / 2, r.len() / 2 + 100);
        assert_eq!(run.get_range(i as u64, a, b).unwrap(), r[a.min(r.len())..b.min(r.len())]);
    }
    assert!(matches!(run.get(2000), Err(Error::NotFound)));
    let mut seen = Vec::new();
    run.scan(100, 1900, &mut |i, b| {
        assert_eq!(b, src[i as usize].as_slice());
        seen.push(i);
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, (100..1900).collect::<Vec<_>>());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_and_tiny_runs() {
    for (name, src) in [("empty", Vec::<Vec<u8>>::new()), ("one", vec![b"x".to_vec()]), ("blank", vec![Vec::new(); 3])] {
        let path = temp(name);
        let o = RunOptions { dict_bytes: 0, ..opts() };
        write_run(&path, &mut src.clone().into_iter(), &o).unwrap();
        let run = RunReader::open(&path, 0).unwrap();
        assert_eq!(run.len(), src.len() as u64);
        for (i, r) in src.iter().enumerate() {
            assert_eq!(&run.get(i as u64).unwrap(), r);
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn identical_input_gives_identical_files() {
    let src = rows(1500, 2);
    let (a, b) = (temp("det-a"), temp("det-b"));
    write_run(&a, &mut src.clone().into_iter(), &opts()).unwrap();
    write_run(&b, &mut src.into_iter(), &RunOptions { threads: 1, ..opts() }).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

#[test]
fn corruption_and_truncation_are_detected() {
    let src = rows(800, 3);
    let path = temp("corrupt-src");
    write_run(&path, &mut src.clone().into_iter(), &opts()).unwrap();
    let good = std::fs::read(&path).unwrap();
    // Truncated anywhere: open fails (trailer, index or frame coverage).
    for cut in [0, 7, good.len() / 2, good.len() - 1] {
        let p = temp("cut");
        std::fs::write(&p, &good[..cut]).unwrap();
        assert!(matches!(RunReader::open(&p, 0), Err(Error::Corrupt(_)) | Err(Error::Io(_))), "cut {cut}");
        let _ = std::fs::remove_file(&p);
    }
    // Trailer / index damage: open fails.
    let p = temp("trailer");
    let mut b = good.clone();
    let n = b.len();
    b[n - 20] ^= 1;
    std::fs::write(&p, &b).unwrap();
    assert!(matches!(RunReader::open(&p, 0), Err(Error::Corrupt(_))));
    // Damage inside compressed frames: the affected reads fail, never return wrong bytes.
    let mut b = good.clone();
    for i in (40..b.len() / 2).step_by(211) {
        b[i] ^= 0x5a;
    }
    std::fs::write(&p, &b).unwrap();
    let run = RunReader::open(&p, 0).unwrap();
    let mut failures = 0;
    for (i, r) in src.iter().enumerate() {
        match run.get(i as u64) {
            Ok(got) => assert_eq!(&got, r, "row {i} returned wrong bytes"),
            Err(Error::Corrupt(_)) => failures += 1,
            Err(e) => panic!("unexpected error {e}"),
        }
    }
    assert!(failures > 0);
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn shared_dictionary_runs_need_that_dictionary() {
    use oojuro_store::run::SharedDict;
    let path = temp("shared");
    let src = rows(1500, 7);
    let train: Vec<Vec<u8>> = rows(4000, 99);
    let d = oojuro_store::store::train_dict(&train, 16 << 10).expect("dictionary");
    let shared = SharedDict::new(&d);
    let o = RunOptions { dict_bytes: 0, level: 3, shared_dict: Some(shared.clone()), ..opts() };
    let info = write_run(&path, &mut src.clone().into_iter(), &o).unwrap();
    // the dictionary is not stored in the run
    let plain = temp("shared-plain");
    let p = write_run(&plain, &mut src.clone().into_iter(), &RunOptions { dict_bytes: 0, level: 3, ..opts() }).unwrap();
    assert!(info.file_bytes < p.file_bytes, "{info:?} vs {p:?}");
    let run = RunReader::open_with(&path, 1 << 20, Some(&shared)).unwrap();
    for (i, r) in src.iter().enumerate() {
        assert_eq!(blake3::hash(&run.get(i as u64).unwrap()), blake3::hash(r), "row {i}");
    }
    assert!(matches!(RunReader::open(&path, 1 << 20), Err(Error::Corrupt(_))));
    let other = SharedDict::new(&oojuro_store::store::train_dict(&rows(4000, 5), 16 << 10).unwrap());
    assert!(matches!(RunReader::open_with(&path, 1 << 20, Some(&other)), Err(Error::Corrupt(_))));
    // a run that trains its own dictionary ignores the shared one; when
    // training is not possible (too little data) it falls back to it
    let own = temp("shared-own");
    write_run(&own, &mut src.clone().into_iter(), &RunOptions { shared_dict: Some(shared.clone()), dict_sample: 8 << 20, ..opts() }).unwrap();
    let r2 = RunReader::open(&own, 1 << 20).unwrap();
    assert_eq!(r2.get(3).unwrap(), src[3]);
    let _ = std::fs::remove_file(&own);
    write_run(&own, &mut src[..20].iter().cloned(), &RunOptions { shared_dict: Some(shared.clone()), ..opts() }).unwrap();
    assert!(matches!(RunReader::open(&own, 1 << 20), Err(Error::Corrupt(_))));
    assert_eq!(RunReader::open_with(&own, 1 << 20, Some(&shared)).unwrap().get(3).unwrap(), src[3]);
    for p in [path, plain, own] {
        let _ = std::fs::remove_file(&p);
    }
}
