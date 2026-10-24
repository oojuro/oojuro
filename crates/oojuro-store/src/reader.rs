//! Frame read path shared by the store and immutable run files: partial
//! decode (meta first, then only the body bytes a range needs), a
//! byte-bounded cache of decoded frames, and one-hop reference resolution.

use crate::format::*;
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};
use zstd::zstd_safe;

/// Where a frame lives. Records in it are `first_seq..first_seq + count`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Loc {
    pub first_seq: u64,
    pub count: u32,
    pub file: u64,
    /// Offset of the frame (its `len` field) in the file.
    pub offset: u64,
    pub payload_len: u32,
    pub raw_len: u32,
}

/// A sequence of frames ordered by `first_seq`.
pub(crate) trait Frames {
    fn loc(&self, i: usize) -> Loc;
    fn frame_index(&self, seq: u64) -> Result<usize>;
    fn file(&self, id: u64) -> &File;
    fn dict(&self, id: u32) -> Option<&[u8]>;
    /// A prepared shared dictionary for `id`, referenced instead of loaded.
    fn ddict(&self, _id: u32) -> Option<&'static zstd_safe::DDict<'static>> {
        None
    }
    fn read_state(&self) -> &ReadState;
    /// The key sealed frames are opened with.
    fn cipher(&self) -> Option<&crate::crypto::FrameCipher> {
        None
    }
    fn require_sealed(&self) -> bool {
        false
    }
}

pub(crate) struct Decoded {
    pub meta: Meta,
    /// Body prefix; complete when its length equals the frame's raw length.
    pub body: Vec<u8>,
}

impl Decoded {
    fn cost(&self) -> usize {
        fn allocated<T>(v: &Vec<T>) -> usize {
            v.capacity().saturating_mul(std::mem::size_of::<T>())
        }
        // Charge known retained allocations, including spare capacity. Arc/cache
        // table/allocator and decoder overhead are separate; callers can retain
        // an Arc after eviction, so this is not a process RSS limit.
        [
            allocated(&self.body),
            allocated(&self.meta.records),
            allocated(&self.meta.side),
            allocated(&self.meta.text_start),
            allocated(&self.meta.pieces),
            allocated(&self.meta.piece_list),
        ]
        .into_iter()
        .fold(std::mem::size_of::<Self>(), usize::saturating_add)
    }
}

struct ReadCtx {
    /// Idle decoder contexts per dictionary: a reader takes one out, decodes
    /// without holding the lock, and puts it back (track/tracedb: one shared
    /// context per dictionary serialized concurrent readers of a run).
    dctx: HashMap<u32, Vec<zstd_safe::DCtx<'static>>>,
    cache: HashMap<u64, Arc<Decoded>>,
    order: VecDeque<u64>,
    cached_bytes: usize,
}

/// Decoder contexts (one per dictionary) and the decoded-frame cache.
pub(crate) struct ReadState {
    ctx: Mutex<ReadCtx>,
    budget: usize,
}

impl ReadState {
    pub fn new(budget: usize) -> Self {
        Self { ctx: Mutex::new(ReadCtx { dctx: HashMap::new(), cache: HashMap::new(), order: VecDeque::new(), cached_bytes: 0 }), budget }
    }

    pub fn clear_cache(&self) {
        let mut rc = self.ctx.lock().unwrap();
        rc.cache.clear();
        rc.order.clear();
        rc.cached_bytes = 0;
    }

    pub fn forget_dict(&self, id: u32) {
        self.ctx.lock().unwrap().dctx.remove(&id);
    }
}

pub(crate) fn zerr(code: usize) -> Error {
    Error::Codec(zstd_safe::get_error_name(code))
}

/// A decoder referencing a prepared (shared) dictionary.
pub(crate) fn new_dctx_ref(dd: &'static zstd_safe::DDict<'static>) -> Result<zstd_safe::DCtx<'static>> {
    let mut d = zstd_safe::DCtx::create();
    d.set_parameter(zstd_safe::DParameter::Format(zstd_safe::FrameFormat::Magicless)).map_err(zerr)?;
    d.ref_ddict(dd).map_err(zerr)?;
    Ok(d)
}

pub(crate) fn new_dctx(dict: Option<&[u8]>) -> Result<zstd_safe::DCtx<'static>> {
    let mut d = zstd_safe::DCtx::create();
    d.set_parameter(zstd_safe::DParameter::Format(zstd_safe::FrameFormat::Magicless)).map_err(zerr)?;
    if let Some(dict) = dict {
        d.load_dictionary(dict).map_err(zerr)?;
    }
    Ok(d)
}

fn read_payload(file: &File, f: &Loc) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; 8 + f.payload_len as usize];
    file.read_exact_at(&mut buf, f.offset)?;
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap());
    let want = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    buf.drain(..8);
    if len != f.payload_len || frame_crc(&buf) != want {
        return Err(Error::Corrupt("frame checksum mismatch"));
    }
    Ok(buf)
}

fn check_head(f: &Loc, payload: &[u8]) -> Result<FrameHead> {
    let head = get_head(payload)?;
    if head.first_seq != f.first_seq || head.count != f.count || head.raw_len != f.raw_len {
        return Err(Error::Corrupt("frame does not match its index entry"));
    }
    Ok(head)
}

fn meta_of(head: &FrameHead, body: &[u8]) -> Result<Meta> {
    let meta = decode_meta(&body[..head.meta_len as usize], head.count as usize)?;
    if *meta.text_start.last().unwrap() as usize != head.raw_len as usize {
        return Err(Error::Corrupt("frame body length disagrees with its meta"));
    }
    Ok(meta)
}

/// Decode the meta section, then body bytes up to `need(&meta)` (all when
/// `need` is None or asks for more). Returns (meta, body prefix).
pub(crate) fn decode_payload<F: Frames + ?Sized>(fs: &F, payload: &[u8], head: &FrameHead, need: Option<&dyn Fn(&Meta) -> usize>) -> Result<(Meta, Vec<u8>)> {
    if fs.require_sealed() && !head.encrypted {
        return Err(Error::Crypto("plaintext frame in strict sealed store"));
    }
    let opened;
    let packed: &[u8] = if head.encrypted {
        let c = fs.cipher().ok_or(Error::KeyRequired)?;
        opened = c.open_frame(&payload[..head.body_at as usize], &payload[head.body_at as usize..])?;
        &opened
    } else {
        &payload[head.body_at as usize..]
    };
    let raw_len = head.raw_len as usize;
    let meta_len = head.meta_len as usize;
    let (meta, body) = match head.codec {
        CODEC_RAW | CODEC_LZ4 => {
            let body = if head.codec == CODEC_RAW {
                packed.to_vec()
            } else {
                lz4_flex::block::decompress(packed, raw_len).map_err(|_| Error::Corrupt("lz4 frame"))?
            };
            if body.len() != raw_len {
                return Err(Error::Corrupt("frame body length"));
            }
            (meta_of(head, &body)?, body)
        }
        CODEC_ZSTD => {
            let pooled = fs.read_state().ctx.lock().unwrap().dctx.get_mut(&head.dict).and_then(|v| v.pop());
            let mut owned = match pooled {
                Some(d) => d,
                None => match fs.ddict(head.dict) {
                    Some(dd) => new_dctx_ref(dd)?,
                    None => {
                        let dict = match head.dict {
                            0 => None,
                            id => Some(fs.dict(id).ok_or(Error::Corrupt("frame names an unknown dictionary"))?),
                        };
                        new_dctx(dict)?
                    }
                },
            };
            let dctx = &mut owned;
            dctx.reset(zstd_safe::ResetDirective::SessionOnly).map_err(zerr)?;
            let mut out = vec![0u8; if need.is_none() { raw_len } else { meta_len }];
            let mut input = zstd_safe::InBuffer::around(packed);
            let mut pos = 0;
            let mut fill = |target: usize, out: &mut Vec<u8>, pos: &mut usize, input: &mut zstd_safe::InBuffer| -> Result<()> {
                while *pos < target {
                    let before = (*pos, input.pos());
                    let mut ob = zstd_safe::OutBuffer::around_pos(&mut out[..target], *pos);
                    dctx.decompress_stream(&mut ob, input).map_err(zerr)?;
                    *pos = ob.pos();
                    if (*pos, input.pos()) == before {
                        return Err(Error::Corrupt("zstd frame ends early"));
                    }
                }
                Ok(())
            };
            fill(meta_len, &mut out, &mut pos, &mut input)?;
            let meta = meta_of(head, &out)?;
            let target = need.map_or(raw_len, |n| n(&meta).clamp(meta_len, raw_len));
            // Standard buffered streaming permits relocation; our contexts do
            // not enable zstd's experimental stable-output-buffer mode.
            out.resize(target, 0);
            fill(target, &mut out, &mut pos, &mut input)?;
            let mut rc = fs.read_state().ctx.lock().unwrap();
            let idle = rc.dctx.entry(head.dict).or_default();
            if idle.len() < 8 {
                idle.push(owned);
            }
            (meta, out)
        }
        _ => return Err(Error::Corrupt("unknown codec")),
    };
    if body.len() == raw_len && head.digest.is_some_and(|d| digest16(&body) != d) {
        return Err(Error::Corrupt("frame body digest mismatch"));
    }
    Ok((meta, body))
}

#[cfg(test)]
#[path = "reader/tests.rs"]
mod tests;

/// Frame `i` decoded through at least the bytes `need` asks for.
pub(crate) fn decoded_with<F: Frames + ?Sized>(fs: &F, i: usize, need: Option<&dyn Fn(&Meta) -> usize>) -> Result<Arc<Decoded>> {
    let f = fs.loc(i);
    let state = fs.read_state();
    let cached = state.ctx.lock().unwrap().cache.get(&f.first_seq).cloned();
    if let Some(d) = cached {
        let want = need.map_or(f.raw_len as usize, |n| n(&d.meta).min(f.raw_len as usize));
        if d.body.len() >= want {
            return Ok(d);
        }
    }
    let payload = read_payload(fs.file(f.file), &f)?;
    let head = check_head(&f, &payload)?;
    let (meta, body) = decode_payload(fs, &payload, &head, need)?;
    let d = Arc::new(Decoded { meta, body });
    if state.budget > 0 && d.cost() <= state.budget {
        let mut rc = state.ctx.lock().unwrap();
        match rc.cache.insert(f.first_seq, d.clone()) {
            Some(old) => rc.cached_bytes -= old.cost(),
            None => rc.order.push_back(f.first_seq),
        }
        rc.cached_bytes += d.cost();
        while rc.cached_bytes > state.budget {
            let Some(k) = rc.order.pop_front() else { break };
            if let Some(old) = rc.cache.remove(&k) {
                rc.cached_bytes -= old.cost();
            }
        }
    }
    Ok(d)
}

pub(crate) fn decoded<F: Frames + ?Sized>(fs: &F, i: usize, want: usize) -> Result<Arc<Decoded>> {
    if want == usize::MAX { decoded_with(fs, i, None) } else { decoded_with(fs, i, Some(&|_: &Meta| want)) }
}

/// Literal bytes `at..at+len` of the text area of the frame starting at `first_seq`.
fn lit<F: Frames + ?Sized>(fs: &F, first_seq: u64, at: usize, len: usize) -> Result<Vec<u8>> {
    let i = fs.frame_index(first_seq)?;
    if fs.loc(i).first_seq != first_seq {
        return Err(Error::Corrupt("reference does not name a frame"));
    }
    let d = decoded_with(fs, i, Some(&|m: &Meta| m.text_start[0] as usize + at + len))?;
    let base = d.meta.text_start[0] as usize + at;
    let end = *d.meta.text_start.last().unwrap() as usize;
    if base + len > end {
        return Err(Error::Corrupt("reference outside the target frame"));
    }
    Ok(d.body[base..base + len].to_vec())
}

/// Bytes `start..end` (clamped) of record `r` in frame `i`, resolving references.
pub(crate) fn text_range<F: Frames + ?Sized>(fs: &F, i: usize, r: usize, start: usize, end: usize) -> Result<Vec<u8>> {
    let span = |m: &Meta| {
        let len = m.records[r].text_len as usize;
        (start.min(len), end.max(start).min(len))
    };
    // Body bytes needed: the overlapped part of this record's literal bytes.
    let need = |m: &Meta| -> usize {
        let (a, b) = span(m);
        let (p0, p1) = m.pieces[r];
        if p0 == p1 {
            return m.text_start[r] as usize + b;
        }
        let (mut pos, mut lit, mut need) = (0usize, m.text_start[r] as usize, m.text_start[r] as usize);
        for p in &m.piece_list[p0 as usize..p1 as usize] {
            let l = p.len() as usize;
            if let Piece::Lit(_) = p {
                if pos < b && pos + l > a {
                    need = lit + (b.min(pos + l) - pos);
                }
                lit += l;
            }
            pos += l;
        }
        need
    };
    let d = decoded_with(fs, i, Some(&need))?;
    let m = &d.meta;
    let (a, b) = span(m);
    let (p0, p1) = m.pieces[r];
    if p0 == p1 {
        let s = m.text_start[r] as usize;
        return Ok(d.body[s + a..s + b].to_vec());
    }
    let first_seq = fs.loc(i).first_seq;
    let mut out = Vec::with_capacity(b - a);
    let (mut pos, mut lit_at) = (0usize, m.text_start[r] as usize);
    for p in &m.piece_list[p0 as usize..p1 as usize] {
        let l = p.len() as usize;
        let (x, y) = (a.max(pos), b.min(pos + l));
        if x < y {
            match *p {
                Piece::Lit(_) => out.extend_from_slice(&d.body[lit_at + x - pos..lit_at + y - pos]),
                Piece::Ref { frame_delta, at, .. } => {
                    let target = first_seq.checked_sub(frame_delta).ok_or(Error::Corrupt("reference before the first frame"))?;
                    out.extend_from_slice(&lit(fs, target, at as usize + x - pos, y - x)?);
                }
            }
        }
        if let Piece::Lit(_) = p {
            lit_at += l;
        }
        pos += l;
        if pos >= b {
            break;
        }
    }
    Ok(out)
}

/// Decode one whole frame from its file without the cache or reference
/// resolution (compaction reads its sources this way).
pub(crate) fn read_frame_body(
    file: &File,
    f: &Loc,
    dicts: &BTreeMap<u32, Vec<u8>>,
    dctx: &mut HashMap<u32, zstd_safe::DCtx<'static>>,
    cipher: Option<&crate::crypto::FrameCipher>,
    require_sealed: bool,
) -> Result<(Meta, Vec<u8>)> {
    let payload = read_payload(file, f)?;
    let head = check_head(f, &payload)?;
    if require_sealed && !head.encrypted {
        return Err(Error::Crypto("plaintext frame in strict sealed store"));
    }
    let opened;
    let packed: &[u8] = if head.encrypted {
        opened = cipher.ok_or(Error::KeyRequired)?.open_frame(&payload[..head.body_at as usize], &payload[head.body_at as usize..])?;
        &opened
    } else {
        &payload[head.body_at as usize..]
    };
    let body = match head.codec {
        CODEC_RAW => packed.to_vec(),
        CODEC_LZ4 => lz4_flex::block::decompress(packed, head.raw_len as usize).map_err(|_| Error::Corrupt("lz4 frame"))?,
        CODEC_ZSTD => {
            let d = match dctx.entry(head.dict) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    let dict = match head.dict {
                        0 => None,
                        id => Some(dicts.get(&id).ok_or(Error::Corrupt("frame names an unknown dictionary"))?.as_slice()),
                    };
                    v.insert(new_dctx(dict)?)
                }
            };
            let mut out = Vec::with_capacity(head.raw_len as usize);
            d.decompress(&mut out, packed).map_err(zerr)?;
            out
        }
        _ => return Err(Error::Corrupt("unknown codec")),
    };
    if body.len() != head.raw_len as usize || head.digest.is_some_and(|d| d != digest16(&body)) {
        return Err(Error::Corrupt("frame body mismatch"));
    }
    let meta = meta_of(&head, &body)?;
    Ok((meta, body))
}
