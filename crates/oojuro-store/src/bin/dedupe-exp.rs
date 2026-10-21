//! Experiment: content-defined dedupe across frames with one-hop references.
//!
//!   dedupe-exp <corpus.rec> <sample_bytes> <frame_bytes> <avg_chunk|0> <level> <dict_bytes>
//!
//! Records are packed in append order into frames of `frame_bytes` *literal*
//! bytes. With `avg_chunk > 0` each record is cut by FastCDC; a chunk whose
//! hash was stored literally in an earlier frame becomes a reference
//! (frame delta, literal offset, length) into that frame. References always
//! point at literal bytes, so reads are one hop. Frames are zstd(level) with a
//! trained dictionary. Every record is rebuilt and checked against BLAKE3.
//! Fetches: QUERIES (400) x 20 random records, 2 KiB at a random offset,
//! through an LRU decoded-frame cache of CACHE bytes (default 4 MiB).
//! Env GROUP=kind packs each kind into its own frames with a dictionary per kind.
//! Env PREFIX=sim lets a frame compress against the raw body of the earlier
//! I-frame it references most (zstd prefix, no dictionary) when that is >= 3%
//! smaller; such P-frames are never used as prefixes, so a read decodes at most
//! one extra frame.

use oojuro_store::corpus::{CorpusRecord, read_corpus};
use oojuro_store::format::{get_varint, put_varint};
use oojuro_store::util::{Rng, percentile};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Copy)]
enum Piece {
    Lit { at: u32, len: u32 },
    Ref { frame: u32, at: u32, len: u32 },
}

struct Frame {
    /// Index (append order) of the frame's first record.
    first: usize,
    packed: Vec<u8>,
    raw_len: usize,
    meta_len: usize,
    dict: usize,
    /// P-frame: compressed against this earlier I-frame's raw body as a zstd prefix.
    prefix: Option<u32>,
}

fn zstd_prefix_compress(level: i32, body: &[u8], prefix: &[u8]) -> Vec<u8> {
    use zstd::zstd_safe;
    let mut c = zstd_safe::CCtx::create();
    c.set_parameter(zstd_safe::CParameter::CompressionLevel(level)).unwrap();
    c.set_parameter(zstd_safe::CParameter::WindowLog(20)).unwrap();
    c.ref_prefix(prefix).unwrap();
    let mut out = Vec::with_capacity(zstd_safe::compress_bound(body.len()));
    c.compress2(&mut out, body).unwrap();
    out
}

fn zstd_prefix_decompress(packed: &[u8], raw_len: usize, prefix: &[u8]) -> Vec<u8> {
    use zstd::zstd_safe;
    let mut d = zstd_safe::DCtx::create();
    d.ref_prefix(prefix).unwrap();
    let mut out = Vec::with_capacity(raw_len);
    d.decompress(&mut out, packed).unwrap();
    out
}

fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn train(recs: &[&CorpusRecord], bytes: usize) -> Vec<u8> {
    // TRAIN_FRAC < 1 trains on the first fraction only (out-of-sample test).
    let frac: f64 = env("TRAIN_FRAC", 1.0);
    let n = ((recs.len() as f64) * frac).ceil() as usize;
    let texts: Vec<Vec<u8>> = recs[..n.min(recs.len())].iter().map(|r| r.text.clone()).collect();
    oojuro_store::store::train_dict(&texts, bytes).unwrap_or_default()
}

struct Reader<'a> {
    frames: &'a [Frame],
    dctx: Vec<zstd::bulk::Decompressor<'a>>,
    plain: zstd::bulk::Decompressor<'a>,
    cache: HashMap<u32, Rc<Vec<u8>>>,
    order: VecDeque<u32>,
    bytes: usize,
    budget: usize,
    decodes: usize,
}

impl Reader<'_> {
    fn body(&mut self, f: u32) -> Rc<Vec<u8>> {
        if let Some(b) = self.cache.get(&f) {
            return b.clone();
        }
        if let Some(p) = self.frames[f as usize].prefix {
            let pb = self.body(p);
            self.decodes += 1;
            let fr = &self.frames[f as usize];
            let b = Rc::new(zstd_prefix_decompress(&fr.packed, fr.raw_len, &pb));
            return self.remember(f, b);
        }
        self.decodes += 1;
        let fr = &self.frames[f as usize];
        let b = if fr.dict == usize::MAX {
            self.plain.decompress(&fr.packed, fr.raw_len).unwrap()
        } else {
            self.dctx[fr.dict].decompress(&fr.packed, fr.raw_len).unwrap()
        };
        let b = Rc::new(b);
        self.remember(f, b)
    }

    fn remember(&mut self, f: u32, b: Rc<Vec<u8>>) -> Rc<Vec<u8>> {
        if self.budget > 0 {
            self.cache.insert(f, b.clone());
            self.order.push_back(f);
            self.bytes += b.len();
            while self.bytes > self.budget {
                let k = self.order.pop_front().unwrap();
                self.bytes -= self.cache.remove(&k).unwrap().len();
            }
        }
        b
    }

    /// Bytes [s, e) of record `index` in `frame`, resolving references.
    fn read(&mut self, frame: u32, index: u32, s: usize, e: usize) -> Vec<u8> {
        let body = self.body(frame);
        let mut at = 0;
        let mut lit_at = 0usize;
        let mut ps: Vec<Piece> = Vec::new();
        for m in 0..=index {
            let n = get_varint(&body, &mut at).unwrap();
            for _ in 0..n {
                let tag = get_varint(&body, &mut at).unwrap();
                let len = (tag >> 1) as u32;
                let p = if tag & 1 == 0 {
                    let p = Piece::Lit { at: lit_at as u32, len };
                    lit_at += len as usize;
                    p
                } else {
                    let fd = get_varint(&body, &mut at).unwrap() as u32;
                    let off = get_varint(&body, &mut at).unwrap() as u32;
                    Piece::Ref { frame: frame - fd, at: off, len }
                };
                if m == index {
                    ps.push(p);
                }
            }
        }
        let meta_len = self.frames[frame as usize].meta_len;
        let mut out = Vec::new();
        let mut pos = 0usize;
        for p in ps {
            let len = match p {
                Piece::Lit { len, .. } | Piece::Ref { len, .. } => len as usize,
            };
            let (a, b) = (s.max(pos), e.min(pos + len));
            if a < b {
                match p {
                    Piece::Lit { at, .. } => {
                        let base = meta_len + at as usize;
                        out.extend_from_slice(&body[base + a - pos..base + b - pos]);
                    }
                    Piece::Ref { frame: tf, at, .. } => {
                        let tb = self.body(tf);
                        let base = self.frames[tf as usize].meta_len + at as usize;
                        out.extend_from_slice(&tb[base + a - pos..base + b - pos]);
                    }
                }
            }
            pos += len;
            if pos >= e {
                break;
            }
        }
        out
    }
}

fn push_lit(ps: &mut Vec<Piece>, lit: &mut Vec<u8>, b: &[u8]) {
    if b.is_empty() {
        return;
    }
    let start = lit.len() as u32;
    lit.extend_from_slice(b);
    if let Some(Piece::Lit { at, len }) = ps.last_mut() {
        if *at + *len == start {
            *len += b.len() as u32;
            return;
        }
    }
    ps.push(Piece::Lit { at: start, len: b.len() as u32 });
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = &a[1];
    let sample: usize = a[2].parse().unwrap();
    let frame_bytes: usize = a[3].parse().unwrap();
    let avg: u32 = a[4].parse().unwrap();
    let level: i32 = a[5].parse().unwrap();
    let dict_bytes: usize = a[6].parse().unwrap();
    let by_kind = std::env::var("GROUP").is_ok_and(|g| g == "kind");
    let mut recs = read_corpus(Path::new(path)).unwrap();
    if sample > 0 {
        let mut total = 0;
        recs.retain(|r| {
            let keep = total < sample;
            total += r.text.len();
            keep
        });
    }
    let raw: usize = recs.iter().map(|r| r.text.len()).sum();
    let mut order: Vec<usize> = (0..recs.len()).collect();
    if by_kind {
        order.sort_by_key(|&i| (recs[i].kind, i));
    }
    // Dictionaries: one global, or one per kind (kinds too small get none).
    let t = Instant::now();
    let mut dicts: Vec<Vec<u8>> = Vec::new();
    let mut dict_of_kind = vec![usize::MAX; 8];
    if dict_bytes > 0 {
        if by_kind {
            for k in 0..8u8 {
                let of: Vec<&CorpusRecord> = recs.iter().filter(|r| r.kind == k).collect();
                let d = train(&of, dict_bytes);
                if !d.is_empty() {
                    dicts.push(d);
                    dict_of_kind[k as usize] = dicts.len() - 1;
                }
            }
        } else {
            let d = train(&recs.iter().collect::<Vec<_>>(), dict_bytes);
            if !d.is_empty() {
                dicts.push(d);
                dict_of_kind = vec![0; 8];
            }
        }
    }
    let train_s = t.elapsed().as_secs_f64();
    let cdicts: Vec<zstd::dict::EncoderDictionary> = dicts.iter().map(|d| zstd::dict::EncoderDictionary::copy(d, level)).collect();
    let ddicts: Vec<zstd::dict::DecoderDictionary> = dicts.iter().map(|d| zstd::dict::DecoderDictionary::copy(d)).collect();
    let mut plain = zstd::bulk::Compressor::new(level).unwrap();
    let mut comps: Vec<zstd::bulk::Compressor> = cdicts.iter().map(|d| zstd::bulk::Compressor::with_prepared_dictionary(d).unwrap()).collect();

    let t = Instant::now();
    let mut table: HashMap<u64, (u32, u32)> = HashMap::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut loc: Vec<(u32, u32)> = vec![(0, 0); recs.len()];
    let (mut ref_bytes, mut refs, mut registered) = (0usize, 0usize, 0usize);
    let use_prefix = std::env::var("PREFIX").is_ok_and(|p| p == "sim");
    let mut raw_bodies: Vec<Vec<u8>> = Vec::new();
    let mut last_iframe: Option<u32> = None;
    let mut pframes = 0usize;
    let mut at = 0;
    while at < order.len() {
        let fidx = frames.len() as u32;
        let kind = recs[order[at]].kind;
        let first_rec = order[at];
        let mut members: Vec<Vec<Piece>> = Vec::new();
        let mut lit: Vec<u8> = Vec::new();
        let mut pending: Vec<(u64, u32, u32)> = Vec::new();
        while at < order.len() && (members.is_empty() || lit.len() < frame_bytes) && (!by_kind || recs[order[at]].kind == kind) {
            let i = order[at];
            let text = &recs[i].text;
            let mut ps: Vec<Piece> = Vec::new();
            if avg == 0 || text.len() < (avg / 4).max(64) as usize {
                push_lit(&mut ps, &mut lit, text);
            } else {
                for c in fastcdc::v2020::FastCDC::new(text, (avg / 4).max(64), avg, avg * 8) {
                    let bytes = &text[c.offset..c.offset + c.length];
                    let h = u64::from_le_bytes(blake3::hash(bytes).as_bytes()[..8].try_into().unwrap());
                    match table.get(&h) {
                        Some(&(f, off)) => {
                            ps.push(Piece::Ref { frame: f, at: off, len: c.length as u32 });
                            refs += 1;
                            ref_bytes += c.length;
                        }
                        None => {
                            pending.push((h, fidx, lit.len() as u32));
                            push_lit(&mut ps, &mut lit, bytes);
                        }
                    }
                }
            }
            loc[i] = (fidx, members.len() as u32);
            members.push(ps);
            at += 1;
        }
        // Meta: per member, piece count then pieces (literal offsets implicit).
        let mut body = Vec::new();
        for ps in &members {
            put_varint(&mut body, ps.len() as u64);
            for p in ps {
                match *p {
                    Piece::Lit { len, .. } => put_varint(&mut body, (len as u64) << 1),
                    Piece::Ref { frame, at, len } => {
                        put_varint(&mut body, ((len as u64) << 1) | 1);
                        put_varint(&mut body, (fidx - frame) as u64);
                        put_varint(&mut body, at as u64);
                    }
                }
            }
        }
        let meta_len = body.len();
        body.extend_from_slice(&lit);
        let d = dict_of_kind[kind as usize];
        let mut packed = if d == usize::MAX { plain.compress(&body).unwrap() } else { comps[d].compress(&body).unwrap() };
        let mut prefix = None;
        if use_prefix && fidx > 0 {
            // Reference the I-frame this frame points into most; else the newest I-frame.
            let mut votes: HashMap<u32, usize> = HashMap::new();
            for ps in &members {
                for p in ps {
                    if let Piece::Ref { frame, len, .. } = *p {
                        let root = frames[frame as usize].prefix.unwrap_or(frame);
                        *votes.entry(root).or_default() += len as usize;
                    }
                }
            }
            let pick = votes.into_iter().max_by_key(|&(f, n)| (n, f)).map(|(f, _)| f).or(last_iframe);
            if let Some(r) = pick {
                let p = zstd_prefix_compress(level, &body, &raw_bodies[r as usize]);
                if p.len() < packed.len() * 97 / 100 {
                    packed = p;
                    prefix = Some(r);
                    pframes += 1;
                }
            }
        }
        if prefix.is_none() {
            last_iframe = Some(fidx);
        }
        raw_bodies.push(if use_prefix { body.clone() } else { Vec::new() });
        frames.push(Frame { first: first_rec, packed, raw_len: body.len(), meta_len, dict: d, prefix });
        for (h, f, off) in pending {
            table.entry(h).or_insert((f, off));
            registered += 1;
        }
    }
    let pack_s = t.elapsed().as_secs_f64();

    let mut rd = Reader {
        frames: &frames,
        dctx: ddicts.iter().map(|d| zstd::bulk::Decompressor::with_prepared_dictionary(d).unwrap()).collect(),
        plain: zstd::bulk::Decompressor::new().unwrap(),
        cache: HashMap::new(),
        order: VecDeque::new(),
        bytes: 0,
        budget: env("CACHE", 4usize << 20),
        decodes: 0,
    };
    // Gate: every record rebuilds exactly.
    let mut bad = 0;
    for (i, r) in recs.iter().enumerate() {
        let (f, x) = loc[i];
        bad += usize::from(blake3::hash(&rd.read(f, x, 0, usize::MAX)) != blake3::hash(&r.text));
    }
    assert_eq!(bad, 0, "dedupe roundtrip failed");
    // Fetch latency starting from an empty cache.
    rd.cache.clear();
    rd.order.clear();
    rd.bytes = 0;
    rd.decodes = 0;
    let mut rng = Rng::new(42);
    let mut per_query = Vec::new();
    let queries: usize = env("QUERIES", 400);
    for _ in 0..queries {
        let t = Instant::now();
        for _ in 0..20 {
            let i = rng.below(recs.len());
            let len = recs[i].text.len();
            let want = len.min(2048);
            let s = rng.below(len - want + 1);
            let (f, x) = loc[i];
            std::hint::black_box(rd.read(f, x, s, s + want));
        }
        per_query.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let packed: usize = frames.iter().map(|f| f.packed.len() + 12).sum();
    // Frames-only ratio over records past the training fraction.
    let cut = ((recs.len() as f64) * env("TRAIN_FRAC", 1.0)).ceil() as usize;
    let after_packed: usize = frames.iter().filter(|f| f.first >= cut).map(|f| f.packed.len() + 12).sum();
    let after_raw: usize = recs[cut.min(recs.len())..].iter().map(|r| r.text.len()).sum();
    let dict_total: usize = dicts.iter().map(|d| d.len()).sum();
    let table_bytes = registered * 12;
    println!(
        "{}",
        json!({"mode":"dedupe-frames","corpus":Path::new(path).file_stem().unwrap().to_string_lossy(),"group":if by_kind {"kind"} else {"append"},
            "frame_bytes":frame_bytes,"avg_chunk":avg,"prefix":use_prefix,"pframes":pframes,"level":level,"dict_bytes":dict_total,"raw":raw,"frames":frames.len(),
            "refs":refs,"ref_bytes_fraction":ref_bytes as f64/raw as f64,"stored":packed+dict_total,
            "ratio":(packed+dict_total) as f64/raw as f64,"chunk_table_bytes":table_bytes,
            "ratio_with_table":(packed+dict_total+table_bytes) as f64/raw as f64,
            "pack_mbps":raw as f64/1e6/pack_s,"frames_only_ratio":packed as f64/raw as f64,
            "train_frac":env("TRAIN_FRAC", 1.0),"after_ratio":if after_raw > 0 {after_packed as f64/after_raw as f64} else {f64::NAN},"train_s":train_s,"cache_bytes":rd.budget,
            "decodes_per_fetch":rd.decodes as f64/(queries*20) as f64,
            "query20_us_p50":percentile(&mut per_query.clone(),0.5),"query20_us_p99":percentile(&mut per_query,0.99),"verify_mismatches":bad})
    );
}
