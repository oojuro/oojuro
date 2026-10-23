use super::*;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "oojuro-bounded-{}-{}",
            std::process::id(),
            N.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn file(&self, bytes: &[u8]) -> File {
        let p = self.0.join("synthetic.bin");
        std::fs::write(&p, bytes).unwrap();
        File::open(p).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn old_frame(data: &[u8], at: usize) -> Option<usize> {
    let len = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
    let want = u32::from_le_bytes(data.get(at + 4..at + 8)?.try_into().ok()?);
    if len == 0 || len > MAX_FRAME + 64 {
        return None;
    }
    let b = data.get(at + 8..at + 8 + len)?;
    (frame_crc(b) == want).then_some(len)
}
fn old_any(data: &[u8], from: usize) -> bool {
    (from..data.len().saturating_sub(8)).any(|i| old_frame(data, i).is_some())
}
#[test]
fn positional_scanner_matches_old_candidate_semantics_at_every_boundary() {
    let t = Temp::new();
    for prefix in [
        0,
        1,
        7,
        8,
        SCAN_BYTES - 8,
        SCAN_BYTES - 1,
        SCAN_BYTES,
        SCAN_BYTES + 7,
    ] {
        let mut bytes = vec![0; prefix];
        let good = seal_frame(b"synthetic later CRC payload");
        bytes.extend(&good);
        bytes.extend([0; 9]);
        let f = t.file(&bytes);
        for from in [
            0,
            prefix.saturating_sub(1),
            prefix,
            prefix + 1,
            bytes.len().saturating_sub(8),
            bytes.len(),
        ] {
            assert_eq!(
                any_after(&f, bytes.len() as u64, from as u64).unwrap(),
                old_any(&bytes, from),
                "prefix{prefix} from{from}"
            );
        }
        let p = payload(&f, bytes.len() as u64, prefix as u64)
            .unwrap()
            .unwrap();
        assert_eq!(p, b"synthetic later CRC payload");
    }
}
#[test]
fn malformed_length_torn_crc_and_embedded_later_frame_keep_old_semantics() {
    let t = Temp::new();
    let mut rng = crate::util::Rng::new(993);
    for case in 0..24 {
        let mut bytes = vec![0; SCAN_BYTES + 19];
        for b in &mut bytes {
            *b = rng.next_u64() as u8;
        }
        if case % 3 == 0 {
            let at = SCAN_BYTES - 3;
            let good = seal_frame(&vec![case as u8; SCAN_BYTES + 31]);
            bytes.resize(at + good.len() + 3, 0);
            bytes[at..at + good.len()].copy_from_slice(&good);
        }
        let f = t.file(&bytes);
        for from in [0, 3, SCAN_BYTES - 4, SCAN_BYTES + 4] {
            assert_eq!(
                any_after(&f, bytes.len() as u64, from as u64).unwrap(),
                old_any(&bytes, from)
            );
        }
        for at in [0, 3, SCAN_BYTES - 4, bytes.len() - 5] {
            let got = payload(&f, bytes.len() as u64, at as u64).unwrap();
            assert_eq!(got.as_ref().map(Vec::len), old_frame(&bytes, at));
        }
    }
    let mut huge = vec![0; 16];
    huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    let f = t.file(&huge);
    assert!(payload(&f, huge.len() as u64, 0).unwrap().is_none());
}
#[test]
fn training_is_bounded_deterministic_and_samples_beyond_prefix() {
    let mut a = Training::new();
    let mut b = Training::new();
    let mut text = vec![b'x'; 32 << 10];
    for i in 0u64..20_000 {
        text[..8].copy_from_slice(&i.to_le_bytes());
        a.push(&text);
        b.push(&text);
    }
    assert_eq!(a.samples.len(), SAMPLE_COUNT);
    assert_eq!(
        a.samples.iter().map(|s| s.bytes.len()).sum::<usize>(),
        TRAIN_BYTES
    );
    assert!(a.samples.iter().any(|s| s.ordinal > 10_000));
    let mut left = a
        .samples
        .iter()
        .map(|s| (s.ordinal, s.priority, s.bytes.clone()))
        .collect::<Vec<_>>();
    let mut right = b
        .samples
        .iter()
        .map(|s| (s.ordinal, s.priority, s.bytes.clone()))
        .collect::<Vec<_>>();
    left.sort();
    right.sort();
    assert_eq!(left, right);
    drop(left);
    drop(right);
    assert_eq!(a.finish(16 << 10), b.finish(16 << 10));
    let mut small = Training::new();
    for _ in 0..15 {
        small.push(&[b'a'; 32]);
    }
    assert!(small.finish(16 << 10).is_none());
    assert!(Training::new().finish(usize::MAX).is_none());
}
#[test]
fn streamed_hot_training_reopen_and_compaction_preserve_sealed_original_fields() {
    let t = Temp::new();
    let key = [211; 32];
    let opts = Options {
        hot_level: 0,
        cold_level: 1,
        cold_frame_bytes: 16 << 10,
        dict_bytes: 16 << 10,
        dict_train_after: 128 << 10,
        hot_file_bytes: 64 << 10,
        sync: true,
        compact_threads: 1,
        dedupe_chunk: 0,
        cipher: Some(Arc::new(FrameCipher::new(&key))),
        require_sealed: true,
        ..Options::default()
    };
    let mut s = Store::open(&t.0, opts.clone()).unwrap();
    for i in 0..160u64 {
        let text = format!("synthetic bounded recovery row {i} ")
            .repeat(140)
            .into_bytes();
        let id = s
            .append(&[NewRecord {
                kind: 3,
                valid_at: i as i64,
                recorded_at: i as i64 + 9,
                key: b"synthetic-key",
                attrs: b"{synthetic}",
                text: &text,
            }])
            .unwrap();
        assert_eq!(id, i);
    }
    drop(s);
    let mut s = Store::open(&t.0, opts.clone()).unwrap();
    assert_eq!(s.len(), 160);
    let before = (0..160).map(|i| s.record(i).unwrap()).collect::<Vec<_>>();
    s.compact().unwrap();
    drop(s);
    let s = Store::open(&t.0, opts).unwrap();
    for (old, i) in before.iter().zip(0..160) {
        let r = s.record(i).unwrap();
        assert_eq!(
            (
                &r.text,
                &r.key,
                &r.attrs,
                r.seq,
                r.kind,
                r.valid_at,
                r.recorded_at
            ),
            (
                &old.text,
                &old.key,
                &old.attrs,
                old.seq,
                old.kind,
                old.valid_at,
                old.recorded_at
            )
        );
    }
    for item in std::fs::read_dir(&t.0).unwrap() {
        let b = std::fs::read(item.unwrap().path()).unwrap();
        let marker = b"synthetic bounded recovery ";
        assert!(!b.windows(marker.len()).any(|p| p == marker));
    }
}

#[test]
fn short_records_train_and_unusable_records_retry_only_geometrically() {
    let t = Temp::new();
    let opts = Options {
        hot_level: 1,
        cold_level: 1,
        dict_bytes: 16 << 10,
        dict_train_after: 1,
        hot_file_bytes: 1 << 30,
        sync: false,
        dedupe_chunk: 0,
        compact_threads: 1,
        ..Options::default()
    };
    let mut store = Store::open(&t.0, opts.clone()).unwrap();
    let mut attempts = 0;
    for i in 0u64..8192 {
        let mut text = [b'x'; 32];
        text[..8].copy_from_slice(&i.to_le_bytes());
        let next = store.hot_train_next;
        store
            .append(&[NewRecord {
                kind: 1,
                valid_at: i as i64,
                recorded_at: i as i64,
                key: b"",
                attrs: b"",
                text: &text,
            }])
            .unwrap();
        attempts += usize::from(store.hot_train_next != next);
    }
    assert!(
        store.hot_dict != 0,
        "enough valid32-byte rows must produce a dictionary"
    );
    assert!(attempts <= 16);
    drop(store);
    let store = Store::open(&t.0, opts).unwrap();
    for i in [0u64, 1, 4095, 8191] {
        assert_eq!(&store.get(i).unwrap()[..8], &i.to_le_bytes());
    }
    let u = Temp::new();
    let opts = Options {
        dict_bytes: 16 << 10,
        dict_train_after: 0,
        sync: false,
        dedupe_chunk: 0,
        compact_threads: 1,
        cold_level: 1,
        ..Options::default()
    };
    let mut store = Store::open(&u.0, opts).unwrap();
    let mut attempts = 0;
    for i in 0..2000 {
        let next = store.hot_train_next;
        store
            .append(&[NewRecord {
                kind: 1,
                valid_at: i,
                recorded_at: i,
                key: b"",
                attrs: b"",
                text: b"x",
            }])
            .unwrap();
        attempts += usize::from(store.hot_train_next != next);
    }
    assert_eq!(store.hot_dict, 0);
    assert!(
        attempts <= 16,
        "failed training scanned too often: {attempts}"
    );
    store.compact().unwrap();
    assert_eq!(store.hot_train_next, store.hot_raw + 1);
    store.hot_raw = usize::MAX - 1;
    store.hot_train_next = usize::MAX - 1;
    store
        .append(&[NewRecord {
            kind: 1,
            valid_at: 2001,
            recorded_at: 2001,
            key: b"",
            attrs: b"",
            text: b"x",
        }])
        .unwrap();
    assert_eq!(store.hot_train_next, usize::MAX);
    store
        .append(&[NewRecord {
            kind: 1,
            valid_at: 2002,
            recorded_at: 2002,
            key: b"",
            attrs: b"",
            text: b"x",
        }])
        .unwrap();
    assert_eq!(store.hot_train_next, usize::MAX);
}
#[test]
fn buffered_hot_payloads_match_positional_frames_across_lookahead_windows() {
    let t = Temp::new();
    let mut bytes = Vec::new();
    let mut positions = Vec::new();
    for i in 0..75 {
        positions.push(bytes.len());
        bytes.extend(seal_frame(&vec![
            i as u8;
            if i % 9 == 0 {
                SCAN_BYTES + 19
            } else {
                199
            }
        ]));
    }
    let file = t.file(&bytes);
    let mut input = HotReader::new(&file, bytes.len() as u64);
    for at in positions {
        assert_eq!(
            input.payload(at as u64).unwrap(),
            payload(&file, bytes.len() as u64, at as u64).unwrap()
        );
        assert!(input.bytes.len() <= SCAN_BYTES);
    }
}

#[test]
#[ignore]
fn local_matched_old_and_windowed_crc_scan_measurement() {
    use std::{hint::black_box, time::Instant};
    let t = Temp::new();
    let mut bytes = Vec::new();
    for i in 0..2000 {
        bytes.extend(seal_frame(&vec![i as u8; 512]));
    }
    let file = t.file(&bytes);
    let path = t.0.join("synthetic.bin");
    let loops = 50;
    let start = Instant::now();
    let mut old_count = 0;
    for _ in 0..loops {
        let whole = std::fs::read(&path).unwrap();
        let mut at = 0;
        while at < whole.len() {
            let len = old_frame(&whole, at).unwrap();
            old_count += 1;
            black_box(&whole[at + 8..at + 8 + len]);
            at += 8 + len;
        }
    }
    let old = start.elapsed();
    let start = Instant::now();
    let mut new_count = 0;
    for _ in 0..loops {
        let mut reader = HotReader::new(&file, bytes.len() as u64);
        let mut at = 0;
        while at < bytes.len() as u64 {
            let payload = reader.payload(at).unwrap().unwrap();
            new_count += 1;
            black_box(&payload);
            at += 8 + payload.len() as u64;
        }
    }
    let new = start.elapsed();
    assert_eq!(old_count, new_count);
    assert_eq!(new_count, loops * 2000);
    println!(
        "LOCAL CRC-only warm-file scan: iterations={loops}, bytes_per_iteration={}, frames={new_count}, old_whole_file_ms={:.3}, bounded_window_ms={:.3}; excludes AEAD/decode, no provider/scale claim",
        bytes.len(),
        old.as_secs_f64() * 1000.0,
        new.as_secs_f64() * 1000.0
    );
}
