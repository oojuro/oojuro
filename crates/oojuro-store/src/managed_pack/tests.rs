use super::*;
use crate::{NewRecord, crypto::FrameCipher};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
const KEY: [u8; 32] = [181; 32];
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "oojuro-managed-pack-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self { dir }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn birth(n: u8) -> Birth {
    Birth {
        format: FORMAT,
        stable_origin: [1; 32],
        identity: [2; 32],
        epoch: [n + 10; 32],
        generation: n as u64,
        predecessor_logical: [3; 32],
        parent_checkpoint: [n + 20; 32],
        key_version: [4; 32],
    }
}
#[derive(Clone)]
struct Fake(Arc<Mutex<State>>);
struct State {
    generation: u8,
    packs: BTreeMap<u64, PackRef>,
    locators: BTreeMap<u64, Locator>,
    dicts: BTreeMap<u32, DictionaryRef>,
    unavailable: bool,
    key_missing: bool,
    key_reads: u64,
}
impl Fake {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            generation: 1,
            packs: BTreeMap::new(),
            locators: BTreeMap::new(),
            dicts: BTreeMap::new(),
            unavailable: false,
            key_missing: false,
            key_reads: 0,
        })))
    }
    fn install(&self, p: PackRef, ls: Vec<Locator>) {
        let mut s = self.0.lock().unwrap();
        s.packs.insert(p.file_id, p);
        for l in ls {
            s.locators.insert(l.first_physical_seq, l);
        }
    }
}
impl Catalog for Fake {
    fn current(&self) -> Result<Vec<u8>> {
        let s = self.0.lock().unwrap();
        if s.unavailable {
            return Err(Error::Invalid("synthetic authority unavailable"));
        }
        Ok(vec![s.generation])
    }
    fn validate_birth(&self, b: &Birth) -> Result<()> {
        let s = self.0.lock().unwrap();
        if s.packs.values().any(|p| p.birth == *b) || s.dicts.values().any(|d| d.birth == *b) {
            Ok(())
        } else {
            Err(Error::Invalid("synthetic uncommitted birth"))
        }
    }
    fn locate(&self, seq: u64) -> Result<Locator> {
        self.0
            .lock()
            .unwrap()
            .locators
            .range(..=seq)
            .next_back()
            .map(|(_, l)| *l)
            .ok_or(Error::NotFound)
    }
    fn pack(&self, id: u64) -> Result<PackRef> {
        self.0
            .lock()
            .unwrap()
            .packs
            .get(&id)
            .cloned()
            .ok_or(Error::NotFound)
    }
    fn dictionary(&self, id: u32) -> Result<Option<DictionaryRef>> {
        Ok(self.0.lock().unwrap().dicts.get(&id).cloned())
    }
    fn cipher(&self, v: &[u8; 32]) -> Result<Arc<FrameCipher>> {
        let mut s = self.0.lock().unwrap();
        s.key_reads += 1;
        if s.key_missing || *v != [4; 32] {
            return Err(Error::KeyRequired);
        }
        Ok(Arc::new(FrameCipher::new(&KEY)))
    }
}
fn row(text: &[u8], time: i64) -> NewRecord<'_> {
    NewRecord {
        kind: 5,
        valid_at: time,
        recorded_at: time + 100,
        key: b"original-key",
        attrs: b"{original provenance and logical ID}",
        text,
    }
}
fn staged(f: &Fixture, id: u64, first: u64, n: u8, text: &[u8]) -> (PackRef, Vec<Locator>) {
    let mut s = Stage::new(
        &f.dir,
        birth(n),
        Reservation {
            file_id: id,
            first_physical_seq: first,
        },
        Arc::new(FrameCipher::new(&KEY)),
        Options::default(),
    )
    .unwrap();
    let mut ls = vec![];
    s.push(&[row(text, 7), row(b"punctuation !?!", 9)], &mut |l| {
        ls.push(l);
        Ok(())
    })
    .unwrap();
    s.push(&[row(text, 8)], &mut |l| {
        ls.push(l);
        Ok(())
    })
    .unwrap();
    (s.finish().unwrap(), ls)
}
#[test]
fn immutable_packs_incremental_original_fields_restart_and_current_fence() {
    let f = Fixture::new();
    let c = Fake::new();
    let text = b"original synthetic compressible trace ".repeat(300);
    let (p, ls) = staged(&f, 1, 40, 1, &text);
    assert_eq!(p.record_count, 3);
    assert_eq!(p.frame_count, 2);
    assert!(p.byte_len < text.len() as u64);
    let old = std::fs::read(codec::token_path(&f.dir, &p.token, false)).unwrap();
    c.install(p.clone(), ls);
    let r = Reader::new(&f.dir, c.clone(), 1 << 20).unwrap();
    let a = r.record(40).unwrap();
    assert_eq!((a.seq, a.kind, a.valid_at, a.recorded_at), (40, 5, 7, 107));
    assert_eq!(a.key, b"original-key");
    assert_eq!(a.attrs, b"{original provenance and logical ID}");
    assert_eq!(a.text, text);
    assert_eq!(r.get_range(42, 3, 21).unwrap(), text[3..21]);
    let (p2, l2) = staged(&f, 2, 43, 2, b"tail original");
    c.install(p2, l2);
    c.0.lock().unwrap().generation = 2;
    assert!(r.get(40).is_err());
    assert_eq!(r.cache_usage().unwrap().open_files, 0);
    let r = Reader::new(&f.dir, c.clone(), 1 << 20).unwrap();
    assert_eq!(r.get(40).unwrap(), text);
    assert_eq!(r.get(43).unwrap(), b"tail original");
    assert_eq!(
        std::fs::read(codec::token_path(&f.dir, &p.token, false)).unwrap(),
        old
    );
    for e in std::fs::read_dir(&f.dir).unwrap() {
        let b = std::fs::read(e.unwrap().path()).unwrap();
        for needle in [
            b"original-key".as_slice(),
            b"original provenance",
            b"original synthetic",
        ] {
            assert!(!b.windows(needle.len()).any(|v| v == needle));
        }
    }
}
#[test]
fn candidates_never_visible_poisoned_bounds_and_no_warm_key_fallback() {
    let f = Fixture::new();
    let c = Fake::new();
    let (p, ls) = staged(&f, 1, 0, 1, b"authorized synthetic text");
    let r = Reader::new(&f.dir, c.clone(), 1 << 20).unwrap();
    assert!(r.get(0).is_err());
    drop(r);
    c.install(p, ls);
    let r = Reader::new(&f.dir, c.clone(), 1 << 20).unwrap();
    assert_eq!(r.get(0).unwrap(), b"authorized synthetic text");
    let calls = c.0.lock().unwrap().key_reads;
    c.0.lock().unwrap().key_missing = true;
    assert!(r.get(0).is_err());
    assert!(c.0.lock().unwrap().key_reads > calls);
    assert_eq!(r.cache_usage().unwrap().open_files, 0);
    c.0.lock().unwrap().unavailable = true;
    assert!(r.get(0).is_err());
    assert_eq!(r.cache_usage().unwrap().open_files, 0);
    let mut s = Stage::new(
        &f.dir,
        birth(2),
        Reservation {
            file_id: 2,
            first_physical_seq: 3,
        },
        Arc::new(FrameCipher::new(&KEY)),
        Options::default(),
    )
    .unwrap();
    assert!(
        s.push(&[row(b"not committed", 1)], &mut |_| Err(Error::Invalid(
            "synthetic callback failure"
        )))
        .is_err()
    );
    assert!(s.finish().is_err());
    let mut s = Stage::new(
        &f.dir,
        birth(2),
        Reservation {
            file_id: 3,
            first_physical_seq: 3,
        },
        Arc::new(FrameCipher::new(&KEY)),
        Options::default(),
    )
    .unwrap();
    let big = vec![0; MAX_FRAME_BYTES];
    assert!(s.push(&[row(&big, 1)], &mut |_| Ok(())).is_err());
    assert!(s.finish().is_err());
    let mut s = Stage::new(
        &f.dir,
        birth(2),
        Reservation {
            file_id: 4,
            first_physical_seq: 3,
        },
        Arc::new(FrameCipher::new(&KEY)),
        Options::default(),
    )
    .unwrap();
    let rows = (0..1025).map(|_| row(b"tiny", 1)).collect::<Vec<_>>();
    assert!(s.push(&rows, &mut |_| Ok(())).is_err());
    assert!(s.finish().is_err());
}
#[test]
fn exact_parent_key_epoch_and_corruption_refuse_without_source_fallback() {
    let f = Fixture::new();
    let (p, ls) = staged(&f, 1, 0, 1, b"bound synthetic content");
    for field in 0..4 {
        let c = Fake::new();
        let mut bad = p.clone();
        match field {
            0 => bad.birth.parent_checkpoint = [90; 32],
            1 => bad.birth.epoch = [91; 32],
            2 => bad.birth.predecessor_logical = [92; 32],
            _ => bad.birth.key_version = [93; 32],
        };
        c.install(bad, ls.clone());
        assert!(Reader::new(&f.dir, c, 0).unwrap().get(0).is_err());
    }
    let c = Fake::new();
    c.install(p.clone(), ls);
    let path = codec::token_path(&f.dir, &p.token, false);
    let mut b = std::fs::read(&path).unwrap();
    *b.last_mut().unwrap() ^= 1;
    std::fs::write(path, b).unwrap();
    assert!(Reader::new(&f.dir, c, 0).unwrap().record(0).is_err());
}
#[test]
fn actual_dictionary_reuse_is_authenticated_and_inventory_is_exact() {
    let f = Fixture::new();
    let cipher = FrameCipher::new(&KEY);
    let bytes = b"synthetic training dictionary repeated words ".repeat(300);
    let d = stage_dictionary(&f.dir, birth(1), 77, &bytes, &cipher).unwrap();
    let mut s = Stage::with_dictionary(
        &f.dir,
        birth(1),
        Reservation {
            file_id: 1,
            first_physical_seq: 0,
        },
        Arc::new(cipher),
        Options::default(),
        Some(d.clone()),
    )
    .unwrap();
    let text = bytes.repeat(4);
    let mut ls = vec![];
    s.push(&[row(&text, 7)], &mut |l| {
        assert_eq!(l.dictionary_id, 77);
        ls.push(l);
        Ok(())
    })
    .unwrap();
    let p = s.finish().unwrap();
    let c = Fake::new();
    c.install(p.clone(), ls.clone());
    assert!(Reader::new(&f.dir, c.clone(), 0).unwrap().get(0).is_err());
    c.0.lock().unwrap().dicts.insert(77, d.clone());
    assert_eq!(
        Reader::new(&f.dir, c.clone(), 0).unwrap().get(0).unwrap(),
        text
    );
    let mut bad = d;
    bad.birth.parent_checkpoint = [88; 32];
    assert!(
        Stage::with_dictionary(
            &f.dir,
            birth(2),
            Reservation {
                file_id: 2,
                first_physical_seq: 1
            },
            Arc::new(FrameCipher::new(&KEY)),
            Options::default(),
            Some(bad)
        )
        .is_err()
    );
    let path = codec::token_path(&f.dir, &p.dictionary.unwrap().token, true);
    let mut b = std::fs::read(&path).unwrap();
    *b.last_mut().unwrap() ^= 1;
    std::fs::write(path, b).unwrap();
    assert!(Reader::new(&f.dir, c, 0).unwrap().get(0).is_err());
}
#[test]
fn resource_handles_bounded_across_more_than_cache_capacity() {
    let f = Fixture::new();
    let c = Fake::new();
    for id in 1..=34 {
        let (p, ls) = staged(&f, id, (id - 1) * 3, 1, b"bounded resource synthetic");
        c.install(p, ls);
    }
    let r = Reader::new(&f.dir, c, 0).unwrap();
    for id in 1..=34 {
        assert_eq!(r.get((id - 1) * 3).unwrap(), b"bounded resource synthetic");
    }
    assert_eq!(r.cache_usage().unwrap().open_files, 32);
}
#[test]
#[ignore]
fn managed_pack_crash_child() {
    let f = Fixture {
        dir: PathBuf::from(std::env::var("OOJURO_MANAGED_PACK_CRASH_ROOT").unwrap()),
    };
    let _ = staged(&f, 2, 3, 2, b"candidate never ACKed");
    panic!("phase not reached");
}
#[test]
fn sigkill_staging_publication_never_changes_committed_inventory_or_original_data() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::process::ExitStatusExt,
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    for phase in [
        "FrameStaged",
        "PackTempDurable",
        "PackPublished",
        "PackDurable",
    ] {
        let f = Fixture::new();
        let c = Fake::new();
        let (p, ls) = staged(&f, 1, 0, 1, b"old committed original");
        c.install(p, ls);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "managed_pack::tests::managed_pack_crash_child",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env("OOJURO_MANAGED_PACK_CRASH_ROOT", &f.dir)
            .env("OOJURO_MANAGED_PACK_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let listener = std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                if line.unwrap().contains("MANAGED_PACK_READY") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        let reached = rx.recv_timeout(Duration::from_secs(8));
        let _ = child.kill();
        let status = child.wait().unwrap();
        listener.join().unwrap();
        assert!(reached.is_ok(), "{phase}");
        assert_eq!(status.signal(), Some(9));
        let r = Reader::new(&f.dir, c, 0).unwrap();
        assert_eq!(r.get(0).unwrap(), b"old committed original");
        assert!(r.get(3).is_err());
    }
}

#[test]
fn complete_owned_data_scan_covers_committed_orphan_partial_and_refuses_foreign_types() {
    let f = Fixture::new();
    let _ = staged(&f, 1, 0, 1, b"published synthetic inventory");
    let _orphan = Stage::new(
        &f.dir,
        birth(2),
        Reservation {
            file_id: 2,
            first_physical_seq: 3,
        },
        Arc::new(FrameCipher::new(&KEY)),
        Options::default(),
    )
    .unwrap();
    let _ = stage_dictionary(
        &f.dir,
        birth(1),
        7,
        b"inventory dictionary",
        &FrameCipher::new(&KEY),
    )
    .unwrap();
    let mut count = 0;
    let mut temp = 0;
    let mut dict = 0;
    scan_inventory(&f.dir, |a| {
        assert_eq!(a.path.parent(), Some(f.dir.as_path()));
        assert_eq!(std::fs::metadata(&a.path).unwrap().len(), a.byte_len);
        count += 1;
        temp += usize::from(a.temporary);
        dict += usize::from(a.kind == ArtifactKind::Dictionary);
        Ok(())
    })
    .unwrap();
    assert_eq!((count, temp, dict), (3, 1, 1));
    let partial = f.dir.join(format!("data-{}.stage", "a".repeat(64)));
    std::fs::write(&partial, b"").unwrap();
    scan_inventory(&f.dir, |_| Ok(())).unwrap();
    let foreign = f.dir.join("foreign-authority");
    std::fs::write(&foreign, b"private unrelated authority").unwrap();
    assert!(scan_inventory(&f.dir, |_| Ok(())).is_err());
    assert!(foreign.exists());
    std::fs::remove_file(foreign).unwrap();
    let link = f.dir.join(format!("data-{}.bin", "b".repeat(64)));
    std::os::unix::fs::symlink(&partial, &link).unwrap();
    assert!(scan_inventory(&f.dir, |_| Ok(())).is_err());
    std::fs::remove_file(link).unwrap();
    let directory = f.dir.join(format!("dict-{}.stage", "c".repeat(64)));
    std::fs::create_dir(&directory).unwrap();
    assert!(scan_inventory(&f.dir, |_| Ok(())).is_err());
}
