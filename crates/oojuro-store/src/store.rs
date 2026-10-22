//! The store: a hot log of per-turn frames plus compacted cold segments.
//!
//! Directory layout (all integers little-endian):
//!
//! ```text
//! LOCK              exclusive advisory lock, one writer per directory
//! MANIFEST          magic "OJMAN001" | next_file u64 | hot_from u64 | n_cold u32 | cold ids u64*
//!                   | n_dict u32 | dict ids u32* | crc32 (replaced atomically by rename)
//! hot-<id>.ojl      header (32B) then one frame per appended turn batch
//! cold-<id>.ojc     header (32B), compacted frames, index, trailer (written once, fsynced)
//! dict-<id>.ojd     magic "OJDICT01" | id u32 | len u32 | crc32 | trained_at_raw u64 | zstd dictionary bytes
//! ```
//!
//! Durability: `append` writes one frame and calls `fdatasync` once per batch.
//! Hot logs with id >= `hot_from` are live and replayed in id order on open.
//! Only the last live hot log can end in a torn frame; the tail is truncated
//! when no valid frame follows it, otherwise open fails as corrupt. Compaction
//! writes and syncs a complete cold segment, then publishes it by replacing
//! MANIFEST, and only then deletes the hot logs it replaced.
//!
//! zstd frames are written magicless, without content size or dictionary id
//! (the frame head already carries both), so small per-turn frames stay small.

use crate::crypto::FrameCipher;
use crate::format::*;
use crate::pack::{PackOpts, Packer, Row, new_cctx, pack_body};
use crate::reader::{self, Frames, Loc, ReadState};
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use zstd::zstd_safe;

const HOT_MAGIC: &[u8; 8] = b"OJHOT001";
const COLD_MAGIC: &[u8; 8] = b"OJCOLD01";
const COLD_END: &[u8; 8] = b"OJCEND01";
const MAN_MAGIC: &[u8; 8] = b"OJMAN001";
const DICT_MAGIC: &[u8; 8] = b"OJDICT01";
/// Dictionary file whose body is sealed with the store key.
const DICT_MAGIC_SEALED: &[u8; 8] = b"OJDICT02";
/// Marker file of a store that holds sealed data: `OJENC001 | key id (8 B)`.
const ENC_MARKER: &str = "ENCRYPTED";
const ENC_MAGIC: &[u8; 8] = b"OJENC001";
/// magic | id | len | crc (over trained_at_raw + bytes) | trained_at_raw
const DICT_HEAD: usize = 28;
const FILE_HEAD: usize = 32;

#[derive(Clone, Debug)]
pub struct Options {
    /// zstd level for hot (per-turn) frames; 0 stores them uncompressed.
    pub hot_level: i32,
    /// zstd level for compacted cold frames.
    pub cold_level: i32,
    /// Target raw bytes per compacted frame.
    pub cold_frame_bytes: usize,
    /// Dictionary size in bytes; 0 disables dictionaries.
    pub dict_bytes: usize,
    /// Raw hot bytes appended before the first dictionary is trained.
    pub dict_train_after: usize,
    /// Rotate the active hot log after this many bytes.
    pub hot_file_bytes: u64,
    /// `fdatasync` after every appended batch.
    pub sync: bool,
    /// Decoded-frame cache budget in bytes (0 disables caching).
    pub cache_bytes: usize,
    /// Retrain the dictionary at compaction once the raw text added since the
    /// newest one was trained reaches `dict_spacing` x its size, so stored
    /// dictionaries stay near 1/dict_spacing of the raw text.
    pub dict_spacing: u64,
    /// A trained dictionary is about 1/dict_scale of the text it is trained
    /// on, capped by `dict_bytes` and at least 16 KiB.
    pub dict_scale: u64,
    /// Average FastCDC chunk size for cross-frame dedupe at compaction (0 = off).
    pub dedupe_chunk: u32,
    /// Literal bytes of the newest cold data whose chunks new data may reference.
    pub dedupe_window: u64,
    /// Threads compressing compacted frames (output is identical for any value).
    pub compact_threads: usize,
    /// Seal every new frame and dictionary with this key (compress first, then encrypt). A store that
    /// holds sealed data opens only with its key.
    pub cipher: Option<std::sync::Arc<FrameCipher>>,
    /// Reject plaintext frames/dictionaries instead of permitting a legacy plaintext upgrade.
    /// Requires `cipher`; used for stores that must have been sealed from their first byte.
    pub require_sealed: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            hot_level: 3,
            cold_level: 19,
            cold_frame_bytes: 64 << 10,
            dict_bytes: 512 << 10,
            dict_train_after: 4 << 20,
            hot_file_bytes: 64 << 20,
            sync: true,
            cache_bytes: 4 << 20,
            dict_spacing: 512,
            dict_scale: 256,
            dedupe_chunk: 1024,
            dedupe_window: 1 << 30,
            compact_threads: 4,
            cipher: None,
            require_sealed: false,
        }
    }
}

pub use crate::format::RecordIn as NewRecord;

/// A stored record returned by [`Store::record`] and [`Store::scan_time`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub kind: u8,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub key: Vec<u8>,
    pub attrs: Vec<u8>,
    pub text: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct FrameRef {
    first_seq: u64,
    count: u32,
    file: u64,
    /// Offset of the frame (its `len` field) in the file.
    offset: u64,
    payload_len: u32,
    raw_len: u32,
    min_valid: i64,
    max_valid: i64,
    cold: bool,
    /// Dictionary the frame was compressed with (0 = none).
    dict: u32,
}

impl FrameRef {
    fn loc(&self) -> Loc {
        Loc { first_seq: self.first_seq, count: self.count, file: self.file, offset: self.offset, payload_len: self.payload_len, raw_len: self.raw_len }
    }
}

impl Frames for Store {
    fn loc(&self, i: usize) -> Loc {
        self.frames[i].loc()
    }
    fn frame_index(&self, seq: u64) -> Result<usize> {
        Store::frame_index(self, seq)
    }
    fn file(&self, id: u64) -> &File {
        &self.files[&id]
    }
    fn dict(&self, id: u32) -> Option<&[u8]> {
        self.dicts.get(&id).map(|d| d.as_slice())
    }
    fn read_state(&self) -> &ReadState {
        &self.read
    }
    fn cipher(&self) -> Option<&FrameCipher> {
        self.opts.cipher.as_deref()
    }
    fn require_sealed(&self) -> bool {
        self.opts.require_sealed
    }
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub records: u64,
    pub raw_text_bytes: u64,
    pub hot_frames: usize,
    pub cold_frames: usize,
    pub disk_bytes: u64,
    pub dicts: usize,
}

#[derive(Default, Clone, Debug)]
pub struct CompactStats {
    pub records: u64,
    pub raw_bytes: u64,
    pub hot_bytes_before: u64,
    pub cold_bytes_after: u64,
    pub frames: usize,
    /// Text bytes stored as references to earlier literal bytes.
    pub ref_bytes: u64,
    pub seconds: f64,
}

struct Manifest {
    next_file: u64,
    hot_from: u64,
    cold: Vec<u64>,
    dicts: Vec<u32>,
}

pub struct Store {
    dir: PathBuf,
    opts: Options,
    _lock: File,
    man: Manifest,
    dicts: BTreeMap<u32, Vec<u8>>,
    /// Raw text bytes in the store when each dictionary was trained.
    dict_trained: BTreeMap<u32, u64>,
    files: HashMap<u64, File>,
    /// Live hot log ids, oldest first; the last one is active.
    hot_files: Vec<u64>,
    frames: Vec<FrameRef>,
    hot_len: u64,
    hot_cctx: zstd_safe::CCtx<'static>,
    hot_dict: u32,
    hot_raw: usize,
    next_seq: u64,
    raw_text: u64,
    read: ReadState,
    /// Set when a write or sync failed: the kernel may have dropped dirty
    /// pages, so no further writes are accepted until the store is reopened.
    poisoned: bool,
    /// A `CompactionJob` is outstanding.
    compacting: bool,
    /// Next dictionary id (ids are never reused within a process).
    next_dict: u32,
    /// Test hook: fail the next append after writing this many frame bytes.
    #[doc(hidden)]
    pub fail_next_append_after: Option<usize>,
}

fn crc(b: &[u8]) -> u32 {
    crc32fast::hash(b)
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn hot_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("hot-{id:08}.ojl"))
}
fn cold_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("cold-{id:08}.ojc"))
}
fn dict_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("dict-{id:08}.ojd"))
}

fn file_head(magic: &[u8; 8], id: u64, first_seq: u64) -> [u8; FILE_HEAD] {
    let mut h = [0u8; FILE_HEAD];
    h[..8].copy_from_slice(magic);
    h[8..16].copy_from_slice(&id.to_le_bytes());
    h[16..24].copy_from_slice(&first_seq.to_le_bytes());
    let c = crc(&h[..24]);
    h[24..28].copy_from_slice(&c.to_le_bytes());
    h
}

fn check_head(h: &[u8], magic: &[u8; 8], id: u64) -> Result<u64> {
    if h.len() < FILE_HEAD || &h[..8] != magic {
        return Err(Error::Corrupt("file magic/version mismatch"));
    }
    if crc(&h[..24]) != u32::from_le_bytes(h[24..28].try_into().unwrap()) {
        return Err(Error::Corrupt("file header checksum mismatch"));
    }
    if u64::from_le_bytes(h[8..16].try_into().unwrap()) != id {
        return Err(Error::Corrupt("file id mismatch"));
    }
    Ok(u64::from_le_bytes(h[16..24].try_into().unwrap()))
}

/// Build one frame payload; returns (payload, uncompressed body, meta_len).
fn build_payload(
    cctx: Option<&mut zstd_safe::CCtx<'static>>,
    dict: u32,
    first_seq: u64,
    records: &[RecordIn],
    digest: bool,
    cipher: Option<&FrameCipher>,
) -> Result<(Vec<u8>, Vec<u8>, usize)> {
    let (body, meta_len) = encode_body(records);
    let mut payload = pack_body(cctx, dict, first_seq, records.len(), &body, meta_len, digest)?;
    if let Some(c) = cipher {
        payload = crate::pack::encrypt_payload(c, &payload)?;
    }
    Ok((payload, body, meta_len))
}

fn valid_range(records: &[RecordIn]) -> (i64, i64) {
    records.iter().fold((i64::MAX, i64::MIN), |(lo, hi), r| (lo.min(r.valid_at), hi.max(r.valid_at)))
}

impl Store {
    /// A store that holds sealed data says so in `ENCRYPTED` (the key id, never the key): opening it
    /// without a key, or with another key, fails before anything is read. A key given to a store
    /// without the marker creates it, so frames appended from now on are sealed (older ones stay as they are).
    fn check_marker(dir: &Path, cipher: Option<&FrameCipher>) -> Result<()> {
        let path = dir.join(ENC_MARKER);
        match std::fs::read(&path) {
            Ok(b) => {
                if b.len() != 16 || &b[..8] != ENC_MAGIC {
                    return Err(Error::Corrupt("encryption marker"));
                }
                let c = cipher.ok_or(Error::KeyRequired)?;
                if b[8..16] != c.key_id() {
                    return Err(Error::WrongKey);
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(c) = cipher {
                    let mut b = Vec::with_capacity(16);
                    b.extend_from_slice(ENC_MAGIC);
                    b.extend_from_slice(&c.key_id());
                    let tmp = dir.join("ENCRYPTED.tmp");
                    std::fs::write(&tmp, &b)?;
                    File::open(&tmp)?.sync_all()?;
                    std::fs::rename(&tmp, &path)?;
                    sync_dir(dir)?;
                }
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn open(dir: &Path, opts: Options) -> Result<Self> {
        if opts.require_sealed {
            if opts.cipher.is_none() {
                return Err(Error::KeyRequired);
            }
            // Refuse an unmarked existing store before creating/upgrading its marker.
            if dir.join("MANIFEST").exists() && !dir.join(ENC_MARKER).exists() {
                return Err(Error::Crypto("strict sealed store has no encryption marker"));
            }
        }
        let cache_bytes = opts.cache_bytes;
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("LOCK"))?;
        lock.try_lock().map_err(|_| Error::Invalid("store is locked by another writer"))?;
        let man = match std::fs::read(dir.join("MANIFEST")) {
            Ok(b) => Self::parse_manifest(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let m = Manifest { next_file: 1, hot_from: 1, cold: Vec::new(), dicts: Vec::new() };
                Self::write_manifest(dir, &m)?;
                m
            }
            Err(e) => return Err(e.into()),
        };
        Self::check_marker(dir, opts.cipher.as_deref())?;
        let mut dicts = BTreeMap::new();
        let mut dict_trained = BTreeMap::new();
        for &id in &man.dicts {
            let b = std::fs::read(dict_path(dir, id))?;
            let sealed = b.len() >= 8 && &b[..8] == DICT_MAGIC_SEALED;
            if opts.require_sealed && !sealed {
                return Err(Error::Crypto("plaintext dictionary in strict sealed store"));
            }
            if b.len() < 20 || !(sealed || &b[..8] == DICT_MAGIC) || u32::from_le_bytes(b[8..12].try_into().unwrap()) != id {
                return Err(Error::Corrupt("dictionary header"));
            }
            let len = u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize;
            if crc(&b[20..]) != u32::from_le_bytes(b[16..20].try_into().unwrap()) {
                return Err(Error::Corrupt("dictionary checksum"));
            }
            let tail = if sealed {
                let c = opts.cipher.as_deref().ok_or(Error::KeyRequired)?;
                c.open_dict(&b[8..16], &b[20..])?
            } else {
                b[20..].to_vec()
            };
            if tail.len() != 8 + len {
                return Err(Error::Corrupt("dictionary length"));
            }
            dict_trained.insert(id, u64::from_le_bytes(tail[..8].try_into().unwrap()));
            dicts.insert(id, tail[8..].to_vec());
        }
        let mut store = Store {
            dir: dir.into(),
            opts,
            _lock: lock,
            hot_files: Vec::new(),
            hot_len: 0,
            hot_cctx: zstd_safe::CCtx::create(),
            hot_dict: 0,
            hot_raw: 0,
            next_seq: 0,
            raw_text: 0,
            frames: Vec::new(),
            files: HashMap::new(),
            dicts,
            dict_trained,
            man,
            read: ReadState::new(cache_bytes),
            poisoned: false,
            compacting: false,
            next_dict: 1,
            fail_next_append_after: None,
        };
        store.remove_orphans()?;
        for id in store.man.cold.clone() {
            store.load_cold(id)?;
        }
        // Live hot logs, in id order; only the last may have a torn tail.
        let mut hot: Vec<u64> = Self::list(dir, "hot-", ".ojl")?.into_iter().filter(|&i| i >= store.man.hot_from).collect();
        hot.sort_unstable();
        for (n, &id) in hot.iter().enumerate() {
            store.load_hot(id, n + 1 == hot.len())?;
        }
        store.hot_dict = store.dicts.keys().next_back().copied().unwrap_or(0);
        store.next_dict = store.hot_dict + 1;
        store.hot_cctx = new_cctx(store.opts.hot_level, store.dicts.get(&store.hot_dict).map(|d| d.as_slice()))?;
        match hot.last() {
            Some(&id) => store.hot_len = store.files[&id].metadata()?.len(),
            None => store.start_hot()?,
        }
        Ok(store)
    }

    fn hot_id(&self) -> u64 {
        *self.hot_files.last().unwrap()
    }

    fn list(dir: &Path, prefix: &str, suffix: &str) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(suffix)).and_then(|s| s.parse().ok()) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Delete files a crash left behind: unpublished cold segments and
    /// dictionaries, and hot logs already replaced by a published segment.
    fn remove_orphans(&self) -> Result<()> {
        let mut removed = false;
        for id in Self::list(&self.dir, "cold-", ".ojc")? {
            if !self.man.cold.contains(&id) {
                std::fs::remove_file(cold_path(&self.dir, id))?;
                removed = true;
            }
        }
        for id in Self::list(&self.dir, "dict-", ".ojd")? {
            if !self.man.dicts.contains(&(id as u32)) {
                std::fs::remove_file(dict_path(&self.dir, id as u32))?;
                removed = true;
            }
        }
        for id in Self::list(&self.dir, "hot-", ".ojl")? {
            if id < self.man.hot_from {
                std::fs::remove_file(hot_path(&self.dir, id))?;
                removed = true;
            }
        }
        if removed {
            sync_dir(&self.dir)?;
        }
        Ok(())
    }

    fn parse_manifest(b: &[u8]) -> Result<Manifest> {
        if b.len() < 36 || &b[..8] != MAN_MAGIC {
            return Err(Error::Corrupt("manifest magic/version mismatch"));
        }
        let body = &b[..b.len() - 4];
        if crc(body) != u32::from_le_bytes(b[b.len() - 4..].try_into().unwrap()) {
            return Err(Error::Corrupt("manifest checksum mismatch"));
        }
        let u64_at = |i: usize| u64::from_le_bytes(body[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(body[i..i + 4].try_into().unwrap());
        let next_file = u64_at(8);
        let hot_from = u64_at(16);
        let n_cold = u32_at(24) as usize;
        let mut at = 28;
        if at + n_cold * 8 + 4 > body.len() {
            return Err(Error::Corrupt("manifest truncated"));
        }
        let cold = (0..n_cold).map(|i| u64_at(at + 8 * i)).collect();
        at += 8 * n_cold;
        let n_dict = u32_at(at) as usize;
        at += 4;
        if at + n_dict * 4 != body.len() {
            return Err(Error::Corrupt("manifest length"));
        }
        let dicts = (0..n_dict).map(|i| u32_at(at + 4 * i)).collect();
        Ok(Manifest { next_file, hot_from, cold, dicts })
    }

    fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
        let mut b = Vec::new();
        b.extend_from_slice(MAN_MAGIC);
        b.extend_from_slice(&m.next_file.to_le_bytes());
        b.extend_from_slice(&m.hot_from.to_le_bytes());
        b.extend_from_slice(&(m.cold.len() as u32).to_le_bytes());
        for id in &m.cold {
            b.extend_from_slice(&id.to_le_bytes());
        }
        b.extend_from_slice(&(m.dicts.len() as u32).to_le_bytes());
        for id in &m.dicts {
            b.extend_from_slice(&id.to_le_bytes());
        }
        let c = crc(&b);
        b.extend_from_slice(&c.to_le_bytes());
        let tmp = dir.join("MANIFEST.tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(&b)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join("MANIFEST"))?;
        sync_dir(dir)
    }

    fn start_hot(&mut self) -> Result<()> {
        let id = self.man.next_file;
        self.man.next_file += 1;
        Self::write_manifest(&self.dir, &self.man)?;
        let path = hot_path(&self.dir, id);
        let mut f = OpenOptions::new().create_new(true).read(true).write(true).open(&path)?;
        f.write_all(&file_head(HOT_MAGIC, id, self.next_seq))?;
        f.sync_all()?;
        sync_dir(&self.dir)?;
        self.files.insert(id, f);
        self.hot_files.push(id);
        self.hot_len = FILE_HEAD as u64;
        Ok(())
    }

    /// Scan a hot log, verifying every frame. A bad frame at the tail of the
    /// last log is a torn write and is truncated; anything else is corruption.
    fn load_hot(&mut self, id: u64, last: bool) -> Result<()> {
        let path = hot_path(&self.dir, id);
        let f = OpenOptions::new().read(true).write(true).open(&path)?;
        let data = std::fs::read(&path)?;
        let first_seq = check_head(&data, HOT_MAGIC, id)?;
        if first_seq != self.next_seq {
            return Err(Error::Corrupt("hot log sequence gap"));
        }
        let mut at = FILE_HEAD;
        while at < data.len() {
            match Self::frame_at(&data, at) {
                Some(len) => {
                    let payload = &data[at + 8..at + 8 + len];
                    let head = get_head(payload)?;
                    if head.first_seq != self.next_seq {
                        return Err(Error::Corrupt("hot frame sequence gap"));
                    }
                    let (meta, _) = reader::decode_payload(&*self, payload, &head, None)?;
                    let (lo, hi) = meta.records.iter().fold((i64::MAX, i64::MIN), |(lo, hi), r| (lo.min(r.valid_at), hi.max(r.valid_at)));
                    let text: u64 = meta.records.iter().map(|r| r.text_len as u64).sum();
                    self.push_frame(id, at as u64, len as u32, &head, lo, hi, text);
                    at += 8 + len;
                }
                None => {
                    if !last || Self::any_frame_after(&data, at + 1) {
                        return Err(Error::Corrupt("hot log checksum mismatch before its tail"));
                    }
                    f.set_len(at as u64)?;
                    f.sync_all()?;
                    break;
                }
            }
        }
        self.files.insert(id, f);
        self.hot_files.push(id);
        Ok(())
    }

    /// Length of a valid frame starting at `at`, if any.
    fn frame_at(data: &[u8], at: usize) -> Option<usize> {
        let len = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        let want = u32::from_le_bytes(data.get(at + 4..at + 8)?.try_into().ok()?);
        if len == 0 || len > MAX_FRAME + 64 {
            return None;
        }
        let payload = data.get(at + 8..at + 8 + len)?;
        (frame_crc(payload) == want).then_some(len)
    }

    fn any_frame_after(data: &[u8], from: usize) -> bool {
        (from..data.len().saturating_sub(8)).any(|i| Self::frame_at(data, i).is_some())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_frame(&mut self, file: u64, offset: u64, payload_len: u32, head: &FrameHead, lo: i64, hi: i64, text: u64) {
        self.raw_text += text;
        self.frames.push(FrameRef {
            first_seq: head.first_seq,
            count: head.count,
            file,
            offset,
            payload_len,
            raw_len: head.raw_len,
            min_valid: lo,
            max_valid: hi,
            cold: false,
            dict: head.dict,
        });
        self.next_seq = head.first_seq + head.count as u64;
    }

    fn load_cold(&mut self, id: u64) -> Result<()> {
        let path = cold_path(&self.dir, id);
        let f = File::open(&path)?;
        let size = f.metadata()?.len();
        if size < (FILE_HEAD + 16) as u64 {
            return Err(Error::Corrupt("cold segment truncated"));
        }
        let mut head = [0u8; FILE_HEAD];
        f.read_exact_at(&mut head, 0)?;
        let first_seq = check_head(&head, COLD_MAGIC, id)?;
        if first_seq != self.next_seq {
            return Err(Error::Corrupt("cold segment sequence gap"));
        }
        let mut trailer = [0u8; 16];
        f.read_exact_at(&mut trailer, size - 16)?;
        if &trailer[8..] != COLD_END {
            return Err(Error::Corrupt("cold segment trailer"));
        }
        let index_len = u32::from_le_bytes(trailer[..4].try_into().unwrap()) as u64;
        let index_crc = u32::from_le_bytes(trailer[4..8].try_into().unwrap());
        if index_len + 16 + FILE_HEAD as u64 > size {
            return Err(Error::Corrupt("cold segment index length"));
        }
        let mut index = vec![0u8; index_len as usize];
        f.read_exact_at(&mut index, size - 16 - index_len)?;
        if crc(&index) != index_crc {
            return Err(Error::Corrupt("cold segment index checksum"));
        }
        let mut at = 0;
        let n = get_varint(&index, &mut at)? as usize;
        let seg_dict = get_varint(&index, &mut at)? as u32;
        if seg_dict != 0 && !self.dicts.contains_key(&seg_dict) {
            return Err(Error::Corrupt("cold segment names an unknown dictionary"));
        }
        let mut offset = FILE_HEAD as u64;
        let mut prev_valid = 0i64;
        for _ in 0..n {
            let payload_len = get_varint(&index, &mut at)? as u32;
            let count = get_varint(&index, &mut at)? as u32;
            let raw_len = get_varint(&index, &mut at)? as u32;
            let min_valid = prev_valid.wrapping_add(unzigzag(get_varint(&index, &mut at)?));
            let max_valid = min_valid.wrapping_add(unzigzag(get_varint(&index, &mut at)?));
            prev_valid = min_valid;
            self.frames.push(FrameRef { first_seq: self.next_seq, count, file: id, offset, payload_len, raw_len, min_valid, max_valid, cold: true, dict: seg_dict });
            self.next_seq += count as u64;
            offset += 8 + payload_len as u64;
        }
        if offset != size - 16 - index_len {
            return Err(Error::Corrupt("cold segment index does not cover its frames"));
        }
        self.raw_text += get_varint(&index, &mut at)?;
        self.files.insert(id, f);
        Ok(())
    }

    /// Append one turn batch durably (one write, one `fdatasync`); returns the
    /// first assigned sequence number. Records get consecutive numbers.
    pub fn append(&mut self, batch: &[NewRecord]) -> Result<u64> {
        if self.poisoned {
            return Err(Error::Invalid("an earlier write failed; reopen the store to recover"));
        }
        if batch.is_empty() {
            return Err(Error::Invalid("empty batch"));
        }
        if batch.iter().any(|r| r.kind > 0x7f) {
            return Err(Error::Invalid("record kind must be < 128"));
        }
        if self.hot_len >= self.opts.hot_file_bytes {
            self.files[&self.hot_id()].sync_all()?;
            self.start_hot()?;
        }
        let first = self.next_seq;
        let cctx = (self.opts.hot_level != 0).then_some(&mut self.hot_cctx);
        let (payload, body, _) = build_payload(cctx, self.hot_dict, first, batch, false, self.opts.cipher.as_deref())?;
        let frame = seal_frame(&payload);
        let id = self.hot_id();
        let f = &self.files[&id];
        self.poisoned = true;
        if let Some(n) = self.fail_next_append_after.take() {
            f.write_all_at(&frame[..n.min(frame.len())], self.hot_len)?;
            return Err(Error::Io(std::io::Error::other("injected write failure")));
        }
        f.write_all_at(&frame, self.hot_len)?;
        if self.opts.sync {
            f.sync_data()?;
        }
        self.poisoned = false;
        let head = get_head(&payload)?;
        let (lo, hi) = valid_range(batch);
        let text: u64 = batch.iter().map(|r| r.text.len() as u64).sum();
        let offset = self.hot_len;
        self.hot_len += frame.len() as u64;
        self.push_frame(id, offset, payload.len() as u32, &head, lo, hi, text);
        self.hot_raw += body.len();
        if self.opts.dict_bytes > 0 && self.hot_dict == 0 && self.hot_raw >= self.opts.dict_train_after {
            self.train_hot_dict()?;
        }
        Ok(first)
    }

    /// Train the first dictionary from the hot data appended so far. New hot
    /// frames use it; older frames keep decoding without one.
    fn train_hot_dict(&mut self) -> Result<()> {
        let mut texts = Vec::new();
        for i in 0..self.frames.len() {
            if self.frames[i].cold {
                continue;
            }
            let d = reader::decoded(self, i, usize::MAX)?;
            for r in 0..d.meta.records.len() {
                texts.push(d.body[d.meta.text_start[r] as usize..d.meta.text_start[r + 1] as usize].to_vec());
            }
        }
        let size = dict_size(&self.opts, texts.iter().map(|t| t.len() as u64).sum());
        let Some(dict) = train_dict(&texts, size) else { return Ok(()) };
        let id = self.next_dict;
        self.next_dict += 1;
        self.publish_dict(id, dict, self.raw_text)?;
        self.hot_dict = id;
        self.hot_cctx = new_cctx(self.opts.hot_level, Some(&self.dicts[&id]))?;
        Ok(())
    }

    fn publish_dict(&mut self, id: u32, dict: Vec<u8>, trained_at: u64) -> Result<u32> {
        let mut tail = Vec::with_capacity(dict.len() + 8);
        tail.extend_from_slice(&trained_at.to_le_bytes());
        tail.extend_from_slice(&dict);
        let mut head = Vec::with_capacity(16);
        head.extend_from_slice(&id.to_le_bytes());
        head.extend_from_slice(&(dict.len() as u32).to_le_bytes());
        if let Some(c) = self.opts.cipher.as_deref() {
            // a dictionary holds sample text of the store: sealed like a frame
            tail = c.seal_dict(&head, &tail);
        }
        let mut b = Vec::with_capacity(tail.len() + DICT_HEAD);
        b.extend_from_slice(if self.opts.cipher.is_some() { DICT_MAGIC_SEALED } else { DICT_MAGIC });
        b.extend_from_slice(&head);
        b.extend_from_slice(&crc(&tail).to_le_bytes());
        b.extend_from_slice(&tail);
        let mut f = File::create(dict_path(&self.dir, id))?;
        f.write_all(&b)?;
        f.sync_all()?;
        self.man.dicts.push(id);
        Self::write_manifest(&self.dir, &self.man)?;
        self.dicts.insert(id, dict);
        self.dict_trained.insert(id, trained_at);
        Ok(id)
    }

    /// Forget dictionaries no live frame (and not the hot tier) uses: drop them
    /// from MANIFEST first, then delete the files (orphans are removed on open).
    fn drop_unused_dicts(&mut self) -> Result<()> {
        let mut live: std::collections::BTreeSet<u32> = self.frames.iter().map(|f| f.dict).collect();
        live.insert(self.hot_dict);
        let newest = self.dicts.keys().next_back().copied();
        let dead: Vec<u32> = self.man.dicts.iter().copied().filter(|d| !live.contains(d) && Some(*d) != newest).collect();
        if dead.is_empty() {
            return Ok(());
        }
        self.man.dicts.retain(|d| !dead.contains(d));
        Self::write_manifest(&self.dir, &self.man)?;
        for d in dead {
            std::fs::remove_file(dict_path(&self.dir, d))?;
            self.dicts.remove(&d);
            self.dict_trained.remove(&d);
            self.read.forget_dict(d);
        }
        sync_dir(&self.dir)
    }

    fn frame_index(&self, seq: u64) -> Result<usize> {
        let i = self.frames.partition_point(|f| f.first_seq + f.count as u64 <= seq);
        if i < self.frames.len() && self.frames[i].first_seq <= seq { Ok(i) } else { Err(Error::NotFound) }
    }

    /// Bytes `start..end` of record `seq`'s text (clamped to its length).
    /// Decodes only the frame's meta section and the body up to the bytes it
    /// needs, plus the referenced bytes of a deduplicated record.
    pub fn get_range(&self, seq: u64, start: usize, end: usize) -> Result<Vec<u8>> {
        let i = self.frame_index(seq)?;
        reader::text_range(self, i, (seq - self.frames[i].first_seq) as usize, start, end)
    }

    pub fn get(&self, seq: u64) -> Result<Vec<u8>> {
        self.get_range(seq, 0, usize::MAX)
    }

    fn materialize(&self, i: usize, d: &reader::Decoded, r: usize) -> Result<Record> {
        let m = d.meta.records[r];
        let (ks, kl, al) = d.meta.side[r];
        let (ks, kl, al) = (ks as usize, kl as usize, al as usize);
        Ok(Record {
            seq: self.frames[i].first_seq + r as u64,
            kind: m.kind,
            valid_at: m.valid_at,
            recorded_at: m.recorded_at,
            key: d.body[ks..ks + kl].to_vec(),
            attrs: d.body[ks + kl..ks + kl + al].to_vec(),
            text: reader::text_range(self, i, r, 0, usize::MAX)?,
        })
    }

    pub fn record(&self, seq: u64) -> Result<Record> {
        let i = self.frame_index(seq)?;
        let d = reader::decoded(self, i, 0)?;
        self.materialize(i, &d, (seq - self.frames[i].first_seq) as usize)
    }

    /// Records with `from <= valid_at < to`, in sequence order, until `f` returns false.
    pub fn scan_time(&self, from: i64, to: i64, mut f: impl FnMut(&Record) -> bool) -> Result<()> {
        for i in 0..self.frames.len() {
            let fr = self.frames[i];
            if fr.max_valid < from || fr.min_valid >= to {
                continue;
            }
            let d = reader::decoded(self, i, usize::MAX)?;
            for r in 0..d.meta.records.len() {
                let m = d.meta.records[r];
                if m.valid_at >= from && m.valid_at < to && !f(&self.materialize(i, &d, r)?) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Read-only ciphertext incarnation fingerprint for disposable derived caches.
    /// Requires a healthy, strictly sealed nonempty store. Compaction may change
    /// it, deliberately invalidating caches; it is not a durable authority ID.
    pub fn sealed_origin_digest(&self) -> Result<[u8; 32]> {
        if self.poisoned || !self.opts.require_sealed || self.opts.cipher.is_none() {
            return Err(Error::Invalid("sealed origin requires healthy sealed store"));
        }
        let frame = self.frames.first().ok_or(Error::Invalid("empty sealed store"))?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"oojuro/sealed-origin/v1\0");
        let file = self.files.get(&frame.file).ok_or(Error::Corrupt("origin file"))?;
        let mut at = frame.offset;
        let mut left = u64::from(frame.payload_len) + 8;
        let mut buffer = [0u8; 8192];
        while left != 0 {
            let n = left.min(buffer.len() as u64) as usize;
            file.read_exact_at(&mut buffer[..n], at)?;
            hash.update(&buffer[..n]);
            at += n as u64;
            left -= n as u64;
        }
        Ok(*hash.finalize().as_bytes())
    }

    pub fn len(&self) -> u64 {
        self.next_seq
    }

    /// Turn per-append `fdatasync` on or off (bulk loads, benchmarks).
    pub fn set_sync(&mut self, sync: bool) {
        self.opts.sync = sync;
    }

    pub fn is_compacting(&self) -> bool {
        self.compacting
    }

    /// Raw bytes (meta + text) held in hot per-turn frames, for scheduling
    /// compaction: the hot tail compresses ~4-5x worse than compacted frames,
    /// so compacting every 8-16 MiB keeps it a small share of the store.
    pub fn hot_bytes(&self) -> u64 {
        self.frames.iter().filter(|f| !f.cold).map(|f| f.raw_len as u64).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.next_seq == 0
    }

    pub fn stats(&self) -> Result<Stats> {
        let mut disk = 0;
        for e in std::fs::read_dir(&self.dir)? {
            disk += e?.metadata()?.len();
        }
        Ok(Stats {
            records: self.next_seq,
            raw_text_bytes: self.raw_text,
            hot_frames: self.frames.iter().filter(|f| !f.cold).count(),
            cold_frames: self.frames.iter().filter(|f| f.cold).count(),
            disk_bytes: disk,
            dicts: self.dicts.len(),
        })
    }

    /// Rewrite every hot log into one cold segment. The active log is rotated
    /// first, so the segment covers everything appended so far.
    /// Rewrite every hot log into one cold segment, in the foreground:
    /// `begin_compaction` + `CompactionJob::run` + `finish_compaction`.
    pub fn compact(&mut self) -> Result<CompactStats> {
        let Some(job) = self.begin_compaction()? else {
            return Ok(CompactStats::default());
        };
        match job.run() {
            Ok(prepared) => self.finish_compaction(prepared),
            Err(e) => {
                self.abort_compaction();
                Err(e)
            }
        }
    }

    /// Rotate the hot log and snapshot what a compaction will rewrite: every
    /// hot log up to now. Quick; the returned job does the heavy work on any
    /// thread while appends continue into the new hot log.
    pub fn begin_compaction(&mut self) -> Result<Option<CompactionJob>> {
        if self.poisoned {
            return Err(Error::Invalid("an earlier write failed; reopen the store to recover"));
        }
        if self.compacting {
            return Err(Error::Invalid("a compaction is already running"));
        }
        let Some(first_hot) = self.frames.iter().position(|f| !f.cold) else {
            return Ok(None);
        };
        let old_hot = self.hot_files.clone();
        let mut hot_bytes_before = 0;
        for id in &old_hot {
            hot_bytes_before += self.files[id].metadata()?.len();
        }
        self.files[&self.hot_id()].sync_all()?;
        self.start_hot()?;
        let hot: Vec<FrameRef> = self.frames[first_hot..].to_vec();
        let mut from = first_hot;
        if self.opts.dedupe_chunk > 0 {
            let mut bytes = 0u64;
            while from > 0 && bytes + self.frames[from - 1].raw_len as u64 <= self.opts.dedupe_window {
                from -= 1;
                bytes += self.frames[from].raw_len as u64;
            }
        }
        let window: Vec<FrameRef> = self.frames[from..first_hot].to_vec();
        let mut files = HashMap::new();
        for f in hot.iter().chain(&window) {
            if let std::collections::hash_map::Entry::Vacant(v) = files.entry(f.file) {
                v.insert(self.files[&f.file].try_clone()?);
            }
        }
        // Reuse the newest dictionary until enough new text arrived to pay for a
        // fresh one (dictionaries are stored, so retraining at every compaction
        // can cost more than it saves).
        let latest = self.dicts.keys().next_back().copied();
        let retrain = self.opts.dict_bytes > 0
            && match latest {
                None => true,
                Some(id) => {
                    let since = self.raw_text.saturating_sub(self.dict_trained[&id]);
                    since >= (self.dicts[&id].len() as u64).saturating_mul(self.opts.dict_spacing.max(1))
                }
            };
        let new_dict_id = self.next_dict;
        if retrain {
            self.next_dict += 1;
        }
        let seg_id = self.man.next_file;
        self.man.next_file += 1;
        self.compacting = true;
        Ok(Some(CompactionJob {
            dir: self.dir.clone(),
            opts: self.opts.clone(),
            seg_id,
            hot,
            window,
            files,
            dicts: self.dicts.clone(),
            reuse: if self.opts.dict_bytes > 0 { latest } else { None },
            retrain,
            new_dict_id,
            old_hot,
            hot_bytes_before,
            trained_at: self.raw_text,
            started: std::time::Instant::now(),
        }))
    }

    /// Forget a job that failed or was dropped; its partial files are orphans
    /// that the next open removes.
    pub fn abort_compaction(&mut self) {
        self.compacting = false;
    }

    /// Publish a prepared segment: it replaces the hot logs the job read.
    pub fn finish_compaction(&mut self, p: PreparedSegment) -> Result<CompactStats> {
        if !self.compacting {
            return Err(Error::Invalid("no compaction is running"));
        }
        self.poisoned = true;
        let r = self.finish_inner(p);
        self.poisoned = r.is_err();
        self.compacting = false;
        r
    }

    fn finish_inner(&mut self, p: PreparedSegment) -> Result<CompactStats> {
        if let Some(d) = p.new_dict {
            self.publish_dict(p.dict_id, d, p.trained_at)?;
            // New hot frames use the newest dictionary too.
            self.hot_cctx = new_cctx(self.opts.hot_level, Some(&self.dicts[&p.dict_id]))?;
            self.hot_dict = p.dict_id;
        }
        self.man.cold.push(p.seg_id);
        self.man.hot_from = *self.hot_files.iter().find(|id| !p.old_hot.contains(id)).ok_or(Error::Invalid("no live hot log"))?;
        Self::write_manifest(&self.dir, &self.man)?;
        for id in &p.old_hot {
            self.files.remove(id);
            std::fs::remove_file(hot_path(&self.dir, *id))?;
        }
        sync_dir(&self.dir)?;
        self.hot_files.retain(|id| !p.old_hot.contains(id));
        self.files.insert(p.seg_id, File::open(&p.path)?);
        // Swap the compacted hot frames (a contiguous run) for the new cold ones.
        let a = self.frames.iter().position(|f| !f.cold).ok_or(Error::Invalid("compacted frames vanished"))?;
        let b = a + self.frames[a..].iter().take_while(|f| p.old_hot.contains(&f.file)).count();
        let cold: Vec<FrameRef> = p
            .entries
            .iter()
            .map(|&(payload_len, count, raw_len, min_valid, max_valid, offset, first_seq)| FrameRef {
                first_seq,
                count,
                file: p.seg_id,
                offset,
                payload_len,
                raw_len,
                min_valid,
                max_valid,
                cold: true,
                dict: p.dict_id,
            })
            .collect();
        if cold.first().map(|f| f.first_seq) != Some(self.frames[a].first_seq) || cold.iter().map(|f| f.count as u64).sum::<u64>() != self.frames[a..b].iter().map(|f| f.count as u64).sum::<u64>() {
            return Err(Error::Corrupt("prepared segment does not match the compacted frames"));
        }
        self.frames.splice(a..b, cold);
        self.read.clear_cache();
        self.drop_unused_dicts()?;
        Ok(CompactStats {
            records: p.records,
            raw_bytes: p.raw_total,
            hot_bytes_before: p.hot_bytes_before,
            cold_bytes_after: std::fs::metadata(&p.path)?.len(),
            frames: p.entries.len(),
            ref_bytes: p.ref_bytes,
            seconds: p.started.elapsed().as_secs_f64(),
        })
    }
}


/// A compaction prepared by [`Store::begin_compaction`]. `run` reads the hot
/// logs it captured (and a window of cold frames for dedupe) through its own
/// file handles, so it can run on another thread while the store keeps
/// appending; [`Store::finish_compaction`] then publishes the result.
pub struct CompactionJob {
    dir: PathBuf,
    opts: Options,
    seg_id: u64,
    hot: Vec<FrameRef>,
    window: Vec<FrameRef>,
    files: HashMap<u64, File>,
    dicts: BTreeMap<u32, Vec<u8>>,
    reuse: Option<u32>,
    retrain: bool,
    new_dict_id: u32,
    old_hot: Vec<u64>,
    hot_bytes_before: u64,
    /// Raw text in the store when the job began (what a new dictionary saw).
    trained_at: u64,
    started: std::time::Instant,
}

/// A written and synced, not yet published, cold segment.
pub struct PreparedSegment {
    seg_id: u64,
    path: PathBuf,
    entries: Vec<(u32, u32, u32, i64, i64, u64, u64)>,
    dict_id: u32,
    new_dict: Option<Vec<u8>>,
    trained_at: u64,
    old_hot: Vec<u64>,
    records: u64,
    raw_total: u64,
    ref_bytes: u64,
    hot_bytes_before: u64,
    started: std::time::Instant,
}

impl CompactionJob {
    /// Decode, dedupe, compress, verify and write the segment (synced, not
    /// yet published). Two streaming passes over the captured hot frames: a
    /// deterministic stride sample to train the dictionary (when due), then
    /// packing, so memory stays bounded by a few frame batches.
    pub fn run(self) -> Result<PreparedSegment> {
        let mut dctx = HashMap::new();
        let first_seq = self.hot.first().map(|f| f.first_seq).ok_or(Error::Invalid("nothing to compact"))?;
        let hot_raw: u64 = self.hot.iter().map(|f| f.raw_len as u64).sum();
        let records: u64 = self.hot.iter().map(|f| f.count as u64).sum();
        let mut new_dict = None;
        if self.retrain {
            let size = dict_size(&self.opts, hot_raw);
            // Every `step`-th record, about 100x the dictionary size in total.
            let step = (records * size as u64 * 100).checked_div(hot_raw.max(1)).map_or(1, |per| (records / per.max(1)).max(1));
            let (mut texts, mut n) = (Vec::new(), 0u64);
            for f in &self.hot {
                let (meta, body) = reader::read_frame_body(&self.files[&f.file], &f.loc(), &self.dicts, &mut dctx, self.opts.cipher.as_deref(), self.opts.require_sealed)?;
                for r in 0..meta.records.len() {
                    if n % step == 0 {
                        texts.push(body[meta.text_start[r] as usize..meta.text_start[r + 1] as usize].to_vec());
                    }
                    n += 1;
                }
            }
            new_dict = train_dict(&texts, size);
        }
        let (dict_id, dict_bytes): (u32, Option<Vec<u8>>) = match (&new_dict, self.reuse) {
            (Some(d), _) => (self.new_dict_id, Some(d.clone())),
            (None, Some(id)) => (id, Some(self.dicts[&id].clone())),
            _ => (0, None),
        };
        let opts = PackOpts {
            level: self.opts.cold_level,
            frame_bytes: self.opts.cold_frame_bytes,
            dedupe_chunk: self.opts.dedupe_chunk,
            threads: self.opts.compact_threads,
            cipher: self.opts.cipher.clone(),
        };
        let mut packer = Packer::new(opts, first_seq, dict_id, dict_bytes.as_deref());
        // Chunks of the newest cold data (within the window) that new frames may reference.
        for f in &self.window {
            let (meta, body) = reader::read_frame_body(&self.files[&f.file], &f.loc(), &self.dicts, &mut dctx, self.opts.cipher.as_deref(), self.opts.require_sealed)?;
            packer.register(f.first_seq, &meta, &body);
        }
        let path = cold_path(&self.dir, self.seg_id);
        let mut out = std::io::BufWriter::new(OpenOptions::new().create_new(true).write(true).open(&path)?);
        out.write_all(&file_head(COLD_MAGIC, self.seg_id, first_seq))?;
        let mut entries = Vec::new();
        let mut offset = FILE_HEAD as u64;
        let mut emit = |p: crate::pack::Packed| -> Result<()> {
            let frame = seal_frame(&p.payload);
            out.write_all(&frame)?;
            entries.push((p.payload.len() as u32, p.count, p.raw_len, p.lo, p.hi, offset, p.first_seq));
            offset += frame.len() as u64;
            Ok(())
        };
        for f in &self.hot {
            let (meta, body) = reader::read_frame_body(&self.files[&f.file], &f.loc(), &self.dicts, &mut dctx, self.opts.cipher.as_deref(), self.opts.require_sealed)?;
            for r in 0..meta.records.len() {
                if meta.pieces[r].0 != meta.pieces[r].1 {
                    return Err(Error::Corrupt("hot frame holds a deduplicated record"));
                }
                let (ks, kl, al) = meta.side[r];
                let (ks, kl, al) = (ks as usize, kl as usize, al as usize);
                let row = Row {
                    meta: meta.records[r],
                    key: body[ks..ks + kl].to_vec(),
                    attrs: body[ks + kl..ks + kl + al].to_vec(),
                    text: body[meta.text_start[r] as usize..meta.text_start[r + 1] as usize].to_vec(),
                };
                packer.push(row, &mut emit)?;
            }
        }
        packer.finish(&mut emit)?;
        drop(emit);
        let mut index = Vec::new();
        put_varint(&mut index, entries.len() as u64);
        put_varint(&mut index, dict_id as u64);
        let mut prev = 0i64;
        for &(pl, count, raw, lo, hi, _, _) in &entries {
            put_varint(&mut index, pl as u64);
            put_varint(&mut index, count as u64);
            put_varint(&mut index, raw as u64);
            put_varint(&mut index, zigzag(lo.wrapping_sub(prev)));
            put_varint(&mut index, zigzag(hi.wrapping_sub(lo)));
            prev = lo;
        }
        put_varint(&mut index, packer.raw_total);
        out.write_all(&index)?;
        out.write_all(&(index.len() as u32).to_le_bytes())?;
        out.write_all(&crc(&index).to_le_bytes())?;
        out.write_all(COLD_END)?;
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(PreparedSegment {
            seg_id: self.seg_id,
            path,
            entries,
            dict_id,
            new_dict,
            trained_at: self.trained_at,
            old_hot: self.old_hot,
            records: packer.records,
            raw_total: packer.raw_total,
            ref_bytes: packer.ref_bytes,
            hot_bytes_before: self.hot_bytes_before,
            started: self.started,
        })
    }
}

/// Dictionary size for `raw` bytes of training text (see `Options::dict_scale`).
fn dict_size(opts: &Options, raw: u64) -> usize {
    ((raw / opts.dict_scale.max(1)) as usize).clamp(16 << 10, opts.dict_bytes.max(16 << 10))
}

/// Train a zstd dictionary on a deterministic sample of `texts`
/// (about 100x the dictionary size). None when there is too little data.
pub fn train_dict(texts: &[Vec<u8>], dict_bytes: usize) -> Option<Vec<u8>> {
    let total: usize = texts.iter().map(|t| t.len()).sum();
    let usable = texts.iter().filter(|t| t.len() >= 8).count();
    if total < dict_bytes * 8 || usable < 16 {
        return None;
    }
    let (mut samples, mut sizes) = (Vec::new(), Vec::new());
    let mut rng = crate::util::Rng::new(0x0dd5_eed5);
    let budget = (dict_bytes * 100).min(total);
    let mut tries = 0;
    while samples.len() < budget && tries < texts.len() * 4 {
        tries += 1;
        let t = &texts[rng.below(texts.len())];
        let s = &t[..t.len().min(128 << 10)];
        if s.len() >= 8 {
            samples.extend_from_slice(s);
            sizes.push(s.len());
        }
    }
    zstd::dict::from_continuous(&samples, &sizes, dict_bytes).ok()
}
