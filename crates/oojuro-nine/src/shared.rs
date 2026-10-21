//! The shared write log: one group-commit journal under many v4 stores.
//!
//! Each store keeps its own index, segments, archive runs and manifest (it
//! stays the erasure domain); only durability is shared. A commit of any
//! store is a batch of frames tagged with the store id and the batch's first
//! row. Batches of different stores that arrive while a sync is in flight are
//! written together and made durable by one `sync_data` (leader/follower
//! group commit; after each round the leader hands leadership to a waiting
//! writer, so no writer leads for longer than one round).
//!
//! Directory: `LOCK`, `LOGID` (the log identity, also recorded in every file
//! header and in the manifest of every bound store), `log-G.nine` files (the
//! highest generation is the active one; every open starts a new one),
//! `spare-*.nine` (a zero-filled file prepared in the background, renamed
//! into the next generation) and `log-G.dead` (retired by an erasure scrub;
//! zeroed and removed, also by the next open after a crash).
//!
//! Preallocation: log files are written over space that already holds
//! zeros (the spare), so a round's `sync_data` flushes data only. Appending
//! to a file would also commit its new size through the filesystem journal
//! (on ext4 a second device flush per sync); with many agents that cost is
//! what one shared sync must beat (see `docs/SHARED_LOG.md`).
//!
//! File header (48 bytes): magic `NINESL01`, salt u64, generation u64, log id
//! (16 bytes), 4 reserved bytes, CRC32 of the preceding 44 bytes.
//!
//! Frame header (48 bytes): payload_len u32, flags u8 (MORE), 3 reserved
//! bytes, seq u64 (per file), round u64 (seq of the first frame of its
//! round), store u64, first_row u64 (row of the frame's first entry),
//! payload CRC32, header CRC32 of (salt, first 44 bytes). The payload is
//! LEB128-length-prefixed entries (encoded records), one per row.
//!
//! A batch is one store's frames, all but the last flagged MORE, written
//! contiguously; recovery delivers it only once its final frame is valid, so
//! batches are all-or-nothing. A round holds whole batches of any stores and
//! is synced before the next round is written. Recovery of the newest file
//! (the journal rule of `wal.rs`, refined by rounds): at the first damaged
//! frame the rest of the file is searched for later valid frames. One that
//! belongs to a later round proves the damaged round was synced: the damage
//! is in committed history and opening fails without modifying anything.
//! Otherwise the damage is a torn, unsynced round (its pages may persist out
//! of order inside preallocated space): the complete batches before the
//! damage are kept, the rest is truncated. Older files were complete and
//! synced when they were sealed: after their last batch only zeros may
//! follow, and any damage fails the open.
//!
//! Truncation: a file other than the active one is removed once every store
//! with frames in it has durably absorbed them (its manifest covers those
//! rows); stores report this after each flush.
//!
//! Erasure: [`SharedLog::scrub`] requires every frame to be absorbed, starts
//! a new file, renames every older file to `.dead`, overwrites it with zeros,
//! syncs and removes it.
use crate::codec::{get_varint, put_varint};
use crate::wal::{count_journal_sync, random_salt, sync_dir};
use oojuro_core::Result;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

const MAGIC: [u8; 8] = *b"NINESL01";
const ID_MAGIC: [u8; 8] = *b"NINESLID";
const ID_FILE: &str = "LOGID";
pub(crate) const FILE_HEADER: usize = 48;
pub(crate) const FRAME_HEADER: usize = 48;
const MORE: u8 = 1;
/// Bound on any frame payload this format accepts.
const FRAME_BOUND: usize = 64 * 1024 * 1024;
const POISONED: &str = "shared log I/O previously failed; close and reopen the database";

#[derive(Clone, Debug)]
pub struct SharedLogOptions {
    /// Start a new log file once the active one holds this many bytes; with
    /// `preallocate`, also the size of each zero-filled file.
    pub segment_bytes: u64,
    /// Bound on one frame's payload; larger batches span several frames.
    pub max_frame: usize,
    /// Write rounds into zero-filled space prepared in the background, so a
    /// sync flushes data only (no file size change to journal).
    pub preallocate: bool,
}

impl Default for SharedLogOptions {
    fn default() -> Self {
        Self {
            segment_bytes: 32 * 1024 * 1024,
            max_frame: FRAME_BOUND,
            preallocate: true,
        }
    }
}

/// Binds a store to a shared log under a stable store id. The id is recorded
/// in the store's manifest; reopening the store needs the same log and id.
#[derive(Clone)]
pub struct LogBinding {
    pub log: Arc<SharedLog>,
    pub store: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SharedLogStats {
    /// Group-commit rounds (one write and one `sync_data` each).
    pub rounds: u64,
    /// Store batches made durable.
    pub batches: u64,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BatchLoc {
    pub(crate) generation: u64,
    pub(crate) offset: u64,
    pub(crate) len: u64,
    pub(crate) first_row: u64,
    pub(crate) rows: u64,
}

impl BatchLoc {
    fn end(&self) -> u64 {
        self.first_row + self.rows
    }
}

struct LogFile {
    path: PathBuf,
    file: File,
    salt: u64,
    /// Logical end: the end of the last complete batch.
    bytes: u64,
    /// Bytes of zero-filled space (0 for a file that grows by appends).
    capacity: u64,
    seq: u64,
    /// Per store: the end row of its last batch in this file.
    ends: HashMap<u64, u64>,
}

#[derive(Default)]
struct StoreLog {
    /// Batches not yet absorbed, in log order (which is row order).
    batches: VecDeque<BatchLoc>,
    /// Rows the store has made durable in its own segments.
    absorbed: Option<u64>,
    attached: bool,
    /// The row its next batch must start at (known while attached).
    next: Option<u64>,
}

struct Inner {
    files: BTreeMap<u64, LogFile>,
    stores: HashMap<u64, StoreLog>,
    next_generation: u64,
    poisoned: bool,
    #[cfg(test)]
    fail_after_bytes: Option<usize>,
}

impl Inner {
    fn absorbed(&self, store: u64, end: u64) -> bool {
        self.stores
            .get(&store)
            .and_then(|s| s.absorbed)
            .is_some_and(|a| a >= end)
    }

    fn active(&self) -> u64 {
        *self.files.keys().next_back().expect("an active log file")
    }
}

/// The zero-filled spare, shared with the thread that prepares it (which
/// never holds the log itself, so dropping the log releases its lock).
#[derive(Default)]
struct Spare {
    /// A zero-filled, synced file ready to become the next generation.
    ready: Option<(PathBuf, File)>,
    /// A background thread is preparing one.
    preparing: bool,
    /// The log is gone: a finished spare is removed instead of kept.
    closed: bool,
}

enum SlotState {
    Waiting,
    Lead,
    Done(Result<()>),
}

struct Slot {
    state: Mutex<SlotState>,
    cv: Condvar,
}

struct Ticket {
    store: u64,
    first_row: u64,
    rows: u64,
    frames: Vec<u8>,
    /// Offsets of the frame headers inside `frames`.
    starts: Vec<usize>,
    slot: Arc<Slot>,
}

#[derive(Default)]
struct Queue {
    pending: Vec<Ticket>,
    leader: bool,
}

/// A process-wide write log shared by many stores (see the module docs).
pub struct SharedLog {
    dir: PathBuf,
    id: [u8; 16],
    options: SharedLogOptions,
    _lock: File,
    queue: Mutex<Queue>,
    inner: Mutex<Inner>,
    spare: Arc<Mutex<Spare>>,
    rounds: AtomicU64,
    batches: AtomicU64,
}

impl Drop for SharedLog {
    fn drop(&mut self) {
        let mut spare = relock(&self.spare);
        spare.closed = true;
        if let Some((path, file)) = spare.ready.take() {
            drop(file);
            let _ = fs::remove_file(path);
        }
    }
}

fn relock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn file_name(generation: u64) -> String {
    format!("log-{generation:08}.nine")
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().expect("8 bytes"))
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().expect("4 bytes"))
}

fn header_crc(salt: u64, h: &[u8]) -> u32 {
    let mut c = crc32fast::Hasher::new();
    c.update(&salt.to_le_bytes());
    c.update(&h[..44]);
    c.finalize()
}

fn file_header(salt: u64, generation: u64, id: &[u8; 16]) -> [u8; FILE_HEADER] {
    let mut h = [0u8; FILE_HEADER];
    h[..8].copy_from_slice(&MAGIC);
    h[8..16].copy_from_slice(&salt.to_le_bytes());
    h[16..24].copy_from_slice(&generation.to_le_bytes());
    h[24..40].copy_from_slice(id);
    let crc = crc32fast::hash(&h[..44]);
    h[44..48].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Validate a file header; returns the file's salt.
fn check_header(bytes: &[u8], generation: u64, id: &[u8; 16]) -> Result<u64> {
    let h = &bytes[..FILE_HEADER];
    if h[..8] != MAGIC {
        return Err("shared log magic/version mismatch".into());
    }
    if crc32fast::hash(&h[..44]) != le32(&h[44..]) {
        return Err("shared log file header checksum mismatch".into());
    }
    if le64(&h[16..]) != generation || h[24..40] != id[..] || h[40..44] != [0; 4] {
        return Err("shared log file belongs to another log or generation".into());
    }
    Ok(le64(&h[8..]))
}

fn random_id(path: &Path) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(path.as_os_str().as_encoded_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    h.update(&now.as_nanos().to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(&random_salt(path).to_le_bytes());
    h.finalize().as_bytes()[..16].try_into().expect("16 bytes")
}

fn create_private(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// The directory holding `path`, for syncing a new entry in it.
fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Position-independent I/O: the active file is read (replays) and written
/// (rounds) at explicit offsets, never through a shared cursor.
fn read_at(file: &File, offset: u64, len: u64) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; usize::try_from(len)?];
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(&mut buf, offset)?;
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err("short read from the shared log".into());
            }
            done += n;
        }
    }
    Ok(buf)
}

fn write_all_at(file: &File, offset: u64, buf: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, offset)?;
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = file.seek_write(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err("short write to the shared log".into());
            }
            done += n;
        }
    }
    Ok(())
}

/// Overwrite a file's bytes with zeros and make that durable.
fn zero_file(file: &File) -> Result<()> {
    zero_fill(file, file.metadata()?.len())?;
    file.sync_data()?;
    Ok(())
}

/// Write zeros over `0..len` (real blocks: no holes, no unwritten extents).
fn zero_fill(file: &File, len: u64) -> Result<()> {
    let zeros = vec![0u8; 1 << 20];
    let mut at = 0u64;
    while at < len {
        let n = (len - at).min(zeros.len() as u64) as usize;
        write_all_at(file, at, &zeros[..n])?;
        at += n as u64;
    }
    Ok(())
}

/// Zero then remove `path`; a file that is already gone is fine.
fn zero_and_remove(path: &Path) -> Result<()> {
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    zero_file(&file)?;
    drop(file);
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn read_or_create_id(dir: &Path, may_create: bool) -> Result<[u8; 16]> {
    let path = dir.join(ID_FILE);
    match fs::read(&path) {
        Ok(b) => {
            if b.len() != 28 || b[..8] != ID_MAGIC || crc32fast::hash(&b[..24]) != le32(&b[24..]) {
                return Err("shared log identity file is damaged".into());
            }
            Ok(b[8..24].try_into().expect("16 bytes"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && may_create => {
            let id = random_id(&path);
            let mut body = Vec::with_capacity(28);
            body.extend_from_slice(&ID_MAGIC);
            body.extend_from_slice(&id);
            body.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
            let tmp = dir.join("LOGID.tmp");
            let _ = fs::remove_file(&tmp);
            let mut f = create_private(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &path)?;
            sync_dir(dir)?;
            Ok(id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("shared log files exist without the log identity file".into())
        }
        Err(e) => Err(e.into()),
    }
}

struct Frame {
    len: usize,
    flags: u8,
    seq: u64,
    round: u64,
    store: u64,
    first_row: u64,
}

/// A frame is valid only if both CRCs hold and it fits in the buffer.
/// In-order replay (`exact`) also needs the next sequence number; the
/// corruption scan accepts any later one.
fn parse(bytes: &[u8], pos: usize, salt: u64, seq: u64, exact: bool) -> Option<Frame> {
    let h = bytes.get(pos..pos.checked_add(FRAME_HEADER)?)?;
    let len = le32(h) as usize;
    if len > FRAME_BOUND || h[4] & !MORE != 0 || h[5..8] != [0, 0, 0] {
        return None;
    }
    let found = le64(&h[8..]);
    if found < seq || (exact && found != seq) {
        return None;
    }
    if header_crc(salt, h) != le32(&h[44..]) {
        return None;
    }
    let body = bytes.get(pos + FRAME_HEADER..pos + FRAME_HEADER + len)?;
    if crc32fast::hash(body) != le32(&h[40..]) {
        return None;
    }
    let round = le64(&h[16..]);
    if round > found {
        return None;
    }
    Some(Frame {
        len,
        flags: h[4],
        seq: found,
        round,
        store: le64(&h[24..]),
        first_row: le64(&h[32..]),
    })
}

/// Visit the LEB128-prefixed entries of a payload; returns their count.
fn split_entries<'a>(payload: &'a [u8], mut visit: impl FnMut(&'a [u8])) -> Result<u64> {
    let mut at = 0;
    let mut n = 0u64;
    while at < payload.len() {
        let len = usize::try_from(get_varint(payload, &mut at)?)?;
        let end = at
            .checked_add(len)
            .filter(|e| *e <= payload.len())
            .ok_or("shared log entry exceeds its frame")?;
        visit(&payload[at..end]);
        at = end;
        n += 1;
    }
    Ok(n)
}

/// Encode one batch as frames whose seq, round and header CRC are filled in
/// by the round that writes them. Returns the bytes and each frame's offset.
fn encode_batch(
    store: u64,
    first_row: u64,
    entries: &[Vec<u8>],
    max_frame: usize,
) -> Result<(Vec<u8>, Vec<usize>)> {
    fn close(out: &mut [u8], start: usize, store: u64, first_row: u64, more: bool) {
        let len = (out.len() - start - FRAME_HEADER) as u32;
        let crc = crc32fast::hash(&out[start + FRAME_HEADER..]);
        let h = &mut out[start..start + FRAME_HEADER];
        h[..4].copy_from_slice(&len.to_le_bytes());
        h[4] = if more { MORE } else { 0 };
        h[24..32].copy_from_slice(&store.to_le_bytes());
        h[32..40].copy_from_slice(&first_row.to_le_bytes());
        h[40..44].copy_from_slice(&crc.to_le_bytes());
    }
    let size: usize = entries.iter().map(|e| e.len() + 5).sum();
    let mut out = Vec::with_capacity(size + FRAME_HEADER);
    let mut starts = vec![0];
    out.resize(FRAME_HEADER, 0);
    let (mut start, mut frame_row, mut row) = (0usize, first_row, first_row);
    let mut prefix = Vec::with_capacity(10);
    for e in entries {
        prefix.clear();
        put_varint(&mut prefix, e.len() as u64);
        if prefix.len() + e.len() > max_frame {
            return Err("shared log entry exceeds the frame bound".into());
        }
        if out.len() - start - FRAME_HEADER + prefix.len() + e.len() > max_frame {
            close(&mut out, start, store, frame_row, true);
            start = out.len();
            starts.push(start);
            out.resize(start + FRAME_HEADER, 0);
            frame_row = row;
        }
        out.extend_from_slice(&prefix);
        out.extend_from_slice(e);
        row += 1;
    }
    close(&mut out, start, store, frame_row, false);
    Ok((out, starts))
}

/// Fill in the sequence number, round and header CRC of a frame about to be
/// written.
fn seal(h: &mut [u8], seq: u64, round: u64, salt: u64) {
    h[8..16].copy_from_slice(&seq.to_le_bytes());
    h[16..24].copy_from_slice(&round.to_le_bytes());
    let crc = header_crc(salt, h);
    h[44..48].copy_from_slice(&crc.to_le_bytes());
}

pub(crate) struct Scanned {
    pub(crate) batches: Vec<(u64, BatchLoc)>,
    /// End of the last complete batch.
    pub(crate) end: usize,
    seq: u64,
    ends: HashMap<u64, u64>,
}

/// Parse a file's frames into complete batches (see the recovery rule).
pub(crate) fn scan_file(bytes: &[u8], generation: u64, salt: u64, sealed: bool) -> Result<Scanned> {
    let mut pos = FILE_HEADER;
    let mut seq = 0u64;
    let (mut batch_start, mut batch_seq) = (pos, seq);
    let mut open: Option<(u64, u64, u64)> = None;
    let mut round: Option<u64> = None;
    let mut batches = Vec::new();
    let mut ends: HashMap<u64, u64> = HashMap::new();
    let mut damaged = None;
    while pos < bytes.len() {
        let Some(f) = parse(bytes, pos, salt, seq, true) else {
            damaged = Some(pos);
            break;
        };
        // Valid CRCs with inconsistent structure are not a torn write.
        if round != Some(f.round) {
            if f.round != f.seq || open.is_some() {
                return Err("shared log rounds are inconsistent".into());
            }
            round = Some(f.round);
        }
        let body = &bytes[pos + FRAME_HEADER..pos + FRAME_HEADER + f.len];
        let n = split_entries(body, |_| {})?;
        match &mut open {
            Some((store, first, rows)) => {
                if f.store != *store || Some(f.first_row) != first.checked_add(*rows) {
                    return Err("shared log batch continues with another store or row".into());
                }
                *rows += n;
            }
            None => open = Some((f.store, f.first_row, n)),
        }
        pos += FRAME_HEADER + f.len;
        seq = f.seq + 1;
        if f.flags & MORE == 0 {
            let (store, first_row, rows) = open.take().expect("an open batch");
            let end = first_row.checked_add(rows).ok_or("shared log row overflow")?;
            let e = ends.entry(store).or_insert(0);
            *e = (*e).max(end);
            batches.push((
                store,
                BatchLoc {
                    generation,
                    offset: batch_start as u64,
                    len: (pos - batch_start) as u64,
                    first_row,
                    rows,
                },
            ));
            batch_start = pos;
            batch_seq = seq;
        }
    }
    if let (Some(at), false) = (damaged, sealed) {
        // Frames of the damaged round itself (the round in progress, or one
        // starting at the damage) may survive a torn write in any order; a
        // frame of any later round proves the damaged round was synced. No
        // frame starts in the zero-filled space after the last non-zero byte.
        let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |p| p + 1);
        let later_round = (at + 1..end).any(|p| {
            parse(bytes, p, salt, seq, false)
                .is_some_and(|f| Some(f.round) != round && f.round != seq)
        });
        if later_round {
            return Err(
                "shared log frame is corrupt before committed frames; refusing to truncate".into(),
            );
        }
    }
    // A sealed file was synced whole: after its last batch only the zeros
    // of never-used preallocated space may follow.
    if sealed && bytes[batch_start..].iter().any(|b| *b != 0) {
        return Err("sealed shared log file is damaged or incomplete".into());
    }
    Ok(Scanned {
        batches,
        end: batch_start,
        seq: batch_seq,
        ends,
    })
}

/// Decode one batch read back from a file, re-checking every frame.
fn decode_batch(
    bytes: &[u8],
    salt: u64,
    store: u64,
    first_row: u64,
    out: &mut Vec<Vec<u8>>,
) -> Result<()> {
    let mut pos = 0;
    let mut row = first_row;
    let mut last = false;
    while pos < bytes.len() {
        let f = parse(bytes, pos, salt, 0, false)
            .ok_or("shared log batch failed its checksum on replay")?;
        if last || f.store != store || f.first_row != row {
            return Err("shared log batch does not match its location".into());
        }
        let body = &bytes[pos + FRAME_HEADER..pos + FRAME_HEADER + f.len];
        row += split_entries(body, |e| out.push(e.to_vec()))?;
        last = f.flags & MORE == 0;
        pos += FRAME_HEADER + f.len;
    }
    if !last {
        return Err("shared log batch is incomplete on replay".into());
    }
    Ok(())
}

impl LogFile {
    /// Create a new, empty log file and make its header durable.
    fn create(dir: &Path, generation: u64, id: &[u8; 16]) -> Result<Self> {
        let path = dir.join(file_name(generation));
        let mut file = create_private(&path)?;
        let salt = random_salt(&path);
        file.write_all(&file_header(salt, generation, id))?;
        file.sync_all()?;
        sync_dir(dir)?;
        Ok(Self {
            path,
            file,
            salt,
            bytes: FILE_HEADER as u64,
            capacity: 0,
            seq: 0,
            ends: HashMap::new(),
        })
    }

    /// Turn a zero-filled spare into generation `generation`: write its
    /// header over the zeros (a data-only sync), then rename it in.
    fn from_spare(dir: &Path, spare: (PathBuf, File), generation: u64, id: &[u8; 16]) -> Result<Self> {
        let (from, file) = spare;
        let path = dir.join(file_name(generation));
        let salt = random_salt(&path);
        write_all_at(&file, 0, &file_header(salt, generation, id))?;
        file.sync_data()?;
        let capacity = file.metadata()?.len();
        fs::rename(&from, &path)?;
        sync_dir(dir)?;
        Ok(Self {
            path,
            file,
            salt,
            bytes: FILE_HEADER as u64,
            capacity,
            seq: 0,
            ends: HashMap::new(),
        })
    }
}

/// A queued batch. Waiting may lead a round; a dropped handle still waits,
/// so a batch chosen to lead can never stall the queue.
pub(crate) struct Pending<'a> {
    log: &'a SharedLog,
    slot: Arc<Slot>,
    lead: bool,
    done: bool,
}

impl Pending<'_> {
    /// Block until the batch is durable (or failed).
    pub(crate) fn wait(mut self) -> Result<()> {
        self.done = true;
        self.finish()
    }

    fn finish(&mut self) -> Result<()> {
        if std::mem::take(&mut self.lead) {
            self.log.lead_round();
        }
        loop {
            let mut state = relock(&self.slot.state);
            while matches!(*state, SlotState::Waiting) {
                state = self
                    .slot
                    .cv
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            match std::mem::replace(&mut *state, SlotState::Waiting) {
                SlotState::Done(result) => return result,
                SlotState::Lead => {
                    drop(state);
                    self.log.lead_round();
                }
                SlotState::Waiting => unreachable!("waited for a change"),
            }
        }
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.finish();
        }
    }
}

/// Attachment of one open store; detaches when the store closes.
pub(crate) struct Attached {
    log: Arc<SharedLog>,
    store: u64,
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.log.detach(self.store);
    }
}

/// Attach a store whose segments durably hold rows `..durable`; returns
/// the guard, the first row to replay (`durable`) and the entries of every
/// later row in the log, in row order. `created`: the store was just
/// initialized, so the log must hold nothing for its id.
pub(crate) fn attach(
    binding: &LogBinding,
    durable: u64,
    created: bool,
) -> Result<(Attached, u64, Vec<Vec<u8>>)> {
    let (first, entries) = binding.log.attach(binding.store, durable, created)?;
    let guard = Attached {
        log: Arc::clone(&binding.log),
        store: binding.store,
    };
    Ok((guard, first, entries))
}

impl SharedLog {
    /// Open (or create) the shared log in `dir`, recovering its files. Stores
    /// replay their rows from it when they open (see [`SharedLog::stores`]).
    pub fn open(dir: &Path, options: SharedLogOptions) -> Result<Arc<Self>> {
        if options.max_frame == 0 || options.max_frame > FRAME_BOUND {
            return Err("shared log frame bound out of range".into());
        }
        let created = !dir.exists();
        fs::create_dir_all(dir)?;
        if created {
            sync_dir(&parent_of(dir))?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("LOCK"))?;
        lock.try_lock()?;
        let mut generations = Vec::new();
        let mut dead = Vec::new();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(g) = name.strip_prefix("log-").and_then(|r| r.strip_suffix(".nine")) {
                generations.push(g.parse::<u64>()?);
            } else if name.starts_with("log-") && name.ends_with(".dead") {
                dead.push(dir.join(name));
            } else if name.starts_with("spare-") && name.ends_with(".nine") {
                // Zeros (and at most a header): never part of the log. A
                // closing instance's spare thread may remove it first.
                match fs::remove_file(dir.join(name)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
        // Finish an interrupted scrub: these files were fully absorbed.
        for path in &dead {
            zero_and_remove(path)?;
        }
        if !dead.is_empty() {
            sync_dir(dir)?;
        }
        generations.sort_unstable();
        let id = read_or_create_id(dir, generations.is_empty())?;
        let mut inner = Inner {
            files: BTreeMap::new(),
            stores: HashMap::new(),
            next_generation: generations.last().map_or(1, |g| g + 1),
            poisoned: false,
            #[cfg(test)]
            fail_after_bytes: None,
        };
        let newest = generations.last().copied();
        for generation in generations {
            let path = dir.join(file_name(generation));
            let sealed = Some(generation) != newest;
            let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if bytes.len() < FILE_HEADER {
                if sealed {
                    return Err("sealed shared log file has an incomplete header".into());
                }
                // Only a crash inside `create` leaves a short header, and
                // nothing was written through such a file.
                drop(file);
                fs::remove_file(&path)?;
                sync_dir(dir)?;
                continue;
            }
            let salt = check_header(&bytes, generation, &id)?;
            let scanned = scan_file(&bytes, generation, salt, sealed)?;
            // Cut the newest file's torn tail; a sealed file keeps its zeros.
            if !sealed && scanned.end < bytes.len() {
                file.set_len(scanned.end as u64)?;
                file.sync_all()?;
            }
            for (store, loc) in scanned.batches {
                inner.stores.entry(store).or_default().batches.push_back(loc);
            }
            inner.files.insert(
                generation,
                LogFile {
                    path,
                    file,
                    salt,
                    bytes: scanned.end as u64,
                    capacity: 0,
                    seq: scanned.seq,
                    ends: scanned.ends,
                },
            );
        }
        // Appends always go to a file this process created; recovered files
        // stay until their stores absorb them. The first file grows by
        // appends until the background spare is ready.
        let generation = inner.next_generation;
        inner.files.insert(generation, LogFile::create(dir, generation, &id)?);
        inner.next_generation += 1;
        let log = Arc::new(Self {
            dir: dir.into(),
            id,
            options,
            _lock: lock,
            queue: Mutex::new(Queue::default()),
            inner: Mutex::new(inner),
            spare: Arc::default(),
            rounds: AtomicU64::new(0),
            batches: AtomicU64::new(0),
        });
        // Files without frames (e.g. an unused active file) go now.
        log.want_spare();
        let retired = log.retirable(&mut relock(&log.inner));
        log.remove_files(retired);
        Ok(log)
    }

    /// The log identity (hex), recorded in the manifest of each bound store.
    pub fn id(&self) -> String {
        self.id.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Store ids with rows in the log that their stores have not absorbed.
    /// Owners open these stores on start so recovery can replay them.
    pub fn stores(&self) -> Vec<u64> {
        let inner = relock(&self.inner);
        let mut v: Vec<u64> = inner
            .stores
            .iter()
            .filter(|(_, s)| !s.batches.is_empty())
            .map(|(id, _)| *id)
            .collect();
        v.sort_unstable();
        v
    }

    /// Stores whose unabsorbed rows keep the oldest sealed file alive: the
    /// ones to flush when the log grows past its budget.
    pub fn pinning(&self) -> Vec<u64> {
        let inner = relock(&self.inner);
        let active = inner.active();
        let Some((_, oldest)) = inner.files.iter().find(|(g, _)| **g != active) else {
            return Vec::new();
        };
        let mut v: Vec<u64> = oldest
            .ends
            .iter()
            .filter(|(s, end)| !inner.absorbed(**s, **end))
            .map(|(s, _)| *s)
            .collect();
        v.sort_unstable();
        v
    }

    /// Bytes in every live log file.
    pub fn bytes(&self) -> u64 {
        relock(&self.inner).files.values().map(|f| f.bytes).sum()
    }

    pub fn files(&self) -> Vec<PathBuf> {
        relock(&self.inner).files.values().map(|f| f.path.clone()).collect()
    }

    pub fn stats(&self) -> SharedLogStats {
        let (files, bytes) = {
            let inner = relock(&self.inner);
            (inner.files.len(), inner.files.values().map(|f| f.bytes).sum())
        };
        SharedLogStats {
            rounds: self.rounds.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
            files,
            bytes,
        }
    }

    /// Start a new active file when every frame of the current one is
    /// absorbed, then remove every absorbed sealed file. Returns how many
    /// files were removed.
    pub fn checkpoint(&self) -> Result<usize> {
        let retired = {
            let mut inner = relock(&self.inner);
            if inner.poisoned {
                return Err(POISONED.into());
            }
            let active = &inner.files[&inner.active()];
            if active.bytes > FILE_HEADER as u64
                && active.ends.iter().all(|(s, end)| inner.absorbed(*s, *end))
            {
                self.rotate(&mut inner)?;
            }
            self.retirable(&mut inner)
        };
        let n = retired.len();
        self.remove_files(retired);
        Ok(n)
    }

    /// Erase the log: every frame must already be absorbed by its store
    /// (flush every store first), else this fails and changes nothing.
    /// Starts a new file, then renames every other log file to `.dead`,
    /// overwrites it with zeros, syncs it and removes it, so no file of the
    /// log holds a byte written before the call. Leftovers of an earlier
    /// interrupted scrub or retirement are zeroed and removed as well.
    pub fn scrub(&self) -> Result<()> {
        let (old, leftovers) = {
            let mut inner = relock(&self.inner);
            if inner.poisoned {
                return Err(POISONED.into());
            }
            for f in inner.files.values() {
                for (s, end) in &f.ends {
                    if !inner.absorbed(*s, *end) {
                        return Err(format!(
                            "the shared log holds rows of store {s} that it has not absorbed; \
                             flush every store before scrubbing"
                        )
                        .into());
                    }
                }
            }
            if inner.files[&inner.active()].bytes > FILE_HEADER as u64 {
                self.rotate(&mut inner)?;
            }
            let keep = inner.active();
            let gens: Vec<u64> = inner.files.keys().filter(|g| **g != keep).copied().collect();
            let old: Vec<LogFile> = gens
                .iter()
                .map(|g| inner.files.remove(g).expect("listed"))
                .collect();
            for s in inner.stores.values_mut() {
                s.batches.clear();
            }
            // Files no longer tracked: retirements whose removal failed, or
            // `.dead` files of an interrupted scrub.
            let tracked: HashSet<PathBuf> = old.iter().map(|f| f.path.clone()).collect();
            let mut leftovers = Vec::new();
            for entry in fs::read_dir(&self.dir)? {
                let name = entry?.file_name();
                let Some(name) = name.to_str() else { continue };
                let path = self.dir.join(name);
                let stale = name
                    .strip_prefix("log-")
                    .and_then(|r| r.strip_suffix(".nine"))
                    .and_then(|g| g.parse::<u64>().ok())
                    .is_some_and(|g| g != keep && !tracked.contains(&path));
                if stale || (name.starts_with("log-") && name.ends_with(".dead")) {
                    leftovers.push(path);
                }
            }
            (old, leftovers)
        };
        // Rename first (and sync the directory): a file is zeroed only once
        // its name says it is retired, so a crash mid-zeroing never leaves a
        // `log-G.nine` with a wiped header for the next open to trip on.
        let mut dead = Vec::with_capacity(old.len());
        for f in old {
            let to = f.path.with_extension("dead");
            fs::rename(&f.path, &to)?;
            dead.push((f.file, to));
        }
        let mut stale = Vec::with_capacity(leftovers.len());
        for path in leftovers {
            if path.extension().is_some_and(|e| e == "dead") {
                stale.push(path);
                continue;
            }
            let to = path.with_extension("dead");
            match fs::rename(&path, &to) {
                Ok(()) => stale.push(to),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        sync_dir(&self.dir)?;
        for (file, path) in dead {
            zero_file(&file)?;
            drop(file);
            fs::remove_file(&path)?;
        }
        for path in stale {
            zero_and_remove(&path)?;
        }
        sync_dir(&self.dir)
    }

    /// Start a new active file (the spare when one is ready); the old one is
    /// sealed (complete and synced).
    fn rotate(&self, inner: &mut Inner) -> Result<()> {
        let generation = inner.next_generation;
        let ready = relock(&self.spare).ready.take();
        let file = match ready {
            Some(spare) => LogFile::from_spare(&self.dir, spare, generation, &self.id)?,
            None => LogFile::create(&self.dir, generation, &self.id)?,
        };
        inner.next_generation += 1;
        inner.files.insert(generation, file);
        self.want_spare();
        Ok(())
    }

    /// Start preparing a zero-filled spare in the background if none is
    /// ready or in progress.
    fn want_spare(&self) {
        let mut spare = relock(&self.spare);
        if !self.options.preallocate || spare.ready.is_some() || spare.preparing || spare.closed {
            return;
        }
        let (slot, dir, size) = (Arc::clone(&self.spare), self.dir.clone(), self.options.segment_bytes);
        let path = dir.join(format!("spare-{:016x}.nine", random_salt(&dir)));
        spare.preparing = true;
        let spawned = std::thread::Builder::new().name("nine-log-spare".into()).spawn(move || {
            let made = (|| -> Result<File> {
                let file = create_private(&path)?;
                zero_fill(&file, size.max(FILE_HEADER as u64))?;
                file.sync_all()?;
                sync_dir(&dir)?;
                Ok(file)
            })();
            let mut spare = relock(&slot);
            spare.preparing = false;
            match made {
                Ok(file) if !spare.closed => spare.ready = Some((path, file)),
                _ => {
                    drop(spare);
                    let _ = fs::remove_file(&path);
                }
            }
        });
        if spawned.is_err() {
            spare.preparing = false;
        }
    }

    /// Take out every sealed file whose frames are all absorbed.
    fn retirable(&self, inner: &mut Inner) -> Vec<LogFile> {
        let active = inner.active();
        let gens: Vec<u64> = inner
            .files
            .iter()
            .filter(|(g, f)| {
                **g != active && f.ends.iter().all(|(s, end)| inner.absorbed(*s, *end))
            })
            .map(|(g, _)| *g)
            .collect();
        if gens.is_empty() {
            return Vec::new();
        }
        let out: Vec<LogFile> = gens
            .iter()
            .map(|g| inner.files.remove(g).expect("listed"))
            .collect();
        for s in inner.stores.values_mut() {
            s.batches.retain(|b| !gens.contains(&b.generation));
        }
        out
    }

    fn remove_files(&self, files: Vec<LogFile>) {
        if files.is_empty() {
            return;
        }
        for f in files {
            drop(f.file);
            // A file that cannot be removed holds only absorbed rows: the
            // next open re-reads and skips it, and `scrub` removes it.
            let _ = fs::remove_file(&f.path);
        }
        let _ = sync_dir(&self.dir);
    }

    fn attach(&self, store: u64, durable: u64, created: bool) -> Result<(u64, Vec<Vec<u8>>)> {
        let retired;
        let out = {
            let mut inner = relock(&self.inner);
            if inner.poisoned {
                return Err(POISONED.into());
            }
            let s = inner.stores.entry(store).or_default();
            if s.attached {
                return Err("store id is already attached to this shared log".into());
            }
            if created && (!s.batches.is_empty() || s.absorbed.is_some_and(|a| a > 0)) {
                return Err(
                    "the shared log holds rows of this store id but the store has no manifest"
                        .into(),
                );
            }
            if s.absorbed.is_some_and(|a| a > durable) {
                return Err("store is behind rows the shared log already retired".into());
            }
            let locs: Vec<BatchLoc> = s.batches.iter().filter(|b| b.end() > durable).copied().collect();
            let mut next = durable;
            let mut out = Vec::new();
            for loc in &locs {
                if loc.first_row > next {
                    return Err("the shared log is missing rows of this store".into());
                }
                if loc.first_row < next && next != durable {
                    return Err("the shared log holds overlapping rows of this store".into());
                }
                let f = inner
                    .files
                    .get(&loc.generation)
                    .ok_or("shared log file of a batch is gone")?;
                let bytes = read_at(&f.file, loc.offset, loc.len)?;
                let mut entries = Vec::new();
                decode_batch(&bytes, f.salt, store, loc.first_row, &mut entries)?;
                if entries.len() as u64 != loc.rows {
                    return Err("shared log batch row count changed".into());
                }
                out.extend(entries.into_iter().skip((next - loc.first_row) as usize));
                next = loc.end();
            }
            let s = inner.stores.get_mut(&store).expect("inserted");
            s.absorbed = Some(durable);
            s.batches.retain(|b| b.end() > durable);
            s.attached = true;
            s.next = Some(next);
            retired = self.retirable(&mut inner);
            out
        };
        self.remove_files(retired);
        Ok((durable, out))
    }

    fn detach(&self, store: u64) {
        if let Some(s) = relock(&self.inner).stores.get_mut(&store) {
            s.attached = false;
            s.next = None;
        }
    }

    /// The store's segments durably hold rows `..rows`.
    pub(crate) fn absorbed(&self, store: u64, rows: u64) {
        let retired = {
            let mut inner = relock(&self.inner);
            let s = inner.stores.entry(store).or_default();
            s.absorbed = Some(s.absorbed.map_or(rows, |a| a.max(rows)));
            s.batches.retain(|b| b.end() > rows);
            self.retirable(&mut inner)
        };
        self.remove_files(retired);
    }

    /// Append one atomic batch of `store` (rows `first_row..` in order) and
    /// return once it is durable. Batches of concurrent callers, whatever
    /// their store, share one write and one sync.
    #[cfg(test)]
    pub(crate) fn append(&self, store: u64, first_row: u64, entries: &[Vec<u8>]) -> Result<()> {
        self.enqueue(store, first_row, entries)?.wait()
    }

    /// Queue one atomic batch of `store` (rows `first_row..`); the batch is
    /// durable once [`Pending::wait`] returns Ok. A store queues its batches
    /// in row order (the caller serializes them) but need not wait for one
    /// before queuing the next: they share rounds with every other store.
    pub(crate) fn enqueue(&self, store: u64, first_row: u64, entries: &[Vec<u8>]) -> Result<Pending<'_>> {
        let (frames, starts) = encode_batch(store, first_row, entries, self.options.max_frame)?;
        let slot = Arc::new(Slot {
            state: Mutex::new(SlotState::Waiting),
            cv: Condvar::new(),
        });
        let lead = {
            let mut q = relock(&self.queue);
            q.pending.push(Ticket {
                store,
                first_row,
                rows: entries.len() as u64,
                frames,
                starts,
                slot: Arc::clone(&slot),
            });
            !std::mem::replace(&mut q.leader, true)
        };
        Ok(Pending {
            log: self,
            slot,
            lead,
            done: false,
        })
    }

    /// One group commit of everything queued (the leader's own batch
    /// included), then hand leadership to the next waiting writer.
    fn lead_round(&self) {
        let tickets = std::mem::take(&mut relock(&self.queue).pending);
        let results = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.write_round(&tickets)
        }))
        .unwrap_or_else(|_| {
            relock(&self.inner).poisoned = true;
            tickets
                .iter()
                .map(|_| Err("shared log commit panicked; reopen the database".into()))
                .collect()
        });
        for (t, r) in tickets.iter().zip(results) {
            *relock(&t.slot.state) = SlotState::Done(r);
            t.slot.cv.notify_all();
        }
        let next = {
            let mut q = relock(&self.queue);
            match q.pending.first() {
                None => {
                    q.leader = false;
                    None
                }
                Some(t) => Some(Arc::clone(&t.slot)),
            }
        };
        if let Some(slot) = next {
            *relock(&slot.state) = SlotState::Lead;
            slot.cv.notify_all();
        }
    }

    fn write_round(&self, tickets: &[Ticket]) -> Vec<Result<()>> {
        let fail = |m: &str| tickets.iter().map(|_| Err(m.to_string().into())).collect();
        let mut guard = relock(&self.inner);
        if guard.poisoned {
            return fail(POISONED);
        }
        let need: u64 = tickets.iter().map(|t| t.frames.len() as u64).sum();
        let f = &guard.files[&guard.active()];
        let used = f.bytes > FILE_HEADER as u64;
        // Rotate when the file is full, when this round would overflow its
        // zero-filled space, or to leave a growing file for a ready spare.
        let rotate = (used && f.bytes >= self.options.segment_bytes)
            || (used && f.capacity > 0 && f.bytes + need > f.capacity)
            || (f.capacity == 0 && relock(&self.spare).ready.is_some());
        if rotate {
            if let Err(e) = self.rotate(&mut guard) {
                guard.poisoned = true;
                return fail(&e.to_string());
            }
        }
        let generation = guard.active();
        let inner = &mut *guard;
        let f = inner.files.get_mut(&generation).expect("active");
        let mut results: Vec<Result<()>> = Vec::with_capacity(tickets.len());
        let mut next: HashMap<u64, u64> = HashMap::new();
        let mut buf = Vec::with_capacity(tickets.iter().map(|t| t.frames.len()).sum());
        let mut seq = f.seq;
        let round = f.seq;
        let mut locs = Vec::with_capacity(tickets.len());
        for t in tickets {
            // Each store's rows enter the log contiguously and in order.
            let expected = next
                .get(&t.store)
                .copied()
                .or_else(|| inner.stores.get(&t.store).filter(|s| s.attached).and_then(|s| s.next));
            if expected != Some(t.first_row) {
                results.push(Err("batch does not continue its store's rows in the shared log".into()));
                continue;
            }
            let start = buf.len();
            buf.extend_from_slice(&t.frames);
            for &s in &t.starts {
                seal(&mut buf[start + s..start + s + FRAME_HEADER], seq, round, f.salt);
                seq += 1;
            }
            next.insert(t.store, t.first_row + t.rows);
            locs.push((
                t.store,
                BatchLoc {
                    generation,
                    offset: f.bytes + start as u64,
                    len: t.frames.len() as u64,
                    first_row: t.first_row,
                    rows: t.rows,
                },
            ));
            results.push(Ok(()));
        }
        if locs.is_empty() {
            return results;
        }
        inner.poisoned = true;
        let written: Result<()> = (|| {
            #[cfg(test)]
            if let Some(n) = inner.fail_after_bytes.take() {
                write_all_at(&f.file, f.bytes, &buf[..n.min(buf.len())])?;
                return Err("injected partial write".into());
            }
            write_all_at(&f.file, f.bytes, &buf)?;
            f.file.sync_data()?;
            count_journal_sync();
            Ok(())
        })();
        if let Err(e) = written {
            let message = e.to_string();
            return results
                .into_iter()
                .map(|r| r.and_then(|_| Err(message.clone().into())))
                .collect();
        }
        inner.poisoned = false;
        f.bytes += buf.len() as u64;
        f.seq = seq;
        for (store, loc) in &locs {
            let e = f.ends.entry(*store).or_insert(0);
            *e = (*e).max(loc.end());
        }
        let n = locs.len() as u64;
        for (store, loc) in locs {
            let s = inner.stores.entry(store).or_default();
            s.batches.push_back(loc);
            s.next = Some(loc.end());
        }
        self.rounds.fetch_add(1, Ordering::Relaxed);
        self.batches.fetch_add(n, Ordering::Relaxed);
        results
    }

    #[cfg(test)]
    pub(crate) fn fail_next_write(&self, after_bytes: usize) {
        relock(&self.inner).fail_after_bytes = Some(after_bytes);
    }

    #[cfg(test)]
    pub(crate) fn locations(&self, store: u64) -> Vec<BatchLoc> {
        relock(&self.inner)
            .stores
            .get(&store)
            .map(|s| s.batches.iter().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
