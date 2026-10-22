//! Encrypted stores: frames and dictionaries are sealed with the store key (compress first, then encrypt),
//! a store opens only with its key, and forged or damaged bytes are refused.

use oojuro_store::crypto::{FrameCipher, random_key};
use oojuro_store::util::Rng;
use oojuro_store::{Error, NewRecord, Options, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn temp(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let p = base.join(format!("oojuro-enc-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

const SECRET: &str = "ZEBRA-QUOKKA-PLAINTEXT-MARKER";

/// Records with a marker in every text, repeated bodies (so dedupe references appear) and some noise.
fn records(n: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng::new(seed);
    let body: Vec<u8> = (0..6000).map(|i| b"abcdefghij klmnop\n"[(i * 7 + i / 13) % 18]).collect();
    (0..n)
        .map(|i| {
            let mut t = format!("{SECRET} record {i} ").into_bytes();
            t.extend_from_slice(&body);
            for _ in 0..rng.below(2000) {
                t.push(b'a' + rng.below(26) as u8);
            }
            t
        })
        .collect()
}

fn opts(cipher: Option<Arc<FrameCipher>>) -> Options {
    Options { dict_train_after: 64 << 10, dict_bytes: 8 << 10, sync: false, cold_frame_bytes: 64 << 10, cipher, ..Options::default() }
}

fn append(s: &mut Store, recs: &[Vec<u8>], from: usize) {
    for (i, t) in recs.iter().enumerate() {
        let t_us = 1_700_000_000 + (from + i) as i64;
        s.append(&[NewRecord { kind: 7, valid_at: t_us, recorded_at: t_us, key: b"", attrs: b"", text: t }]).unwrap();
    }
}

fn check(s: &Store, recs: &[Vec<u8>]) {
    assert_eq!(s.len(), recs.len() as u64);
    for (i, t) in recs.iter().enumerate() {
        assert_eq!(&s.get(i as u64).unwrap(), t, "record {i}");
    }
}

fn all_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_file()).collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn encrypted_roundtrip_hot_cold_and_reopen() {
    let dir = temp("roundtrip");
    let key = random_key();
    let c = Arc::new(FrameCipher::new(&key));
    let recs = records(400, 3);
    {
        let mut s = Store::open(&dir, opts(Some(c.clone()))).unwrap();
        append(&mut s, &recs[..250], 0);
        check_prefix(&s, &recs[..250]);
        s.compact().unwrap();
        append(&mut s, &recs[250..], 250);
        check(&s, &recs);
    }
    // reopen with the key: hot replay, cold segments, dedupe references
    let mut s = Store::open(&dir, opts(Some(c.clone()))).unwrap();
    check(&s, &recs);
    s.compact().unwrap();
    check(&s, &recs);
    drop(s);
    let s = Store::open(&dir, opts(Some(c))).unwrap();
    check(&s, &recs);
    // no file of the store holds the plaintext marker, not even in the dictionary
    for f in all_files(&dir) {
        let b = std::fs::read(&f).unwrap();
        assert!(!contains(&b, SECRET.as_bytes()), "plaintext in {}", f.display());
        assert!(!contains(&b, b"abcdefghij"), "plaintext in {}", f.display());
    }
    assert!(all_files(&dir).iter().any(|f| f.to_string_lossy().contains("dict-")), "a dictionary was trained");
}

fn check_prefix(s: &Store, recs: &[Vec<u8>]) {
    for (i, t) in recs.iter().enumerate() {
        assert_eq!(&s.get(i as u64).unwrap(), t);
    }
}

#[test]
fn a_store_opens_only_with_its_key() {
    let dir = temp("keys");
    let c = Arc::new(FrameCipher::new(&random_key()));
    let recs = records(20, 4);
    {
        let mut s = Store::open(&dir, opts(Some(c.clone()))).unwrap();
        append(&mut s, &recs, 0);
    }
    assert!(matches!(Store::open(&dir, opts(None)), Err(Error::KeyRequired)));
    let other = Arc::new(FrameCipher::new(&random_key()));
    assert!(matches!(Store::open(&dir, opts(Some(other))), Err(Error::WrongKey)));
    let s = Store::open(&dir, opts(Some(c))).unwrap();
    check(&s, &recs);
}

#[test]
fn forged_frames_are_refused_even_with_a_valid_checksum() {
    let dir = temp("forged");
    let c = Arc::new(FrameCipher::new(&random_key()));
    let recs = records(60, 5);
    {
        let mut s = Store::open(&dir, opts(Some(c.clone()))).unwrap();
        append(&mut s, &recs, 0);
        s.compact().unwrap();
    }
    let cold: Vec<PathBuf> = all_files(&dir).into_iter().filter(|p| p.extension().is_some_and(|e| e == "ojc")).collect();
    assert_eq!(cold.len(), 1);
    let mut b = std::fs::read(&cold[0]).unwrap();
    // first frame: len u32 | crc u32 | payload (32 B file head before it)
    let at = 32;
    let len = u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    b[at + 8 + len - 5] ^= 0x55; // damage the sealed body
    let mut h = crc32fast::Hasher::new();
    h.update(&(len as u32).to_le_bytes());
    h.update(&b[at + 8..at + 8 + len]);
    let crc = h.finalize();
    b[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes()); // a forger recomputes the checksum
    std::fs::write(&cold[0], &b).unwrap();
    let s = Store::open(&dir, opts(Some(c))).unwrap();
    let r = s.get(0);
    assert!(matches!(r, Err(Error::Crypto(_))), "got {r:?}");
}

#[test]
fn an_unencrypted_store_gets_a_key_and_old_frames_stay_readable() {
    let dir = temp("upgrade");
    let recs = records(80, 6);
    {
        let mut s = Store::open(&dir, opts(None)).unwrap();
        append(&mut s, &recs[..40], 0);
    }
    let c = Arc::new(FrameCipher::new(&random_key()));
    {
        let mut s = Store::open(&dir, opts(Some(c.clone()))).unwrap();
        append(&mut s, &recs[40..], 40);
        check(&s, &recs);
    }
    assert!(matches!(Store::open(&dir, opts(None)), Err(Error::KeyRequired)));
    let s = Store::open(&dir, opts(Some(c))).unwrap();
    check(&s, &recs);
}
