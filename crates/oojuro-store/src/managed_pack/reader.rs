//! Reads only through a freshly checked committed catalog. No source replay or writer bootstrap.
use super::codec::{DICT_MAGIC, HEADER_BYTES, MAGIC, aad, prepare, token_path};
use super::*;
use crate::{
    Record,
    crypto::{FrameCipher, SEAL_OVERHEAD},
    format::*,
    reader::{self, DictLease, FileLease, Frames, Loc, ReadState},
};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::Read,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
/// Trusted managed-control adapter. Implementations must independently resolve
/// current authority on every call; prepared/orphan roots cannot implement it.
/// cipher() must freshly validate DATA-key lifecycle, without a warm-key fallback.
pub trait Catalog {
    fn current(&self) -> Result<Vec<u8>>;
    fn validate_birth(&self, birth: &Birth) -> Result<()>;
    fn locate(&self, physical_seq: u64) -> Result<Locator>;
    fn pack(&self, file_id: u64) -> Result<PackRef>;
    fn dictionary(&self, id: u32) -> Result<Option<DictionaryRef>>;
    fn cipher(&self, key_version: &[u8; 32]) -> Result<Arc<FrameCipher>>;
}
struct Resources {
    files: VecDeque<(PackRef, Arc<File>)>,
    dictionaries: VecDeque<(DictionaryRef, Arc<Vec<u8>>)>,
    dictionary_bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheUsage {
    pub open_files: usize,
    pub dictionaries: usize,
    pub dictionary_bytes: usize,
}
pub struct Reader<C: Catalog> {
    directory: PathBuf,
    catalog: C,
    binding: Vec<u8>,
    resources: Mutex<Resources>,
    read: ReadState,
}
impl<C: Catalog> Reader<C> {
    pub fn new(directory: &Path, catalog: C, decoded_cache_bytes: usize) -> Result<Self> {
        prepare(directory)?;
        if decoded_cache_bytes > 16 << 20 {
            return Err(Error::Invalid("managed decoded cache bound"));
        }
        let binding = catalog.current()?;
        if binding.is_empty() || binding.len() > 2048 {
            return Err(Error::Corrupt("managed current binding"));
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            catalog,
            binding,
            resources: Mutex::new(Resources {
                files: VecDeque::new(),
                dictionaries: VecDeque::new(),
                dictionary_bytes: 0,
            }),
            read: ReadState::bounded(decoded_cache_bytes),
        })
    }
    fn wipe(&self) {
        self.read.clear_sensitive();
        if let Ok(mut r) = self.resources.lock() {
            r.files.clear();
            r.dictionaries.clear();
            r.dictionary_bytes = 0;
        }
    }
    fn check(&self) -> Result<()> {
        match self.catalog.current() {
            Ok(b) if b == self.binding => Ok(()),
            Ok(_) => {
                self.wipe();
                Err(Error::Corrupt("managed authority changed"))
            }
            Err(e) => {
                self.wipe();
                Err(e)
            }
        }
    }
    fn scoped_pack(&self, file_id: u64) -> Result<PackRef> {
        self.check()?;
        let p = self.catalog.pack(file_id)?;
        p.validate()?;
        if p.file_id != file_id {
            return Err(Error::Corrupt("managed file identity"));
        }
        self.catalog.validate_birth(&p.birth)?;
        self.check()?;
        Ok(p)
    }
    fn locator(&self, seq: u64) -> Result<(Locator, PackRef)> {
        self.check()?;
        let l = self.catalog.locate(seq)?;
        l.validate()?;
        let p = self.scoped_pack(l.file_id)?;
        let end = p
            .first_physical_seq
            .checked_add(p.record_count)
            .ok_or(Error::Corrupt("managed sequence overflow"))?;
        if seq < l.first_physical_seq
            || seq >= l.first_physical_seq + u64::from(l.count)
            || l.first_physical_seq < p.first_physical_seq
            || l.first_physical_seq + u64::from(l.count) > end
            || l.frame_ordinal >= p.frame_count
            || l.offset < HEADER_BYTES
            || l.offset + 8 + u64::from(l.payload_len) > p.byte_len
            || l.dictionary_id != 0
                && p.dictionary
                    .as_ref()
                    .is_none_or(|d| d.id != l.dictionary_id)
        {
            return Err(Error::Corrupt("locator outside managed pack"));
        }
        if let Some(d) = &p.dictionary {
            if self.catalog.dictionary(d.id)?.as_ref() != Some(d) {
                return Err(Error::Corrupt("managed dictionary inventory mismatch"));
            }
            self.catalog.validate_birth(&d.birth)?;
        }
        // This fresh resolution is required even on a decoded-cache hit.
        self.catalog.cipher(&p.birth.key_version)?;
        self.check()?;
        Ok((l, p))
    }
    fn open_pack(&self, p: &PackRef) -> Result<Arc<File>> {
        self.check()?;
        self.catalog.validate_birth(&p.birth)?;
        {
            let mut r = self
                .resources
                .lock()
                .map_err(|_| Error::Invalid("managed resource lock"))?;
            if let Some(at) = r.files.iter().position(|(k, _)| k == p) {
                let e = r.files.remove(at).unwrap();
                let f = e.1.clone();
                r.files.push_back(e);
                return Ok(f);
            }
        }
        let path = token_path(&self.directory, &p.token, false);
        let file = verified_file(&path, p.byte_len, p.ciphertext_digest)?;
        let mut header = vec![0; HEADER_BYTES as usize];
        file.read_exact_at(&mut header, 0)?;
        if &header[..8] != MAGIC
            || header[8..40] != p.token
            || u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize != header.len() - 44
        {
            return Err(Error::Corrupt("managed pack header"));
        }
        let mut extra = p.file_id.to_le_bytes().to_vec();
        extra.extend(p.first_physical_seq.to_le_bytes());
        let mut expected = p.birth.encode()?;
        expected.extend(&extra);
        let key = self.catalog.cipher(&p.birth.key_version)?;
        if key.open_frame(&aad(&p.birth, &p.token, b"header", &extra)?, &header[44..])? != expected
        {
            return Err(Error::Corrupt("managed pack birth mismatch"));
        }
        // Validate complete ordinal/count coverage in bounded frame carriers.
        let mut at = HEADER_BYTES;
        let mut seq = p.first_physical_seq;
        let mut ordinal = 0;
        while at < p.byte_len {
            let mut carrier = [0; 8];
            file.read_exact_at(&mut carrier, at)?;
            let len = u32::from_le_bytes(carrier[..4].try_into().unwrap()) as u64;
            let end = at
                .checked_add(8 + len)
                .ok_or(Error::Corrupt("managed carrier overflow"))?;
            if len < SEAL_OVERHEAD as u64 || len > MAX_FRAME_BYTES as u64 + 128 || end > p.byte_len
            {
                return Err(Error::Corrupt("managed carrier bound"));
            }
            let mut payload = vec![0; len as usize];
            file.read_exact_at(&mut payload, at + 8)?;
            if frame_crc(&payload) != u32::from_le_bytes(carrier[4..].try_into().unwrap()) {
                return Err(Error::Corrupt("managed carrier CRC"));
            }
            let h = get_head(&payload)?;
            check_head(&h)?;
            if h.first_seq != seq
                || h.dict != 0 && p.dictionary.as_ref().is_none_or(|d| d.id != h.dict)
            {
                return Err(Error::Corrupt("managed pack coverage"));
            }
            seq = seq
                .checked_add(u64::from(h.count))
                .ok_or(Error::Corrupt("managed pack count overflow"))?;
            ordinal += 1;
            at = end;
        }
        if seq != p.first_physical_seq + p.record_count || ordinal != p.frame_count {
            return Err(Error::Corrupt("managed pack counts"));
        }
        self.check()?;
        let file = Arc::new(file);
        let mut r = self
            .resources
            .lock()
            .map_err(|_| Error::Invalid("managed resource lock"))?;
        r.files.push_back((p.clone(), file.clone()));
        while r.files.len() > 32 {
            r.files.pop_front();
        }
        Ok(file)
    }
    fn validate_meta(l: &Locator, m: &Meta) -> Result<()> {
        if !m.piece_list.is_empty()
            || m.records.len() != l.count as usize
            || m.records.iter().map(|r| r.valid_at).min() != Some(l.valid_lo)
            || m.records.iter().map(|r| r.valid_at).max() != Some(l.valid_hi)
        {
            return Err(Error::Corrupt("managed frame metadata/locator mismatch"));
        }
        Ok(())
    }
    fn guarded<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        let result = work().and_then(|v| {
            self.check()?;
            Ok(v)
        });
        if result.is_err() {
            self.wipe();
        }
        result
    }
    pub fn get_range(&self, seq: u64, start: usize, end: usize) -> Result<Vec<u8>> {
        self.guarded(|| {
            let (l, _) = self.locator(seq)?;
            let i = usize::try_from(l.first_physical_seq)
                .map_err(|_| Error::Invalid("managed index width"))?;
            let meta = reader::decoded(self, i, 0)?;
            Self::validate_meta(&l, &meta.meta)?;
            reader::text_range(self, i, (seq - l.first_physical_seq) as usize, start, end)
        })
    }
    pub fn get(&self, seq: u64) -> Result<Vec<u8>> {
        self.get_range(seq, 0, usize::MAX)
    }
    pub fn record(&self, seq: u64) -> Result<Record> {
        self.guarded(|| {
            let (l, _) = self.locator(seq)?;
            let i = usize::try_from(l.first_physical_seq)
                .map_err(|_| Error::Invalid("managed index width"))?;
            let d = reader::decoded(self, i, usize::MAX)?;
            Self::validate_meta(&l, &d.meta)?;
            let n = (seq - l.first_physical_seq) as usize;
            let m = d.meta.records[n];
            let (ks, kl, al) = d.meta.side[n];
            let (ks, kl, al) = (ks as usize, kl as usize, al as usize);
            Ok(Record {
                seq,
                kind: m.kind,
                valid_at: m.valid_at,
                recorded_at: m.recorded_at,
                key: d.body[ks..ks + kl].to_vec(),
                attrs: d.body[ks + kl..ks + kl + al].to_vec(),
                text: d.body[d.meta.text_start[n] as usize..d.meta.text_start[n + 1] as usize]
                    .to_vec(),
            })
        })
    }
    pub fn cache_usage(&self) -> Result<CacheUsage> {
        let r = self
            .resources
            .lock()
            .map_err(|_| Error::Invalid("managed resource lock"))?;
        Ok(CacheUsage {
            open_files: r.files.len(),
            dictionaries: r.dictionaries.len(),
            dictionary_bytes: r.dictionary_bytes,
        })
    }
}
fn check_head(h: &FrameHead) -> Result<()> {
    if !h.encrypted
        || h.digest.is_some()
        || h.count == 0
        || h.count as usize > MAX_FRAME_ROWS
        || h.raw_len as usize > MAX_FRAME_BYTES
        || h.codec != CODEC_RAW && h.codec != CODEC_ZSTD
    {
        return Err(Error::Corrupt("managed frame format/bounds"));
    }
    Ok(())
}
impl<C: Catalog> Frames for Reader<C> {
    fn loc(&self, i: usize) -> Result<Loc> {
        let (l, _) = self.locator(i as u64)?;
        Ok(Loc {
            first_seq: l.first_physical_seq,
            count: l.count,
            file: l.file_id,
            offset: l.offset,
            payload_len: l.payload_len,
            raw_len: l.raw_len,
        })
    }
    fn frame_index(&self, seq: u64) -> Result<usize> {
        let (l, _) = self.locator(seq)?;
        usize::try_from(l.first_physical_seq).map_err(|_| Error::Invalid("managed index width"))
    }
    fn file(&self, id: u64) -> Result<FileLease<'_>> {
        let p = self.scoped_pack(id)?;
        Ok(FileLease::Shared(self.open_pack(&p)?))
    }
    fn dict(&self, id: u32) -> Result<Option<DictLease<'_>>> {
        self.check()?;
        let Some(d) = self.catalog.dictionary(id)? else {
            return Ok(None);
        };
        d.validate()?;
        if d.id != id {
            return Err(Error::Corrupt("managed dictionary ID"));
        }
        self.catalog.validate_birth(&d.birth)?;
        let cipher = self.catalog.cipher(&d.birth.key_version)?;
        {
            let mut r = self
                .resources
                .lock()
                .map_err(|_| Error::Invalid("managed resource lock"))?;
            if let Some(at) = r.dictionaries.iter().position(|(k, _)| *k == d) {
                let e = r.dictionaries.remove(at).unwrap();
                let b = e.1.clone();
                r.dictionaries.push_back(e);
                return Ok(Some(DictLease::Shared(b)));
            }
        }
        let b = Arc::new(read_dictionary(&self.directory, &d, &cipher)?);
        self.check()?;
        let mut r = self
            .resources
            .lock()
            .map_err(|_| Error::Invalid("managed resource lock"))?;
        r.dictionary_bytes += b.len();
        r.dictionaries.push_back((d, b.clone()));
        while r.dictionaries.len() > 16 || r.dictionary_bytes > 16 << 20 {
            let (_, old) = r.dictionaries.pop_front().unwrap();
            r.dictionary_bytes -= old.len();
        }
        Ok(Some(DictLease::Shared(b)))
    }
    fn open_frame(&self, h: &FrameHead, head: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        check_head(h)?;
        let mut canonical = Vec::new();
        put_head(&mut canonical, h);
        if canonical != head {
            return Err(Error::Corrupt("noncanonical managed frame header"));
        }
        let (l, p) = self.locator(h.first_seq)?;
        if h.first_seq != l.first_physical_seq || h.dict != l.dictionary_id {
            return Err(Error::Corrupt("managed frame/index mismatch"));
        }
        let mut extra = l.frame_ordinal.to_le_bytes().to_vec();
        extra.extend(head);
        let key = self.catalog.cipher(&p.birth.key_version)?;
        let out = key.open_frame(&aad(&p.birth, &p.token, b"frame", &extra)?, sealed)?;
        self.check()?;
        Ok(out)
    }
    fn require_sealed(&self) -> bool {
        true
    }
    fn read_state(&self) -> &ReadState {
        &self.read
    }
}
pub(super) fn verified_file(path: &Path, len: u64, digest: [u8; 32]) -> Result<File> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() != len {
        return Err(Error::Corrupt("managed immutable file length/type"));
    }
    let mut file = File::open(path)?;
    if file.metadata()?.len() != len {
        return Err(Error::Corrupt("managed file changed"));
    }
    let mut hash = blake3::Hasher::new();
    let mut buf = [0u8; 64 << 10];
    let mut total = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        total += n as u64;
        if total > len {
            return Err(Error::Corrupt("managed file grew"));
        }
    }
    if total != len || *hash.finalize().as_bytes() != digest {
        return Err(Error::Corrupt("managed immutable digest"));
    }
    Ok(file)
}
pub(super) fn read_dictionary(
    directory: &Path,
    d: &DictionaryRef,
    cipher: &FrameCipher,
) -> Result<Vec<u8>> {
    d.validate()?;
    let file = verified_file(
        &token_path(directory, &d.token, true),
        d.byte_len,
        d.ciphertext_digest,
    )?;
    let mut bytes = vec![0; d.byte_len as usize];
    file.read_exact_at(&mut bytes, 0)?;
    let mut extra = d.id.to_le_bytes().to_vec();
    extra.extend(d.raw_bytes.to_le_bytes());
    if bytes.len() < 48 + SEAL_OVERHEAD
        || &bytes[..8] != DICT_MAGIC
        || bytes[8..40] != d.token
        || bytes[40..48] != extra
    {
        return Err(Error::Corrupt("managed dictionary header"));
    }
    let plain = cipher.open_dict(
        &aad(&d.birth, &d.token, b"dictionary", &extra)?,
        &bytes[48..],
    )?;
    if plain.len() != d.raw_bytes as usize {
        return Err(Error::Corrupt("managed dictionary size"));
    }
    Ok(plain)
}
