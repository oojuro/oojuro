//! Exact existing JSON bytes through bounded writers and verified-part readers.
use super::*;
use driver::RowRef;
use oojuro_store::managed_pack::{Reader, Reservation, Stage};
use serde::ser::SerializeSeq;
use std::io::{Read, Write};
pub(super) const CHUNK: usize = 4 << 20;
const ARRAY_BYTES: usize = 2;
// {"id":u64,"recorded_at":i64,"input":VALUE}; maximum numeric spellings are 20 bytes.
const ASSIGNED_HEADER: usize =
    b"{\"id\":".len() + 20 + b",\"recorded_at\":".len() + 20 + b",\"input\":".len() + 1;
const STREAM_BYTES: usize = 6 * MAX_BATCH_BYTES + ASSIGNED_HEADER;
pub(super) fn limit(layout: u32) -> Result<usize> {
    match layout {
        1 => Ok(MAX_BATCH_BYTES),
        2 => Ok(STREAM_BYTES),
        _ => Err(Error::Authority),
    }
}
#[derive(Serialize)]
struct InputRef<'a> {
    agent_id: &'a String,
    parent_id: &'a Option<String>,
    valid_at: i64,
    kind: Kind,
    tool: &'a Option<String>,
    status: Option<crate::record::Status>,
    exit: Option<i32>,
    key: &'a Option<String>,
    text: &'a str,
    attrs: &'a BTreeMap<String, serde_json::Value>,
    indexed: bool,
}
impl<'a> InputRef<'a> {
    fn empty(v: &'a Input) -> Self {
        // Exhaustive destructure makes additions to Input a compile-time budgeting decision.
        let Input {
            agent_id,
            parent_id,
            valid_at,
            kind,
            tool,
            status,
            exit,
            key,
            text: _,
            attrs,
            indexed,
        } = v;
        Self {
            agent_id,
            parent_id,
            valid_at: *valid_at,
            kind: *kind,
            tool,
            status: *status,
            exit: *exit,
            key,
            text: "",
            attrs,
            indexed: *indexed,
        }
    }
}
struct EmptyInputs<'a>(&'a [Input]);
impl Serialize for EmptyInputs<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for i in self.0 {
            seq.serialize_element(&InputRef::empty(i))?;
        }
        seq.end()
    }
}
struct HashWriter {
    hash: blake3::Hasher,
    bytes: usize,
    max: usize,
    full: bool,
}
impl Write for HashWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.bytes.checked_add(b.len()).filter(|n| *n <= self.max);
        if let Some(n) = n {
            self.bytes = n;
            self.hash.update(b);
            Ok(b.len())
        } else {
            self.full = true;
            Err(std::io::Error::other("canonical bound"))
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn hash<T: Serialize + ?Sized>(v: &T, key: &[u8; 32], max: usize) -> Result<([u8; 32], usize)> {
    let mut w = HashWriter {
        hash: blake3::Hasher::new_keyed(key),
        bytes: 0,
        max,
        full: false,
    };
    serde_json::to_writer(&mut w, v).map_err(|_| {
        if w.full {
            Error::Bounds
        } else {
            Error::Invalid
        }
    })?;
    Ok((*w.hash.finalize().as_bytes(), w.bytes))
}
pub(super) fn fingerprint(key: &[u8; 32], inputs: &[Input], layout: u32) -> Result<[u8; 32]> {
    limit(layout)?;
    if layout == 2 {
        let metadata = hash(&EmptyInputs(inputs), key, MAX_BATCH_BYTES)?.1;
        let logical = inputs.iter().try_fold(metadata, |n, i| {
            n.checked_add(i.text.len()).ok_or(Error::Bounds)
        })?;
        if logical > MAX_BATCH_BYTES || metadata < ARRAY_BYTES {
            return Err(Error::Bounds);
        }
    }
    // Array brackets/commas are already counted in metadata. JSON escaping uses at
    // most six bytes per input UTF-8 byte; there is no uncounted request overhead.
    Ok(hash(
        inputs,
        key,
        if layout == 1 {
            MAX_BATCH_BYTES
        } else {
            6 * MAX_BATCH_BYTES
        },
    )?
    .0)
}
pub(super) fn digest<T: Serialize + ?Sized>(
    v: &T,
    key: &[u8; 32],
    layout: u32,
) -> Result<[u8; 32]> {
    Ok(hash(v, key, limit(layout)?)?.0)
}
pub(super) fn encoded_len<T: Serialize + ?Sized>(v: &T, layout: u32) -> Result<usize> {
    Ok(hash(v, &[0; 32], limit(layout)?)?.1)
}
#[derive(Serialize)]
pub(super) struct AssignedRef<'a> {
    pub id: u64,
    pub recorded_at: i64,
    pub input: &'a Input,
}
struct Chunks<'a> {
    bytes: usize,
    max: usize,
    buf: Vec<u8>,
    emit: &'a mut dyn FnMut(&[u8]) -> Result<()>,
    error: Option<Error>,
}
impl Write for Chunks<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.error.is_some() {
            return Err(std::io::Error::other("stream poisoned"));
        }
        let result = (|| {
            self.bytes = self
                .bytes
                .checked_add(b.len())
                .filter(|n| *n <= self.max)
                .ok_or(Error::Bounds)?;
            let mut rest = b;
            while !rest.is_empty() {
                let n = (CHUNK - self.buf.len()).min(rest.len());
                self.buf.extend_from_slice(&rest[..n]);
                rest = &rest[n..];
                if self.buf.len() == CHUNK {
                    (self.emit)(&self.buf)?;
                    self.buf.clear();
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(e);
            Err(std::io::Error::other("bounded stream refused"))
        } else {
            Ok(b.len())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) struct Packs<'a, 'c> {
    directory: PathBuf,
    birth: Birth,
    cipher: Arc<FrameCipher>,
    layout: u32,
    physical: &'a mut physical::Editor<'c>,
    segments: &'a mut tree::Editor<'c>,
    stage: Option<Stage>,
    file: u64,
    seq: u64,
    parts: u32,
    pack_first: u64,
    logical_first: u64,
    row_packs: bool,
}
impl<'a, 'c> Packs<'a, 'c> {
    pub fn new(
        directory: PathBuf,
        birth: Birth,
        cipher: Arc<FrameCipher>,
        layout: u32,
        physical: &'a mut physical::Editor<'c>,
        segments: &'a mut tree::Editor<'c>,
        file: u64,
        seq: u64,
        row_packs: bool,
    ) -> Self {
        Self {
            directory,
            birth,
            cipher,
            layout,
            physical,
            segments,
            stage: None,
            file,
            seq,
            parts: 0,
            pack_first: seq,
            logical_first: 0,
            row_packs,
        }
    }
    fn finish_pack(&mut self) -> Result<()> {
        if let Some(stage) = self.stage.take() {
            let p = stage.finish().map_err(io)?;
            if p.first_physical_seq != self.pack_first
                || p.record_count != self.seq - self.pack_first
            {
                return Err(Error::Authority);
            }
            self.physical.insert_pack(&p)?;
            let key = if self.layout == 2 {
                self.pack_first
            } else {
                self.logical_first
            };
            self.segments.insert_immutable(tree::Entry::new(
                key.to_be_bytes().to_vec(),
                self.file.to_be_bytes().to_vec(),
            )?)?;
            self.file = self.file.checked_add(1).ok_or(Error::Bounds)?;
            self.parts = 0;
        }
        Ok(())
    }
    fn part(&mut self, id: u64, b: &[u8]) -> Result<()> {
        // RAW fallback bounds each frame by its original <=4MiB carrier plus fixed
        // headers. Eight frames stay far below the unchanged 64MiB pack ceiling.
        if self.layout == 2 && self.parts == 8 {
            self.finish_pack()?;
        }
        if self.stage.is_none() {
            self.pack_first = self.seq;
            self.logical_first = id;
            self.stage = Some(
                Stage::new(
                    &self.directory,
                    self.birth.clone(),
                    Reservation {
                        file_id: self.file,
                        first_physical_seq: self.seq,
                    },
                    self.cipher.clone(),
                    oojuro_store::managed_pack::Options::default(),
                )
                .map_err(io)?,
            );
        }
        self.stage
            .as_mut()
            .ok_or(Error::Authority)?
            .push(&[super::carrier(b)], &mut |l| {
                self.physical
                    .insert_frame(&l)
                    .map_err(|_| oojuro_store::Error::Corrupt("stream locator refused"))
            })
            .map_err(io)?;
        self.seq = self.seq.checked_add(1).ok_or(Error::Bounds)?;
        self.parts += 1;
        Ok(())
    }
    pub fn row(&mut self, id: u64, recorded_at: i64, input: &Input) -> Result<RowRef> {
        if self.row_packs && self.layout == 1 {
            self.finish_pack()?;
        }
        let first = self.seq;
        let max = limit(self.layout)?;
        let mut emit = |b: &[u8]| self.part(id, b);
        let mut w = Chunks {
            bytes: 0,
            max,
            buf: Vec::with_capacity(CHUNK),
            emit: &mut emit,
            error: None,
        };
        if serde_json::to_writer(
            &mut w,
            &AssignedRef {
                id,
                recorded_at,
                input,
            },
        )
        .is_err()
        {
            return Err(w.error.unwrap_or(Error::Invalid));
        }
        if !w.buf.is_empty() {
            (w.emit)(&w.buf)?;
        }
        Ok(RowRef {
            first,
            chunks: u32::try_from(w.bytes.div_ceil(CHUNK)).map_err(|_| Error::Bounds)?,
            bytes: u32::try_from(w.bytes).map_err(|_| Error::Bounds)?,
        })
    }
    pub fn finish(mut self) -> Result<(u64, u64)> {
        self.finish_pack()?;
        Ok((self.file, self.seq))
    }
}
struct Parts<'a, C: oojuro_store::managed_pack::Catalog> {
    reader: &'a Reader<C>,
    r: &'a RowRef,
    part: u32,
    buf: Vec<u8>,
    at: usize,
    total: usize,
}
impl<C: oojuro_store::managed_pack::Catalog> Read for Parts<'_, C> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.at == self.buf.len() {
            if self.part == self.r.chunks {
                return Ok(0);
            }
            self.buf = self
                .reader
                .get(
                    self.r
                        .first
                        .checked_add(u64::from(self.part))
                        .ok_or_else(|| std::io::Error::other("part sequence"))?,
                )
                .map_err(std::io::Error::other)?;
            let expected = CHUNK.min(self.r.bytes as usize - self.total);
            if self.buf.len() != expected {
                return Err(std::io::Error::other("part length"));
            }
            self.part += 1;
            self.at = 0;
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        self.total += n;
        Ok(n)
    }
}
fn parts<'a, C: oojuro_store::managed_pack::Catalog>(
    reader: &'a Reader<C>,
    r: &'a RowRef,
) -> Parts<'a, C> {
    Parts {
        reader,
        r,
        part: 0,
        buf: vec![],
        at: 0,
        total: 0,
    }
}
// Preflight precedes serde allocation: bounded decoded strings, node/token count,
// depth and primitive spellings. Serde then validates complete JSON/UTF-8 semantics.
fn preflight(mut r: impl Read) -> Result<()> {
    let (mut quoted, mut escape, mut unicode, mut code, mut high) =
        (false, false, 0u8, 0u16, false);
    let (mut strings, mut tokens, mut depth, mut primitive) = (0usize, 0usize, 0usize, 0usize);
    let mut buf = [0u8; 64 << 10];
    loop {
        let n = r.read(&mut buf).map_err(|_| Error::Authority)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if quoted {
                let add = if unicode != 0 {
                    code = code
                        .checked_mul(16)
                        .and_then(|v| (b as char).to_digit(16).map(|d| v + d as u16))
                        .ok_or(Error::Authority)?;
                    unicode -= 1;
                    if unicode != 0 {
                        0
                    } else if high {
                        if !(0xdc00..=0xdfff).contains(&code) {
                            return Err(Error::Authority);
                        }
                        high = false;
                        0
                    } else if (0xd800..=0xdbff).contains(&code) {
                        high = true;
                        4
                    } else if (0xdc00..=0xdfff).contains(&code) {
                        return Err(Error::Authority);
                    } else if code < 0x80 {
                        1
                    } else if code < 0x800 {
                        2
                    } else {
                        3
                    }
                } else if escape {
                    escape = false;
                    if b == b'u' {
                        unicode = 4;
                        code = 0;
                        0
                    } else if !high && b"\"\\/bfnrt".contains(&b) {
                        1
                    } else {
                        return Err(Error::Authority);
                    }
                } else if b == b'\\' {
                    escape = true;
                    0
                } else if high {
                    return Err(Error::Authority);
                } else if b == b'"' {
                    quoted = false;
                    0
                } else if b < 0x20 {
                    return Err(Error::Authority);
                } else {
                    1
                };
                strings = strings.checked_add(add).ok_or(Error::Bounds)?;
                if strings > MAX_BATCH_BYTES + ASSIGNED_HEADER {
                    return Err(Error::Bounds);
                }
            } else {
                match b {
                    b'"' => {
                        quoted = true;
                        primitive = 0;
                        tokens += 1;
                    }
                    b'{' | b'[' => {
                        depth += 1;
                        tokens += 1;
                        primitive = 0;
                    }
                    b'}' | b']' => {
                        depth = depth.checked_sub(1).ok_or(Error::Authority)?;
                        primitive = 0;
                    }
                    b',' | b':' | b' ' | b'\t' | b'\r' | b'\n' => {
                        primitive = 0;
                    }
                    _ => {
                        if primitive == 0 {
                            tokens += 1;
                        }
                        primitive += 1;
                        if primitive > 64 {
                            return Err(Error::Bounds);
                        }
                    }
                }
            }
            if tokens > 3 * 4096 + 64 || depth > 40 {
                return Err(Error::Bounds);
            }
        }
    }
    if quoted || escape || unicode != 0 || high || depth != 0 {
        return Err(Error::Authority);
    }
    Ok(())
}
pub(super) fn decode<C: oojuro_store::managed_pack::Catalog>(
    reader: &Reader<C>,
    r: &RowRef,
    id: u64,
    layout: u32,
) -> Result<Option<Assigned>> {
    if r.bytes == 0
        || r.bytes as usize > limit(layout)?
        || r.chunks == 0
        || r.chunks as usize != (r.bytes as usize).div_ceil(CHUNK)
    {
        return Err(Error::Authority);
    }
    preflight(parts(reader, r))?;
    let buffered = std::io::BufReader::with_capacity(64 << 10, parts(reader, r));
    let mut de = serde_json::Deserializer::from_reader(buffered);
    let row = Assigned::deserialize(&mut de).map_err(|_| Error::Authority)?;
    de.end().map_err(|_| Error::Authority)?;
    if row.id != id {
        return Err(Error::Authority);
    }
    row.input.validate()?;
    Ok(Some(row))
}
#[cfg(test)]
mod tests;
