use super::*;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture {
    dir: PathBuf,
    cipher: FrameCipher,
    authority: Fake,
}
struct Fake {
    line: RefCell<Vec<Vec<u8>>>,
    unavailable: RefCell<bool>,
    seen: RefCell<Vec<Vec<u8>>>,
}
impl Fake {
    fn new() -> Self {
        Self {
            line: RefCell::new(vec![vec![1; 32]]),
            unavailable: RefCell::new(false),
            seen: RefCell::new(vec![]),
        }
    }
    fn advance(&self) {
        self.line.borrow_mut().push(vec![2; 32]);
    }
}
impl Authority for Fake {
    fn current(&self) -> Result<Vec<u8>> {
        if *self.unavailable.borrow() {
            return Err(Error::Unavailable);
        }
        Ok(self.line.borrow().last().unwrap().clone())
    }
    fn validate_birth(&self, birth: &[u8], _: Purpose, _: KeyClass) -> Result<()> {
        self.current()?;
        if !self.line.borrow().iter().any(|b| b == birth) {
            return Err(Error::Authority);
        }
        self.seen.borrow_mut().push(birth.to_vec());
        Ok(())
    }
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::current_dir()
            .unwrap()
            .join(".scratch")
            .join(format!(
                "record-tree-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            cipher: FrameCipher::new(&[91; 32]),
            authority: Fake::new(),
        }
    }
    fn ctx(&self) -> Context<'_> {
        Context {
            directory: &self.dir,
            cipher: &self.cipher,
            authority: &self.authority,
            purpose: Purpose::Record,
            identity: [92; 32],
            class: KeyClass::Data,
            sort_of: None,
            cache: None,
            cache_durability: CacheDurability::Durable,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn entry(i: u64) -> Entry {
    Entry::new(
        i.to_be_bytes().to_vec(),
        format!("synthetic-private-value-{i}").into_bytes(),
    )
    .unwrap()
}
#[test]
fn packed_sorted_build_point_seek_and_complete_cursor_do_not_need_population_metadata() {
    let f = Fixture::new();
    let mut b = Builder::new(f.ctx()).unwrap();
    for i in 0..5000 {
        b.push(entry(i)).unwrap();
    }
    let root = b.finish().unwrap();
    assert_eq!(root.count, 5000);
    assert_eq!(
        get(f.ctx(), &root, &2500u64.to_be_bytes()).unwrap(),
        Some(entry(2500).value)
    );
    assert!(
        get(f.ctx(), &root, &6000u64.to_be_bytes())
            .unwrap()
            .is_none()
    );
    let mut c = Cursor::seek(f.ctx(), &root, &1998u64.to_be_bytes()).unwrap();
    for i in 1998..5000 {
        assert_eq!(c.next().unwrap(), Some(entry(i)));
    }
    assert_eq!(c.next().unwrap(), None);
    let files: Vec<_> = fs::read_dir(&f.dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let bytes = fs::read(&files[0]).unwrap();
    assert!(!bytes.windows(23).any(|b| b == b"synthetic-private-value"));
}
#[test]
fn cow_preserves_birth_bindings_and_refuses_same_number_fork_or_midread_advance() {
    let f = Fixture::new();
    let mut b = Builder::new(f.ctx()).unwrap();
    for i in 0..500 {
        b.push(entry(i)).unwrap();
    }
    let root = b.finish().unwrap();
    let mut old_cursor = Cursor::seek(f.ctx(), &root, &[]).unwrap();
    assert!(old_cursor.next().unwrap().is_some());
    f.authority.advance();
    assert!(old_cursor.next().is_err());
    assert!(get(f.ctx(), &root, &0u64.to_be_bytes()).is_err());
    let mut edit = Editor::new(f.ctx(), root.clone()).unwrap();
    for i in 500..900 {
        edit.insert(entry(i)).unwrap();
    }
    let next = edit.finish().unwrap();
    assert_eq!(next.count, 900);
    let mut c = Cursor::seek(f.ctx(), &next, &[]).unwrap();
    for i in 0..900 {
        assert_eq!(c.next().unwrap(), Some(entry(i)));
    }
    assert!(f.authority.seen.borrow().iter().any(|b| b == &vec![1; 32]));
    assert!(f.authority.seen.borrow().iter().any(|b| b == &vec![2; 32]));
    let files = fs::read_dir(&f.dir).unwrap().count();
    assert_eq!(files, 2, "updates pack many pages into one file");
    f.authority.line.replace(vec![vec![3; 32]]);
    assert!(Editor::new(f.ctx(), next).is_err());
}
#[test]
fn external_sort_spills_many_runs_merges_bounded_groups_and_refuses_cross_run_duplicate() {
    let f = Fixture::new();
    let mut sorter = Sorter::new(f.ctx()).unwrap();
    sorter.budget_for_test(1024);
    for i in (0..2000).rev() {
        sorter.push(entry(i)).unwrap();
    }
    let root = sorter.finish().unwrap();
    assert_eq!(root.count, 2000);
    let mut c = Cursor::seek(f.ctx(), &root, &[]).unwrap();
    for i in 0..2000 {
        assert_eq!(c.next().unwrap(), Some(entry(i)));
    }
    assert!(c.next().unwrap().is_none());
    assert_eq!(
        fs::read_dir(&f.dir).unwrap().count(),
        1,
        "temporary queues/runs removed after durable output"
    );
    let mut duplicate = Sorter::new(f.ctx()).unwrap();
    duplicate.budget_for_test(256);
    for i in 0..100 {
        duplicate.push(entry(i)).unwrap();
    }
    duplicate.push(entry(0)).unwrap();
    assert!(matches!(duplicate.finish(), Err(Error::Conflict)));
}
#[test]
fn metadata_purposes_key_classes_bounds_and_corruption_fail_closed() {
    let f = Fixture::new();
    let mut b = Builder::new(f.ctx()).unwrap();
    b.push(entry(1)).unwrap();
    assert!(b.push(entry(1)).is_err());
    let root = b.finish().unwrap();
    let mut wrong = f.ctx();
    wrong.purpose = Purpose::KeyVersion;
    assert!(get(wrong.clone(), &root, &1u64.to_be_bytes()).is_err());
    wrong.class = KeyClass::RetainedControl;
    assert!(wrong.check().is_err());
    let mut allowed = f.ctx();
    allowed.purpose = Purpose::Request;
    allowed.class = KeyClass::RetainedControl;
    assert!(allowed.check().is_ok());
    assert!(allowed.for_sort().unwrap().check().is_ok());
    assert!(Entry::new(vec![0; 65], vec![]).is_err());
    assert!(Entry::new(vec![1], vec![0; MAX_VALUE + 1]).is_err());
    let node = root.node.as_ref().unwrap();
    let path = path(&f.dir, node.generation, node.file, false);
    let mut bytes = fs::read(&path).unwrap();
    bytes[node.offset as usize + 10] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(get(f.ctx(), &root, &1u64.to_be_bytes()).is_err());
    *f.authority.unavailable.borrow_mut() = true;
    assert!(get(f.ctx(), &root, &1u64.to_be_bytes()).is_err());
}
#[test]
fn codec_rejects_truncation_overflow_padding_and_unbounded_ref_claims() {
    let leaf = Node::Leaf(vec![entry(1), entry(2)]);
    let bytes = encode_node(&leaf, 0).unwrap();
    for n in 0..bytes.len() {
        assert!(decode_node(&bytes[..n], 0).is_err());
    }
    let mut bad = bytes.clone();
    bad[10] = 1;
    assert!(decode_node(&bad, 0).is_err());
    let mut bad = bytes.clone();
    bad.extend([0]);
    assert!(decode_node(&bad, 0).is_err());
    assert!(decode_node(&bytes, 17).is_err());
    let f = Fixture::new();
    let mut b = Builder::new(f.ctx()).unwrap();
    b.push(entry(1)).unwrap();
    let root = b.finish().unwrap();
    let mut r = root.node.unwrap();
    r.bytes = u32::MAX;
    assert!(check_ref(&r).is_err());
}

#[test]
fn bounded_page_cache_cannot_reuse_plaintext_under_a_different_cipher() {
    let f = Fixture::new();
    let cipher = Arc::new(FrameCipher::new(&[93; 32]));
    let cache = PageCache::new(cipher.clone());
    let mut ctx = f.ctx();
    ctx.cipher = &cipher;
    ctx.cache = Some(&cache);
    let mut b = Builder::new(ctx.clone()).unwrap();
    for i in 0u64..6000 {
        b.push(Entry::new(i.to_be_bytes().to_vec(), vec![i as u8; 1024]).unwrap())
            .unwrap();
    }
    let root = b.finish().unwrap();
    let mut c = Cursor::seek(ctx.clone(), &root, &[]).unwrap();
    for _ in 0..6000 {
        assert!(c.next().unwrap().is_some());
    }
    assert!(cache.accounted_bytes().unwrap() <= 4 << 20);
    assert!(cache.accounted_bytes().unwrap() > 0);
    let other = FrameCipher::new(&[94; 32]);
    let mut wrong = ctx;
    wrong.cipher = &other;
    assert!(get(wrong, &root, &0u64.to_be_bytes()).is_err());
}

#[test]
fn bounded_batch_editor_matches_sorted_map_and_preserves_ancestor_pages() {
    let f = Fixture::new();
    let mut b = Builder::new(f.ctx()).unwrap();
    for n in (0..6000).step_by(2) {
        b.push(entry(n)).unwrap();
    }
    let old = b.finish().unwrap();
    f.authority.advance();
    let mut edit = Editor::new(f.ctx(), old.clone()).unwrap();
    for start in (1..6000).step_by(2000) {
        let mut batch: Vec<_> = (start..start + 2000).step_by(2).map(entry).collect();
        batch.reverse();
        edit.insert_batch(batch).unwrap();
    }
    let current = edit.finish().unwrap();
    assert_eq!(current.count, 6000);
    let mut cursor = Cursor::seek(f.ctx(), &current, &[]).unwrap();
    for n in 0..6000 {
        assert_eq!(cursor.next().unwrap().unwrap().key, entry(n).key);
    }
    assert!(cursor.next().unwrap().is_none());
    let mut old_cursor = Cursor::seek_ancestor(f.ctx(), &old, &[]).unwrap();
    for n in (0..6000).step_by(2) {
        assert_eq!(old_cursor.next().unwrap().unwrap().key, entry(n).key);
    }
    assert!(old_cursor.next().unwrap().is_none());
    let mut edit = Editor::new(f.ctx(), current).unwrap();
    assert_eq!(edit.insert_batch(vec![entry(2)]), Err(Error::Conflict));
    assert_eq!(
        edit.insert_batch(vec![entry(7000), entry(7000)]),
        Err(Error::Conflict)
    );
    assert_eq!(
        edit.insert_batch((7000..8001).map(entry).collect()),
        Err(Error::Bounds)
    );
}

#[test]
fn cache_mode_is_bound_to_roots_pages_and_plaintext_reuse_and_cannot_weaken_control() {
    let f = Fixture::new();
    let cipher = Arc::new(FrameCipher::new(&[99; 32]));
    let cache = PageCache::new(cipher.clone());
    let mut weak = f.ctx();
    weak.cipher = &cipher;
    weak.cache = Some(&cache);
    weak.cache_durability = CacheDurability::Rebuildable;
    let mut builder = Builder::new(weak.clone()).unwrap();
    builder.push(entry(1)).unwrap();
    let root = builder.finish().unwrap();
    assert!(
        get(weak.clone(), &root, &1u64.to_be_bytes())
            .unwrap()
            .is_some()
    );
    let mut strong = weak.clone();
    strong.cache_durability = CacheDurability::Durable;
    assert!(root.check(&strong).is_err());
    let mut forged = root.clone();
    forged.cache_durability = CacheDurability::Durable;
    assert!(get(strong, &forged, &1u64.to_be_bytes()).is_err());
    weak.class = KeyClass::RetainedControl;
    weak.purpose = Purpose::Prefix;
    assert_eq!(weak.check(), Err(Error::Scope));
}
