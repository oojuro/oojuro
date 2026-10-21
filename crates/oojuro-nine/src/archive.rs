//! Storage boundary for exact originals of immutable segments.
//!
//! The engine hands an archive one run of encoded records (one per row, in
//! row order) when it flushes or merges a segment, and later asks for single
//! rows or row ranges. Codecs (for example the compression track's) plug in
//! by implementing [`Archive`] and [`Run`]; the engine stores the codec name
//! in its manifest and never interprets archive files itself.
//!
//! The default [`BlockArchive`] is deliberately plain: LZ4 blocks of about
//! 64 KiB, one BLAKE3 digest per block checked on every decode, and a shared
//! byte-bounded decode cache.
use crate::codec::{get_varint, put_varint};
use oojuro_core::Result;
use std::{
    collections::{HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Writes and opens immutable runs of originals.
pub trait Archive: Send + Sync {
    /// Stable codec identifier recorded in the manifest.
    fn name(&self) -> &str;
    /// Durably write one run (`sync_all` before returning). `originals`
    /// yields every row of the run in order.
    fn write(&self, path: &Path, originals: &mut dyn Iterator<Item = Vec<u8>>) -> Result<RunInfo>;
    fn open(&self, path: &Path) -> Result<Box<dyn Run>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RunInfo {
    pub rows: u64,
    pub raw_bytes: u64,
    pub file_bytes: u64,
}

/// An opened, immutable run. Row indexes are relative to the run.
pub trait Run: Send + Sync {
    fn len(&self) -> u64;
    fn file_bytes(&self) -> u64;
    /// Exact bytes of row `i`.
    fn get(&self, i: u64) -> Result<Vec<u8>>;
    /// Exact bytes of row `i` for a point read: a run whose rows share
    /// compressed frames may decode only as much of the frame as this row
    /// needs (the next read of the same frame then decodes again), where
    /// [`Run::get`] decodes and caches the whole frame for its neighbours.
    fn get_point(&self, i: u64) -> Result<Vec<u8>> {
        self.get(i)
    }
    /// Visit rows `start..end` in order.
    fn scan(&self, start: u64, end: u64, visit: &mut dyn FnMut(u64, &[u8]) -> Result<()>)
    -> Result<()>;
}

const MAGIC: [u8; 8] = *b"NINEA004";
const END: [u8; 8] = *b"NINEAEND";
const INDEX_ENTRY: usize = 8 + 4 + 4 + 8 + 4 + 32;
const FOOTER: usize = 8 + 8 + 8 + 4 + 4 + 8;

struct Decoded {
    data: Vec<u8>,
    /// Start offsets of each row inside `data`, plus the end.
    offsets: Vec<u32>,
}

/// Byte-bounded FIFO cache of decoded blocks shared by every run.
pub struct BlockCache {
    budget: usize,
    inner: Mutex<CacheInner>,
}

type CacheInner = (HashMap<(u64, u32), Arc<Decoded>>, VecDeque<(u64, u32)>, usize);

impl BlockCache {
    pub fn new(budget: usize) -> Arc<Self> {
        Arc::new(Self {
            budget,
            inner: Mutex::new((HashMap::new(), VecDeque::new(), 0)),
        })
    }
    fn get(&self, key: (u64, u32)) -> Option<Arc<Decoded>> {
        self.inner.lock().ok()?.0.get(&key).cloned()
    }
    fn put(&self, key: (u64, u32), block: Arc<Decoded>) {
        let size = block.data.len() + 4 * block.offsets.len();
        if size > self.budget {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let (map, order, bytes) = &mut *guard;
        if map.insert(key, block).is_some() {
            return;
        }
        order.push_back(key);
        *bytes += size;
        while *bytes > self.budget {
            let Some(old) = order.pop_front() else { break };
            if let Some(b) = map.remove(&old) {
                *bytes -= b.data.len() + 4 * b.offsets.len();
            }
        }
    }
}

pub struct BlockArchive {
    block_bytes: usize,
    cache: Arc<BlockCache>,
}

impl BlockArchive {
    pub fn new(block_bytes: usize, cache: Arc<BlockCache>) -> Self {
        Self { block_bytes, cache }
    }
}

impl Default for BlockArchive {
    fn default() -> Self {
        Self::new(64 * 1024, BlockCache::new(32 * 1024 * 1024))
    }
}

static RUN_IDS: AtomicU64 = AtomicU64::new(0);

struct BlockWriter {
    out: BufWriter<File>,
    offset: u64,
    index: Vec<u8>,
    first: u64,
}

impl BlockWriter {
    fn emit(&mut self, lengths: &mut Vec<u8>, data: &mut Vec<u8>, count: u64) -> Result<()> {
        let mut raw = Vec::with_capacity(10 + lengths.len() + data.len());
        put_varint(&mut raw, count);
        raw.extend_from_slice(lengths);
        raw.extend_from_slice(data);
        let packed = lz4_flex::block::compress(&raw);
        self.out.write_all(&packed)?;
        self.index.extend_from_slice(&self.offset.to_le_bytes());
        self.index.extend_from_slice(&u32::try_from(packed.len())?.to_le_bytes());
        self.index.extend_from_slice(&u32::try_from(raw.len())?.to_le_bytes());
        self.index.extend_from_slice(&self.first.to_le_bytes());
        self.index.extend_from_slice(&u32::try_from(count)?.to_le_bytes());
        self.index.extend_from_slice(blake3::hash(&raw).as_bytes());
        self.offset += packed.len() as u64;
        self.first += count;
        lengths.clear();
        data.clear();
        Ok(())
    }
}

impl Archive for BlockArchive {
    fn name(&self) -> &str {
        "lz4-block-64k-v1"
    }

    fn write(&self, path: &Path, originals: &mut dyn Iterator<Item = Vec<u8>>) -> Result<RunInfo> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut w = BlockWriter {
            out: BufWriter::new(options.open(path)?),
            offset: MAGIC.len() as u64,
            index: Vec::new(),
            first: 0,
        };
        w.out.write_all(&MAGIC)?;
        let (mut rows, mut raw_bytes, mut count) = (0u64, 0u64, 0u64);
        let mut lengths = Vec::new();
        let mut data = Vec::new();
        for original in originals {
            put_varint(&mut lengths, original.len() as u64);
            raw_bytes += original.len() as u64;
            data.extend_from_slice(&original);
            count += 1;
            rows += 1;
            if data.len() >= self.block_bytes {
                w.emit(&mut lengths, &mut data, count)?;
                count = 0;
            }
        }
        if count > 0 {
            w.emit(&mut lengths, &mut data, count)?;
        }
        let blocks = (w.index.len() / INDEX_ENTRY) as u64;
        let BlockWriter {
            mut out,
            offset,
            index,
            ..
        } = w;
        out.write_all(&index)?;
        out.write_all(&offset.to_le_bytes())?;
        out.write_all(&blocks.to_le_bytes())?;
        out.write_all(&rows.to_le_bytes())?;
        out.write_all(&crc32fast::hash(&index).to_le_bytes())?;
        out.write_all(&[0u8; 4])?;
        out.write_all(&END)?;
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(RunInfo {
            rows,
            raw_bytes,
            file_bytes: file.metadata()?.len(),
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn Run>> {
        let file = File::open(path)?;
        // SAFETY: archive files are immutable once written and synced; the
        // store's lock file excludes other writers. Truncation by another
        // process is outside the contract, as for every mapped database.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        if map.len() < MAGIC.len() + FOOTER || map[..8] != MAGIC || map[map.len() - 8..] != END {
            return Err("archive run magic mismatch".into());
        }
        let f = &map[map.len() - FOOTER..];
        let index_offset = u64::from_le_bytes(f[..8].try_into()?) as usize;
        let blocks = u64::from_le_bytes(f[8..16].try_into()?) as usize;
        let rows = u64::from_le_bytes(f[16..24].try_into()?);
        let crc = u32::from_le_bytes(f[24..28].try_into()?);
        let index_end = blocks
            .checked_mul(INDEX_ENTRY)
            .and_then(|n| n.checked_add(index_offset))
            .ok_or("archive index overflow")?;
        if index_end != map.len() - FOOTER || index_offset < MAGIC.len() {
            return Err("archive index bounds".into());
        }
        if crc32fast::hash(&map[index_offset..index_end]) != crc {
            return Err("archive index checksum mismatch".into());
        }
        let mut firsts = Vec::with_capacity(blocks);
        let mut expect = 0u64;
        let mut at = MAGIC.len() as u64;
        for b in 0..blocks {
            let e = &map[index_offset + b * INDEX_ENTRY..];
            let offset = u64::from_le_bytes(e[..8].try_into()?);
            let packed = u32::from_le_bytes(e[8..12].try_into()?) as u64;
            let first = u64::from_le_bytes(e[16..24].try_into()?);
            let count = u32::from_le_bytes(e[24..28].try_into()?) as u64;
            if offset != at || first != expect || count == 0 {
                return Err("archive block index is inconsistent".into());
            }
            at += packed;
            expect += count;
            firsts.push(first);
        }
        if expect != rows || at as usize != index_offset {
            return Err("archive row count mismatch".into());
        }
        Ok(Box::new(BlockRun {
            id: RUN_IDS.fetch_add(1, Ordering::Relaxed),
            map,
            index_offset,
            firsts,
            rows,
            cache: Arc::clone(&self.cache),
        }))
    }
}

struct BlockRun {
    id: u64,
    map: memmap2::Mmap,
    index_offset: usize,
    firsts: Vec<u64>,
    rows: u64,
    cache: Arc<BlockCache>,
}

impl BlockRun {
    fn block(&self, b: usize) -> Result<Arc<Decoded>> {
        if let Some(hit) = self.cache.get((self.id, b as u32)) {
            return Ok(hit);
        }
        let e = &self.map[self.index_offset + b * INDEX_ENTRY..][..INDEX_ENTRY];
        let offset = u64::from_le_bytes(e[..8].try_into()?) as usize;
        let packed = u32::from_le_bytes(e[8..12].try_into()?) as usize;
        let raw_len = u32::from_le_bytes(e[12..16].try_into()?) as usize;
        let count = u32::from_le_bytes(e[24..28].try_into()?) as u64;
        let raw = lz4_flex::block::decompress(&self.map[offset..offset + packed], raw_len)?;
        if blake3::hash(&raw).as_bytes() != &e[28..60] {
            return Err("archive block digest mismatch".into());
        }
        let mut pos = 0;
        if get_varint(&raw, &mut pos)? != count {
            return Err("archive block count mismatch".into());
        }
        let mut lengths = Vec::with_capacity(count as usize);
        for _ in 0..count {
            lengths.push(usize::try_from(get_varint(&raw, &mut pos)?)?);
        }
        let mut offsets = Vec::with_capacity(count as usize + 1);
        let mut at = pos;
        for n in lengths {
            offsets.push(u32::try_from(at)?);
            at = at.checked_add(n).ok_or("archive length overflow")?;
        }
        if at != raw.len() {
            return Err("archive block length mismatch".into());
        }
        offsets.push(u32::try_from(at)?);
        let decoded = Arc::new(Decoded { data: raw, offsets });
        self.cache.put((self.id, b as u32), Arc::clone(&decoded));
        Ok(decoded)
    }
    fn locate(&self, i: u64) -> usize {
        self.firsts.partition_point(|f| *f <= i) - 1
    }
}

impl Run for BlockRun {
    fn len(&self) -> u64 {
        self.rows
    }
    fn file_bytes(&self) -> u64 {
        self.map.len() as u64
    }
    fn get(&self, i: u64) -> Result<Vec<u8>> {
        if i >= self.rows {
            return Err("archive row out of range".into());
        }
        let b = self.locate(i);
        let block = self.block(b)?;
        let k = (i - self.firsts[b]) as usize;
        Ok(block.data[block.offsets[k] as usize..block.offsets[k + 1] as usize].to_vec())
    }
    fn scan(
        &self,
        start: u64,
        end: u64,
        visit: &mut dyn FnMut(u64, &[u8]) -> Result<()>,
    ) -> Result<()> {
        if start > end || end > self.rows {
            return Err("archive range out of bounds".into());
        }
        let mut i = start;
        while i < end {
            let b = self.locate(i);
            let block = self.block(b)?;
            let first = self.firsts[b];
            let stop = end.min(first + block.offsets.len() as u64 - 1);
            while i < stop {
                let k = (i - first) as usize;
                visit(i, &block.data[block.offsets[k] as usize..block.offsets[k + 1] as usize])?;
                i += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_roundtrip_rows_ranges_and_detect_damage() {
        let dir = std::env::temp_dir().join(format!(
            "nine-archive-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = BlockArchive::new(256, BlockCache::new(4096));
        let rows: Vec<Vec<u8>> = (0..500)
            .map(|i| format!("row {i} {}", "x".repeat(i % 97)).into_bytes())
            .chain([Vec::new(), "東京🦀".as_bytes().to_vec()])
            .collect();
        let path = dir.join("run");
        let info = archive.write(&path, &mut rows.clone().into_iter()).unwrap();
        assert_eq!(info.rows, rows.len() as u64);
        let run = archive.open(&path).unwrap();
        for (i, r) in rows.iter().enumerate().rev() {
            assert_eq!(&run.get(i as u64).unwrap(), r);
        }
        let mut seen = Vec::new();
        run.scan(37, 401, &mut |i, b| {
            seen.push((i, b.to_vec()));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 364);
        assert!(seen.iter().all(|(i, b)| &rows[*i as usize] == b));
        assert!(run.get(rows.len() as u64).is_err());
        // Flip one compressed byte: the per-block digest must reject it.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[20] ^= 0x40;
        let bad = dir.join("bad");
        std::fs::write(&bad, &bytes).unwrap();
        let fresh = BlockArchive::new(256, BlockCache::new(4096));
        let run = fresh.open(&bad).unwrap();
        assert!((0..rows.len() as u64).any(|i| run.get(i).is_err()));
        let empty = dir.join("empty");
        archive.write(&empty, &mut std::iter::empty()).unwrap();
        assert_eq!(archive.open(&empty).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
