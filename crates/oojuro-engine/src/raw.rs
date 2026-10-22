//! A local raw-source authority, independent of parsed text and retrieval indexes.
//!
//! The caller supplies a dedicated directory and key for one writer, stable opaque source
//! identities, and a recording-policy decision BEFORE calling this API. No source path is
//! opened automatically. Descriptors and arbitrary source bytes are sealed in Store frames.
//! Cursors are derived from those same committed frames; there is no second cursor file.

use oojuro_store::{Error, NewRecord, Options, Result, Store, crypto::FrameCipher};
use std::{collections::BTreeMap, io::Write, path::Path, sync::Arc};

const FORMAT: &[u8] = b"oojuro-raw-1";
/// Bounded append/retry work. Adapters split larger inputs and resume from the returned cursor.
pub const MAX_BLOCK_BYTES: usize = 256 * 1024;
pub const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;

/// Stable within one writer. A rotated/replaced/truncated source needs a new identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceId(pub [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// All bytes in [0, offset) are durably retained locally.
    pub offset: u64,
}

struct Block {
    seq: u64,
    offset: u64,
    len: u64,
}
struct Source {
    descriptor: Vec<u8>,
    end: u64,
    blocks: Vec<Block>,
}

pub struct RawJournal {
    store: Store,
    sources: BTreeMap<SourceId, Source>,
    failed: bool,
}

impl RawJournal {
    /// Open with strict sealing and per-append fsync. The key is supplied, never stored.
    /// This is local durability, not replication or a host/AZ survival guarantee.
    pub fn open(dir: &Path, key: &[u8; 32]) -> Result<Self> {
        Self::open_with_parent_sync(dir, key, |parent| std::fs::File::open(parent)?.sync_all())
    }

    fn open_with_parent_sync(
        dir: &Path,
        key: &[u8; 32],
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Self> {
        let parent = dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.is_dir() {
            return Err(Error::Invalid(
                "raw journal parent directory must already exist",
            ));
        }
        if dir.is_dir()
            && !dir.join("MANIFEST").exists()
            && std::fs::read_dir(dir)?.next().is_some()
        {
            return Err(Error::Invalid(
                "raw journal requires a dedicated empty directory",
            ));
        }
        let store = Store::open(
            dir,
            Options {
                cipher: Some(Arc::new(FrameCipher::new(key))),
                require_sealed: true,
                sync: true,
                compact_threads: 1,
                ..Options::default()
            },
        )?;
        let mut journal = Self {
            store,
            sources: BTreeMap::new(),
            failed: false,
        };
        if journal.store.is_empty() {
            journal.commit(FORMAT)?;
        } else if journal.store.get(0)? != FORMAT {
            return Err(Error::Corrupt("not an Oojuro raw journal"));
        }
        for seq in 1..journal.store.len() {
            let bytes = journal.store.get(seq)?;
            journal.replay(seq, &bytes)?;
        }
        // Retry even after FORMAT committed on an earlier open whose parent sync failed.
        // No caller can append/ACK until the directory entry itself is durably linked.
        sync_parent(parent)?;
        Ok(journal)
    }

    fn healthy(&self) -> Result<()> {
        if self.failed {
            Err(Error::Invalid(
                "raw append failed; close and reopen before any operation",
            ))
        } else {
            Ok(())
        }
    }

    fn commit(&mut self, text: &[u8]) -> Result<u64> {
        self.healthy()?;
        let result = self.store.append(&[NewRecord {
            kind: 7,
            valid_at: 0,
            recorded_at: 0,
            key: b"",
            attrs: b"",
            text,
        }]);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn replay(&mut self, seq: u64, bytes: &[u8]) -> Result<()> {
        if bytes.len() < 17 {
            return Err(Error::Corrupt("raw record header"));
        }
        let id = SourceId(bytes[1..17].try_into().unwrap());
        match bytes[0] {
            1 => {
                let descriptor = &bytes[17..];
                if descriptor.len() > MAX_DESCRIPTOR_BYTES || self.sources.contains_key(&id) {
                    return Err(Error::Corrupt("raw source registration"));
                }
                self.sources.insert(
                    id,
                    Source {
                        descriptor: descriptor.to_vec(),
                        end: 0,
                        blocks: Vec::new(),
                    },
                );
            }
            2 => {
                if bytes.len() < 26 || bytes.len() - 25 > MAX_BLOCK_BYTES {
                    return Err(Error::Corrupt("raw block length"));
                }
                let offset = u64::from_le_bytes(bytes[17..25].try_into().unwrap());
                let source = self
                    .sources
                    .get_mut(&id)
                    .ok_or(Error::Corrupt("raw block has no source"))?;
                if offset != source.end {
                    return Err(Error::Corrupt("raw source offset gap"));
                }
                let len = (bytes.len() - 25) as u64;
                source.end = offset
                    .checked_add(len)
                    .ok_or(Error::Corrupt("raw source offset overflow"))?;
                source.blocks.push(Block { seq, offset, len });
            }
            _ => return Err(Error::Corrupt("unknown raw record kind")),
        }
        Ok(())
    }

    /// Descriptor may encode a path, mode or harness metadata; it is opaque sealed data.
    /// Repeated registration must be byte-identical. No plaintext descriptor sidecar exists.
    pub fn register(&mut self, id: SourceId, descriptor: &[u8]) -> Result<()> {
        self.healthy()?;
        if descriptor.len() > MAX_DESCRIPTOR_BYTES {
            return Err(Error::Invalid("raw descriptor too large"));
        }
        if let Some(source) = self.sources.get(&id) {
            return if source.descriptor == descriptor {
                Ok(())
            } else {
                Err(Error::Invalid(
                    "source identity reused with a different descriptor",
                ))
            };
        }
        let mut record = Vec::with_capacity(17 + descriptor.len());
        record.push(1);
        record.extend_from_slice(&id.0);
        record.extend_from_slice(descriptor);
        let seq = self.commit(&record)?;
        self.replay(seq, &record)
    }

    pub fn descriptor(&self, id: SourceId) -> Result<&[u8]> {
        self.healthy()?;
        Ok(&self.sources.get(&id).ok_or(Error::NotFound)?.descriptor)
    }

    pub fn cursor(&self, id: SourceId) -> Result<Cursor> {
        self.healthy()?;
        Ok(Cursor {
            offset: self.sources.get(&id).ok_or(Error::NotFound)?.end,
        })
    }

    /// Retry already recorded bytes safely; an overlapping prefix must match byte for byte.
    /// Gaps and changed prefixes fail instead of silently skipping or rewriting history.
    /// Offset/prefix checking is serialized by &mut self and Store's process lock.
    pub fn append(&mut self, id: SourceId, offset: u64, bytes: &[u8]) -> Result<Cursor> {
        self.healthy()?;
        if bytes.len() > MAX_BLOCK_BYTES {
            return Err(Error::Invalid("raw block too large"));
        }
        let end = self.cursor(id)?.offset;
        if offset > end {
            return Err(Error::Invalid("raw source offset gap"));
        }
        offset
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Invalid("raw source offset overflow"))?;
        let overlap = (end - offset).min(bytes.len() as u64) as usize;
        if self.read_range(id, offset, overlap)? != bytes[..overlap] {
            return Err(Error::Invalid(
                "raw source prefix changed; use a new source identity",
            ));
        }
        let suffix = &bytes[overlap..];
        if suffix.is_empty() {
            return Ok(Cursor { offset: end });
        }
        let mut record = Vec::with_capacity(25 + suffix.len());
        record.push(2);
        record.extend_from_slice(&id.0);
        record.extend_from_slice(&end.to_le_bytes());
        record.extend_from_slice(suffix);
        let seq = self.commit(&record)?;
        self.replay(seq, &record)?;
        self.cursor(id)
    }

    fn read_range(&self, id: SourceId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let source = self.sources.get(&id).ok_or(Error::NotFound)?;
        let end = offset
            .checked_add(len as u64)
            .ok_or(Error::Invalid("raw range overflow"))?;
        if end > source.end {
            return Err(Error::Invalid("raw range beyond cursor"));
        }
        let mut result = Vec::with_capacity(len);
        for block in source
            .blocks
            .iter()
            .skip_while(|b| b.offset + b.len <= offset)
            .take_while(|b| b.offset < end)
        {
            let record = self.store.get(block.seq)?;
            let start = offset.saturating_sub(block.offset) as usize;
            let stop = (end - block.offset).min(block.len) as usize;
            result.extend_from_slice(&record[25 + start..25 + stop]);
        }
        Ok(result)
    }

    /// Stream exact bytes to a caller-owned sink. This function never chooses output paths.
    pub fn restore(&self, id: SourceId, out: &mut impl Write) -> Result<Cursor> {
        self.healthy()?;
        let source = self.sources.get(&id).ok_or(Error::NotFound)?;
        for block in &source.blocks {
            let bytes = self.store.get(block.seq)?;
            out.write_all(&bytes[25..])?;
        }
        Ok(Cursor { offset: source.end })
    }

    pub fn sources(&self) -> Result<impl Iterator<Item = (SourceId, Cursor)> + '_> {
        self.healthy()?;
        Ok(self
            .sources
            .iter()
            .map(|(id, source)| (*id, Cursor { offset: source.end })))
    }

    pub fn compact(&mut self) -> Result<()> {
        self.healthy()?;
        if let Err(error) = self.store.compact() {
            self.failed = true;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "oojuro-raw-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn key() -> [u8; 32] {
        [0x73; 32]
    }
    const A: SourceId = SourceId([1; 16]);
    const B: SourceId = SourceId([2; 16]);

    // flock follows the open file description: a concurrently spawning test can inherit
    // this fixture's FD until exec, retaining its lock after the parent drops Store.
    // This private-fixture assertion must wait for that ownership window before checking
    // the key. Only the exact lock refusal is retried; corruption/I/O/key errors return
    // immediately, the deadline is bounded, and runtime open remains fail-fast.
    fn open_after_spawn_window(
        dir: &Path,
        key: &[u8; 32],
        mut on_locked: impl FnMut(),
    ) -> Result<RawJournal> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match RawJournal::open(dir, key) {
                Err(Error::Invalid("store is locked by another writer"))
                    if std::time::Instant::now() < deadline =>
                {
                    on_locked();
                    std::thread::yield_now();
                }
                result => return result,
            }
        }
    }

    #[test]
    fn reopen_retries_parent_sync_after_format_committed_but_sync_failed() {
        let dir = Temp::new();
        assert!(
            RawJournal::open_with_parent_sync(&dir.0, &key(), |_| {
                Err(std::io::Error::other(
                    "synthetic transient directory sync failure",
                ))
            })
            .is_err()
        );
        assert!(dir.0.join("MANIFEST").exists());
        let mut retried = false;
        let mut j = RawJournal::open_with_parent_sync(&dir.0, &key(), |parent| {
            retried = true;
            std::fs::File::open(parent)?.sync_all()
        })
        .unwrap();
        assert!(retried);
        j.register(A, b"synthetic source").unwrap();
        j.append(A, 0, b"durable after retry").unwrap();
        drop(j);
        let j = RawJournal::open(&dir.0, &key()).unwrap();
        let mut restored = Vec::new();
        j.restore(A, &mut restored).unwrap();
        assert_eq!(restored, b"durable after retry");
    }

    #[test]
    fn arbitrary_bytes_replay_resume_compact_and_restore() {
        let dir = Temp::new();
        let expected = b"line1\r\n\n\x00\xff\xfe a partial tail\r\nfinished";
        {
            let mut j = RawJournal::open(&dir.0, &key()).unwrap();
            j.register(A, b"synthetic source descriptor").unwrap();
            j.register(B, b"empty source").unwrap();
            for chunk in expected.chunks(3) {
                let offset = j.cursor(A).unwrap().offset;
                assert_eq!(
                    j.append(A, offset, chunk).unwrap().offset,
                    offset + chunk.len() as u64
                );
            }
            // Retry spans earlier boundaries and appends only its new suffix.
            assert_eq!(
                j.append(A, 0, &expected[..12]).unwrap().offset,
                expected.len() as u64
            );
            j.compact().unwrap();
        }
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        assert_eq!(j.descriptor(A).unwrap(), b"synthetic source descriptor");
        let mut actual = Vec::new();
        j.restore(A, &mut actual).unwrap();
        assert_eq!(actual, expected);
        let mut empty = Vec::new();
        assert_eq!(j.restore(B, &mut empty).unwrap().offset, 0);
        assert_eq!(j.sources().unwrap().count(), 2);
        let start = expected.len() - 4;
        let mut retry = expected[start..].to_vec();
        retry.extend_from_slice(b" new suffix");
        assert_eq!(
            j.append(A, start as u64, &retry).unwrap().offset,
            (expected.len() + 11) as u64
        );
        let mut actual = Vec::new();
        j.restore(A, &mut actual).unwrap();
        assert_eq!(actual, [expected.as_slice(), b" new suffix"].concat());
        drop(j);
        let j = RawJournal::open(&dir.0, &key()).unwrap();
        assert_eq!(j.cursor(A).unwrap().offset, actual.len() as u64);
    }

    #[test]
    fn raw_bytes_and_source_descriptors_never_appear_on_disk() {
        let dir = Temp::new();
        let descriptor = b"PRIVATE-SYNTHETIC-SOURCE-PATH-QUOKKA";
        let payload = b"PRIVATE-SYNTHETIC-RAW-PAYLOAD-ZEBRA";
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        j.register(A, descriptor).unwrap();
        j.append(A, 0, payload).unwrap();
        j.compact().unwrap();
        drop(j);
        for entry in std::fs::read_dir(&dir.0).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(!bytes.windows(descriptor.len()).any(|w| w == descriptor));
            assert!(!bytes.windows(payload.len()).any(|w| w == payload));
        }
    }

    #[test]
    fn gaps_changed_prefix_identity_reuse_and_wrong_keys_are_refused() {
        let dir = Temp::new();
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        assert!(j.append(A, 0, b"data").is_err());
        j.register(A, b"original").unwrap();
        j.register(A, b"original").unwrap();
        assert!(j.register(A, b"different").is_err());
        j.append(A, 0, b"original bytes").unwrap();
        assert!(j.append(A, 20, b"gap").is_err());
        assert!(j.append(A, 0, b"changed").is_err());
        assert!(j.append(A, 0, &vec![1; MAX_BLOCK_BYTES + 1]).is_err());
        assert!(j.register(B, &vec![1; MAX_DESCRIPTOR_BYTES + 1]).is_err());
        let mut actual = Vec::new();
        j.restore(A, &mut actual).unwrap();
        assert_eq!(actual, b"original bytes");
        assert!(RawJournal::open(&dir.0, &key()).is_err()); // process lock
        drop(j);
        assert!(matches!(
            open_after_spawn_window(&dir.0, &[0x42; 32], || {}),
            Err(Error::WrongKey)
        ));
    }

    #[test]
    fn interrupted_append_never_advances_the_acknowledged_cursor() {
        let dir = Temp::new();
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        j.register(A, b"source").unwrap();
        j.append(A, 0, b"committed").unwrap();
        j.store.fail_next_append_after = Some(9);
        assert!(j.append(A, 9, b"interrupted bytes").is_err());
        assert!(j.cursor(A).is_err());
        assert!(j.append(A, 9, b"retry").is_err());
        drop(j);
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        assert_eq!(j.cursor(A).unwrap().offset, 9);
        j.append(A, 9, b"interrupted bytes").unwrap();
        let mut actual = Vec::new();
        j.restore(A, &mut actual).unwrap();
        assert_eq!(actual, b"committedinterrupted bytes");
    }

    #[test]
    fn complete_but_unacknowledged_append_is_found_on_reopen_and_retry() {
        let dir = Temp::new();
        {
            let mut j = RawJournal::open(&dir.0, &key()).unwrap();
            j.register(A, b"source").unwrap();
            j.append(A, 0, b"first").unwrap();
            // A process may disappear after the durable append but before updating its cursor cache.
            let mut bytes = vec![2];
            bytes.extend_from_slice(&A.0);
            bytes.extend_from_slice(&5u64.to_le_bytes());
            bytes.extend_from_slice(b" second");
            j.commit(&bytes).unwrap();
        }
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        assert_eq!(j.cursor(A).unwrap().offset, 12);
        assert_eq!(j.append(A, 5, b" second").unwrap().offset, 12);
        let mut actual = Vec::new();
        j.restore(A, &mut actual).unwrap();
        assert_eq!(actual, b"first second");
    }

    #[test]
    fn sink_failure_does_not_mutate_the_journal() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic sink failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = Temp::new();
        let mut j = RawJournal::open(&dir.0, &key()).unwrap();
        j.register(A, b"source").unwrap();
        j.append(A, 0, b"raw").unwrap();
        assert!(j.restore(A, &mut Broken).is_err());
        assert_eq!(j.cursor(A).unwrap().offset, 3);
    }
    #[test]
    #[ignore = "subprocess fixture invoked by sigkill_recovers_an_acknowledged_raw_source"]
    fn raw_process_child() {
        let Some(path) = std::env::var_os("OOJURO_SYNTHETIC_RAW_CHILD") else {
            return;
        };
        let path = PathBuf::from(path);
        let mut j = RawJournal::open(&path.join("store"), &key()).unwrap();
        j.register(A, b"synthetic source").unwrap();
        j.append(A, 0, b"acknowledged\r\n\xfftail").unwrap();
        std::fs::write(path.join("ready"), b"").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    #[test]
    fn sigkill_recovers_an_acknowledged_raw_source() {
        use std::process::{Child, Command, Stdio};
        struct Cleanup(Child);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let dir = Temp::new();
        std::fs::create_dir_all(&dir.0).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "raw::tests::raw_process_child"])
            .env("OOJURO_SYNTHETIC_RAW_CHILD", &dir.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut child = Cleanup(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !dir.0.join("ready").exists() && std::time::Instant::now() < deadline {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "synthetic child exited before acknowledgement"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            dir.0.join("ready").exists(),
            "synthetic child acknowledgement timed out"
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let j = RawJournal::open(&dir.0.join("store"), &key()).unwrap();
        let mut bytes = Vec::new();
        j.restore(A, &mut bytes).unwrap();
        assert_eq!(bytes, b"acknowledged\r\n\xfftail");
    }

    #[test]
    fn opening_refuses_unrelated_directories_and_missing_parents() {
        let dir = Temp::new();
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join("unrelated"), b"unchanged").unwrap();
        assert!(RawJournal::open(&dir.0, &key()).is_err());
        assert_eq!(
            std::fs::read(dir.0.join("unrelated")).unwrap(),
            b"unchanged"
        );
        assert!(!dir.0.join("MANIFEST").exists());
        assert!(RawJournal::open(&dir.0.join("missing").join("raw"), &key()).is_err());
        assert!(!dir.0.join("missing").exists());
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess fixture invoked by inherited_spawn_lock_still_requires_wrong_key"]
    fn inherited_spawn_lock_process_child() {
        use std::{
            io::Read,
            os::{fd::AsRawFd, unix::net::UnixStream},
        };
        if std::env::var_os("OOJURO_SYNTHETIC_RAW_LOCK_CHILD").is_none() {
            return;
        }
        struct Child(libc::pid_t);
        impl Drop for Child {
            fn drop(&mut self) {
                if self.0 > 0 {
                    unsafe {
                        libc::kill(self.0, libc::SIGKILL);
                        libc::waitpid(self.0, std::ptr::null_mut(), 0);
                    }
                }
            }
        }
        let dir = Temp::new();
        let mut journal = RawJournal::open(&dir.0, &key()).unwrap();
        journal
            .register(A, b"synthetic inherited-lock source")
            .unwrap();
        journal.append(A, 0, b"unchanged sealed bytes").unwrap();
        let (mut parent, child) = UnixStream::pair().unwrap();
        let parent_fd = parent.as_raw_fd();
        let child_fd = child.as_raw_fd();
        // This test runs in its own process, so the fork cannot retain another test's stores.
        // The child performs only async-signal-safe syscalls and deliberately never uses Store.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(parent_fd);
                let ready = [1u8];
                if libc::write(child_fd, ready.as_ptr().cast(), 1) != 1 {
                    libc::_exit(1);
                }
                let mut release = [0u8];
                if libc::read(child_fd, release.as_mut_ptr().cast(), 1) != 1 {
                    libc::_exit(1);
                }
                libc::_exit(0);
            }
        }
        let mut child_guard = Child(pid);
        drop(child);
        let mut ready = [0u8];
        parent.read_exact(&mut ready).unwrap();
        assert_eq!(ready, [1]);
        drop(journal);
        assert!(matches!(
            RawJournal::open(&dir.0, &[0x42; 32]),
            Err(Error::Invalid("store is locked by another writer"))
        ));
        let mut saw_locked = false;
        let result = open_after_spawn_window(&dir.0, &[0x42; 32], || {
            assert!(!saw_locked);
            saw_locked = true;
            parent.write_all(&[1]).unwrap();
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            child_guard.0 = 0;
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        });
        assert!(saw_locked);
        assert!(matches!(result, Err(Error::WrongKey)));
        let journal = RawJournal::open(&dir.0, &key()).unwrap();
        let mut bytes = Vec::new();
        journal.restore(A, &mut bytes).unwrap();
        assert_eq!(bytes, b"unchanged sealed bytes");
    }

    #[cfg(unix)]
    #[test]
    fn inherited_spawn_lock_still_requires_wrong_key() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "raw::tests::inherited_spawn_lock_process_child",
            ])
            .env("OOJURO_SYNTHETIC_RAW_LOCK_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated inherited-lock regression failed"
        );
    }
}
