use super::*;
use oojuro_store::{
    NewRecord,
    crypto::FrameCipher,
    managed_pack::{Birth, FORMAT, Options, Reservation, Stage},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const KEY: [u8; 32] = [191; 32];
struct A {
    generation: AtomicU64,
}
fn birth(n: u64) -> Birth {
    Birth {
        format: FORMAT,
        stable_origin: [1; 32],
        identity: [2; 32],
        epoch: [(n + 10) as u8; 32],
        generation: n,
        predecessor_logical: [3; 32],
        parent_checkpoint: [(n + 20) as u8; 32],
        key_version: [4; 32],
    }
}
impl tree::Authority for A {
    fn current(&self) -> Result<Vec<u8>> {
        let n = self.generation.load(Ordering::Relaxed);
        if n == 999 {
            return Err(Error::Unavailable);
        }
        birth(n).encode().map_err(|_| Error::Authority)
    }
    fn validate_birth(&self, b: &[u8], _: Purpose, class: KeyClass) -> Result<()> {
        let v = Birth::decode(b).map_err(|_| Error::Authority)?;
        if class != KeyClass::Data
            || v != birth(v.generation)
            || v.generation > self.generation.load(Ordering::Relaxed)
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
struct F {
    root: PathBuf,
    frame: PathBuf,
    file: PathBuf,
    dictionary: PathBuf,
    cipher: Arc<FrameCipher>,
    a: A,
}
impl F {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "oojuro-physical-tree-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        for n in ["data", "frame", "file", "dictionary"] {
            std::fs::create_dir(root.join(n)).unwrap();
        }
        Self {
            frame: root.join("frame"),
            file: root.join("file"),
            dictionary: root.join("dictionary"),
            root,
            cipher: Arc::new(FrameCipher::new(&KEY)),
            a: A {
                generation: AtomicU64::new(1),
            },
        }
    }
    fn contexts(&self) -> Contexts<'_> {
        let context = |purpose, identity, directory| Context {
            directory,
            cipher: &self.cipher,
            authority: &self.a,
            purpose,
            identity: [identity; 32],
            class: KeyClass::Data,
            sort_of: None,
            cache: None,
        };
        Contexts {
            frame: context(Purpose::Frame, 11, &self.frame),
            file: context(Purpose::File, 12, &self.file),
            dictionary: context(Purpose::Dictionary, 13, &self.dictionary),
        }
    }
    fn append(&self, e: &mut Editor<'_>, id: u64, seq: u64, n: u64) -> PackRef {
        let mut s = Stage::new(
            &self.root.join("data"),
            birth(n),
            Reservation {
                file_id: id,
                first_physical_seq: seq,
            },
            self.cipher.clone(),
            Options::default(),
        )
        .unwrap();
        let row = NewRecord {
            kind: 3,
            valid_at: seq as i64,
            recorded_at: 1000 + seq as i64,
            key: b"original logical key",
            attrs: b"original provenance",
            text: b"actual immutable frame and original contents",
        };
        s.push(&[row], &mut |l| {
            e.insert_frame(&l)
                .map_err(|_| oojuro_store::Error::Invalid("tree frame stage"))
        })
        .unwrap();
        let p = s.finish().unwrap();
        e.insert_pack(&p).unwrap();
        p
    }
}
impl Drop for F {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn real_pack_cow_tail_reopen_floor_lookup_and_full_inventory_without_replay() {
    let f = F::new();
    let c = f.contexts();
    let roots = Roots::empty(&c).unwrap();
    let mut e = Editor::new(c.clone(), roots).unwrap();
    for id in 1..=140 {
        f.append(&mut e, id, id * 2, 1);
    }
    let old = e.finish().unwrap();
    assert_eq!(old.frame.count, 140);
    assert_eq!(old.file.count, 140);
    assert_eq!(old.locate(&c, 278).unwrap().first_physical_seq, 278);
    assert!(old.locate(&c, 279).is_err());
    let old_head = old.file.node.as_ref().unwrap().clone();
    let mut page_count = 0;
    old.metadata_pages(&c, &mut |_, _| {
        page_count += 1;
        Ok(())
    })
    .unwrap();
    assert!(page_count > 3);
    let bytes = serde_json::to_vec(&old).unwrap();
    let reopened: Roots = serde_json::from_slice(&bytes).unwrap();
    let mut inventory = 0;
    reopened
        .inventory(&c, &mut |i| {
            match i {
                Inventory::Pack(p) => {
                    assert_eq!(p.file_id, p.first_physical_seq / 2);
                    inventory += 1
                }
                Inventory::Dictionary(_) => panic!("unexpected dictionary"),
            };
            Ok(())
        })
        .unwrap();
    assert_eq!(inventory, 140);
    f.a.generation.store(2, Ordering::Relaxed);
    let c = f.contexts();
    let mut e = Editor::new(c.clone(), reopened).unwrap();
    f.append(&mut e, 141, 282, 2);
    f.append(&mut e, 142, 284, 2);
    let next = e.finish().unwrap();
    assert_eq!(next.frame.count, 142);
    assert_eq!(next.locate(&c, 2).unwrap().first_physical_seq, 2);
    assert_eq!(next.locate(&c, 284).unwrap().file_id, 142);
    let mut old_births = 0;
    next.metadata_pages(&c, &mut |_, r| {
        if r.birth == birth(1).encode().unwrap() {
            old_births += 1;
        }
        Ok(())
    })
    .unwrap();
    assert!(
        old_births > 0,
        "COW must retain untouched authenticated pages"
    );
    assert!(old.file.node.as_ref().unwrap().digest == old_head.digest);
    assert_ne!(next.file.node.as_ref().unwrap().digest, old_head.digest);
    // Healthy old pages are ancestors; a forged same-generation birth is not.
    let mut fork = next.clone();
    fork.file.binding = birth(2).encode().unwrap();
    fork.file.binding[70] ^= 1;
    assert!(fork.pack(&c, 1).is_err());
    f.a.generation.store(999, Ordering::Relaxed);
    assert!(next.locate(&c, 2).is_err());
}
#[test]
fn missing_pack_coverage_replacement_and_wrong_root_classes_refuse_candidates() {
    let f = F::new();
    let c = f.contexts();
    let roots = Roots::empty(&c).unwrap();
    let mut e = Editor::new(c.clone(), roots).unwrap();
    let p = f.append(&mut e, 1, 0, 1);
    let roots = e.finish().unwrap();
    let mut e = Editor::new(c.clone(), roots.clone()).unwrap();
    assert!(e.insert_pack(&p).is_err());
    assert!(e.finish().is_err());
    let mut e = Editor::new(c.clone(), roots.clone()).unwrap();
    let l = Locator {
        file_id: 2,
        frame_ordinal: 0,
        first_physical_seq: 1,
        count: 1,
        offset: 304,
        payload_len: 60,
        raw_len: 12,
        dictionary_id: 0,
        valid_lo: 1,
        valid_hi: 1,
    };
    e.insert_frame(&l).unwrap();
    assert!(e.finish().is_err(), "unpaired locator cannot finish");
    let mut wrong = c.clone();
    wrong.file.class = KeyClass::RetainedControl;
    assert!(Roots::empty(&wrong).is_err());
    let mut wrong = c.clone();
    wrong.file.identity = wrong.frame.identity;
    assert!(Roots::empty(&wrong).is_err());
    let mut wrong = roots;
    std::mem::swap(&mut wrong.file, &mut wrong.frame);
    assert!(wrong.locate(&c, 0).is_err());
}

#[test]
fn actual_dictionary_inventory_cow_is_immutable_and_bounded() {
    use oojuro_store::managed_pack::stage_dictionary;
    let f = F::new();
    let c = f.contexts();
    let roots = Roots::empty(&c).unwrap();
    let mut e = Editor::new(c.clone(), roots).unwrap();
    let d = stage_dictionary(
        &f.root.join("data"),
        birth(1),
        7,
        b"actual dictionary contents for managed reuse",
        &f.cipher,
    )
    .unwrap();
    e.insert_dictionary(&d).unwrap();
    let mut s = Stage::with_dictionary(
        &f.root.join("data"),
        birth(1),
        Reservation {
            file_id: 1,
            first_physical_seq: 0,
        },
        f.cipher.clone(),
        Options::default(),
        Some(d.clone()),
    )
    .unwrap();
    let text = b"actual dictionary contents for managed reuse".repeat(200);
    s.push(
        &[NewRecord {
            kind: 1,
            valid_at: 1,
            recorded_at: 2,
            key: b"",
            attrs: b"",
            text: &text,
        }],
        &mut |l| {
            e.insert_frame(&l)
                .map_err(|_| oojuro_store::Error::Invalid("tree fixture"))
        },
    )
    .unwrap();
    let p = s.finish().unwrap();
    e.insert_pack(&p).unwrap();
    let roots = e.finish().unwrap();
    assert_eq!(roots.dictionary(&c, 7).unwrap(), Some(d.clone()));
    let mut packs = 0;
    let mut dicts = 0;
    roots
        .inventory(&c, &mut |i| {
            match i {
                Inventory::Pack(_) => packs += 1,
                Inventory::Dictionary(x) => {
                    assert_eq!(x, &d);
                    dicts += 1
                }
            }
            Ok(())
        })
        .unwrap();
    assert_eq!((packs, dicts), (1, 1));
    let mut e = Editor::new(c, roots).unwrap();
    let mut bad = d;
    bad.token = [93; 32];
    assert!(e.insert_dictionary(&bad).is_err());
    assert!(e.finish().is_err());
}

#[test]
fn candidate_cannot_introduce_pack_or_dictionary_from_other_birth_even_valid_ancestor() {
    let f = F::new();
    let c = f.contexts();
    let roots = Roots::empty(&c).unwrap();
    f.a.generation.store(2, Ordering::Relaxed);
    let c = f.contexts();
    let mut e = Editor::new(c.clone(), roots.clone()).unwrap();
    let mut s = Stage::new(
        &f.root.join("data"),
        birth(1),
        Reservation {
            file_id: 1,
            first_physical_seq: 0,
        },
        f.cipher.clone(),
        Options::default(),
    )
    .unwrap();
    s.push(
        &[NewRecord {
            kind: 1,
            valid_at: 1,
            recorded_at: 1,
            key: b"",
            attrs: b"",
            text: b"old prepared candidate",
        }],
        &mut |l| {
            e.insert_frame(&l)
                .map_err(|_| oojuro_store::Error::Invalid("fixture"))
        },
    )
    .unwrap();
    let p = s.finish().unwrap();
    assert!(e.insert_pack(&p).is_err());
    assert!(e.finish().is_err());
    let d = oojuro_store::managed_pack::stage_dictionary(
        &f.root.join("data"),
        birth(1),
        7,
        b"old prepared dictionary",
        &f.cipher,
    )
    .unwrap();
    let mut e = Editor::new(c, roots).unwrap();
    assert!(e.insert_dictionary(&d).is_err());
    assert!(e.finish().is_err());
}

#[test]
fn complete_metadata_scan_covers_orphan_packed_tmp_queue_without_key_and_current_guard() {
    let f = F::new();
    let c = f.contexts();
    let roots = Roots::empty(&c).unwrap();
    let mut e = Editor::new(c.clone(), roots).unwrap();
    f.append(&mut e, 1, 0, 1);
    let _ = e.finish().unwrap();
    let tmp = f
        .frame
        .join(format!("tmp-{}-00000000000000ab.bin", "a".repeat(64)));
    std::fs::write(&tmp, b"partial encrypted page bytes").unwrap();
    let queue = f.file.join(format!("queue-{}.bin", "b".repeat(64)));
    std::fs::write(&queue, b"").unwrap();
    let orphan = f
        .dictionary
        .join(format!("pack-{}-0000000000000000.bin", "c".repeat(64)));
    std::fs::write(&orphan, b"corrupt orphan").unwrap();
    let mut count = 0;
    let mut temporary = 0;
    scan_metadata(&c, &mut |_, a| {
        count += 1;
        let name = a.path.file_name().unwrap().to_str().unwrap();
        temporary += usize::from(name.starts_with("tmp-") || name.starts_with("queue-"));
        Ok(())
    })
    .unwrap();
    assert_eq!((count, temporary), (5, 2));
    // No DATA-key lookup/decryption occurs; a separate trusted prune guard is
    // mandatory around these raw post-denial filesystem yields.
    f.a.generation.store(999, Ordering::Relaxed);
    assert!(scan_metadata(&c, &mut |_, _| Ok(())).is_err());
    let mut raw = 0;
    tree::scan_owned_paths(&f.frame, |_| {
        raw += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(raw, 2);
    let foreign = f.frame.join("CURRENT");
    std::fs::write(&foreign, b"retained control must not be pruned").unwrap();
    assert!(tree::scan_owned_paths(&f.frame, |_| Ok(())).is_err());
    assert!(foreign.exists());
    std::fs::remove_file(foreign).unwrap();
    let link = f
        .frame
        .join(format!("pack-{}-0000000000000000.bin", "d".repeat(64)));
    std::os::unix::fs::symlink(&orphan, &link).unwrap();
    assert!(tree::scan_owned_paths(&f.frame, |_| Ok(())).is_err());
}

// Test-only diagnostics: the joined writer uses complete owned namespaces,
// not reference-only inventory; production dictionary adoption is not exposed.
enum Inventory<'a> {
    Pack(&'a PackRef),
    Dictionary(&'a DictionaryRef),
}

impl Roots {
    /// Streams complete data-file/dictionary inventory; it never authorizes unlink.
    /// Managed owner must first publish exclusion/tombstone and prune DATA trees.
    fn inventory(
        &self,
        c: &Contexts<'_>,
        visit: &mut dyn FnMut(Inventory<'_>) -> Result<()>,
    ) -> Result<()> {
        let b = self.check(c)?;
        let mut cursor = tree::Cursor::seek_ancestor(c.file.clone(), &self.file, &[])?;
        while let Some(e) = cursor.next()? {
            let p = PackRef::decode(&e.value).map_err(|_| Error::Authority)?;
            if e.key != p.file_id.to_be_bytes() {
                return Err(Error::Authority);
            }
            c.file.authority.validate_birth(
                &p.birth.encode().map_err(|_| Error::Authority)?,
                Purpose::File,
                KeyClass::Data,
            )?;
            visit(Inventory::Pack(&p))?;
        }
        let mut cursor = tree::Cursor::seek_ancestor(c.dictionary.clone(), &self.dictionary, &[])?;
        while let Some(e) = cursor.next()? {
            let d = DictionaryRef::decode(&e.value).map_err(|_| Error::Authority)?;
            if e.key != d.id.to_be_bytes() {
                return Err(Error::Authority);
            }
            c.dictionary.authority.validate_birth(
                &d.birth.encode().map_err(|_| Error::Authority)?,
                Purpose::Dictionary,
                KeyClass::Data,
            )?;
            visit(Inventory::Dictionary(&d))?;
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    /// Includes reused COW pages, not only pages born in the newest generation.
    fn metadata_pages(
        &self,
        c: &Contexts<'_>,
        visit: &mut dyn FnMut(Purpose, &tree::Reference) -> Result<()>,
    ) -> Result<()> {
        let b = self.check(c)?;
        for (root, ctx) in [
            (&self.frame, &c.frame),
            (&self.file, &c.file),
            (&self.dictionary, &c.dictionary),
        ] {
            tree::visit_ancestor_pages(ctx.clone(), root, |r: &tree::Reference| {
                visit(ctx.purpose, r)
            })?;
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
impl Editor<'_> {
    fn insert_dictionary(&mut self, d: &DictionaryRef) -> Result<()> {
        let result = (|| {
            self.check()?;
            let bytes = d.encode().map_err(|_| Error::Bounds)?;
            match self.dictionary.lookup(&d.id.to_be_bytes())? {
                Some(old) if old == bytes => self.contexts.dictionary.authority.validate_birth(
                    &d.birth.encode().map_err(|_| Error::Authority)?,
                    Purpose::Dictionary,
                    KeyClass::Data,
                ),
                Some(_) => Err(Error::Conflict),
                None => {
                    if d.birth.encode().map_err(|_| Error::Authority)? != self.binding {
                        return Err(Error::Authority);
                    }
                    self.dictionary
                        .insert_immutable(Entry::new(d.id.to_be_bytes().to_vec(), bytes)?)
                }
            }
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}
fn scan_metadata(
    c: &Contexts<'_>,
    visit: &mut dyn FnMut(Purpose, tree::Artifact) -> Result<()>,
) -> Result<()> {
    let binding = c.check()?;
    for ctx in [&c.frame, &c.file, &c.dictionary] {
        tree::scan_owned_paths(ctx.directory, |entry| {
            if c.check()? != binding {
                return Err(Error::Conflict);
            }
            visit(ctx.purpose, entry)?;
            if c.check()? != binding {
                return Err(Error::Conflict);
            }
            Ok(())
        })?;
    }
    Ok(())
}
