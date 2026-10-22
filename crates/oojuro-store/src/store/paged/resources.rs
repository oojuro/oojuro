//! Independently checked source metadata and bounded resource leases.
use super::*;
use cursor::Cursor;
use std::{collections::VecDeque, os::unix::fs::MetadataExt, sync::Mutex};
const MAX_DICT: usize = 16 << 20;
pub(super) fn open_source(path: &Path, write: bool) -> Result<File> {
    let before = std::fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(Error::Invalid("source file is not regular"));
    }
    let file = OpenOptions::new().read(true).write(write).open(path)?;
    let actual = file.metadata()?;
    let after = std::fs::symlink_metadata(path)?;
    if after.file_type().is_symlink()
        || actual.dev() != before.dev()
        || actual.ino() != before.ino()
        || actual.dev() != after.dev()
        || actual.ino() != after.ino()
    {
        return Err(Error::Invalid("source file changed during open"));
    }
    Ok(file)
}
pub(super) struct ManifestSource {
    pub file: File,
    pub next_file: u64,
    pub hot_from: u64,
    pub cold: u32,
    pub dicts: u32,
    dict_at: u64,
}
impl ManifestSource {
    pub fn open(directory: &Path) -> Result<Self> {
        let file = open_source(&directory.join("MANIFEST"), false)?;
        let size = file.metadata()?.len();
        if size < 36 {
            return Err(Error::Corrupt("manifest truncated"));
        }
        let mut trailer = [0; 4];
        file.read_exact_at(&mut trailer, size - 4)?;
        if cursor::crc(&file, 0, size - 4)? != u32::from_le_bytes(trailer) {
            return Err(Error::Corrupt("manifest checksum"));
        }
        let mut c = Cursor::new(&file, 0, size - 4)?;
        if c.fixed::<8>()? != *MAN_MAGIC {
            return Err(Error::Corrupt("manifest magic"));
        }
        let next_file = c.u64()?;
        let hot_from = c.u64()?;
        let cold = c.u32()?;
        let mut prev = 0;
        for _ in 0..cold {
            let id = c.u64()?;
            if id <= prev || id >= next_file {
                return Err(Error::Corrupt("manifest cold order"));
            }
            prev = id;
        }
        let dicts = c.u32()?;
        let dict_at = c.pos();
        let mut prev = 0;
        for _ in 0..dicts {
            let id = c.u32()?;
            if id == 0 || id <= prev {
                return Err(Error::Corrupt("manifest dictionary order"));
            }
            prev = id;
        }
        c.finish()?;
        if next_file == 0 || hot_from == 0 || hot_from > next_file {
            return Err(Error::Corrupt("manifest sequence"));
        }
        Ok(Self {
            file,
            next_file,
            hot_from,
            cold,
            dicts,
            dict_at,
        })
    }
    pub fn cold_at(&self, index: u32) -> Result<u64> {
        if index >= self.cold {
            return Err(Error::NotFound);
        }
        let mut bytes = [0; 8];
        self.file.read_exact_at(&mut bytes, 28 + index as u64 * 8)?;
        Ok(u64::from_le_bytes(bytes))
    }
    pub fn dict_at(&self, index: u32) -> Result<u32> {
        if index >= self.dicts {
            return Err(Error::NotFound);
        }
        let mut bytes = [0; 4];
        self.file
            .read_exact_at(&mut bytes, self.dict_at + index as u64 * 4)?;
        Ok(u32::from_le_bytes(bytes))
    }
    pub fn has_dict(&self, id: u32) -> Result<bool> {
        let (mut lo, mut hi) = (0, self.dicts);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.dict_at(mid)? < id {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        Ok(lo < self.dicts && self.dict_at(lo)? == id)
    }
    pub fn has_cold(&self, id: u64) -> Result<bool> {
        let (mut lo, mut hi) = (0, self.cold);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.cold_at(mid)? < id {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        Ok(lo < self.cold && self.cold_at(lo)? == id)
    }
}
struct Cache<T> {
    entries: VecDeque<(u64, Arc<T>, usize)>,
    bytes: usize,
    count: usize,
    budget: usize,
}
impl<T> Cache<T> {
    fn new(count: usize, budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            count,
            budget,
        }
    }
    fn get(&mut self, id: u64) -> Option<Arc<T>> {
        let at = self.entries.iter().position(|(key, _, _)| *key == id)?;
        let row = self.entries.remove(at)?;
        let value = row.1.clone();
        self.entries.push_front(row);
        Some(value)
    }
    fn put(&mut self, id: u64, value: Arc<T>, bytes: usize) {
        if bytes > self.budget {
            return;
        }
        while self.entries.len() >= self.count || self.bytes.saturating_add(bytes) > self.budget {
            let Some((_, _, old)) = self.entries.pop_back() else {
                break;
            };
            self.bytes -= old;
        }
        self.entries.push_front((id, value, bytes));
        self.bytes += bytes;
    }
}
pub(super) struct Resources {
    pub directory: PathBuf,
    pub manifest: ManifestSource,
    pub cipher: Arc<FrameCipher>,
    files: Mutex<Cache<File>>,
    dicts: Mutex<Cache<Vec<u8>>>,
}
impl Resources {
    pub fn new(directory: &Path, manifest: ManifestSource, cipher: Arc<FrameCipher>) -> Self {
        Self {
            directory: directory.into(),
            manifest,
            cipher,
            files: Mutex::new(Cache::new(32, 0)),
            dicts: Mutex::new(Cache::new(16, MAX_DICT)),
        }
    }
    pub fn dictionary(&self, id: u32) -> Result<Option<Arc<Vec<u8>>>> {
        if !self.manifest.has_dict(id)? {
            return Ok(None);
        }
        if let Some(hit) = self
            .dicts
            .lock()
            .map_err(|_| Error::Invalid("dictionary cache poisoned"))?
            .get(id as u64)
        {
            return Ok(Some(hit));
        }
        let file = open_source(&dict_path(&self.directory, id), false)?;
        let size = file.metadata()?.len();
        if !(20..=(MAX_DICT + 20 + 8 + crate::crypto::SEAL_OVERHEAD) as u64).contains(&size) {
            return Err(Error::Corrupt("dictionary file bound"));
        }
        let mut bytes = vec![0; size as usize];
        file.read_exact_at(&mut bytes, 0)?;
        if bytes[..8] != *DICT_MAGIC_SEALED
            || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != id
        {
            return Err(Error::Corrupt("sealed dictionary header"));
        }
        let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        if len > MAX_DICT
            || crc(&bytes[20..]) != u32::from_le_bytes(bytes[16..20].try_into().unwrap())
        {
            return Err(Error::Corrupt("dictionary size or CRC"));
        }
        let plain = self.cipher.open_dict(&bytes[8..16], &bytes[20..])?;
        if plain.len() != 8 + len {
            return Err(Error::Corrupt("dictionary decoded length"));
        }
        let dict = Arc::new(plain[8..].to_vec());
        self.dicts
            .lock()
            .map_err(|_| Error::Invalid("dictionary cache poisoned"))?
            .put(id as u64, dict.clone(), dict.len());
        Ok(Some(dict))
    }
    pub fn file(&self, id: u64, cold: bool, size: u64) -> Result<Arc<File>> {
        if let Some(file) = self
            .files
            .lock()
            .map_err(|_| Error::Invalid("file cache poisoned"))?
            .get(id)
        {
            if file.metadata()?.len() != size {
                return Err(Error::Corrupt("source file length changed"));
            }
            return Ok(file);
        }
        let file = Arc::new(open_source(
            &if cold {
                cold_path(&self.directory, id)
            } else {
                hot_path(&self.directory, id)
            },
            false,
        )?);
        if file.metadata()?.len() != size {
            return Err(Error::Corrupt("source file length changed"));
        }
        self.files
            .lock()
            .map_err(|_| Error::Invalid("file cache poisoned"))?
            .put(id, file.clone(), 0);
        Ok(file)
    }
    pub fn cache_usage(&self) -> Result<(usize, usize, usize)> {
        let f = self
            .files
            .lock()
            .map_err(|_| Error::Invalid("file cache poisoned"))?;
        let d = self
            .dicts
            .lock()
            .map_err(|_| Error::Invalid("dictionary cache poisoned"))?;
        Ok((f.entries.len(), d.entries.len(), d.bytes))
    }
}
pub(super) struct Bootstrap<'a> {
    pub resources: &'a Resources,
    pub state: &'a ReadState,
}
impl Frames for Bootstrap<'_> {
    fn loc(&self, _: usize) -> Result<Loc> {
        Err(Error::Invalid("bootstrap has no public frame table"))
    }
    fn frame_index(&self, _: u64) -> Result<usize> {
        Err(Error::Invalid("bootstrap has no public frame table"))
    }
    fn file(&self, _: u64) -> Result<reader::FileLease<'_>> {
        Err(Error::Invalid(
            "bootstrap decoder does not resolve frame files",
        ))
    }
    fn dict(&self, id: u32) -> Result<Option<reader::DictLease<'_>>> {
        Ok(self
            .resources
            .dictionary(id)?
            .map(reader::DictLease::Shared))
    }
    fn read_state(&self) -> &ReadState {
        self.state
    }
    fn cipher(&self) -> Option<&FrameCipher> {
        Some(&self.resources.cipher)
    }
    fn require_sealed(&self) -> bool {
        true
    }
}
