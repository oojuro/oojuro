//! Native derived lexical format, version 1 (not oracle v3 compatible).
//!
//! Source map: foundation SPEC.md 4.2, nine/src/rows.rs `encode_block`
//! (128-row minimum/base and minimum-width LSB packing), and
//! nine/src/segment.rs `rice_k`, `encode_rice`, `BitWriter::gamma`
//! (gap-minus-one Rice with LSB remainders and Elias gamma TF), and its
//! 32-term byte-prefix dictionary. These algorithms are implemented locally;
//! no oracle runtime, Events, I/O, encryption or authority state is involved.
//!
//! Header (192 bytes, LE): magic[8], version/header-size/start/rows/terms/
//! row-blocks/dict-blocks/reserved (u32), six u64 values (rows, postings,
//! dictionary, dictionary-index, footer offsets, posting count), length/scope
//! totals (u64), then 88 zero bytes. Rows have 128-row FOR blocks followed by
//! a u32 relative offset table (blocks + 1). Each block has count/reserved
//! u16, ID/length/scope bit widths and reserved byte, bases u64/u32/u32,
//! independently padded bit streams, then exact 96-bit fingerprints.
//! Dictionary blocks restart prefixes every 32 terms: postings-offset u32,
//! then canonical LEB128 prefix/suffix lengths, suffix bytes, df/cf/list bytes.
//! A u32 relative offset index follows. Footer: CRC32 u32, reserved u32,
//! end-magic[8]. All byte padding and reserved fields must be zero.
//!
//! Both encoded bytes and the accounted decoded representation are <=16 MiB.
//! Decoded accounting charges 32/row, 64/term, term bytes and 8/posting. This
//! explicit expansion bound also applies to encode; highly compressible inputs
//! outside it must be split into smaller segments by the caller.
use crate::lexical::{Error, Result};
use std::collections::BTreeMap;

pub(crate) const MAX_ROWS: usize = 8192;
pub(crate) const MAX_BYTES: usize = 16 << 20;
const HEADER: usize = 192;
const FOOTER: usize = 16;
const BLOCK: usize = 128;
const DICT_BLOCK: usize = 32;
const MAGIC: &[u8; 8] = b"OOJLEX01";
const END: &[u8; 8] = b"OOJLEXND";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub id: u64,
    pub length: u32,
    pub scope: u32,
    pub fingerprint: [u8; 12],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub start: u32,
    pub rows: Vec<Row>,
    pub postings: BTreeMap<String, Vec<(u32, u32)>>,
}
fn invalid<T>() -> Result<T> {
    Err(Error::Invalid)
}
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(Error::Invalid)
}
fn charge(budget: &mut usize, n: usize) -> Result<()> {
    *budget = add(*budget, n)?;
    if *budget > MAX_BYTES {
        return invalid();
    }
    Ok(())
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<()> {
    v.try_reserve(n).map_err(|_| Error::Unavailable)
}
#[derive(Default)]
struct Out(Vec<u8>);
impl Out {
    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        if add(self.0.len(), b.len())? > MAX_BYTES {
            return invalid();
        }
        reserve(&mut self.0, b.len())?;
        self.0.extend_from_slice(b);
        Ok(())
    }
    fn byte(&mut self, b: u8) -> Result<()> {
        self.bytes(&[b])
    }
    fn u16(&mut self, n: u16) -> Result<()> {
        self.bytes(&n.to_le_bytes())
    }
    fn u32(&mut self, n: u32) -> Result<()> {
        self.bytes(&n.to_le_bytes())
    }
    fn u64(&mut self, n: u64) -> Result<()> {
        self.bytes(&n.to_le_bytes())
    }
    fn var(&mut self, mut n: u64) -> Result<()> {
        loop {
            let b = (n & 127) as u8;
            n >>= 7;
            self.byte(b | if n == 0 { 0 } else { 128 })?;
            if n == 0 {
                return Ok(());
            }
        }
    }
}
struct Read<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Read<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = add(self.pos, n)?;
        let b = self.bytes.get(self.pos..end).ok_or(Error::Invalid)?;
        self.pos = end;
        Ok(b)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn var(&mut self) -> Result<u64> {
        let mut n = 0u64;
        for i in 0..10 {
            let b = self.byte()?;
            if i == 9 && b > 1 {
                return invalid();
            }
            n |= u64::from(b & 127) << (7 * i);
            if b & 128 == 0 {
                if i != 0 && b == 0 {
                    return invalid();
                }
                return Ok(n);
            }
        }
        invalid()
    }
    fn done(&self) -> Result<()> {
        if self.pos != self.bytes.len() {
            return invalid();
        }
        Ok(())
    }
}
fn usize_of(n: u64) -> Result<usize> {
    usize::try_from(n).map_err(|_| Error::Invalid)
}
fn u32_of(n: u64) -> Result<u32> {
    u32::try_from(n).map_err(|_| Error::Invalid)
}
fn width(n: u64) -> u32 {
    64 - n.leading_zeros()
}
struct BitsOut {
    out: Out,
    byte: u8,
    used: u8,
}
impl BitsOut {
    fn new() -> Self {
        Self {
            out: Out::default(),
            byte: 0,
            used: 0,
        }
    }
    fn bits(&mut self, n: u64, count: u32) -> Result<()> {
        for i in 0..count {
            self.byte |= (((n >> i) & 1) as u8) << self.used;
            self.used += 1;
            if self.used == 8 {
                self.out.byte(self.byte)?;
                self.byte = 0;
                self.used = 0;
            }
        }
        Ok(())
    }
    fn unary(&mut self, n: u64) -> Result<()> {
        for _ in 0..n {
            self.bits(0, 1)?;
        }
        self.bits(1, 1)
    }
    fn gamma(&mut self, n: u32) -> Result<()> {
        if n == 0 {
            return invalid();
        }
        let k = 31 - n.leading_zeros();
        self.unary(u64::from(k))?;
        self.bits(u64::from(n), k)
    }
    fn finish(mut self) -> Result<Vec<u8>> {
        if self.used != 0 {
            self.out.byte(self.byte)?;
        }
        Ok(self.out.0)
    }
}
struct Bits<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn bits(&mut self, count: u32) -> Result<u64> {
        if count > 64 || add(self.pos, count as usize)? > self.bytes.len() * 8 {
            return invalid();
        }
        let mut n = 0;
        for i in 0..count {
            n |= u64::from((self.bytes[self.pos / 8] >> (self.pos % 8)) & 1) << i;
            self.pos += 1;
        }
        Ok(n)
    }
    fn unary(&mut self, max: u64) -> Result<u64> {
        let mut n = 0;
        while self.bits(1)? == 0 {
            if n == max {
                return invalid();
            }
            n += 1;
        }
        Ok(n)
    }
    fn gamma(&mut self) -> Result<u32> {
        let k = self.unary(31)? as u32;
        Ok(((1u64 << k) | self.bits(k)?) as u32)
    }
    fn done(&mut self) -> Result<()> {
        let left = self.bytes.len() * 8 - self.pos;
        if left >= 8 || self.bits(left as u32)? != 0 {
            return invalid();
        }
        Ok(())
    }
}
fn rice_k(rows: u32, df: u32) -> u32 {
    let q = rows / df.max(1);
    if q <= 1 { 0 } else { 31 - q.leading_zeros() }
}
fn prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}
const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut byte = 0;
    while byte < table.len() {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(*byte)) & 255) as usize];
    }
    !crc
}
fn field(out: &mut Out, vals: &[u64], base: u64, w: u32) -> Result<()> {
    let mut b = BitsOut::new();
    for &n in vals {
        b.bits(n - base, w)?;
    }
    out.bytes(&b.finish()?)
}
fn decode_field(r: &mut Read<'_>, n: usize, base: u64, w: u32, max: u64) -> Result<Vec<u64>> {
    if w > width(max) {
        return invalid();
    }
    let bytes = r.take((n * w as usize).div_ceil(8))?;
    let mut bits = Bits::new(bytes);
    let mut vals = Vec::new();
    reserve(&mut vals, n)?;
    let mut min = u64::MAX;
    let mut delta = 0u64;
    for _ in 0..n {
        let d = bits.bits(w)?;
        let v = base
            .checked_add(d)
            .filter(|v| *v <= max)
            .ok_or(Error::Invalid)?;
        min = min.min(v);
        delta = delta.max(d);
        vals.push(v);
    }
    bits.done()?;
    if min != base || width(delta) != w {
        return invalid();
    }
    Ok(vals)
}

pub(crate) fn encode(s: &Segment) -> Result<Vec<u8>> {
    let n = s.rows.len();
    if n > MAX_ROWS || s.start.checked_add(n as u32).is_none() {
        return invalid();
    }
    let mut budget = n * 32;
    let mut count = 0u64;
    let mut last = 0u64;
    let mut total_length = 0u64;
    let mut total_scope = 0u64;
    for row in &s.rows {
        if row.id == 0 || row.id <= last {
            return invalid();
        }
        last = row.id;
        total_length += u64::from(row.length);
        total_scope += u64::from(row.scope);
    }
    for (term, list) in &s.postings {
        if term.is_empty() || list.is_empty() || list.len() > n {
            return invalid();
        }
        charge(&mut budget, add(64, term.len())?)?;
        charge(
            &mut budget,
            list.len().checked_mul(8).ok_or(Error::Invalid)?,
        )?;
        let mut prev = None;
        for &(row, tf) in list {
            if row as usize >= n || tf == 0 || prev.is_some_and(|p| row <= p) {
                return invalid();
            }
            prev = Some(row);
        }
        count += list.len() as u64;
    }
    let mut out = Out::default();
    out.bytes(&[0; HEADER])?;
    let row_start = out.0.len();
    let mut offsets = vec![0u32];
    for rows in s.rows.chunks(BLOCK) {
        out.u16(rows.len() as u16)?;
        out.u16(0)?;
        let ids: Vec<u64> = rows.iter().map(|r| r.id).collect();
        let lens: Vec<u64> = rows.iter().map(|r| u64::from(r.length)).collect();
        let scopes: Vec<u64> = rows.iter().map(|r| u64::from(r.scope)).collect();
        let stats = |v: &[u64]| {
            let base = *v.iter().min().unwrap_or(&0);
            (base, width(v.iter().max().copied().unwrap_or(0) - base))
        };
        let (ib, iw) = stats(&ids);
        let (lb, lw) = stats(&lens);
        let (sb, sw) = stats(&scopes);
        out.byte(iw as u8)?;
        out.byte(lw as u8)?;
        out.byte(sw as u8)?;
        out.byte(0)?;
        out.u64(ib)?;
        out.u32(lb as u32)?;
        out.u32(sb as u32)?;
        field(&mut out, &ids, ib, iw)?;
        field(&mut out, &lens, lb, lw)?;
        field(&mut out, &scopes, sb, sw)?;
        for row in rows {
            out.bytes(&row.fingerprint)?;
        }
        offsets.push((out.0.len() - row_start) as u32);
    }
    for off in offsets {
        out.u32(off)?;
    }
    let postings_start = out.0.len();
    let mut dict = Out::default();
    let mut index = Vec::new();
    let mut previous: &[u8] = &[];
    for (i, (term, list)) in s.postings.iter().enumerate() {
        if i % DICT_BLOCK == 0 {
            index.push(dict.0.len() as u32);
            dict.u32((out.0.len() - postings_start) as u32)?;
            previous = &[];
        }
        let p = prefix(previous, term.as_bytes());
        dict.var(p as u64)?;
        dict.var((term.len() - p) as u64)?;
        dict.bytes(&term.as_bytes()[p..])?;
        let mut bits = BitsOut::new();
        let mut prev = None;
        let mut cf = 0u64;
        let k = rice_k(n as u32, list.len() as u32);
        for &(row, tf) in list {
            let gap = prev.map_or(row, |p| row - p - 1);
            bits.unary(u64::from(gap >> k))?;
            bits.bits(u64::from(gap), k)?;
            bits.gamma(tf)?;
            cf += u64::from(tf);
            prev = Some(row);
        }
        let packed = bits.finish()?;
        dict.var(list.len() as u64)?;
        dict.var(cf)?;
        dict.var(packed.len() as u64)?;
        out.bytes(&packed)?;
        previous = term.as_bytes();
    }
    let dict_start = out.0.len();
    out.bytes(&dict.0)?;
    let index_start = out.0.len();
    for &off in &index {
        out.u32(off)?;
    }
    let footer_start = out.0.len();
    let mut h = Out::default();
    h.bytes(MAGIC)?;
    for v in [
        1,
        HEADER as u32,
        s.start,
        n as u32,
        s.postings.len() as u32,
        n.div_ceil(BLOCK) as u32,
        index.len() as u32,
        0,
    ] {
        h.u32(v)?;
    }
    for v in [
        row_start as u64,
        postings_start as u64,
        dict_start as u64,
        index_start as u64,
        footer_start as u64,
        count,
        total_length,
        total_scope,
    ] {
        h.u64(v)?;
    }
    out.0[..h.0.len()].copy_from_slice(&h.0);
    out.u32(crc32(&out.0))?;
    out.u32(0)?;
    out.bytes(END)?;
    Ok(out.0)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Segment> {
    if bytes.len() < HEADER + FOOTER || bytes.len() > MAX_BYTES {
        return invalid();
    }
    let footer = bytes.len() - FOOTER;
    let mut f = Read::new(&bytes[footer..]);
    if f.u32()? != crc32(&bytes[..footer]) || f.u32()? != 0 || f.take(8)? != END {
        return invalid();
    }
    let mut h = Read::new(&bytes[..HEADER]);
    if h.take(8)? != MAGIC || h.u32()? != 1 || h.u32()? != HEADER as u32 {
        return invalid();
    }
    let start = h.u32()?;
    let n = h.u32()? as usize;
    let terms = h.u32()? as usize;
    let row_blocks = h.u32()? as usize;
    let dict_blocks = h.u32()? as usize;
    if h.u32()? != 0
        || n > MAX_ROWS
        || start.checked_add(n as u32).is_none()
        || row_blocks != n.div_ceil(BLOCK)
        || dict_blocks != terms.div_ceil(DICT_BLOCK)
    {
        return invalid();
    }
    let row_start = usize_of(h.u64()?)?;
    let post_start = usize_of(h.u64()?)?;
    let dict_start = usize_of(h.u64()?)?;
    let index_start = usize_of(h.u64()?)?;
    let end = usize_of(h.u64()?)?;
    let expected_count = h.u64()?;
    let expected_length = h.u64()?;
    let expected_scope = h.u64()?;
    if h.take(HEADER - h.pos)?.iter().any(|b| *b != 0) {
        return invalid();
    }
    if row_start != HEADER
        || row_start > post_start
        || post_start > dict_start
        || dict_start > index_start
        || index_start > end
        || end != footer
        || terms > MAX_BYTES / 64
        || end - index_start != dict_blocks.checked_mul(4).ok_or(Error::Invalid)?
    {
        return invalid();
    }
    let mut budget = n * 32;
    charge(&mut budget, terms * 64)?;
    let row_table_size = (row_blocks + 1) * 4;
    if post_start - row_start < row_table_size {
        return invalid();
    }
    let table_start = post_start - row_table_size;
    let mut tab = Read::new(&bytes[table_start..post_start]);
    let mut previous = tab.u32()? as usize;
    if previous != 0 {
        return invalid();
    }
    let mut rows = Vec::new();
    reserve(&mut rows, n)?;
    let mut last_id = 0;
    let mut total_length = 0;
    let mut total_scope = 0;
    for i in 0..row_blocks {
        let next = tab.u32()? as usize;
        if next <= previous || next > table_start - row_start {
            return invalid();
        }
        let mut r = Read::new(&bytes[row_start + previous..row_start + next]);
        let count = (n - i * BLOCK).min(BLOCK);
        if r.u16()? as usize != count || r.u16()? != 0 {
            return invalid();
        }
        let iw = u32::from(r.byte()?);
        let lw = u32::from(r.byte()?);
        let sw = u32::from(r.byte()?);
        if r.byte()? != 0 {
            return invalid();
        }
        let ib = r.u64()?;
        let lb = u64::from(r.u32()?);
        let sb = u64::from(r.u32()?);
        let ids = decode_field(&mut r, count, ib, iw, u64::MAX)?;
        let lens = decode_field(&mut r, count, lb, lw, u64::from(u32::MAX))?;
        let scopes = decode_field(&mut r, count, sb, sw, u64::from(u32::MAX))?;
        for j in 0..count {
            if ids[j] == 0 || ids[j] <= last_id {
                return invalid();
            }
            let mut fingerprint = [0; 12];
            fingerprint.copy_from_slice(r.take(12)?);
            rows.push(Row {
                id: ids[j],
                length: lens[j] as u32,
                scope: scopes[j] as u32,
                fingerprint,
            });
            last_id = ids[j];
            total_length += lens[j];
            total_scope += scopes[j];
        }
        r.done()?;
        previous = next;
    }
    if previous != table_start - row_start
        || total_length != expected_length
        || total_scope != expected_scope
    {
        return invalid();
    }
    let mut postings = BTreeMap::new();
    let mut dictionary = Read::new(&bytes[dict_start..index_start]);
    let mut index = Read::new(&bytes[index_start..end]);
    let mut post_pos = 0usize;
    let mut previous = String::new();
    let mut count = 0u64;
    for i in 0..terms {
        let block_start = i % DICT_BLOCK == 0;
        if block_start
            && (index.u32()? as usize != dictionary.pos || dictionary.u32()? as usize != post_pos)
        {
            return invalid();
        }
        let p = usize_of(dictionary.var()?)?;
        let suffix_len = usize_of(dictionary.var()?)?;
        if (block_start && p != 0) || p > previous.len() {
            return invalid();
        }
        let size = add(p, suffix_len)?;
        if size == 0 {
            return invalid();
        }
        charge(&mut budget, size)?;
        let suffix = dictionary.take(suffix_len)?;
        let mut term = Vec::new();
        reserve(&mut term, size)?;
        term.extend_from_slice(&previous.as_bytes()[..p]);
        term.extend_from_slice(suffix);
        if (!block_start && prefix(previous.as_bytes(), &term) != p)
            || (i != 0 && term.as_slice() <= previous.as_bytes())
        {
            return invalid();
        }
        let term = String::from_utf8(term).map_err(|_| Error::Invalid)?;
        let df = usize_of(dictionary.var()?)?;
        let expected_cf = dictionary.var()?;
        let list_bytes = usize_of(dictionary.var()?)?;
        if df == 0 || df > n || list_bytes == 0 {
            return invalid();
        }
        charge(&mut budget, df * 8)?;
        let post_end = add(post_pos, list_bytes)?;
        if post_end > dict_start - post_start {
            return invalid();
        }
        let mut bits = Bits::new(&bytes[post_start + post_pos..post_start + post_end]);
        let k = rice_k(n as u32, df as u32);
        let mut list = Vec::new();
        reserve(&mut list, df)?;
        let mut prev: Option<u32> = None;
        let mut cf = 0u64;
        for _ in 0..df {
            let q = bits.unary((n as u64) >> k)?;
            let gap = (q << k) | bits.bits(k)?;
            let row = prev
                .map_or(0, |p| u64::from(p) + 1)
                .checked_add(gap)
                .ok_or(Error::Invalid)?;
            if row >= n as u64 {
                return invalid();
            }
            let tf = bits.gamma()?;
            cf += u64::from(tf);
            list.push((u32_of(row)?, tf));
            prev = Some(row as u32);
        }
        bits.done()?;
        if cf != expected_cf {
            return invalid();
        }
        count = count.checked_add(df as u64).ok_or(Error::Invalid)?;
        previous = term.clone();
        postings.insert(term, list);
        post_pos = post_end;
    }
    dictionary.done()?;
    index.done()?;
    if post_pos != dict_start - post_start || count != expected_count {
        return invalid();
    }
    Ok(Segment {
        start,
        rows,
        postings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(n: usize) -> Segment {
        let rows = (0..n)
            .map(|i| Row {
                id: if i + 1 == n {
                    u64::MAX
                } else {
                    1 + i as u64 * 7
                },
                length: if i % 3 == 0 { u32::MAX } else { i as u32 },
                scope: (i % 11) as u32,
                fingerprint: [i as u8; 12],
            })
            .collect();
        let mut postings = BTreeMap::new();
        if n != 0 {
            for i in 0..65 {
                let list = (0..n)
                    .filter(|r| r % (i + 1) == 0)
                    .map(|r| (r as u32, if r % 3 == 0 { u32::MAX } else { 1 + r as u32 }))
                    .collect();
                postings.insert(format!("shared-prefix-{i:03}-é"), list);
            }
        }
        Segment {
            start: 13,
            rows,
            postings,
        }
    }
    fn reseal(b: &mut [u8]) {
        let end = b.len() - FOOTER;
        let crc = crc32(&b[..end]);
        b[end..end + 4].copy_from_slice(&crc.to_le_bytes());
    }
    fn set32(b: &mut [u8], at: usize, n: u32) {
        b[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    fn offset(b: &[u8], at: usize) -> usize {
        Read::new(&b[at..at + 8]).u64().unwrap() as usize
    }
    #[test]
    fn roundtrip_blocks_full_width_ids_and_gamma_tf() {
        for n in [0, 1, 2, 127, 128, 129, MAX_ROWS] {
            let s = sample(n);
            let b = encode(&s).unwrap();
            assert_eq!(decode(&b).unwrap(), s);
            assert_eq!(encode(&decode(&b).unwrap()).unwrap(), b);
        }
    }
    #[test]
    fn oracle_posting_bit_order_and_crc_vector() {
        let mut bits = BitsOut::new();
        // Oracle encode_rice(8, [(0,1),(3,2)]), k=2:
        // 1,00,1 | 1,01,01,0 => LSB bytes 0x59,0x01.
        for (gap, tf) in [(0, 1), (2, 2)] {
            bits.unary(gap >> 2).unwrap();
            bits.bits(gap, 2).unwrap();
            bits.gamma(tf).unwrap();
        }
        assert_eq!(bits.finish().unwrap(), vec![0x59, 0x01]);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!((rice_k(8, 2), rice_k(8192, 1), rice_k(1, 1)), (2, 13, 0));
    }
    #[test]
    fn table_crc_matches_frozen_bitwise_format_across_lengths_and_bytes() {
        fn bitwise(bytes: &[u8]) -> u32 {
            let mut crc = !0u32;
            for byte in bytes {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
                }
            }
            !crc
        }
        // Include all byte values and block boundaries with deterministic mixed data.
        let bytes = (0..32769)
            .map(|i| ((i * 131 + i / 257) & 255) as u8)
            .collect::<Vec<_>>();
        for length in (0..=257).chain([511, 512, 513, 8191, 8192, 8193, 32768, 32769]) {
            assert_eq!(
                crc32(&bytes[..length]),
                bitwise(&bytes[..length]),
                "{length}"
            );
        }
        // Encoding must retain the old footer checksum, not merely round-trip itself.
        let encoded = encode(&sample(129)).unwrap();
        let end = encoded.len() - FOOTER;
        assert_eq!(
            Read::new(&encoded[end..]).u32().unwrap(),
            bitwise(&encoded[..end])
        );
    }
    #[test]
    fn rejects_all_truncations_and_single_bit_corruption() {
        let b = encode(&sample(3)).unwrap();
        for len in 0..b.len() {
            assert!(decode(&b[..len]).is_err(), "prefix {len}");
        }
        for i in 0..b.len() {
            let mut bad = b.clone();
            bad[i] ^= 1;
            assert!(decode(&bad).is_err(), "byte {i}");
        }
    }
    #[test]
    fn rejects_authenticated_structural_corruption() {
        let b = encode(&sample(3)).unwrap();
        for at in [
            8,
            12,
            24,
            28,
            32,
            36,
            40,
            48,
            56,
            64,
            72,
            80,
            88,
            96,
            104,
            191,
            HEADER + 2,
            HEADER + 7,
        ] {
            let mut bad = b.clone();
            bad[at] ^= 0x80;
            reseal(&mut bad);
            assert!(decode(&bad).is_err(), "at {at}");
        }
        let ds = offset(&b, 56);
        let ps = offset(&b, 48);
        let ix = offset(&b, 64);
        for (at, value) in [(ds + 4, 1), (ix, 1), (ps, 0)] {
            let mut bad = b.clone();
            bad[at] = value;
            reseal(&mut bad);
            assert!(decode(&bad).is_err(), "at {at}");
        }
        let mut bad = b.clone();
        set32(&mut bad, 20, u32::MAX);
        reseal(&mut bad);
        assert!(decode(&bad).is_err());
        let mut bad = b.clone();
        bad.extend_from_slice(&[0]);
        assert!(decode(&bad).is_err());
    }
    #[test]
    fn rejects_padding_overflow_and_invalid_utf8() {
        let s = Segment {
            start: 0,
            rows: vec![Row {
                id: 1,
                length: 0,
                scope: 0,
                fingerprint: [0; 12],
            }],
            postings: BTreeMap::from([("a".to_owned(), vec![(0, 1)])]),
        };
        let b = encode(&s).unwrap();
        let ps = offset(&b, 48);
        let ds = offset(&b, 56);
        let mut bad = b.clone();
        bad[ps] |= 128;
        reseal(&mut bad);
        assert!(decode(&bad).is_err());
        let mut bad = b.clone();
        bad[ds + 6] = 255;
        reseal(&mut bad);
        assert!(decode(&bad).is_err());
        // Force a 64-bit ID delta to overflow its nonzero base.
        let b = encode(&sample(2)).unwrap();
        let mut bad = b.clone();
        bad[HEADER + 24..HEADER + 40].fill(255);
        reseal(&mut bad);
        assert!(decode(&bad).is_err());
    }
    #[test]
    fn rejects_invalid_inputs_and_expansion() {
        let mut s = sample(2);
        s.rows[1].id = s.rows[0].id;
        assert!(encode(&s).is_err());
        let mut s = sample(2);
        s.rows[0].id = 0;
        assert!(encode(&s).is_err());
        let mut s = sample(2);
        s.postings.values_mut().next().unwrap()[0].1 = 0;
        assert!(encode(&s).is_err());
        let mut s = sample(2);
        s.postings.values_mut().next().unwrap().push((0, 1));
        assert!(encode(&s).is_err());
        let mut s = sample(2);
        s.postings.insert("out".into(), vec![(2, 1)]);
        assert!(encode(&s).is_err());
        let mut s = sample(1);
        s.start = u32::MAX;
        assert!(encode(&s).is_err());
        let mut s = sample(1);
        s.rows.resize(MAX_ROWS + 1, s.rows[0].clone());
        assert!(encode(&s).is_err());
        let mut s = sample(1);
        s.postings.clear();
        s.postings.insert("x".repeat(MAX_BYTES), vec![(0, 1)]);
        assert!(encode(&s).is_err());
        assert!(decode(&vec![0; MAX_BYTES + 1]).is_err());
    }
}
