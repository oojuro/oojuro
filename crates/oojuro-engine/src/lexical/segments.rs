//! Explicit opt-in, encrypted and disposable native lexical segments.
//! CaptureLog alone commits data. Cache failure never changes an authority ACK.
//! Bounded RAM merge, not the SPEC's external-memory multi-gigabyte engine.
pub(crate) mod codec;
use super::{Error, Hit, Index, Query, Result};
use crate::capture_log::{CachePrefix, CaptureLog, MAX_EVENTS};
use oojuro_store::crypto::{FrameCipher, SEAL_OVERHEAD, random_key};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

const FORMAT: u32 = 1;
const MANIFEST_BOUND: usize = 1 << 20;
const CACHE_BOUND: usize = 128 << 20;
const SEGMENTS_BOUND: usize = 1024;
const MANIFEST: &str = "LEXICAL";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: String,
    prefix: CachePrefix,
    start: u32,
    rows: u32,
    bytes: usize,
    decoded: usize,
    digest: [u8; 32],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    origin: [u8; 32],
    identity: [u8; 32],
    prefix: CachePrefix,
    rows: usize,
    segments: Vec<Reference>,
}
/// What happened on this open. Rebuilt includes absent, invalid and foreign
/// caches; the authoritative log is always validated independently first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opened {
    Loaded,
    Extended,
    Rebuilt,
}
/// One publisher per dedicated cache directory, independently of Store's lock.
/// No cache file is consulted to recover or acknowledge capture data.
pub struct PersistentIndex {
    index: Index,
    manifest: Manifest,
    directory: PathBuf,
    cipher: Arc<FrameCipher>,
    _lock: File,
    opened: Opened,
    failed: bool,
    #[cfg(test)]
    hook: Option<Box<dyn Fn(Phase)>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    SegmentTemp,
    SegmentPublished,
    ManifestTemp,
    ManifestPublished,
    ManifestDurable,
}
impl PersistentIndex {
    pub fn open(log: &CaptureLog, directory: &Path) -> Result<Self> {
        let tip = log.cache_tip().map_err(|_| Error::Unavailable)?;
        let origin = log.cache_origin().map_err(|_| Error::Unavailable)?;
        let identity = identity(log)?;
        let lock = directory_lock(directory)?;
        let cipher = log.cache_cipher();
        let loaded = load(log, directory, &cipher, origin, identity);
        let (index, manifest, opened) = match loaded {
            Ok((index, manifest)) => {
                let opened = if manifest.prefix == tip {
                    Opened::Loaded
                } else {
                    Opened::Extended
                };
                (index, manifest, opened)
            }
            Err(_) => (
                Index::rebuild(log)?,
                Manifest {
                    format: FORMAT,
                    origin,
                    identity,
                    prefix: tip.clone(),
                    rows: 0,
                    segments: vec![],
                },
                Opened::Rebuilt,
            ),
        };
        let mut cache = Self {
            index,
            manifest,
            directory: directory.into(),
            cipher,
            _lock: lock,
            opened,
            failed: false,
            #[cfg(test)]
            hook: None,
        };
        if opened != Opened::Loaded {
            cache.index.refresh(log)?;
            cache.publish(log, opened == Opened::Rebuilt)?;
        }
        Ok(cache)
    }
    pub fn opened(&self) -> Opened {
        self.opened
    }
    pub fn applied_batches(&self) -> usize {
        self.index.applied_batches()
    }
    pub fn segment_count(&self) -> usize {
        self.manifest.segments.len()
    }
    /// A current authority append makes this cache unreadable until refresh.
    /// This check includes registrations/raw-only receipts, not merely row count.
    pub fn search(&self, log: &CaptureLog, query: &Query<'_>) -> Result<Vec<Hit>> {
        self.current(log)?;
        self.index.search(query)
    }
    pub fn search_where(
        &self,
        log: &CaptureLog,
        query: &Query<'_>,
        keep: &mut dyn FnMut(&crate::capture_log::Event) -> bool,
    ) -> Result<Vec<Hit>> {
        self.current(log)?;
        self.index.search_where(query, keep)
    }
    fn current(&self, log: &CaptureLog) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if self.index.authority != log.instance() {
            return Err(Error::Authority);
        }
        if self.manifest.prefix != log.cache_tip().map_err(|_| Error::Unavailable)?
            || self.manifest.origin != log.cache_origin().map_err(|_| Error::Unavailable)?
        {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    /// Projects only newly committed parsed receipts. A publication failure
    /// invalidates this handle; reopen/rebuild instead of serving old results.
    pub fn refresh(&mut self, log: &CaptureLog) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if self.index.authority != log.instance() {
            return Err(Error::Authority);
        }
        if self.current(log).is_ok() {
            return Ok(());
        }
        let result = (|| {
            self.index.refresh(log)?;
            let changed =
                self.manifest.origin != log.cache_origin().map_err(|_| Error::Unavailable)?;
            self.publish(log, changed)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    /// Bounded merge re-encodes the current native RAM projection (<=100k rows,
    /// <=128MiB encoded candidate). It is not an external-memory streaming merge.
    pub fn merge(&mut self, log: &CaptureLog) -> Result<()> {
        self.current(log)?;
        let result = self.publish(log, true);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn phase(&self, phase: Phase) {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(phase);
        }
        #[cfg(not(test))]
        let _ = phase;
    }
    fn publish(&mut self, log: &CaptureLog, replace: bool) -> Result<()> {
        let prefix = log.cache_tip().map_err(|_| Error::Unavailable)?;
        let origin = log.cache_origin().map_err(|_| Error::Unavailable)?;
        let mut next = Manifest {
            format: FORMAT,
            origin,
            identity: identity(log)?,
            prefix: prefix.clone(),
            rows: self.index.documents.len(),
            segments: if replace {
                vec![]
            } else {
                self.manifest.segments.clone()
            },
        };
        if next.rows > MAX_EVENTS {
            return Err(Error::Invalid);
        }
        if candidate_bytes(&self.index)? > CACHE_BOUND {
            return Err(Error::Invalid);
        }
        let mut start = if replace { 0 } else { self.manifest.rows };
        let mut total = next
            .segments
            .iter()
            .try_fold(0usize, |n, r| n.checked_add(r.bytes).ok_or(Error::Invalid))?;
        let mut decoded_total = next.segments.iter().try_fold(0usize, |n, r| {
            n.checked_add(r.decoded).ok_or(Error::Invalid)
        })?;
        while start < next.rows {
            let mut count = (next.rows - start).min(codec::MAX_ROWS);
            let (plain, decoded) = loop {
                let data = self.segment(start, count)?;
                match codec::encode(&data) {
                    Ok(bytes) => break (bytes, segment_bytes(&data)?),
                    Err(_) if count > 1 => count = count.div_ceil(2),
                    Err(error) => return Err(error),
                }
            };
            total = total.checked_add(plain.len()).ok_or(Error::Invalid)?;
            decoded_total = decoded_total.checked_add(decoded).ok_or(Error::Invalid)?;
            if total > CACHE_BOUND
                || decoded_total > CACHE_BOUND
                || next.segments.len() >= SEGMENTS_BOUND
            {
                return Err(Error::Invalid);
            }
            let id = hex(&random_key());
            let mut reference = Reference {
                id: id.clone(),
                prefix: prefix.clone(),
                start: start as u32,
                rows: count as u32,
                bytes: plain.len(),
                decoded,
                digest: [0; 32],
            };
            let aad = segment_aad(&next, &reference)?;
            let sealed = self.cipher.seal_frame(&aad, &plain);
            reference.digest = *blake3::hash(&sealed).as_bytes();
            let temp = self.directory.join(format!("tmp-{id}.idx"));
            write_new(&temp, &sealed)?;
            self.phase(Phase::SegmentTemp);
            fs::rename(&temp, self.directory.join(format!("seg-{id}.idx")))
                .map_err(|_| Error::Unavailable)?;
            directory_sync(&self.directory)?;
            self.phase(Phase::SegmentPublished);
            next.segments.push(reference);
            start += count;
        }
        let plain = serde_json::to_vec(&next).map_err(|_| Error::Invalid)?;
        if plain.len() > MANIFEST_BOUND {
            return Err(Error::Invalid);
        }
        let sealed = self
            .cipher
            .seal_frame(&manifest_aad(origin, next.identity), &plain);
        let temp = self
            .directory
            .join(format!("tmp-{}.man", hex(&random_key())));
        write_new(&temp, &sealed)?;
        self.phase(Phase::ManifestTemp);
        fs::rename(&temp, self.directory.join(MANIFEST)).map_err(|_| Error::Unavailable)?;
        self.phase(Phase::ManifestPublished);
        directory_sync(&self.directory)?;
        self.phase(Phase::ManifestDurable);
        self.manifest = next;
        reap(&self.directory, &self.manifest)?;
        Ok(())
    }
    fn segment(&self, start: usize, count: usize) -> Result<codec::Segment> {
        let end = start.checked_add(count).ok_or(Error::Invalid)?;
        let rows = self.index.documents[start..end]
            .iter()
            .map(|d| codec::Row {
                id: d.event.id,
                length: d.length,
                scope: d.scope,
                fingerprint: fingerprint(d.event.id),
            })
            .collect();
        let mut postings = BTreeMap::new();
        for (term, list) in &self.index.postings {
            let selected: Vec<_> = list
                .iter()
                .skip_while(|(r, _)| *r < start)
                .take_while(|(r, _)| *r < end)
                .map(|(r, tf)| ((*r - start) as u32, *tf))
                .collect();
            if !selected.is_empty() {
                postings.insert(term.clone(), selected);
            }
        }
        Ok(codec::Segment {
            start: start as u32,
            rows,
            postings,
        })
    }
}
fn segment_bytes(segment: &codec::Segment) -> Result<usize> {
    segment.postings.iter().try_fold(
        segment.rows.len().checked_mul(32).ok_or(Error::Invalid)?,
        |n, (t, p)| {
            n.checked_add(64)
                .and_then(|n| n.checked_add(t.len()))
                .and_then(|n| p.len().checked_mul(8).and_then(|v| n.checked_add(v)))
                .ok_or(Error::Invalid)
        },
    )
}
fn candidate_bytes(index: &Index) -> Result<usize> {
    index.postings.iter().try_fold(
        index
            .documents
            .len()
            .checked_mul(32)
            .ok_or(Error::Invalid)?,
        |n, (t, p)| {
            n.checked_add(64)
                .and_then(|n| n.checked_add(t.len()))
                .and_then(|n| p.len().checked_mul(8).and_then(|v| n.checked_add(v)))
                .ok_or(Error::Invalid)
        },
    )
}
fn identity(log: &CaptureLog) -> Result<[u8; 32]> {
    Ok(*blake3::hash(&serde_json::to_vec(log.identity()).map_err(|_| Error::Invalid)?).as_bytes())
}
fn fingerprint(id: u64) -> [u8; 12] {
    blake3::hash(id.to_string().as_bytes()).as_bytes()[..12]
        .try_into()
        .unwrap()
}
fn hex(id: &[u8]) -> String {
    id.iter().map(|x| format!("{x:02x}")).collect()
}
fn id_valid(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn manifest_aad(origin: [u8; 32], identity: [u8; 32]) -> Vec<u8> {
    let mut a = b"oojuro/native-lexical/manifest/v1\0".to_vec();
    a.extend(origin);
    a.extend(identity);
    a
}
fn segment_aad(manifest: &Manifest, r: &Reference) -> Result<Vec<u8>> {
    let mut a = b"oojuro/native-lexical/segment/v1\0".to_vec();
    a.extend(manifest.origin);
    a.extend(manifest.identity);
    a.extend(r.id.as_bytes());
    a.extend(serde_json::to_vec(&r.prefix).map_err(|_| Error::Invalid)?);
    a.extend(r.start.to_le_bytes());
    a.extend(r.rows.to_le_bytes());
    a.extend((r.bytes as u64).to_le_bytes());
    a.extend((r.decoded as u64).to_le_bytes());
    Ok(a)
}
fn load(
    log: &CaptureLog,
    dir: &Path,
    cipher: &FrameCipher,
    origin: [u8; 32],
    identity: [u8; 32],
) -> Result<(Index, Manifest)> {
    let sealed = read_bounded(&dir.join(MANIFEST), MANIFEST_BOUND + SEAL_OVERHEAD)?;
    let plain = cipher
        .open_frame(&manifest_aad(origin, identity), &sealed)
        .map_err(|_| Error::Invalid)?;
    let m: Manifest = serde_json::from_slice(&plain).map_err(|_| Error::Invalid)?;
    if m.format != FORMAT
        || m.origin != origin
        || m.identity != identity
        || m.rows > MAX_EVENTS
        || m.segments.len() > SEGMENTS_BOUND
        || log
            .cache_prefix(m.prefix.records)
            .map_err(|_| Error::Authority)?
            != m.prefix
    {
        return Err(Error::Authority);
    }
    let batches = log.normalized_batches().map_err(|_| Error::Unavailable)?;
    if m.prefix.batches > batches.len() {
        return Err(Error::Authority);
    }
    let mut index = Index {
        writer: log.identity().writer.clone(),
        agent: log.identity().agent,
        authority: log.instance(),
        applied: 0,
        failed: false,
        documents: vec![],
        postings: BTreeMap::new(),
        seen: HashMap::new(),
    };
    // Shared host/dedupe logic validates every row ID against the exact receipt
    // prefix; no tokenizer/stemmer runs on the valid-cache path.
    for batch in &batches[..m.prefix.batches] {
        index.apply_mode(batch, false)?;
    }
    index.applied = m.prefix.batches;
    if index.documents.len() != m.rows {
        return Err(Error::Authority);
    }
    let mut decoded = 0usize;
    let mut expected = 0usize;
    let mut total = 0usize;
    let mut ids = HashSet::new();
    let mut lengths = vec![0u64; m.rows];
    let mut scopes = vec![0u64; m.rows];
    for r in &m.segments {
        if !id_valid(&r.id)
            || !ids.insert(&r.id)
            || r.start as usize != expected
            || r.rows == 0
            || r.rows as usize > codec::MAX_ROWS
            || r.bytes > codec::MAX_BYTES
            || r.bytes < 192
            || r.prefix.records > m.prefix.records
            || log
                .cache_prefix(r.prefix.records)
                .map_err(|_| Error::Authority)?
                != r.prefix
        {
            return Err(Error::Invalid);
        }
        total = total.checked_add(r.bytes).ok_or(Error::Invalid)?;
        if total > CACHE_BOUND {
            return Err(Error::Invalid);
        }
        let end = expected
            .checked_add(r.rows as usize)
            .ok_or(Error::Invalid)?;
        if end > m.rows {
            return Err(Error::Invalid);
        }
        // A segment may never name rows authorized only in a later receipt.
        let authorized_next = batches
            .get(r.prefix.batches.wrapping_sub(1))
            .map_or(log.identity().first_id, |b| b.outcome.next_id);
        let sealed = read_bounded(
            &dir.join(format!("seg-{}.idx", r.id)),
            codec::MAX_BYTES + SEAL_OVERHEAD,
        )?;
        if sealed.len() != r.bytes + SEAL_OVERHEAD || *blake3::hash(&sealed).as_bytes() != r.digest
        {
            return Err(Error::Invalid);
        }
        let bytes = cipher
            .open_frame(&segment_aad(&m, r)?, &sealed)
            .map_err(|_| Error::Invalid)?;
        let segment = codec::decode(&bytes)?;
        let accounted = segment_bytes(&segment)?;
        if accounted != r.decoded {
            return Err(Error::Invalid);
        }
        decoded = decoded.checked_add(accounted).ok_or(Error::Invalid)?;
        if decoded > CACHE_BOUND {
            return Err(Error::Invalid);
        }
        if segment.start != r.start || segment.rows.len() != r.rows as usize {
            return Err(Error::Invalid);
        }
        for (offset, row) in segment.rows.iter().enumerate() {
            let doc = &mut index.documents[expected + offset];
            if row.id >= authorized_next
                || row.id != doc.event.id
                || row.fingerprint != fingerprint(doc.event.id)
            {
                return Err(Error::Authority);
            }
            doc.length = row.length;
            doc.scope = row.scope;
        }
        for (term, list) in segment.postings {
            let output = index.postings.entry(term).or_default();
            for (local, tf) in list {
                let row = expected + local as usize;
                if row >= end || tf == 0 {
                    return Err(Error::Invalid);
                }
                lengths[row] = lengths[row]
                    .checked_add(u64::from(tf))
                    .ok_or(Error::Invalid)?;
                scopes[row] += 1;
                output.push((row, tf));
            }
        }
        expected = end;
    }
    if expected != m.rows {
        return Err(Error::Invalid);
    }
    for (row, doc) in index.documents.iter().enumerate() {
        if lengths[row] != u64::from(doc.length)
            || scopes[row] != u64::from(doc.scope)
            || ((!doc.event.indexed || doc.event.text.is_empty())
                && (doc.length != 0 || doc.scope != 0))
        {
            return Err(Error::Invalid);
        }
    }
    Ok((index, m))
}
fn read_bounded(path: &Path, bound: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)?;
    let metadata = file.metadata().map_err(|_| Error::Unavailable)?;
    if !metadata.is_file() || metadata.len() > bound as u64 {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(bound as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    if bytes.len() > bound {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Unavailable)
}
fn directory_sync(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::Unavailable)
}
fn known(name: &str) -> bool {
    name == MANIFEST
        || name == "LOCK"
        || [("seg-", ".idx"), ("tmp-", ".idx"), ("tmp-", ".man")]
            .iter()
            .any(|(a, b)| {
                name.strip_prefix(a)
                    .and_then(|s| s.strip_suffix(b))
                    .is_some_and(id_valid)
            })
}
fn directory_lock(dir: &Path) -> Result<File> {
    match fs::symlink_metadata(dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => return Err(Error::Invalid),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|_| Error::Unavailable)?
        }
        Err(_) => return Err(Error::Unavailable),
    }
    for entry in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if !known(name) || !entry.file_type().map_err(|_| Error::Unavailable)?.is_file() {
            return Err(Error::Invalid);
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Unavailable)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("LOCK"))
        .map_err(|_| Error::Unavailable)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Unavailable);
    }
    Ok(lock)
}
fn reap(dir: &Path, m: &Manifest) -> Result<()> {
    let live: HashSet<_> = m
        .segments
        .iter()
        .map(|r| format!("seg-{}.idx", r.id))
        .collect();
    for entry in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if known(name) && name != "LOCK" && name != MANIFEST && !live.contains(name) {
            fs::remove_file(entry.path()).map_err(|_| Error::Unavailable)?;
        }
    }
    directory_sync(dir)
}
#[cfg(test)]
mod tests;
