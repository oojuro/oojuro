//! On-disk posting-reference tree. No segment-reference Vec is retained.
use super::*;
use crate::record::paged::tree::{self, Builder, Editor, Entry, Purpose, Root, Sorter};
const DISK_MANIFEST: &str = "DISK_RECORD_INDEX";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    format: u32,
    origin: [u8; 32],
    identity: [u8; 32],
    prefix: Watermark,
    rows: usize,
    refs: Root,
    bytes: usize,
    decoded: usize,
}
pub(super) struct References {
    root: Root,
    bytes: usize,
    decoded: usize,
}
impl References {
    pub(super) fn count(&self) -> usize {
        self.root.count as usize
    }
    pub(super) fn current(&self, reader: &Reader, path: &Path) -> Result<()> {
        self.root
            .check(&reader.tree_context(path, Purpose::Segment))
    }
    pub(super) fn visit(
        &self,
        reader: &Reader,
        path: &Path,
        historical: bool,
        mut visit: impl FnMut(&Reference) -> Result<bool>,
    ) -> Result<()> {
        let ctx = reader.tree_context(path, Purpose::Segment);
        let mut cursor = if historical {
            tree::Cursor::seek_ancestor(ctx, &self.root, &[])?
        } else {
            tree::Cursor::seek(ctx, &self.root, &[])?
        };
        while let Some(entry) = cursor.next()? {
            let reference: Reference =
                serde_json::from_slice(&entry.value).map_err(|_| Error::Authority)?;
            if entry.key != reference.start.to_be_bytes() {
                return Err(Error::Authority);
            }
            if !visit(&reference)? {
                break;
            }
        }
        Ok(())
    }
}
enum Sink<'a> {
    Build(Builder<'a>),
    Edit(Editor<'a>),
}
impl Sink<'_> {
    fn push(&mut self, entry: Entry) -> Result<()> {
        match self {
            Self::Build(b) => b.push(entry),
            Self::Edit(e) => e.insert(entry),
        }
    }
    fn finish(self) -> Result<Root> {
        match self {
            Self::Build(b) => b.finish(),
            Self::Edit(e) => e.finish(),
        }
    }
}
struct Draft<'a> {
    manifest: Manifest,
    sink: Sink<'a>,
    count: usize,
    bytes: usize,
    decoded: usize,
}
impl Index {
    /// Explicit fresh cache namespace; never silently interprets the Vec-manifest directory.
    pub fn open_disk(reader: &Reader, directory: &Path) -> Result<Self> {
        let tip = reader.watermark()?;
        let lock = lock_directory(directory)?;
        let loaded = load_descriptor(reader, directory).and_then(|d| {
            validate_descriptor(reader, directory, &d)?;
            Ok(d)
        });
        let (descriptor, opened) = match loaded {
            Ok(d) => {
                let opened = if same_prefix(&d.prefix, &tip) {
                    Opened::Loaded
                } else {
                    Opened::Extended
                };
                (Some(d), opened)
            }
            Err(_) => (None, Opened::Rebuilt),
        };
        let (mut manifest, refs) = if let Some(d) = descriptor {
            (
                Manifest {
                    format: FORMAT,
                    origin: d.origin,
                    identity: d.identity,
                    prefix: d.prefix,
                    rows: d.rows,
                    segments: vec![],
                },
                Some(References {
                    root: d.refs,
                    bytes: d.bytes,
                    decoded: d.decoded,
                }),
            )
        } else {
            (
                Manifest {
                    format: FORMAT,
                    origin: tip.origin,
                    identity: tip.identity,
                    prefix: tip.clone(),
                    rows: 0,
                    segments: vec![],
                },
                None,
            )
        };
        // No incomplete root is exposed during rebuild.
        if opened == Opened::Rebuilt {
            manifest.prefix = tip;
        }
        let mut index = Self {
            manifest,
            disk: refs,
            directory: directory.into(),
            _lock: lock,
            opened,
            failed: false,
            #[cfg(test)]
            hook: None,
        };
        if opened != Opened::Loaded {
            index.append_disk_tail(reader, opened == Opened::Rebuilt)?;
        }
        Ok(index)
    }
    pub fn rebuild_disk(&mut self, reader: &Reader) -> Result<()> {
        if self.disk.is_none() {
            return Err(Error::Invalid);
        }
        reader.validate(&self.manifest.prefix)?;
        let result = self.append_disk_tail(reader, true);
        self.failed = result.is_err();
        result
    }
    pub fn retained_reference_count(&self) -> usize {
        self.manifest.segments.len()
    }
    pub(super) fn append_disk_tail(&mut self, reader: &Reader, replace: bool) -> Result<()> {
        let directory = self.directory.clone();
        let prefix = reader.watermark()?;
        let count = reader.validate(&prefix)?;
        let start = if replace {
            0
        } else {
            reader.validate(&self.manifest.prefix)?
        };
        let ctx = reader.tree_context(&directory, Purpose::Segment);
        let (sink, old_count, bytes, decoded) = if replace {
            (Sink::Build(Builder::new(ctx)?), 0, 0, 0)
        } else {
            let old = self.disk.as_ref().ok_or(Error::Authority)?;
            (
                Sink::Edit(Editor::new(ctx, old.root.clone())?),
                old.count(),
                old.bytes,
                old.decoded,
            )
        };
        let mut draft = Draft {
            manifest: Manifest {
                format: FORMAT,
                origin: prefix.origin,
                identity: prefix.identity,
                prefix: prefix.clone(),
                rows: count,
                segments: vec![],
            },
            sink,
            count: old_count,
            bytes,
            decoded,
        };
        let mut rows = Vec::new();
        let mut text = 0usize;
        let mut at = start;
        reader.scan(&prefix, start, count, |row| {
            if !rows.is_empty()
                && (rows.len() >= BUILD_ROWS
                    || text
                        .checked_add(row.input.text.len())
                        .is_none_or(|n| n > BUILD_TEXT))
            {
                self.flush_disk_rows(reader, &mut draft, &rows, at)?;
                at += rows.len();
                rows.clear();
                text = 0;
            }
            text = text
                .checked_add(row.input.text.len())
                .ok_or(Error::Bounds)?;
            if text > BUILD_TEXT {
                return Err(Error::Bounds);
            }
            rows.push(row.clone());
            Ok(true)
        })?;
        if !rows.is_empty() {
            self.flush_disk_rows(reader, &mut draft, &rows, at)?;
        }
        self.finish_disk(reader, draft)
    }
    fn flush_disk_rows(
        &self,
        reader: &Reader,
        draft: &mut Draft<'_>,
        rows: &[Assigned],
        start: usize,
    ) -> Result<()> {
        let identity = reader.identity();
        let projection = lexical::Index::from_record_rows(
            &identity.writer,
            identity.agent,
            rows.iter().map(|r| {
                (
                    r.id,
                    r.input.valid_at,
                    r.input.text.as_str(),
                    r.input.indexed,
                )
            }),
        )
        .map_err(map_codec)?;
        if projection::candidate_bytes(&projection).map_err(map_codec)? > CACHE_BOUND {
            return Err(Error::Bounds);
        }
        let mut local = 0;
        while local < rows.len() {
            let mut count = (rows.len() - local).min(codec::MAX_ROWS);
            loop {
                let mut segment =
                    projection::snapshot(&projection, local, count).map_err(map_codec)?;
                segment.start = (start + local) as u32;
                match codec::encode(&segment) {
                    Ok(bytes) => {
                        self.emit_disk(reader, draft, &segment, &bytes)?;
                        break;
                    }
                    Err(_) if count > 1 => count = count.div_ceil(2),
                    Err(e) => return Err(map_codec(e)),
                }
            }
            local += count;
        }
        Ok(())
    }
    fn emit_disk(
        &self,
        reader: &Reader,
        draft: &mut Draft<'_>,
        segment: &codec::Segment,
        bytes: &[u8],
    ) -> Result<()> {
        let decoded = projection::segment_bytes(segment).map_err(map_codec)?;
        draft.bytes = draft.bytes.checked_add(bytes.len()).ok_or(Error::Bounds)?;
        draft.decoded = draft.decoded.checked_add(decoded).ok_or(Error::Bounds)?;
        draft.count = draft.count.checked_add(1).ok_or(Error::Bounds)?;
        if draft.bytes > CACHE_BOUND || draft.decoded > CACHE_BOUND || draft.count > SEGMENTS_BOUND
        {
            return Err(Error::Bounds);
        }
        let mut reference = Reference {
            id: hex(&random_key()),
            prefix: draft.manifest.prefix.clone(),
            start: segment.start,
            rows: segment.rows.len() as u32,
            bytes: bytes.len(),
            decoded,
            digest: [0; 32],
        };
        let sealed = reader.seal(&segment_aad(&draft.manifest, &reference)?, bytes);
        reference.digest = *blake3::hash(&sealed).as_bytes();
        let tmp = self.directory.join(format!("tmp-{}.idx", reference.id));
        write_new(&tmp, &sealed)?;
        self.phase(Phase::SegmentTemp);
        fs::rename(
            tmp,
            self.directory.join(format!("seg-{}.idx", reference.id)),
        )
        .map_err(|_| Error::Unavailable)?;
        directory_sync(&self.directory)?;
        self.phase(Phase::SegmentPublished);
        draft.sink.push(Entry::new(
            reference.start.to_be_bytes().to_vec(),
            json(&reference, 32 << 10)?,
        )?)
    }
    fn finish_disk(&mut self, reader: &Reader, draft: Draft<'_>) -> Result<()> {
        let root = draft.sink.finish()?;
        self.phase(Phase::DirectoryPagesDurable);
        let descriptor = Descriptor {
            format: 1,
            origin: draft.manifest.origin,
            identity: draft.manifest.identity,
            prefix: draft.manifest.prefix.clone(),
            rows: draft.manifest.rows,
            refs: root,
            bytes: draft.bytes,
            decoded: draft.decoded,
        };
        validate_descriptor(reader, &self.directory, &descriptor)?;
        let bytes = json(&descriptor, ROOT_BYTES)?;
        let sealed = reader.seal(
            &descriptor_aad(descriptor.origin, descriptor.identity),
            &bytes,
        );
        let tmp = self
            .directory
            .join(format!("disk-{}.tmp", hex(&random_key())));
        write_new(&tmp, &sealed)?;
        self.phase(Phase::ManifestTemp);
        fs::rename(tmp, self.directory.join(DISK_MANIFEST)).map_err(|_| Error::Unavailable)?;
        self.phase(Phase::ManifestPublished);
        directory_sync(&self.directory)?;
        self.phase(Phase::ManifestDurable);
        self.manifest = draft.manifest;
        self.disk = Some(References {
            root: descriptor.refs,
            bytes: descriptor.bytes,
            decoded: descriptor.decoded,
        });
        self.reap_disk(reader)
    }
    pub(super) fn merge_disk(&mut self, reader: &Reader) -> Result<()> {
        self.current(reader)?;
        let result = (|| {
            let directory = self.directory.clone();
            let prefix = reader.watermark()?;
            let mut draft = Draft {
                manifest: Manifest {
                    format: FORMAT,
                    origin: prefix.origin,
                    identity: prefix.identity,
                    prefix,
                    rows: self.manifest.rows,
                    segments: vec![],
                },
                sink: Sink::Build(Builder::new(
                    reader.tree_context(&directory, Purpose::Segment),
                )?),
                count: 0,
                bytes: 0,
                decoded: 0,
            };
            let mut carry: Option<codec::Segment> = None;
            self.visit_references(reader, |reference| {
                let segment = load_segment(reader, &self.directory, &self.manifest, reference)?;
                if let Some(old) = &carry {
                    let bytes = projection::segment_bytes(old)
                        .map_err(map_codec)?
                        .checked_add(projection::segment_bytes(&segment).map_err(map_codec)?)
                        .and_then(|n| n.checked_add(192))
                        .ok_or(Error::Bounds)?;
                    if old.rows.len() + segment.rows.len() > codec::MAX_ROWS
                        || bytes > codec::MAX_BYTES
                    {
                        let old = carry.take().unwrap();
                        self.emit_disk(
                            reader,
                            &mut draft,
                            &old,
                            &codec::encode(&old).map_err(map_codec)?,
                        )?;
                    }
                }
                if let Some(old) = &mut carry {
                    if old.start as usize + old.rows.len() != segment.start as usize {
                        return Err(Error::Authority);
                    }
                    let offset = old.rows.len() as u32;
                    old.rows.extend(segment.rows);
                    for (term, list) in segment.postings {
                        old.postings
                            .entry(term)
                            .or_default()
                            .extend(list.into_iter().map(|(r, tf)| (r + offset, tf)));
                    }
                } else {
                    carry = Some(segment);
                }
                Ok(true)
            })?;
            if let Some(old) = carry {
                self.emit_disk(
                    reader,
                    &mut draft,
                    &old,
                    &codec::encode(&old).map_err(map_codec)?,
                )?;
            }
            self.finish_disk(reader, draft)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn reap_disk(&self, reader: &Reader) -> Result<()> {
        let refs = self.disk.as_ref().ok_or(Error::Authority)?;
        let ctx = reader.tree_context(&self.directory, Purpose::Segment);
        let mut sorter = Sorter::new(ctx.clone())?;
        tree::visit_pages(ctx.clone(), &refs.root, |r| {
            let mut key = vec![0];
            key.extend(r.generation);
            key.extend(r.file.to_be_bytes());
            key.extend(r.offset.to_be_bytes());
            sorter.push(Entry::new(key, vec![])?)
        })?;
        refs.visit(reader, &self.directory, false, |r| {
            let mut key = vec![1];
            key.extend(decode_hex(&r.id)?);
            sorter.push(Entry::new(key, vec![])?)?;
            Ok(true)
        })?;
        let inventory = sorter.finish()?;
        let generation = inventory.fresh.ok_or(Error::Authority)?;
        for item in fs::read_dir(&self.directory).map_err(|_| Error::Unavailable)? {
            let item = item.map_err(|_| Error::Unavailable)?;
            let name = item.file_name();
            let name = name.to_str().ok_or(Error::Invalid)?;
            if name == "LOCK" || name == DISK_MANIFEST {
                continue;
            }
            if !known(name) || !item.file_type().map_err(|_| Error::Unavailable)?.is_file() {
                return Err(Error::Invalid);
            }
            let keep = if let Some(id) = name
                .strip_prefix("seg-")
                .and_then(|n| n.strip_suffix(".idx"))
            {
                let mut key = vec![1];
                key.extend(decode_hex(id)?);
                tree::get(ctx.clone(), &inventory, &key)?.is_some()
            } else if let Some((g, file)) = pack_name(name) {
                if g == generation {
                    true
                } else {
                    let mut key = vec![0];
                    key.extend(g);
                    key.extend(file.to_be_bytes());
                    let mut c = tree::Cursor::seek(ctx.clone(), &inventory, &key)?;
                    c.next()?.is_some_and(|e| e.key.starts_with(&key))
                }
            } else {
                false
            };
            if !keep {
                fs::remove_file(item.path()).map_err(|_| Error::Unavailable)?;
            }
        }
        if let Some(node) = inventory.node {
            for file in 0..=node.file {
                fs::remove_file(
                    self.directory
                        .join(format!("pack-{}-{file:016x}.bin", hex(&generation))),
                )
                .map_err(|_| Error::Unavailable)?;
            }
        }
        directory_sync(&self.directory)
    }
}
const ROOT_BYTES: usize = 64 << 10;
fn descriptor_aad(origin: [u8; 32], identity: [u8; 32]) -> Vec<u8> {
    let mut out = b"oojuro/record-paged/disk-manifest/v1\0".to_vec();
    out.extend(origin);
    out.extend(identity);
    out
}
fn load_descriptor(reader: &Reader, path: &Path) -> Result<Descriptor> {
    let tip = reader.watermark()?;
    let sealed = read_bounded(&path.join(DISK_MANIFEST), ROOT_BYTES + SEAL_OVERHEAD)?;
    let plain = reader.unseal(&descriptor_aad(tip.origin, tip.identity), &sealed)?;
    serde_json::from_slice(&plain).map_err(|_| Error::Authority)
}
fn validate_descriptor(reader: &Reader, path: &Path, d: &Descriptor) -> Result<()> {
    let tip = reader.watermark()?;
    if d.format != 1
        || d.origin != tip.origin
        || d.identity != tip.identity
        || d.rows > MAX_RECORDS
        || reader.validate(&d.prefix)? != d.rows
        || d.refs.count > SEGMENTS_BOUND as u64
        || d.bytes > CACHE_BOUND
        || d.decoded > CACHE_BOUND
        || d.refs.binding != reader.boundary_binding(&d.prefix)?
    {
        return Err(Error::Authority);
    }
    let historical = !same_prefix(&d.prefix, &tip);
    let refs = References {
        root: d.refs.clone(),
        bytes: d.bytes,
        decoded: d.decoded,
    };
    let m = Manifest {
        format: FORMAT,
        origin: d.origin,
        identity: d.identity,
        prefix: d.prefix.clone(),
        rows: d.rows,
        segments: vec![],
    };
    let (mut rows, mut bytes, mut decoded, mut count) = (0usize, 0usize, 0usize, 0usize);
    refs.visit(reader, path, historical, |r| {
        if !id_valid(&r.id)
            || r.start as usize != rows
            || r.rows == 0
            || r.rows as usize > codec::MAX_ROWS
            || r.bytes > codec::MAX_BYTES
            || r.decoded > codec::MAX_BYTES
            || r.bytes < 192
            || r.prefix.sequence > d.prefix.sequence
        {
            return Err(Error::Authority);
        }
        rows = rows.checked_add(r.rows as usize).ok_or(Error::Bounds)?;
        bytes = bytes.checked_add(r.bytes).ok_or(Error::Bounds)?;
        decoded = decoded.checked_add(r.decoded).ok_or(Error::Bounds)?;
        count += 1;
        if rows > d.rows
            || rows > reader.validate(&r.prefix)?
            || bytes > CACHE_BOUND
            || decoded > CACHE_BOUND
        {
            return Err(Error::Bounds);
        }
        let segment = load_segment(reader, path, &m, r)?;
        check_original(reader, &d.prefix, &segment)?;
        Ok(true)
    })?;
    if rows != d.rows || bytes != d.bytes || decoded != d.decoded || count as u64 != d.refs.count {
        return Err(Error::Authority);
    }
    Ok(())
}
fn lock_directory(path: &Path) -> Result<File> {
    tree::prepare_directory(path)?;
    for item in fs::read_dir(path).map_err(|_| Error::Unavailable)? {
        let item = item.map_err(|_| Error::Unavailable)?;
        let name = item.file_name();
        if !known(name.to_str().ok_or(Error::Invalid)?)
            || !item.file_type().map_err(|_| Error::Unavailable)?.is_file()
        {
            return Err(Error::Invalid);
        }
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path.join("LOCK"))
        .map_err(|_| Error::Unavailable)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Unavailable);
    }
    Ok(lock)
}
fn decode_hex(id: &str) -> Result<Vec<u8>> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid);
    }
    id.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            let digit = |c: u8| {
                if c.is_ascii_digit() {
                    c - b'0'
                } else {
                    c - b'a' + 10
                }
            };
            Ok(digit(p[0]) * 16 + digit(p[1]))
        })
        .collect()
}
fn pack_name(name: &str) -> Option<([u8; 32], u64)> {
    let s = name.strip_prefix("pack-")?.strip_suffix(".bin")?;
    let (g, file) = s.split_once('-')?;
    let g = decode_hex(g).ok()?.try_into().ok()?;
    if file.len() != 16
        || !file
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    Some((g, u64::from_str_radix(file, 16).ok()?))
}
fn known(name: &str) -> bool {
    if name == "LOCK" || name == DISK_MANIFEST {
        return true;
    }
    if pack_name(name).is_some() {
        return true;
    }
    if let Some(s) = name
        .strip_prefix("tmp-")
        .and_then(|n| n.strip_suffix(".bin"))
    {
        return pack_name(&format!("pack-{s}.bin")).is_some();
    }
    for (a, b) in [
        ("seg-", ".idx"),
        ("tmp-", ".idx"),
        ("disk-", ".tmp"),
        ("queue-", ".bin"),
    ] {
        if let Some(id) = name.strip_prefix(a).and_then(|n| n.strip_suffix(b)) {
            return decode_hex(id).is_ok();
        }
    }
    false
}
#[cfg(test)]
mod tests;
