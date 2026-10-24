use super::*;
use crate::run::{RunOptions, RunReader, write_run};
use crate::{NewRecord, Options, Store};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

const ROW_BYTES: usize = 128 << 10;
const BUDGET: usize = 16 << 10;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let name = format!(
            "oojuro-prefix-capacity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn rows() -> Vec<Vec<u8>> {
    (0..32)
        .map(|i| {
            let mut bytes = format!("row-{i}: \0 unicode 日本 ✓\n").into_bytes();
            bytes.resize(ROW_BYTES, b'a' + i % 3);
            bytes
        })
        .collect()
}
fn options() -> RunOptions {
    RunOptions {
        level: 3,
        frame_bytes: 64 << 10,
        dict_bytes: 0,
        dedupe_chunk: 0,
        threads: 1,
        ..RunOptions::default()
    }
}
fn cache(fs: &impl Frames) -> (usize, usize, usize) {
    let state = fs.read_state().ctx.lock().unwrap();
    (
        state.cache.len(),
        state.cache.values().map(|d| d.body.capacity()).sum(),
        state.cached_bytes,
    )
}
fn check_prefix_cache(fs: &impl Frames) {
    let (entries, body_capacity, charged) = cache(fs);
    // All useful prefixes fit. The old path retains 32 whole 128KiB bodies
    // despite charging only their short lengths against this 16KiB budget.
    assert_eq!(entries, 32);
    assert!(body_capacity < ROW_BYTES && body_capacity <= BUDGET);
    assert!(charged <= BUDGET);
    let state = fs.read_state().ctx.lock().unwrap();
    assert!(state.cache.values().all(|d| d.body.capacity() < 4096));
}

#[test]
fn sealed_store_prefix_cache_bounds_retained_capacity_and_preserves_full_records() {
    let directory = Directory::new();
    let rows = rows();
    let settings = Options {
        dict_bytes: 0,
        dict_train_after: usize::MAX,
        cache_bytes: BUDGET,
        cipher: Some(Arc::new(crate::crypto::FrameCipher::new(&[71; 32]))),
        require_sealed: true,
        sync: false,
        ..Options::default()
    };
    let mut store = Store::open(&directory.0.join("store"), settings.clone()).unwrap();
    for (i, text) in rows.iter().enumerate() {
        store
            .append(&[NewRecord {
                kind: 7,
                valid_at: i as i64,
                recorded_at: i as i64 + 1,
                key: b"key",
                attrs: b"attrs",
                text,
            }])
            .unwrap();
    }
    for (i, text) in rows.iter().enumerate() {
        assert_eq!(store.get_range(i as u64, 0, 64).unwrap(), text[..64]);
    }
    check_prefix_cache(&store);
    let first = store.read_state().ctx.lock().unwrap().cache[&0].clone();
    assert_eq!(store.get_range(0, 0, 64).unwrap(), rows[0][..64]);
    assert!(Arc::ptr_eq(
        &first,
        &store.read_state().ctx.lock().unwrap().cache[&0]
    ));
    drop(first);
    for (i, text) in rows.iter().enumerate() {
        let record = store.record(i as u64).unwrap();
        assert_eq!(record.text, *text);
        assert_eq!(record.seq, i as u64);
        assert_eq!(
            (record.kind, record.valid_at, record.recorded_at),
            (7, i as i64, i as i64 + 1)
        );
        assert_eq!(record.key, b"key");
        assert_eq!(record.attrs, b"attrs");
    }
    assert!(cache(&store).1 <= BUDGET && cache(&store).2 <= BUDGET);
    drop(store);
    let reopened = Store::open(&directory.0.join("store"), settings).unwrap();
    for (i, text) in rows.iter().enumerate() {
        assert_eq!(reopened.get(i as u64).unwrap(), *text);
    }
}

#[test]
fn run_prefix_expansion_evicts_under_budget_without_changing_originals() {
    let directory = Directory::new();
    let rows = rows();
    let path = directory.0.join("rows.run");
    write_run(&path, &mut rows.clone().into_iter(), &options()).unwrap();
    let run = RunReader::open(&path, BUDGET).unwrap();
    assert_eq!(
        get_head(&read_payload(run.file(0), &run.loc(0)).unwrap())
            .unwrap()
            .codec,
        CODEC_ZSTD
    );
    for (i, text) in rows.iter().enumerate() {
        assert_eq!(run.get_range(i as u64, 0, 64).unwrap(), text[..64]);
    }
    check_prefix_cache(&run);
    for (i, text) in rows.iter().enumerate() {
        assert_eq!(run.get_range(i as u64, 1, 2048).unwrap(), text[1..2048]);
    }
    let (entries, capacity, charged) = cache(&run);
    assert!(entries > 0 && entries < 32 && capacity <= BUDGET && charged <= BUDGET);
    for (i, text) in rows.iter().enumerate() {
        assert_eq!(run.get(i as u64).unwrap(), *text);
    }
    run.scan(0, 32, &mut |i, text| {
        assert_eq!(text, rows[i as usize].as_slice());
        Ok(())
    })
    .unwrap();
    assert!(cache(&run).1 <= BUDGET && cache(&run).2 <= BUDGET);
}

#[test]
fn metadata_allocations_participate_in_real_cache_admission() {
    let directory = Directory::new();
    let path = directory.0.join("many-small.run");
    let rows = vec![b"x".to_vec(); 1024];
    let opts = RunOptions {
        frame_bytes: 1 << 20,
        ..options()
    };
    write_run(&path, &mut rows.into_iter(), &opts).unwrap();
    let run = RunReader::open(&path, 32 << 10).unwrap();
    assert_eq!(run.loc(0).count, 1024);
    assert_eq!(run.get_range(0, 0, 0).unwrap(), b"");
    // Just these two real metadata allocations already exceed the budget,
    // before body/text offsets/pieces/struct overhead; the old charge admits it.
    let decoded = decoded(&run, 0, 0).unwrap();
    let lower_bound = decoded.meta.records.capacity() * std::mem::size_of::<RecordMeta>()
        + decoded.meta.side.capacity() * std::mem::size_of::<(u32, u32, u32)>();
    assert!(lower_bound > 32 << 10);
    assert_eq!(cache(&run).0, 0);
}

#[test]
fn partial_read_checks_crc_of_unselected_compressed_tail() {
    let directory = Directory::new();
    let good = directory.0.join("good.run");
    write_run(&good, &mut rows().into_iter(), &options()).unwrap();
    let run = RunReader::open(&good, BUDGET).unwrap();
    let first = run.loc(0);
    let head = get_head(&read_payload(run.file(0), &first).unwrap()).unwrap();
    assert_eq!(head.codec, CODEC_ZSTD);
    let mut bytes = std::fs::read(&good).unwrap();
    bytes[first.offset as usize + 8 + first.payload_len as usize - 1] ^= 1;
    let bad = directory.0.join("bad.run");
    std::fs::write(&bad, bytes).unwrap();
    let corrupted = RunReader::open(&bad, BUDGET).unwrap();
    assert!(matches!(corrupted.get_range(0, 0, 1), Err(Error::Corrupt(_))));
    assert_eq!(cache(&corrupted).0, 0);
}
