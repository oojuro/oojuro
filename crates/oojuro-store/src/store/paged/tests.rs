use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
const KEY: [u8; 32] = [171; 32];
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "oojuro-paged-store-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn source(&self) -> PathBuf {
        self.root.join("source")
    }
    fn cache(&self) -> PathBuf {
        self.root.join("metadata")
    }
    fn opts(&self) -> Options {
        Options {
            hot_level: 1,
            cold_level: 1,
            cold_frame_bytes: 8 << 10,
            dict_bytes: 16 << 10,
            dict_train_after: 64 << 10,
            hot_file_bytes: 8 << 10,
            cache_bytes: 64 << 10,
            sync: true,
            dedupe_chunk: 1024,
            compact_threads: 1,
            cipher: Some(Arc::new(FrameCipher::new(&KEY))),
            require_sealed: true,
            ..Options::default()
        }
    }
    fn writer(&self) -> Store {
        Store::open(&self.source(), self.opts()).unwrap()
    }
    fn reader(&self) -> PagedStore {
        PagedStore::open(&self.source(), self.opts(), &self.cache()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn text(i: u64) -> Vec<u8> {
    format!("synthetic paging original {i} ")
        .repeat(80)
        .into_bytes()
}
fn append(store: &mut Store, start: u64, end: u64) {
    for i in start..end {
        assert_eq!(
            store
                .append(&[NewRecord {
                    kind: 3,
                    valid_at: 1000 + i as i64,
                    recorded_at: 9000 + i as i64,
                    key: b"synthetic-key",
                    attrs: b"{synthetic attrs}",
                    text: &text(i)
                }])
                .unwrap(),
            i
        );
    }
}
fn check(reader: &PagedStore, start: u64, end: u64) {
    for i in start..end {
        let r = reader.record(i).unwrap();
        assert_eq!(
            (r.seq, r.kind, r.valid_at, r.recorded_at),
            (i, 3, 1000 + i as i64, 9000 + i as i64)
        );
        assert_eq!(r.key, b"synthetic-key");
        assert_eq!(r.attrs, b"{synthetic attrs}");
        assert_eq!(r.text, text(i));
        assert_eq!(reader.get_range(i, 7, 29).unwrap(), text(i)[7..29]);
    }
}
#[test]
fn paged_store_real_hot_cold_dedupe_original_fields_and_origin_match_without_population_metadata() {
    let f = Fixture::new();
    let mut source = f.writer();
    append(&mut source, 0, 160);
    source.compact().unwrap();
    append(&mut source, 160, 210);
    let origin = source.sealed_origin_digest().unwrap();
    let raw = source.raw_text;
    drop(source);
    let reader = f.reader();
    assert_eq!(reader.len(), 210);
    assert_eq!(reader.raw_text_bytes(), raw);
    assert_eq!(reader.sealed_origin_digest().unwrap(), origin);
    check(&reader, 0, 210);
    let mut selected = Vec::new();
    reader
        .scan_time(1090, 1097, |r| {
            selected.push(r.seq);
            true
        })
        .unwrap();
    assert_eq!(selected, (90..97).collect::<Vec<_>>());
    let usage = reader.cache_usage().unwrap();
    assert!(
        usage.open_files <= 32 && usage.dictionaries <= 16 && usage.dictionary_bytes <= 16 << 20
    );
    assert_eq!(usage.metadata_pages, 3);
    assert!(matches!(reader.get(210), Err(Error::NotFound)));
    assert!(Store::open(&f.source(), f.opts()).is_err());
    drop(reader);
    let reader = f.reader();
    check(&reader, 0, 210);
}
#[test]
fn missing_corrupt_stale_derived_tables_rebuild_only_from_verified_source_and_root_changes_fail_closed()
 {
    let f = Fixture::new();
    let mut source = f.writer();
    append(&mut source, 0, 12);
    drop(source);
    let reader = f.reader();
    let old_root = std::fs::read(f.cache().join(ROOT)).unwrap();
    let table = std::fs::read_dir(f.cache())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().ends_with("-1.bin"))
        .unwrap();
    let mut bytes = std::fs::read(&table).unwrap();
    bytes[71] ^= 1;
    std::fs::write(table, bytes).unwrap();
    assert!(reader.get(0).is_err());
    drop(reader);
    let reader = f.reader();
    check(&reader, 0, 12);
    drop(reader);
    std::fs::write(f.cache().join(ROOT), b"broken derived root").unwrap();
    let reader = f.reader();
    check(&reader, 0, 12);
    std::fs::write(f.cache().join(ROOT), old_root).unwrap();
    assert!(reader.get(0).is_err());
    drop(reader);
    let mut source = f.writer();
    append(&mut source, 12, 15);
    drop(source);
    let reader = f.reader();
    check(&reader, 0, 15);
}
#[test]
fn valid_old_cache_never_bypasses_corrupt_source_aead_or_wrong_key_or_missing_authority() {
    let f = Fixture::new();
    let mut source = f.writer();
    append(&mut source, 0, 5);
    drop(source);
    drop(f.reader());
    let path = std::fs::read_dir(f.source())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("hot-"))
        .unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let len = u32::from_le_bytes(bytes[FILE_HEAD..FILE_HEAD + 4].try_into().unwrap()) as usize;
    let end = FILE_HEAD + 8 + len;
    bytes[end - 1] ^= 1;
    let crc = frame_crc(&bytes[FILE_HEAD + 8..end]);
    bytes[FILE_HEAD + 4..FILE_HEAD + 8].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    assert!(PagedStore::open(&f.source(), f.opts(), &f.cache()).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes.len() as u64);
    let mut opts = f.opts();
    opts.cipher = Some(Arc::new(FrameCipher::new(&[172; 32])));
    assert!(PagedStore::open(&f.source(), opts, &f.cache()).is_err());
    std::fs::remove_file(f.source().join("MANIFEST")).unwrap();
    assert!(PagedStore::open(&f.source(), f.opts(), &f.cache()).is_err());
    assert!(!f.source().join("MANIFEST").exists());
}
#[test]
fn paged_store_repairs_only_last_torn_tail_refuses_misuse_and_keeps_metadata_encrypted() {
    let f = Fixture::new();
    let mut source = f.writer();
    append(&mut source, 0, 20);
    drop(source);
    let mut hot = std::fs::read_dir(f.source())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with("hot-"))
        .collect::<Vec<_>>();
    hot.sort_by_key(|p| {
        parse_id(p.file_name().unwrap().to_str().unwrap(), "hot-", ".ojl").unwrap()
    });
    let tail = hot.last().unwrap();
    let before = std::fs::metadata(tail).unwrap().len();
    let mut file = OpenOptions::new().append(true).open(tail).unwrap();
    file.write_all(b"bad trailing synthetic").unwrap();
    drop(file);
    let reader = f.reader();
    assert_eq!(std::fs::metadata(tail).unwrap().len(), before);
    check(&reader, 0, 20);
    assert!(PagedStore::open(&f.source(), f.opts(), &f.cache()).is_err());
    for item in std::fs::read_dir(f.cache()).unwrap() {
        let bytes = std::fs::read(item.unwrap().path()).unwrap();
        for needle in [
            b"synthetic paging".as_slice(),
            b"synthetic-key",
            b"synthetic attrs",
        ] {
            assert!(!bytes.windows(needle.len()).any(|b| b == needle));
        }
    }
    drop(reader);
    assert!(PagedStore::open(&f.source(), f.opts(), &f.source()).is_err());
    let unknown = f.root.join("foreign");
    std::fs::create_dir(&unknown).unwrap();
    std::fs::write(unknown.join("other-engine-authority"), b"synthetic").unwrap();
    assert!(PagedStore::open(&f.source(), f.opts(), &unknown).is_err());
    std::os::unix::fs::symlink(f.source(), f.root.join("link")).unwrap();
    assert!(PagedStore::open(&f.root.join("link"), f.opts(), &f.cache()).is_err());
}
