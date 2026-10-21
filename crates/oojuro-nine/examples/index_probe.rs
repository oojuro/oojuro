//! Where the bytes of v4 index segments go, and what other layouts would cost
//! on the same data (sizes from the real lists; nothing is written): the
//! index layout study of the compress track.
//!
//! Usage: index_probe [--kinds FILE] [--bench] [--json OUT] [--label L] <dir>...
//!   (every `seg-*.idx` below the directories)
//!
//! `--kinds FILE`: one byte per event id (kind code, 255 = none; written by
//! `replay_export --kinds` or `searchprobe kinds`): rows are attributed to
//! kinds through their id (TraceDb ids are decimal event ids; `b<id>` rows
//! are dedupe bundles).
//! `--bench`: decode every list in each postings encoding, in memory, and
//! report ns per posting (take the bench lock around it).
//!
//! Postings encodings, each list padded to whole bytes:
//! - actual: the bytes in the files (version 1 LEB128 or version 2 Rice)
//! - leb:  LEB128 (row gap, tf) pairs (segment version 1)
//! - for:  blocks of 128: one byte of widths, then (gap - 1) and (tf - 1)
//!   bit-packed at the block's widths; a short last block likewise
//! - for-vint: full blocks of 128 bit-packed (BitPacker4x layout: one width
//!   byte each for gaps and tf), the tail (< 128) as LEB128 pairs
//! - pfor: like for-vint, but each block's gap width b is chosen to minimize
//!   size, values >= 2^b are exceptions (one position byte plus the high
//!   bits at one width per block); tf as in for-vint
//! - ef:   Elias-Fano rows (universe = segment rows) + Elias-gamma tf
//! - ef+w: Elias-Fano rows + (tf - 1) at one bit width per list
//! - pef:  uniform partitioned Elias-Fano (partitions of 128; each EF, a
//!   bitmap or nothing when dense, whichever is smaller; partition ends and
//!   pointers in two top-level EF sequences) + Elias-gamma tf
//! - roaring: roaring bitmap (portable format, run containers allowed) +
//!   Elias-gamma tf
//! - bound: log2 C(U, n) per list + empirical entropy of tf over all postings
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
fn i64_at(b: &[u8], p: usize) -> i64 {
    i64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
fn u128_at(b: &[u8], p: usize) -> u128 {
    u128::from_le_bytes(b[p..p + 16].try_into().unwrap())
}
fn varint(b: &[u8], p: &mut usize) -> u64 {
    let (mut v, mut s) = (0u64, 0);
    loop {
        let x = b[*p];
        *p += 1;
        v |= u64::from(x & 0x7f) << s;
        if x < 0x80 {
            return v;
        }
        s += 7;
    }
}
fn vlen(mut v: u64) -> u64 {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}
fn bits(v: u64) -> u64 {
    64 - v.leading_zeros() as u64
}
fn gamma_bits(v: u64) -> u64 {
    2 * (bits(v) - 1) + 1
}
fn log2_binom(u: u64, n: u64) -> f64 {
    let n = n.min(u - n);
    let mut s = 0f64;
    for i in 0..n {
        s += ((u - i) as f64).log2() - ((i + 1) as f64).log2();
    }
    s
}
/// Elias-Fano bits for n sorted values below u.
fn ef_bits(n: u64, u: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let l = if u > n { bits(u / n) - 1 } else { 0 };
    n * l + n + (u >> l) + 1
}
fn rice_k(rows: u64, df: u64) -> u64 {
    let q = rows / df.max(1);
    if q <= 1 { 0 } else { bits(q) - 1 }
}

const KINDS: usize = 9;
const KIND_NAMES: [&str; KINDS] =
    ["system", "user_input", "thinking", "assistant", "tool_call", "tool_result", "explicit", "bundle", "unknown"];
const DF_FRACS: [f64; 4] = [0.01, 0.05, 0.10, 0.25];
const FC_BLOCKS: [usize; 5] = [16, 32, 64, 128, 256];

#[derive(Default, Debug)]
struct Tot {
    segs: u64,
    rows: u64,
    indexed_rows: u64,
    file: u64,
    sec: [u64; 8],
    header_footer: u64,
    postings: u64,
    lists: u64,
    actual: u64,
    rice_bits: u64,
    gamma_bits: u64,
    leb: u64,
    forb: u64,
    for_vint: u64,
    pfor: u64,
    ef: u64,
    efw: u64,
    pef: u64,
    roaring: u64,
    roaring_rows: u64,
    rice_rows_only: u64,
    tf_for128: u64,
    ef_rows_only: u64,
    bound: f64,
    tf_hist: HashMap<u64, u64>,
    df1: u64,
    cf_sum: u64,
    positions_bits: f64,
    // df distribution: postings, list bytes, dict entry bytes, terms above each fraction
    df_post: [u64; 4],
    df_bytes: [u64; 4],
    df_dict: [u64; 4],
    df_terms: [u64; 4],
    // the same with df summed over the store's segments (fractions of its indexed rows)
    sdf_post: [u64; 4],
    sdf_bytes: [u64; 4],
    sdf_dict: [u64; 4],
    sdf_terms: [u64; 4],
    // dictionary fields
    d_terms: u64,
    d_term_bytes: u64,
    d_df: u64,
    d_cf: u64,
    d_len: u64,
    d_block_off: u64,
    d_df1_entry_bytes: u64,
    fc: [u64; 5],
    fc_nocf: [u64; 5],
    fc_cols: [u64; 5],
    fst_cols: u64,
    fst_only: u64,
    col_df: u64,
    col_off: u64,
    // fixed tables
    lean_rows: u64,
    lean_rows_fields: [u64; 7],
    rec_eq_valid_rows: u64,
    numeric_id_rows: u64,
    id_monotone_breaks: u64,
    ids_lean: u64,
    ids_fp64: u64,
    key_ne_id_rows: u64,
    time_not_identity: u64,
    time_runs: u64,
    skip_lists: u64,
    skip_entries: u64,
    // by kind: rows, indexed rows, postings, rice bits, gamma bits, exclusive-term dict bytes,
    // exclusive-term list bytes, text bytes
    k_rows: [u64; KINDS],
    k_irows: [u64; KINDS],
    k_post: [u64; KINDS],
    k_rice: [u64; KINDS],
    k_gamma: [u64; KINDS],
    k_excl_dict: [u64; KINDS],
    k_excl_list: [u64; KINDS],
    k_dict_share: [f64; KINDS],
    k_text: [u64; KINDS],
    largest_seg_rows: u64,
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "idx") {
                out.push(path);
            }
        }
    }
}

/// Every list of every segment, decoded, for `--bench`.
#[derive(Default)]
struct All {
    /// (segment rows, start in rows/tfs, df)
    lists: Vec<(u32, usize, u32)>,
    rows: Vec<u32>,
    tfs: Vec<u32>,
}

fn kind_of(id_prefix: u64, kinds: &[u8]) -> usize {
    let b = id_prefix.to_be_bytes();
    let n = b.iter().position(|&x| x == 0).unwrap_or(8);
    let s = &b[..n];
    if s.first() == Some(&b'b') {
        return 7;
    }
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return 8;
    }
    let id: usize = std::str::from_utf8(s).unwrap().parse().unwrap();
    match kinds.get(id) {
        Some(&k) if (k as usize) < 7 => k as usize,
        _ => 8,
    }
}

fn numeric_id(id_prefix: u64) -> Option<u64> {
    let b = id_prefix.to_be_bytes();
    let n = b.iter().position(|&x| x == 0).unwrap_or(8);
    let s = &b[..n];
    (!s.is_empty() && n < 8 && s.iter().all(u8::is_ascii_digit)).then(|| std::str::from_utf8(s).unwrap().parse().unwrap())
}

/// Frame of reference over blocks of 128: per block a base (`base_bytes`),
/// one width byte, then values - base at that width.
fn for_bytes(vals: &[u64], base_bytes: u64) -> u64 {
    let mut b = 0;
    for c in vals.chunks(128) {
        let lo = *c.iter().min().unwrap();
        let hi = *c.iter().max().unwrap();
        b += 8 * (base_bytes + 1) + c.len() as u64 * bits(hi - lo);
    }
    b.div_ceil(8)
}

/// Every term of a segment with its df, and the segment's indexed rows.
fn dict_terms(map: &[u8], out: &mut HashMap<Vec<u8>, u64>) -> u64 {
    let rows = u32_at(map, 20) as usize;
    let mut sec = [0usize; 9];
    for (i, s) in sec.iter_mut().enumerate() {
        *s = u64_at(map, 32 + 8 * i) as usize;
    }
    let indexed = (0..rows).filter(|r| u32_at(map, sec[0] + 40 * r + 28) & 1 == 0).count() as u64;
    let (s_dict, s_dindex, s_end) = (sec[6], sec[7], sec[8]);
    let blocks = (s_end - s_dindex) / 8;
    for b in 0..blocks {
        let start = s_dict + u64_at(map, s_dindex + 8 * b) as usize;
        let end = if b + 1 < blocks { s_dict + u64_at(map, s_dindex + 8 * (b + 1)) as usize } else { s_dindex };
        let mut p = start;
        varint(map, &mut p);
        let mut prev: Vec<u8> = Vec::new();
        while p < end {
            let shared = varint(map, &mut p) as usize;
            let sn = varint(map, &mut p) as usize;
            let mut term = prev[..shared].to_vec();
            term.extend_from_slice(&map[p..p + sn]);
            p += sn;
            let df = varint(map, &mut p);
            varint(map, &mut p);
            varint(map, &mut p);
            *out.entry(term.clone()).or_default() += df;
            prev = term;
        }
    }
    indexed
}

fn probe(map: &[u8], kinds: &[u8], t: &mut Tot, all: Option<&mut All>, store: (&HashMap<Vec<u8>, u64>, u64)) {
    let version = u32_at(map, 8);
    let rows = u32_at(map, 20) as u64;
    let mut sec = [0usize; 9];
    for (i, s) in sec.iter_mut().enumerate() {
        *s = u64_at(map, 32 + 8 * i) as usize;
    }
    t.segs += 1;
    t.rows += rows;
    t.largest_seg_rows = t.largest_seg_rows.max(rows);
    t.file += map.len() as u64;
    t.header_footer += (map.len() - (sec[8] - sec[0])) as u64;
    for i in 0..8 {
        t.sec[i] += (sec[i + 1] - sec[i]) as u64;
    }
    let (s_rows, s_ids, s_keys, s_time) = (sec[0], sec[1], sec[2], sec[3]);
    let (s_post, s_dict, s_dindex, s_end) = (sec[5], sec[6], sec[7], sec[8]);
    let n = rows as usize;
    // ---- rows table
    let mut kind = vec![8usize; n];
    let mut length = vec![0u64; n];
    let mut indexed = 0u64;
    let mut f_valid = Vec::with_capacity(n);
    let mut f_rec = Vec::with_capacity(n);
    let mut f_len = Vec::with_capacity(n);
    let mut f_scope = Vec::with_capacity(n);
    let mut f_text = Vec::with_capacity(n);
    let mut f_flags = Vec::with_capacity(n);
    let mut ids_num: Vec<Option<u64>> = Vec::with_capacity(n);
    let mut all_rec_eq = true;
    for r in 0..n {
        let p = s_rows + 40 * r;
        let valid = i64_at(map, p);
        let rec = i64_at(map, p + 8);
        let len = u32_at(map, p + 16) as u64;
        let scope = u32_at(map, p + 20) as u64;
        let text_len = u32_at(map, p + 24) as u64;
        let flags = u32_at(map, p + 28) as u64;
        let idp = u64::from_be_bytes(map[p + 32..p + 40].try_into().unwrap());
        let k = kind_of(idp, kinds);
        kind[r] = k;
        length[r] = len;
        t.k_rows[k] += 1;
        t.k_text[k] += text_len;
        if flags & 1 == 0 {
            indexed += 1;
            t.k_irows[k] += 1;
        }
        all_rec_eq &= rec == valid;
        t.rec_eq_valid_rows += (rec == valid) as u64;
        f_valid.push(valid as u64);
        f_rec.push(rec.wrapping_sub(valid) as u64);
        f_len.push(len);
        f_scope.push(scope);
        f_text.push(text_len);
        f_flags.push(flags);
        ids_num.push(numeric_id(idp));
    }
    t.indexed_rows += indexed;
    // lean rows: FOR-128 per field; recorded_at free when it always equals valid_at;
    // id prefixes as numbers when every id of a block is decimal, else 8 B
    let mut fields = [
        for_bytes(&f_valid, 8),
        if all_rec_eq { 0 } else { for_bytes(&f_rec, 8) },
        for_bytes(&f_len, 4),
        for_bytes(&f_scope, 4),
        for_bytes(&f_text, 4),
        for_bytes(&f_flags, 4),
        0,
    ];
    for c in ids_num.chunks(128) {
        if c.iter().all(Option::is_some) {
            let v: Vec<u64> = c.iter().map(|x| x.unwrap()).collect();
            fields[6] += for_bytes(&v, 8);
        } else {
            fields[6] += 8 * c.len() as u64;
        }
    }
    for (a, b) in t.lean_rows_fields.iter_mut().zip(fields) {
        *a += b;
    }
    t.lean_rows += fields.iter().sum::<u64>();
    // ---- ids / keys / time
    let mut id_fp = vec![0u128; n];
    let mut key_fp = vec![0u128; n];
    for i in 0..n {
        let p = s_ids + 20 * i;
        id_fp[u32_at(map, p + 16) as usize] = u128_at(map, p);
        let q = s_keys + 20 * i;
        key_fp[u32_at(map, q + 16) as usize] = u128_at(map, q);
    }
    let mut prev_num: Option<u64> = None;
    let mut ids_lean = 0u64;
    let row_w = bits(rows.max(1)).div_ceil(8);
    for r in 0..n {
        t.key_ne_id_rows += (key_fp[r] != id_fp[r]) as u64;
        match ids_num[r] {
            Some(x) => {
                t.numeric_id_rows += 1;
                if prev_num.is_some_and(|p| x <= p) {
                    // an id lookup by binary search over the id column needs this row listed
                    t.id_monotone_breaks += 1;
                    ids_lean += 16 + row_w;
                }
                prev_num = Some(x);
            }
            None => ids_lean += 16 + row_w,
        }
    }
    t.ids_lean += ids_lean;
    t.ids_fp64 += n as u64 * (8 + row_w);
    for i in 0..n {
        let x = u32_at(map, s_time + 4 * i);
        t.time_not_identity += (x as usize != i) as u64;
        t.time_runs += (i == 0 || x != u32_at(map, s_time + 4 * (i - 1)).wrapping_add(1)) as u64;
    }
    // ---- dictionary and postings
    let blocks = (s_end - s_dindex) / 8;
    let u = rows.max(1);
    let mut terms: Vec<(Vec<u8>, u64, u64, u64)> = Vec::new(); // term, df, cf, len
    let mut list_off: Vec<u64> = Vec::new();
    let mut all = all;
    for b in 0..blocks {
        let start = s_dict + u64_at(map, s_dindex + 8 * b) as usize;
        let end = if b + 1 < blocks { s_dict + u64_at(map, s_dindex + 8 * (b + 1)) as usize } else { s_dindex };
        let mut p = start;
        let p0 = p;
        let mut off = s_post + varint(map, &mut p) as usize;
        t.d_block_off += (p - p0) as u64;
        let mut prev_term: Vec<u8> = Vec::new();
        while p < end {
            let e0 = p;
            let shared = varint(map, &mut p) as usize;
            let sn = varint(map, &mut p) as usize;
            let mut term = prev_term[..shared].to_vec();
            term.extend_from_slice(&map[p..p + sn]);
            p += sn;
            let term_bytes = (p - e0) as u64;
            let df = varint(map, &mut p);
            let cf = varint(map, &mut p);
            let len = varint(map, &mut p) as usize;
            let entry_bytes = (p - e0) as u64;
            t.d_terms += 1;
            t.d_term_bytes += term_bytes;
            t.d_df += vlen(df);
            t.d_cf += vlen(cf);
            t.d_len += vlen(len as u64);
            t.cf_sum += cf;
            if df == 1 {
                t.d_df1_entry_bytes += entry_bytes;
            }
            list_off.push((off - s_post) as u64);
            let list = &map[off..off + len];
            off += len;
            let (mut row, mut first) = (0u64, true);
            let mut rs = Vec::with_capacity(df as usize);
            let mut tfs = Vec::with_capacity(df as usize);
            let (mut rb, mut gb) = (0u64, 0u64);
            if version == 1 {
                let mut q = 0;
                while q < list.len() {
                    let gap = varint(list, &mut q);
                    let tf = varint(list, &mut q);
                    row = if first { gap } else { row + gap };
                    first = false;
                    rs.push(row);
                    tfs.push(tf);
                }
            } else {
                let k = rice_k(u, df);
                let mut pos = 0usize;
                let bit = |pos: &mut usize| -> u64 {
                    let b = (list[*pos / 8] >> (*pos % 8)) & 1;
                    *pos += 1;
                    u64::from(b)
                };
                for _ in 0..df {
                    let mut q = 0u64;
                    while bit(&mut pos) == 0 {
                        q += 1;
                    }
                    let mut r = 0u64;
                    for i in 0..k {
                        r |= bit(&mut pos) << i;
                    }
                    let gap = (q << k) | r;
                    let mut z = 0u64;
                    while bit(&mut pos) == 0 {
                        z += 1;
                    }
                    let mut tf = 1u64 << z;
                    for i in 0..z {
                        tf |= bit(&mut pos) << i;
                    }
                    row = if first { gap } else { row + gap + 1 };
                    first = false;
                    rs.push(row);
                    tfs.push(tf);
                }
            }
            assert_eq!(rs.len() as u64, df);
            // per-posting Rice and gamma bits (what version 2 spends)
            let k = rice_k(u, df);
            let mut prevr: Option<u64> = None;
            for (&r, &tf) in rs.iter().zip(&tfs) {
                let gap = match prevr {
                    None => r,
                    Some(p) => r - p - 1,
                };
                prevr = Some(r);
                let pr = (gap >> k) + 1 + k;
                let pg = gamma_bits(tf);
                rb += pr;
                gb += pg;
                let kk = kind[r as usize];
                t.k_post[kk] += 1;
                t.k_rice[kk] += pr;
                t.k_gamma[kk] += pg;
                // positions: Rice of in-record position gaps, k = floor(log2(len / tf))
                let l = length[r as usize].max(tf);
                t.positions_bits += tf as f64 * (rice_k(l, tf) + 2) as f64;
            }
            t.rice_bits += rb;
            t.gamma_bits += gb;
            t.rice_rows_only += rb.div_ceil(8);
            t.lists += 1;
            t.postings += df;
            t.df1 += (df == 1) as u64;
            t.actual += len as u64;
            // kinds: exclusive terms and proportional dict share
            let mut kc = [0u64; KINDS];
            for &r in &rs {
                kc[kind[r as usize]] += 1;
            }
            for (i, &c) in kc.iter().enumerate() {
                if c == df {
                    t.k_excl_dict[i] += entry_bytes;
                    t.k_excl_list[i] += len as u64;
                }
                t.k_dict_share[i] += entry_bytes as f64 * c as f64 / df as f64;
            }
            // df distribution (fractions of the segment's indexed rows)
            for (i, f) in DF_FRACS.iter().enumerate() {
                if df as f64 > f * indexed.max(1) as f64 {
                    t.df_post[i] += df;
                    t.df_bytes[i] += len as u64;
                    t.df_dict[i] += entry_bytes;
                    t.df_terms[i] += 1;
                }
            }
            let sdf = store.0.get(&term).copied().unwrap_or(df);
            for (i, f) in DF_FRACS.iter().enumerate() {
                if sdf as f64 > f * store.1.max(1) as f64 {
                    t.sdf_post[i] += df;
                    t.sdf_bytes[i] += len as u64;
                    t.sdf_dict[i] += entry_bytes;
                    t.sdf_terms[i] += 1;
                }
            }
            if df >= 1024 {
                t.skip_lists += 1;
                t.skip_entries += df.div_ceil(128);
            }
            // LEB128 pairs
            let mut prev = 0u64;
            for (i, (&r, &tf)) in rs.iter().zip(&tfs).enumerate() {
                t.leb += vlen(if i == 0 { r } else { r - prev }) + vlen(tf);
                prev = r;
            }
            for &x in &tfs {
                *t.tf_hist.entry(x.min(64)).or_default() += 1;
            }
            let mut prev: Option<u64> = None;
            let gaps: Vec<u64> = rs
                .iter()
                .map(|&r| {
                    let g = match prev {
                        None => r,
                        Some(p) => r - p - 1,
                    };
                    prev = Some(r);
                    g
                })
                .collect();
            // FOR blocks of 128 (tail bit-packed too)
            let mut fb = 0u64;
            for (cg, ct) in gaps.chunks(128).zip(tfs.chunks(128)) {
                let wg = cg.iter().map(|&g| bits(g)).max().unwrap();
                let wt = ct.iter().map(|&x| bits(x - 1)).max().unwrap();
                fb += 8 + cg.len() as u64 * (wg + wt);
            }
            t.forb += fb.div_ceil(8);
            // tf alone: blocks of 128, one width byte, (tf - 1) at that width
            let mut tb = 0u64;
            for ct in tfs.chunks(128) {
                tb += 8 + ct.len() as u64 * ct.iter().map(|&x| bits(x - 1)).max().unwrap();
            }
            t.tf_for128 += tb.div_ceil(8);
            // FOR-128 / PFor-128 with a LEB128 tail
            let full = gaps.len() / 128 * 128;
            let (mut fv, mut pf) = (0u64, 0u64);
            for (cg, ct) in gaps[..full].chunks(128).zip(tfs[..full].chunks(128)) {
                let wg = cg.iter().map(|&g| bits(g)).max().unwrap();
                let wt = ct.iter().map(|&x| bits(x - 1)).max().unwrap();
                fv += 8 * 2 + 128 * (wg + wt);
                let mut best = u64::MAX;
                for b in 0..=wg {
                    let exc: Vec<u64> = cg.iter().filter(|&&g| bits(g) > b).map(|&g| g >> b).collect();
                    let we = exc.iter().map(|&x| bits(x)).max().unwrap_or(0);
                    let cost = 128 * b + exc.len() as u64 * (8 + we) + 24;
                    best = best.min(cost);
                }
                pf += best + 8 + 128 * wt;
            }
            let mut tail = 0u64;
            let mut prev = if full > 0 { rs[full - 1] } else { 0 };
            for i in full..rs.len() {
                tail += vlen(if i == 0 { rs[i] } else { rs[i] - prev }) + vlen(tfs[i]);
                prev = rs[i];
            }
            t.for_vint += fv.div_ceil(8) + tail;
            t.pfor += pf.div_ceil(8) + tail;
            // Elias-Fano
            let nn = df;
            let efr = ef_bits(nn, u);
            let gamma: u64 = tfs.iter().map(|&x| gamma_bits(x)).sum();
            let wt = tfs.iter().map(|&x| bits(x - 1)).max().unwrap();
            t.ef += (efr + gamma).div_ceil(8);
            t.ef_rows_only += efr.div_ceil(8);
            t.efw += (efr + 8 + nn * wt).div_ceil(8);
            // uniform partitioned EF (128)
            let pefb = if rs.len() <= 128 {
                efr
            } else {
                let parts = rs.len().div_ceil(128) as u64;
                let mut body = 0u64;
                let mut base = 0u64;
                for c in rs.chunks(128) {
                    let last = *c.last().unwrap();
                    let up = last - base + 1;
                    let np = c.len() as u64;
                    let cost = if np == up { 0 } else { ef_bits(np, up).min(up) };
                    body += cost;
                    base = last + 1;
                }
                body + ef_bits(parts, u) + ef_bits(parts, body.max(1) + 1)
            };
            t.pef += (pefb + gamma).div_ceil(8);
            // roaring
            let mut rb = roaring::RoaringBitmap::from_sorted_iter(rs.iter().map(|&r| r as u32)).unwrap();
            rb.optimize();
            let rsz = rb.serialized_size() as u64;
            t.roaring_rows += rsz;
            t.roaring += rsz + gamma.div_ceil(8);
            t.bound += log2_binom(u, nn.min(u));
            if let Some(a) = all.as_deref_mut() {
                a.lists.push((rows as u32, a.rows.len(), df as u32));
                a.rows.extend(rs.iter().map(|&r| r as u32));
                a.tfs.extend(tfs.iter().map(|&x| x as u32));
            }
            terms.push((term.clone(), df, cf, len as u64));
            prev_term = term;
        }
    }
    // ---- dictionary alternatives
    let off_bits = ef_bits(terms.len() as u64, (s_dict - s_post) as u64 + 1);
    let mut df_col = 0u64;
    for c in terms.chunks(128) {
        df_col += 32 + c.iter().map(|x| gamma_bits(x.1)).sum::<u64>();
    }
    t.col_df += df_col.div_ceil(8);
    t.col_off += off_bits.div_ceil(8);
    for (bi, &bs) in FC_BLOCKS.iter().enumerate() {
        let (mut full, mut nocf, mut cols) = (0u64, 0u64, 0u64);
        let mut off = 0u64;
        for c in terms.chunks(bs) {
            let head = vlen(off) + 8;
            full += head;
            nocf += head;
            cols += 8;
            let mut prev: &[u8] = &[];
            for (j, (term, df, cf, len)) in c.iter().enumerate() {
                let shared = if j == 0 { 0 } else { term.iter().zip(prev).take_while(|(a, b)| a == b).count() };
                let tb = vlen(shared as u64) + vlen((term.len() - shared) as u64) + (term.len() - shared) as u64;
                full += tb + vlen(*df) + vlen(*cf) + vlen(*len);
                nocf += tb + vlen(*df) + vlen(*len);
                cols += tb;
                off += len;
                prev = term;
            }
        }
        t.fc[bi] += full;
        t.fc_nocf[bi] += nocf;
        t.fc_cols[bi] += cols + df_col.div_ceil(8) + off_bits.div_ceil(8);
    }
    let mut mb = fst::MapBuilder::memory();
    for (i, (term, _, _, _)) in terms.iter().enumerate() {
        mb.insert(term, i as u64).unwrap();
    }
    let fst_len = mb.into_inner().unwrap().len() as u64;
    t.fst_only += fst_len;
    t.fst_cols += fst_len + df_col.div_ceil(8) + off_bits.div_ceil(8);
}

// ------------------------------------------------------------------ bench

/// LSB-first bit writer.
#[derive(Default)]
struct BW {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}
impl BW {
    fn bits(&mut self, v: u64, n: u32) {
        if n == 0 {
            return;
        }
        if n > 32 {
            self.bits(v & 0xffff_ffff, 32);
            self.bits(v >> 32, n - 32);
            return;
        }
        self.acc |= (v & ((1u64 << n) - 1)) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn unary(&mut self, mut q: u64) {
        while q >= 32 {
            self.bits(0, 32);
            q -= 32;
        }
        self.bits(1 << q, q as u32 + 1);
    }
    fn gamma(&mut self, v: u32) {
        let k = 31 - v.leading_zeros();
        self.unary(u64::from(k));
        self.bits(u64::from(v), k);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

#[inline(always)]
fn window(buf: &[u8], pos: usize) -> u64 {
    // buffers carry 16 bytes of zero padding at the end
    let b = pos >> 3;
    let w = u64::from_le_bytes(buf[b..b + 8].try_into().unwrap());
    w >> (pos & 7)
}

/// Rice + gamma, one 8-byte window per posting when it fits (as P8).
fn dec_rice(buf: &[u8], mut pos: usize, rows: u32, df: u32, sink: &mut (u64, u64)) -> usize {
    let k = rice_k(rows as u64, df as u64) as u32;
    let mut row: u64 = 0;
    let mut first = true;
    let mask = (1u64 << k) - 1;
    for _ in 0..df {
        let mut w = window(buf, pos);
        // fast path: the whole posting inside one 57-bit window
        let q0 = w.trailing_zeros();
        if q0 + 1 + k <= 56 {
            let rest = w >> (q0 + 1 + k);
            let z = rest.trailing_zeros();
            let used = q0 + 1 + k + 2 * z + 1;
            if used <= 57 {
                let gap = ((q0 as u64) << k) | ((w >> (q0 + 1)) & mask);
                let tf = if z == 0 { 1 } else { (1u64 << z) | ((rest >> (z + 1)) & ((1u64 << z) - 1)) };
                pos += used as usize;
                row = if first { gap } else { row + gap + 1 };
                first = false;
                sink.0 = sink.0.wrapping_add(row);
                sink.1 = sink.1.wrapping_add(tf);
                continue;
            }
        }
        // quotient in unary
        let mut q = 0u64;
        while w == 0 || (w & ((1u64 << 56) - 1)) == 0 {
            // long run of zeros (rare): skip 56 bits at a time
            q += 56;
            pos += 56;
            w = window(buf, pos);
        }
        let z = w.trailing_zeros() as u64;
        q += z;
        pos += z as usize + 1;
        w = window(buf, pos);
        let r = if k == 0 { 0 } else { w & ((1u64 << k) - 1) };
        pos += k as usize;
        let gap = (q << k) | r;
        w = window(buf, pos);
        let tz = w.trailing_zeros();
        pos += tz as usize + 1;
        let tf = if tz == 0 {
            1
        } else {
            let w2 = window(buf, pos);
            pos += tz as usize;
            (1u64 << tz) | (w2 & ((1u64 << tz) - 1))
        };
        row = if first { gap } else { row + gap + 1 };
        first = false;
        sink.0 = sink.0.wrapping_add(row);
        sink.1 = sink.1.wrapping_add(tf);
    }
    pos
}

/// Rice row gaps only (no tf in the stream).
fn dec_rice_rows(buf: &[u8], mut pos: usize, rows: u32, df: u32, sink: &mut u64) -> usize {
    let k = rice_k(rows as u64, df as u64) as u32;
    let mask = (1u64 << k) - 1;
    let mut row: u64 = 0;
    let mut first = true;
    for _ in 0..df {
        let mut w = window(buf, pos);
        let mut q = 0u64;
        let mut z = w.trailing_zeros();
        while z >= 56 {
            q += 56;
            pos += 56;
            w = window(buf, pos);
            z = w.trailing_zeros();
        }
        q += z as u64;
        pos += z as usize + 1;
        let r = if k == 0 { 0 } else if z + 1 + k <= 57 { (w >> (z + 1)) & mask } else { window(buf, pos) & mask };
        pos += k as usize;
        let gap = (q << k) | r;
        row = if first { gap } else { row + gap + 1 };
        first = false;
        *sink = sink.wrapping_add(row);
    }
    pos
}

fn dec_gamma(buf: &[u8], mut pos: usize, df: u32, sink: &mut u64) -> usize {
    for _ in 0..df {
        let w = window(buf, pos);
        let tz = w.trailing_zeros();
        if 2 * tz < 57 {
            let tf = if tz == 0 { 1 } else { (1u64 << tz) | ((w >> (tz + 1)) & ((1u64 << tz) - 1)) };
            pos += 2 * tz as usize + 1;
            *sink = sink.wrapping_add(tf);
            continue;
        }
        pos += tz as usize + 1;
        let tf = if tz == 0 {
            1
        } else {
            let w2 = window(buf, pos);
            pos += tz as usize;
            (1u64 << tz) | (w2 & ((1u64 << tz) - 1))
        };
        *sink = sink.wrapping_add(tf);
    }
    pos
}

/// Elias-Fano: low bits array then high bits (unary buckets), LSB first.
fn enc_ef(rows: &[u32], u: u64, w: &mut BW) {
    let n = rows.len() as u64;
    let l = if u > n { bits(u / n) - 1 } else { 0 } as u32;
    for &r in rows {
        w.bits(u64::from(r), l);
    }
    let mut prev_hi = 0u64;
    for &r in rows {
        let hi = u64::from(r) >> l;
        w.unary(hi - prev_hi);
        prev_hi = hi;
    }
}

fn dec_ef(buf: &[u8], pos: usize, u: u64, df: u32, sink: &mut u64) -> usize {
    let n = df as u64;
    let l = if u > n { bits(u / n) - 1 } else { 0 } as u32;
    let lo_start = pos;
    let mut hp = pos + (n * l as u64) as usize;
    let mut hi = 0u64;
    let mask = if l == 0 { 0 } else { (1u64 << l) - 1 };
    for i in 0..df as usize {
        let mut w = window(buf, hp);
        while w & ((1u64 << 56) - 1) == 0 {
            hi += 56;
            hp += 56;
            w = window(buf, hp);
        }
        let z = w.trailing_zeros() as u64;
        hi += z;
        hp += z as usize + 1;
        let low = if l == 0 { 0 } else { window(buf, lo_start + i * l as usize) & mask };
        *sink = sink.wrapping_add((hi << l) | low);
    }
    hp
}

fn bench(all: &All) -> serde_json::Value {
    use bitpacking::{BitPacker, BitPacker4x};
    let bp = BitPacker4x::new();
    let total: u64 = all.lists.iter().map(|l| l.2 as u64).sum();
    let long: u64 = all.lists.iter().filter(|l| l.2 >= 1024).map(|l| l.2 as u64).sum();
    let want = (
        all.rows.iter().map(|&x| x as u64).fold(0u64, u64::wrapping_add),
        all.tfs.iter().map(|&x| x as u64).fold(0u64, u64::wrapping_add),
    );
    // ---- encode every list in each encoding (in memory)
    let pad = |mut v: Vec<u8>| {
        v.extend_from_slice(&[0u8; 16]);
        v
    };
    // rice+gamma, per list bit offset
    let mut w = BW::default();
    let mut rice_at = Vec::new();
    for &(rows, s, df) in &all.lists {
        // each list byte aligned (as on disk)
        while w.n != 0 {
            w.bits(0, 1);
        }
        rice_at.push(w.out.len() * 8);
        let k = rice_k(rows as u64, df as u64);
        let mut prev: Option<u32> = None;
        for i in s..s + df as usize {
            let r = all.rows[i];
            let gap = u64::from(match prev {
                None => r,
                Some(p) => r - p - 1,
            });
            w.unary(gap >> k);
            w.bits(gap, k as u32);
            w.gamma(all.tfs[i]);
            prev = Some(r);
        }
    }
    let rice = pad(w.finish());
    let mut w = BW::default();
    let mut rr_at = Vec::new();
    for &(rows, s, df) in &all.lists {
        while w.n != 0 {
            w.bits(0, 1);
        }
        rr_at.push(w.out.len() * 8);
        let k = rice_k(rows as u64, df as u64);
        let mut prev: Option<u32> = None;
        for i in s..s + df as usize {
            let r = all.rows[i];
            let gap = u64::from(match prev {
                None => r,
                Some(p) => r - p - 1,
            });
            w.unary(gap >> k);
            w.bits(gap, k as u32);
            prev = Some(r);
        }
    }
    let rice_rows = pad(w.finish());
    // EF rows + gamma tf (separate streams)
    let (mut we, mut wg) = (BW::default(), BW::default());
    let (mut ef_at, mut g_at) = (Vec::new(), Vec::new());
    for &(rows, s, df) in &all.lists {
        while we.n != 0 {
            we.bits(0, 1);
        }
        while wg.n != 0 {
            wg.bits(0, 1);
        }
        ef_at.push(we.out.len() * 8);
        g_at.push(wg.out.len() * 8);
        enc_ef(&all.rows[s..s + df as usize], rows.max(1) as u64, &mut we);
        for i in s..s + df as usize {
            wg.gamma(all.tfs[i]);
        }
    }
    let ef = pad(we.finish());
    let gam = pad(wg.finish());
    // FOR-128 (BitPacker4x) + LEB128 tail; PFor-128 + LEB128 tail
    let mut forb: Vec<u8> = Vec::new();
    let mut pforb: Vec<u8> = Vec::new();
    let (mut for_at, mut pfor_at) = (Vec::new(), Vec::new());
    let mut tmp = [0u8; 4 * 128 + 16];
    for &(_, s, df) in &all.lists {
        for_at.push(forb.len());
        pfor_at.push(pforb.len());
        let rs = &all.rows[s..s + df as usize];
        let tf: Vec<u32> = all.tfs[s..s + df as usize].iter().map(|&x| x - 1).collect();
        let full = rs.len() / 128 * 128;
        let mut initial: Option<u32> = None;
        for b in (0..full).step_by(128) {
            let blk = &rs[b..b + 128];
            let nb = bp.num_bits_strictly_sorted(initial, blk);
            let nt = bp.num_bits(&tf[b..b + 128]);
            forb.push(nb);
            forb.push(nt);
            let n1 = bp.compress_strictly_sorted(initial, blk, &mut tmp, nb);
            forb.extend_from_slice(&tmp[..n1]);
            let n2 = bp.compress(&tf[b..b + 128], &mut tmp, nt);
            forb.extend_from_slice(&tmp[..n2]);
            // pfor: gaps (minus one) at width b, exceptions patched
            let mut gaps = [0u32; 128];
            let mut p = initial;
            for (j, &r) in blk.iter().enumerate() {
                gaps[j] = match p {
                    None => r,
                    Some(q) => r - q - 1,
                };
                p = Some(r);
            }
            let wmax = gaps.iter().map(|&g| bits(g as u64)).max().unwrap();
            let (mut bestb, mut bestc) = (wmax, u64::MAX);
            for bb in 0..=wmax {
                let exc = gaps.iter().filter(|&&g| bits(g as u64) > bb);
                let we = exc.clone().map(|&g| bits((g >> bb) as u64)).max().unwrap_or(0);
                let c = 128 * bb + exc.count() as u64 * (8 + we);
                if c < bestc {
                    bestc = c;
                    bestb = bb;
                }
            }
            let bb = bestb as u8;
            let low: Vec<u32> = gaps.iter().map(|&g| if bb >= 32 { g } else { g & ((1u32 << bb) - 1) }).collect();
            let excs: Vec<(u8, u32)> =
                gaps.iter().enumerate().filter(|(_, g)| bits(**g as u64) > bb as u64).map(|(j, g)| (j as u8, g >> bb)).collect();
            let we = excs.iter().map(|e| bits(e.1 as u64)).max().unwrap_or(0) as u8;
            pforb.push(bb);
            pforb.push(excs.len() as u8);
            pforb.push(we);
            pforb.push(nt);
            let n1 = bp.compress(&low, &mut tmp, bb);
            pforb.extend_from_slice(&tmp[..n1]);
            let n2 = bp.compress(&tf[b..b + 128], &mut tmp, nt);
            pforb.extend_from_slice(&tmp[..n2]);
            for e in &excs {
                pforb.push(e.0);
            }
            let mut ww = BW::default();
            for e in &excs {
                ww.bits(e.1 as u64, we as u32);
            }
            pforb.extend_from_slice(&ww.finish());
            initial = Some(blk[127]);
        }
        let mut prev = initial;
        for i in full..rs.len() {
            let gap = match prev {
                None => rs[i],
                Some(p) => rs[i] - p,
            };
            for out in [&mut forb, &mut pforb] {
                let mut v = gap as u64;
                while v >= 0x80 {
                    out.push((v as u8) | 0x80);
                    v >>= 7;
                }
                out.push(v as u8);
                let mut v = (tf[i] + 1) as u64;
                while v >= 0x80 {
                    out.push((v as u8) | 0x80);
                    v >>= 7;
                }
                out.push(v as u8);
            }
            prev = Some(rs[i]);
        }
    }
    let forb = pad(forb);
    let pforb = pad(pforb);
    // tf alone, FOR-128 blocks (BitPacker4x) + one LEB128 per tail value
    let mut tfb: Vec<u8> = Vec::new();
    let mut tf_at = Vec::new();
    for &(_, s, df) in &all.lists {
        tf_at.push(tfb.len());
        let tf: Vec<u32> = all.tfs[s..s + df as usize].iter().map(|&x| x - 1).collect();
        let full = tf.len() / 128 * 128;
        for b in (0..full).step_by(128) {
            let nt = bp.num_bits(&tf[b..b + 128]);
            tfb.push(nt);
            let n = bp.compress(&tf[b..b + 128], &mut tmp, nt);
            tfb.extend_from_slice(&tmp[..n]);
        }
        for &x in &tf[full..] {
            let mut v = x as u64 + 1;
            while v >= 0x80 {
                tfb.push((v as u8) | 0x80);
                v >>= 7;
            }
            tfb.push(v as u8);
        }
    }
    let tfb = pad(tfb);
    // roaring + gamma tf
    let roar: Vec<roaring::RoaringBitmap> = all
        .lists
        .iter()
        .map(|&(_, s, df)| {
            let mut r = roaring::RoaringBitmap::from_sorted_iter(all.rows[s..s + df as usize].iter().copied()).unwrap();
            r.optimize();
            r
        })
        .collect();
    let sizes = serde_json::json!({
        "rice_gamma": rice.len() - 16, "ef_gamma": ef.len() + gam.len() - 32, "for_vint": forb.len() - 16,
        "pfor_vint": pforb.len() - 16, "tf_for": tfb.len() - 16, "ef_rows": ef.len() - 16, "rice_rows": rice_rows.len() - 16, "gamma_tf": gam.len() - 16,
        "roaring_gamma": roar.iter().map(|r| r.serialized_size()).sum::<usize>() + gam.len() - 16,
    });
    // ---- decoders
    let leb = |b: &[u8], p: &mut usize| -> u64 {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let x = b[*p];
            *p += 1;
            v |= u64::from(x & 0x7f) << s;
            if x < 0x80 {
                return v;
            }
            s += 7;
        }
    };
    let run = |name: &str, only_long: bool| -> (f64, (u64, u64)) {
        let mut best = f64::MAX;
        let mut sum = (0u64, 0u64);
        for _ in 0..3 {
            let mut s = (0u64, 0u64);
            let t0 = Instant::now();
            let mut out = [0u32; 128];
            for (li, &(rows, st, df)) in all.lists.iter().enumerate() {
                if only_long && df < 1024 {
                    continue;
                }
                let _ = st;
                match name {
                    "rice_gamma" => {
                        dec_rice(&rice, rice_at[li], rows, df, &mut s);
                    }
                    "ef_gamma" => {
                        dec_ef(&ef, ef_at[li], rows.max(1) as u64, df, &mut s.0);
                        dec_gamma(&gam, g_at[li], df, &mut s.1);
                    }
                    "rice_rows_only" => {
                        dec_rice_rows(&rice_rows, rr_at[li], rows, df, &mut s.0);
                    }
                    "ef_only" => {
                        dec_ef(&ef, ef_at[li], rows.max(1) as u64, df, &mut s.0);
                    }
                    "gamma_only" => {
                        dec_gamma(&gam, g_at[li], df, &mut s.1);
                    }
                    "tf_for_only" => {
                        let mut p = tf_at[li];
                        for _ in 0..df as usize / 128 {
                            let nt = tfb[p];
                            p += 1;
                            p += bp.decompress(&tfb[p..], &mut out, nt);
                            for &x in &out {
                                s.1 = s.1.wrapping_add(x as u64 + 1);
                            }
                        }
                        for _ in df as usize / 128 * 128..df as usize {
                            s.1 = s.1.wrapping_add(leb(&tfb, &mut p));
                        }
                    }
                    "for_vint" | "pfor_vint" => {
                        let buf = if name == "for_vint" { &forb } else { &pforb };
                        let mut p = if name == "for_vint" { for_at[li] } else { pfor_at[li] };
                        let full = df as usize / 128;
                        let mut initial: Option<u32> = None;
                        for _ in 0..full {
                            if name == "for_vint" {
                                let (nb, nt) = (buf[p], buf[p + 1]);
                                p += 2;
                                p += bp.decompress_strictly_sorted(initial, &buf[p..], &mut out, nb);
                                for &r in &out {
                                    s.0 = s.0.wrapping_add(r as u64);
                                }
                                initial = Some(out[127]);
                                p += bp.decompress(&buf[p..], &mut out, nt);
                                for &x in &out {
                                    s.1 = s.1.wrapping_add(x as u64 + 1);
                                }
                            } else {
                                let (bb, ne, we, nt) = (buf[p], buf[p + 1] as usize, buf[p + 2], buf[p + 3]);
                                p += 4;
                                p += bp.decompress(&buf[p..], &mut out, bb);
                                let mut tfo = [0u32; 128];
                                p += bp.decompress(&buf[p..], &mut tfo, nt);
                                let pos0 = p;
                                p += ne;
                                let mut bpos = p * 8;
                                for e in 0..ne {
                                    let j = buf[pos0 + e] as usize;
                                    let hi = window(buf, bpos) & ((1u64 << we) - 1);
                                    bpos += we as usize;
                                    out[j] |= (hi as u32) << bb;
                                }
                                p = bpos.div_ceil(8);
                                let mut r = initial.map_or(0u64, |x| x as u64 + 1);
                                for (j, &g) in out.iter().enumerate() {
                                    r = if j == 0 { r + g as u64 } else { r + g as u64 + 1 };
                                    s.0 = s.0.wrapping_add(r);
                                }
                                initial = Some(r as u32);
                                for &x in &tfo {
                                    s.1 = s.1.wrapping_add(x as u64 + 1);
                                }
                            }
                        }
                        let mut prev = initial.map(u64::from);
                        for _ in full * 128..df as usize {
                            let g = leb(buf, &mut p);
                            let tf = leb(buf, &mut p);
                            let r = match prev {
                                None => g,
                                Some(q) => q + g,
                            };
                            prev = Some(r);
                            s.0 = s.0.wrapping_add(r);
                            s.1 = s.1.wrapping_add(tf);
                        }
                    }
                    "roaring_gamma" => {
                        for r in roar[li].iter() {
                            s.0 = s.0.wrapping_add(r as u64);
                        }
                        dec_gamma(&gam, g_at[li], df, &mut s.1);
                    }
                    _ => unreachable!(),
                }
            }
            let ns = t0.elapsed().as_nanos() as f64;
            best = best.min(ns);
            sum = s;
        }
        let n = if only_long { long } else { total };
        (best / n.max(1) as f64, sum)
    };
    let mut res = serde_json::Map::new();
    for name in ["rice_gamma", "ef_gamma", "for_vint", "pfor_vint", "roaring_gamma", "rice_rows_only", "ef_only", "gamma_only", "tf_for_only"] {
        let (all_ns, s) = run(name, false);
        match name {
            "ef_only" | "rice_rows_only" => assert_eq!(s.0, want.0, "{name}: decode mismatch"),
            "gamma_only" | "tf_for_only" => assert_eq!(s.1, want.1, "{name}: decode mismatch"),
            _ => assert_eq!(s, want, "{name}: decode mismatch"),
        }
        let (long_ns, _) = run(name, true);
        res.insert(name.into(), serde_json::json!({"ns_per_posting": all_ns, "ns_per_posting_long": long_ns}));
    }
    serde_json::json!({"postings": total, "postings_in_long_lists": long, "sizes": sizes, "decode": res,
                       "loadavg": std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim()})
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut kinds = Vec::new();
    let mut dirs = Vec::new();
    let mut bench_on = false;
    let mut json_out = None;
    let mut label = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--kinds" => {
                kinds = std::fs::read(&args[i + 1]).unwrap();
                i += 1;
            }
            "--bench" => bench_on = true,
            "--json" => {
                json_out = Some(args[i + 1].clone());
                i += 1;
            }
            "--label" => {
                label = args[i + 1].clone();
                i += 1;
            }
            d => dirs.push(d.to_string()),
        }
        i += 1;
    }
    let mut files = Vec::new();
    for d in &dirs {
        walk(Path::new(d), &mut files);
    }
    files.sort();
    let mut t = Tot::default();
    let mut all = bench_on.then(All::default);
    // one store = the segments of one directory (an agent store)
    let mut i = 0;
    while i < files.len() {
        let dir = files[i].parent().map(Path::to_path_buf);
        let j = i + files[i..].iter().take_while(|f| f.parent().map(Path::to_path_buf) == dir).count();
        let mut store_df = HashMap::new();
        let mut store_rows = 0;
        for f in &files[i..j] {
            store_rows += dict_terms(&std::fs::read(f).unwrap(), &mut store_df);
        }
        for f in &files[i..j] {
            let map = std::fs::read(f).unwrap();
            probe(&map, &kinds, &mut t, all.as_mut(), (&store_df, store_rows));
        }
        i = j;
    }
    let total_tf: u64 = t.tf_hist.values().sum();
    let h_tf: f64 = t
        .tf_hist
        .values()
        .map(|&c| {
            let p = c as f64 / total_tf as f64;
            -p.log2() * c as f64
        })
        .sum::<f64>();
    let names = ["rows", "ids", "keys", "time", "multi", "postings", "dict", "dindex"];
    println!("segments {} rows {} (indexed {}) file bytes {} (largest segment {} rows)", t.segs, t.rows, t.indexed_rows, t.file, t.largest_seg_rows);
    for (n, b) in names.iter().zip(t.sec.iter()) {
        println!("  {n:<9} {b:>12} ({:.1}%, {:.1} B/row)", 100.0 * *b as f64 / t.file as f64, *b as f64 / t.rows as f64);
    }
    println!("  hdr+ftr   {:>12}", t.header_footer);
    println!("lists {} (df=1: {}), postings {}", t.lists, t.df1, t.postings);
    let p = t.postings as f64;
    for (n, b) in [
        ("actual", t.actual as f64),
        ("leb", t.leb as f64),
        ("for128", t.forb as f64),
        ("for-vint", t.for_vint as f64),
        ("pfor-vint", t.pfor as f64),
        ("ef+gamma", t.ef as f64),
        ("ef+width", t.efw as f64),
        ("pef+gamma", t.pef as f64),
        ("roaring+g", t.roaring as f64),
    ] {
        println!("  {n:<9} {:>12.0} B  {:.2} bits/posting", b, 8.0 * b / p);
    }
    println!("  bound     {:>12.0} B  {:.2} bits/posting (rows {:.2} + tf {:.2})", (t.bound + h_tf) / 8.0, (t.bound + h_tf) / p, t.bound / p, h_tf / p);
    println!(
        "  rice split: rows {:.0} B ({:.2} bits/posting), tf {:.0} B ({:.2}), padding {:.0} B",
        t.rice_bits as f64 / 8.0,
        t.rice_bits as f64 / p,
        t.gamma_bits as f64 / 8.0,
        t.gamma_bits as f64 / p,
        t.actual as f64 - (t.rice_bits + t.gamma_bits) as f64 / 8.0
    );
    let mut h: Vec<_> = t.tf_hist.iter().collect();
    h.sort();
    println!("  tf: {:?}", h.iter().take(6).map(|(k, v)| (**k, format!("{:.1}%", 100.0 * **v as f64 / total_tf as f64))).collect::<Vec<_>>());
    let kinds_json: serde_json::Map<String, serde_json::Value> = (0..KINDS)
        .filter(|&k| t.k_rows[k] > 0)
        .map(|k| {
            (
                KIND_NAMES[k].to_string(),
                serde_json::json!({
                    "rows": t.k_rows[k], "indexed_rows": t.k_irows[k], "text_bytes": t.k_text[k],
                    "postings": t.k_post[k], "rice_bytes": t.k_rice[k] / 8, "gamma_bytes": t.k_gamma[k] / 8,
                    "excl_terms_dict_bytes": t.k_excl_dict[k], "excl_terms_list_bytes": t.k_excl_list[k],
                    "dict_share_bytes": t.k_dict_share[k] as u64,
                }),
            )
        })
        .collect();
    let df_json: Vec<serde_json::Value> = DF_FRACS
        .iter()
        .enumerate()
        .map(|(i, f)| serde_json::json!({"frac": f,
            "segment": {"terms": t.df_terms[i], "postings": t.df_post[i], "list_bytes": t.df_bytes[i], "dict_bytes": t.df_dict[i]},
            "store": {"terms": t.sdf_terms[i], "postings": t.sdf_post[i], "list_bytes": t.sdf_bytes[i], "dict_bytes": t.sdf_dict[i]}}))
        .collect();
    let lean_fields: serde_json::Map<String, serde_json::Value> = ["valid_at", "recorded_at", "length", "scope", "text_len", "flags", "id_prefix"]
        .iter()
        .zip(t.lean_rows_fields.iter())
        .map(|(n, v)| (n.to_string(), serde_json::json!(v)))
        .collect();
    let mut out = serde_json::json!({
        "label": label, "dirs": dirs, "segments": t.segs, "rows": t.rows, "indexed_rows": t.indexed_rows, "file_bytes": t.file,
        "largest_segment_rows": t.largest_seg_rows,
        "sections": names.iter().zip(t.sec.iter()).map(|(n, b)| (n.to_string(), serde_json::json!(b))).collect::<serde_json::Map<_, _>>(),
        "header_footer": t.header_footer,
        "postings": {"lists": t.lists, "df1_lists": t.df1, "postings": t.postings, "cf_sum": t.cf_sum,
            "actual": t.actual, "rice_row_bits": t.rice_bits, "gamma_tf_bits": t.gamma_bits,
            "leb": t.leb, "for128": t.forb, "for_vint": t.for_vint, "pfor_vint": t.pfor, "ef_gamma": t.ef, "ef_width": t.efw,
            "pef_gamma": t.pef, "roaring_gamma": t.roaring, "roaring_rows_only": t.roaring_rows,
            "rice_rows_only": t.rice_rows_only, "ef_rows_only": t.ef_rows_only, "tf_for128": t.tf_for128,
            "bound_bytes": (t.bound + h_tf) / 8.0, "bound_rows_bits": t.bound, "tf_entropy_bits": h_tf,
            "positions_estimate_bytes": t.positions_bits / 8.0,
            "skip_lists_ge1024": t.skip_lists, "skip_entries": t.skip_entries},
        "df": df_json,
        "dict": {"terms": t.d_terms, "term_bytes": t.d_term_bytes, "df_bytes": t.d_df, "cf_bytes": t.d_cf, "len_bytes": t.d_len,
            "block_offset_bytes": t.d_block_off, "df1_entry_bytes": t.d_df1_entry_bytes,
            "front_coded": FC_BLOCKS.iter().zip(t.fc.iter()).map(|(b, v)| (b.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
            "front_coded_no_cf": FC_BLOCKS.iter().zip(t.fc_nocf.iter()).map(|(b, v)| (b.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
            "front_coded_terms_plus_columns": FC_BLOCKS.iter().zip(t.fc_cols.iter()).map(|(b, v)| (b.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
            "fst_only": t.fst_only, "fst_plus_columns": t.fst_cols, "df_column": t.col_df, "offsets_ef": t.col_off},
        "fixed": {"rows_lean": t.lean_rows,
            "rows_lean_fields": lean_fields,
            "recorded_eq_valid_rows": t.rec_eq_valid_rows, "numeric_id_rows": t.numeric_id_rows,
            "id_monotone_breaks": t.id_monotone_breaks, "ids_lean": t.ids_lean, "ids_fp64": t.ids_fp64,
            "key_ne_id_rows": t.key_ne_id_rows, "time_not_identity": t.time_not_identity, "time_runs": t.time_runs},
        "kinds": kinds_json,
    });
    println!("{}", serde_json::to_string_pretty(&out["df"]).unwrap());
    println!("dict {}", out["dict"]);
    println!("fixed {}", out["fixed"]);
    println!("kinds {}", out["kinds"]);
    if let Some(a) = &all {
        let b = bench(a);
        println!("bench {b}");
        out["bench"] = b;
    }
    if let Some(o) = json_out {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(o).unwrap();
        writeln!(f, "{out}").unwrap();
    }
}
