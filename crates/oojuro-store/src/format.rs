//! On-disk encodings shared by the hot log and cold segments.
//!
//! A *frame* is the unit of compression and integrity. Hot logs hold one frame
//! per appended turn batch; cold segments hold larger compacted frames.
//!
//! ```text
//! frame   := len u32 | crc u32 | payload            crc = crc32(len_le ++ payload)
//! payload := codec u8 | dict varint | first_seq varint | count varint
//!            | raw_len varint | meta_len varint | [digest 16B if codec&0x80] | packed body
//! body    := meta (meta_len bytes) | texts
//! meta    := per record: flags u8 | text_len varint | key_len varint | attrs_len varint
//!                        | zigzag(valid_at - prev_valid_at) | zigzag(recorded_at - valid_at)
//!            then every record's key and attrs bytes, in record order
//! ```
//! `flags` low 7 bits are the record kind; bit 7 marks a deduplicated record
//! whose text is a piece list (stored after all record headers, before the
//! keys): `n varint` then per piece `tag varint` = `len << 1 | is_ref`, and for
//! a reference `frame_delta varint | at varint`. A reference names `len` literal
//! bytes at text-area offset `at` of the frame whose first sequence number is
//! `first_seq - frame_delta` (an earlier frame); references never point at
//! other references, so a read is at most one hop. The texts area holds each
//! record's literal bytes in order.
//!
//! Records in a frame have consecutive sequence numbers starting at
//! `first_seq`. `digest` is the first 16 bytes of BLAKE3(body), present in
//! compacted (cold) frames. The meta section comes first so an excerpt needs
//! to decode only `meta_len + end` body bytes.

use crate::{Error, Result};

pub const CODEC_RAW: u8 = 0;
pub const CODEC_LZ4: u8 = 1;
pub const CODEC_ZSTD: u8 = 2;
pub const HAS_DIGEST: u8 = 0x80;
/// Record flag: text is a piece list (see module docs).
pub const PIECES: u8 = 0x80;
pub const MAX_FRAME: usize = 64 << 20;

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn get_varint(buf: &[u8], at: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*at).ok_or(Error::Corrupt("truncated varint"))?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(Error::Corrupt("varint overflow"))
}

pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Record header as stored in a frame's meta section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordMeta {
    pub kind: u8,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub text_len: u32,
}

/// One record as given to the encoder.
pub struct RecordIn<'a> {
    pub kind: u8,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub key: &'a [u8],
    pub attrs: &'a [u8],
    pub text: &'a [u8],
}

/// One piece of a deduplicated record's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece {
    /// `len` bytes taken from this record's literal bytes, in order.
    Lit(u32),
    /// `len` literal bytes at text-area offset `at` of an earlier frame.
    Ref { frame_delta: u64, at: u32, len: u32 },
}

impl Piece {
    pub fn len(&self) -> u32 {
        match *self {
            Piece::Lit(l) | Piece::Ref { len: l, .. } => l,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A record as laid out in a body: `lit` are its literal bytes; `pieces` is
/// empty for a plain record (then `lit` is the whole text).
pub struct BodyRecord<'a> {
    pub kind: u8,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub key: &'a [u8],
    pub attrs: &'a [u8],
    pub text_len: usize,
    pub pieces: &'a [Piece],
    pub lit: &'a [u8],
}

/// Build the uncompressed body; returns (body, meta_len).
pub fn encode_body(records: &[RecordIn]) -> (Vec<u8>, usize) {
    let recs: Vec<BodyRecord> = records
        .iter()
        .map(|r| BodyRecord {
            kind: r.kind,
            valid_at: r.valid_at,
            recorded_at: r.recorded_at,
            key: r.key,
            attrs: r.attrs,
            text_len: r.text.len(),
            pieces: &[],
            lit: r.text,
        })
        .collect();
    encode_records(&recs)
}

pub fn encode_records(records: &[BodyRecord]) -> (Vec<u8>, usize) {
    let lit_bytes: usize = records.iter().map(|r| r.lit.len()).sum();
    let mut body = Vec::with_capacity(lit_bytes + 16 * records.len());
    let mut prev = 0i64;
    for r in records {
        body.push((r.kind & 0x7f) | if r.pieces.is_empty() { 0 } else { PIECES });
        put_varint(&mut body, r.text_len as u64);
        put_varint(&mut body, r.key.len() as u64);
        put_varint(&mut body, r.attrs.len() as u64);
        put_varint(&mut body, zigzag(r.valid_at.wrapping_sub(prev)));
        put_varint(&mut body, zigzag(r.recorded_at.wrapping_sub(r.valid_at)));
        prev = r.valid_at;
    }
    for r in records.iter().filter(|r| !r.pieces.is_empty()) {
        put_varint(&mut body, r.pieces.len() as u64);
        for p in r.pieces {
            match *p {
                Piece::Lit(len) => put_varint(&mut body, (len as u64) << 1),
                Piece::Ref { frame_delta, at, len } => {
                    put_varint(&mut body, ((len as u64) << 1) | 1);
                    put_varint(&mut body, frame_delta);
                    put_varint(&mut body, at as u64);
                }
            }
        }
    }
    for r in records {
        body.extend_from_slice(r.key);
        body.extend_from_slice(r.attrs);
    }
    let meta_len = body.len();
    for r in records {
        body.extend_from_slice(r.lit);
    }
    (body, meta_len)
}

/// Parsed meta section: per-record headers plus byte offsets into the body.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub records: Vec<RecordMeta>,
    /// (key_start, key_len, attrs_len) per record, offsets within the body.
    pub side: Vec<(u32, u32, u32)>,
    /// Start of each record's literal bytes within the body; one extra entry = body end.
    pub text_start: Vec<u32>,
    /// Range into `piece_list` per record; empty for plain records.
    pub pieces: Vec<(u32, u32)>,
    pub piece_list: Vec<Piece>,
}

pub fn decode_meta(meta: &[u8], count: usize) -> Result<Meta> {
    let mut at = 0;
    let mut out = Meta {
        records: Vec::with_capacity(count),
        side: Vec::with_capacity(count),
        text_start: Vec::with_capacity(count + 1),
        pieces: Vec::with_capacity(count),
        piece_list: Vec::new(),
    };
    let mut lens = Vec::with_capacity(count);
    let mut flagged = Vec::new();
    let mut prev = 0i64;
    for i in 0..count {
        let flags = *meta.get(at).ok_or(Error::Corrupt("truncated meta"))?;
        at += 1;
        let text_len = get_varint(meta, &mut at)?;
        let key_len = get_varint(meta, &mut at)?;
        let attrs_len = get_varint(meta, &mut at)?;
        let valid_at = prev.wrapping_add(unzigzag(get_varint(meta, &mut at)?));
        let recorded_at = valid_at.wrapping_add(unzigzag(get_varint(meta, &mut at)?));
        prev = valid_at;
        if text_len > u32::MAX as u64 || key_len > MAX_FRAME as u64 || attrs_len > MAX_FRAME as u64 {
            return Err(Error::Corrupt("record length out of range"));
        }
        out.records.push(RecordMeta { kind: flags & 0x7f, valid_at, recorded_at, text_len: text_len as u32 });
        lens.push((key_len as u32, attrs_len as u32));
        if flags & PIECES != 0 {
            flagged.push(i);
        }
    }
    out.pieces = vec![(0, 0); count];
    let mut lit_len: Vec<u64> = out.records.iter().map(|r| r.text_len as u64).collect();
    for i in flagged {
        let n = get_varint(meta, &mut at)?;
        if n == 0 || n > out.records[i].text_len as u64 {
            return Err(Error::Corrupt("piece count out of range"));
        }
        let start = out.piece_list.len() as u32;
        let (mut total, mut lits) = (0u64, 0u64);
        for _ in 0..n {
            let tag = get_varint(meta, &mut at)?;
            let len = tag >> 1;
            if len == 0 || len > u32::MAX as u64 {
                return Err(Error::Corrupt("piece length out of range"));
            }
            let p = if tag & 1 == 0 {
                lits += len;
                Piece::Lit(len as u32)
            } else {
                let frame_delta = get_varint(meta, &mut at)?;
                let off = get_varint(meta, &mut at)?;
                if frame_delta == 0 || off > u32::MAX as u64 {
                    return Err(Error::Corrupt("reference out of range"));
                }
                Piece::Ref { frame_delta, at: off as u32, len: len as u32 }
            };
            total += len;
            out.piece_list.push(p);
        }
        if total != out.records[i].text_len as u64 {
            return Err(Error::Corrupt("pieces do not cover the record"));
        }
        lit_len[i] = lits;
        out.pieces[i] = (start, out.piece_list.len() as u32);
    }
    let mut side_at = at;
    for (k, a) in lens {
        out.side.push((side_at as u32, k, a));
        side_at += (k + a) as usize;
    }
    if side_at != meta.len() {
        return Err(Error::Corrupt("meta length mismatch"));
    }
    let mut t = meta.len() as u64;
    for l in lit_len {
        out.text_start.push(t as u32);
        t += l;
        if t > MAX_FRAME as u64 {
            return Err(Error::Corrupt("body overflow"));
        }
    }
    out.text_start.push(t as u32);
    Ok(out)
}

/// Fixed fields at the start of a payload (outside compression).
#[derive(Clone, Copy, Debug)]
pub struct FrameHead {
    pub codec: u8,
    pub dict: u32,
    pub first_seq: u64,
    pub count: u32,
    pub raw_len: u32,
    pub meta_len: u32,
    pub digest: Option<[u8; 16]>,
    /// Offset of the packed body inside the payload.
    pub body_at: u32,
}

pub fn put_head(out: &mut Vec<u8>, h: &FrameHead) {
    out.push(h.codec | if h.digest.is_some() { HAS_DIGEST } else { 0 });
    put_varint(out, h.dict as u64);
    put_varint(out, h.first_seq);
    put_varint(out, h.count as u64);
    put_varint(out, h.raw_len as u64);
    put_varint(out, h.meta_len as u64);
    if let Some(d) = h.digest {
        out.extend_from_slice(&d);
    }
}

pub fn get_head(payload: &[u8]) -> Result<FrameHead> {
    let flags = *payload.first().ok_or(Error::Corrupt("empty payload"))?;
    let mut at = 1;
    let dict = get_varint(payload, &mut at)? as u32;
    let first_seq = get_varint(payload, &mut at)?;
    let count = get_varint(payload, &mut at)?;
    let raw_len = get_varint(payload, &mut at)?;
    let meta_len = get_varint(payload, &mut at)?;
    if raw_len > MAX_FRAME as u64 || meta_len > raw_len || count > raw_len.max(1) * 2 + 1 {
        return Err(Error::Corrupt("frame head out of range"));
    }
    let digest = if flags & HAS_DIGEST != 0 {
        let d: [u8; 16] = payload.get(at..at + 16).ok_or(Error::Corrupt("truncated digest"))?.try_into().unwrap();
        at += 16;
        Some(d)
    } else {
        None
    };
    Ok(FrameHead {
        codec: flags & !HAS_DIGEST,
        dict,
        first_seq,
        count: count as u32,
        raw_len: raw_len as u32,
        meta_len: meta_len as u32,
        digest,
        body_at: at as u32,
    })
}

pub fn frame_crc(payload: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&(payload.len() as u32).to_le_bytes());
    h.update(payload);
    h.finalize()
}

/// Wrap a payload as `len | crc | payload`.
pub fn seal_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&frame_crc(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn digest16(body: &[u8]) -> [u8; 16] {
    blake3::hash(body).as_bytes()[..16].try_into().unwrap()
}
