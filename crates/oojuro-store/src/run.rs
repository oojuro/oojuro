//! Immutable run files: one self-contained compressed file per run of rows
//! (opaque byte strings, for example an engine's encoded records) with random
//! access by row index. Shaped like the lexical engine's `Archive` / `Run`
//! storage boundary (write a run from an iterator, then `get`, `scan`), so an
//! adapter there is a few lines.
//!
//! ```text
//! file    := "OJRUN001" | frames | dictionary | index | trailer (32 B)
//! index   := frames varint | rows varint | raw varint | dict varint (0/1/2)
//!            | [shared dictionary id, 16 B, when dict = 2]
//!            | per frame: payload_len, count, raw_len (varints)
//! trailer := dict_len u32 | dict_crc u32 | index_len u32 | index_crc u32
//!            | reserved u64 | "OJRUNEND"
//! ```
//! Frames are the store's frame format (`format.rs`) with row indexes as
//! sequence numbers. Dedupe references stay inside the run. The dictionary
//! is either embedded (dict = 1, trained on the run) or a shared dictionary
//! shipped with the code (dict = 2, e.g. the starter dictionary for new
//! stores), named by the first 16 bytes of its BLAKE3; such a run opens only
//! with that dictionary.

use crate::format::*;
use crate::pack::{PackOpts, Packed, Packer, Row};
use crate::reader::{self, Frames, Loc, ReadState};
use crate::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;

const RUN_MAGIC: &[u8; 8] = b"OJRUN001";
const RUN_END: &[u8; 8] = b"OJRUNEND";
const TRAILER: usize = 32;

/// A dictionary shared by many runs and not stored in them (the starter
/// dictionary): bytes plus their 16-byte BLAKE3 id, prepared once for
/// decoding (every reader references the same tables) and once per level
/// for encoding. The prepared forms live for the rest of the process.
#[derive(Clone)]
pub struct SharedDict {
    pub id: [u8; 16],
    pub bytes: std::sync::Arc<[u8]>,
    pub(crate) ddict: &'static zstd::zstd_safe::DDict<'static>,
    cdicts: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<i32, &'static zstd::zstd_safe::CDict<'static>>>>,
}

impl SharedDict {
    pub fn new(bytes: &[u8]) -> Self {
        Self {
            id: digest16(bytes),
            bytes: bytes.into(),
            ddict: Box::leak(Box::new(zstd::zstd_safe::DDict::create(bytes))),
            cdicts: Default::default(),
        }
    }

    pub(crate) fn cdict(&self, level: i32) -> &'static zstd::zstd_safe::CDict<'static> {
        self.cdicts.lock().unwrap().entry(level).or_insert_with(|| Box::leak(Box::new(zstd::zstd_safe::CDict::create(&self.bytes, level))))
    }
}

impl std::fmt::Debug for SharedDict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedDict({} B)", self.bytes.len())
    }
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub level: i32,
    /// Target literal bytes per frame.
    pub frame_bytes: usize,
    /// Average FastCDC chunk size for dedupe inside the run (0 = off).
    pub dedupe_chunk: u32,
    /// Dictionary cap (0 = no dictionary); its size is about 1/`dict_scale`
    /// of the rows it is trained on.
    pub dict_bytes: usize,
    pub dict_scale: u64,
    /// Rows buffered (by bytes) to train the dictionary before streaming.
    pub dict_sample: usize,
    pub threads: usize,
    /// Used when no dictionary is trained (`dict_bytes` = 0).
    pub shared_dict: Option<SharedDict>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            level: 19,
            frame_bytes: 64 << 10,
            dedupe_chunk: 1024,
            dict_bytes: 512 << 10,
            dict_scale: 256,
            dict_sample: 64 << 20,
            threads: 2,
            shared_dict: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunInfo {
    pub rows: u64,
    pub raw_bytes: u64,
    pub file_bytes: u64,
}

fn row(bytes: Vec<u8>) -> Row {
    let meta = RecordMeta { kind: 0, valid_at: 0, recorded_at: 0, text_len: bytes.len() as u32 };
    Row { meta, key: Vec::new(), attrs: Vec::new(), text: bytes }
}

/// Durably write one run (`sync_all` before returning). Fails if `path` exists.
pub fn write_run(path: &Path, rows: &mut dyn Iterator<Item = Vec<u8>>, opts: &RunOptions) -> Result<RunInfo> {
    let mut first: Vec<Vec<u8>> = Vec::new();
    let mut buffered = 0usize;
    if opts.dict_bytes > 0 {
        while let Some(r) = rows.next() {
            if r.len() > MAX_FRAME {
                return Err(Error::Invalid("row exceeds the 64 MiB frame limit"));
            }
            buffered += r.len();
            first.push(r);
            if buffered >= opts.dict_sample {
                break;
            }
        }
    }
    let size = ((buffered as u64 / opts.dict_scale.max(1)) as usize).clamp(16 << 10, opts.dict_bytes.max(16 << 10));
    let dict = if opts.dict_bytes > 0 { crate::store::train_dict(&first, size) } else { None };
    let shared = if dict.is_none() { opts.shared_dict.as_ref() } else { None };
    let dict_id = if dict.is_some() {
        1
    } else if shared.is_some() {
        2
    } else {
        0
    };
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    let mut out = std::io::BufWriter::new(&mut file);
    out.write_all(RUN_MAGIC)?;
    let mut entries: Vec<(u32, u32, u32)> = Vec::new();
    let mut emit = |p: Packed| -> Result<()> {
        out.write_all(&seal_frame(&p.payload))?;
        entries.push((p.payload.len() as u32, p.count, p.raw_len));
        Ok(())
    };
    let pack = PackOpts { level: opts.level, frame_bytes: opts.frame_bytes, dedupe_chunk: opts.dedupe_chunk, threads: opts.threads, cipher: None };
    let mut packer = Packer::new(pack, 0, dict_id, dict.as_deref().or(shared.map(|d| &d.bytes[..])));
    packer.shared = shared.cloned();
    for r in first.into_iter().chain(rows) {
        if r.len() > MAX_FRAME {
            return Err(Error::Invalid("row exceeds the 64 MiB frame limit"));
        }
        packer.push(row(r), &mut emit)?;
    }
    packer.finish(&mut emit)?;
    drop(emit);
    let dict_bytes: &[u8] = dict.as_deref().unwrap_or(&[]);
    out.write_all(dict_bytes)?;
    let mut index = Vec::new();
    put_varint(&mut index, entries.len() as u64);
    put_varint(&mut index, packer.records);
    put_varint(&mut index, packer.raw_total);
    put_varint(&mut index, dict_id as u64);
    if let Some(d) = shared {
        index.extend_from_slice(&d.id);
    }
    for &(pl, count, raw) in &entries {
        put_varint(&mut index, pl as u64);
        put_varint(&mut index, count as u64);
        put_varint(&mut index, raw as u64);
    }
    out.write_all(&index)?;
    let mut trailer = [0u8; TRAILER];
    trailer[..4].copy_from_slice(&(dict_bytes.len() as u32).to_le_bytes());
    trailer[4..8].copy_from_slice(&crc32fast::hash(dict_bytes).to_le_bytes());
    trailer[8..12].copy_from_slice(&(index.len() as u32).to_le_bytes());
    trailer[12..16].copy_from_slice(&crc32fast::hash(&index).to_le_bytes());
    trailer[24..].copy_from_slice(RUN_END);
    out.write_all(&trailer)?;
    out.flush()?;
    drop(out);
    file.sync_all()?;
    Ok(RunInfo { rows: packer.records, raw_bytes: packer.raw_total, file_bytes: file.metadata()?.len() })
}

/// An opened, immutable run. Row indexes are relative to the run.
pub struct RunReader {
    file: File,
    frames: Vec<Loc>,
    dict: Option<Vec<u8>>,
    shared: Option<SharedDict>,
    rows: u64,
    raw_bytes: u64,
    file_bytes: u64,
    read: ReadState,
}

impl Frames for RunReader {
    fn loc(&self, i: usize) -> Loc {
        self.frames[i]
    }
    fn frame_index(&self, seq: u64) -> Result<usize> {
        let i = self.frames.partition_point(|f| f.first_seq + f.count as u64 <= seq);
        if i < self.frames.len() && self.frames[i].first_seq <= seq { Ok(i) } else { Err(Error::NotFound) }
    }
    fn file(&self, _: u64) -> &File {
        &self.file
    }
    fn dict(&self, id: u32) -> Option<&[u8]> {
        match id {
            1 => self.dict.as_deref(),
            2 => self.shared.as_ref().map(|d| &d.bytes[..]),
            _ => None,
        }
    }
    fn ddict(&self, id: u32) -> Option<&'static zstd::zstd_safe::DDict<'static>> {
        (id == 2).then(|| self.shared.as_ref().map(|d| d.ddict)).flatten()
    }
    fn read_state(&self) -> &ReadState {
        &self.read
    }
}

impl RunReader {
    /// Open a run; `cache_bytes` bounds its decoded-frame cache.
    pub fn open(path: &Path, cache_bytes: usize) -> Result<Self> {
        Self::open_with(path, cache_bytes, None)
    }

    /// Open a run that may use `shared` (it must be the dictionary the run
    /// names, else `Corrupt`).
    pub fn open_with(path: &Path, cache_bytes: usize, shared: Option<&SharedDict>) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < (RUN_MAGIC.len() + TRAILER) as u64 {
            return Err(Error::Corrupt("run file truncated"));
        }
        let mut magic = [0u8; 8];
        file.read_exact_at(&mut magic, 0)?;
        let mut trailer = [0u8; TRAILER];
        file.read_exact_at(&mut trailer, size - TRAILER as u64)?;
        if &magic != RUN_MAGIC || &trailer[24..] != RUN_END {
            return Err(Error::Corrupt("run magic/version mismatch"));
        }
        let u32_at = |i: usize| u32::from_le_bytes(trailer[i..i + 4].try_into().unwrap());
        let (dict_len, dict_crc, index_len, index_crc) = (u32_at(0) as u64, u32_at(4), u32_at(8) as u64, u32_at(12));
        let tail = dict_len + index_len + TRAILER as u64;
        if tail + RUN_MAGIC.len() as u64 > size {
            return Err(Error::Corrupt("run trailer lengths"));
        }
        let mut index = vec![0u8; index_len as usize];
        file.read_exact_at(&mut index, size - TRAILER as u64 - index_len)?;
        let mut dict = vec![0u8; dict_len as usize];
        file.read_exact_at(&mut dict, size - tail)?;
        if crc32fast::hash(&index) != index_crc || crc32fast::hash(&dict) != dict_crc {
            return Err(Error::Corrupt("run index or dictionary checksum"));
        }
        let mut at = 0;
        let n = get_varint(&index, &mut at)? as usize;
        let rows = get_varint(&index, &mut at)?;
        let raw_bytes = get_varint(&index, &mut at)?;
        let flag = get_varint(&index, &mut at)?;
        let has_dict = flag == 1;
        if flag > 2 || has_dict == dict.is_empty() || n > index.len() {
            return Err(Error::Corrupt("run dictionary flag"));
        }
        let shared = if flag == 2 {
            let id: [u8; 16] = index.get(at..at + 16).ok_or(Error::Corrupt("run shared dictionary id"))?.try_into().unwrap();
            at += 16;
            match shared {
                Some(d) if d.id == id => Some(d.clone()),
                _ => return Err(Error::Corrupt("run needs a shared dictionary that was not given")),
            }
        } else {
            None
        };
        let mut frames = Vec::with_capacity(n);
        let (mut offset, mut first_seq) = (RUN_MAGIC.len() as u64, 0u64);
        for _ in 0..n {
            let payload_len = get_varint(&index, &mut at)? as u32;
            let count = get_varint(&index, &mut at)? as u32;
            let raw_len = get_varint(&index, &mut at)? as u32;
            frames.push(Loc { first_seq, count, file: 0, offset, payload_len, raw_len });
            first_seq += count as u64;
            offset += 8 + payload_len as u64;
        }
        if at != index.len() || offset != size - tail || first_seq != rows {
            return Err(Error::Corrupt("run index does not cover its frames"));
        }
        Ok(Self { file, frames, dict: has_dict.then_some(dict), shared, rows, raw_bytes, file_bytes: size, read: ReadState::new(cache_bytes) })
    }

    pub fn len(&self) -> u64 {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn raw_bytes(&self) -> u64 {
        self.raw_bytes
    }

    pub fn file_bytes(&self) -> u64 {
        self.file_bytes
    }

    /// Exact bytes of row `i`.
    pub fn get(&self, i: u64) -> Result<Vec<u8>> {
        self.get_range(i, 0, usize::MAX)
    }

    /// Bytes `start..end` (clamped) of row `i`; decodes only what is needed.
    pub fn get_range(&self, i: u64, start: usize, end: usize) -> Result<Vec<u8>> {
        let f = self.frame_index(i)?;
        reader::text_range(self, f, (i - self.frames[f].first_seq) as usize, start, end)
    }

    /// Visit rows `start..end` in order.
    pub fn scan(&self, start: u64, end: u64, visit: &mut dyn FnMut(u64, &[u8]) -> Result<()>) -> Result<()> {
        let end = end.min(self.rows);
        let mut i = start;
        while i < end {
            let f = self.frame_index(i)?;
            let loc = self.frames[f];
            let d = reader::decoded(self, f, usize::MAX)?;
            while i < end && i < loc.first_seq + loc.count as u64 {
                let r = (i - loc.first_seq) as usize;
                if d.meta.pieces[r].0 == d.meta.pieces[r].1 {
                    visit(i, &d.body[d.meta.text_start[r] as usize..d.meta.text_start[r + 1] as usize])?;
                } else {
                    visit(i, &reader::text_range(self, f, r, 0, usize::MAX)?)?;
                }
                i += 1;
            }
        }
        Ok(())
    }
}
