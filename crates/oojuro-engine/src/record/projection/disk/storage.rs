mod metadata;
use super::*;
use oojuro_store::crypto::{SEAL_OVERHEAD, random_key};
use serde::de::DeserializeOwned;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
};
pub(super) const COLUMN_ROWS: usize = 1024;
pub(super) const COLUMN_BYTES: usize = 100;
const PACK_BYTES: u64 = 256 << 20;
const MAX_META: usize = (32 << 10) - 5;
const MAX_DESCRIPTOR: usize = 8 << 20;
const FORMAT: u32 = 1;
pub(super) const COMPACT_FORMAT: u32 = 2;
pub(super) const POSITIONAL_FORMAT: u32 = 3;
pub(super) fn compact_format(format: u32) -> bool {
    matches!(format, COMPACT_FORMAT | POSITIONAL_FORMAT)
}
pub(super) const TEXT: u8 = 1;
pub(super) const COLUMNS: u8 = 2;
const MANIFEST: &str = "PROJECTION_CURRENT";
#[cfg(test)]
thread_local! { pub(super) static FAIL_AFTER_MANIFEST_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[derive(Clone)]
pub(super) struct Authority {
    pub identity: Vec<u8>,
    pub binding: Vec<u8>,
    pub budget: Option<Arc<budget::Budget>>,
}
impl Authority {
    #[cfg(test)]
    pub fn new(binding: &Binding) -> Result<Self> {
        Self::for_layout(binding, FORMAT)
    }
    pub fn for_layout(binding: &Binding, format: u32) -> Result<Self> {
        if !matches!(format, FORMAT | COMPACT_FORMAT | POSITIONAL_FORMAT) {
            return Err(Error::Bounds);
        }
        if !name(&binding.writer, 256) {
            return Err(Error::Bounds);
        }
        let mut identity = match format {
            FORMAT => b"OJPROJ01".to_vec(),
            COMPACT_FORMAT => b"OJPROJ02".to_vec(),
            POSITIONAL_FORMAT => b"OJPROJ03".to_vec(),
            _ => return Err(Error::Bounds),
        };
        identity.extend(binding.namespace);
        identity.extend((binding.writer.len() as u16).to_le_bytes());
        identity.extend(binding.writer.as_bytes());
        let mut full = identity.clone();
        full.extend(binding.view);
        Ok(Self {
            identity,
            binding: full,
            budget: None,
        })
    }
    pub fn reserve(&self, path: &Path, length: u64) -> Result<()> {
        self.budget
            .as_ref()
            .map_or(Ok(()), |budget| budget.reserve(path, length))
    }
}
impl tree::Authority for Authority {
    fn sync_file(&self, file: &File, purpose: tree::Purpose) -> Result<()> {
        // Small intermediate sort files are never named by a published root.
        // Keep explicit writeback for larger runs to bound dirty working data.
        if matches!(
            purpose,
            tree::Purpose::Sort | tree::Purpose::ProjectionTerms
        ) && file.metadata().map_err(|_| Error::Unavailable)?.len() <= 1 << 20
        {
            return Ok(());
        }
        file.sync_all().map_err(|_| Error::Unavailable)
    }
    fn sync_directory(&self, _directory: &Path) -> Result<()> {
        // finish_directory syncs the complete generation once, after every
        // persistent pack/blob is synced and all temporary files are removed.
        Ok(())
    }
    fn reserve_file(&self, path: &Path, length: u64) -> Result<()> {
        self.reserve(path, length)
    }
    fn rename_file(&self, source: &Path, target: &Path) -> Result<()> {
        if let Some(budget) = &self.budget {
            budget.rename(source, target)
        } else {
            fs::rename(source, target).map_err(|_| Error::Unavailable)
        }
    }
    fn remove_file(&self, path: &Path) -> Result<()> {
        if let Some(budget) = &self.budget {
            budget.remove(path)
        } else {
            fs::remove_file(path).map_err(|_| Error::Unavailable)
        }
    }
    fn current(&self) -> Result<Vec<u8>> {
        Ok(self.binding.clone())
    }
    fn matches_current(&self, expected: &[u8]) -> Result<bool> {
        Ok(expected == self.binding)
    }
    fn validate_birth(
        &self,
        birth: &[u8],
        purpose: tree::Purpose,
        class: tree::KeyClass,
    ) -> Result<()> {
        if class != tree::KeyClass::Data
            || !matches!(
                purpose,
                tree::Purpose::ProjectionMetadata
                    | tree::Purpose::ProjectionIds
                    | tree::Purpose::ProjectionKeys
                    | tree::Purpose::ProjectionPostings
                    | tree::Purpose::ProjectionTerms
            )
            || birth.len() != self.identity.len() + 32
            || !birth.starts_with(&self.identity)
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
pub(super) fn context<'a>(
    directory: &'a Path,
    cipher: &'a FrameCipher,
    authority: &'a Authority,
    purpose: tree::Purpose,
    cache: Option<&'a tree::PageCache>,
) -> tree::Context<'a> {
    tree::Context {
        directory,
        cipher,
        authority,
        purpose,
        identity: *blake3::hash(&authority.identity).as_bytes(),
        class: tree::KeyClass::Data,
        sort_of: None,
        cache,
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Blob {
    pub file: u32,
    pub offset: u64,
    pub stored: u32,
    pub bytes: u32,
    pub kind: u8,
    pub codec: u8,
    pub digest: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub format: u32,
    pub generation: String,
    pub binding: Vec<u8>,
    pub rows: u64,
    pub live_rows: u64,
    pub source_bytes: u64,
    pub limits: Limits,
    pub build_metrics: BuildMetrics,
    pub files: Vec<String>,
    pub columns: Vec<Blob>,
    pub metadata: tree::Root,
    pub ids: tree::Root,
    pub keys: tree::Root,
    pub postings: tree::Root,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positions: Option<tree::Root>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_codec: Option<u8>,
}
#[derive(Clone, Copy)]
pub(super) struct Column {
    pub valid_at: i64,
    pub length: u32,
    pub scope: u32,
    pub kind: u8,
    pub status: u8,
    pub flags: u8,
    pub tool: [u8; 32],
    pub agent: [u8; 32],
    pub label: u128,
}
pub(super) struct Selection {
    from: i64,
    to: i64,
    kind: Option<u8>,
    status: Option<u8>,
    tool: Option<[u8; 32]>,
    agent: Option<[u8; 32]>,
}
pub(super) struct Selected {
    pub bits: Vec<u64>,
    pub count: usize,
    pub totals: Totals,
}
#[derive(Clone, Copy, Default)]
pub(super) struct Totals {
    pub length: u64,
    pub scope: u64,
}
impl Totals {
    pub fn add(&mut self, column: &Column) -> Result<()> {
        if column.live() && column.indexed() {
            self.length = self
                .length
                .checked_add(u64::from(column.length))
                .ok_or(Error::Bounds)?;
            self.scope = self
                .scope
                .checked_add(u64::from(column.scope))
                .ok_or(Error::Bounds)?;
        }
        Ok(())
    }
    pub fn remove(&mut self, column: &Column) -> Result<()> {
        if column.live() && column.indexed() {
            self.length = self
                .length
                .checked_sub(u64::from(column.length))
                .ok_or(Error::Authority)?;
            self.scope = self
                .scope
                .checked_sub(u64::from(column.scope))
                .ok_or(Error::Authority)?;
        }
        Ok(())
    }
}
impl Selection {
    pub fn unfiltered(&self) -> bool {
        self.from == i64::MIN
            && self.to == i64::MAX
            && self.kind.is_none()
            && self.status.is_none()
            && self.tool.is_none()
            && self.agent.is_none()
    }
    pub fn new(q: &Query<'_>) -> Self {
        Self {
            from: q.from,
            to: q.to,
            kind: q.filter.kind.map(kind),
            status: q.filter.status.map(|v| status(Some(v))),
            tool: q
                .filter
                .tool
                .as_deref()
                .map(|v| field_hash(b"tool", Some(v))),
            agent: q
                .filter
                .agent_id
                .as_deref()
                .map(|v| field_hash(b"agent", Some(v))),
        }
    }
}
impl Column {
    pub fn indexed(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn live(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn visible(&self, q: &Selection) -> bool {
        self.live()
            && self.indexed()
            && self.valid_at >= q.from
            && self.valid_at <= q.to
            && q.kind.is_none_or(|k| k == self.kind)
            && q.status.is_none_or(|s| s == self.status)
            && q.tool.is_none_or(|v| v == self.tool)
            && q.agent.is_none_or(|v| v == self.agent)
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.valid_at.to_le_bytes());
        out.extend(self.length.to_le_bytes());
        out.extend(self.scope.to_le_bytes());
        out.extend([self.kind, self.status, self.flags, 0]);
        out.extend(self.tool);
        out.extend(self.agent);
        out.extend(self.label.to_le_bytes());
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != COLUMN_BYTES
            || bytes[16] > 7
            || bytes[17] > 2
            || bytes[18] & !3 != 0
            || bytes[19] != 0
        {
            return Err(Error::Authority);
        }
        Ok(Self {
            valid_at: i64::from_le_bytes(bytes[..8].try_into().unwrap()),
            length: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            scope: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            kind: bytes[16],
            status: bytes[17],
            flags: bytes[18],
            tool: bytes[20..52].try_into().unwrap(),
            agent: bytes[52..84].try_into().unwrap(),
            label: u128::from_le_bytes(bytes[84..100].try_into().unwrap()),
        })
    }
}
fn kind(value: Kind) -> u8 {
    match value {
        Kind::Input => 0,
        Kind::Output => 1,
        Kind::Thinking => 2,
        Kind::ToolCall => 3,
        Kind::ToolResult => 4,
        Kind::System => 5,
        Kind::Explicit => 6,
        Kind::Raw => 7,
    }
}
fn status(value: Option<Status>) -> u8 {
    match value {
        None => 0,
        Some(Status::Ok) => 1,
        Some(Status::Error) => 2,
    }
}
fn field_hash(domain: &[u8], value: Option<&str>) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"oojuro/projection/field/v1\0");
    h.update(domain);
    h.update(&[u8::from(value.is_some())]);
    if let Some(value) = value {
        h.update(value.as_bytes());
    }
    *h.finalize().as_bytes()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Metadata {
    pub id: String,
    pub key: Option<String>,
    pub valid_at: i64,
    pub reported_recorded_at: i64,
    pub kind: Kind,
    pub tool: Option<String>,
    pub status: Option<Status>,
    pub agent_id: String,
    pub indexed: bool,
    pub text: Blob,
    pub text_digest: [u8; 32],
    pub length: u32,
    pub scope: u32,
}
impl Metadata {
    pub fn from_row(row: Row, text: Blob, length: u32, scope: u32) -> Self {
        let Row {
            id,
            key,
            text: original,
            valid_at,
            reported_recorded_at,
            kind,
            tool,
            status,
            agent_id,
            indexed,
        } = row;
        Self {
            id,
            key,
            valid_at,
            reported_recorded_at,
            kind,
            tool,
            status,
            agent_id,
            indexed,
            text,
            text_digest: *blake3::hash(original.as_bytes()).as_bytes(),
            length,
            scope,
        }
    }
    pub fn matches(&self, row: &Row) -> bool {
        let Row {
            id,
            key,
            text,
            valid_at,
            reported_recorded_at,
            kind,
            tool,
            status,
            agent_id,
            indexed,
        } = row;
        self.id == *id
            && self.key == *key
            && self.valid_at == *valid_at
            && self.reported_recorded_at == *reported_recorded_at
            && self.kind == *kind
            && self.tool == *tool
            && self.status == *status
            && self.agent_id == *agent_id
            && self.indexed == *indexed
            && self.text.bytes as usize == text.len()
            && self.text_digest == *blake3::hash(text.as_bytes()).as_bytes()
    }
    pub fn column(&self, label: u128) -> Column {
        Column {
            valid_at: self.valid_at,
            length: self.length,
            scope: self.scope,
            kind: kind(self.kind),
            status: status(self.status),
            flags: 2 | u8::from(self.indexed),
            tool: field_hash(b"tool", self.tool.as_deref()),
            agent: field_hash(b"agent", Some(&self.agent_id)),
            label,
        }
    }
}
pub(super) fn parse_u32(bytes: &[u8]) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| Error::Authority)?,
    ))
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn valid_generation(name: &str) -> bool {
    name.len() == 75
        && name.starts_with("generation-")
        && name[11..]
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
fn valid_blob(name: &str) -> bool {
    name.len() == 82
        && name.starts_with("blob-")
        && name.ends_with(".bin")
        && name[5..69]
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && name.as_bytes()[69] == b'-'
        && name[70..78]
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
pub(super) fn new_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)
}
pub(super) fn open_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)?;
    if !file.metadata().map_err(|_| Error::Unavailable)?.is_file() {
        return Err(Error::Invalid);
    }
    Ok(file)
}
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::Unavailable)
}
pub(super) fn read_bounded(path: &Path, bound: usize) -> Result<Vec<u8>> {
    let mut file = open_file(path)?;
    let length = file.metadata().map_err(|_| Error::Unavailable)?.len();
    if length > bound as u64 {
        return Err(Error::Bounds);
    }
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    Ok(bytes)
}
pub(super) fn pack_bytes(plain: &[u8], compression: Compression) -> Result<Vec<u8>> {
    let packed = if compression == Compression::Zstd {
        Some(zstd::bulk::compress(plain, 3).map_err(|_| Error::Unavailable)?)
    } else {
        None
    };
    let (codec, bytes) = match packed.as_ref() {
        Some(p) if p.len() < plain.len() => (1, p.as_slice()),
        _ => (0, plain),
    };
    let mut out = Vec::with_capacity(5 + bytes.len());
    out.push(codec);
    out.extend((plain.len() as u32).to_le_bytes());
    out.extend(bytes);
    Ok(out)
}
pub(super) fn unpack_bytes(bytes: &[u8], bound: usize) -> Result<Vec<u8>> {
    if bytes.len() < 5 {
        return Err(Error::Authority);
    }
    let length = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
    if length > bound {
        return Err(Error::Bounds);
    }
    let data = match bytes[0] {
        0 => bytes[5..].to_vec(),
        1 => {
            let data = &bytes[5..];
            if !data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
                || zstd::zstd_safe::find_frame_compressed_size(data).ok() != Some(data.len())
                || zstd::zstd_safe::get_dict_id_from_frame(data).is_some()
            {
                return Err(Error::Authority);
            }
            let mut decoder = zstd::bulk::Decompressor::new().map_err(|_| Error::Unavailable)?;
            decoder
                .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(25))
                .map_err(|_| Error::Unavailable)?;
            decoder
                .decompress(data, length)
                .map_err(|_| Error::Authority)?
        }
        _ => return Err(Error::Authority),
    };
    if data.len() != length {
        return Err(Error::Authority);
    }
    Ok(data)
}
pub(super) fn pack_json<T: Serialize>(value: &T, compression: Compression) -> Result<Vec<u8>> {
    let bytes = json(value, MAX_META)?;
    pack_bytes(&bytes, compression)
}
pub(super) fn unpack_json<T: DeserializeOwned>(bytes: &[u8], _: &Limits) -> Result<T> {
    serde_json::from_slice(&unpack_bytes(bytes, MAX_META)?).map_err(|_| Error::Authority)
}
pub(super) fn encode_metadata(
    value: &Metadata,
    compression: Compression,
    format: u32,
) -> Result<Vec<u8>> {
    match format {
        1 => pack_json(value, compression),
        COMPACT_FORMAT | POSITIONAL_FORMAT => metadata::encode(value, compression),
        _ => Err(Error::Authority),
    }
}
pub(super) fn decode_metadata(bytes: &[u8], limits: &Limits, format: u32) -> Result<Metadata> {
    match format {
        1 => unpack_json(bytes, limits),
        COMPACT_FORMAT | POSITIONAL_FORMAT => metadata::decode(bytes),
        _ => Err(Error::Authority),
    }
}
pub(super) fn encode_layout_postings(
    rows: &[(u32, u32)],
    compression: Compression,
    format: u32,
) -> Result<Vec<u8>> {
    if compact_format(format) {
        let base = rows.first().ok_or(Error::Bounds)?.0 & !255;
        pack_bytes(&compact::TfBlock::encode(base, rows)?, compression)
    } else if format == 1 {
        encode_postings(rows, compression)
    } else {
        Err(Error::Authority)
    }
}
pub(super) fn decode_layout_postings(
    bytes: &[u8],
    first: u32,
    rows: u64,
    format: u32,
) -> Result<Vec<(u32, u32)>> {
    if compact_format(format) {
        let raw = unpack_bytes(bytes, 1283)?;
        let pairs = compact::TfBlock::decode(first & !255, rows, raw)?.pairs();
        if pairs.first().map(|pair| pair.0) != Some(first) {
            return Err(Error::Authority);
        }
        Ok(pairs)
    } else if format == 1 {
        decode_postings(bytes)
    } else {
        Err(Error::Authority)
    }
}
pub(super) fn encode_postings(rows: &[(u32, u32)], compression: Compression) -> Result<Vec<u8>> {
    if rows.is_empty() || rows.len() > 256 {
        return Err(Error::Bounds);
    }
    let mut bytes = Vec::with_capacity(rows.len() * 8);
    for &(row, tf) in rows {
        bytes.extend(row.to_le_bytes());
        bytes.extend(tf.to_le_bytes());
    }
    pack_bytes(&bytes, compression)
}
pub(super) fn decode_postings(bytes: &[u8]) -> Result<Vec<(u32, u32)>> {
    let raw = unpack_bytes(bytes, 256 * 8)?;
    if raw.is_empty() || raw.len() % 8 != 0 {
        return Err(Error::Authority);
    }
    let mut rows = Vec::with_capacity(raw.len() / 8);
    for pair in raw.as_chunks::<8>().0 {
        let row = u32::from_le_bytes(pair[..4].try_into().unwrap());
        let tf = u32::from_le_bytes(pair[4..].try_into().unwrap());
        if tf == 0 || rows.last().is_some_and(|(old, _)| *old >= row) {
            return Err(Error::Authority);
        }
        rows.push((row, tf));
    }
    Ok(rows)
}
fn blob_aad(authority: &Authority, name: &str, reference: &Blob) -> Vec<u8> {
    let mut aad = b"oojuro/projection/blob/v1\0".to_vec();
    aad.extend(&authority.identity);
    aad.extend(name.as_bytes());
    aad.extend(reference.offset.to_le_bytes());
    aad.extend(reference.stored.to_le_bytes());
    aad.extend(reference.bytes.to_le_bytes());
    aad.extend([reference.kind, reference.codec]);
    aad
}
pub(super) struct Blobs {
    directory: PathBuf,
    cipher: Arc<FrameCipher>,
    authority: Authority,
    compression: Compression,
    pub files: Vec<String>,
    current: Option<File>,
    offset: u64,
    generation: [u8; 32],
    number: u32,
    encoder: Option<zstd::bulk::Compressor<'static>>,
}
impl Blobs {
    pub fn new(
        directory: PathBuf,
        cipher: Arc<FrameCipher>,
        authority: Authority,
        compression: Compression,
        files: Vec<String>,
    ) -> Self {
        Self {
            directory,
            cipher,
            authority,
            compression,
            files,
            current: None,
            offset: 0,
            generation: random_key(),
            number: 0,
            encoder: None,
        }
    }
    pub fn put(&mut self, kind: u8, plain: &[u8]) -> Result<Blob> {
        if plain.len() > MAX_TEXT_BYTES.max(COLUMN_ROWS * COLUMN_BYTES) {
            return Err(Error::Bounds);
        }
        let packed = if self.compression == Compression::Zstd {
            if self.encoder.is_none() {
                self.encoder =
                    Some(zstd::bulk::Compressor::new(3).map_err(|_| Error::Unavailable)?);
            }
            Some(
                self.encoder
                    .as_mut()
                    .unwrap()
                    .compress(plain)
                    .map_err(|_| Error::Unavailable)?,
            )
        } else {
            None
        };
        let (codec, body) = match packed.as_ref() {
            Some(p) if p.len() < plain.len() => (1, p.as_slice()),
            _ => (0, plain),
        };
        let stored = body.len().checked_add(SEAL_OVERHEAD).ok_or(Error::Bounds)?;
        if self.current.is_some() && self.offset + stored as u64 > PACK_BYTES {
            self.close()?;
        }
        if self.current.is_none() {
            if self.files.len() >= 4096 {
                return Err(Error::Bounds);
            }
            let name = format!("blob-{}-{:08x}.bin", hex(&self.generation), self.number);
            self.number += 1;
            self.current = Some(new_file(&self.directory.join(&name))?);
            self.files.push(name);
            self.offset = 0;
        }
        let mut reference = Blob {
            file: (self.files.len() - 1) as u32,
            offset: self.offset,
            stored: stored as u32,
            bytes: plain.len() as u32,
            kind,
            codec,
            digest: [0; 32],
        };
        let sealed = self.cipher.seal_frame(
            &blob_aad(&self.authority, self.files.last().unwrap(), &reference),
            body,
        );
        reference.digest = *blake3::hash(&sealed).as_bytes();
        self.authority.reserve(
            &self.directory.join(self.files.last().unwrap()),
            self.offset
                .checked_add(stored as u64)
                .ok_or(Error::Bounds)?,
        )?;
        self.current
            .as_mut()
            .unwrap()
            .write_all(&sealed)
            .map_err(|_| Error::Unavailable)?;
        self.offset += stored as u64;
        Ok(reference)
    }
    fn close(&mut self) -> Result<()> {
        if let Some(file) = self.current.take() {
            file.sync_all().map_err(|_| Error::Unavailable)?;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        self.close()
    }
}
pub(super) fn read_blob(
    directory: &Path,
    cipher: &FrameCipher,
    authority: &Authority,
    files: &[String],
    reference: &Blob,
    bound: usize,
) -> Result<Vec<u8>> {
    if reference.bytes as usize > bound
        || reference.stored as usize > bound + SEAL_OVERHEAD
        || reference.codec > 1
        || !matches!(reference.kind, TEXT | COLUMNS)
    {
        return Err(Error::Bounds);
    }
    let name = files
        .get(reference.file as usize)
        .filter(|n| valid_blob(n))
        .ok_or(Error::Authority)?;
    let file = open_file(&directory.join(name))?;
    let size = file.metadata().map_err(|_| Error::Unavailable)?.len();
    if size > PACK_BYTES
        || reference
            .offset
            .checked_add(u64::from(reference.stored))
            .is_none_or(|end| end > size)
    {
        return Err(Error::Bounds);
    }
    let mut sealed = vec![0; reference.stored as usize];
    file.read_exact_at(&mut sealed, reference.offset)
        .map_err(|_| Error::Unavailable)?;
    if *blake3::hash(&sealed).as_bytes() != reference.digest {
        return Err(Error::Authority);
    }
    let plain = cipher
        .open_frame(&blob_aad(authority, name, reference), &sealed)
        .map_err(|_| Error::Authority)?;
    let bytes = if reference.codec == 0 {
        plain
    } else {
        if !plain.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
            || zstd::zstd_safe::find_frame_compressed_size(&plain).ok() != Some(plain.len())
            || zstd::zstd_safe::get_dict_id_from_frame(&plain).is_some()
        {
            return Err(Error::Authority);
        }
        zstd::bulk::decompress(&plain, reference.bytes as usize).map_err(|_| Error::Authority)?
    };
    if bytes.len() != reference.bytes as usize {
        return Err(Error::Authority);
    }
    Ok(bytes)
}
fn manifest_aad(authority: &Authority) -> Vec<u8> {
    let mut bytes = b"oojuro/projection/manifest/v1\0".to_vec();
    bytes.extend(&authority.binding);
    bytes
}
pub(super) fn publish(
    root: &Path,
    cipher: &FrameCipher,
    authority: &Authority,
    manifest: &mut Manifest,
) -> Result<()> {
    let temporary = root.join(format!(".manifest-{}.tmp", hex(&random_key())));
    // Reserve before writing. Updating the serialized peak counters may grow
    // the descriptor by a few digits; converge before exposing any bytes.
    let mut plain = json(manifest, MAX_DESCRIPTOR)?;
    for _ in 0..4 {
        authority.reserve(&temporary, (plain.len() + SEAL_OVERHEAD) as u64)?;
        if let Some(budget) = &authority.budget {
            manifest.build_metrics = budget.observe()?;
        }
        let next = json(manifest, MAX_DESCRIPTOR)?;
        if next.len() <= plain.len() {
            plain = next;
            break;
        }
        plain = next;
    }
    authority.reserve(&temporary, (plain.len() + SEAL_OVERHEAD) as u64)?;
    let sealed = cipher.seal_frame(&manifest_aad(authority), &plain);
    let mut file = new_file(&temporary)?;
    file.write_all(&sealed)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Unavailable)?;
    tree::Authority::rename_file(authority, &temporary, &root.join(MANIFEST))?;
    #[cfg(test)]
    if FAIL_AFTER_MANIFEST_RENAME.with(|fail| fail.replace(false)) {
        return Err(Error::Unavailable);
    }
    sync_directory(root)
}
pub(super) fn open(root: &Path, key: &[u8; 32], binding: &Binding) -> Result<Index> {
    open_with_limits(root, key, binding, Limits::default())
}
pub(super) fn open_with_limits(
    root: &Path,
    key: &[u8; 32],
    binding: &Binding,
    limits: Limits,
) -> Result<Index> {
    open_with_cipher(root, Arc::new(FrameCipher::new(key)), binding, limits)
}
pub(super) fn open_with_cipher(
    root: &Path,
    cipher: Arc<FrameCipher>,
    binding: &Binding,
    limits: Limits,
) -> Result<Index> {
    limits.validate()?;
    let sealed = read_bounded(&root.join(MANIFEST), MAX_DESCRIPTOR + SEAL_OVERHEAD)?;
    let mut opened = None;
    for format in [FORMAT, COMPACT_FORMAT, POSITIONAL_FORMAT] {
        let authority = Authority::for_layout(binding, format)?;
        if let Ok(plain) = cipher.open_frame(&manifest_aad(&authority), &sealed) {
            opened = Some((format, authority, plain));
            break;
        }
    }
    let (format, authority, plain) = opened.ok_or(Error::Authority)?;
    let mut manifest: Manifest = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
    manifest.limits.validate()?;
    // Stored policy cannot loosen this handle's caller-supplied work caps.
    // Both policies are positive/validated; retaining the stricter one also
    // prevents a default reopen from silently raising a deliberately low cap.
    manifest.limits.original_bytes_per_query = manifest
        .limits
        .original_bytes_per_query
        .min(limits.original_bytes_per_query);
    manifest.limits.query_terms = manifest.limits.query_terms.min(limits.query_terms);
    if manifest.format != format
        || manifest.positions.is_some() != (format == POSITIONAL_FORMAT)
        || manifest
            .position_codec
            .is_some_and(|codec| format != POSITIONAL_FORMAT || codec != 2)
        || manifest.binding != authority.binding
        || !valid_generation(&manifest.generation)
        || manifest.files.len() > 4096
        || manifest.rows > limits.rows
        || manifest.live_rows > manifest.rows
        || manifest.source_bytes > limits.source_bytes
        || manifest.rows as usize > limits.metadata_bytes / std::mem::size_of::<Column>()
        || manifest.columns.len() != (manifest.rows as usize).div_ceil(COLUMN_ROWS)
    {
        return Err(Error::Bounds);
    }
    let directory = root.join(&manifest.generation);
    let lock = open_file(&directory.join("LOCK"))?;
    lock.try_lock_shared().map_err(|_| Error::Unavailable)?;
    let pages = Arc::new(tree::PageCache::new(cipher.clone()));
    for (purpose, tree) in [
        (tree::Purpose::ProjectionMetadata, &manifest.metadata),
        (tree::Purpose::ProjectionIds, &manifest.ids),
        (tree::Purpose::ProjectionKeys, &manifest.keys),
        (tree::Purpose::ProjectionPostings, &manifest.postings),
    ] {
        tree.check(&context(
            &directory,
            &cipher,
            &authority,
            purpose,
            Some(&pages),
        ))?;
    }
    if let Some(positions) = &manifest.positions {
        positions.check(&context(
            &directory,
            &cipher,
            &authority,
            tree::Purpose::ProjectionPostings,
            Some(&pages),
        ))?;
    }
    if manifest.metadata.count != manifest.rows || manifest.ids.count != manifest.rows {
        return Err(Error::Authority);
    }
    let mut columns: Vec<Arc<[Column]>> = Vec::with_capacity(manifest.columns.len());
    let mut rows = 0usize;
    let mut live = 0u64;
    let mut totals = Totals::default();
    for reference in &manifest.columns {
        if reference.kind != COLUMNS {
            return Err(Error::Authority);
        }
        let bytes = read_blob(
            &directory,
            &cipher,
            &authority,
            &manifest.files,
            reference,
            COLUMN_ROWS * COLUMN_BYTES,
        )?;
        if bytes.len() % COLUMN_BYTES != 0 || bytes.is_empty() {
            return Err(Error::Authority);
        }
        let block = bytes
            .as_chunks::<COLUMN_BYTES>()
            .0
            .iter()
            .map(|bytes| Column::decode(bytes))
            .collect::<Result<Vec<_>>>()?;
        rows += block.len();
        live += block.iter().filter(|c| c.live()).count() as u64;
        for column in &block {
            totals.add(column)?;
        }
        if rows < manifest.rows as usize && block.len() != COLUMN_ROWS {
            return Err(Error::Authority);
        }
        columns.push(Arc::from(block));
    }
    if rows != manifest.rows as usize || live != manifest.live_rows {
        return Err(Error::Authority);
    }
    let mass = index_mass(&columns, manifest.format, limits.metadata_bytes)?;
    let retained_disk_bytes = directory_bytes(&directory)?
        .checked_add(sealed.len() as u64)
        .ok_or(Error::Bounds)?;
    if retained_disk_bytes > limits.index_bytes {
        return Err(Error::Bounds);
    }
    let retained_allocated_bytes = allocated_directory_bytes(&directory)?
        .checked_add(
            fs::metadata(root.join(MANIFEST))
                .map_err(|_| Error::Unavailable)?
                .blocks()
                * 512,
        )
        .and_then(|n| n.checked_add(fs::metadata(root).ok()?.blocks() * 512))
        .ok_or(Error::Bounds)?;
    Ok(Index {
        directory,
        root_directory: root.to_path_buf(),
        binding: binding.clone(),
        authority,
        cipher,
        pages,
        manifest,
        columns,
        mass,
        totals,
        queries: query_cache::QueryCache::default(),
        terms: term_cache::TermCache::default(),
        retained_disk_bytes,
        retained_allocated_bytes,
        _lock: lock,
    })
}
pub(super) fn index_mass(
    columns: &Vec<Arc<[Column]>>,
    format: u32,
    limit: usize,
) -> Result<Option<compact::Mass>> {
    if !compact_format(format) {
        return Ok(None);
    }
    Ok(Some(compact::Mass::new(
        columns,
        mass_limit(columns, limit)?,
    )?))
}
pub(super) fn mass_limit(columns: &Vec<Arc<[Column]>>, limit: usize) -> Result<usize> {
    let column_bytes = columns
        .iter()
        .map(|block| std::mem::size_of_val(block.as_ref()) + 2 * std::mem::size_of::<usize>())
        .sum::<usize>()
        .checked_add(columns.capacity() * std::mem::size_of::<Arc<[Column]>>())
        .ok_or(Error::Bounds)?;
    limit.checked_sub(column_bytes).ok_or(Error::Bounds)
}
pub(super) fn allocated_directory_bytes(directory: &Path) -> Result<u64> {
    let mut bytes = fs::metadata(directory)
        .map_err(|_| Error::Unavailable)?
        .blocks()
        * 512;
    for entry in fs::read_dir(directory).map_err(|_| Error::Unavailable)? {
        let metadata = fs::symlink_metadata(entry.map_err(|_| Error::Unavailable)?.path())
            .map_err(|_| Error::Unavailable)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::Invalid);
        }
        bytes = bytes
            .checked_add(metadata.blocks() * 512)
            .ok_or(Error::Bounds)?;
    }
    Ok(bytes)
}
pub(super) fn directory_bytes(directory: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    let mut count = 0usize;
    for entry in fs::read_dir(directory).map_err(|_| Error::Unavailable)? {
        count += 1;
        if count > 65536 {
            return Err(Error::Bounds);
        }
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| Error::Unavailable)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::Invalid);
        }
        bytes = bytes.checked_add(metadata.len()).ok_or(Error::Bounds)?;
    }
    Ok(bytes)
}
pub(super) fn prepare(root: &Path) -> Result<(File, PathBuf, String)> {
    tree::prepare_directory(root)?;
    let root_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("BUILD_LOCK"))
        .map_err(|_| Error::Unavailable)?;
    root_lock.try_lock().map_err(|_| Error::Unavailable)?;
    let name = format!("generation-{}", hex(&random_key()));
    let directory = root.join(format!(".build-{name}"));
    fs::create_dir(&directory).map_err(|_| Error::Unavailable)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| Error::Unavailable)?;
    new_file(&directory.join("LOCK"))?
        .sync_all()
        .map_err(|_| Error::Unavailable)?;
    Ok((root_lock, directory, name))
}
pub(super) fn finish_directory(root: &Path, directory: &Path, name: &str) -> Result<PathBuf> {
    if !valid_generation(name) {
        return Err(Error::Invalid);
    }
    let destination = root.join(name);
    sync_directory(directory)?;
    fs::rename(directory, &destination).map_err(|_| Error::Unavailable)?;
    sync_directory(root)?;
    Ok(destination)
}
