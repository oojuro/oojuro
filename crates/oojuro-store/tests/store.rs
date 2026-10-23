//! Gate tests: byte-exact roundtrip, crash recovery (torn writes inside
//! compressed frames, header/manifest corruption, interrupted compaction)
//! and determinism.

use oojuro_store::util::Rng;
use oojuro_store::{Error, NewRecord, Options, Store};
use std::path::{Path, PathBuf};

fn temp(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let p = base.join(format!("oojuro-store-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

type Rec = (u8, i64, Vec<u8>, Vec<u8>);

/// Deterministic agent-like records: repeated tool output, JSON calls, prose.
fn records(n: usize, seed: u64) -> Vec<Rec> {
    let mut rng = Rng::new(seed);
    let words = ["error", "pytest", "def", "return", "json", "{\"name\":", "tool", "result", "file.py", "\n", "line", "ok"];
    (0..n)
        .map(|i| {
            let len = [0usize, 5, 80, 600, 4000][rng.below(5)];
            let mut t = Vec::new();
            while t.len() < len {
                t.extend_from_slice(words[rng.below(words.len())].as_bytes());
                t.push(b' ');
            }
            if rng.below(7) == 0 {
                t.extend_from_slice("unicode ✓ ü 日本".as_bytes());
            }
            let key = if i % 3 == 0 { format!("k{}", i / 3).into_bytes() } else { Vec::new() };
            ((i % 6) as u8, 1_700_000_000 + i as i64, key, t)
        })
        .collect()
}

fn opts() -> Options {
    Options { dict_train_after: 32 << 10, dict_bytes: 4 << 10, sync: false, cold_frame_bytes: 8 << 10, ..Options::default() }
}

fn append_all(s: &mut Store, recs: &[Rec], batch: usize) {
    for chunk in recs.chunks(batch) {
        let b: Vec<NewRecord> = chunk
            .iter()
            .map(|(k, t, key, text)| NewRecord { kind: *k, valid_at: *t, recorded_at: *t + 1, key, attrs: b"", text })
            .collect();
        s.append(&b).unwrap();
    }
}

fn check_all(s: &Store, recs: &[Rec]) {
    assert_eq!(s.len(), recs.len() as u64);
    for (i, (k, t, key, text)) in recs.iter().enumerate() {
        let r = s.record(i as u64).unwrap();
        assert_eq!(blake3::hash(&r.text), blake3::hash(text), "record {i}");
        assert_eq!((r.kind, r.valid_at, r.recorded_at, &r.key), (*k, *t, *t + 1, key));
        assert_eq!(&s.get(i as u64).unwrap(), text);
        let (a, b) = (text.len() / 3, text.len() / 3 + 50);
        assert_eq!(s.get_range(i as u64, a, b).unwrap(), text[a.min(text.len())..b.min(text.len())]);
    }
    assert!(matches!(s.get(recs.len() as u64), Err(Error::NotFound)));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        std::fs::copy(&p, to.join(p.file_name().unwrap())).unwrap();
    }
}

#[test]
fn roundtrip_reopen_compact() {
    let dir = temp("roundtrip");
    let recs = records(3000, 1);
    {
        let mut s = Store::open(&dir, opts()).unwrap();
        append_all(&mut s, &recs[..1500], 7);
        check_all(&s, &recs[..1500]);
        let st = s.compact().unwrap();
        assert_eq!(st.records, 1500);
        append_all(&mut s, &recs[1500..], 5);
        check_all(&s, &recs);
    }
    let mut s = Store::open(&dir, opts()).unwrap();
    check_all(&s, &recs);
    s.compact().unwrap();
    check_all(&s, &recs);
    drop(s);
    let s = Store::open(&dir, opts()).unwrap();
    check_all(&s, &recs);
    let mut n = 0;
    s.scan_time(1_700_000_100, 1_700_000_200, |r| {
        assert_eq!(r.text, recs[r.seq as usize].3);
        n += 1;
        true
    })
    .unwrap();
    assert_eq!(n, 100);
    // Both segments share one reused dictionary; the hot-tier one was collected.
    assert_eq!(s.stats().unwrap().dicts, 1);
    let dict_files = std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "ojd")).count();
    assert_eq!(dict_files, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn second_writer_is_refused() {
    let dir = temp("lock");
    let _s = Store::open(&dir, opts()).unwrap();
    assert!(matches!(Store::open(&dir, opts()), Err(Error::Invalid(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

fn hot_logs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ojl"))
        .collect();
    v.sort();
    v
}

#[test]
fn torn_tail_inside_compressed_frame_recovers_every_cut() {
    // Optional disjoint test sharding preserves exhaustive coverage under bounded gates.
    let shard = std::env::var("OOJURO_TORN_SHARD").unwrap_or_else(|_| "0/1".into());
    let (part, count) = shard.split_once('/').expect("shard must be index/count");
    let (part, count) = (part.parse::<usize>().unwrap(), count.parse::<usize>().unwrap());
    assert!(count > 0 && count <= 256 && part < count);
    let mut tested = 0;
    let dir = temp("torn-src");
    let recs = records(400, 2);
    let mut s = Store::open(&dir, opts()).unwrap();
    append_all(&mut s, &recs[..390], 13);
    let log = hot_logs(&dir).pop().unwrap();
    let before = std::fs::metadata(&log).unwrap().len() as usize;
    append_all(&mut s, &recs[390..], 10); // final batch: one compressed frame
    drop(s);
    let full = std::fs::read(&log).unwrap();
    assert!(full.len() > before + 16);
    // Every cut inside the final frame (header, zstd body, last byte).
    for cut in before..full.len() {
        if (cut - before) % count != part { continue; }
        tested += 1;
        let d = temp("torn-case");
        copy_dir(&dir, &d);
        std::fs::write(d.join(log.file_name().unwrap()), &full[..cut]).unwrap();
        let s = Store::open(&d, opts()).unwrap();
        assert_eq!(s.len(), 390, "cut at {cut} of {}", full.len());
        check_all(&s, &recs[..390]);
        drop(s);
        // Recovery is idempotent and appends continue after the truncated tail.
        let mut s = Store::open(&d, opts()).unwrap();
        append_all(&mut s, &recs[390..], 10);
        check_all(&s, &recs);
        drop(s);
        let _ = std::fs::remove_dir_all(&d);
    }
    let s = Store::open(&dir, opts()).unwrap();
    check_all(&s, &recs);
    assert!(tested > 0);
    eprintln!("torn shard {part}/{count}: {tested} disjoint cases of {} total", full.len() - before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zero_and_garbage_tails_are_truncated() {
    for (name, tail) in [("zeros", vec![0u8; 4096]), ("garbage", (0..999u32).map(|i| (i * 7 + 3) as u8).collect())] {
        let dir = temp(name);
        let recs = records(200, 3);
        {
            let mut s = Store::open(&dir, opts()).unwrap();
            append_all(&mut s, &recs, 9);
        }
        let log = hot_logs(&dir).pop().unwrap();
        let mut b = std::fs::read(&log).unwrap();
        b.extend_from_slice(&tail);
        std::fs::write(&log, &b).unwrap();
        let s = Store::open(&dir, opts()).unwrap();
        check_all(&s, &recs);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn corruption_before_the_tail_is_an_error() {
    let dir = temp("midcorrupt");
    let recs = records(300, 4);
    {
        let mut s = Store::open(&dir, opts()).unwrap();
        append_all(&mut s, &recs, 6);
    }
    let log = hot_logs(&dir).pop().unwrap();
    let mut b = std::fs::read(&log).unwrap();
    let i = b.len() / 3;
    b[i] ^= 0x20;
    std::fs::write(&log, &b).unwrap();
    assert!(matches!(Store::open(&dir, opts()), Err(Error::Corrupt(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn header_and_manifest_corruption_are_errors() {
    let recs = records(300, 5);
    let build = |name: &str| {
        let dir = temp(name);
        let mut s = Store::open(&dir, opts()).unwrap();
        append_all(&mut s, &recs[..200], 6);
        s.compact().unwrap();
        append_all(&mut s, &recs[200..], 6);
        drop(s);
        dir
    };
    let flip = |p: &Path, at: usize| {
        let mut b = std::fs::read(p).unwrap();
        b[at] ^= 1;
        std::fs::write(p, b).unwrap();
    };
    let find = |dir: &Path, ext: &str| -> PathBuf {
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == ext)).unwrap()
    };
    let dir = build("hdr-hot");
    flip(&hot_logs(&dir).pop().unwrap(), 9);
    assert!(matches!(Store::open(&dir, opts()), Err(Error::Corrupt(_))));
    let dir2 = build("hdr-man");
    flip(&dir2.join("MANIFEST"), 10);
    assert!(matches!(Store::open(&dir2, opts()), Err(Error::Corrupt(_))));
    let dir3 = build("hdr-cold");
    flip(&find(&dir3, "ojc"), 3);
    assert!(matches!(Store::open(&dir3, opts()), Err(Error::Corrupt(_))));
    let dir4 = build("idx-cold");
    let c = find(&dir4, "ojc");
    let len = std::fs::metadata(&c).unwrap().len() as usize;
    flip(&c, len - 20);
    assert!(matches!(Store::open(&dir4, opts()), Err(Error::Corrupt(_))));
    // A flipped byte inside a compressed cold frame is caught on read.
    let dir5 = build("frame-cold");
    flip(&find(&dir5, "ojc"), 200);
    let s = Store::open(&dir5, opts()).unwrap();
    let bad = (0..200u64).filter(|&i| matches!(s.get(i), Err(Error::Corrupt(_)))).count();
    assert!(bad > 0);
    drop(s);
    let dir6 = build("dict");
    flip(&find(&dir6, "ojd"), 30);
    assert!(matches!(Store::open(&dir6, opts()), Err(Error::Corrupt(_))));
    for d in [dir, dir2, dir3, dir4, dir5, dir6] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn interrupted_compaction_leaves_a_readable_store() {
    let dir = temp("crash-compact");
    let recs = records(500, 6);
    {
        let mut s = Store::open(&dir, opts()).unwrap();
        append_all(&mut s, &recs, 8);
    }
    // A crash after writing a segment and dictionary but before MANIFEST was
    // replaced leaves orphans with unpublished ids and maybe a torn temp file.
    std::fs::write(dir.join("cold-00000099.ojc"), b"partial segment").unwrap();
    std::fs::write(dir.join("dict-00000099.ojd"), b"partial dict").unwrap();
    std::fs::write(dir.join("MANIFEST.tmp"), b"torn manifest").unwrap();
    {
        let mut s = Store::open(&dir, opts()).unwrap();
        check_all(&s, &recs);
        assert!(!dir.join("cold-00000099.ojc").exists());
        // A crash after publishing but before old hot logs were deleted.
        let before: Vec<PathBuf> = hot_logs(&dir);
        let saved: Vec<Vec<u8>> = before.iter().map(|p| std::fs::read(p).unwrap()).collect();
        s.compact().unwrap();
        drop(s);
        for (p, b) in before.iter().zip(saved) {
            std::fs::write(p, b).unwrap();
        }
    }
    let s = Store::open(&dir, opts()).unwrap();
    check_all(&s, &recs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn identical_input_gives_identical_bytes() {
    let recs = records(2000, 7);
    let mut images = Vec::new();
    for n in 0..2 {
        let dir = temp(&format!("det{n}"));
        // Thread count must not change a single byte.
        let o = Options { compact_threads: [1, 4][n], ..opts() };
        let mut s = Store::open(&dir, o).unwrap();
        append_all(&mut s, &recs[..1200], 11);
        s.compact().unwrap();
        append_all(&mut s, &recs[1200..], 11);
        drop(s);
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
            .collect();
        files.sort();
        images.push(files);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(images[0], images[1]);
}

#[test]
fn invalid_batches_are_rejected() {
    let dir = temp("invalid");
    let mut s = Store::open(&dir, opts()).unwrap();
    assert!(matches!(s.append(&[]), Err(Error::Invalid(_))));
    let r = NewRecord { kind: 200, valid_at: 0, recorded_at: 0, key: b"", attrs: b"", text: b"x" };
    assert!(matches!(s.append(&[r]), Err(Error::Invalid(_))));
    assert_eq!(s.len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Records that repeat large file-like bodies, as agents re-reading files do.
fn repetitive(n: usize, seed: u64) -> Vec<Rec> {
    let mut rng = Rng::new(seed);
    let files: Vec<Vec<u8>> = (0..5)
        .map(|f| (0..6000).map(|i| b"abcdefghijklmnopqrstuvwxyz \n{}"[(i * 7 + f * 13 + i / 50) % 30]).collect())
        .collect();
    (0..n)
        .map(|i| {
            let mut t = format!("$ cat file{}.py\n", i % 5).into_bytes();
            t.extend_from_slice(&files[rng.below(5)]);
            t.extend_from_slice(format!("\nexit {}\n", rng.below(3)).as_bytes());
            (4u8, 1_700_000_000 + i as i64, Vec::new(), t)
        })
        .collect()
}

#[test]
fn dedupe_references_roundtrip_across_segments() {
    let dir = temp("dedupe");
    let recs = repetitive(120, 8);
    let o = Options { dedupe_chunk: 512, ..opts() };
    let mut s = Store::open(&dir, o.clone()).unwrap();
    append_all(&mut s, &recs[..60], 3);
    let a = s.compact().unwrap();
    append_all(&mut s, &recs[60..], 3);
    let b = s.compact().unwrap();
    assert!(a.ref_bytes > 0 && b.ref_bytes > 0, "{a:?} {b:?}");
    // The second segment must reference the first (cross-segment refs).
    assert!(b.ref_bytes as f64 > 0.5 * b.raw_bytes as f64);
    check_all(&s, &recs);
    drop(s);
    let s = Store::open(&dir, o.clone()).unwrap();
    check_all(&s, &recs);
    drop(s);
    // Corrupting the first segment's frames breaks referring reads loudly.
    let seg = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "ojc")).min().unwrap();
    let mut bytes = std::fs::read(&seg).unwrap();
    for i in (64..bytes.len() - 64).step_by(97) {
        bytes[i] ^= 0x55;
    }
    std::fs::write(&seg, bytes).unwrap();
    let s = Store::open(&dir, o).unwrap_or_else(|_| panic!("index is intact"));
    let errors = (60..120u64).filter(|&i| matches!(s.get(i), Err(Error::Corrupt(_)))).count();
    assert!(errors > 0);
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn failed_write_poisons_until_reopen_and_loses_only_that_batch() {
    let dir = temp("poison");
    let recs = records(60, 9);
    let mut s = Store::open(&dir, opts()).unwrap();
    append_all(&mut s, &recs[..50], 5);
    s.fail_next_append_after = Some(7);
    let b: Vec<NewRecord> = recs[50..55].iter().map(|(k, t, key, text)| NewRecord { kind: *k, valid_at: *t, recorded_at: *t + 1, key, attrs: b"", text }).collect();
    assert!(matches!(s.append(&b), Err(Error::Io(_))));
    assert!(matches!(s.append(&b), Err(Error::Invalid(_))), "poisoned store must refuse writes");
    assert!(matches!(s.compact(), Err(Error::Invalid(_))));
    check_all(&s, &recs[..50]);
    drop(s);
    let mut s = Store::open(&dir, opts()).unwrap();
    check_all(&s, &recs[..50]);
    append_all(&mut s, &recs[50..], 5);
    check_all(&s, &recs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn background_compaction_while_appending() {
    let dir = temp("background");
    let recs = records(3000, 10);
    // Small hot logs so appends rotate files while the job runs.
    let o = Options { dedupe_chunk: 512, hot_file_bytes: 64 << 10, ..opts() };
    let mut s = Store::open(&dir, o.clone()).unwrap();
    append_all(&mut s, &recs[..1500], 7);
    let job = s.begin_compaction().unwrap().unwrap();
    assert!(matches!(s.begin_compaction(), Err(Error::Invalid(_))), "one job at a time");
    let worker = std::thread::spawn(move || job.run());
    append_all(&mut s, &recs[1500..2500], 5);
    check_all(&s, &recs[..2500]);
    let prepared = worker.join().unwrap().unwrap();
    let st = s.finish_compaction(prepared).unwrap();
    assert_eq!(st.records, 1500);
    check_all(&s, &recs[..2500]);
    append_all(&mut s, &recs[2500..], 5);
    s.compact().unwrap();
    check_all(&s, &recs);
    drop(s);
    let s = Store::open(&dir, o.clone()).unwrap();
    check_all(&s, &recs);
    drop(s);
    // A job that is dropped (never run) or fails leaves the store usable.
    let mut s = Store::open(&dir, o.clone()).unwrap();
    append_all(&mut s, &recs[..10], 5);
    let job = s.begin_compaction().unwrap().unwrap();
    drop(job);
    s.abort_compaction();
    s.compact().unwrap();
    assert_eq!(s.len(), 3010);
    let _ = std::fs::remove_dir_all(&dir);
}
