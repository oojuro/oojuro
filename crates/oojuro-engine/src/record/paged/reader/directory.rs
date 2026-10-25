//! Exact derived directories for Record2. No payload population or locator Vec.
//! Full source verification and complete tree coverage precede public reads.
use super::*;
use crate::record::paged::tree::{
    self, Authority, Builder, Context, Editor, Entry, KeyClass, Purpose, Root, Sorter,
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::PathBuf,
};
const ROOT: &str = "DIRECTORY";
const ROOT_BYTES: usize = 64 << 10;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Roots {
    format: u32,
    #[serde(default, skip_serializing_if = "CacheDurability::is_durable")]
    cache_durability: CacheDurability,
    binding: Vec<u8>,
    prefix: Root,
    request: Root,
    record: Root,
    key_version: Root,
}
// The complete source Batch was verified before exposing this Reader. A birth
// proof needs only its authenticated watermark, not a second payload population.
#[derive(Deserialize)]
struct BirthHeader {
    watermark: Watermark,
}
pub(super) struct Directory {
    path: PathBuf,
    roots: Roots,
    _lock: File,
    failed: bool,
}
impl Reader {
    pub fn open_disk(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        directory: &Path,
        minimum: Option<&Watermark>,
    ) -> Result<Self> {
        Self::open_disk_inner(
            dir,
            key,
            identity,
            directory,
            minimum,
            DiskLimits::LEGACY,
            false,
            CacheDurability::Durable,
        )
    }
    /// Disk-backed admission policy; legacy constructors retain their original limits.
    pub fn open_disk_with_limits(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        directory: &Path,
        minimum: Option<&Watermark>,
        limits: DiskLimits,
    ) -> Result<Self> {
        Self::open_disk_inner(
            dir,
            key,
            identity,
            directory,
            minimum,
            limits,
            false,
            CacheDurability::Durable,
        )
    }
    pub fn open_disk_with_cache_policy(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        directory: &Path,
        minimum: Option<&Watermark>,
        limits: DiskLimits,
        policy: CacheDurability,
    ) -> Result<Self> {
        Self::open_disk_inner(
            dir, key, identity, directory, minimum, limits, false, policy,
        )
    }
    pub(super) fn open_disk_inner(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        directory: &Path,
        minimum: Option<&Watermark>,
        limits: DiskLimits,
        initialize: bool,
        policy: CacheDurability,
    ) -> Result<Self> {
        // Source Store settings are fixed inside base and never receive this policy.
        let (mut reader, mut total) = Self::base(dir, key, identity, initialize, limits)?;
        reader.cache_durability = policy;
        if total > limits.source_bytes {
            return Err(Error::Bounds);
        }
        reader.refs.clear();
        reader.refs.shrink_to_fit();
        for sequence in 1..reader.store.len() {
            let encoded = reader.store.get(sequence).map_err(|_| Error::Unavailable)?;
            total = total.checked_add(encoded.len()).ok_or(Error::Bounds)?;
            if total > limits.source_bytes {
                return Err(Error::Bounds);
            }
            let (batch, digest) = reader.verify_after(sequence, &encoded, &reader.tip)?;
            reader.tip = next_ref(&reader.tip, &batch, digest)?;
            reader.sequence = sequence;
        }
        reader.encoded_bytes = total;
        if let Some(w) = minimum {
            let r = reader.scalar_at(w.sequence)?;
            reader.check(w, w.sequence, r.records, r.digest)?;
        }
        let disk = Directory::open(&reader, directory)?;
        reader.disk = Some(disk);
        Ok(reader)
    }
    pub(super) fn publish_group(&mut self, batches: &[(Batch, [u8; 32])]) -> Result<()> {
        let previous = self.tip.clone();
        for (batch, digest) in batches {
            if self.sequence.checked_add(1) != Some(batch.watermark.sequence) {
                return Err(Error::Authority);
            }
            self.tip = next_ref(&self.tip, batch, *digest)?;
            self.sequence = batch.watermark.sequence;
        }
        let mut disk = self.disk.take().ok_or(Error::Invalid)?;
        let result = disk.advance(self, &previous, batches);
        if result.is_err() {
            disk.failed = true;
        }
        self.disk = Some(disk);
        result
    }
    pub fn compact_directory(&mut self) -> Result<()> {
        let mut disk = self.disk.take().ok_or(Error::Invalid)?;
        let result = (|| {
            let roots = build(self, &disk.path)?;
            validate_coverage(self, &disk.path, &roots, self.sequence, false)?;
            disk.roots = roots;
            reap(&disk.path, &disk.roots)
        })();
        if result.is_err() {
            disk.failed = true;
        }
        self.disk = Some(disk);
        result
    }
    pub fn metadata_cache_accounted_bytes(&self) -> Result<usize> {
        self.pages.accounted_bytes()
    }
    pub fn uses_disk_directory(&self) -> bool {
        self.disk.is_some()
    }
    pub(in crate::record::paged) fn boundary_binding(&self, w: &Watermark) -> Result<Vec<u8>> {
        self.validate(w)?;
        let reference = self.reference(w.sequence)?;
        Ok(self.binding_at(w.sequence, &reference))
    }
    pub(in crate::record::paged) fn tree_context<'a>(
        &'a self,
        path: &'a Path,
        purpose: Purpose,
    ) -> Context<'a> {
        Context {
            directory: path,
            cipher: &self.cipher,
            authority: self,
            purpose,
            identity: self.identity_tag,
            class: KeyClass::Data,
            sort_of: None,
            cache: Some(&self.pages),
            cache_durability: self.cache_durability,
        }
    }
    fn binding_at(&self, sequence: u64, r: &Ref) -> Vec<u8> {
        let mut bytes = b"OJREC2B1".to_vec();
        bytes.extend(self.origin);
        bytes.extend(self.identity_tag);
        bytes.extend(sequence.to_be_bytes());
        bytes.extend(r.records.to_be_bytes());
        bytes.extend(r.digest);
        bytes
    }
    fn parse_binding(&self, bytes: &[u8]) -> Result<(u64, u64, [u8; 32])> {
        if bytes.len() != 120
            || &bytes[..8] != b"OJREC2B1"
            || bytes[8..40] != self.origin
            || bytes[40..72] != self.identity_tag
        {
            return Err(Error::Authority);
        }
        let sequence = u64::from_be_bytes(bytes[72..80].try_into().unwrap());
        let records = u64::from_be_bytes(bytes[80..88].try_into().unwrap());
        let digest = bytes[88..120].try_into().unwrap();
        if sequence > self.sequence {
            return Err(Error::Authority);
        }
        Ok((sequence, records, digest))
    }
    fn scalar_at(&self, sequence: u64) -> Result<Ref> {
        if sequence > self.sequence {
            return Err(Error::Authority);
        }
        if sequence == self.sequence {
            return Ok(self.tip.clone());
        }
        let mut previous = self.genesis.clone();
        for at in 1..=sequence {
            let encoded = self.store.get(at).map_err(|_| Error::Unavailable)?;
            let (batch, digest) = self.verify_after(at, &encoded, &previous)?;
            previous = next_ref(&previous, &batch, digest)?;
        }
        Ok(previous)
    }
    pub(super) fn reference(&self, sequence: u64) -> Result<Ref> {
        self.healthy()?;
        if sequence > self.sequence {
            return Err(Error::Authority);
        }
        let r = if let Some(disk) = &self.disk {
            disk.prefix(self, sequence)?
        } else {
            self.refs
                .get(usize::try_from(sequence).map_err(|_| Error::Bounds)?)
                .cloned()
                .ok_or(Error::Authority)?
        };
        if r.records > self.tip.records
            || sequence == 0 && r != self.genesis
            || sequence == self.sequence && r != self.tip
        {
            return Err(Error::Authority);
        }
        Ok(r)
    }
    pub(super) fn locate(&self, ordinal: usize) -> Result<(u64, usize)> {
        if ordinal as u64 >= self.tip.records {
            return Err(Error::Bounds);
        }
        if let Some(disk) = &self.disk {
            return disk.record(
                self,
                self.identity
                    .first_id
                    .checked_add(ordinal as u64)
                    .ok_or(Error::Bounds)?,
            );
        }
        let sequence = self.refs.partition_point(|r| r.records <= ordinal as u64);
        let offset = ordinal - self.refs[sequence - 1].records as usize;
        Ok((sequence as u64, offset))
    }
    fn visit_source(
        &self,
        until: u64,
        mut visit: impl FnMut(u64, &Ref, Option<&Batch>) -> Result<()>,
    ) -> Result<()> {
        if until > self.sequence {
            return Err(Error::Authority);
        }
        let mut previous = self.genesis.clone();
        visit(0, &previous, None)?;
        for sequence in 1..=until {
            let encoded = self.store.get(sequence).map_err(|_| Error::Unavailable)?;
            let (batch, digest) = self.verify_after(sequence, &encoded, &previous)?;
            let next = next_ref(&previous, &batch, digest)?;
            visit(sequence, &next, Some(&batch))?;
            previous = next;
        }
        if until == self.sequence && previous != self.tip {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
impl Authority for Reader {
    fn current(&self) -> Result<Vec<u8>> {
        if self.store.len() != self.sequence.checked_add(1).ok_or(Error::Bounds)? {
            return Err(Error::Authority);
        }
        Ok(self.binding_at(self.sequence, &self.tip))
    }
    fn validate_birth(&self, birth: &[u8], _: Purpose, class: KeyClass) -> Result<()> {
        if class != KeyClass::Data {
            return Err(Error::Scope);
        }
        let (sequence, records, digest) = self.parse_binding(birth)?;
        if birth == self.current()?.as_slice() {
            return Ok(());
        }
        if self
            .births
            .lock()
            .map_err(|_| Error::Unavailable)?
            .iter()
            .any(|b| b == birth)
        {
            return Ok(());
        }
        // Open verified the entire source chain. Store exposes immutable sealed
        // sequence slots: authenticate that slot's watermark without replaying
        // all its ancestors on every metadata-cache miss.
        if sequence == 0 {
            if self.genesis.records != records || self.genesis.digest != digest {
                return Err(Error::Authority);
            }
        } else {
            let encoded = self.store.get(sequence).map_err(|_| Error::Unavailable)?;
            if encoded.len() > MAX_BATCH_BYTES {
                return Err(Error::Bounds);
            }
            let header: BirthHeader =
                serde_json::from_slice(&encoded).map_err(|_| Error::Authority)?;
            self.check(&header.watermark, sequence, records, digest)?;
        }
        if records > self.tip.records {
            return Err(Error::Authority);
        }
        let mut cache = self.births.lock().map_err(|_| Error::Unavailable)?;
        cache.push_back(birth.to_vec());
        while cache.len() > 64 {
            cache.pop_front();
        }
        Ok(())
    }
}
fn next_ref(previous: &Ref, batch: &Batch, digest: [u8; 32]) -> Result<Ref> {
    Ok(Ref {
        records: previous
            .records
            .checked_add(batch.assigned.len() as u64)
            .ok_or(Error::Bounds)?,
        last_at: batch.assigned.last().map(|r| r.recorded_at),
        digest,
        request: batch.request,
    })
}
fn slot(sequence: u64, offset: usize) -> Result<Vec<u8>> {
    let mut out = sequence.to_be_bytes().to_vec();
    out.extend(
        u32::try_from(offset)
            .map_err(|_| Error::Bounds)?
            .to_be_bytes(),
    );
    Ok(out)
}
fn parse_slot(reader: &Reader, bytes: &[u8]) -> Result<(u64, usize)> {
    if bytes.len() != 12 {
        return Err(Error::Authority);
    }
    let sequence = u64::from_be_bytes(bytes[..8].try_into().unwrap());
    let offset = u32::from_be_bytes(bytes[8..].try_into().unwrap()) as usize;
    if sequence == 0 || sequence > reader.sequence || offset >= 1000 {
        return Err(Error::Authority);
    }
    Ok((sequence, offset))
}
fn descending(value: i64) -> [u8; 8] {
    (!(value as u64 ^ (1u64 << 63))).to_be_bytes()
}
fn key_entry(reader: &Reader, row: &Assigned) -> Result<Option<Vec<u8>>> {
    let Some(key) = &row.input.key else {
        return Ok(None);
    };
    let mut out = reader.tag("paged-key-directory-v1", key)?.to_vec();
    out.extend(descending(row.input.valid_at));
    out.extend(descending(row.recorded_at));
    out.extend((!row.id).to_be_bytes());
    Ok(Some(out))
}
fn root_aad(reader: &Reader) -> Vec<u8> {
    let mut a = b"oojuro/record-directory/root/v1\0".to_vec();
    a.extend(reader.origin);
    a.extend(reader.identity_tag);
    reader.cache_durability.bind(&mut a);
    a
}
fn lookup(
    reader: &Reader,
    path: &Path,
    root: &Root,
    purpose: Purpose,
    key: &[u8],
    historical: bool,
) -> Result<Option<Vec<u8>>> {
    let ctx = reader.tree_context(path, purpose);
    if historical {
        tree::get_ancestor(ctx, root, key)
    } else {
        tree::get(ctx, root, key)
    }
}
impl Directory {
    fn open(reader: &Reader, path: &Path) -> Result<Self> {
        tree::prepare_directory(path)?;
        for entry in fs::read_dir(path).map_err(|_| Error::Unavailable)? {
            let entry = entry.map_err(|_| Error::Unavailable)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(Error::Invalid)?;
            if !known(name) || !entry.file_type().map_err(|_| Error::Unavailable)?.is_file() {
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
        let roots = load_roots(reader, path).ok().and_then(|roots| {
            let (sequence, _, _) = reader.parse_binding(&roots.binding).ok()?;
            let historical = sequence != reader.sequence;
            if validate_coverage(reader, path, &roots, sequence, historical).is_ok() {
                Some((roots, sequence))
            } else {
                None
            }
        });
        let roots = match roots {
            Some((roots, sequence)) if sequence == reader.sequence => roots,
            Some((roots, sequence)) => extend(reader, path, roots, sequence)?,
            None => build(reader, path)?,
        };
        validate_coverage(reader, path, &roots, reader.sequence, false)?;
        Ok(Self {
            path: path.into(),
            roots,
            _lock: lock,
            failed: false,
        })
    }
    fn advance(
        &mut self,
        reader: &Reader,
        previous: &Ref,
        batches: &[(Batch, [u8; 32])],
    ) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        let (old, records, digest) = reader.parse_binding(&self.roots.binding)?;
        if batches.is_empty()
            || batches.len() > 1000
            || records != previous.records
            || digest != previous.digest
            || old.checked_add(batches.len() as u64) != Some(reader.sequence)
        {
            return Err(Error::Authority);
        }
        let mut prefix = Editor::new(
            reader.tree_context(&self.path, Purpose::Prefix),
            self.roots.prefix.clone(),
        )?;
        let mut request = Editor::new(
            reader.tree_context(&self.path, Purpose::Request),
            self.roots.request.clone(),
        )?;
        let mut record = Editor::new(
            reader.tree_context(&self.path, Purpose::Record),
            self.roots.record.clone(),
        )?;
        let mut keys = Editor::new(
            reader.tree_context(&self.path, Purpose::KeyVersion),
            self.roots.key_version.clone(),
        )?;
        let mut prefixes = Vec::new();
        let mut requests = Vec::new();
        let mut rows = Vec::new();
        let mut versions = Vec::new();
        let mut previous = previous.clone();
        for (at, (batch, digest)) in batches.iter().enumerate() {
            let sequence = old.checked_add(at as u64 + 1).ok_or(Error::Bounds)?;
            let next = next_ref(&previous, batch, *digest)?;
            reader.check(&batch.watermark, sequence, next.records, next.digest)?;
            prefixes.push(Entry::new(
                sequence.to_be_bytes().to_vec(),
                json(&next, 32 << 10)?,
            )?);
            requests.push(Entry::new(
                batch.request.to_vec(),
                sequence.to_be_bytes().to_vec(),
            )?);
            for (offset, row) in batch.assigned.iter().enumerate() {
                let value = slot(sequence, offset)?;
                rows.push(Entry::new(row.id.to_be_bytes().to_vec(), value.clone())?);
                if rows.len() > 1000 {
                    return Err(Error::Bounds);
                }
                if let Some(key) = key_entry(reader, row)? {
                    versions.push(Entry::new(key, value)?);
                }
            }
            previous = next;
        }
        if previous != reader.tip {
            return Err(Error::Authority);
        }
        prefix.insert_batch(prefixes)?;
        request.insert_batch(requests)?;
        record.insert_batch(rows)?;
        if !versions.is_empty() {
            keys.insert_batch(versions)?;
        }
        let roots = Roots {
            format: 1,
            cache_durability: reader.cache_durability,
            binding: reader.current()?,
            prefix: prefix.finish()?,
            request: request.finish()?,
            record: record.finish()?,
            key_version: keys.finish()?,
        };
        if roots.prefix.count != reader.sequence + 1
            || roots.request.count != reader.sequence
            || roots.record.count != reader.tip.records
        {
            return Err(Error::Authority);
        }
        publish(reader, &self.path, &roots)?;
        self.roots = roots;
        Ok(())
    }
    pub(super) fn prefix(&self, reader: &Reader, sequence: u64) -> Result<Ref> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        let bytes = tree::get(
            reader.tree_context(&self.path, Purpose::Prefix),
            &self.roots.prefix,
            &sequence.to_be_bytes(),
        )?
        .ok_or(Error::Authority)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Authority)
    }
    pub(super) fn request(&self, reader: &Reader, tag: &[u8; 32]) -> Result<Option<u64>> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        tree::get(
            reader.tree_context(&self.path, Purpose::Request),
            &self.roots.request,
            tag,
        )?
        .map(|b| {
            if b.len() != 8 {
                return Err(Error::Authority);
            }
            let s = u64::from_be_bytes(b.try_into().unwrap());
            if s == 0 || s > reader.sequence {
                return Err(Error::Authority);
            }
            Ok(s)
        })
        .transpose()
    }
    pub(super) fn record(&self, reader: &Reader, id: u64) -> Result<(u64, usize)> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        let bytes = tree::get(
            reader.tree_context(&self.path, Purpose::Record),
            &self.roots.record,
            &id.to_be_bytes(),
        )?
        .ok_or(Error::Authority)?;
        parse_slot(reader, &bytes)
    }
    pub(super) fn winner(
        &self,
        reader: &Reader,
        snapshot: &Snapshot,
        key: &str,
    ) -> Result<Option<u64>> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if key.is_empty() || key.len() > 1024 {
            return Ok(None);
        }
        let tag = reader.tag("paged-key-directory-v1", key)?;
        let mut low = tag.to_vec();
        low.extend(descending(snapshot.as_of.valid_at));
        low.extend([0; 16]);
        let mut cursor = tree::Cursor::seek(
            reader.tree_context(&self.path, Purpose::KeyVersion),
            &self.roots.key_version,
            &low,
        )?;
        while let Some(entry) = cursor.next()? {
            if entry.key.len() != 56 {
                return Err(Error::Authority);
            }
            if entry.key[..32] != tag {
                break;
            }
            let (sequence, offset) = parse_slot(reader, &entry.value)?;
            if sequence > snapshot.watermark.sequence {
                continue;
            }
            let id = !u64::from_be_bytes(entry.key[48..56].try_into().unwrap());
            let Some(at) = id.checked_sub(reader.identity.first_id) else {
                return Err(Error::Authority);
            };
            if at >= snapshot.watermark.records {
                continue;
            }
            let batch = reader.batch(sequence as usize)?;
            let row = batch.assigned.get(offset).ok_or(Error::Authority)?;
            if row.id != id || key_entry(reader, row)?.as_deref() != Some(entry.key.as_slice()) {
                return Err(Error::Authority);
            }
            if row.input.key.as_deref() == Some(key)
                && row.input.valid_at <= snapshot.as_of.valid_at
                && row.recorded_at <= snapshot.as_of.known_at
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}
fn validate_coverage(
    reader: &Reader,
    path: &Path,
    roots: &Roots,
    until: u64,
    historical: bool,
) -> Result<()> {
    if roots.format != 1 || roots.cache_durability != reader.cache_durability {
        return Err(Error::Authority);
    }
    let scalar = reader.scalar_at(until)?;
    if roots.binding != reader.binding_at(until, &scalar)
        || roots.prefix.count != until + 1
        || roots.request.count != until
        || roots.record.count != scalar.records
    {
        return Err(Error::Authority);
    }
    let mut keys = 0u64;
    for (root, purpose) in [
        (&roots.prefix, Purpose::Prefix),
        (&roots.request, Purpose::Request),
        (&roots.record, Purpose::Record),
        (&roots.key_version, Purpose::KeyVersion),
    ] {
        if root.binding != roots.binding {
            return Err(Error::Authority);
        }
        let ctx = reader.tree_context(path, purpose);
        if historical {
            tree::validate_ancestor_root(&ctx, root)?;
        } else {
            root.check(&ctx)?;
        }
    }
    reader.visit_source(until, |sequence, reference, batch| {
        let bytes = lookup(
            reader,
            path,
            &roots.prefix,
            Purpose::Prefix,
            &sequence.to_be_bytes(),
            historical,
        )?
        .ok_or(Error::Authority)?;
        let actual: Ref = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
        if actual != *reference {
            return Err(Error::Authority);
        }
        if let Some(batch) = batch {
            if lookup(
                reader,
                path,
                &roots.request,
                Purpose::Request,
                &batch.request,
                historical,
            )?
            .as_deref()
                != Some(sequence.to_be_bytes().as_slice())
            {
                return Err(Error::Authority);
            }
            for (offset, row) in batch.assigned.iter().enumerate() {
                let expected = slot(sequence, offset)?;
                if lookup(
                    reader,
                    path,
                    &roots.record,
                    Purpose::Record,
                    &row.id.to_be_bytes(),
                    historical,
                )?
                .as_deref()
                    != Some(expected.as_slice())
                {
                    return Err(Error::Authority);
                }
                if let Some(key) = key_entry(reader, row)? {
                    keys = keys.checked_add(1).ok_or(Error::Bounds)?;
                    if lookup(
                        reader,
                        path,
                        &roots.key_version,
                        Purpose::KeyVersion,
                        &key,
                        historical,
                    )?
                    .as_deref()
                        != Some(expected.as_slice())
                    {
                        return Err(Error::Authority);
                    }
                }
            }
        }
        Ok(())
    })?;
    if roots.key_version.count != keys {
        return Err(Error::Authority);
    }
    Ok(())
}
fn build(reader: &Reader, path: &Path) -> Result<Roots> {
    let mut prefix = Builder::new(reader.tree_context(path, Purpose::Prefix))?;
    let mut record = Builder::new(reader.tree_context(path, Purpose::Record))?;
    let mut request = Sorter::new(reader.tree_context(path, Purpose::Request))?;
    let mut key_version = Sorter::new(reader.tree_context(path, Purpose::KeyVersion))?;
    reader.visit_source(reader.sequence, |sequence, reference, batch| {
        prefix.push(Entry::new(
            sequence.to_be_bytes().to_vec(),
            json(reference, 32 << 10)?,
        )?)?;
        if let Some(batch) = batch {
            request.push(Entry::new(
                batch.request.to_vec(),
                sequence.to_be_bytes().to_vec(),
            )?)?;
            for (offset, row) in batch.assigned.iter().enumerate() {
                let value = slot(sequence, offset)?;
                record.push(Entry::new(row.id.to_be_bytes().to_vec(), value.clone())?)?;
                if let Some(key) = key_entry(reader, row)? {
                    key_version.push(Entry::new(key, value)?)?;
                }
            }
        }
        Ok(())
    })?;
    let roots = Roots {
        format: 1,
        cache_durability: reader.cache_durability,
        binding: reader.current()?,
        prefix: prefix.finish()?,
        request: request.finish().map_err(|e| {
            if e == Error::Conflict {
                Error::Authority
            } else {
                e
            }
        })?,
        record: record.finish()?,
        key_version: key_version.finish()?,
    };
    publish(reader, path, &roots)?;
    Ok(roots)
}
fn extend(reader: &Reader, path: &Path, roots: Roots, old: u64) -> Result<Roots> {
    let mut prefix = Editor::new(reader.tree_context(path, Purpose::Prefix), roots.prefix)?;
    let mut request = Editor::new(reader.tree_context(path, Purpose::Request), roots.request)?;
    let mut record = Editor::new(reader.tree_context(path, Purpose::Record), roots.record)?;
    let mut key_version = Editor::new(
        reader.tree_context(path, Purpose::KeyVersion),
        roots.key_version,
    )?;
    reader.visit_source(reader.sequence, |sequence, reference, batch| {
        if sequence <= old {
            return Ok(());
        }
        prefix.insert(Entry::new(
            sequence.to_be_bytes().to_vec(),
            json(reference, 32 << 10)?,
        )?)?;
        if let Some(batch) = batch {
            request.insert(Entry::new(
                batch.request.to_vec(),
                sequence.to_be_bytes().to_vec(),
            )?)?;
            for (offset, row) in batch.assigned.iter().enumerate() {
                let value = slot(sequence, offset)?;
                record.insert(Entry::new(row.id.to_be_bytes().to_vec(), value.clone())?)?;
                if let Some(key) = key_entry(reader, row)? {
                    key_version.insert(Entry::new(key, value)?)?;
                }
            }
        }
        Ok(())
    })?;
    let roots = Roots {
        format: 1,
        cache_durability: reader.cache_durability,
        binding: reader.current()?,
        prefix: prefix.finish()?,
        request: request.finish()?,
        record: record.finish()?,
        key_version: key_version.finish()?,
    };
    publish(reader, path, &roots)?;
    Ok(roots)
}
fn load_roots(reader: &Reader, path: &Path) -> Result<Roots> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path.join(ROOT))
        .map_err(|_| Error::Unavailable)?;
    let m = file.metadata().map_err(|_| Error::Unavailable)?;
    if !m.is_file() || m.len() > ROOT_BYTES as u64 + oojuro_store::crypto::SEAL_OVERHEAD as u64 {
        return Err(Error::Bounds);
    }
    let mut sealed = vec![];
    file.take(ROOT_BYTES as u64 + oojuro_store::crypto::SEAL_OVERHEAD as u64 + 1)
        .read_to_end(&mut sealed)
        .map_err(|_| Error::Unavailable)?;
    let plain = reader.unseal(&root_aad(reader), &sealed)?;
    serde_json::from_slice(&plain).map_err(|_| Error::Authority)
}
fn publish(reader: &Reader, path: &Path, roots: &Roots) -> Result<()> {
    #[cfg(test)]
    tree::test_phase("RootPagesDurable");
    let bytes = json(roots, ROOT_BYTES)?;
    let sealed = reader.seal(&root_aad(reader), &bytes);
    let tmp = path.join(format!(
        "root-{}.tmp",
        hex(&oojuro_store::crypto::random_key())
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|_| Error::Unavailable)?;
    file.write_all(&sealed).map_err(|_| Error::Unavailable)?;
    if reader.cache_durability.is_durable() {
        file.sync_all().map_err(|_| Error::Unavailable)?;
    }
    #[cfg(test)]
    tree::test_phase("RootTemp");
    fs::rename(tmp, path.join(ROOT)).map_err(|_| Error::Unavailable)?;
    #[cfg(test)]
    tree::test_phase("RootPublished");
    if reader.cache_durability.is_durable() {
        File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
    }
    #[cfg(test)]
    tree::test_phase("RootDurable");
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn valid_hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn known(name: &str) -> bool {
    if name == ROOT || name == "LOCK" {
        return true;
    }
    if let Some(s) = name
        .strip_prefix("queue-")
        .and_then(|s| s.strip_suffix(".bin"))
    {
        return valid_hex(s, 64);
    }
    if let Some(s) = name
        .strip_prefix("root-")
        .and_then(|s| s.strip_suffix(".tmp"))
    {
        return valid_hex(s, 64);
    }
    for prefix in ["pack-", "tmp-"] {
        if let Some(s) = name
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(".bin"))
            && let Some((generation, file)) = s.split_once('-')
        {
            return valid_hex(generation, 64) && valid_hex(file, 16);
        }
    }
    false
}

fn reap(path: &Path, roots: &Roots) -> Result<()> {
    let mut live = BTreeSet::new();
    for root in [
        &roots.prefix,
        &roots.request,
        &roots.record,
        &roots.key_version,
    ] {
        if let Some(node) = &root.node {
            let generation = root.fresh.ok_or(Error::Authority)?;
            if generation != node.generation {
                return Err(Error::Authority);
            }
            live.insert(hex(&generation));
        }
    }
    for entry in fs::read_dir(path).map_err(|_| Error::Unavailable)? {
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if name == ROOT || name == "LOCK" {
            continue;
        }
        if !known(name) || !entry.file_type().map_err(|_| Error::Unavailable)?.is_file() {
            return Err(Error::Invalid);
        }
        let preserve = name
            .strip_prefix("pack-")
            .and_then(|n| n.split_once('-'))
            .is_some_and(|(g, _)| live.contains(g));
        if !preserve {
            fs::remove_file(entry.path()).map_err(|_| Error::Unavailable)?;
        }
    }
    if roots.cache_durability.is_durable() {
        File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests;
