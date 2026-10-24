//! Explicit opt-in, encrypted and disposable native Record segments.
//! RecordLog alone commits data. Cache failure never changes an authority ACK.
//! Bounded RAM merge, not the SPEC's external-memory multi-gigabyte engine.
use super::*;
use crate::lexical::{self, record_cache as projection, segments::codec};
use oojuro_store::crypto::{SEAL_OVERHEAD, random_key};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

const FORMAT: u32 = 1;
const MANIFEST_BOUND: usize = 1 << 20;
const CACHE_BOUND: usize = 128 << 20;
const SEGMENTS_BOUND: usize = 1024;
const MANIFEST: &str = "RECORD_INDEX";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: String,
    prefix: Watermark,
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
    prefix: Watermark,
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
/// No cache file is consulted to recover or acknowledge Record data.
pub struct PersistentIndex {
    index: Index,
    manifest: Manifest,
    directory: PathBuf,
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
    pub fn open(log: &RecordLog, directory: &Path) -> Result<Self> {
        let tip = log.tip()?;
        let origin = tip.origin;
        let identity = tip.identity;
        let lock = directory_lock(directory)?;
        let loaded = load(log, directory, origin, identity);
        let (index, manifest, opened) = match loaded {
            Ok((index, manifest)) => {
                let opened = if same_prefix(&manifest.prefix, &tip) {
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
            _lock: lock,
            opened,
            failed: false,
            #[cfg(test)]
            hook: None,
        };
        if opened != Opened::Loaded {
            cache.extend(log)?;
            cache.publish(log, opened == Opened::Rebuilt)?;
        }
        Ok(cache)
    }
    pub fn opened(&self) -> Opened {
        self.opened
    }
    pub fn segment_count(&self) -> usize {
        self.manifest.segments.len()
    }
    /// A current authority append makes this cache unreadable until refresh.
    /// The exact committed prefix is authenticated, not merely its row count.
    pub fn search(&self, log: &RecordLog, query: &Query<'_>) -> Result<Search> {
        self.current(log)?;
        self.index.search(log, query)
    }
    pub fn search_view(&self, log: &RecordLog, view: &View, query: &Query<'_>) -> Result<Search> {
        self.current(log)?;
        self.index.search_view(log, view, query)
    }
    fn current(&self, log: &RecordLog) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        log.validate(&self.manifest.prefix)?;
        if !same_prefix(&self.manifest.prefix, &log.tip()?) {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    fn extend(&mut self, log: &RecordLog) -> Result<()> {
        let start = log.validate(&self.index.mark)?;
        self.index
            .inner
            .append_record_rows(
                log.records[start..].iter().map(|r| {
                    (
                        r.id,
                        r.input.valid_at,
                        r.input.text.as_str(),
                        r.input.indexed,
                    )
                }),
                true,
            )
            .map_err(map_codec)?;
        self.index.mark = log.tip()?;
        Ok(())
    }
    /// Projects only newly committed typed rows. A publication failure
    /// invalidates this handle; reopen/rebuild instead of serving old results.
    pub fn refresh(&mut self, log: &RecordLog) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if self.current(log).is_ok() {
            return Ok(());
        }
        let result = (|| {
            self.extend(log)?;
            self.publish(log, false)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    /// Bounded merge re-encodes the current native RAM projection (<=100k rows,
    /// <=128MiB encoded candidate). It is not an external-memory streaming merge.
    pub fn merge(&mut self, log: &RecordLog) -> Result<()> {
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
    fn publish(&mut self, log: &RecordLog, replace: bool) -> Result<()> {
        let prefix = log.tip()?;
        let origin = prefix.origin;
        let mut next = Manifest {
            format: FORMAT,
            origin,
            identity: prefix.identity,
            prefix: prefix.clone(),
            rows: log.records.len(),
            segments: if replace {
                vec![]
            } else {
                self.manifest.segments.clone()
            },
        };
        if next.rows > MAX_RECORDS {
            return Err(Error::Invalid);
        }
        if projection::candidate_bytes(&self.index.inner).map_err(map_codec)? > CACHE_BOUND {
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
                let data =
                    projection::snapshot(&self.index.inner, start, count).map_err(map_codec)?;
                match codec::encode(&data) {
                    Ok(bytes) => {
                        break (bytes, projection::segment_bytes(&data).map_err(map_codec)?);
                    }
                    Err(_) if count > 1 => count = count.div_ceil(2),
                    Err(error) => return Err(map_codec(error)),
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
            let sealed = log.seal(&aad, &plain);
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
        let sealed = log.seal(&manifest_aad(origin, next.identity), &plain);
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
}
fn map_codec(e: lexical::Error) -> Error {
    match e {
        lexical::Error::Invalid => Error::Bounds,
        lexical::Error::Authority => Error::Authority,
        lexical::Error::Unavailable => Error::Unavailable,
    }
}
fn same_prefix(a: &Watermark, b: &Watermark) -> bool {
    // Callers authenticate a against the independently opened RecordLog first.
    a.origin == b.origin
        && a.identity == b.identity
        && a.sequence == b.sequence
        && a.records == b.records
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
    let mut a = b"oojuro/native-record/manifest/v1\0".to_vec();
    a.extend(origin);
    a.extend(identity);
    a
}
fn segment_aad(manifest: &Manifest, r: &Reference) -> Result<Vec<u8>> {
    let mut a = b"oojuro/native-record/segment/v1\0".to_vec();
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
    log: &RecordLog,
    dir: &Path,
    origin: [u8; 32],
    identity: [u8; 32],
) -> Result<(Index, Manifest)> {
    let sealed = read_bounded(&dir.join(MANIFEST), MANIFEST_BOUND + SEAL_OVERHEAD)?;
    let plain = log.unseal(&manifest_aad(origin, identity), &sealed)?;
    let m: Manifest = serde_json::from_slice(&plain).map_err(|_| Error::Invalid)?;
    if m.format != FORMAT
        || m.origin != origin
        || m.identity != identity
        || m.rows > MAX_RECORDS
        || m.segments.len() > SEGMENTS_BOUND
        || log.validate(&m.prefix)? != m.rows
    {
        return Err(Error::Authority);
    }
    let id = log.identity();
    let inner = lexical::Index::borrowed_record_rows(
        &id.writer,
        id.agent,
        log.records[..m.rows].iter().map(|r| {
            (
                r.id,
                r.input.valid_at,
                r.input.text.as_str(),
                r.input.indexed,
            )
        }),
        false,
    )
    .map_err(map_codec)?;
    let mut index = Index {
        inner,
        mark: m.prefix.clone(),
        failed: false,
    };
    let (mut decoded, mut expected, mut total) = (0usize, 0usize, 0usize);
    let mut ids = HashSet::new();
    for r in &m.segments {
        let authorized = log.validate(&r.prefix)?;
        if !id_valid(&r.id)
            || !ids.insert(&r.id)
            || r.start as usize != expected
            || r.rows == 0
            || r.rows as usize > codec::MAX_ROWS
            || r.bytes > codec::MAX_BYTES
            || r.bytes < 192
            || r.prefix.sequence > m.prefix.sequence
            || authorized > m.rows
        {
            return Err(Error::Invalid);
        }
        total = total.checked_add(r.bytes).ok_or(Error::Bounds)?;
        if total > CACHE_BOUND {
            return Err(Error::Bounds);
        }
        let end = expected.checked_add(r.rows as usize).ok_or(Error::Bounds)?;
        if end > m.rows || end > authorized {
            return Err(Error::Authority);
        }
        let sealed = read_bounded(
            &dir.join(format!("seg-{}.idx", r.id)),
            codec::MAX_BYTES + SEAL_OVERHEAD,
        )?;
        if sealed.len() != r.bytes + SEAL_OVERHEAD || *blake3::hash(&sealed).as_bytes() != r.digest
        {
            return Err(Error::Invalid);
        }
        let bytes = log.unseal(&segment_aad(&m, r)?, &sealed)?;
        let segment = codec::decode(&bytes).map_err(map_codec)?;
        let accounted = projection::segment_bytes(&segment).map_err(map_codec)?;
        if accounted != r.decoded {
            return Err(Error::Invalid);
        }
        decoded = decoded.checked_add(accounted).ok_or(Error::Bounds)?;
        if decoded > CACHE_BOUND {
            return Err(Error::Bounds);
        }
        if segment.start != r.start || segment.rows.len() != r.rows as usize {
            return Err(Error::Invalid);
        }
        projection::hydrate(&mut index.inner, segment, &|row, id| {
            Index::original_text(log, row, id)
        })
        .map_err(map_codec)?;
        expected = end;
    }
    if expected != m.rows {
        return Err(Error::Invalid);
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
