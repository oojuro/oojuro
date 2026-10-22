//! Bounded compression-before-encryption staging. No mutable data file or ACK.
use super::*;
use crate::{
    NewRecord,
    crypto::{FrameCipher, random_key},
    format::*,
    pack::{new_cctx, pack_body},
};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
pub(super) const MAGIC: &[u8; 8] = b"OJMPACK1";
pub(super) const DICT_MAGIC: &[u8; 8] = b"OJMDICT1";
pub(super) const HEADER_BYTES: u64 = 304;
pub(super) fn token_path(dir: &Path, token: &[u8; 32], dictionary: bool) -> PathBuf {
    let hex: String = token.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!(
        "{}-{hex}.bin",
        if dictionary { "dict" } else { "data" }
    ))
}
pub(super) fn aad(birth: &Birth, token: &[u8; 32], kind: &[u8], extra: &[u8]) -> Result<Vec<u8>> {
    let mut a = b"oojuro/managed-immutable-data/v1\0".to_vec();
    a.extend((kind.len() as u32).to_le_bytes());
    a.extend(kind);
    a.extend(birth.encode()?);
    a.extend(token);
    a.extend((extra.len() as u32).to_le_bytes());
    a.extend(extra);
    Ok(a)
}
pub(super) fn prepare(dir: &Path) -> Result<()> {
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Invalid("managed pack directory"));
    }
    Ok(())
}
fn new_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?)
}
fn durable_publish(file: &File, temp: &Path, final_path: &Path, dir: &Path) -> Result<()> {
    file.sync_all()?;
    phase("PackTempDurable");
    // create-only atomic publication: an existing immutable name is never replaced.
    fs::hard_link(temp, final_path)?;
    phase("PackPublished");
    File::open(dir)?.sync_all()?;
    phase("PackDurable");
    fs::remove_file(temp)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: i32,
    pub max_pack_bytes: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            level: 3,
            max_pack_bytes: MAX_PACK_BYTES,
        }
    }
}
/// Caller supplies a trusted prepared DATA key for this exact immutable version.
/// Possession of it permits candidate staging, never public read authorization.
pub struct Stage {
    directory: PathBuf,
    temp: PathBuf,
    path: PathBuf,
    file: File,
    birth: Birth,
    token: [u8; 32],
    reservation: Reservation,
    cipher: Arc<FrameCipher>,
    options: Options,
    hash: blake3::Hasher,
    bytes: u64,
    records: u64,
    frames: u64,
    poisoned: bool,
    cctx: zstd::zstd_safe::CCtx<'static>,
    dictionary: Option<DictionaryRef>,
}
impl Stage {
    pub fn new(
        directory: &Path,
        birth: Birth,
        reservation: Reservation,
        cipher: Arc<FrameCipher>,
        options: Options,
    ) -> Result<Self> {
        Self::with_dictionary(directory, birth, reservation, cipher, options, None)
    }
    pub fn with_dictionary(
        directory: &Path,
        birth: Birth,
        reservation: Reservation,
        cipher: Arc<FrameCipher>,
        options: Options,
        dictionary: Option<DictionaryRef>,
    ) -> Result<Self> {
        prepare(directory)?;
        birth.validate()?;
        if reservation.file_id == 0
            || options.max_pack_bytes < HEADER_BYTES + 64
            || options.max_pack_bytes > MAX_PACK_BYTES
        {
            return Err(Error::Invalid("managed staging bounds"));
        }
        let dictionary_bytes = if let Some(d) = &dictionary {
            d.validate()?;
            if d.birth.identity != birth.identity
                || d.birth.stable_origin != birth.stable_origin
                || d.birth.key_version != birth.key_version
            {
                return Err(Error::Invalid("managed dictionary scope"));
            }
            Some(super::reader::read_dictionary(directory, d, &cipher)?)
        } else {
            None
        };
        let cctx = new_cctx(options.level, dictionary_bytes.as_deref())?;
        let token = random_key();
        let path = token_path(directory, &token, false);
        let temp = path.with_extension("stage");
        let file = new_file(&temp)?;
        let mut extra = Vec::new();
        extra.extend(reservation.file_id.to_le_bytes());
        extra.extend(reservation.first_physical_seq.to_le_bytes());
        let mut plain = birth.encode()?;
        plain.extend(&extra);
        let sealed = cipher.seal_frame(&aad(&birth, &token, b"header", &extra)?, &plain);
        let mut header = MAGIC.to_vec();
        header.extend(token);
        header.extend((sealed.len() as u32).to_le_bytes());
        header.extend(sealed);
        if header.len() as u64 != HEADER_BYTES {
            return Err(Error::Invalid("managed header length"));
        }
        let mut s = Self {
            directory: directory.to_path_buf(),
            temp,
            path,
            file,
            birth,
            token,
            reservation,
            cipher,
            options,
            hash: blake3::Hasher::new(),
            bytes: 0,
            records: 0,
            frames: 0,
            poisoned: false,
            cctx,
            dictionary,
        };
        s.write(&header)?;
        Ok(s)
    }
    fn write(&mut self, b: &[u8]) -> Result<()> {
        let end = self
            .bytes
            .checked_add(b.len() as u64)
            .ok_or(Error::Invalid("managed pack size overflow"))?;
        if end > self.options.max_pack_bytes {
            return Err(Error::Invalid("managed pack full"));
        }
        self.file.write_all(b)?;
        self.hash.update(b);
        self.bytes = end;
        Ok(())
    }
    /// Emits only actual written locators. Any error poisons the whole candidate.
    /// Caller must not publish callback entries unless finish and control both commit.
    pub fn push(
        &mut self,
        rows: &[NewRecord<'_>],
        emit: &mut dyn FnMut(Locator) -> Result<()>,
    ) -> Result<()> {
        if self.poisoned {
            return Err(Error::Invalid("managed stage poisoned"));
        }
        let result = self.push_inner(rows, emit);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn push_inner(
        &mut self,
        rows: &[NewRecord<'_>],
        emit: &mut dyn FnMut(Locator) -> Result<()>,
    ) -> Result<()> {
        if rows.is_empty() || rows.len() > MAX_FRAME_ROWS {
            return Err(Error::Invalid("managed frame row bound"));
        }
        let mut bound = 0usize;
        let mut previous = 0i64;
        for r in rows {
            bound = bound
                .checked_add(r.key.len())
                .and_then(|n| n.checked_add(r.attrs.len()))
                .and_then(|n| n.checked_add(r.text.len()))
                .and_then(|n| n.checked_add(48))
                .ok_or(Error::Invalid("managed frame size overflow"))?;
            if bound > MAX_FRAME_BYTES
                || r.kind >= 128
                || r.valid_at.checked_sub(previous).is_none()
                || r.recorded_at.checked_sub(r.valid_at).is_none()
            {
                return Err(Error::Invalid("managed frame input bounds"));
            }
            previous = r.valid_at;
        }
        let first = self
            .reservation
            .first_physical_seq
            .checked_add(self.records)
            .ok_or(Error::Invalid("managed sequence overflow"))?;
        first
            .checked_add(rows.len() as u64)
            .ok_or(Error::Invalid("managed sequence overflow"))?;
        let (body, meta_len) = encode_body(rows);
        let dict = self.dictionary.as_ref().map_or(0, |d| d.id);
        let packed = pack_body(
            Some(&mut self.cctx),
            dict,
            first,
            rows.len(),
            &body,
            meta_len,
            false,
        )?;
        let mut head = get_head(&packed)?;
        let data = &packed[head.body_at as usize..];
        head.encrypted = true;
        let mut payload = Vec::with_capacity(packed.len() + 40);
        put_head(&mut payload, &head);
        let mut extra = self.frames.to_le_bytes().to_vec();
        extra.extend(&payload);
        let sealed = self
            .cipher
            .seal_frame(&aad(&self.birth, &self.token, b"frame", &extra)?, data);
        payload.extend(sealed);
        let loc = Locator {
            file_id: self.reservation.file_id,
            frame_ordinal: self.frames,
            first_physical_seq: first,
            count: rows.len() as u32,
            offset: self.bytes,
            payload_len: payload.len() as u32,
            raw_len: body.len() as u32,
            dictionary_id: head.dict,
            valid_lo: rows.iter().map(|r| r.valid_at).min().unwrap(),
            valid_hi: rows.iter().map(|r| r.valid_at).max().unwrap(),
        };
        loc.validate()?;
        self.write(&seal_frame(&payload))?;
        emit(loc)?;
        self.records += rows.len() as u64;
        self.frames += 1;
        phase("FrameStaged");
        Ok(())
    }
    pub fn finish(mut self) -> Result<PackRef> {
        if self.poisoned || self.records == 0 {
            return Err(Error::Invalid("empty or poisoned managed stage"));
        }
        let r = PackRef {
            format: FORMAT,
            token: self.token,
            birth: self.birth.clone(),
            file_id: self.reservation.file_id,
            byte_len: self.bytes,
            ciphertext_digest: *self.hash.finalize().as_bytes(),
            first_physical_seq: self.reservation.first_physical_seq,
            record_count: self.records,
            frame_count: self.frames,
            dictionary: self.dictionary.take(),
        };
        r.validate()?;
        durable_publish(&self.file, &self.temp, &self.path, &self.directory)?;
        Ok(r)
    }
}
/// Stages one <=16MiB sealed dictionary. Publication is still only a candidate.
pub fn stage_dictionary(
    directory: &Path,
    birth: Birth,
    id: u32,
    bytes: &[u8],
    cipher: &FrameCipher,
) -> Result<DictionaryRef> {
    prepare(directory)?;
    birth.validate()?;
    if id == 0 || bytes.is_empty() || bytes.len() > 16 << 20 {
        return Err(Error::Invalid("managed dictionary bounds"));
    }
    let token = random_key();
    let mut extra = id.to_le_bytes().to_vec();
    extra.extend((bytes.len() as u32).to_le_bytes());
    let sealed = cipher.seal_dict(&aad(&birth, &token, b"dictionary", &extra)?, bytes);
    let mut content = DICT_MAGIC.to_vec();
    content.extend(token);
    content.extend(extra);
    content.extend(sealed);
    let path = token_path(directory, &token, true);
    let temp = path.with_extension("stage");
    let mut file = new_file(&temp)?;
    file.write_all(&content)?;
    let r = DictionaryRef {
        id,
        token,
        birth,
        byte_len: content.len() as u64,
        ciphertext_digest: *blake3::hash(&content).as_bytes(),
        raw_bytes: bytes.len() as u32,
    };
    r.validate()?;
    durable_publish(&file, &temp, &path, directory)?;
    Ok(r)
}
#[cfg(test)]
pub(super) fn phase(at: &str) {
    if std::env::var("OOJURO_MANAGED_PACK_PHASE").ok().as_deref() == Some(at) {
        println!("MANAGED_PACK_READY");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}
#[cfg(not(test))]
fn phase(_: &str) {}
