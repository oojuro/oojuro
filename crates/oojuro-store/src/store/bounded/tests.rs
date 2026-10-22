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
        assert!(!b.windows(26).any(|p| p == b"synthetic bounded recovery "));
    }
}
