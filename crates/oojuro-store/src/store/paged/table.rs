//! Encrypted fixed-width derived metadata pages. Source replay remains authoritative.
use super::*;
use std::{os::unix::fs::OpenOptionsExt, sync::Mutex};
pub(super) const PAGE: usize = 64 << 10;
const MAX_WIDTH: usize = 128;
fn name(build: &[u8; 32], kind: u8) -> String {
    format!("table-{}-{kind}.bin", hex(build))
}
fn aad(build: &[u8; 32], kind: u8, width: usize, page: u64) -> Vec<u8> {
    let mut out = b"oojuro/store/paged/table/v1\0".to_vec();
    out.extend(build);
    out.push(kind);
    out.extend((width as u32).to_le_bytes());
    out.extend(page.to_le_bytes());
    out
}
pub(super) struct Builder {
    directory: PathBuf,
    build: [u8; 32],
    kind: u8,
    width: usize,
    file: File,
    buffer: Vec<u8>,
    pending: usize,
    rows: u64,
    pages: u64,
    cipher: Arc<FrameCipher>,
}
impl Builder {
    pub fn new(
        directory: &Path,
        build: [u8; 32],
        kind: u8,
        width: usize,
        cipher: Arc<FrameCipher>,
    ) -> Result<Self> {
        if width == 0 || width > MAX_WIDTH || kind == 0 || kind > 3 {
            return Err(Error::Invalid("table shape"));
        }
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(directory.join(format!("{}.tmp", name(&build, kind))))?;
        Ok(Self {
            directory: directory.into(),
            build,
            kind,
            width,
            file,
            buffer: vec![0; PAGE],
            pending: 0,
            rows: 0,
            pages: 0,
            cipher,
        })
    }
    fn capacity(&self) -> usize {
        (PAGE - 8) / self.width
    }
    pub fn push(&mut self, row: &[u8]) -> Result<()> {
        if row.len() != self.width {
            return Err(Error::Invalid("table row width"));
        }
        if self.pending == self.capacity() {
            self.flush()?;
        }
        let at = 8 + self.pending * self.width;
        self.buffer[at..at + self.width].copy_from_slice(row);
        self.pending += 1;
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or(Error::Corrupt("table row overflow"))?;
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.pending == 0 {
            return Ok(());
        }
        self.buffer[..8].copy_from_slice(&(self.pending as u64).to_le_bytes());
        let sealed = self.cipher.seal_frame(
            &aad(&self.build, self.kind, self.width, self.pages),
            &self.buffer,
        );
        self.file.write_all(&sealed)?;
        self.pages += 1;
        self.pending = 0;
        self.buffer.fill(0);
        Ok(())
    }
    pub fn finish(mut self) -> Result<Table> {
        self.flush()?;
        self.file.sync_all()?;
        let final_path = self.directory.join(name(&self.build, self.kind));
        std::fs::rename(
            self.directory
                .join(format!("{}.tmp", name(&self.build, self.kind))),
            &final_path,
        )?;
        File::open(&self.directory)?.sync_all()?;
        Ok(Table {
            file: Arc::new(self.file),
            cipher: self.cipher,
            build: self.build,
            kind: self.kind,
            width: self.width,
            rows: self.rows,
            cached: Mutex::new(None),
        })
    }
}
pub(super) struct Table {
    file: Arc<File>,
    cipher: Arc<FrameCipher>,
    build: [u8; 32],
    kind: u8,
    width: usize,
    pub rows: u64,
    cached: Mutex<Option<(u64, Arc<Vec<u8>>)>>,
}
impl Table {
    pub fn get(&self, index: u64) -> Result<Vec<u8>> {
        if index >= self.rows {
            return Err(Error::NotFound);
        }
        let capacity = ((PAGE - 8) / self.width) as u64;
        let page = index / capacity;
        let slot = (index % capacity) as usize;
        let hit = self
            .cached
            .lock()
            .map_err(|_| Error::Invalid("table cache poisoned"))?
            .as_ref()
            .filter(|(p, _)| *p == page)
            .map(|(_, v)| v.clone());
        let bytes = if let Some(v) = hit {
            v
        } else {
            let mut sealed = vec![0; PAGE + crate::crypto::SEAL_OVERHEAD];
            let offset = page
                .checked_mul(sealed.len() as u64)
                .ok_or(Error::Corrupt("table offset"))?;
            self.file.read_exact_at(&mut sealed, offset)?;
            let plain = self
                .cipher
                .open_frame(&aad(&self.build, self.kind, self.width, page), &sealed)?;
            let expected = (self.rows - page * capacity).min(capacity);
            if plain.len() != PAGE
                || u64::from_le_bytes(plain[..8].try_into().unwrap()) != expected
                || plain[8 + expected as usize * self.width..]
                    .iter()
                    .any(|b| *b != 0)
            {
                return Err(Error::Corrupt("table page bounds"));
            }
            let plain = Arc::new(plain);
            *self
                .cached
                .lock()
                .map_err(|_| Error::Invalid("table cache poisoned"))? = Some((page, plain.clone()));
            plain
        };
        let at = 8 + slot * self.width;
        Ok(bytes[at..at + self.width].to_vec())
    }
    pub fn lower(&self, key: u64) -> Result<u64> {
        let (mut lo, mut hi) = (0, self.rows);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let row = self.get(mid)?;
            if u64::from_le_bytes(row[..8].try_into().unwrap()) < key {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        Ok(lo)
    }
    pub fn find(&self, key: u64) -> Result<Vec<u8>> {
        let i = self.lower(key)?;
        let row = self.get(i)?;
        if u64::from_le_bytes(row[..8].try_into().unwrap()) != key {
            return Err(Error::NotFound);
        }
        Ok(row)
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    const H: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [H[(b >> 4) as usize] as char, H[(b & 15) as usize] as char])
        .collect()
}
