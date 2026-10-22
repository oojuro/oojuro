//! Explicit owning/recovering bounded-metadata read path over an existing strictly sealed Store.
//! Metadata is disposable; no append, allocation, new ACK or managed-control authority exists here.
mod cursor;
mod resources;
mod table;
use super::*;
use resources::{Bootstrap, ManifestSource, Resources, open_source};
use std::{os::unix::fs::OpenOptionsExt, sync::Arc};
use table::{Builder, Table};
const ROOT: &str = "PAGED_STORE";
const FRAME_WIDTH: usize = 72;

pub struct PagedStore {
    directory: PathBuf,
    resources: Resources,
    frames: Table,
    files: Table,
    dicts: Table,
    read: ReadState,
    _source_lock: File,
    _derived_lock: File,
    root: Vec<u8>,
    root_aad: Vec<u8>,
    next_seq: u64,
    raw_text: u64,
    origin: [u8; 32],
}
#[derive(Clone, Copy, Debug)]
pub struct CacheUsage {
    pub open_files: usize,
    pub dictionaries: usize,
    pub dictionary_bytes: usize,
    pub metadata_pages: usize,
}
impl PagedStore {
    /// Opens an EXISTING sealed authority. Always verifies source independently before
    /// publishing fresh encrypted derived tables; no old table/root can create source rows.
    pub fn open(source: &Path, opts: Options, derived: &Path) -> Result<Self> {
        if !opts.require_sealed {
            return Err(Error::Invalid("paged Store requires strict sealing"));
        }
        let cipher = opts.cipher.ok_or(Error::KeyRequired)?;
        check_directory(source)?;
        let marker = open_source(&source.join(ENC_MARKER), false)?;
        if marker.metadata()?.len() != 16 {
            return Err(Error::Crypto("encryption marker length"));
        }
        let mut marked = [0; 16];
        marker.read_exact_at(&mut marked, 0)?;
        if &marked[..8] != ENC_MAGIC || marked[8..] != cipher.key_id() {
            return Err(Error::Crypto("encryption marker key"));
        }
        let source_lock = open_source(&source.join("LOCK"), true)?;
        source_lock
            .try_lock()
            .map_err(|_| Error::Invalid("store is locked by another writer"))?;
        if source == derived || derived.starts_with(source) {
            return Err(Error::Invalid(
                "derived tables require a separate directory",
            ));
        }
        prepare_directory(derived)?;
        let derived_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(derived.join("LOCK"))?;
        derived_lock
            .try_lock()
            .map_err(|_| Error::Invalid("paged metadata is locked"))?;
        let manifest = ManifestSource::open(source)?;
        let resources = Resources::new(source, manifest, cipher.clone());
        let read = ReadState::bounded(opts.cache_bytes);
        let build = crate::crypto::random_key();
        let mut frames = Builder::new(derived, build, 1, FRAME_WIDTH, cipher.clone())?;
        let mut files = Builder::new(derived, build, 2, 24, cipher.clone())?;
        let mut dicts = Builder::new(derived, build, 3, 8, cipher.clone())?;
        let mut physical = blake3::Hasher::new();
        physical.update(b"oojuro/store/paged/source/v1\0");
        physical.update(&marked);
        cursor::hash(&resources.manifest.file, &mut physical)?;
        for i in 0..resources.manifest.dicts {
            let id = resources.manifest.dict_at(i)?;
            resources
                .dictionary(id)?
                .ok_or(Error::Corrupt("manifest dictionary missing"))?;
            dicts.push(&(id as u64).to_le_bytes())?;
            cursor::hash(&open_source(&dict_path(source, id), false)?, &mut physical)?;
        }
        let decoder = Bootstrap {
            resources: &resources,
            state: &read,
        };
        let mut replay = Replay {
            next: 0,
            raw_text: 0,
            origin: None,
        };
        for i in 0..resources.manifest.cold {
            let id = resources.manifest.cold_at(i)?;
            replay.cold(&decoder, id, &mut frames, &mut physical)?;
        }
        let mut hot_after = 0;
        while let Some(id) = next_hot(source, &resources.manifest, hot_after)? {
            let last = next_hot(source, &resources.manifest, id)?.is_none();
            replay.hot(&decoder, id, last, &mut frames, &mut physical)?;
            hot_after = id;
        }
        // Numeric directory passes are bounded in RAM, deliberately not claimed linear IO.
        let mut after = 0;
        while let Some((id, cold)) = next_file(source, &resources.manifest, after)? {
            let file = open_source(
                &if cold {
                    cold_path(source, id)
                } else {
                    hot_path(source, id)
                },
                false,
            )?;
            let mut row = [0; 24];
            row[..8].copy_from_slice(&id.to_le_bytes());
            row[8] = u8::from(cold);
            row[16..24].copy_from_slice(&file.metadata()?.len().to_le_bytes());
            files.push(&row)?;
            after = id;
        }
        let frames = frames.finish()?;
        let files = files.finish()?;
        let dicts = dicts.finish()?;
        let origin = replay.origin.unwrap_or([0; 32]);
        let mut root = b"OJPGROOT1".to_vec();
        root.extend(build);
        root.extend(physical.finalize().as_bytes());
        root.extend(origin);
        for n in [
            frames.rows,
            files.rows,
            dicts.rows,
            replay.next,
            replay.raw_text,
        ] {
            root.extend(n.to_le_bytes());
        }
        let mut root_aad = b"oojuro/store/paged/root/v1\0".to_vec();
        root_aad.extend(cipher.key_id());
        let sealed = cipher.seal_frame(&root_aad, &root);
        let temp = derived.join(format!("root-{}.tmp", table::hex(&build)));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&sealed)?;
        file.sync_all()?;
        #[cfg(test)]
        test_phase("RootTemp");
        std::fs::rename(temp, derived.join(ROOT))?;
        #[cfg(test)]
        test_phase("RootPublished");
        File::open(derived)?.sync_all()?;
        #[cfg(test)]
        test_phase("RootDurable");
        let store = Self {
            directory: derived.into(),
            resources,
            frames,
            files,
            dicts,
            read,
            _source_lock: source_lock,
            _derived_lock: derived_lock,
            root,
            root_aad,
            next_seq: replay.next,
            raw_text: replay.raw_text,
            origin,
        };
        store.check()?;
        store.reap(&build)?;
        Ok(store)
    }
    fn check(&self) -> Result<()> {
        let file = open_source(&self.directory.join(ROOT), false)?;
        let size = file.metadata()?.len();
        if size != self.root.len() as u64 + crate::crypto::SEAL_OVERHEAD as u64 {
            return Err(Error::Corrupt("paged root length"));
        }
        let mut sealed = vec![0; size as usize];
        file.read_exact_at(&mut sealed, 0)?;
        if self.resources.cipher.open_frame(&self.root_aad, &sealed)? != self.root {
            return Err(Error::Corrupt("paged root differs from verified source"));
        }
        Ok(())
    }
    fn reap(&self, build: &[u8; 32]) -> Result<()> {
        let stem = format!("table-{}-", table::hex(build));
        for item in std::fs::read_dir(&self.directory)? {
            let item = item?;
            let name = item.file_name();
            let name = name.to_str().ok_or(Error::Invalid("derived filename"))?;
            if name == "LOCK" || name == ROOT || name.starts_with(&stem) {
                continue;
            }
            std::fs::remove_file(item.path())?;
        }
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    pub fn len(&self) -> u64 {
        self.next_seq
    }
    pub fn is_empty(&self) -> bool {
        self.next_seq == 0
    }
    pub fn frame_count(&self) -> u64 {
        self.frames.rows
    }
    pub fn cache_usage(&self) -> Result<CacheUsage> {
        let (open_files, dictionaries, dictionary_bytes) = self.resources.cache_usage()?;
        Ok(CacheUsage {
            open_files,
            dictionaries,
            dictionary_bytes,
            metadata_pages: 3,
        })
    }
    pub fn sealed_origin_digest(&self) -> Result<[u8; 32]> {
        self.check()?;
        if self.is_empty() {
            return Err(Error::Invalid("empty sealed store"));
        }
        Ok(self.origin)
    }
    pub fn get_range(&self, seq: u64, start: usize, end: usize) -> Result<Vec<u8>> {
        self.check()?;
        let i = self.frame_index(seq)?;
        let frame = self.loc(i)?;
        reader::text_range(self, i, (seq - frame.first_seq) as usize, start, end)
    }
    pub fn get(&self, seq: u64) -> Result<Vec<u8>> {
        self.get_range(seq, 0, usize::MAX)
    }
    pub fn record(&self, seq: u64) -> Result<Record> {
        self.check()?;
        let i = self.frame_index(seq)?;
        let frame = self.loc(i)?;
        let d = reader::decoded(self, i, 0)?;
        self.materialize(&frame, &d, (seq - frame.first_seq) as usize)
    }
    fn materialize(&self, frame: &Loc, d: &reader::Decoded, r: usize) -> Result<Record> {
        let m = d.meta.records[r];
        let (ks, kl, al) = d.meta.side[r];
        let (ks, kl, al) = (ks as usize, kl as usize, al as usize);
        Ok(Record {
            seq: frame.first_seq + r as u64,
            kind: m.kind,
            valid_at: m.valid_at,
            recorded_at: m.recorded_at,
            key: d.body[ks..ks + kl].to_vec(),
            attrs: d.body[ks + kl..ks + kl + al].to_vec(),
            text: reader::text_range(
                self,
                usize::try_from(self.frames.lower(frame.first_seq)?)
                    .map_err(|_| Error::Corrupt("frame ordinal overflow"))?,
                r,
                0,
                usize::MAX,
            )?,
        })
    }
    pub fn scan_time(
        &self,
        from: i64,
        to: i64,
        mut visit: impl FnMut(&Record) -> bool,
    ) -> Result<()> {
        self.check()?;
        for n in 0..self.frames.rows {
            let raw = self.frames.get(n)?;
            let f = decode_frame(&raw)?;
            if f.max_valid < from || f.min_valid >= to {
                continue;
            }
            let i = usize::try_from(n).map_err(|_| Error::Corrupt("frame ordinal overflow"))?;
            let d = reader::decoded(self, i, usize::MAX)?;
            for r in 0..d.meta.records.len() {
                let m = d.meta.records[r];
                if m.valid_at >= from
                    && m.valid_at < to
                    && !visit(&self.materialize(&f.loc(), &d, r)?)
                {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
    pub fn raw_text_bytes(&self) -> u64 {
        self.raw_text
    }
}
impl Frames for PagedStore {
    fn loc(&self, i: usize) -> Result<Loc> {
        Ok(decode_frame(&self.frames.get(i as u64)?)?.loc())
    }
    fn frame_index(&self, seq: u64) -> Result<usize> {
        if seq >= self.next_seq {
            return Err(Error::NotFound);
        }
        let n = self.frames.lower(seq)?;
        let n = if n < self.frames.rows
            && u64::from_le_bytes(self.frames.get(n)?[..8].try_into().unwrap()) == seq
        {
            n
        } else {
            n.checked_sub(1).ok_or(Error::NotFound)?
        };
        let f = decode_frame(&self.frames.get(n)?)?;
        if seq
            >= f.first_seq
                .checked_add(f.count as u64)
                .ok_or(Error::Corrupt("frame range"))?
        {
            return Err(Error::NotFound);
        }
        usize::try_from(n).map_err(|_| Error::Corrupt("frame index overflow"))
    }
    fn file(&self, id: u64) -> Result<reader::FileLease<'_>> {
        let row = self.files.find(id)?;
        if row[8] > 1 || row[9..16].iter().any(|v| *v != 0) {
            return Err(Error::Corrupt("file table entry"));
        }
        let size = u64::from_le_bytes(row[16..24].try_into().unwrap());
        Ok(reader::FileLease::Shared(self.resources.file(
            id,
            row[8] == 1,
            size,
        )?))
    }
    fn dict(&self, id: u32) -> Result<Option<reader::DictLease<'_>>> {
        if matches!(self.dicts.find(id as u64), Err(Error::NotFound)) {
            return Ok(None);
        }
        self.dicts.find(id as u64)?;
        Ok(self
            .resources
            .dictionary(id)?
            .map(reader::DictLease::Shared))
    }
    fn read_state(&self) -> &ReadState {
        &self.read
    }
    fn cipher(&self) -> Option<&FrameCipher> {
        Some(&self.resources.cipher)
    }
    fn require_sealed(&self) -> bool {
        true
    }
}
struct Replay {
    next: u64,
    raw_text: u64,
    origin: Option<[u8; 32]>,
}
impl Replay {
    #[allow(clippy::too_many_arguments)] // Mirrors the existing Store frame-verification columns.
    fn frame(
        &mut self,
        decoder: &Bootstrap<'_>,
        id: u64,
        at: u64,
        payload: &[u8],
        expected: Option<(u32, u32, u32, i64, i64)>,
        cold: bool,
        builder: &mut Builder,
    ) -> Result<()> {
        let head = get_head(payload)?;
        if head.meta_len as usize > 1 << 20
            || head.count as usize > 131072
            || head.count as usize > head.meta_len as usize / 6
        {
            return Err(Error::Invalid(
                "paged frame metadata exceeds bounded read window",
            ));
        }
        if head.first_seq != self.next || head.count == 0 {
            return Err(Error::Corrupt("source frame sequence gap"));
        }
        let (meta, _) = reader::decode_payload(decoder, payload, &head, None)?;
        let (lo, hi) = meta
            .records
            .iter()
            .fold((i64::MAX, i64::MIN), |(lo, hi), r| {
                (lo.min(r.valid_at), hi.max(r.valid_at))
            });
        if let Some((count, raw, dict, wantlo, wanthi)) = expected
            && (count != head.count
                || raw != head.raw_len
                || dict != head.dict
                || wantlo != lo
                || wanthi != hi)
        {
            return Err(Error::Corrupt(
                "cold source index differs from verified frame",
            ));
        }
        let text = meta
            .records
            .iter()
            .try_fold(0u64, |n, r| n.checked_add(r.text_len as u64))
            .ok_or(Error::Corrupt("source text overflow"))?;
        let f = FrameRef {
            first_seq: self.next,
            count: head.count,
            file: id,
            offset: at,
            payload_len: payload.len() as u32,
            raw_len: head.raw_len,
            min_valid: lo,
            max_valid: hi,
            cold,
            dict: head.dict,
        };
        builder.push(&encode_frame(&f))?;
        self.next = self
            .next
            .checked_add(head.count as u64)
            .ok_or(Error::Corrupt("source count overflow"))?;
        self.raw_text = self
            .raw_text
            .checked_add(text)
            .ok_or(Error::Corrupt("source text overflow"))?;
        if self.origin.is_none() {
            let mut h = blake3::Hasher::new();
            h.update(b"oojuro/sealed-origin/v1\0");
            h.update(&(payload.len() as u32).to_le_bytes());
            h.update(&frame_crc(payload).to_le_bytes());
            h.update(payload);
            self.origin = Some(*h.finalize().as_bytes());
        }
        Ok(())
    }
    fn cold(
        &mut self,
        decoder: &Bootstrap<'_>,
        id: u64,
        builder: &mut Builder,
        physical: &mut blake3::Hasher,
    ) -> Result<()> {
        let file = open_source(&cold_path(&decoder.resources.directory, id), false)?;
        let size = file.metadata()?.len();
        if size < (FILE_HEAD + 16) as u64 {
            return Err(Error::Corrupt("cold source truncated"));
        }
        let mut head = [0; FILE_HEAD];
        file.read_exact_at(&mut head, 0)?;
        if check_head(&head, COLD_MAGIC, id)? != self.next {
            return Err(Error::Corrupt("cold source sequence"));
        }
        let mut tail = [0; 16];
        file.read_exact_at(&mut tail, size - 16)?;
        if &tail[8..] != COLD_END {
            return Err(Error::Corrupt("cold source trailer"));
        }
        let len = u32::from_le_bytes(tail[..4].try_into().unwrap()) as u64;
        let start = size
            .checked_sub(16 + len)
            .filter(|n| *n >= FILE_HEAD as u64)
            .ok_or(Error::Corrupt("cold index length"))?;
        if cursor::crc(&file, start, size - 16)?
            != u32::from_le_bytes(tail[4..8].try_into().unwrap())
        {
            return Err(Error::Corrupt("cold index checksum"));
        }
        let mut index = cursor::Cursor::new(&file, start, size - 16)?;
        let n = index.varint()?;
        let dict =
            u32::try_from(index.varint()?).map_err(|_| Error::Corrupt("dictionary id overflow"))?;
        let mut at = FILE_HEAD as u64;
        let mut prev = 0i64;
        let initial_text = self.raw_text;
        let mut input = bounded::HotReader::new(&file, start);
        for _ in 0..n {
            let bytes = u32::try_from(index.varint()?)
                .map_err(|_| Error::Corrupt("cold frame size overflow"))?;
            let count = u32::try_from(index.varint()?)
                .map_err(|_| Error::Corrupt("cold frame count overflow"))?;
            let raw = u32::try_from(index.varint()?)
                .map_err(|_| Error::Corrupt("cold raw size overflow"))?;
            let lo = prev.wrapping_add(unzigzag(index.varint()?));
            let hi = lo.wrapping_add(unzigzag(index.varint()?));
            prev = lo;
            let payload = input.payload(at)?.ok_or(Error::Corrupt("cold frame CRC"))?;
            if payload.len() != bytes as usize {
                return Err(Error::Corrupt("cold frame indexed length"));
            }
            self.frame(
                decoder,
                id,
                at,
                &payload,
                Some((count, raw, dict, lo, hi)),
                true,
                builder,
            )?;
            at = at
                .checked_add(8 + bytes as u64)
                .ok_or(Error::Corrupt("cold frame offset overflow"))?;
        }
        if at != start || index.varint()? != self.raw_text - initial_text {
            return Err(Error::Corrupt("cold frame coverage or text sum"));
        }
        index.finish()?;
        cursor::hash(&file, physical)
    }
    fn hot(
        &mut self,
        decoder: &Bootstrap<'_>,
        id: u64,
        last: bool,
        builder: &mut Builder,
        physical: &mut blake3::Hasher,
    ) -> Result<()> {
        let file = open_source(&hot_path(&decoder.resources.directory, id), last)?;
        let size = file.metadata()?.len();
        let mut header = [0; FILE_HEAD];
        let head_len = size.min(FILE_HEAD as u64) as usize;
        file.read_exact_at(&mut header[..head_len], 0)?;
        if check_head(&header[..head_len], HOT_MAGIC, id)? != self.next {
            return Err(Error::Corrupt("hot source sequence"));
        }
        let mut at = FILE_HEAD as u64;
        let mut input = bounded::HotReader::new(&file, size);
        while at < size {
            if let Some(payload) = input.payload(at)? {
                self.frame(decoder, id, at, &payload, None, false, builder)?;
                at += 8 + payload.len() as u64;
            } else {
                if !last || bounded::any_after(&file, size, at + 1)? {
                    return Err(Error::Corrupt("hot source corruption before tail"));
                }
                file.set_len(at)?;
                file.sync_all()?;
                break;
            }
        }
        cursor::hash(&file, physical)
    }
}
fn encode_frame(f: &FrameRef) -> [u8; FRAME_WIDTH] {
    let mut row = [0; FRAME_WIDTH];
    row[..8].copy_from_slice(&f.first_seq.to_le_bytes());
    row[8..12].copy_from_slice(&f.count.to_le_bytes());
    row[12..20].copy_from_slice(&f.file.to_le_bytes());
    row[20..28].copy_from_slice(&f.offset.to_le_bytes());
    row[28..32].copy_from_slice(&f.payload_len.to_le_bytes());
    row[32..36].copy_from_slice(&f.raw_len.to_le_bytes());
    row[36..44].copy_from_slice(&f.min_valid.to_le_bytes());
    row[44..52].copy_from_slice(&f.max_valid.to_le_bytes());
    row[52] = u8::from(f.cold);
    row[53..57].copy_from_slice(&f.dict.to_le_bytes());
    row
}
fn decode_frame(row: &[u8]) -> Result<FrameRef> {
    if row.len() != FRAME_WIDTH || row[52] > 1 || row[57..].iter().any(|b| *b != 0) {
        return Err(Error::Corrupt("frame table shape"));
    }
    let f = FrameRef {
        first_seq: u64::from_le_bytes(row[..8].try_into().unwrap()),
        count: u32::from_le_bytes(row[8..12].try_into().unwrap()),
        file: u64::from_le_bytes(row[12..20].try_into().unwrap()),
        offset: u64::from_le_bytes(row[20..28].try_into().unwrap()),
        payload_len: u32::from_le_bytes(row[28..32].try_into().unwrap()),
        raw_len: u32::from_le_bytes(row[32..36].try_into().unwrap()),
        min_valid: i64::from_le_bytes(row[36..44].try_into().unwrap()),
        max_valid: i64::from_le_bytes(row[44..52].try_into().unwrap()),
        cold: row[52] == 1,
        dict: u32::from_le_bytes(row[53..57].try_into().unwrap()),
    };
    if f.count == 0
        || f.payload_len as usize > MAX_FRAME + 64
        || f.raw_len as usize > MAX_FRAME
        || f.min_valid > f.max_valid
    {
        return Err(Error::Corrupt("frame table bounds"));
    }
    Ok(f)
}
fn parse_id(name: &str, prefix: &str, suffix: &str) -> Option<u64> {
    name.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}
fn next_hot(source: &Path, manifest: &ManifestSource, after: u64) -> Result<Option<u64>> {
    let mut next = None;
    for item in std::fs::read_dir(source)? {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(id) = parse_id(name, "hot-", ".ojl")
            && id >= manifest.hot_from
            && id > after
        {
            if id >= manifest.next_file {
                return Err(Error::Corrupt("hot file beyond manifest"));
            }
            next = Some(next.map_or(id, |n: u64| n.min(id)));
        }
    }
    Ok(next)
}
fn next_file(source: &Path, manifest: &ManifestSource, after: u64) -> Result<Option<(u64, bool)>> {
    let mut next = None;
    for item in std::fs::read_dir(source)? {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        let candidate = if let Some(id) = parse_id(name, "cold-", ".ojc") {
            manifest.has_cold(id)?.then_some((id, true))
        } else if let Some(id) = parse_id(name, "hot-", ".ojl") {
            (id >= manifest.hot_from && id < manifest.next_file).then_some((id, false))
        } else {
            None
        };
        if let Some((id, cold)) = candidate
            && id > after
        {
            match next {
                Some((n, c)) if n == id && c != cold => {
                    return Err(Error::Corrupt("duplicate source file id"));
                }
                Some((n, _)) if n < id => {}
                _ => next = Some((id, cold)),
            }
        }
    }
    Ok(next)
}
fn check_directory(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Invalid("source directory is not regular"));
    }
    Ok(())
}
fn known(name: &str) -> bool {
    if name == ROOT || name == "LOCK" {
        return true;
    }
    if let Some(s) = name
        .strip_prefix("root-")
        .and_then(|n| n.strip_suffix(".tmp"))
    {
        return s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    }
    let name = name.strip_suffix(".tmp").unwrap_or(name);
    let Some(s) = name
        .strip_prefix("table-")
        .and_then(|n| n.strip_suffix(".bin"))
    else {
        return false;
    };
    let Some((g, kind)) = s.split_once('-') else {
        return false;
    };
    g.len() == 64 && g.bytes().all(|b| b.is_ascii_hexdigit()) && matches!(kind, "1" | "2" | "3")
}
fn prepare_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    check_directory(path)?;
    for item in std::fs::read_dir(path)? {
        let item = item?;
        let m = item.file_type()?;
        if !m.is_file()
            || m.is_symlink()
            || !known(
                item.file_name()
                    .to_str()
                    .ok_or(Error::Invalid("derived filename"))?,
            )
        {
            return Err(Error::Invalid("unknown entry in paged directory"));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests;

#[cfg(test)]
fn test_phase(phase: &str) {
    if std::env::var("OOJURO_PAGED_STORE_CRASH_PHASE").as_deref() == Ok(phase) {
        println!("PAGED_STORE_BOUNDARY_READY");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}
