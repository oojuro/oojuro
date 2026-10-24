//! Frame packing shared by compaction and run files: records are cut into
//! frames of `frame_bytes` literal bytes; a FastCDC chunk already stored
//! literally in an earlier frame becomes a reference to those bytes (one hop,
//! never to another reference; consecutive chunks whose targets are adjacent
//! in the same frame share one reference); frames are then compressed and verified in
//! parallel batches, in an order and with bytes independent of the thread
//! count.

use crate::format::*;
use crate::reader::{new_dctx, new_dctx_ref, zerr};
use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use zstd::zstd_safe;

pub(crate) fn new_cctx(level: i32, dict: Option<&[u8]>) -> Result<zstd_safe::CCtx<'static>> {
    use zstd_safe::CParameter as P;
    let mut c = zstd_safe::CCtx::create();
    c.set_parameter(P::CompressionLevel(level)).map_err(zerr)?;
    c.set_parameter(P::ContentSizeFlag(false)).map_err(zerr)?;
    c.set_parameter(P::DictIdFlag(false)).map_err(zerr)?;
    c.set_parameter(P::Format(zstd_safe::FrameFormat::Magicless)).map_err(zerr)?;
    if let Some(d) = dict {
        c.load_dictionary(d).map_err(zerr)?;
    }
    Ok(c)
}

/// Wrap a body as a frame payload (zstd when that is smaller, else raw).
pub(crate) fn pack_body(
    cctx: Option<&mut zstd_safe::CCtx<'static>>,
    dict: u32,
    first_seq: u64,
    count: usize,
    body: &[u8],
    meta_len: usize,
    digest: bool,
) -> Result<Vec<u8>> {
    if body.len() > MAX_FRAME {
        return Err(Error::Invalid("batch exceeds the 64 MiB frame limit"));
    }
    let mut packed = None;
    if let Some(c) = cctx {
        let mut z = Vec::with_capacity(zstd_safe::compress_bound(body.len()));
        c.compress2(&mut z, body).map_err(zerr)?;
        if z.len() < body.len() {
            packed = Some(z);
        }
    }
    let codec = if packed.is_some() { CODEC_ZSTD } else { CODEC_RAW };
    let head = FrameHead {
        codec,
        dict: if codec == CODEC_ZSTD { dict } else { 0 },
        first_seq,
        count: count as u32,
        raw_len: body.len() as u32,
        meta_len: meta_len as u32,
        digest: digest.then(|| digest16(body)),
        encrypted: false,
        body_at: 0,
    };
    let data = packed.as_deref().unwrap_or(body);
    let mut payload = Vec::with_capacity(data.len() + 40);
    put_head(&mut payload, &head);
    payload.extend_from_slice(data);
    Ok(payload)
}

/// Seal a finished payload: the packed body becomes `nonce | ciphertext | tag`, the head (with the
/// encrypted flag and without a digest, which would give away equal bodies) is the associated data.
pub(crate) fn encrypt_payload(cipher: &crate::crypto::FrameCipher, payload: &[u8]) -> Result<Vec<u8>> {
    let mut head = get_head(payload)?;
    let packed = &payload[head.body_at as usize..];
    head.digest = None;
    head.encrypted = true;
    let mut out = Vec::with_capacity(packed.len() + 48 + crate::crypto::SEAL_OVERHEAD);
    put_head(&mut out, &head);
    let sealed = cipher.seal_frame(&out, packed);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Chunk hash (128-bit BLAKE3 prefix) -> (frame first_seq, literal offset).
type ChunkTable = HashMap<u128, (u64, u32)>;

fn chunk_hash(b: &[u8]) -> u128 {
    u128::from_le_bytes(blake3::hash(b).as_bytes()[..16].try_into().unwrap())
}

fn min_chunk(avg: u32) -> u32 {
    (avg / 4).max(64)
}

/// One record to pack.
pub(crate) struct Row {
    pub meta: RecordMeta,
    pub key: Vec<u8>,
    pub attrs: Vec<u8>,
    pub text: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct PackOpts {
    pub level: i32,
    pub frame_bytes: usize,
    pub dedupe_chunk: u32,
    pub threads: usize,
    pub cipher: Option<std::sync::Arc<crate::crypto::FrameCipher>>,
}

/// A compressed, verified frame ready to be written.
pub(crate) struct Packed {
    pub payload: Vec<u8>,
    pub count: u32,
    pub raw_len: u32,
    pub lo: i64,
    pub hi: i64,
    pub first_seq: u64,
}

struct Planned {
    body: Vec<u8>,
    meta_len: usize,
    count: usize,
    lo: i64,
    hi: i64,
    first_seq: u64,
}

pub(crate) struct Packer<'d> {
    opts: PackOpts,
    dict_id: u32,
    dict: Option<&'d [u8]>,
    table: ChunkTable,
    /// First sequence number of the frame being filled.
    seq: u64,
    cur: Vec<(Row, Vec<Piece>, Vec<u8>)>,
    cur_lit: usize,
    pending: Vec<(u128, u32)>,
    planned: Vec<Planned>,
    pub records: u64,
    pub raw_total: u64,
    pub ref_bytes: u64,
    /// Encode with this prepared shared dictionary (its bytes are `dict`).
    pub shared: Option<crate::run::SharedDict>,
}

impl<'d> Packer<'d> {
    pub fn new(opts: PackOpts, first_seq: u64, dict_id: u32, dict: Option<&'d [u8]>) -> Self {
        Self {
            opts,
            dict_id,
            dict,
            table: ChunkTable::new(),
            seq: first_seq,
            cur: Vec::new(),
            cur_lit: 0,
            pending: Vec::new(),
            planned: Vec::new(),
            records: 0,
            raw_total: 0,
            ref_bytes: 0,
            shared: None,
        }
    }

    /// Make the literal chunks of an existing frame referenceable.
    pub fn register(&mut self, first_seq: u64, m: &Meta, body: &[u8]) {
        let avg = self.opts.dedupe_chunk;
        if avg == 0 {
            return;
        }
        let base = m.text_start[0] as usize;
        let table = &mut self.table;
        let mut add = |run: &[u8], at: usize| {
            if run.len() < min_chunk(avg) as usize {
                return;
            }
            for c in fastcdc::v2020::FastCDC::new(run, min_chunk(avg), avg, avg * 8) {
                table.entry(chunk_hash(&run[c.offset..c.offset + c.length])).or_insert((first_seq, (at - base + c.offset) as u32));
            }
        };
        for r in 0..m.records.len() {
            let (p0, p1) = m.pieces[r];
            let mut lit = m.text_start[r] as usize;
            if p0 == p1 {
                add(&body[lit..m.text_start[r + 1] as usize], lit);
                continue;
            }
            for p in &m.piece_list[p0 as usize..p1 as usize] {
                if let Piece::Lit(l) = *p {
                    add(&body[lit..lit + l as usize], lit);
                    lit += l as usize;
                }
            }
        }
    }

    pub fn push(&mut self, row: Row, emit: &mut dyn FnMut(Packed) -> Result<()>) -> Result<()> {
        if !self.cur.is_empty() && self.cur_lit >= self.opts.frame_bytes {
            self.seal();
            if self.planned.len() >= self.opts.threads.max(1) * 8 {
                self.flush(emit)?;
            }
        }
        let avg = self.opts.dedupe_chunk;
        let (mut pieces, mut lit) = (Vec::new(), Vec::new());
        if avg > 0 && row.text.len() >= min_chunk(avg) as usize {
            for c in fastcdc::v2020::FastCDC::new(&row.text, min_chunk(avg), avg, avg * 8) {
                let bytes = &row.text[c.offset..c.offset + c.length];
                let h = chunk_hash(bytes);
                match self.table.get(&h) {
                    Some(&(f, off)) => {
                        let fd = self.seq - f;
                        match pieces.last_mut() {
                            // the chunk continues the previous reference's target bytes
                            // (adjacent literal bytes of the same frame): one longer reference
                            Some(Piece::Ref { frame_delta, at, len }) if *frame_delta == fd && *at + *len == off => {
                                *len += c.length as u32
                            }
                            _ => pieces.push(Piece::Ref { frame_delta: fd, at: off, len: c.length as u32 }),
                        }
                        self.ref_bytes += c.length as u64;
                    }
                    None => {
                        self.pending.push((h, (self.cur_lit + lit.len()) as u32));
                        lit.extend_from_slice(bytes);
                        match pieces.last_mut() {
                            Some(Piece::Lit(l)) => *l += c.length as u32,
                            _ => pieces.push(Piece::Lit(c.length as u32)),
                        }
                    }
                }
            }
        }
        if !pieces.iter().any(|p| matches!(p, Piece::Ref { .. })) {
            pieces.clear();
            lit = row.text.clone();
        }
        self.cur_lit += lit.len();
        self.records += 1;
        self.raw_total += row.text.len() as u64;
        self.cur.push((row, pieces, lit));
        Ok(())
    }

    pub fn finish(&mut self, emit: &mut dyn FnMut(Packed) -> Result<()>) -> Result<()> {
        if !self.cur.is_empty() {
            self.seal();
        }
        self.flush(emit)
    }

    /// Close the frame being filled; its literal chunks become referenceable.
    fn seal(&mut self) {
        let recs: Vec<BodyRecord> = self
            .cur
            .iter()
            .map(|(r, pieces, lit)| BodyRecord {
                kind: r.meta.kind,
                valid_at: r.meta.valid_at,
                recorded_at: r.meta.recorded_at,
                key: &r.key,
                attrs: &r.attrs,
                text_len: r.text.len(),
                pieces,
                lit,
            })
            .collect();
        let (body, meta_len) = encode_records(&recs);
        let (lo, hi) = recs.iter().fold((i64::MAX, i64::MIN), |(lo, hi), r| (lo.min(r.valid_at), hi.max(r.valid_at)));
        let count = recs.len();
        drop(recs);
        self.planned.push(Planned { body, meta_len, count, lo, hi, first_seq: self.seq });
        for (h, off) in self.pending.drain(..) {
            self.table.entry(h).or_insert((self.seq, off));
        }
        self.seq += count as u64;
        self.cur.clear();
        self.cur_lit = 0;
    }

    /// Compress and verify the planned frames in parallel, emit them in order.
    fn flush(&mut self, emit: &mut dyn FnMut(Packed) -> Result<()>) -> Result<()> {
        if self.planned.is_empty() {
            return Ok(());
        }
        let planned = std::mem::take(&mut self.planned);
        let threads = self.opts.threads.max(1).min(planned.len());
        let (level, dict, dict_id) = (self.opts.level, self.dict, self.dict_id);
        let cipher = self.opts.cipher.clone();
        let shared = self.shared.clone();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<Mutex<Option<Result<Vec<u8>>>>> = planned.iter().map(|_| Mutex::new(None)).collect();
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    let ctx = match &shared {
                        Some(sd) => new_cctx(level, None).and_then(|mut c| {
                            c.ref_cdict(sd.cdict(level)).map_err(zerr)?;
                            Ok((c, new_dctx_ref(sd.ddict)?))
                        }),
                        None => new_cctx(level, dict).and_then(|c| Ok((c, new_dctx(dict)?))),
                    };
                    let (mut cctx, mut vctx) = match ctx {
                        Ok(c) => c,
                        Err(e) => {
                            *results[0].lock().unwrap() = Some(Err(e));
                            return;
                        }
                    };
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = planned.get(i) else { break };
                        let r = (|| {
                            let mut payload = pack_body(Some(&mut cctx), dict_id, p.first_seq, p.count, &p.body, p.meta_len, true)?;
                            let head = get_head(&payload)?;
                            // Verify the frame decodes to the exact body before it is written.
                            if head.codec == CODEC_ZSTD {
                                let mut check = Vec::with_capacity(p.body.len());
                                vctx.decompress(&mut check, &payload[head.body_at as usize..]).map_err(zerr)?;
                                if check != p.body {
                                    return Err(Error::Codec("packed frame failed verification"));
                                }
                            }
                            if let Some(c) = &cipher {
                                payload = encrypt_payload(c, &payload)?;
                            }
                            payload.shrink_to_fit();
                            Ok(payload)
                        })();
                        *results[i].lock().unwrap() = Some(r);
                    }
                });
            }
        });
        for (p, r) in planned.iter().zip(results) {
            let payload = r.into_inner().unwrap().ok_or(Error::Codec("packing worker failed"))??;
            emit(Packed { payload, count: p.count as u32, raw_len: p.body.len() as u32, lo: p.lo, hi: p.hi, first_seq: p.first_seq })?;
        }
        Ok(())
    }
}
