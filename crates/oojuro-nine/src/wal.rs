//! v4 journal: append-only, CRC-protected frames with group commit.
//!
//! File header (32 bytes): magic `NINEW004`, salt u64, first_row u64, kind u8,
//! 3 reserved bytes, CRC32 of the preceding 28 bytes.
//!
//! Frame header (24 bytes): payload_len u32, flags u8, 3 reserved bytes,
//! seq u64, payload CRC32, header CRC32 of (salt, first 20 header bytes).
//! The payload is a sequence of LEB128-length-prefixed entries.
//!
//! One `append` is one atomic batch: it may span several frames, all but the
//! last flagged MORE, followed by a single `sync_data`. Recovery delivers a
//! batch only once its final frame is valid.
//!
//! Recovery rule: at the first frame that is incomplete or fails a check, the
//! rest of the file is searched for any later valid frame (salted header CRC,
//! sequence at least the expected one, payload CRC). If one exists the damage
//! is in committed history and opening fails without modifying the file.
//! Otherwise the damaged bytes are a torn tail and the file is truncated to
//! the start of the incomplete batch. A corrupted length that points past
//! end-of-file can therefore no longer discard committed frames after it.
//! Damage confined to the very last frame is indistinguishable from a torn
//! write; like other write-ahead logs, it is treated as one.
use crate::codec::{get_varint, put_varint};
use oojuro_core::Result;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAGIC: [u8; 8] = *b"NINEW004";
const FILE_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
const MORE: u8 = 1;
/// Bound on one frame's payload; larger batches span several frames.
#[cfg(not(test))]
pub(crate) const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
#[cfg(test)]
pub(crate) const MAX_PAYLOAD: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Records = 1,
    Outcomes = 2,
    Receipts = 3,
}

pub(crate) struct Journal {
    file: File,
    path: PathBuf,
    salt: u64,
    first_row: u64,
    seq: u64,
    end: u64,
    poisoned: bool,
    #[cfg(test)]
    pub(crate) fail_after_bytes: Option<usize>,
}

fn header_crc(salt: u64, h: &[u8]) -> u32 {
    let mut c = crc32fast::Hasher::new();
    c.update(&salt.to_le_bytes());
    c.update(&h[..20]);
    c.finalize()
}

/// Journal `sync_data` calls made by this process (private journals and the
/// shared log): the per-commit durability cost.
pub(crate) static JOURNAL_SYNCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn count_journal_sync() {
    JOURNAL_SYNCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(feature = "simulated-sync")]
    simulated_sync_delay();
}

/// Benchmark-only (feature `simulated-sync`): sleep
/// `OOJURO_NINE_SIMULATED_SYNC_US` after every journal sync, to model a
/// device's flush latency on tmpfs. Sleeps overlap freely, unlike flushes
/// on one device, so only sync counts (not throughput) carry over.
#[cfg(feature = "simulated-sync")]
fn simulated_sync_delay() {
    static DELAY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let us = *DELAY.get_or_init(|| {
        std::env::var("OOJURO_NINE_SIMULATED_SYNC_US")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    });
    if us > 0 {
        std::thread::sleep(std::time::Duration::from_micros(us));
    }
}

pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

pub(crate) fn random_salt(path: &Path) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(path.as_os_str().as_encoded_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    h.update(&now.as_nanos().to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(&(&now as *const _ as usize).to_le_bytes());
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().expect("8 bytes"))
}

struct Frame {
    len: usize,
    flags: u8,
    seq: u64,
}

impl Journal {
    /// Create a new, empty journal and make its header durable.
    pub(crate) fn create(path: &Path, kind: Kind, first_row: u64) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        let salt = random_salt(path);
        file.write_all(&file_header(salt, first_row, kind))?;
        file.sync_all()?;
        if let Some(dir) = path.parent() {
            sync_dir(dir)?;
        }
        Ok(Self {
            file,
            path: path.into(),
            salt,
            first_row,
            seq: 0,
            end: FILE_HEADER as u64,
            poisoned: false,
            #[cfg(test)]
            fail_after_bytes: None,
        })
    }

    /// Open an existing journal, delivering every committed entry in order.
    /// Callback effects are tentative until the whole open succeeds.
    pub(crate) fn open(
        path: &Path,
        kind: Kind,
        mut visit: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.len() < FILE_HEADER {
            // Only a crash inside `create` leaves a short header, and nothing
            // was acknowledged through such a file. The caller decides.
            return Err("v4 journal header is incomplete; it was never initialized".into());
        }
        let h = &bytes[..FILE_HEADER];
        if h[..8] != MAGIC {
            return Err("journal magic/version mismatch".into());
        }
        if crc32fast::hash(&h[..28]) != u32::from_le_bytes(h[28..32].try_into()?) {
            return Err("journal header checksum mismatch".into());
        }
        if h[24] != kind as u8 || h[25..28] != [0, 0, 0] {
            return Err("journal kind mismatch".into());
        }
        let salt = u64::from_le_bytes(h[8..16].try_into()?);
        let first_row = u64::from_le_bytes(h[16..24].try_into()?);
        let mut pos = FILE_HEADER;
        let mut seq = 0u64;
        let mut batch_start = pos;
        let mut batch_seq = seq;
        let mut pending: Vec<(usize, usize)> = Vec::new();
        let mut damaged = None;
        while pos < bytes.len() {
            let Some(frame) = parse(&bytes, pos, salt, seq, true) else {
                damaged = Some(pos);
                break;
            };
            let body = pos + FRAME_HEADER;
            let stop = body + frame.len;
            let mut at = body;
            while at < stop {
                let n = usize::try_from(get_varint(&bytes[..stop], &mut at)?)?;
                let end = at.checked_add(n).filter(|e| *e <= stop);
                let end = end.ok_or("journal entry exceeds its frame")?;
                pending.push((at, end));
                at = end;
            }
            pos = stop;
            seq = frame.seq + 1;
            if frame.flags & MORE == 0 {
                for (a, b) in pending.drain(..) {
                    visit(&bytes[a..b])?;
                }
                batch_start = pos;
                batch_seq = seq;
            }
        }
        if let Some(at) = damaged {
            if (at + 1..bytes.len()).any(|p| parse(&bytes, p, salt, seq, false).is_some()) {
                return Err(
                    "journal frame is corrupt before committed frames; refusing to truncate"
                        .into(),
                );
            }
        }
        // A torn tail or an unfinished multi-frame batch: drop it.
        if batch_start < bytes.len() {
            file.set_len(batch_start as u64)?;
            file.sync_all()?;
        }
        file.seek(SeekFrom::Start(batch_start as u64))?;
        Ok(Self {
            file,
            path: path.into(),
            salt,
            first_row,
            seq: batch_seq,
            end: batch_start as u64,
            poisoned: false,
            #[cfg(test)]
            fail_after_bytes: None,
        })
    }

    pub(crate) fn first_row(&self) -> u64 {
        self.first_row
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.end
    }

    /// Write one atomic batch of entries; durable only when `sync` is true.
    pub(crate) fn append<'a>(
        &mut self,
        entries: impl IntoIterator<Item = &'a [u8]>,
        sync: bool,
    ) -> Result<()> {
        if self.poisoned {
            return Err("journal I/O previously failed; close and reopen for recovery".into());
        }
        let salt = self.salt;
        let mut out = Vec::new();
        let mut payload = Vec::new();
        let mut seq = self.seq;
        let mut frame = |payload: &mut Vec<u8>, last: bool, out: &mut Vec<u8>| {
            let mut h = [0u8; FRAME_HEADER];
            h[..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            h[4] = if last { 0 } else { MORE };
            h[8..16].copy_from_slice(&seq.to_le_bytes());
            h[16..20].copy_from_slice(&crc32fast::hash(payload).to_le_bytes());
            let crc = header_crc(salt, &h);
            h[20..24].copy_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&h);
            out.extend_from_slice(payload);
            payload.clear();
            seq += 1;
        };
        let mut prefix = Vec::with_capacity(10);
        for e in entries {
            prefix.clear();
            put_varint(&mut prefix, e.len() as u64);
            if prefix.len() + e.len() > MAX_PAYLOAD {
                return Err("journal entry exceeds the frame bound".into());
            }
            if payload.len() + prefix.len() + e.len() > MAX_PAYLOAD {
                frame(&mut payload, false, &mut out);
            }
            payload.extend_from_slice(&prefix);
            payload.extend_from_slice(e);
        }
        frame(&mut payload, true, &mut out);
        self.poisoned = true;
        #[cfg(test)]
        if let Some(n) = self.fail_after_bytes.take() {
            self.file.write_all(&out[..n.min(out.len())])?;
            return Err(std::io::Error::other("injected partial write").into());
        }
        self.file.write_all(&out)?;
        if sync {
            self.file.sync_data()?;
            count_journal_sync();
        }
        self.poisoned = false;
        self.seq = seq;
        self.end += out.len() as u64;
        Ok(())
    }

    pub(crate) fn sync(&mut self) -> Result<()> {
        if self.poisoned {
            return Err("journal I/O previously failed; close and reopen for recovery".into());
        }
        self.file.sync_data()?;
        count_journal_sync();
        Ok(())
    }

    /// Copy committed bytes through the owned handle (Windows locks are
    /// mandatory, so reopening for reading is not portable).
    pub(crate) fn copy_committed(&mut self, destination: &mut impl Write) -> Result<u64> {
        if self.poisoned {
            return Err("cannot snapshot a poisoned journal".into());
        }
        self.file.sync_data()?;
        self.file.seek(SeekFrom::Start(0))?;
        let copied = std::io::copy(&mut (&mut self.file).take(self.end), destination);
        if let Err(error) = self.file.seek(SeekFrom::Start(self.end)) {
            self.poisoned = true;
            return Err(error.into());
        }
        Ok(copied?)
    }
}

fn file_header(salt: u64, first_row: u64, kind: Kind) -> [u8; FILE_HEADER] {
    let mut h = [0u8; FILE_HEADER];
    h[..8].copy_from_slice(&MAGIC);
    h[8..16].copy_from_slice(&salt.to_le_bytes());
    h[16..24].copy_from_slice(&first_row.to_le_bytes());
    h[24] = kind as u8;
    let crc = crc32fast::hash(&h[..28]);
    h[28..32].copy_from_slice(&crc.to_le_bytes());
    h
}

/// A frame is valid only if both CRCs hold and it fits in the file. In-order
/// replay (`exact`) also needs the next sequence number; the corruption scan
/// accepts any later one.
fn parse(bytes: &[u8], pos: usize, salt: u64, seq: u64, exact: bool) -> Option<Frame> {
    let h = bytes.get(pos..pos.checked_add(FRAME_HEADER)?)?;
    let len = u32::from_le_bytes(h[..4].try_into().ok()?) as usize;
    if len > MAX_PAYLOAD || h[4] & !MORE != 0 || h[5..8] != [0, 0, 0] {
        return None;
    }
    let found = u64::from_le_bytes(h[8..16].try_into().ok()?);
    if found < seq || (exact && found != seq) {
        return None;
    }
    if header_crc(salt, h) != u32::from_le_bytes(h[20..24].try_into().ok()?) {
        return None;
    }
    let body = bytes.get(pos + FRAME_HEADER..pos + FRAME_HEADER + len)?;
    if crc32fast::hash(body) != u32::from_le_bytes(h[16..20].try_into().ok()?) {
        return None;
    }
    Some(Frame {
        len,
        flags: h[4],
        seq: found,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nine-wal-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("j")
    }

    fn replay(path: &Path) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        Journal::open(path, Kind::Records, |e| {
            out.push(e.to_vec());
            Ok(())
        })?;
        Ok(out)
    }

    fn entries(n: usize, size: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| vec![(i % 251) as u8; size + i]).collect()
    }

    #[test]
    fn batches_roundtrip_and_kind_is_checked() {
        let path = temp("roundtrip");
        let mut j = Journal::create(&path, Kind::Records, 42).unwrap();
        let a = entries(3, 10);
        let b = entries(1, 0);
        j.append(a.iter().map(Vec::as_slice), true).unwrap();
        j.append(b.iter().map(Vec::as_slice), true).unwrap();
        j.append(std::iter::empty(), true).unwrap();
        drop(j);
        let got = replay(&path).unwrap();
        assert_eq!(got, [a, b].concat());
        let j = Journal::open(&path, Kind::Records, |_| Ok(())).unwrap();
        assert_eq!(j.first_row(), 42);
        drop(j);
        assert!(Journal::open(&path, Kind::Receipts, |_| Ok(())).is_err());
        assert!(Journal::create(&path, Kind::Records, 0).is_err());
    }

    #[test]
    fn every_torn_tail_length_recovers_the_committed_prefix() {
        let path = temp("torn");
        let mut j = Journal::create(&path, Kind::Records, 0).unwrap();
        let first = entries(4, 30);
        j.append(first.iter().map(Vec::as_slice), true).unwrap();
        drop(j);
        let committed = std::fs::read(&path).unwrap();
        let mut j = Journal::open(&path, Kind::Records, |_| Ok(())).unwrap();
        j.append(entries(2, 50).iter().map(Vec::as_slice), true).unwrap();
        drop(j);
        let full = std::fs::read(&path).unwrap();
        for cut in committed.len()..full.len() {
            std::fs::write(&path, &full[..cut]).unwrap();
            assert_eq!(replay(&path).unwrap(), first, "cut at {cut}");
            assert_eq!(std::fs::read(&path).unwrap(), committed);
        }
        // Zero-filled tails (delayed allocation after a crash) are torn too.
        let mut zeroed = committed.clone();
        zeroed.extend_from_slice(&[0u8; 300]);
        std::fs::write(&path, &zeroed).unwrap();
        assert_eq!(replay(&path).unwrap(), first);
        assert_eq!(std::fs::read(&path).unwrap(), committed);
        // Appends continue after recovery with the right sequence.
        let mut j = Journal::open(&path, Kind::Records, |_| Ok(())).unwrap();
        j.append([b"after".as_slice()], true).unwrap();
        drop(j);
        assert_eq!(replay(&path).unwrap().len(), first.len() + 1);
    }

    #[test]
    fn corrupted_header_before_committed_frames_is_an_error_not_a_truncation() {
        let path = temp("header");
        let mut j = Journal::create(&path, Kind::Records, 0).unwrap();
        for i in 0..3 {
            j.append(entries(2, 20 + i).iter().map(Vec::as_slice), true).unwrap();
        }
        drop(j);
        let good = std::fs::read(&path).unwrap();
        // Length past end-of-file (the v3 failure), flags, and sequence damage.
        for (offset, value) in [
            (FILE_HEADER, 0xff),
            (FILE_HEADER + 2, 0x10),
            (FILE_HEADER + 4, 2),
            (FILE_HEADER + 9, 1),
            (FILE_HEADER + 21, 4),
        ] {
            let mut bad = good.clone();
            bad[offset] ^= value;
            std::fs::write(&path, &bad).unwrap();
            assert!(replay(&path).is_err(), "offset {offset}");
            assert_eq!(std::fs::read(&path).unwrap(), bad, "file must be untouched");
        }
        // Payload damage in the middle is also an error.
        let mut bad = good.clone();
        bad[FILE_HEADER + FRAME_HEADER + 3] ^= 1;
        std::fs::write(&path, &bad).unwrap();
        assert!(replay(&path).is_err());
        // Damage to the final frame alone is indistinguishable from a torn
        // write and is dropped.
        let mut bad = good.clone();
        *bad.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(replay(&path).unwrap().len(), 4);
        // File header damage is an error.
        let mut bad = good;
        bad[12] ^= 1;
        std::fs::write(&path, &bad).unwrap();
        assert!(replay(&path).is_err());
    }

    #[test]
    fn multi_frame_batches_are_atomic() {
        let path = temp("atomic");
        let mut j = Journal::create(&path, Kind::Records, 0).unwrap();
        let small = entries(2, 5);
        j.append(small.iter().map(Vec::as_slice), true).unwrap();
        drop(j);
        let committed = std::fs::read(&path).unwrap();
        let big: Vec<Vec<u8>> = (0..3).map(|i| vec![i as u8; MAX_PAYLOAD / 2 + 7]).collect();
        let mut j = Journal::open(&path, Kind::Records, |_| Ok(())).unwrap();
        j.append(big.iter().map(Vec::as_slice), true).unwrap();
        drop(j);
        assert_eq!(replay(&path).unwrap(), [small.clone(), big].concat());
        // Cut inside the last frame: the earlier, complete MORE frames of the
        // same batch must not be delivered.
        let full = std::fs::read(&path).unwrap();
        std::fs::write(&path, &full[..full.len() - 10]).unwrap();
        assert_eq!(replay(&path).unwrap(), small);
        assert_eq!(std::fs::read(&path).unwrap(), committed);
        assert!(
            Journal::open(&path, Kind::Records, |_| Ok(()))
                .unwrap()
                .append([vec![0u8; MAX_PAYLOAD].as_slice()], true)
                .is_err()
        );
    }

    #[test]
    fn partial_write_poisons_until_reopen() {
        let path = temp("poison");
        let mut j = Journal::create(&path, Kind::Records, 0).unwrap();
        j.append([b"one".as_slice()], true).unwrap();
        j.fail_after_bytes = Some(10);
        assert!(j.append([b"two".as_slice()], true).is_err());
        assert!(j.append([b"three".as_slice()], true).is_err());
        drop(j);
        let mut j = Journal::open(&path, Kind::Records, |_| Ok(())).unwrap();
        j.append([b"four".as_slice()], true).unwrap();
        drop(j);
        assert_eq!(replay(&path).unwrap(), vec![b"one".to_vec(), b"four".to_vec()]);
    }

    #[test]
    fn a_frame_from_another_journal_is_not_valid_here() {
        let path = temp("salt");
        let mut j = Journal::create(&path, Kind::Records, 0).unwrap();
        j.append([b"x".as_slice()], true).unwrap();
        j.append([b"z".as_slice()], true).unwrap();
        drop(j);
        let mut bytes = std::fs::read(&path).unwrap();
        // Torn garbage, then a frame with a later sequence from another salt.
        let other = temp("salt-other");
        let mut k = Journal::create(&other, Kind::Records, 0).unwrap();
        for _ in 0..4 {
            k.append([b"y".as_slice()], true).unwrap();
        }
        drop(k);
        let foreign = std::fs::read(&other).unwrap()[FILE_HEADER..].to_vec();
        bytes.extend_from_slice(&[7u8; 5]);
        bytes.extend_from_slice(&foreign);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(replay(&path).unwrap(), vec![b"x".to_vec(), b"z".to_vec()]);
    }
}
