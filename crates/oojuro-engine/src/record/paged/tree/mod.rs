//! Private bounded encrypted metadata B+tree. Never an authority or a second WAL.
//! Ref birth bindings are authenticated by parents and independently checked as ancestors.
mod sort;
use super::super::*;
use oojuro_store::crypto::{FrameCipher, SEAL_OVERHEAD, random_key};
pub(super) use sort::Sorter;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub(super) const PAGE_BYTES: usize = 64 << 10;
const PACK_BYTES: u64 = 64 << 20;
const MAX_CHILDREN: usize = 128;
const MAX_DEPTH: u8 = 16;
const MAX_BINDING: usize = 2048;
const MAX_KEY: usize = 64;
const MAX_VALUE: usize = 32 << 10;
const MAGIC: &[u8; 8] = b"OJDIR001";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Purpose {
    Prefix,
    Request,
    Record,
    KeyVersion,
    Segment,
    Sort,
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
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyClass {
    Data,
    RetainedControl,
}
/// Trusted authority adapter, never a wire-selected authenticated bool.
pub(super) trait Authority {
    fn current(&self) -> Result<Vec<u8>>;
    fn validate_birth(&self, birth: &[u8], purpose: Purpose, class: KeyClass) -> Result<()>;
}
#[derive(Clone)]
pub(super) struct Context<'a> {
    pub directory: &'a Path,
    pub cipher: &'a FrameCipher,
    pub authority: &'a dyn Authority,
    pub purpose: Purpose,
    pub identity: [u8; 32],
    pub class: KeyClass,
    pub sort_of: Option<Purpose>,
}
impl<'a> Context<'a> {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
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
pub(super) struct Root {
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
            check_ref(node)?;
        }
        Ok(())
    }
}
#[derive(Clone)]
enum Node {
    Leaf(Vec<Entry>),
    Branch(Vec<Reference>),
}
fn check_ref(r: &Reference) -> Result<()> {
    if r.bytes as usize > PAGE_BYTES + SEAL_OVERHEAD
        || r.bytes as usize <= SEAL_OVERHEAD
        || r.birth.is_empty()
        || r.birth.len() > MAX_BINDING
        || r.level > MAX_DEPTH
        || r.count == 0
        || r.first.is_empty()
        || r.first.len() > MAX_KEY
        || r.last.len() > MAX_KEY
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
    bytes.extend(encode_ref(r)?);
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
pub(super) fn prepare_directory(dir: &Path) -> Result<()> {
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
pub(super) struct Pack<'a> {
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
            file.sync_all().map_err(|_| Error::Unavailable)?;
            fs::rename(
                path(self.ctx.directory, self.generation, self.number, true),
                path(self.ctx.directory, self.generation, self.number, false),
            )
            .map_err(|_| Error::Unavailable)?;
            sync_dir(self.ctx.directory)?;
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
            check_ref(r)?;
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
            let node = decode_node(&plain, r.level)?;
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
        let plain = encode_node(node, level)?;
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
        self.poisoned = true;
        self.current
            .as_mut()
            .unwrap()
            .write_all(&sealed)
            .map_err(|_| Error::Unavailable)?;
        self.offset += sealed.len() as u64;
        self.poisoned = false;
        Ok(r)
    }
}
fn load(ctx: &Context<'_>, r: &Reference) -> Result<Node> {
    check_ref(r)?;
    ctx.authority
        .validate_birth(&r.birth, ctx.sort_of.unwrap_or(ctx.purpose), ctx.class)?;
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
    let node = decode_node(&plain, r.level)?;
    let (first, last, count) = node_bounds(&node)?;
    if first != r.first || last != r.last || count != r.count {
        return Err(Error::Authority);
    }
    Ok(node)
}
pub(super) struct Builder<'a> {
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
        Entry::new(entry.key.clone(), entry.value.clone())?;
        if self.last.as_ref().is_some_and(|old| *old >= entry.key) {
            return Err(Error::Conflict);
        }
        let mut candidate = self.leaf.clone();
        candidate.push(entry.clone());
        if self.leaf.len() >= MAX_CHILDREN || encode_node(&Node::Leaf(candidate), 0).is_err() {
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
                || encode_node(&Node::Branch(candidate), at as u8 + 1).is_err())
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
pub(super) struct Cursor<'a> {
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
            if self.ctx.check()? != self.binding {
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
pub(super) fn get(ctx: Context<'_>, root: &Root, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut c = Cursor::seek(ctx, root, key)?;
    Ok(c.next()?.filter(|e| e.key == key).map(|e| e.value))
}
fn check_ancestor(ctx: &Context<'_>, root: &Root) -> Result<()> {
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
pub(super) struct Editor<'a> {
    pack: Pack<'a>,
    root: Root,
}
impl<'a> Editor<'a> {
    pub fn new(ctx: Context<'a>, root: Root) -> Result<Self> {
        check_ancestor(&ctx, &root)?;
        Ok(Self {
            pack: Pack::new(ctx)?,
            root,
        })
    }
    pub fn insert(&mut self, entry: Entry) -> Result<()> {
        Entry::new(entry.key.clone(), entry.value.clone())?;
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
    pub fn finish(self) -> Result<Root> {
        self.pack.finish()?;
        Ok(self.root)
    }
}
fn insert_at(pack: &mut Pack<'_>, reference: &Reference, entry: Entry) -> Result<Vec<Reference>> {
    let node = pack.load(reference)?;
    let mut node = match node {
        Node::Leaf(mut entries) => {
            let at = entries
                .binary_search_by(|e| e.key.cmp(&entry.key))
                .map_err(|at| at);
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
    if node_len(&node) <= MAX_CHILDREN && encode_node(&node, reference.level).is_ok() {
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
fn encode_ref(r: &Reference) -> Result<Vec<u8>> {
    check_ref(r)?;
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
fn decode_ref(i: &mut In<'_>) -> Result<Reference> {
    let generation = i.bytes(32)?.try_into().unwrap();
    let file = u64::from_le_bytes(i.bytes(8)?.try_into().unwrap());
    let offset = u64::from_le_bytes(i.bytes(8)?.try_into().unwrap());
    let bytes = u32::from_le_bytes(i.bytes(4)?.try_into().unwrap());
    let birth = i.blob(MAX_BINDING)?;
    let level = i.bytes(1)?[0];
    let count = u64::from_le_bytes(i.bytes(8)?.try_into().unwrap());
    let first = i.blob(MAX_KEY)?;
    let last = i.blob(MAX_KEY)?;
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
    check_ref(&r)?;
    Ok(r)
}
fn encode_node(node: &Node, level: u8) -> Result<Vec<u8>> {
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
                Entry::new(e.key.clone(), e.value.clone())?;
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
                o.bytes(&encode_ref(r)?)?;
                o.bytes(&r.digest)?;
            }
        }
    }
    node_bounds(node)?;
    Ok(o.0)
}
fn decode_node(bytes: &[u8], level: u8) -> Result<Node> {
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
            rows.push(Entry::new(i.blob(MAX_KEY)?, i.blob(MAX_VALUE)?)?);
        }
        Node::Leaf(rows)
    } else {
        let mut rows = vec![];
        for _ in 0..n {
            let mut r = decode_ref(&mut i)?;
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
