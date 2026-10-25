//! Private bounded encrypted metadata B+tree. Never an authority or a second WAL.
//! Ref birth bindings are authenticated by parents and independently checked as ancestors.
mod sort;
use super::super::*;
use oojuro_store::crypto::{FrameCipher, SEAL_OVERHEAD, random_key};
pub(crate) use sort::Sorter;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub(crate) const PAGE_BYTES: usize = 64 << 10;
const PACK_BYTES: u64 = 64 << 20;
const MAX_CHILDREN: usize = 128;
const MAX_DEPTH: u8 = 16;
const MAX_BINDING: usize = 2048;
const MAX_KEY: usize = 64;
const MAX_VALUE: usize = 32 << 10;
const MAGIC: &[u8; 8] = b"OJDIR001";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Purpose {
    Prefix,
    Request,
    Record,
    KeyVersion,
    Segment,
    Sort,
    ProjectionMetadata,
    ProjectionIds,
    ProjectionKeys,
    ProjectionPostings,
    ProjectionTerms,
}
impl Purpose {
    fn byte(self) -> u8 {
        match self {
            Self::Prefix => 1,
            Self::Request => 2,
            Self::Record => 3,
            Self::KeyVersion => 4,
            Self::Segment => 5,
            Self::Sort => 6,
            Self::ProjectionMetadata => 7,
            Self::ProjectionIds => 8,
            Self::ProjectionKeys => 9,
            Self::ProjectionPostings => 10,
            Self::ProjectionTerms => 11,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyClass {
    Data,
    RetainedControl,
}
/// Trusted authority adapter, never a wire-selected authenticated bool.
pub(crate) trait Authority {
    fn current(&self) -> Result<Vec<u8>>;
    fn matches_current(&self, expected: &[u8]) -> Result<bool> {
        Ok(self.current()? == expected)
    }
    fn validate_birth(&self, birth: &[u8], purpose: Purpose, class: KeyClass) -> Result<()>;
    /// Optional disposable-cache admission. Source authorities keep their
    /// existing write behavior through these default implementations.
    fn reserve_file(&self, _path: &Path, _length: u64) -> Result<()> {
        Ok(())
    }
    fn rename_file(&self, source: &Path, target: &Path) -> Result<()> {
        fs::rename(source, target).map_err(|_| Error::Unavailable)
    }
    fn remove_file(&self, path: &Path) -> Result<()> {
        fs::remove_file(path).map_err(|_| Error::Unavailable)
    }
    fn sync_file(&self, file: &File, _purpose: Purpose) -> Result<()> {
        file.sync_all().map_err(|_| Error::Unavailable)
    }
    fn sync_directory(&self, directory: &Path) -> Result<()> {
        sync_dir(directory)
    }
}
#[derive(Clone)]
pub(crate) struct Context<'a> {
    pub directory: &'a Path,
    pub cipher: &'a FrameCipher,
    pub authority: &'a dyn Authority,
    pub purpose: Purpose,
    pub identity: [u8; 32],
    pub class: KeyClass,
    pub sort_of: Option<Purpose>,
    pub cache: Option<&'a PageCache>,
}
impl<'a> Context<'a> {
    fn key_bound(&self) -> usize {
        match self.sort_of.unwrap_or(self.purpose) {
            Purpose::ProjectionIds | Purpose::ProjectionKeys => 1024,
            _ => MAX_KEY,
        }
    }
    pub fn check(&self) -> Result<Vec<u8>> {
        let purpose = if self.purpose == Purpose::Sort {
            self.sort_of.ok_or(Error::Scope)?
        } else {
            if self.sort_of.is_some() {
                return Err(Error::Scope);
            }
            self.purpose
        };
        if self.class == KeyClass::RetainedControl
            && !matches!(purpose, Purpose::Prefix | Purpose::Request)
        {
            return Err(Error::Scope);
        }
        let binding = self.authority.current()?;
        if binding.is_empty() || binding.len() > MAX_BINDING {
            return Err(Error::Bounds);
        }
        Ok(binding)
    }
    pub fn for_sort(&self) -> Result<Self> {
        self.check()?;
        let mut ctx = self.clone();
        ctx.sort_of = Some(self.purpose);
        ctx.purpose = Purpose::Sort;
        Ok(ctx)
    }
}
/// Bounded decoded metadata cache bound to one owned immutable cipher instance.
/// A new key/cipher object cannot reuse entries even if caller metadata is copied.
pub(crate) struct PageCache {
    cipher: Arc<FrameCipher>,
    state: Mutex<CacheState>,
}
struct CacheState {
    entries: VecDeque<([u8; 32], Node, usize)>,
    bytes: usize,
}
impl PageCache {
    pub fn new(cipher: Arc<FrameCipher>) -> Self {
        Self {
            cipher,
            state: Mutex::new(CacheState {
                entries: VecDeque::new(),
                bytes: 0,
            }),
        }
    }
    fn key(ctx: &Context<'_>, r: &Reference) -> Result<[u8; 32]> {
        let mut h = blake3::Hasher::new();
        h.update(&aad(ctx, r)?);
        h.update(&r.digest);
        Ok(*h.finalize().as_bytes())
    }
    fn get(&self, ctx: &Context<'_>, r: &Reference) -> Result<Option<Node>> {
        if !std::ptr::eq(self.cipher.as_ref(), ctx.cipher) {
            return Ok(None);
        }
        let key = Self::key(ctx, r)?;
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if let Some(at) = state.entries.iter().position(|(k, _, _)| *k == key) {
            let entry = state.entries.remove(at).unwrap();
            let node = entry.1.clone();
            state.entries.push_back(entry);
            return Ok(Some(node));
        }
        Ok(None)
    }
    fn put(&self, ctx: &Context<'_>, r: &Reference, node: &Node) -> Result<()> {
        if !std::ptr::eq(self.cipher.as_ref(), ctx.cipher) {
            return Ok(());
        }
        let bytes = node_accounted(node)?;
        if bytes > 4 << 20 {
            return Ok(());
        }
        let key = Self::key(ctx, r)?;
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if let Some(at) = state.entries.iter().position(|(k, _, _)| *k == key) {
            let old = state.entries.remove(at).unwrap();
            state.bytes -= old.2;
        }
        state.bytes = state.bytes.checked_add(bytes).ok_or(Error::Bounds)?;
        state.entries.push_back((key, node.clone(), bytes));
        while state.bytes > 4 << 20 {
            let old = state.entries.pop_front().unwrap();
            state.bytes -= old.2;
        }
        Ok(())
    }
    pub fn accounted_bytes(&self) -> Result<usize> {
        Ok(self.state.lock().map_err(|_| Error::Unavailable)?.bytes)
    }
}
fn node_accounted(node: &Node) -> Result<usize> {
    match node {
        Node::Leaf(rows) => {
            rows.iter()
                .try_fold(rows.capacity() * std::mem::size_of::<Entry>(), |n, e| {
                    n.checked_add(e.key.capacity())
                        .and_then(|n| n.checked_add(e.value.capacity()))
                        .ok_or(Error::Bounds)
                })
        }
        Node::Branch(rows) => rows.iter().try_fold(
            rows.capacity() * std::mem::size_of::<Reference>(),
            |n, r| {
                n.checked_add(r.birth.capacity())
                    .and_then(|n| n.checked_add(r.first.capacity()))
                    .and_then(|n| n.checked_add(r.last.capacity()))
                    .ok_or(Error::Bounds)
            },
        ),
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}
impl Entry {
    pub fn new(key: Vec<u8>, value: Vec<u8>) -> Result<Self> {
        if key.is_empty() || key.len() > MAX_KEY || value.len() > MAX_VALUE {
            return Err(Error::Bounds);
        }
        Ok(Self { key, value })
    }
    pub fn accounted(&self) -> usize {
        self.key.len() + self.value.len() + 32
    }
}
fn validate_entry(entry: &Entry, key_bound: usize) -> Result<()> {
    if entry.key.is_empty() || entry.key.len() > key_bound || entry.value.len() > MAX_VALUE {
        return Err(Error::Bounds);
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reference {
    pub generation: [u8; 32],
    pub file: u64,
    pub offset: u64,
    pub bytes: u32,
    pub digest: [u8; 32],
    pub birth: Vec<u8>,
    pub level: u8,
    pub count: u64,
    pub first: Vec<u8>,
    pub last: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Root {
    pub format: u32,
    pub purpose: Purpose,
    pub binding: Vec<u8>,
    pub node: Option<Reference>,
    pub count: u64,
    pub fresh: Option<[u8; 32]>,
}
impl Root {
    pub fn check(&self, ctx: &Context<'_>) -> Result<()> {
        if self.format != 1
            || self.purpose != ctx.purpose
            || self.binding != ctx.check()?
            || self.node.as_ref().map_or(0, |n| n.count) != self.count
        {
            return Err(Error::Authority);
        }
        if let Some(node) = &self.node {
            check_ref_bound(node, ctx.key_bound())?;
        }
        Ok(())
    }
}
#[derive(Clone)]
enum Node {
    Leaf(Vec<Entry>),
    Branch(Vec<Reference>),
}
fn check_ref_bound(r: &Reference, key_bound: usize) -> Result<()> {
    if r.bytes as usize > PAGE_BYTES + SEAL_OVERHEAD
        || r.bytes as usize <= SEAL_OVERHEAD
        || r.birth.is_empty()
        || r.birth.len() > MAX_BINDING
        || r.level > MAX_DEPTH
        || r.count == 0
        || r.first.is_empty()
        || r.first.len() > key_bound
        || r.last.len() > key_bound
        || r.first > r.last
        || r.offset
            .checked_add(u64::from(r.bytes))
            .is_none_or(|n| n > PACK_BYTES)
    {
        return Err(Error::Bounds);
    }
    Ok(())
}
fn aad(ctx: &Context<'_>, r: &Reference) -> Result<Vec<u8>> {
    let mut bytes = b"oojuro/record-directory/page/v1\0".to_vec();
    bytes.push(ctx.purpose.byte());
    bytes.push(ctx.sort_of.map_or(0, Purpose::byte));
    bytes.push(u8::from(ctx.class == KeyClass::RetainedControl));
    bytes.extend(ctx.identity);
    bytes.extend(encode_ref(r, ctx.key_bound())?);
    Ok(bytes)
}
fn path(dir: &Path, generation: [u8; 32], file: u64, temp: bool) -> PathBuf {
    dir.join(format!(
        "{}-{}-{file:016x}.bin",
        if temp { "tmp" } else { "pack" },
        hex(&generation)
    ))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::Unavailable)
}
pub(crate) fn prepare_directory(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => return Err(Error::Invalid),
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|_| Error::Unavailable)?
        }
        Err(_) => return Err(Error::Unavailable),
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Unavailable)
}
fn file_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)
}
pub(crate) struct Pack<'a> {
    ctx: Context<'a>,
    generation: [u8; 32],
    number: u64,
    current: Option<File>,
    offset: u64,
    binding: Vec<u8>,
    poisoned: bool,
}
impl<'a> Pack<'a> {
    pub fn new(ctx: Context<'a>) -> Result<Self> {
        let binding = ctx.check()?;
        Ok(Self {
            ctx,
            generation: random_key(),
            number: 0,
            current: None,
            offset: 0,
            binding,
            poisoned: false,
        })
    }
    fn close(&mut self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Unavailable);
        }
        if let Some(file) = self.current.take() {
            self.poisoned = true;
            self.ctx.authority.sync_file(&file, self.ctx.purpose)?;
            self.ctx.authority.rename_file(
                &path(self.ctx.directory, self.generation, self.number, true),
                &path(self.ctx.directory, self.generation, self.number, false),
            )?;
            self.ctx.authority.sync_directory(self.ctx.directory)?;
            #[cfg(test)]
            test_phase("PackPublished");
            self.poisoned = false;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        if self.ctx.check()? != self.binding {
            return Err(Error::Conflict);
        }
        self.close()
    }
    fn load(&self, r: &Reference) -> Result<Node> {
        if r.generation == self.generation && r.file == self.number && self.current.is_some() {
            check_ref_bound(r, self.ctx.key_bound())?;
            self.ctx.authority.validate_birth(
                &r.birth,
                self.ctx.sort_of.unwrap_or(self.ctx.purpose),
                self.ctx.class,
            )?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path(self.ctx.directory, r.generation, r.file, true))
                .map_err(|_| Error::Unavailable)?;
            let mut sealed = vec![0; r.bytes as usize];
            file.read_exact_at(&mut sealed, r.offset)
                .map_err(|_| Error::Unavailable)?;
            if *blake3::hash(&sealed).as_bytes() != r.digest {
                return Err(Error::Authority);
            }
            let mut header = r.clone();
            header.digest = [0; 32];
            let plain = self
                .ctx
                .cipher
                .open_frame(&aad(&self.ctx, &header)?, &sealed)
                .map_err(|_| Error::Authority)?;
            let node = decode_node_bound(&plain, r.level, self.ctx.key_bound())?;
            let (first, last, count) = node_bounds(&node)?;
            if first != r.first || last != r.last || count != r.count {
                return Err(Error::Authority);
            }
            Ok(node)
        } else {
            load(&self.ctx, r)
        }
    }
    fn put(&mut self, node: &Node, level: u8) -> Result<Reference> {
        if self.poisoned {
            return Err(Error::Unavailable);
        }
        let plain = encode_node_bound(node, level, self.ctx.key_bound())?;
        let (first, last, count) = node_bounds(node)?;
        if self.ctx.check()? != self.binding {
            return Err(Error::Conflict);
        }
        let birth = self.binding.clone();
        let size = plain.len() + SEAL_OVERHEAD;
        if self
            .offset
            .checked_add(size as u64)
            .is_none_or(|n| n > PACK_BYTES)
        {
            self.close()?;
            self.number = self.number.checked_add(1).ok_or(Error::Bounds)?;
            self.offset = 0;
        }
        if self.current.is_none() {
            self.current = Some(file_new(&path(
                self.ctx.directory,
                self.generation,
                self.number,
                true,
            ))?);
        }
        let mut r = Reference {
            generation: self.generation,
            file: self.number,
            offset: self.offset,
            bytes: size as u32,
            digest: [0; 32],
            birth,
            level,
            count,
            first,
            last,
        };
        let sealed = self.ctx.cipher.seal_frame(&aad(&self.ctx, &r)?, &plain);
        r.digest = *blake3::hash(&sealed).as_bytes();
        self.ctx.authority.reserve_file(
            &path(self.ctx.directory, self.generation, self.number, true),
            self.offset
                .checked_add(sealed.len() as u64)
                .ok_or(Error::Bounds)?,
        )?;
        self.poisoned = true;
        self.current
            .as_mut()
            .unwrap()
            .write_all(&sealed)
            .map_err(|_| Error::Unavailable)?;
        self.offset += sealed.len() as u64;
        self.poisoned = false;
        #[cfg(test)]
        test_phase("PackTemp");
        Ok(r)
    }
}
fn load(ctx: &Context<'_>, r: &Reference) -> Result<Node> {
    check_ref_bound(r, ctx.key_bound())?;
    ctx.authority
        .validate_birth(&r.birth, ctx.sort_of.unwrap_or(ctx.purpose), ctx.class)?;
    if let Some(cache) = ctx.cache
        && let Some(node) = cache.get(ctx, r)?
    {
        return Ok(node);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path(ctx.directory, r.generation, r.file, false))
        .map_err(|_| Error::Unavailable)?;
    let meta = file.metadata().map_err(|_| Error::Unavailable)?;
    if !meta.is_file()
        || meta.len() > PACK_BYTES
        || r.offset
            .checked_add(u64::from(r.bytes))
            .is_none_or(|n| n > meta.len())
    {
        return Err(Error::Bounds);
    }
    let mut sealed = vec![0; r.bytes as usize];
    file.read_exact_at(&mut sealed, r.offset)
        .map_err(|_| Error::Unavailable)?;
    if *blake3::hash(&sealed).as_bytes() != r.digest {
        return Err(Error::Authority);
    }
    let mut context = r.clone();
    context.digest = [0; 32];
    let plain = ctx
        .cipher
        .open_frame(&aad(ctx, &context)?, &sealed)
        .map_err(|_| Error::Authority)?;
    let node = decode_node_bound(&plain, r.level, ctx.key_bound())?;
    let (first, last, count) = node_bounds(&node)?;
    if first != r.first || last != r.last || count != r.count {
        return Err(Error::Authority);
    }
    if let Some(cache) = ctx.cache {
        cache.put(ctx, r, &node)?;
    }
    Ok(node)
}
pub(crate) struct Builder<'a> {
    pack: Pack<'a>,
    leaf: Vec<Entry>,
    levels: Vec<Vec<Reference>>,
    last: Option<Vec<u8>>,
    count: u64,
}
impl<'a> Builder<'a> {
    pub fn new(ctx: Context<'a>) -> Result<Self> {
        Ok(Self {
            pack: Pack::new(ctx)?,
            leaf: vec![],
            levels: vec![],
            last: None,
            count: 0,
        })
    }
    pub fn push(&mut self, entry: Entry) -> Result<()> {
        validate_entry(&entry, self.pack.ctx.key_bound())?;
        if self.last.as_ref().is_some_and(|old| *old >= entry.key) {
            return Err(Error::Conflict);
        }
        let mut candidate = self.leaf.clone();
        candidate.push(entry.clone());
        if self.leaf.len() >= MAX_CHILDREN
            || encode_node_bound(&Node::Leaf(candidate), 0, self.pack.ctx.key_bound()).is_err()
        {
            self.flush_leaf()?;
        }
        self.last = Some(entry.key.clone());
        self.leaf.push(entry);
        self.count = self.count.checked_add(1).ok_or(Error::Bounds)?;
        Ok(())
    }
    fn flush_leaf(&mut self) -> Result<()> {
        if !self.leaf.is_empty() {
            let node = Node::Leaf(std::mem::take(&mut self.leaf));
            let r = self.pack.put(&node, 0)?;
            self.parent(0, r)?;
        }
        Ok(())
    }
    fn parent(&mut self, at: usize, r: Reference) -> Result<()> {
        if at >= MAX_DEPTH as usize {
            return Err(Error::Bounds);
        }
        while self.levels.len() <= at {
            self.levels.push(vec![]);
        }
        let mut candidate = self.levels[at].clone();
        candidate.push(r.clone());
        if !self.levels[at].is_empty()
            && (candidate.len() > MAX_CHILDREN
                || encode_node_bound(
                    &Node::Branch(candidate),
                    at as u8 + 1,
                    self.pack.ctx.key_bound(),
                )
                .is_err())
        {
            let node = Node::Branch(std::mem::take(&mut self.levels[at]));
            let parent = self.pack.put(&node, at as u8 + 1)?;
            self.parent(at + 1, parent)?;
        }
        self.levels[at].push(r);
        Ok(())
    }
    pub fn finish(mut self) -> Result<Root> {
        self.flush_leaf()?;
        let mut at = 0;
        let node = loop {
            if at >= self.levels.len() {
                break None;
            }
            if self.levels[at].is_empty() {
                at += 1;
                continue;
            }
            let only =
                self.levels[at].len() == 1 && self.levels.iter().skip(at + 1).all(Vec::is_empty);
            if only {
                break self.levels[at].pop();
            }
            let node = Node::Branch(std::mem::take(&mut self.levels[at]));
            let parent = self.pack.put(&node, at as u8 + 1)?;
            self.parent(at + 1, parent)?;
            at += 1;
        };
        let root = Root {
            format: 1,
            purpose: self.pack.ctx.purpose,
            binding: self.pack.binding.clone(),
            node,
            count: self.count,
            fresh: Some(self.pack.generation),
        };
        root.check(&self.pack.ctx)?;
        self.pack.finish()?;
        Ok(root)
    }
}
pub(crate) struct Cursor<'a> {
    ctx: Context<'a>,
    stack: Vec<(Reference, usize)>,
    leaf: VecDeque<Entry>,
    fresh: Option<[u8; 32]>,
    binding: Vec<u8>,
}
impl<'a> Cursor<'a> {
    pub fn seek(ctx: Context<'a>, root: &Root, key: &[u8]) -> Result<Self> {
        root.check(&ctx)?;
        let mut cursor = Self {
            ctx,
            stack: vec![],
            leaf: VecDeque::new(),
            fresh: root.fresh,
            binding: root.binding.clone(),
        };
        if let Some(r) = &root.node {
            cursor.descend(r.clone(), Some(key))?;
        }
        Ok(cursor)
    }
    pub fn seek_ancestor(ctx: Context<'a>, root: &Root, key: &[u8]) -> Result<Self> {
        validate_ancestor_root(&ctx, root)?;
        let binding = ctx.check()?;
        let mut cursor = Self {
            ctx,
            stack: vec![],
            leaf: VecDeque::new(),
            fresh: root.fresh,
            binding,
        };
        if let Some(r) = &root.node {
            cursor.descend(r.clone(), Some(key))?;
        }
        Ok(cursor)
    }
    fn descend(&mut self, mut reference: Reference, low: Option<&[u8]>) -> Result<()> {
        loop {
            if self.fresh.is_some_and(|g| g != reference.generation) {
                return Err(Error::Authority);
            }
            match load(&self.ctx, &reference)? {
                Node::Leaf(rows) => {
                    self.leaf = rows
                        .into_iter()
                        .filter(|e| low.is_none_or(|k| e.key.as_slice() >= k))
                        .collect();
                    return Ok(());
                }
                Node::Branch(children) => {
                    let at = low.map_or(0, |k| children.partition_point(|r| r.last.as_slice() < k));
                    if at >= children.len() {
                        return Ok(());
                    }
                    let child = children[at].clone();
                    self.stack.push((reference, at + 1));
                    if self.stack.len() > MAX_DEPTH as usize {
                        return Err(Error::Bounds);
                    }
                    reference = child;
                }
            }
        }
    }
    pub fn next(&mut self) -> Result<Option<Entry>> {
        loop {
            if !self.ctx.authority.matches_current(&self.binding)? {
                return Err(Error::Conflict);
            }
            if let Some(entry) = self.leaf.pop_front() {
                return Ok(Some(entry));
            }
            let Some((parent, next)) = self.stack.pop() else {
                return Ok(None);
            };
            let Node::Branch(children) = load(&self.ctx, &parent)? else {
                return Err(Error::Authority);
            };
            if next < children.len() {
                let child = children[next].clone();
                self.stack.push((parent, next + 1));
                self.descend(child, None)?;
            }
        }
    }
}
pub(crate) fn visit_pages(
    ctx: Context<'_>,
    root: &Root,
    mut visit: impl FnMut(&Reference) -> Result<()>,
) -> Result<()> {
    root.check(&ctx)?;
    let mut pending = Vec::new();
    if let Some(node) = &root.node {
        pending.push(node.clone());
    }
    while let Some(reference) = pending.pop() {
        if ctx.check()? != root.binding {
            return Err(Error::Conflict);
        }
        let node = load(&ctx, &reference)?;
        visit(&reference)?;
        if let Node::Branch(children) = node {
            pending.extend(children.into_iter().rev());
            if pending.len() > usize::from(MAX_DEPTH) * MAX_CHILDREN + 1 {
                return Err(Error::Bounds);
            }
        }
    }
    Ok(())
}
pub(crate) fn get(ctx: Context<'_>, root: &Root, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut c = Cursor::seek(ctx, root, key)?;
    Ok(c.next()?.filter(|e| e.key == key).map(|e| e.value))
}
/// Counts authenticated entries in a prefix using covered subtree counts.
/// This is an allocation bound; consumers still validate every selected page.
pub(crate) fn count_prefix(ctx: Context<'_>, root: &Root, prefix: &[u8]) -> Result<u64> {
    root.check(&ctx)?;
    if prefix.len() > ctx.key_bound() {
        return Err(Error::Bounds);
    }
    let mut upper = prefix.to_vec();
    let end = loop {
        match upper.pop() {
            Some(255) => {}
            Some(byte) => {
                upper.push(byte + 1);
                break Some(upper);
            }
            None => break None,
        }
    };
    fn count(
        ctx: &Context<'_>,
        reference: &Reference,
        low: &[u8],
        high: Option<&[u8]>,
        binding: &[u8],
    ) -> Result<u64> {
        if !ctx.authority.matches_current(binding)? {
            return Err(Error::Conflict);
        }
        if reference.last.as_slice() < low
            || high.is_some_and(|high| reference.first.as_slice() >= high)
        {
            return Ok(0);
        }
        if reference.first.as_slice() >= low
            && high.is_none_or(|high| reference.last.as_slice() < high)
        {
            return Ok(reference.count);
        }
        match load(ctx, reference)? {
            Node::Leaf(rows) => Ok(rows
                .iter()
                .filter(|row| {
                    row.key.as_slice() >= low && high.is_none_or(|high| row.key.as_slice() < high)
                })
                .count() as u64),
            Node::Branch(children) => children.iter().try_fold(0u64, |total, child| {
                total
                    .checked_add(count(ctx, child, low, high, binding)?)
                    .ok_or(Error::Bounds)
            }),
        }
    }
    let count = root.node.as_ref().map_or(Ok(0), |node| {
        count(&ctx, node, prefix, end.as_deref(), &root.binding)
    })?;
    if !ctx.authority.matches_current(&root.binding)? {
        return Err(Error::Conflict);
    }
    Ok(count)
}
pub(crate) fn get_ancestor(ctx: Context<'_>, root: &Root, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut c = Cursor::seek_ancestor(ctx, root, key)?;
    Ok(c.next()?.filter(|e| e.key == key).map(|e| e.value))
}
pub(crate) fn validate_ancestor_root(ctx: &Context<'_>, root: &Root) -> Result<()> {
    ctx.check()?;
    if root.format != 1
        || root.purpose != ctx.purpose
        || root.node.as_ref().map_or(0, |n| n.count) != root.count
    {
        return Err(Error::Authority);
    }
    ctx.authority
        .validate_birth(&root.binding, ctx.sort_of.unwrap_or(ctx.purpose), ctx.class)
}
pub(crate) struct Editor<'a> {
    pack: Pack<'a>,
    root: Root,
}
impl<'a> Editor<'a> {
    pub fn new(ctx: Context<'a>, root: Root) -> Result<Self> {
        validate_ancestor_root(&ctx, &root)?;
        Ok(Self {
            pack: Pack::new(ctx)?,
            root,
        })
    }
    pub fn insert(&mut self, entry: Entry) -> Result<()> {
        validate_entry(&entry, self.pack.ctx.key_bound())?;
        let refs = if let Some(r) = &self.root.node {
            insert_at(&mut self.pack, r, entry)?
        } else {
            vec![self.pack.put(&Node::Leaf(vec![entry]), 0)?]
        };
        let r = if refs.len() == 1 {
            refs[0].clone()
        } else {
            let level = refs[0].level.checked_add(1).ok_or(Error::Bounds)?;
            self.pack.put(&Node::Branch(refs), level)?
        };
        self.root = Root {
            format: 1,
            purpose: self.pack.ctx.purpose,
            binding: self.pack.ctx.check()?,
            count: r.count,
            node: Some(r),
            fresh: None,
        };
        Ok(())
    }
    /// Adjacent keys around an absent key, including unpublished edits. This
    /// keeps incremental opaque-ID order labels independent of population size.
    pub fn neighbors(&self, key: &[u8]) -> Result<(Option<Entry>, Option<Entry>)> {
        if key.is_empty() || key.len() > self.pack.ctx.key_bound() {
            return Err(Error::Bounds);
        }
        self.root
            .node
            .as_ref()
            .map_or(Ok((None, None)), |node| neighbors_at(&self.pack, node, key))
    }
    pub fn finish(mut self) -> Result<Root> {
        self.root.binding = self.pack.binding.clone();
        self.pack.finish()?;
        Ok(self.root)
    }
}
fn extreme(pack: &Pack<'_>, reference: &Reference, last: bool) -> Result<Entry> {
    match pack.load(reference)? {
        Node::Leaf(mut entries) => if last {
            entries.pop()
        } else {
            entries.into_iter().next()
        }
        .ok_or(Error::Authority),
        Node::Branch(children) => extreme(
            pack,
            if last {
                children.last()
            } else {
                children.first()
            }
            .ok_or(Error::Authority)?,
            last,
        ),
    }
}
fn neighbors_at(
    pack: &Pack<'_>,
    reference: &Reference,
    key: &[u8],
) -> Result<(Option<Entry>, Option<Entry>)> {
    match pack.load(reference)? {
        Node::Leaf(entries) => {
            match entries.binary_search_by(|entry| entry.key.as_slice().cmp(key)) {
                Ok(_) => Err(Error::Conflict),
                Err(at) => Ok((
                    at.checked_sub(1).map(|i| entries[i].clone()),
                    entries.get(at).cloned(),
                )),
            }
        }
        Node::Branch(children) => {
            let at = children
                .partition_point(|r| r.last.as_slice() < key)
                .min(children.len() - 1);
            let (mut before, mut after) = neighbors_at(pack, &children[at], key)?;
            if before.is_none() && at > 0 {
                before = Some(extreme(pack, &children[at - 1], true)?);
            }
            if after.is_none() && at + 1 < children.len() {
                after = Some(extreme(pack, &children[at + 1], false)?);
            }
            Ok((before, after))
        }
    }
}
fn insert_at(pack: &mut Pack<'_>, reference: &Reference, entry: Entry) -> Result<Vec<Reference>> {
    let node = pack.load(reference)?;
    let mut node = match node {
        Node::Leaf(mut entries) => {
            let at = entries.binary_search_by(|e| e.key.cmp(&entry.key));
            match at {
                Ok(_) => return Err(Error::Conflict),
                Err(at) => entries.insert(at, entry),
            }
            Node::Leaf(entries)
        }
        Node::Branch(mut children) => {
            let at = children
                .partition_point(|r| r.last < entry.key)
                .min(children.len() - 1);
            let next = insert_at(pack, &children[at], entry)?;
            children.splice(at..=at, next);
            Node::Branch(children)
        }
    };
    if node_len(&node) <= MAX_CHILDREN
        && encode_node_bound(&node, reference.level, pack.ctx.key_bound()).is_ok()
    {
        return Ok(vec![pack.put(&node, reference.level)?]);
    }
    let second = match &mut node {
        Node::Leaf(rows) => Node::Leaf(rows.split_off(rows.len() / 2)),
        Node::Branch(rows) => Node::Branch(rows.split_off(rows.len() / 2)),
    };
    Ok(vec![
        pack.put(&node, reference.level)?,
        pack.put(&second, reference.level)?,
    ])
}
fn node_len(node: &Node) -> usize {
    match node {
        Node::Leaf(rows) => rows.len(),
        Node::Branch(rows) => rows.len(),
    }
}
fn node_bounds(node: &Node) -> Result<(Vec<u8>, Vec<u8>, u64)> {
    match node {
        Node::Leaf(rows) => {
            let first = rows.first().ok_or(Error::Bounds)?;
            let last = rows.last().unwrap();
            if rows.windows(2).any(|w| w[0].key >= w[1].key) {
                return Err(Error::Authority);
            }
            Ok((first.key.clone(), last.key.clone(), rows.len() as u64))
        }
        Node::Branch(rows) => {
            let first = rows.first().ok_or(Error::Bounds)?;
            let last = rows.last().unwrap();
            if rows.windows(2).any(|w| w[0].last >= w[1].first) {
                return Err(Error::Authority);
            }
            let count = rows
                .iter()
                .try_fold(0u64, |n, r| n.checked_add(r.count).ok_or(Error::Bounds))?;
            Ok((first.first.clone(), last.last.clone(), count))
        }
    }
}
struct Out(Vec<u8>);
impl Out {
    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        if self
            .0
            .len()
            .checked_add(b.len())
            .is_none_or(|n| n > PAGE_BYTES)
        {
            return Err(Error::Bounds);
        }
        self.0.extend_from_slice(b);
        Ok(())
    }
    fn u16(&mut self, n: usize) -> Result<()> {
        self.bytes(&u16::try_from(n).map_err(|_| Error::Bounds)?.to_le_bytes())
    }
    fn blob(&mut self, b: &[u8]) -> Result<()> {
        self.u16(b.len())?;
        self.bytes(b)
    }
}
struct In<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> In<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(Error::Bounds)?;
        let bytes = self.bytes.get(self.at..end).ok_or(Error::Bounds)?;
        self.at = end;
        Ok(bytes)
    }
    fn u16(&mut self) -> Result<usize> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()) as usize)
    }
    fn blob(&mut self, max: usize) -> Result<Vec<u8>> {
        let n = self.u16()?;
        if n > max {
            return Err(Error::Bounds);
        }
        Ok(self.bytes(n)?.to_vec())
    }
}
fn encode_ref(r: &Reference, key_bound: usize) -> Result<Vec<u8>> {
    check_ref_bound(r, key_bound)?;
    let mut o = Out(vec![]);
    o.bytes(&r.generation)?;
    o.bytes(&r.file.to_le_bytes())?;
    o.bytes(&r.offset.to_le_bytes())?;
    o.bytes(&r.bytes.to_le_bytes())?;
    o.blob(&r.birth)?;
    o.bytes(&[r.level])?;
    o.bytes(&r.count.to_le_bytes())?;
    o.blob(&r.first)?;
    o.blob(&r.last)?;
    Ok(o.0)
}
fn decode_ref(i: &mut In<'_>, key_bound: usize) -> Result<Reference> {
    let generation = i.bytes(32)?.try_into().unwrap();
    let file = u64::from_le_bytes(i.bytes(8)?.try_into().unwrap());
    let offset = u64::from_le_bytes(i.bytes(8)?.try_into().unwrap());
    let bytes = u32::from_le_bytes(i.bytes(4)?.try_into().unwrap());
    let birth = i.blob(MAX_BINDING)?;
    let level = i.bytes(1)?[0];
    let count = u64::from_le_bytes(i.bytes(8)?.try_into().unwrap());
    let first = i.blob(key_bound)?;
    let last = i.blob(key_bound)?;
    let r = Reference {
        generation,
        file,
        offset,
        bytes,
        digest: [0; 32],
        birth,
        level,
        count,
        first,
        last,
    };
    check_ref_bound(&r, key_bound)?;
    Ok(r)
}
fn encode_node_bound(node: &Node, level: u8, key_bound: usize) -> Result<Vec<u8>> {
    if node_len(node) == 0 || node_len(node) > MAX_CHILDREN || level > MAX_DEPTH {
        return Err(Error::Bounds);
    }
    let mut o = Out(MAGIC.to_vec());
    o.bytes(&[1, level, 0, 0])?;
    o.u16(node_len(node))?;
    o.u16(0)?;
    match node {
        Node::Leaf(rows) => {
            if level != 0 {
                return Err(Error::Authority);
            }
            for e in rows {
                validate_entry(e, key_bound)?;
                o.blob(&e.key)?;
                o.blob(&e.value)?;
            }
        }
        Node::Branch(rows) => {
            if level == 0 {
                return Err(Error::Authority);
            }
            for r in rows {
                if r.level + 1 != level {
                    return Err(Error::Authority);
                }
                o.bytes(&encode_ref(r, key_bound)?)?;
                o.bytes(&r.digest)?;
            }
        }
    }
    node_bounds(node)?;
    Ok(o.0)
}
fn decode_node_bound(bytes: &[u8], level: u8, key_bound: usize) -> Result<Node> {
    if bytes.len() > PAGE_BYTES {
        return Err(Error::Bounds);
    }
    let mut i = In { bytes, at: 0 };
    if i.bytes(8)? != MAGIC || i.bytes(4)? != [1, level, 0, 0] {
        return Err(Error::Authority);
    }
    let n = i.u16()?;
    if n == 0 || n > MAX_CHILDREN || i.u16()? != 0 || level > MAX_DEPTH {
        return Err(Error::Bounds);
    }
    let node = if level == 0 {
        let mut rows = vec![];
        for _ in 0..n {
            let entry = Entry {
                key: i.blob(key_bound)?,
                value: i.blob(MAX_VALUE)?,
            };
            validate_entry(&entry, key_bound)?;
            rows.push(entry);
        }
        Node::Leaf(rows)
    } else {
        let mut rows = vec![];
        for _ in 0..n {
            let mut r = decode_ref(&mut i, key_bound)?;
            r.digest = i.bytes(32)?.try_into().unwrap();
            if r.level + 1 != level {
                return Err(Error::Authority);
            }
            rows.push(r);
        }
        Node::Branch(rows)
    };
    if i.at != bytes.len() {
        return Err(Error::Authority);
    }
    node_bounds(&node)?;
    Ok(node)
}
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn test_phase(at: &str) {
    if std::env::var("OOJURO_DIRECTORY_CRASH_PHASE").as_deref() == Ok(at) {
        println!("DIRECTORY_BOUNDARY_READY");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}

#[cfg(test)]
fn encode_node(node: &Node, level: u8) -> Result<Vec<u8>> {
    encode_node_bound(node, level, MAX_KEY)
}
#[cfg(test)]
fn decode_node(bytes: &[u8], level: u8) -> Result<Node> {
    decode_node_bound(bytes, level, MAX_KEY)
}
#[cfg(test)]
fn check_ref(r: &Reference) -> Result<()> {
    check_ref_bound(r, MAX_KEY)
}
