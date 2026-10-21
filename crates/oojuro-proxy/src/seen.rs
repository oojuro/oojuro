//! Which messages a writer's store already holds, so a history the agent
//! sends again on every call is stored once.
//!
//! Each message has a 128-bit identity (see `journal::identity`). Per writer
//! the set keeps at most `cap` identities; when it is full the least
//! recently seen tenth goes. A message counts as seen from the moment its
//! call is queued (so a parallel call does not store it twice); when the
//! journal then fails to store it, it is forgotten again, and the next call
//! that sends it stores it.
//!
//! Persistence: an append-only file of 24-byte entries (writer hash, then
//! identity, little endian) after an 8-byte header. Only identities the
//! sink confirmed are appended, so a crash can only forget entries (a
//! message is then stored once more), never claim one that is not stored.
//! The file is rewritten from memory when it holds more than twice the
//! live entries. It holds hashes, never text.
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"OJSEEN1\n";
const ENTRY: usize = 24;
/// Writers remembered at once (the least recently used one goes).
pub const MAX_WRITERS: usize = 4096;

#[derive(Default)]
struct Writer {
    ids: HashMap<u128, u64>, // identity -> last seen (generation)
    last: u64,
}

pub struct Seen {
    cap: usize,
    clock: u64,
    writers: HashMap<u64, Writer>,
    log: Option<Log>,
}

struct Log {
    path: PathBuf,
    file: BufWriter<File>,
    entries: u64,
}

impl Seen {
    /// In memory only.
    pub fn memory(cap: usize) -> Self {
        Self { cap: cap.max(16), clock: 0, writers: HashMap::new(), log: None }
    }

    /// Load (or create) the file at `path`; `None` keeps the set in memory.
    /// A file that does not start with the header is replaced.
    pub fn open(path: Option<&Path>, cap: usize) -> std::io::Result<Self> {
        let mut s = Self::memory(cap);
        let Some(path) = path else { return Ok(s) };
        let mut entries = 0u64;
        match File::open(path) {
            Ok(mut f) => {
                let mut b = Vec::new();
                f.read_to_end(&mut b)?;
                if b.len() >= MAGIC.len() && &b[..MAGIC.len()] == MAGIC {
                    for e in b[MAGIC.len()..].as_chunks::<ENTRY>().0 {
                        let w = u64::from_le_bytes(e[..8].try_into().unwrap());
                        let id = u128::from_le_bytes(e[8..].try_into().unwrap());
                        s.touch(w, id);
                        entries += 1;
                    }
                    // a torn last entry is dropped by the rewrite below
                    if !(b.len() - MAGIC.len()).is_multiple_of(ENTRY) {
                        entries = u64::MAX;
                    }
                } else {
                    entries = u64::MAX;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => entries = u64::MAX,
            Err(e) => return Err(e),
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().append(true).create(true).open(path)?;
        s.log = Some(Log { path: path.to_path_buf(), file: BufWriter::new(file), entries });
        if entries == u64::MAX || s.should_compact() {
            s.rewrite()?;
        }
        Ok(s)
    }

    /// Identities held now (all writers).
    pub fn len(&self) -> usize {
        self.writers.values().map(|w| w.ids.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, writer: u64, id: u128) -> bool {
        self.writers.get(&writer).is_some_and(|w| w.ids.contains_key(&id))
    }

    fn touch(&mut self, writer: u64, id: u128) -> bool {
        self.clock += 1;
        let g = self.clock;
        if !self.writers.contains_key(&writer) && self.writers.len() >= MAX_WRITERS {
            // forget the least recently used writer
            if let Some(old) = self.writers.iter().min_by_key(|(_, w)| w.last).map(|(k, _)| *k) {
                self.writers.remove(&old);
            }
        }
        let cap = self.cap;
        let w = self.writers.entry(writer).or_default();
        w.last = g;
        let new = w.ids.insert(id, g).is_none();
        if w.ids.len() > cap {
            // the least recently seen tenth goes
            let mut gens: Vec<u64> = w.ids.values().copied().collect();
            let k = w.ids.len() - cap + cap / 10;
            let last = gens.len() - 1;
            let (_, cut, _) = gens.select_nth_unstable(k.min(last));
            let cut = *cut;
            w.ids.retain(|_, g| *g > cut);
        }
        new
    }

    /// Mark `id` seen for `writer`; true when it was not seen before.
    pub fn check_insert(&mut self, writer: u64, id: u128) -> bool {
        self.touch(writer, id)
    }

    /// The journal did not store these: forget them so a re-send stores them.
    pub fn forget(&mut self, writer: u64, ids: &[u128]) {
        if let Some(w) = self.writers.get_mut(&writer) {
            for id in ids {
                w.ids.remove(id);
            }
        }
    }

    /// The journal stored these: make them persistent.
    pub fn ack(&mut self, writer: u64, ids: &[u128]) -> std::io::Result<()> {
        let Some(log) = self.log.as_mut() else { return Ok(()) };
        for id in ids {
            let mut e = [0u8; ENTRY];
            e[..8].copy_from_slice(&writer.to_le_bytes());
            e[8..].copy_from_slice(&id.to_le_bytes());
            log.file.write_all(&e)?;
        }
        log.file.flush()?;
        log.entries += ids.len() as u64;
        if self.should_compact() {
            self.rewrite()?;
        }
        Ok(())
    }

    fn should_compact(&self) -> bool {
        self.log.as_ref().is_some_and(|l| l.entries > (2 * self.len() as u64).max(1 << 16))
    }

    /// Rewrite the file from memory (oldest first), atomically.
    fn rewrite(&mut self) -> std::io::Result<()> {
        let Some(log) = self.log.as_mut() else { return Ok(()) };
        let mut all: Vec<(u64, u64, u128)> =
            self.writers.iter().flat_map(|(w, s)| s.ids.iter().map(move |(id, g)| (*g, *w, *id))).collect();
        all.sort_unstable();
        let tmp = log.path.with_extension("tmp");
        {
            let mut f = BufWriter::new(File::create(&tmp)?);
            f.write_all(MAGIC)?;
            for (_, w, id) in &all {
                f.write_all(&w.to_le_bytes())?;
                f.write_all(&id.to_le_bytes())?;
            }
            f.flush()?;
            f.get_ref().sync_all()?;
        }
        fs::rename(&tmp, &log.path)?;
        log.file = BufWriter::new(OpenOptions::new().append(true).open(&log.path)?);
        log.entries = all.len() as u64;
        Ok(())
    }

    /// Flush the file.
    pub fn flush(&mut self) -> std::io::Result<()> {
        match self.log.as_mut() {
            Some(l) => l.file.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("oojuro-seen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn insert_forget_ack_reload_and_bound() {
        let d = tmp("a");
        let p = d.join("seen.bin");
        let mut s = Seen::open(Some(&p), 100).unwrap();
        assert!(s.check_insert(1, 10));
        assert!(!s.check_insert(1, 10));
        assert!(s.check_insert(2, 10)); // another writer
        assert!(s.check_insert(1, 11));
        s.forget(1, &[11]); // not stored: a re-send stores it
        assert!(s.check_insert(1, 11));
        s.ack(1, &[10]).unwrap();
        s.ack(2, &[10]).unwrap();
        drop(s);
        let mut s = Seen::open(Some(&p), 100).unwrap();
        assert!(s.contains(1, 10) && s.contains(2, 10));
        assert!(!s.contains(1, 11)); // never acknowledged
        // bounded: the least recently seen go, a hot one stays
        for i in 0..1000u128 {
            s.check_insert(1, 1000 + i);
            s.check_insert(1, 10);
        }
        assert!(s.contains(1, 10));
        assert!(s.len() <= 101 + 1);
        // a torn tail is dropped
        drop(s);
        let mut b = fs::read(&p).unwrap();
        b.extend_from_slice(&[1, 2, 3]);
        fs::write(&p, &b).unwrap();
        let s = Seen::open(Some(&p), 100).unwrap();
        assert!(s.contains(1, 10));
        assert_eq!((fs::metadata(&p).unwrap().len() as usize - MAGIC.len()) % ENTRY, 0);
        fs::remove_dir_all(&d).unwrap();
    }
}
