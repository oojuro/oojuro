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
    forge_first_cold_frame(&dir);
    let s = Store::open(&dir, opts(Some(c))).unwrap();
    let r = s.get(0);
    assert!(matches!(r, Err(Error::Crypto(_))), "got {r:?}");
}

#[test]
fn cleared_read_cache_preserves_reads_and_reauthenticates_frames() {
    let dir = temp("clear-cache");
    let c = Arc::new(FrameCipher::new(&random_key()));
    let recs = records(20, 17);
    let mut s = Store::open(&dir, opts(Some(c))).unwrap();
    append(&mut s, &recs, 0);
    s.compact().unwrap();
    check(&s, &recs);
    let original = s.get(0).unwrap();
    s.clear_read_cache();
    check(&s, &recs);
    assert_eq!(original, recs[0]);
    forge_first_cold_frame(&dir);
    s.clear_read_cache();
    assert!(matches!(s.get(0), Err(Error::Crypto(_))));
    assert_eq!(original, recs[0]);
}

fn forge_first_cold_frame(dir: &Path) {
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


#[test]
fn encrypted_raw_frames_preserve_side_fields_ranges_and_time_scan() {
    let dir = temp("raw-fields");
    let cipher = Arc::new(FrameCipher::new(&random_key()));
    let options = Options {
        hot_level: 0,
        cold_level: 0,
        dict_bytes: 0,
        cache_bytes: 0,
        cipher: Some(cipher.clone()),
        sync: false,
        ..Options::default()
    };
    let text = b"synthetic text marker with a range";
    let key = b"synthetic private record key";
    let attrs = b"synthetic private record attributes";
    let expected = oojuro_store::Record {
        seq: 0, kind: 2, valid_at: 100, recorded_at: 200,
        key: key.to_vec(), attrs: attrs.to_vec(), text: text.to_vec(),
    };
    {
        let mut store = Store::open(&dir, options.clone()).unwrap();
        store.append(&[NewRecord {
            kind: expected.kind, valid_at: expected.valid_at, recorded_at: expected.recorded_at,
            key, attrs, text,
        }]).unwrap();
        assert_eq!(store.record(0).unwrap(), expected);
        assert_eq!(store.get_range(0, 10, 21).unwrap(), &text[10..21]);
        for file in all_files(&dir) {
            let bytes = std::fs::read(file).unwrap();
            for marker in [text.as_slice(), key.as_slice(), attrs.as_slice()] {
                assert!(!contains(&bytes, marker), "raw hot body leaked a side field or text");
            }
        }
        store.compact().unwrap();
    }
    let store = Store::open(&dir, options).unwrap();
    assert_eq!(store.record(0).unwrap(), expected);
    assert_eq!(store.get_range(0, 10, 21).unwrap(), &text[10..21]);
    let mut found = Vec::new();
    store.scan_time(100, 101, |record| { found.push(record.clone()); true }).unwrap();
    assert_eq!(found, vec![expected]);
    let mut excluded = false;
    store.scan_time(101, 102, |_| { excluded = true; true }).unwrap();
    assert!(!excluded);
    for file in all_files(&dir) {
        let bytes = std::fs::read(file).unwrap();
        for marker in [text.as_slice(), key.as_slice(), attrs.as_slice()] {
            assert!(!contains(&bytes, marker), "raw cold body leaked a side field or text");
        }
    }
}

#[test]
fn forged_hot_frame_fails_replay_without_truncating_the_log() {
    let dir = temp("hot-forged");
    let cipher = Arc::new(FrameCipher::new(&random_key()));
    let options = Options { dict_bytes: 0, ..opts(Some(cipher)) };
    {
        let mut store = Store::open(&dir, options.clone()).unwrap();
        append(&mut store, &records(1, 9), 0);
    }
    let hot = all_files(&dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "ojl")).unwrap();
    let mut bytes = std::fs::read(&hot).unwrap();
    let at = 32;
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    bytes[at + 8 + len - 1] ^= 1;
    let mut hash = crc32fast::Hasher::new();
    hash.update(&bytes[at..at + 4]);
    hash.update(&bytes[at + 8..at + 8 + len]);
    let checksum = hash.finalize();
    bytes[at + 4..at + 8].copy_from_slice(&checksum.to_le_bytes());
    std::fs::write(&hot, &bytes).unwrap();
    assert!(matches!(Store::open(&dir, options), Err(Error::Crypto(_))));
    assert_eq!(std::fs::read(hot).unwrap(), bytes, "authentication failure must preserve evidence");
}

#[test]
fn forged_dictionary_fails_authentication_even_with_a_valid_checksum() {
    let dir = temp("dict-forged");
    let options = opts(Some(Arc::new(FrameCipher::new(&random_key()))));
    {
        let mut store = Store::open(&dir, options.clone()).unwrap();
        append(&mut store, &records(80, 10), 0);
        store.compact().unwrap();
    }
    let dict = all_files(&dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "ojd")).expect("dictionary trained");
    let mut bytes = std::fs::read(&dict).unwrap();
    assert_eq!(&bytes[..8], b"OJDICT02");
    *bytes.last_mut().unwrap() ^= 1;
    let checksum = crc32fast::hash(&bytes[20..]);
    bytes[16..20].copy_from_slice(&checksum.to_le_bytes());
    std::fs::write(&dict, bytes).unwrap();
    assert!(matches!(Store::open(&dir, options), Err(Error::Crypto(_))));
}

#[test]
fn malformed_encryption_marker_is_rejected_without_replacement() {
    let dir = temp("marker");
    let options = opts(Some(Arc::new(FrameCipher::new(&random_key()))));
    drop(Store::open(&dir, options.clone()).unwrap());
    let marker = dir.join("ENCRYPTED");
    for bytes in [Vec::new(), b"OJENC001".to_vec(), vec![0u8; 16], vec![0u8; 17]] {
        std::fs::write(&marker, &bytes).unwrap();
        assert!(matches!(Store::open(&dir, options.clone()), Err(Error::Corrupt(_))));
        assert_eq!(std::fs::read(&marker).unwrap(), bytes);
    }
}

#[test]
fn sealed_empty_bodies_and_truncation_are_checked() {
    let cipher = FrameCipher::new(&random_key());
    let frame = cipher.seal_frame(b"same head", b"");
    let dict = cipher.seal_dict(b"same head", b"");
    assert_eq!(cipher.open_frame(b"same head", &frame).unwrap(), b"");
    assert_eq!(cipher.open_dict(b"same head", &dict).unwrap(), b"");
    assert!(matches!(cipher.open_dict(b"same head", &frame), Err(Error::Crypto(_))));
    assert!(matches!(cipher.open_frame(b"same head", &dict), Err(Error::Crypto(_))));
    for len in 0..frame.len() {
        assert!(cipher.open_frame(b"same head", &frame[..len]).is_err());
        assert!(cipher.open_dict(b"same head", &dict[..len]).is_err());
    }
}

#[test]
fn strict_sealing_refuses_plaintext_legacy_upgrade_without_mutating_it() {
    let dir=temp("strict-legacy");
    { let mut s=Store::open(&dir,opts(None)).unwrap();append(&mut s,&records(1,9),0); }
    let before: Vec<_>=all_files(&dir).into_iter().map(|p|{let b=std::fs::read(&p).unwrap();(p,b)}).collect();
    let strict=Options { require_sealed:true,..opts(Some(Arc::new(FrameCipher::new(&random_key())))) };
    assert!(matches!(Store::open(&dir,strict),Err(Error::Crypto(_))));
    assert!(!dir.join("ENCRYPTED").exists());
    for (p,b) in before { assert_eq!(std::fs::read(p).unwrap(),b); }
}

#[test]
fn strict_sealing_refuses_plaintext_frame_downgrade_without_truncation() {
    let plain=temp("strict-donor");let sealed=temp("strict-recipient");let cipher=Arc::new(FrameCipher::new(&random_key()));
    { let mut s=Store::open(&plain,opts(None)).unwrap();append(&mut s,&records(1,9),0); }
    { let mut s=Store::open(&sealed,opts(Some(cipher.clone()))).unwrap();append(&mut s,&records(1,9),0); }
    let path=sealed.join("hot-00000001.ojl");
    let downgraded=std::fs::read(plain.join("hot-00000001.ojl")).unwrap();std::fs::write(&path,&downgraded).unwrap();
    let strict=Options { require_sealed:true,..opts(Some(cipher)) };
    assert!(matches!(Store::open(&sealed,strict),Err(Error::Crypto(_))));
    assert_eq!(std::fs::read(path).unwrap(),downgraded);
}

#[test]
fn strict_sealing_requires_a_key_before_creating_a_store() {
    let dir=temp("strict-key");
    assert!(matches!(Store::open(&dir,Options { require_sealed:true,..Options::default() }),Err(Error::KeyRequired)));
    assert!(!dir.exists());
}

#[test]
fn strict_sealing_refuses_plaintext_cold_frame_downgrade() {
    let plain=temp("strict-cold-donor");let sealed=temp("strict-cold-recipient");let cipher=Arc::new(FrameCipher::new(&random_key()));
    { let mut s=Store::open(&plain,opts(None)).unwrap();append(&mut s,&records(2,9),0);s.compact().unwrap(); }
    { let mut s=Store::open(&sealed,opts(Some(cipher.clone()))).unwrap();append(&mut s,&records(2,9),0);s.compact().unwrap(); }
    let donor=all_files(&plain).into_iter().find(|p|p.extension().is_some_and(|e|e=="ojc")).unwrap();
    let path=sealed.join(donor.file_name().unwrap());std::fs::copy(donor,&path).unwrap();
    let strict=Options { require_sealed:true,..opts(Some(cipher)) };
    let s=Store::open(&sealed,strict).unwrap();
    assert!(matches!(s.get(0),Err(Error::Crypto(_))));
}
