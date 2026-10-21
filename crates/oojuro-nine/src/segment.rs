//! Index segments. Rows are global u32 ordinals in append order; a segment
//! covers one contiguous row range. The fresh segment is mutable and lives in
//! memory; immutable segments are written once, synced, and memory-mapped, so
//! the index can exceed RAM and reopening does not rebuild it.
//!
//! Immutable segment file (`seg-NNNNNNNN.idx`), little-endian:
//!   header (192 B): magic `NINEX004`, version, start row, row count, term
//!     count, section offsets, total token length, total scope, total
//!     original text bytes, and min/max valid_at and recorded_at
//!   rows   version 1 and 2: row_count × 40 B: valid_at i64, recorded_at i64,
//!          length u32, scope u32, text_len u32, flags u32 (bit0: stored
//!          only, not indexed), id_prefix u64 (big-endian first 8 id bytes,
//!          for tie-breaks without reading originals). Version 3: the same
//!          fields packed per block of 128 rows (`rows.rs`, about 10 B a row)
//!   ids    row_count × 20 B: (id fingerprint u128, local row u32), sorted
//!   keys   version 1 and 2: row_count × 20 B: (key fingerprint u128, local
//!          row u32), sorted. Version 3: only the rows whose key is not their
//!          id (almost none in an event store); every other row's key is its
//!          id, so the ids section answers for it
//!   time   version 1 and 2: row_count × 4 B: local rows sorted by (valid_at,
//!          row). Version 3: empty; the order is derived on first use
//!   multi  n × 16 B: sorted key fingerprints with more than one version among
//!          rows up to this segment's end
//!   postings  per term, a bit stream (LSB first, padded to a byte): per
//!          posting, the row gap minus one (the first: the row itself)
//!          Rice-coded with parameter k = floor(log2(rows / df)) (quotient
//!          in unary, k remainder bits), then tf in Elias gamma. Close to an
//!          Elias-Fano list in size (k is derived, nothing is stored per
//!          list); read sequentially. Version 1 segments (LEB128 (row gap,
//!          tf) pairs) stay readable.
//!   dict   blocks of up to 32 front-coded terms; each entry is prefix length,
//!          suffix length, suffix, df, cf, postings byte length (LEB128); a
//!          block starts with the postings offset of its first term
//!   dindex block_count × u64: byte offset of every dict block
//!   footer (16 B): CRC32 of everything before it, reserved u32, `NINEXEND`
//!
//! Sections are written in this order by a streaming writer, so flushes and
//! multi-gigabyte merges never hold a whole segment in memory.
use crate::codec::{get_varint, put_varint};
use crate::rows::{self, Rows};
use crate::text::{normalize_word, retrieval_terms};
use oojuro_core::{Record, Result};
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

pub(crate) type Row = u32;

const MAGIC: [u8; 8] = *b"NINEX004";
const END: [u8; 8] = *b"NINEXEND";
/// Written version; version 1 (LEB128 postings, 40 B rows) and 2 (Rice
/// postings, 40 B rows, full key and time tables) are still read.
const VERSION: u32 = 3;

/// Tests can write version 1 segments (LEB128 postings) on this thread, to
/// check that every read path still serves old stores.
#[cfg(test)]
thread_local! {
    pub(crate) static WRITE_V1: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn write_version() -> u32 {
    #[cfg(test)]
    if WRITE_V1.with(|v| v.get()) {
        return 1;
    }
    VERSION
}
const HEADER: usize = 192;
const FOOTER: usize = 16;
pub(crate) const ROW_BYTES: usize = 40;
const FP_BYTES: usize = 20;
const DICT_BLOCK: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowMeta {
    pub valid_at: i64,
    pub recorded_at: i64,
    /// Indexed token count (v3 `length`).
    pub length: u32,
    /// Distinct normalized terms (v3 `scope`).
    pub scope: u32,
    pub text_len: u32,
    pub id_prefix: u64,
    /// ROW_UNINDEXED: stored only; no postings, no background mass.
    pub flags: u32,
}

pub(crate) const ROW_UNINDEXED: u32 = 1;

pub(crate) fn id_prefix(id: &str) -> u64 {
    let mut b = [0u8; 8];
    let n = id.len().min(8);
    b[..n].copy_from_slice(&id.as_bytes()[..n]);
    u64::from_be_bytes(b)
}

/// Time bounds and mass totals of a row range, for whole-view statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub min_valid: i64,
    pub max_valid: i64,
    pub min_recorded: i64,
    pub max_recorded: i64,
    pub length: u64,
    pub scope: u64,
}

impl Default for Span {
    fn default() -> Self {
        Self {
            min_valid: i64::MAX,
            max_valid: i64::MIN,
            min_recorded: i64::MAX,
            max_recorded: i64::MIN,
            length: 0,
            scope: 0,
        }
    }
}

impl Span {
    pub fn add(&mut self, m: &RowMeta) {
        self.min_valid = self.min_valid.min(m.valid_at);
        self.max_valid = self.max_valid.max(m.valid_at);
        self.min_recorded = self.min_recorded.min(m.recorded_at);
        self.max_recorded = self.max_recorded.max(m.recorded_at);
        self.length += u64::from(m.length);
        self.scope += u64::from(m.scope);
    }
    pub fn of(meta: &[RowMeta]) -> Self {
        let mut s = Self::default();
        for m in meta {
            s.add(m);
        }
        s
    }
}

/// Normalized term counts of one text, computed outside any lock. Identical
/// to v3 `index`: raw tokens are counted first, then normalized and merged.
pub(crate) struct Analyzed {
    pub terms: Vec<(String, u32)>,
    pub length: u32,
    pub scope: u32,
}

impl Analyzed {
    /// No terms: the analysis of a stored-only record.
    pub fn none() -> Self {
        Self {
            terms: Vec::new(),
            length: 0,
            scope: 0,
        }
    }
}

pub(crate) fn analyze(text: &str) -> Result<Analyzed> {
    let mut raw = HashMap::<String, usize>::new();
    for token in retrieval_terms(text) {
        *raw.entry(token).or_insert(0) += 1;
    }
    let mut terms = HashMap::<String, usize>::new();
    for (term, count) in raw {
        let term = match normalize_word(&term) {
            Cow::Borrowed(_) => term,
            Cow::Owned(stem) => stem,
        };
        *terms.entry(term).or_insert(0) += count;
    }
    let length = u32::try_from(terms.values().sum::<usize>())?;
    let scope = u32::try_from(terms.len())?;
    let mut terms = terms
        .into_iter()
        .map(|(t, c)| Ok((t, u32::try_from(c)?)))
        .collect::<Result<Vec<_>>>()?;
    terms.sort_unstable();
    Ok(Analyzed {
        terms,
        length,
        scope,
    })
}

/// The mutable, in-memory segment that receives every acknowledged append.
pub(crate) struct Fresh {
    pub start: Row,
    pub meta: Vec<RowMeta>,
    pub records: Vec<Record>,
    pub id_fps: Vec<u128>,
    pub key_fps: Vec<u128>,
    pub postings: HashMap<Box<str>, Vec<(Row, u32)>>,
    pub ids: HashMap<u128, Vec<Row>>,
    pub keys: HashMap<u128, Vec<Row>>,
    pub time: BTreeSet<(i64, Row)>,
    pub text_bytes: usize,
    pub posting_count: usize,
    pub span: Span,
}

impl Fresh {
    pub fn new(start: Row) -> Self {
        Self {
            start,
            meta: Vec::new(),
            records: Vec::new(),
            id_fps: Vec::new(),
            key_fps: Vec::new(),
            postings: HashMap::new(),
            ids: HashMap::new(),
            keys: HashMap::new(),
            time: BTreeSet::new(),
            text_bytes: 0,
            posting_count: 0,
            span: Span::default(),
        }
    }

    pub fn len(&self) -> u32 {
        self.meta.len() as u32
    }

    pub fn end(&self) -> Row {
        self.start + self.len()
    }

    /// Append one validated record whose global row is `self.end()`.
    pub fn push(
        &mut self,
        r: Record,
        a: Analyzed,
        indexed: bool,
        id_fp: u128,
        key_fp: u128,
    ) -> Result<()> {
        let local = self.len();
        let a = if indexed { a } else { Analyzed::none() };
        let meta = RowMeta {
            flags: if indexed { 0 } else { ROW_UNINDEXED },
            valid_at: r.valid_at,
            recorded_at: r.recorded_at,
            length: a.length,
            scope: a.scope,
            text_len: u32::try_from(r.text.len())?,
            id_prefix: id_prefix(&r.id),
        };
        self.span.add(&meta);
        self.meta.push(meta);
        for (term, count) in a.terms {
            self.postings
                .entry(term.into_boxed_str())
                .or_default()
                .push((local, count));
        }
        self.posting_count += a.scope as usize;
        self.ids.entry(id_fp).or_default().push(local);
        self.keys.entry(key_fp).or_default().push(local);
        self.time.insert((r.valid_at, local));
        self.id_fps.push(id_fp);
        self.key_fps.push(key_fp);
        self.text_bytes += r.text.len();
        self.records.push(r);
        Ok(())
    }
}

/// An immutable, memory-mapped index segment plus its archive run.
pub(crate) struct DiskSegment {
    pub id: u64,
    pub start: Row,
    pub rows: u32,
    pub terms: u64,
    pub total_length: u64,
    pub total_scope: u64,
    pub total_text: u64,
    pub span: Span,
    map: memmap2::Mmap,
    sections: [usize; 9],
    version: u32,
    /// Version 3: where the packed rows are.
    packed: Option<Rows>,
    /// Version 3: rows whose key is not their id, sorted.
    exc: Vec<u32>,
    /// Version 3: local rows in (valid_at, row) order, built on first use.
    time: std::sync::OnceLock<Box<[u32]>>,
    pub archive: Box<dyn crate::archive::Run>,
    /// Ids of rows read for search results (rows are immutable, and an
    /// erasure writes a new segment): consecutive turns return many of the
    /// same rows, whose ids would otherwise each cost an archive read.
    ids: std::sync::Mutex<HashMap<u32, Box<str>>>,
    /// Skip tables of long postings lists, built on their second use
    /// (derived from the immutable postings; 32 bytes per 128 postings).
    skips: std::sync::Mutex<HashMap<usize, std::sync::Arc<[Skip]>>>,
    /// Lists asked for a skip table once: the second ask builds it, so a
    /// term searched only once never pays for a table.
    skip_wanted: std::sync::Mutex<std::collections::HashSet<usize>>,
}

/// Rows whose ids one segment remembers before it starts over.
const ID_CACHE_ROWS: usize = 1024;

/// Postings per skip block, and the shortest list given a skip table
/// (tiny under test, so small fixtures build skip tables).
#[cfg(not(test))]
pub(crate) const SKIP_BLOCK: u32 = 128;
#[cfg(not(test))]
pub(crate) const SKIP_MIN: u32 = 1024;
#[cfg(test)]
pub(crate) const SKIP_BLOCK: u32 = 4;
#[cfg(test)]
pub(crate) const SKIP_MIN: u32 = 8;

/// One block of a postings list: the row of its first posting and the
/// decode state at the block start.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Skip {
    pub first_row: u32,
    pub at: Cursor,
}

/// A decode position in one postings list of either version: byte (v1) or
/// bit (v2) position, the previous row, whether the next posting is the
/// first, and (v2) the postings left. Copy, so scans save and resume it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cursor {
    pub pos: usize,
    pub row: u32,
    pub first: bool,
    pub left: u32,
}

/// One term's postings in one disk segment, decodable from any cursor with
/// exactly the results (and end on malformed input) of [`Postings`].
#[derive(Clone, Copy)]
pub(crate) struct List<'a> {
    buf: &'a [u8],
    /// Version 2 Rice parameter; None for version 1 (LEB128).
    rice: Option<u32>,
    pub df: u32,
}

impl<'a> List<'a> {
    pub fn start(&self) -> Cursor {
        Cursor { pos: 0, row: 0, first: true, left: self.df }
    }
    /// A cursor past the last posting.
    pub fn end(&self) -> Cursor {
        let pos = if self.rice.is_some() { self.buf.len() * 8 } else { self.buf.len() };
        Cursor { pos, row: 0, first: false, left: 0 }
    }
    /// The LEB128 bytes of a version 1 list.
    pub fn leb(&self) -> Option<&'a [u8]> {
        self.rice.is_none().then_some(self.buf)
    }
    /// The posting at `c`, advancing `c`; None at the end (then `c` is
    /// unspecified: callers stop or use [`List::end`]).
    #[inline(always)]
    pub fn next(&self, c: &mut Cursor) -> Option<(u32, u32)> {
        match self.rice {
            Some(k) => {
                if c.left == 0 {
                    return None;
                }
                c.left -= 1;
                // Fast path: the whole posting (unary quotient, k remainder
                // bits, gamma tf) inside one 8-byte window of the list; the
                // same bits the reader below would consume.
                let at = c.pos / 8;
                if let Some(b) = self.buf.get(at..at + 8) {
                    let shift = (c.pos % 8) as u32;
                    let w = u64::from_le_bytes(b.try_into().expect("8 bytes")) >> shift;
                    let avail = 64 - shift;
                    let q = w.trailing_zeros();
                    let tail = q + 1 + k;
                    if tail < avail {
                        let rem = (w >> (q + 1)) & ((1u64 << k) - 1);
                        let w2 = w >> tail;
                        let z = w2.trailing_zeros();
                        if z <= 31 && tail + 2 * z < avail {
                            let gap = u32::try_from((u64::from(q) << k) | rem).ok()?;
                            let tf = (1u32 << z) | ((w2 >> (z + 1)) & ((1u64 << z) - 1)) as u32;
                            c.row = if c.first { gap } else { c.row.checked_add(gap)?.checked_add(1)? };
                            c.first = false;
                            c.pos += (tail + 2 * z + 1) as usize;
                            return Some((c.row, tf));
                        }
                    }
                }
                let mut bits = BitReader { buf: self.buf, pos: c.pos };
                let q = bits.unary()?;
                let gap = u32::try_from((q << k) | bits.bits(k)).ok()?;
                let z = bits.unary()?;
                if z > 31 {
                    return None;
                }
                let tf = (1u32 << z) | bits.bits(z as u32) as u32;
                c.row = if c.first { gap } else { c.row.checked_add(gap)?.checked_add(1)? };
                c.first = false;
                c.pos = bits.pos;
                Some((c.row, tf))
            }
            None => {
                let buf = self.buf;
                if c.pos >= buf.len() {
                    return None;
                }
                let gap = leb32(buf, &mut c.pos)?;
                let tf = leb32(buf, &mut c.pos)?;
                c.row = if c.first { gap } else { c.row.checked_add(gap)? };
                c.first = false;
                Some((c.row, tf))
            }
        }
    }
}

/// One LEB128 value of a version 1 list as u32 (None ends the list, as
/// [`Postings`] does).
#[inline(always)]
fn leb32(buf: &[u8], pos: &mut usize) -> Option<u32> {
    let b = *buf.get(*pos)?;
    if b < 0x80 {
        *pos += 1;
        return Some(u32::from(b));
    }
    u32::try_from(get_varint(buf, pos).ok()?).ok()
}

// Section indexes into `sections` (byte offsets; the last is the footer).
const S_ROWS: usize = 0;
const S_IDS: usize = 1;
const S_KEYS: usize = 2;
const S_TIME: usize = 3;
const S_MULTI: usize = 4;
const S_POST: usize = 5;
const S_DICT: usize = 6;
const S_DINDEX: usize = 7;
const S_END: usize = 8;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
fn i64_at(b: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
fn u128_at(b: &[u8], at: usize) -> u128 {
    u128::from_le_bytes(b[at..at + 16].try_into().expect("16 bytes"))
}

/// One term's dictionary entry inside a segment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TermInfo {
    pub df: u32,
    pub cf: u64,
    offset: usize,
    len: usize,
}

impl DiskSegment {
    pub fn open(id: u64, path: &Path, archive: Box<dyn crate::archive::Run>) -> Result<Self> {
        let file = File::open(path)?;
        // SAFETY: segment files are immutable after they are synced and
        // listed in the manifest; the store lock excludes other writers.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        if map.len() < HEADER + FOOTER || map[..8] != MAGIC || map[map.len() - 8..] != END {
            return Err("index segment magic mismatch".into());
        }
        let body = map.len() - FOOTER;
        if crc32fast::hash(&map[..body]) != u32_at(&map, body) {
            return Err("index segment checksum mismatch".into());
        }
        let version = u32_at(&map, 8);
        if !(1..=VERSION).contains(&version) {
            return Err("unsupported index segment version".into());
        }
        let start = u32_at(&map, 16);
        let rows = u32_at(&map, 20);
        let terms = u64_at(&map, 24);
        let mut sections = [0usize; 9];
        for (i, s) in sections.iter_mut().enumerate() {
            *s = usize::try_from(u64_at(&map, 32 + 8 * i))?;
        }
        let total_length = u64_at(&map, 104);
        let total_scope = u64_at(&map, 112);
        let total_text = u64_at(&map, 120);
        let span = Span {
            min_valid: i64_at(&map, 128),
            max_valid: i64_at(&map, 136),
            min_recorded: i64_at(&map, 144),
            max_recorded: i64_at(&map, 152),
            length: total_length,
            scope: total_scope,
        };
        let n = rows as usize;
        let layout_err = || -> Result<Self> { Err("index segment layout is inconsistent".into()) };
        if sections[S_ROWS] != HEADER
            || sections.windows(2).any(|w| w[0] > w[1])
            || sections[S_END] != body
            || (sections[S_POST] - sections[S_MULTI]) % 16 != 0
            || (sections[S_END] - sections[S_DINDEX]) % 8 != 0
            || archive.len() != u64::from(rows)
            || u64::from(start) + u64::from(rows) > u64::from(u32::MAX)
        {
            return layout_err();
        }
        let (mut packed, mut exc) = (None, Vec::new());
        if version >= 3 {
            let keys_len = sections[S_TIME] - sections[S_KEYS];
            if sections[S_KEYS] - sections[S_IDS] != n * FP_BYTES
                || keys_len % FP_BYTES != 0
                || keys_len / FP_BYTES > n
                || sections[S_MULTI] != sections[S_TIME]
            {
                return layout_err();
            }
            packed = Some(Rows::new(&map, sections[S_ROWS], sections[S_IDS], n)?);
            for i in 0..keys_len / FP_BYTES {
                let row = u32_at(&map, sections[S_KEYS] + i * FP_BYTES + 16);
                if row >= rows {
                    return layout_err();
                }
                exc.push(row);
            }
            exc.sort_unstable();
        } else {
            let expect = [
                (S_ROWS, S_IDS, n * ROW_BYTES),
                (S_IDS, S_KEYS, n * FP_BYTES),
                (S_KEYS, S_TIME, n * FP_BYTES),
                (S_TIME, S_MULTI, n * 4),
            ];
            if expect.iter().any(|(a, b, len)| sections[*b] - sections[*a] != *len) {
                return layout_err();
            }
        }
        Ok(Self {
            id,
            start,
            rows,
            terms,
            total_length,
            total_scope,
            total_text,
            span,
            map,
            sections,
            version,
            packed,
            exc,
            time: std::sync::OnceLock::new(),
            archive,
            ids: std::sync::Mutex::new(HashMap::new()),
            skips: std::sync::Mutex::new(HashMap::new()),
            skip_wanted: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// The id of local row `local` when it is its own 8-byte prefix: the
    /// prefix without zero padding, accepted only if the segment's id table
    /// maps that string's 128-bit identity fingerprint to this row (so a
    /// longer id, or one ending in NUL bytes, falls back to the original).
    pub fn short_id(&self, local: u32) -> Option<String> {
        let prefix = self.meta(local).id_prefix.to_be_bytes();
        let n = prefix.iter().rposition(|b| *b != 0)? + 1;
        let id = std::str::from_utf8(&prefix[..n]).ok()?;
        let fp = crate::codec::fingerprint(id);
        self.rows_with_id(fp).any(|r| r == local).then(|| id.to_string())
    }

    /// The id of local row `local`, from the cache or `read` (then cached).
    pub fn id_with(&self, local: u32, read: impl FnOnce() -> Result<String>) -> Result<String> {
        if let Some(id) = self.ids.lock().ok().and_then(|m| m.get(&local).map(|i| i.to_string())) {
            return Ok(id);
        }
        let id = read()?;
        if let Ok(mut m) = self.ids.lock() {
            if m.len() >= ID_CACHE_ROWS {
                m.clear();
            }
            m.insert(local, id.as_str().into());
        }
        Ok(id)
    }

    pub fn end(&self) -> Row {
        self.start + self.rows
    }

    pub fn file_bytes(&self) -> u64 {
        self.map.len() as u64
    }

    #[inline]
    pub fn meta(&self, local: u32) -> RowMeta {
        if let Some(p) = &self.packed {
            return p.meta(&self.map, local as usize);
        }
        let at = self.sections[S_ROWS] + local as usize * ROW_BYTES;
        let b = &self.map[..];
        RowMeta {
            valid_at: i64_at(b, at),
            recorded_at: i64_at(b, at + 8),
            length: u32_at(b, at + 16),
            scope: u32_at(b, at + 20),
            text_len: u32_at(b, at + 24),
            flags: u32_at(b, at + 28),
            id_prefix: u64::from_be_bytes(b[at + 32..at + 40].try_into().expect("8 bytes")),
        }
    }

    /// (scope, length) of one row, without decoding the rest of its metadata.
    #[inline]
    pub fn scope_length(&self, local: u32) -> (u32, u32) {
        if let Some(p) = &self.packed {
            return p.scope_length(&self.map, local as usize);
        }
        let at = self.sections[S_ROWS] + local as usize * ROW_BYTES;
        (u32_at(&self.map, at + 20), u32_at(&self.map, at + 16))
    }

    /// Reads (scope, length) of rows asked for in ascending order.
    pub fn mass_reader(&self) -> MassReader<'_> {
        match &self.packed {
            Some(p) => MassReader::Packed(p.mass_cursor(&self.map)),
            None => MassReader::Plain(self),
        }
    }

    /// valid_at of one row, without decoding the rest of its metadata.
    #[inline]
    pub fn valid_at(&self, local: u32) -> i64 {
        if let Some(p) = &self.packed {
            return p.valid_at(&self.map, local as usize);
        }
        i64_at(&self.map, self.sections[S_ROWS] + local as usize * ROW_BYTES)
    }

    /// Entries in the fingerprint table of `section` (ids, or keys).
    fn fp_count(&self, section: usize) -> usize {
        (self.sections[section + 1] - self.sections[section]) / FP_BYTES
    }

    fn fp_range(&self, section: usize, fp: u128) -> impl Iterator<Item = u32> + '_ {
        let base = self.sections[section];
        let n = self.fp_count(section);
        let key = move |i: usize| u128_at(&self.map, base + i * FP_BYTES);
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if key(mid) < fp {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo..n)
            .take_while(move |i| key(*i) == fp)
            .map(move |i| u32_at(&self.map, base + i * FP_BYTES + 16))
    }

    pub fn rows_with_id(&self, fp: u128) -> impl Iterator<Item = u32> + '_ {
        self.fp_range(S_IDS, fp)
    }

    /// Rows whose key fingerprint is `fp`, ascending. Version 3 stores only
    /// the rows whose key is not their id: every other row has the key of
    /// its id.
    pub fn rows_with_key(&self, fp: u128) -> std::vec::IntoIter<u32> {
        if self.version < 3 {
            return self.fp_range(S_KEYS, fp).collect::<Vec<_>>().into_iter();
        }
        let mut rows: Vec<u32> =
            self.fp_range(S_IDS, fp).filter(|r| self.exc.binary_search(r).is_err()).collect();
        rows.extend(self.fp_range(S_KEYS, fp));
        rows.sort_unstable();
        rows.into_iter()
    }

    pub fn multi_keys(&self) -> impl Iterator<Item = u128> + '_ {
        let (a, b) = (self.sections[S_MULTI], self.sections[S_POST]);
        (a..b).step_by(16).map(|at| u128_at(&self.map, at))
    }

    /// Local row at position `i` of the (valid_at, row) order.
    #[inline]
    pub fn time_at(&self, i: usize) -> u32 {
        if self.version < 3 {
            return u32_at(&self.map, self.sections[S_TIME] + 4 * i);
        }
        self.time_order()[i]
    }

    /// Version 3 keeps no time table: the order is sorted out of the rows
    /// the first time a range read or a merge asks for it.
    fn time_order(&self) -> &[u32] {
        self.time.get_or_init(|| {
            let mut v: Vec<(i64, u32)> = (0..self.rows).map(|l| (self.valid_at(l), l)).collect();
            v.sort_unstable();
            v.into_iter().map(|(_, l)| l).collect()
        })
    }

    /// First position in the time order whose valid_at is >= `at`.
    pub fn time_lower_bound(&self, at: i64) -> usize {
        let (mut lo, mut hi) = (0, self.rows as usize);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.valid_at(self.time_at(mid)) < at {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn block_count(&self) -> usize {
        (self.sections[S_END] - self.sections[S_DINDEX]) / 8
    }

    fn block_start(&self, b: usize) -> usize {
        self.sections[S_DICT] + u64_at(&self.map, self.sections[S_DINDEX] + 8 * b) as usize
    }

    /// Decode one dictionary block, calling `visit` for each term until it
    /// returns false.
    fn scan_block(&self, b: usize, mut visit: impl FnMut(&[u8], TermInfo) -> bool) -> Result<()> {
        let end = if b + 1 < self.block_count() {
            self.block_start(b + 1)
        } else {
            self.sections[S_DINDEX]
        };
        let buf = &self.map[..end];
        let mut pos = self.block_start(b);
        let mut offset = self.sections[S_POST] + usize::try_from(get_varint(buf, &mut pos)?)?;
        let mut term = Vec::new();
        while pos < end {
            let shared = usize::try_from(get_varint(buf, &mut pos)?)?;
            let n = usize::try_from(get_varint(buf, &mut pos)?)?;
            if shared > term.len() || pos + n > end {
                return Err("corrupt dictionary block".into());
            }
            term.truncate(shared);
            term.extend_from_slice(&buf[pos..pos + n]);
            pos += n;
            let df = u32::try_from(get_varint(buf, &mut pos)?)?;
            let cf = get_varint(buf, &mut pos)?;
            let len = usize::try_from(get_varint(buf, &mut pos)?)?;
            if offset + len > self.sections[S_DICT] {
                return Err("corrupt postings bounds".into());
            }
            let info = TermInfo {
                df,
                cf,
                offset,
                len,
            };
            offset += len;
            if !visit(&term, info) {
                break;
            }
        }
        Ok(())
    }

    fn first_term(&self, b: usize) -> &[u8] {
        let buf = &self.map[..self.sections[S_DINDEX]];
        let mut pos = self.block_start(b);
        let ok = get_varint(buf, &mut pos).and_then(|_| get_varint(buf, &mut pos));
        match (ok, get_varint(buf, &mut pos)) {
            (Ok(_), Ok(n)) => buf.get(pos..pos + n as usize).unwrap_or(&[]),
            _ => &[],
        }
    }

    pub fn term(&self, term: &str) -> Option<TermInfo> {
        let key = term.as_bytes();
        // Last block whose first term is <= key.
        let (mut lo, mut hi) = (0, self.block_count());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.first_term(mid) <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return None;
        }
        let mut found = None;
        self.scan_block(lo - 1, |t, info| {
            if t == key {
                found = Some(info);
            }
            t < key
        })
        .ok()?;
        found
    }

    /// Look up strictly increasing `terms` in one merge over the dictionary:
    /// each block holding a term is decoded once, and the block search
    /// gallops forward from the previous term's block. Equal to `term` for
    /// every entry.
    pub fn terms_sorted(&self, terms: &[&str]) -> Vec<Option<TermInfo>> {
        let mut out = vec![None; terms.len()];
        let blocks = self.block_count();
        let mut i = 0;
        let mut from = 0;
        while i < terms.len() {
            let key = terms[i].as_bytes();
            // Last block whose first term is <= key, at or after `from`.
            let mut step = 1;
            let mut hi = from;
            while hi < blocks && self.first_term(hi) <= key {
                from = hi;
                hi = hi.saturating_add(step);
                step *= 2;
            }
            let (mut lo, mut hi) = (from, hi.min(blocks));
            while lo < hi {
                let mid = (lo + hi) / 2;
                if self.first_term(mid) <= key {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if lo == 0 || self.first_term(lo - 1) > key {
                i += 1;
                continue;
            }
            let block = lo - 1;
            from = block;
            // Every remaining term below the next block's first term is in
            // this block (if anywhere).
            let end = match (block + 1 < blocks).then(|| self.first_term(block + 1)) {
                Some(next) => i + terms[i..].partition_point(|t| t.as_bytes() < next),
                None => terms.len(),
            };
            let mut j = i;
            let scanned = self.scan_block(block, |t, info| {
                while j < end && terms[j].as_bytes() < t {
                    j += 1;
                }
                if j < end && terms[j].as_bytes() == t {
                    out[j] = Some(info);
                    j += 1;
                }
                j < end
            });
            if scanned.is_err() {
                // A corrupt block: terms not yet found in it stay absent.
            }
            i = end.max(i + 1);
        }
        out
    }

    /// The skip table of a term's postings if already built.
    pub fn cached_skips(&self, info: TermInfo) -> Option<std::sync::Arc<[Skip]>> {
        self.skips.lock().ok()?.get(&info.offset).cloned()
    }

    /// The skip table of a term's postings: cached, or built by one decode
    /// on the second ask (None on the first).
    pub fn skips(&self, info: TermInfo) -> Option<std::sync::Arc<[Skip]>> {
        if let Some(k) = self.cached_skips(info) {
            return Some(k);
        }
        let first_ask = self.skip_wanted.lock().map(|mut w| w.insert(info.offset)).unwrap_or(false);
        if first_ask && !cfg!(test) {
            return None;
        }
        Some(self.skips_now(info))
    }

    /// The skip table of a term's postings, built now if missing.
    pub fn skips_now(&self, info: TermInfo) -> std::sync::Arc<[Skip]> {
        if let Some(k) = self.cached_skips(info) {
            return k;
        }
        let list = self.list(info);
        let mut out = Vec::with_capacity((info.df / SKIP_BLOCK + 1) as usize);
        let mut c = list.start();
        let mut n = 0u32;
        loop {
            let at = c;
            let Some((row, _)) = list.next(&mut c) else { break };
            if n % SKIP_BLOCK == 0 {
                out.push(Skip { first_row: row, at });
            }
            n += 1;
        }
        let k: std::sync::Arc<[Skip]> = out.into();
        if let Ok(mut m) = self.skips.lock() {
            m.insert(info.offset, k.clone());
        }
        k
    }

    /// The encoded postings of one term (LEB128 (row gap, tf) pairs).
    pub fn postings_bytes(&self, info: TermInfo) -> &[u8] {
        &self.map[info.offset..info.offset + info.len]
    }

    /// The segment file version (1: LEB128 postings, 2: Rice).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// One term's postings as a cursor-decodable list.
    pub fn list(&self, info: TermInfo) -> List<'_> {
        List {
            buf: self.postings_bytes(info),
            rice: (self.version != 1).then(|| rice_k(self.rows, info.df)),
            df: info.df,
        }
    }

    pub fn postings(&self, info: TermInfo) -> Postings<'_> {
        let buf = &self.map[info.offset..info.offset + info.len];
        if self.version == 1 {
            return Postings::Disk {
                buf,
                pos: 0,
                row: 0,
                first: true,
            };
        }
        Postings::Rice {
            bits: BitReader { buf, pos: 0 },
            left: info.df,
            k: rice_k(self.rows, info.df),
            row: 0,
            first: true,
        }
    }

    /// (fingerprint, local row) pairs of the id or key table, in order. The
    /// key table of a version 3 segment is derived: the ids of the rows whose
    /// key is their id, merged with the stored exceptions.
    pub fn fp_entries(&self, keys: bool) -> Box<dyn Iterator<Item = (u128, u32)> + '_> {
        let table = move |section: usize| {
            let base = self.sections[section];
            (0..self.fp_count(section)).map(move |i| {
                let at = base + i * FP_BYTES;
                (u128_at(&self.map, at), u32_at(&self.map, at + 16))
            })
        };
        if !keys {
            return Box::new(table(S_IDS));
        }
        if self.version < 3 {
            return Box::new(table(S_KEYS));
        }
        let plain = table(S_IDS).filter(|(_, r)| self.exc.binary_search(r).is_err());
        Box::new(Merge2 { a: plain.peekable(), b: table(S_KEYS).peekable() })
    }

    /// Dictionary cursor in term byte order (for merges).
    pub fn terms_cursor(&self) -> TermCursor<'_> {
        TermCursor {
            segment: self,
            block: 0,
            pos: 0,
            offset: 0,
            term: Vec::new(),
        }
    }

    /// Every term in byte order with its info (for merges and statistics).
    pub fn for_each_term(&self, mut visit: impl FnMut(&[u8], TermInfo)) -> Result<()> {
        for b in 0..self.block_count() {
            self.scan_block(b, |t, info| {
                visit(t, info);
                true
            })?;
        }
        Ok(())
    }
}

/// (scope, length) of the rows of one segment, read in ascending order
/// (a version 3 segment parses each block's header once).
pub(crate) enum MassReader<'a> {
    Packed(rows::MassCursor<'a>),
    Plain(&'a DiskSegment),
}

impl MassReader<'_> {
    #[inline]
    pub fn get(&mut self, local: u32) -> (u32, u32) {
        match self {
            MassReader::Packed(c) => c.get(local as usize),
            MassReader::Plain(s) => s.scope_length(local),
        }
    }
}

/// Two ascending streams merged into one ascending stream.
struct Merge2<A: Iterator<Item = (u128, u32)>, B: Iterator<Item = (u128, u32)>> {
    a: std::iter::Peekable<A>,
    b: std::iter::Peekable<B>,
}

impl<A: Iterator<Item = (u128, u32)>, B: Iterator<Item = (u128, u32)>> Iterator for Merge2<A, B> {
    type Item = (u128, u32);
    fn next(&mut self) -> Option<Self::Item> {
        match (self.a.peek(), self.b.peek()) {
            (Some(x), Some(y)) => {
                if x <= y {
                    self.a.next()
                } else {
                    self.b.next()
                }
            }
            (Some(_), None) => self.a.next(),
            (None, _) => self.b.next(),
        }
    }
}

/// Streams a segment dictionary term by term.
pub(crate) struct TermCursor<'a> {
    segment: &'a DiskSegment,
    block: usize,
    pos: usize,
    offset: usize,
    term: Vec<u8>,
}

impl TermCursor<'_> {
    /// Advance to the next term; returns it with its info, or None at end.
    pub fn next_term(&mut self) -> Result<Option<(&[u8], TermInfo)>> {
        let s = self.segment;
        let blocks = s.block_count();
        loop {
            if self.block >= blocks {
                return Ok(None);
            }
            let end = if self.block + 1 < blocks {
                s.block_start(self.block + 1)
            } else {
                s.sections[S_DINDEX]
            };
            let buf = &s.map[..end];
            if self.pos == 0 {
                self.pos = s.block_start(self.block);
                self.offset =
                    s.sections[S_POST] + usize::try_from(get_varint(buf, &mut self.pos)?)?;
                self.term.clear();
            }
            if self.pos >= end {
                self.block += 1;
                self.pos = 0;
                continue;
            }
            let shared = usize::try_from(get_varint(buf, &mut self.pos)?)?;
            let n = usize::try_from(get_varint(buf, &mut self.pos)?)?;
            if shared > self.term.len() || self.pos + n > end {
                return Err("corrupt dictionary block".into());
            }
            self.term.resize(shared, 0);
            self.term.extend_from_slice(&buf[self.pos..self.pos + n]);
            self.pos += n;
            let df = u32::try_from(get_varint(buf, &mut self.pos)?)?;
            let cf = get_varint(buf, &mut self.pos)?;
            let len = usize::try_from(get_varint(buf, &mut self.pos)?)?;
            if self.offset + len > s.sections[S_DICT] {
                return Err("corrupt postings bounds".into());
            }
            let info = TermInfo {
                df,
                cf,
                offset: self.offset,
                len,
            };
            self.offset += len;
            return Ok(Some((&self.term, info)));
        }
    }
}

/// Rice parameter of a list of `df` rows among `rows`: floor(log2(rows / df)).
pub(crate) fn rice_k(rows: u32, df: u32) -> u32 {
    let q = rows / df.max(1);
    if q <= 1 { 0 } else { 31 - q.leading_zeros() }
}

/// One version-2 postings list of (sorted local row, tf >= 1) pairs in a
/// segment of `rows` rows (`out` is reused as the buffer).
pub(crate) fn encode_rice(rows: u32, pairs: &[(u32, u32)], mut out: Vec<u8>) -> Vec<u8> {
    let k = rice_k(rows, pairs.len() as u32);
    out.clear();
    let mut w = BitWriter {
        out,
        ..BitWriter::default()
    };
    let mut prev: Option<u32> = None;
    for &(row, tf) in pairs {
        let gap = u64::from(match prev {
            None => row,
            Some(p) => row - p - 1,
        });
        w.unary(gap >> k);
        w.bits(gap, k);
        w.gamma(tf);
        prev = Some(row);
    }
    w.finish()
}

/// LSB-first bit writer.
#[derive(Default)]
pub(crate) struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    /// Append the low `n` (<= 32) bits of `v`.
    #[inline]
    pub fn bits(&mut self, v: u64, n: u32) {
        debug_assert!(n <= 32);
        self.acc |= (v & ((1u64 << n) - 1)) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// `q` zeros, then a one.
    #[inline]
    pub fn unary(&mut self, mut q: u64) {
        while q >= 32 {
            self.bits(0, 32);
            q -= 32;
        }
        self.bits(1 << q, q as u32 + 1);
    }
    /// Elias gamma of `v` >= 1: floor(log2 v) zeros, a one, the bits below the top.
    #[inline]
    pub fn gamma(&mut self, v: u32) {
        let k = 31 - v.leading_zeros();
        self.unary(u64::from(k));
        self.bits(u64::from(v), k);
    }
    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// LSB-first bit reader over a byte slice (reads past the end see zeros).
pub(crate) struct BitReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    #[inline]
    fn window(&self) -> u64 {
        let at = self.pos / 8;
        let w = match self.buf.get(at..at + 8) {
            Some(b) => u64::from_le_bytes(b.try_into().expect("8 bytes")),
            None => {
                let mut b = [0u8; 8];
                let tail = self.buf.get(at..).unwrap_or(&[]);
                b[..tail.len()].copy_from_slice(tail);
                u64::from_le_bytes(b)
            }
        };
        w >> (self.pos % 8)
    }
    #[inline]
    fn bits(&mut self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        let v = self.window() & ((1u64 << n) - 1);
        self.pos += n as usize;
        v
    }
    /// Count zeros up to the next one and skip past it; None past the end.
    #[inline]
    fn unary(&mut self) -> Option<u64> {
        let mut q = 0u64;
        loop {
            if self.pos >= self.buf.len() * 8 {
                return None;
            }
            let w = self.window();
            let avail = 64 - (self.pos % 8) as u32;
            let z = w.trailing_zeros();
            if z < avail {
                self.pos += z as usize + 1;
                return Some(q + u64::from(z));
            }
            q += u64::from(avail);
            self.pos += avail as usize;
        }
    }
}

/// Postings of one term in one segment: (local row, term frequency).
pub(crate) enum Postings<'a> {
    Fresh(std::slice::Iter<'a, (Row, u32)>),
    /// Version 1 segments: LEB128 (row gap, tf) pairs.
    Disk {
        buf: &'a [u8],
        pos: usize,
        row: u32,
        first: bool,
    },
    Rice {
        bits: BitReader<'a>,
        left: u32,
        k: u32,
        row: u32,
        first: bool,
    },
}

impl Iterator for Postings<'_> {
    type Item = (u32, u32);
    #[inline]
    fn next(&mut self) -> Option<(u32, u32)> {
        match self {
            Postings::Fresh(it) => it.next().copied(),
            Postings::Disk {
                buf,
                pos,
                row,
                first,
            } => {
                if *pos >= buf.len() {
                    return None;
                }
                // CRC-checked at open; a malformed stream ends the list.
                let gap = u32::try_from(get_varint(buf, pos).ok()?).ok()?;
                let tf = u32::try_from(get_varint(buf, pos).ok()?).ok()?;
                *row = if *first { gap } else { row.checked_add(gap)? };
                *first = false;
                Some((*row, tf))
            }
            Postings::Rice {
                bits,
                left,
                k,
                row,
                first,
            } => {
                if *left == 0 {
                    return None;
                }
                *left -= 1;
                // CRC-checked at open; a malformed stream ends the list.
                let q = bits.unary()?;
                let gap = u32::try_from((q << *k) | bits.bits(*k)).ok()?;
                let z = bits.unary()?;
                if z > 31 {
                    return None;
                }
                let tf = (1u32 << z) | bits.bits(z as u32) as u32;
                *row = if *first { gap } else { row.checked_add(gap)?.checked_add(1)? };
                *first = false;
                Some((*row, tf))
            }
        }
    }
}

/// Streaming writer for one immutable segment, rows `start..start + n`.
/// Call the section methods in order: `rows`, `fingerprints`, `time`,
/// `multi`, then `term` for every term in strictly increasing byte order,
/// then `finish`. The header is patched last; the CRC covers everything.
pub(crate) struct SegmentWriter {
    out: BufWriter<File>,
    body_crc: crc32fast::Hasher,
    at: u64,
    sections: [u64; 9],
    next_section: usize,
    start: Row,
    rows: u32,
    span: Span,
    total_text: u64,
    terms: u64,
    dict: Vec<u8>,
    dindex: Vec<u8>,
    previous: Vec<u8>,
    in_block: usize,
    postings_bytes: u64,
    list: Vec<u8>,
    pairs: Vec<(u32, u32)>,
}

impl SegmentWriter {
    pub fn create(path: &Path, start: Row) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = BufWriter::with_capacity(1 << 20, options.open(path)?);
        out.write_all(&[0u8; HEADER])?;
        Ok(Self {
            out,
            body_crc: crc32fast::Hasher::new(),
            at: HEADER as u64,
            sections: [0; 9],
            next_section: 0,
            start,
            rows: 0,
            span: Span::default(),
            total_text: 0,
            terms: 0,
            dict: Vec::new(),
            dindex: Vec::new(),
            previous: Vec::new(),
            in_block: DICT_BLOCK,
            postings_bytes: 0,
            list: Vec::new(),
            pairs: Vec::new(),
        })
    }

    fn put(&mut self, b: &[u8]) -> Result<()> {
        self.body_crc.update(b);
        self.out.write_all(b)?;
        self.at += b.len() as u64;
        Ok(())
    }

    fn section(&mut self, index: usize) -> Result<()> {
        if self.next_section != index {
            return Err("segment sections written out of order".into());
        }
        self.sections[index] = self.at;
        self.next_section += 1;
        Ok(())
    }

    pub fn rows(&mut self, meta: impl IntoIterator<Item = RowMeta>) -> Result<()> {
        self.section(S_ROWS)?;
        if write_version() >= 3 {
            let mut enc = rows::Encoder::default();
            for m in meta {
                self.count_row(&m)?;
                if let Some(block) = enc.push(m) {
                    self.put(block)?;
                }
            }
            if let Some(block) = enc.last() {
                self.put(block)?;
            }
            self.put(&enc.table())?;
        } else {
            let mut b = [0u8; ROW_BYTES];
            for m in meta {
                b[..8].copy_from_slice(&m.valid_at.to_le_bytes());
                b[8..16].copy_from_slice(&m.recorded_at.to_le_bytes());
                b[16..20].copy_from_slice(&m.length.to_le_bytes());
                b[20..24].copy_from_slice(&m.scope.to_le_bytes());
                b[24..28].copy_from_slice(&m.text_len.to_le_bytes());
                b[28..32].copy_from_slice(&m.flags.to_le_bytes());
                b[32..40].copy_from_slice(&m.id_prefix.to_be_bytes());
                self.put(&b)?;
                self.count_row(&m)?;
            }
        }
        if u64::from(self.start) + u64::from(self.rows) > u64::from(u32::MAX) {
            return Err("segment rows exceed the row space".into());
        }
        Ok(())
    }

    fn count_row(&mut self, m: &RowMeta) -> Result<()> {
        self.span.add(m);
        self.total_text += u64::from(m.text_len);
        self.rows = self.rows.checked_add(1).ok_or("segment row count overflow")?;
        Ok(())
    }

    fn fp_section(
        &mut self,
        index: usize,
        pairs: impl IntoIterator<Item = (u128, u32)>,
    ) -> Result<()> {
        self.section(index)?;
        let mut last = None;
        let mut n = 0u32;
        for (fp, local) in pairs {
            if local >= self.rows || last.is_some_and(|l| l > (fp, local)) {
                return Err("fingerprint table must be sorted and in range".into());
            }
            last = Some((fp, local));
            self.put(&fp.to_le_bytes())?;
            self.put(&local.to_le_bytes())?;
            n += 1;
        }
        if n != self.rows {
            return Err("fingerprint table must cover every row".into());
        }
        Ok(())
    }

    /// The id table and the key table: (fingerprint, local row) pairs, each
    /// sorted ascending and covering every row. A version 3 segment keeps the
    /// whole id table and, of the key table, only the entries no id entry
    /// repeats (a row whose key is its id has the same entry in both).
    pub fn fingerprints(
        &mut self,
        ids: impl IntoIterator<Item = (u128, u32)>,
        keys: impl IntoIterator<Item = (u128, u32)>,
    ) -> Result<()> {
        if write_version() < 3 {
            self.fp_section(S_IDS, ids)?;
            return self.fp_section(S_KEYS, keys);
        }
        self.section(S_IDS)?;
        let mut keys = keys.into_iter().peekable();
        let mut exceptions: Vec<(u128, u32)> = Vec::new();
        let (mut last_id, mut last_key) = (None, None);
        let (mut n_ids, mut matched) = (0u32, 0u32);
        for (fp, local) in ids {
            if local >= self.rows || last_id.is_some_and(|l| l > (fp, local)) {
                return Err("fingerprint table must be sorted and in range".into());
            }
            last_id = Some((fp, local));
            // key entries ahead of this id entry belong to rows with another id
            while let Some(&k) = keys.peek() {
                if k >= (fp, local) {
                    break;
                }
                if k.1 >= self.rows || last_key.is_some_and(|l| l > k) {
                    return Err("fingerprint table must be sorted and in range".into());
                }
                last_key = Some(k);
                exceptions.push(k);
                keys.next();
            }
            if keys.peek() == Some(&(fp, local)) {
                last_key = Some((fp, local));
                matched += 1;
                keys.next();
            }
            self.put(&fp.to_le_bytes())?;
            self.put(&local.to_le_bytes())?;
            n_ids += 1;
        }
        for k in keys {
            if k.1 >= self.rows || last_key.is_some_and(|l| l > k) {
                return Err("fingerprint table must be sorted and in range".into());
            }
            last_key = Some(k);
            exceptions.push(k);
        }
        if n_ids != self.rows || matched as usize + exceptions.len() != self.rows as usize {
            return Err("fingerprint table must cover every row".into());
        }
        self.section(S_KEYS)?;
        for (fp, local) in exceptions {
            self.put(&fp.to_le_bytes())?;
            self.put(&local.to_le_bytes())?;
        }
        Ok(())
    }

    /// Local rows in (valid_at, row) order. Version 3 keeps no table (the
    /// reader derives the order), so `locals` is not even asked for.
    pub fn time<I: IntoIterator<Item = u32>>(&mut self, locals: impl FnOnce() -> I) -> Result<()> {
        self.section(S_TIME)?;
        if write_version() >= 3 {
            return Ok(());
        }
        let mut n = 0u32;
        for local in locals() {
            if local >= self.rows {
                return Err("time order row out of range".into());
            }
            self.put(&local.to_le_bytes())?;
            n += 1;
        }
        if n != self.rows {
            return Err("time order must cover every row".into());
        }
        Ok(())
    }

    /// Sorted, distinct key fingerprints with several versions.
    pub fn multi(&mut self, keys: impl IntoIterator<Item = u128>) -> Result<()> {
        self.section(S_MULTI)?;
        let mut last = None;
        for k in keys {
            if last.is_some_and(|l| l >= k) {
                return Err("multi-version keys must be sorted and distinct".into());
            }
            last = Some(k);
            self.put(&k.to_le_bytes())?;
        }
        self.section(S_POST)
    }

    /// One term with its (local row, tf) postings in increasing row order.
    pub fn term(
        &mut self,
        term: &[u8],
        postings: impl IntoIterator<Item = (u32, u32)>,
    ) -> Result<()> {
        if self.next_section != S_DICT {
            return Err("terms must follow the multi section".into());
        }
        if self.terms > 0 && term <= self.previous.as_slice() {
            return Err("segment terms must be strictly increasing".into());
        }
        let mut pairs = std::mem::take(&mut self.pairs);
        pairs.clear();
        let (mut last, mut cf) = (None, 0u64);
        for (row, tf) in postings {
            if row >= self.rows || last.is_some_and(|l| l >= row) || tf == 0 {
                return Err("segment postings must be sorted, in range and nonzero".into());
            }
            pairs.push((row, tf));
            cf += u64::from(tf);
            last = Some(row);
        }
        let df = u32::try_from(pairs.len())?;
        if df == 0 {
            return Err("segment terms must have postings".into());
        }
        let list = if write_version() == 1 {
            let mut list = std::mem::take(&mut self.list);
            list.clear();
            let mut last = 0;
            for &(row, tf) in &pairs {
                put_varint(&mut list, u64::from(row - last));
                put_varint(&mut list, u64::from(tf));
                last = row;
            }
            list
        } else {
            encode_rice(self.rows, &pairs, std::mem::take(&mut self.list))
        };
        self.pairs = pairs;
        if self.in_block == DICT_BLOCK {
            self.dindex
                .extend_from_slice(&(self.dict.len() as u64).to_le_bytes());
            put_varint(&mut self.dict, self.postings_bytes);
            self.previous.clear();
            self.in_block = 0;
        }
        let shared = self
            .previous
            .iter()
            .zip(term.iter())
            .take_while(|(a, b)| a == b)
            .count();
        put_varint(&mut self.dict, shared as u64);
        put_varint(&mut self.dict, (term.len() - shared) as u64);
        self.dict.extend_from_slice(&term[shared..]);
        put_varint(&mut self.dict, u64::from(df));
        put_varint(&mut self.dict, cf);
        put_varint(&mut self.dict, list.len() as u64);
        self.put(&list)?;
        self.postings_bytes += list.len() as u64;
        self.previous.clear();
        self.previous.extend_from_slice(term);
        self.in_block += 1;
        self.terms += 1;
        self.list = list;
        Ok(())
    }

    /// Write the dictionary, header and footer, then sync. Returns bytes.
    pub fn finish(mut self) -> Result<u64> {
        if self.next_section != S_DICT {
            return Err("segment is missing sections".into());
        }
        self.section(S_DICT)?;
        let dict = std::mem::take(&mut self.dict);
        self.put(&dict)?;
        self.section(S_DINDEX)?;
        let dindex = std::mem::take(&mut self.dindex);
        self.put(&dindex)?;
        self.sections[S_END] = self.at;
        let mut header = [0u8; HEADER];
        header[..8].copy_from_slice(&MAGIC);
        header[8..12].copy_from_slice(&write_version().to_le_bytes());
        header[16..20].copy_from_slice(&self.start.to_le_bytes());
        header[20..24].copy_from_slice(&self.rows.to_le_bytes());
        header[24..32].copy_from_slice(&self.terms.to_le_bytes());
        for (i, s) in self.sections.iter().enumerate() {
            header[32 + 8 * i..40 + 8 * i].copy_from_slice(&s.to_le_bytes());
        }
        header[104..112].copy_from_slice(&self.span.length.to_le_bytes());
        header[112..120].copy_from_slice(&self.span.scope.to_le_bytes());
        header[120..128].copy_from_slice(&self.total_text.to_le_bytes());
        let span = self.span;
        for (i, v) in [
            span.min_valid,
            span.max_valid,
            span.min_recorded,
            span.max_recorded,
        ]
        .into_iter()
        .enumerate()
        {
            header[128 + 8 * i..136 + 8 * i].copy_from_slice(&v.to_le_bytes());
        }
        let mut crc = crc32fast::Hasher::new();
        crc.update(&header);
        crc.combine(&self.body_crc);
        let mut footer = [0u8; FOOTER];
        footer[..4].copy_from_slice(&crc.finalize().to_le_bytes());
        footer[8..].copy_from_slice(&END);
        self.out.write_all(&footer)?;
        let mut file = self.out.into_inner().map_err(|e| e.into_error())?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.sync_all()?;
        Ok(self.at + FOOTER as u64)
    }
}

#[cfg(test)]
mod postings_tests {
    use super::*;

    fn rice(rows: u32, pairs: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let buf = encode_rice(rows, pairs, Vec::new());
        Postings::Rice {
            bits: BitReader { buf: &buf, pos: 0 },
            left: pairs.len() as u32,
            k: rice_k(rows, pairs.len() as u32),
            row: 0,
            first: true,
        }
        .collect()
    }

    #[test]
    fn rice_postings_roundtrip_edges() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for &(rows, density) in &[(1u32, 1u32), (7, 1), (100, 3), (65_536, 1000), (1 << 22, 3), (u32::MAX - 1, 1 << 20)] {
            for _ in 0..20 {
                let mut v: Vec<(u32, u32)> = Vec::new();
                let mut r = (next() % u64::from(density.max(1))) as u32;
                while r < rows && v.len() < 5000 {
                    let tf = match next() % 10 {
                        0 => u32::MAX,
                        1..=3 => 1 + (next() % 1000) as u32,
                        _ => 1,
                    };
                    v.push((r, tf));
                    let step = 1 + (next() % u64::from(density.max(1) * 2)) as u32;
                    r = match r.checked_add(step) {
                        Some(n) => n,
                        None => break,
                    };
                }
                if v.is_empty() {
                    continue;
                }
                assert_eq!(rice(rows, &v), v, "rows {rows} density {density}");
            }
        }
        // one posting at the last row of the largest segment, huge tf
        let v = vec![(u32::MAX - 1, u32::MAX)];
        assert_eq!(rice(u32::MAX, &v), v);
        // a malformed (truncated) list ends early instead of panicking
        let buf = encode_rice(1000, &[(5, 1), (700, 3), (999, 2)], Vec::new());
        let short: Vec<(u32, u32)> = Postings::Rice {
            bits: BitReader { buf: &buf[..1], pos: 0 },
            left: 3,
            k: rice_k(1000, 3),
            row: 0,
            first: true,
        }
        .collect();
        assert!(short.len() < 3);
    }

    #[test]
    fn cursor_lists_decode_exactly_like_postings_including_truncated_ends() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for &(rows, density) in &[(64u32, 1u32), (1000, 3), (65_536, 40), (1 << 20, 900), (1 << 22, 5)] {
            for round in 0..30 {
                let mut v: Vec<(u32, u32)> = Vec::new();
                let mut r = (next() % u64::from(density)) as u32;
                while r < rows && v.len() < 3000 {
                    let tf = match next() % 12 {
                        0 => 1 + (next() % 100_000) as u32,
                        1..=3 => 1 + (next() % 7) as u32,
                        _ => 1,
                    };
                    v.push((r, tf));
                    r = r.saturating_add(1 + (next() % u64::from(density * 2)) as u32);
                }
                if v.is_empty() {
                    continue;
                }
                let buf = encode_rice(rows, &v, Vec::new());
                for cut in [buf.len(), buf.len().saturating_sub(1), buf.len() / 2, 9, 3] {
                    let part = &buf[..cut.min(buf.len())];
                    let want: Vec<(u32, u32)> = Postings::Rice {
                        bits: BitReader { buf: part, pos: 0 },
                        left: v.len() as u32,
                        k: rice_k(rows, v.len() as u32),
                        row: 0,
                        first: true,
                    }
                    .collect();
                    let list = List { buf: part, rice: Some(rice_k(rows, v.len() as u32)), df: v.len() as u32 };
                    let mut c = list.start();
                    let mut got = Vec::new();
                    while let Some(p) = list.next(&mut c) {
                        got.push(p);
                    }
                    assert_eq!(got, want, "rows {rows} round {round} cut {cut}");
                }
            }
        }
    }

    #[test]
    fn version_one_postings_still_decode() {
        let mut buf = Vec::new();
        for (gap, tf) in [(4u64, 1u64), (3, 2), (300, 1)] {
            put_varint(&mut buf, gap);
            put_varint(&mut buf, tf);
        }
        let got: Vec<(u32, u32)> = Postings::Disk {
            buf: &buf,
            pos: 0,
            row: 0,
            first: true,
        }
        .collect();
        assert_eq!(got, vec![(4, 1), (7, 2), (307, 1)]);
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;
    use crate::archive::Run;

    struct Dummy(u64);
    impl Run for Dummy {
        fn len(&self) -> u64 {
            self.0
        }
        fn file_bytes(&self) -> u64 {
            0
        }
        fn get(&self, _: u64) -> Result<Vec<u8>> {
            Err("no originals".into())
        }
        fn scan(&self, _: u64, _: u64, _: &mut dyn FnMut(u64, &[u8]) -> Result<()>) -> Result<()> {
            Err("no originals".into())
        }
    }

    fn mix(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn fp(x: u64) -> u128 {
        (u128::from(mix(x)) << 64) | u128::from(mix(!x))
    }

    struct Fixture {
        meta: Vec<RowMeta>,
        ids: Vec<u128>,
        keys: Vec<u128>,
    }

    /// `n` rows with id fingerprints of their own; keys equal ids except for
    /// a share of rows that share a key group (versions), take another row's
    /// id as their key, or sit alone under a key of their own.
    fn fixture(n: usize, seed: u64, exceptions: u64) -> Fixture {
        let meta: Vec<RowMeta> = (0..n)
            .map(|i| {
                let r = mix(seed ^ i as u64);
                RowMeta {
                    valid_at: if r.is_multiple_of(5) { 7 } else { 1_790_000_000_000_000 + ((r >> 8) % 100_000_000) as i64 },
                    recorded_at: if r.is_multiple_of(3) { 7 } else { 1_790_000_000_000_000 + ((r >> 12) % 1_000_000) as i64 },
                    length: (r >> 20) as u32 % 5000,
                    scope: (r >> 40) as u32 % 700,
                    text_len: (r >> 7) as u32 % 100_000,
                    flags: (r & 1) as u32,
                    id_prefix: id_prefix(&(100_000 + i).to_string()),
                }
            })
            .collect();
        let ids: Vec<u128> = (0..n).map(|i| fp(seed ^ (i as u64) << 20)).collect();
        let keys: Vec<u128> = (0..n)
            .map(|i| match mix(seed ^ 0x55 ^ i as u64) % 100 {
                x if x < exceptions => fp(seed ^ 0xAAAA ^ (i as u64 % 9)), // a group of versions
                x if x < exceptions * 2 => ids[(mix(i as u64 ^ seed) as usize) % n], // another row's id
                x if x < exceptions * 3 => fp(seed ^ 0xBBBB ^ i as u64), // a key of its own
                _ => ids[i],
            })
            .collect();
        Fixture { meta, ids, keys }
    }

    fn write(path: &Path, f: &Fixture, v1: bool) -> DiskSegment {
        WRITE_V1.with(|v| v.set(v1));
        let n = f.meta.len();
        let sorted = |fps: &[u128]| {
            let mut p: Vec<(u128, u32)> = fps.iter().copied().zip(0u32..).collect();
            p.sort_unstable();
            p
        };
        let mut w = SegmentWriter::create(path, 1000).unwrap();
        w.rows(f.meta.iter().copied()).unwrap();
        w.fingerprints(sorted(&f.ids), sorted(&f.keys)).unwrap();
        let mut order: Vec<(i64, u32)> = f.meta.iter().map(|m| m.valid_at).zip(0u32..).collect();
        order.sort_unstable();
        w.time(|| order.iter().map(|x| x.1)).unwrap();
        w.multi(Vec::new()).unwrap();
        w.finish().unwrap();
        WRITE_V1.with(|v| v.set(false));
        DiskSegment::open(1, path, Box::new(Dummy(n as u64))).unwrap()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
        let d = base.join(format!("nine-tables-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The packed rows, the key exceptions and the derived time order answer
    /// exactly like the version 2 tables for the same data.
    #[test]
    fn version_three_tables_answer_like_version_two() {
        let dir = scratch("v3v2");
        for (n, exceptions, seed) in [(0usize, 0u64, 1u64), (1, 0, 2), (300, 0, 3), (300, 7, 4), (1500, 10, 5), (1500, 33, 6), (129, 33, 7)] {
            let f = fixture(n, seed, exceptions);
            let old = write(&dir.join(format!("old-{n}-{seed}.idx")), &f, true);
            let new = write(&dir.join(format!("new-{n}-{seed}.idx")), &f, false);
            assert_eq!((old.version(), new.version()), (1, 3));
            for l in 0..n as u32 {
                assert_eq!(new.meta(l), old.meta(l), "row {l}");
                assert_eq!(new.meta(l), f.meta[l as usize], "row {l}");
                assert_eq!(new.scope_length(l), old.scope_length(l));
                assert_eq!(new.valid_at(l), f.meta[l as usize].valid_at);
            }
            assert_eq!(new.fp_entries(false).collect::<Vec<_>>(), old.fp_entries(false).collect::<Vec<_>>());
            assert_eq!(new.fp_entries(true).collect::<Vec<_>>(), old.fp_entries(true).collect::<Vec<_>>(), "key table, n {n} seed {seed}");
            assert_eq!((0..n).map(|i| new.time_at(i)).collect::<Vec<_>>(), (0..n).map(|i| old.time_at(i)).collect::<Vec<_>>());
            for at in [i64::MIN, 0, 7, 8, 1_790_000_000_000_000, 1_790_000_050_000_000, i64::MAX] {
                assert_eq!(new.time_lower_bound(at), old.time_lower_bound(at));
            }
            // lookups: every id and key present, and some absent
            let mut probes: Vec<u128> = f.ids.iter().chain(&f.keys).copied().collect();
            probes.extend([0, 1, u128::MAX, fp(99), fp(98)]);
            for p in probes {
                assert_eq!(new.rows_with_id(p).collect::<Vec<_>>(), old.rows_with_id(p).collect::<Vec<_>>());
                assert_eq!(new.rows_with_key(p).collect::<Vec<_>>(), old.rows_with_key(p).collect::<Vec<_>>(), "key {p:x}");
            }
            // what the tables hold: ids in full, a fraction of the keys
            if exceptions == 0 && n > 0 {
                assert_eq!(new.exc.len(), 0);
            }
            if n >= 300 {
                assert!(new.file_bytes() < old.file_bytes(), "{} vs {} bytes", new.file_bytes(), old.file_bytes());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_tables_must_cover_every_row() {
        let dir = scratch("cover");
        let f = fixture(10, 9, 0);
        let sorted = |fps: &[u128]| {
            let mut p: Vec<(u128, u32)> = fps.iter().copied().zip(0u32..).collect();
            p.sort_unstable();
            p
        };
        let try_write = |ids: Vec<(u128, u32)>, keys: Vec<(u128, u32)>, name: &str| {
            let mut w = SegmentWriter::create(&dir.join(name), 0).unwrap();
            w.rows(f.meta.iter().copied()).unwrap();
            w.fingerprints(ids, keys)
        };
        assert!(try_write(sorted(&f.ids), sorted(&f.keys), "ok").is_ok());
        let mut short = sorted(&f.keys);
        short.pop();
        assert!(try_write(sorted(&f.ids), short, "short").is_err());
        let mut twice = sorted(&f.keys);
        twice.push(twice[0]);
        twice.sort_unstable();
        assert!(try_write(sorted(&f.ids), twice, "twice").is_err());
        let mut unsorted = sorted(&f.ids);
        unsorted.swap(0, 1);
        assert!(try_write(unsorted, sorted(&f.keys), "unsorted").is_err());
        let mut out_of_range = sorted(&f.keys);
        out_of_range[3].1 = 99;
        assert!(try_write(sorted(&f.ids), out_of_range, "range").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
