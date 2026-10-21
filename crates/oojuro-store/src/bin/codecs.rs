//! Codec leaderboard over normalized agent corpora (feature `codecs`).
//!
//!   codecs dump   <corpus.rec> <out.txt>
//!   codecs whole  <corpus.rec> <sample_bytes> <codec>...
//!   codecs blocks <corpus.rec> <sample_bytes> <block_bytes> <dict_bytes> <codec>...
//!
//! Every mode verifies a byte-exact roundtrip (BLAKE3) and prints one JSON line
//! per codec. Timings are single-threaded wall clock. `blocks` packs whole
//! records in append order into independent blocks of at least `block_bytes`
//! raw bytes (0 = one record per block) and measures cold excerpt fetches:
//! 400 queries x 20 random records, 2 KiB at a random offset, no decode cache.

use oojuro_store::corpus::{CorpusRecord, read_corpus};
use oojuro_store::util::{Rng, percentile};
use serde_json::json;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

#[derive(Clone, Debug)]
enum Spec {
    Zstd { level: i32, wlog: u32, ldm: bool },
    Lz4,
    Lz4Hc(i32),
    Brotli { q: u32, w: u32 },
    Xz { preset: u32 },
    Bzip2(u32),
    Deflate(i32),
    Snappy,
    Ppmd7 { order: u32, mem_mb: u32 },
    Ppmd8 { order: u32, mem_mb: u32 },
    Fsst,
}

fn parse(s: &str) -> Spec {
    let p: Vec<&str> = s.split(':').collect();
    let n = |i: usize, d: i64| p.get(i).and_then(|x| x.trim_end_matches('e').parse().ok()).unwrap_or(d);
    match p[0] {
        "zstd" => Spec::Zstd {
            level: n(1, 3) as i32,
            wlog: p.iter().find_map(|x| x.strip_prefix('w')).map(|x| x.parse().unwrap()).unwrap_or(0),
            ldm: p.contains(&"ldm"),
        },
        "lz4" => Spec::Lz4,
        "lz4hc" => Spec::Lz4Hc(n(1, 9) as i32),
        "brotli" => Spec::Brotli { q: n(1, 9) as u32, w: n(2, 22) as u32 },
        "xz" => Spec::Xz {
            preset: n(1, 6) as u32 | if p.get(1).is_some_and(|x| x.ends_with('e')) { liblzma::stream::PRESET_EXTREME } else { 0 },
        },
        "bzip2" => Spec::Bzip2(n(1, 9) as u32),
        "deflate" => Spec::Deflate(n(1, 6) as i32),
        "snappy" => Spec::Snappy,
        "ppmd7" => Spec::Ppmd7 { order: n(1, 6) as u32, mem_mb: n(2, 16) as u32 },
        "ppmd8" => Spec::Ppmd8 { order: n(1, 6) as u32, mem_mb: n(2, 16) as u32 },
        "fsst" => Spec::Fsst,
        other => panic!("unknown codec {other}"),
    }
}

/// Shared side information (dictionary or symbol table) for block mode.
struct Side {
    zdict: Option<(zstd::dict::EncoderDictionary<'static>, zstd::dict::DecoderDictionary<'static>)>,
    raw_dict: Vec<u8>,
    fsst: Option<fsst::Compressor>,
}

struct Ctx<'a> {
    spec: Spec,
    side: &'a Side,
    zc: Option<zstd::bulk::Compressor<'a>>,
    zd: Option<zstd::stream::raw::Decoder<'a>>,
}

impl<'a> Ctx<'a> {
    fn new(spec: Spec, side: &'a Side) -> Self {
        let (mut zc, mut zd) = (None, None);
        if let Spec::Zstd { level, wlog, ldm } = spec {
            let mut c = match &side.zdict {
                Some((e, _)) => zstd::bulk::Compressor::with_prepared_dictionary(e).unwrap(),
                None => zstd::bulk::Compressor::new(level).unwrap(),
            };
            use zstd::zstd_safe::CParameter as P;
            c.set_parameter(P::CompressionLevel(level)).unwrap();
            if wlog > 0 {
                c.set_parameter(P::WindowLog(wlog)).unwrap();
            }
            if ldm {
                c.set_parameter(P::EnableLongDistanceMatching(true)).unwrap();
            }
            let mut d = match &side.zdict {
                Some((_, d)) => zstd::stream::raw::Decoder::with_prepared_dictionary(d).unwrap(),
                None => zstd::stream::raw::Decoder::new().unwrap(),
            };
            d.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(31)).unwrap();
            zc = Some(c);
            zd = Some(d);
        }
        Self { spec, side, zc, zd }
    }

    fn compress(&mut self, src: &[u8]) -> Vec<u8> {
        match self.spec {
            Spec::Zstd { .. } => self.zc.as_mut().unwrap().compress(src).unwrap(),
            Spec::Lz4 => {
                if self.side.raw_dict.is_empty() {
                    lz4_flex::block::compress(src)
                } else {
                    lz4_flex::block::compress_with_dict(src, &self.side.raw_dict)
                }
            }
            Spec::Lz4Hc(l) => lz4::block::compress(src, Some(lz4::block::CompressionMode::HIGHCOMPRESSION(l)), false).unwrap(),
            Spec::Brotli { q, w } => {
                let params = brotli::enc::BrotliEncoderParams {
                    quality: q as i32,
                    lgwin: w as i32,
                    large_window: w > 24,
                    size_hint: src.len(),
                    ..Default::default()
                };
                let mut out = Vec::new();
                brotli::BrotliCompress(&mut &src[..], &mut out, &params).unwrap();
                out
            }
            Spec::Xz { preset } => {
                let stream = liblzma::stream::Stream::new_easy_encoder(preset, liblzma::stream::Check::None).unwrap();
                let mut e = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
                e.write_all(src).unwrap();
                e.finish().unwrap()
            }
            Spec::Bzip2(l) => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(l));
                e.write_all(src).unwrap();
                e.finish().unwrap()
            }
            Spec::Deflate(l) => {
                let mut c = libdeflater::Compressor::new(libdeflater::CompressionLvl::new(l).unwrap());
                let mut out = vec![0u8; c.deflate_compress_bound(src.len())];
                let n = c.deflate_compress(src, &mut out).unwrap();
                out.truncate(n);
                out
            }
            Spec::Snappy => snap::raw::Encoder::new().compress_vec(src).unwrap(),
            Spec::Ppmd7 { order, mem_mb } => {
                let mut e = ppmd_rust::Ppmd7Encoder::new(Vec::new(), order, mem_mb << 20).unwrap();
                e.write_all(src).unwrap();
                e.finish(false).unwrap()
            }
            Spec::Ppmd8 { order, mem_mb } => {
                let mut e = ppmd_rust::Ppmd8Encoder::new(Vec::new(), order, mem_mb << 20, ppmd_rust::RestoreMethod::Restart).unwrap();
                e.write_all(src).unwrap();
                e.finish(false).unwrap()
            }
            Spec::Fsst => self.side.fsst.as_ref().unwrap().compress(src),
        }
    }

    /// Decode at least the first `upto` bytes (all when the codec cannot stop early).
    fn decompress(&mut self, src: &[u8], raw_len: usize, upto: usize) -> Vec<u8> {
        match self.spec {
            Spec::Zstd { .. } => {
                use zstd::stream::raw::Operation;
                let d = self.zd.as_mut().unwrap();
                d.reinit().unwrap();
                let mut out = vec![0u8; upto];
                let (mut r, mut w) = (0, 0);
                while w < upto {
                    let st = d.run_on_buffers(&src[r..], &mut out[w..]).unwrap();
                    r += st.bytes_read;
                    w += st.bytes_written;
                    if st.bytes_read == 0 && st.bytes_written == 0 {
                        break;
                    }
                }
                out.truncate(w);
                out
            }
            Spec::Lz4 => {
                if self.side.raw_dict.is_empty() {
                    lz4_flex::block::decompress(src, raw_len).unwrap()
                } else {
                    lz4_flex::block::decompress_with_dict(src, raw_len, &self.side.raw_dict).unwrap()
                }
            }
            Spec::Lz4Hc(_) => lz4::block::decompress(src, Some(raw_len as i32)).unwrap(),
            Spec::Brotli { .. } => {
                let mut out = Vec::with_capacity(raw_len);
                let mut r = brotli::Decompressor::new(src, 1 << 16);
                read_upto(&mut r, &mut out, upto);
                out
            }
            Spec::Xz { .. } => {
                let mut out = Vec::with_capacity(raw_len);
                let mut r = liblzma::read::XzDecoder::new(src);
                read_upto(&mut r, &mut out, upto);
                out
            }
            Spec::Bzip2(_) => {
                let mut out = Vec::with_capacity(raw_len);
                let mut r = bzip2::read::BzDecoder::new(src);
                read_upto(&mut r, &mut out, upto);
                out
            }
            Spec::Deflate(_) => {
                let mut out = vec![0u8; raw_len];
                let n = libdeflater::Decompressor::new().deflate_decompress(src, &mut out).unwrap();
                out.truncate(n);
                out
            }
            Spec::Snappy => snap::raw::Decoder::new().decompress_vec(src).unwrap(),
            Spec::Ppmd7 { order, mem_mb } => {
                let mut out = vec![0u8; upto];
                let mut r = ppmd_rust::Ppmd7Decoder::new(src, order, mem_mb << 20).unwrap();
                r.read_exact(&mut out).unwrap();
                out
            }
            Spec::Ppmd8 { order, mem_mb } => {
                let mut out = vec![0u8; upto];
                let mut r = ppmd_rust::Ppmd8Decoder::new(src, order, mem_mb << 20, ppmd_rust::RestoreMethod::Restart).unwrap();
                r.read_exact(&mut out).unwrap();
                out
            }
            Spec::Fsst => self.side.fsst.as_ref().unwrap().decompressor().decompress(src),
        }
    }
}

fn read_upto(r: &mut impl Read, out: &mut Vec<u8>, upto: usize) {
    let mut buf = [0u8; 1 << 16];
    while out.len() < upto {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

fn sample(records: Vec<CorpusRecord>, bytes: usize) -> Vec<CorpusRecord> {
    if bytes == 0 {
        return records;
    }
    let mut total = 0;
    let mut out = Vec::new();
    for r in records {
        if total >= bytes {
            break;
        }
        total += r.text.len();
        out.push(r);
    }
    out
}

fn corpus_name(p: &str) -> String {
    Path::new(p).file_stem().unwrap().to_string_lossy().into()
}

fn whole(path: &str, sample_bytes: usize, codecs: &[String]) {
    let recs = sample(read_corpus(Path::new(path)).unwrap(), sample_bytes);
    let text: Vec<u8> = recs.iter().flat_map(|r| r.text.iter().copied()).collect();
    let digest = blake3::hash(&text);
    for c in codecs {
        if let Some(inner) = c.strip_prefix("tok:") {
            whole_tokenized(path, &text, inner, c);
            continue;
        }
        let spec = parse(c);
        let side = Side {
            zdict: None,
            raw_dict: Vec::new(),
            fsst: matches!(spec, Spec::Fsst).then(|| train_fsst(&recs)),
        };
        let mut ctx = Ctx::new(spec, &side);
        let t = Instant::now();
        let packed = ctx.compress(&text);
        let ct = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let back = ctx.decompress(&packed, text.len(), text.len());
        let dt = t.elapsed().as_secs_f64();
        let ok = blake3::hash(&back) == digest;
        let mb = text.len() as f64 / 1e6;
        println!(
            "{}",
            json!({"mode":"whole","corpus":corpus_name(path),"codec":c,"raw":text.len(),"stored":packed.len(),
                "ratio":packed.len() as f64 / text.len() as f64,"c_mbps":mb/ct,"d_mbps":mb/dt,"roundtrip":ok})
        );
        assert!(ok, "roundtrip failed for {c}");
    }
}

/// Tokenizer pre-pass (no model): o200k BPE ids as LEB128 varints, then an
/// inner codec. Time includes tokenization and detokenization.
fn whole_tokenized(path: &str, text: &[u8], inner: &str, label: &str) {
    let bpe = tiktoken_rs::o200k_base().unwrap();
    let side = Side { zdict: None, raw_dict: Vec::new(), fsst: None };
    let mut ctx = Ctx::new(parse(inner), &side);
    let s = std::str::from_utf8(text).unwrap();
    let t = Instant::now();
    let ids = bpe.encode_ordinary(s);
    let mut stream = Vec::with_capacity(ids.len() * 3);
    for &id in &ids {
        oojuro_store::format::put_varint(&mut stream, id as u64);
    }
    let packed = ctx.compress(&stream);
    let ct = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let back = ctx.decompress(&packed, stream.len(), stream.len());
    let mut at = 0;
    let mut ids2 = Vec::with_capacity(ids.len());
    while at < back.len() {
        ids2.push(oojuro_store::format::get_varint(&back, &mut at).unwrap() as u32);
    }
    let out = bpe.decode(ids2).unwrap();
    let dt = t.elapsed().as_secs_f64();
    let ok = out.as_bytes() == text;
    let mb = text.len() as f64 / 1e6;
    println!(
        "{}",
        json!({"mode":"whole","corpus":corpus_name(path),"codec":label,"raw":text.len(),"stored":packed.len(),"tokens":ids.len(),
            "bytes_per_token":text.len() as f64/ids.len() as f64,"varint_stream":stream.len(),
            "ratio":packed.len() as f64 / text.len() as f64,"c_mbps":mb/ct,"d_mbps":mb/dt,"roundtrip":ok})
    );
    assert!(ok, "tokenizer roundtrip failed");
}

/// Compressed self-index sizes (fm-index crate): count-only, with sampled
/// locate (every 2^level suffix-array entry), and run-length variants.
fn fmindex(path: &str, prefix: usize) {
    use fm_index::{FMIndex, FMIndexWithLocate, RLFMIndex, RLFMIndexWithLocate, SearchIndex, Text};
    let recs = sample(read_corpus(Path::new(path)).unwrap(), prefix);
    let mut text: Vec<u8> = recs.iter().flat_map(|r| r.text.iter().copied()).filter(|&b| b != 0).collect();
    let raw = text.len();
    text.push(0);
    let t = Text::new(text.as_slice());
    let report = |name: &str, size: usize, secs: f64, count: usize| {
        println!("{}", json!({"mode":"fmindex","corpus":corpus_name(path),"index":name,"raw":raw,"stored":size,"ratio":size as f64/raw as f64,"build_s":secs,"count_def":count}));
    };
    let s = Instant::now();
    let i = FMIndex::new(&t).unwrap();
    report("fm-count", i.heap_size(), s.elapsed().as_secs_f64(), i.search("def ").count());
    drop(i);
    for level in [4usize, 6] {
        let s = Instant::now();
        let i = FMIndexWithLocate::new(&t, level).unwrap();
        report(&format!("fm-locate-l{level}"), i.heap_size(), s.elapsed().as_secs_f64(), i.search("def ").count());
    }
    let s = Instant::now();
    let i = RLFMIndex::new(&t).unwrap();
    report("rlfm-count", i.heap_size(), s.elapsed().as_secs_f64(), i.search("def ").count());
    drop(i);
    let s = Instant::now();
    let i = RLFMIndexWithLocate::new(&t, 6).unwrap();
    report("rlfm-locate-l6", i.heap_size(), s.elapsed().as_secs_f64(), i.search("def ").count());
}

fn train_fsst(recs: &[CorpusRecord]) -> fsst::Compressor {
    // FSST trains on a bounded sample; the paper uses 16 KiB, we give it 1 MiB.
    let mut rng = Rng::new(7);
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut total = 0;
    while total < (1 << 20) && !recs.is_empty() {
        let r = &recs[rng.below(recs.len())];
        lines.push(&r.text);
        total += r.text.len();
    }
    fsst::Compressor::train(&lines)
}

fn train_zstd_dict(recs: &[CorpusRecord], dict_bytes: usize) -> Vec<u8> {
    // Train on up to 100x the dictionary size of records sampled across the corpus.
    let total: usize = recs.iter().map(|r| r.text.len()).sum();
    let mut rng = Rng::new(11);
    let mut data = Vec::new();
    let mut sizes = Vec::new();
    while data.len() < dict_bytes * 100 && data.len() < total / 2 {
        let r = &recs[rng.below(recs.len())];
        let t = &r.text[..r.text.len().min(128 << 10)];
        if t.len() < 8 {
            continue;
        }
        data.extend_from_slice(t);
        sizes.push(t.len());
    }
    zstd::dict::from_continuous(&data, &sizes, dict_bytes).unwrap()
}

fn blocks(path: &str, sample_bytes: usize, block_bytes: usize, dict_bytes: usize, codecs: &[String]) {
    let recs = sample(read_corpus(Path::new(path)).unwrap(), sample_bytes);
    let raw: usize = recs.iter().map(|r| r.text.len()).sum();
    // GROUP=kind packs each record kind into its own blocks (seq order within a
    // kind) and trains one dictionary per kind; default is append order.
    let by_kind = std::env::var("GROUP").is_ok_and(|g| g == "kind");
    let mut order: Vec<usize> = (0..recs.len()).collect();
    if by_kind {
        order.sort_by_key(|&i| (recs[i].kind, i));
    }
    // Pack records into blocks; remember (block, offset, len) for each record.
    let mut blocks: Vec<Vec<u8>> = vec![Vec::new()];
    let mut block_kind: Vec<u8> = vec![recs.first().map_or(0, |r| r.kind)];
    let mut loc = vec![(0, 0, 0); recs.len()];
    for &i in &order {
        let r = &recs[i];
        let last = blocks.last().unwrap();
        let kind_change = by_kind && *block_kind.last().unwrap() != r.kind;
        if !last.is_empty() && (kind_change || last.len() + r.text.len() > block_bytes.max(1)) {
            blocks.push(Vec::new());
            block_kind.push(r.kind);
        }
        let b = blocks.len() - 1;
        loc[i] = (b, blocks[b].len(), r.text.len());
        blocks[b].extend_from_slice(&r.text);
    }
    let t = Instant::now();
    let mut dicts: Vec<Vec<u8>> = vec![Vec::new(); 8];
    if dict_bytes > 0 && by_kind {
        for k in 0..7u8 {
            let of_kind: Vec<CorpusRecord> = recs.iter().filter(|r| r.kind == k).cloned().collect();
            let bytes: usize = of_kind.iter().map(|r| r.text.len()).sum();
            // Kinds too small to train on use no dictionary.
            if bytes >= dict_bytes * 8 && of_kind.len() >= 64 {
                dicts[k as usize] = train_zstd_dict(&of_kind, dict_bytes);
            }
        }
    } else if dict_bytes > 0 {
        dicts[7] = train_zstd_dict(&recs, dict_bytes);
    }
    let train_s = t.elapsed().as_secs_f64();
    let dict_of = |b: usize| if by_kind { block_kind[b] as usize } else { 7 };
    for c in codecs {
        let spec = parse(c);
        let mut side_bytes = 0;
        let sides: Vec<Side> = dicts
            .iter()
            .map(|dict| {
                let mut side = Side { zdict: None, raw_dict: Vec::new(), fsst: None };
                match spec {
                    Spec::Zstd { level, .. } if !dict.is_empty() => {
                        side.zdict = Some((
                            zstd::dict::EncoderDictionary::copy(dict, level),
                            zstd::dict::DecoderDictionary::copy(dict),
                        ));
                        side_bytes += dict.len();
                    }
                    Spec::Lz4 if !dict.is_empty() => {
                        side.raw_dict = dict[dict.len().saturating_sub(65536)..].to_vec();
                        side_bytes += side.raw_dict.len();
                    }
                    _ => {}
                }
                side
            })
            .collect();
        let fsst_side;
        let mut ctxs: Vec<Ctx> = if matches!(spec, Spec::Fsst) {
            let f = train_fsst(&recs);
            side_bytes = f.symbol_table().len() * 9;
            fsst_side = Side { zdict: None, raw_dict: Vec::new(), fsst: Some(f) };
            (0..8).map(|_| Ctx::new(spec.clone(), &fsst_side)).collect()
        } else {
            sides.iter().map(|s| Ctx::new(spec.clone(), s)).collect()
        };
        let t = Instant::now();
        let packed: Vec<Vec<u8>> = blocks.iter().enumerate().map(|(i, b)| ctxs[dict_of(i)].compress(b)).collect();
        let ct = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut ok = true;
        for (i, (b, p)) in blocks.iter().zip(&packed).enumerate() {
            let back = ctxs[dict_of(i)].decompress(p, b.len(), b.len());
            ok &= &back == b;
        }
        let dt = t.elapsed().as_secs_f64();
        assert!(ok, "roundtrip failed for {c}");
        // Cold excerpt fetches: QUERIES (default 400) x 20 hits, each a 2 KiB range.
        let queries: usize = std::env::var("QUERIES").ok().and_then(|q| q.parse().ok()).unwrap_or(400);
        let mut rng = Rng::new(42);
        let (mut per_query, mut per_fetch) = (Vec::new(), Vec::new());
        let mut checksum = 0u64;
        for _ in 0..queries {
            let tq = Instant::now();
            for _ in 0..20 {
                let i = rng.below(recs.len());
                let (b, off, len) = loc[i];
                let want = len.min(2048);
                let start = off + rng.below(len - want + 1);
                let tf = Instant::now();
                let out = ctxs[dict_of(b)].decompress(&packed[b], blocks[b].len(), start + want);
                let excerpt = &out[start..start + want];
                per_fetch.push(tf.elapsed().as_secs_f64() * 1e6);
                checksum = checksum.wrapping_add(excerpt.first().copied().unwrap_or(0) as u64);
            }
            per_query.push(tq.elapsed().as_secs_f64() * 1e6);
        }
        // Per-kind stored bytes (only exact when GROUP=kind).
        let mut kind_stored = [0usize; 8];
        let mut kind_raw = [0usize; 8];
        for (i, p) in packed.iter().enumerate() {
            kind_stored[block_kind[i] as usize] += p.len() + 16;
            kind_raw[block_kind[i] as usize] += blocks[i].len();
        }
        let stored: usize = packed.iter().map(|p| p.len()).sum::<usize>() + side_bytes + 16 * packed.len();
        let mb = raw as f64 / 1e6;
        println!(
            "{}",
            json!({"mode":"blocks","corpus":corpus_name(path),"codec":c,"block_bytes":block_bytes,"dict_bytes":side_bytes,
                "blocks":packed.len(),"raw":raw,"stored":stored,"ratio":stored as f64/raw as f64,
                "c_mbps":mb/ct,"d_mbps":mb/dt,"train_s":train_s,
                "fetch_us_p50":percentile(&mut per_fetch.clone(),0.5),"fetch_us_p99":percentile(&mut per_fetch,0.99),
                "query20_us_p50":percentile(&mut per_query.clone(),0.5),"query20_us_p99":percentile(&mut per_query,0.99),
                "group":if by_kind {"kind"} else {"append"},
                "kind_ratio":if by_kind {json!((0..7).filter(|&k| kind_raw[k] > 0).map(|k| (oojuro_store::corpus::KIND_NAMES[k], kind_stored[k] as f64 / kind_raw[k] as f64)).collect::<std::collections::BTreeMap<_,_>>())} else {json!(null)},
                "roundtrip":ok,"check":checksum})
        );
    }
}

/// Content-defined dedupe potential: fraction of bytes in FastCDC chunks seen
/// before, split into repeats within the same session vs across sessions.
fn dedupe(path: &str, sample_bytes: usize) {
    let recs = sample(read_corpus(Path::new(path)).unwrap(), sample_bytes);
    let raw: usize = recs.iter().map(|r| r.text.len()).sum();
    for avg in [256u32, 512, 1024, 4096] {
        let mut seen: std::collections::HashMap<[u8; 16], u32> = std::collections::HashMap::new();
        let (mut dup_same, mut dup_cross, mut chunks) = (0usize, 0usize, 0usize);
        let mut kind_dup = [0usize; 8];
        let mut kind_raw = [0usize; 8];
        for r in &recs {
            kind_raw[r.kind as usize] += r.text.len();
            for c in fastcdc::v2020::FastCDC::new(&r.text, avg / 4, avg, avg * 8) {
                chunks += 1;
                let h: [u8; 16] = blake3::hash(&r.text[c.offset..c.offset + c.length]).as_bytes()[..16].try_into().unwrap();
                match seen.get(&h) {
                    Some(&s) if s == r.session => dup_same += c.length,
                    Some(_) => dup_cross += c.length,
                    None => {
                        seen.insert(h, r.session);
                        continue;
                    }
                }
                kind_dup[r.kind as usize] += c.length;
            }
        }
        println!(
            "{}",
            json!({"mode":"dedupe","corpus":corpus_name(path),"avg_chunk":avg,"raw":raw,"chunks":chunks,
                "unique_chunks":seen.len(),"dup_fraction":(dup_same+dup_cross) as f64/raw as f64,
                "dup_same_session":dup_same as f64/raw as f64,"dup_cross_session":dup_cross as f64/raw as f64,
                "kind_dup_fraction":(0..7).filter(|&k| kind_raw[k] > 0).map(|k| (oojuro_store::corpus::KIND_NAMES[k], kind_dup[k] as f64 / kind_raw[k] as f64)).collect::<std::collections::BTreeMap<_,_>>()})
        );
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(|s| s.as_str()) {
        Some("dump") => {
            let recs = read_corpus(Path::new(&a[2])).unwrap();
            let mut f = std::io::BufWriter::new(std::fs::File::create(&a[3]).unwrap());
            for r in recs {
                f.write_all(&r.text).unwrap();
            }
        }
        Some("slice") => {
            // Append-order prefix of at least <bytes> raw text, as a new .rec file.
            let recs = sample(read_corpus(Path::new(&a[2])).unwrap(), a[3].parse().unwrap());
            let mut f = std::io::BufWriter::new(std::fs::File::create(&a[4]).unwrap());
            f.write_all(b"OJCORP01").unwrap();
            for r in recs {
                f.write_all(&[r.kind]).unwrap();
                f.write_all(&r.session.to_le_bytes()).unwrap();
                f.write_all(&r.turn.to_le_bytes()).unwrap();
                f.write_all(&(r.text.len() as u32).to_le_bytes()).unwrap();
                f.write_all(&r.text).unwrap();
            }
        }
        Some("jsonl") => {
            // One JSON object per record: seq, session, turn, kind, valid_at, text (exact).
            let recs = read_corpus(Path::new(&a[2])).unwrap();
            let mut f = std::io::BufWriter::new(std::fs::File::create(&a[3]).unwrap());
            for (i, r) in recs.iter().enumerate() {
                let t = std::str::from_utf8(&r.text).unwrap();
                let v = json!({"seq":i,"session":r.session,"turn":r.turn,"kind":oojuro_store::corpus::KIND_NAMES[r.kind as usize],
                    "valid_at":r.session as i64 * 100_000 + r.turn as i64,"text":t});
                serde_json::to_writer(&mut f, &v).unwrap();
                f.write_all(b"\n").unwrap();
            }
        }
        Some("fmindex") => fmindex(&a[2], a[3].parse().unwrap()),
        Some("dedupe") => dedupe(&a[2], a[3].parse().unwrap()),
        Some("whole") => whole(&a[2], a[3].parse().unwrap(), &a[4..]),
        Some("blocks") => blocks(&a[2], a[3].parse().unwrap(), a[4].parse().unwrap(), a[5].parse().unwrap(), &a[6..]),
        _ => eprintln!("usage: codecs dump|whole|blocks ..."),
    }
}
