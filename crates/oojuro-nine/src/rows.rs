//! Packed row metadata of version 3 index segments.
//!
//! Version 1 and 2 segments keep 40 bytes per row. Version 3 groups rows in
//! blocks of 128 and stores every field of a block as a frame of reference:
//! the block's minimum (the base) and each row's difference from it in the
//! fewest bits that hold the largest difference of the block. Reading one row
//! is a few shifts and masks; no other row is decoded.
//!
//! Section layout (little-endian): the blocks back to back, then a table of
//! `blocks + 1` u64 byte offsets from the section start (the last is the end
//! of the blocks). A block of `k` rows (128, fewer in the last):
//!
//! ```text
//! flags u8                      bit 0: recorded_at equals valid_at in every row
//! length     width u8, base u32
//! scope      width u8, base u32
//! valid_at   width u8, base i64
//! recorded_at  width u8, base i64          (absent when flags bit 0)
//! text_len   width u8, base u32
//! flags      width u8, base u32            (row flags)
//! id_prefix  width u8, shift u8, base u64  (shift: trailing zero bits of every
//!                                           difference, not stored)
//! values     per field in the same order, k * width bits, LSB first, padded to a byte
//! ```
use crate::segment::RowMeta;
use oojuro_core::Result;

pub(crate) const BLOCK: usize = 128;

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// Bits needed for `v`.
fn bits_of(v: u64) -> u32 {
    64 - v.leading_zeros()
}

/// Append `vals`, `w` bits each, LSB first, padded to a byte.
fn pack(vals: impl Iterator<Item = u64>, w: u32, out: &mut Vec<u8>) {
    if w == 0 {
        return;
    }
    let mut acc = 0u128;
    let mut n = 0u32;
    for v in vals {
        acc |= u128::from(v) << n;
        n += w;
        while n >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            n -= 8;
        }
    }
    if n > 0 {
        out.push(acc as u8);
    }
}

/// `w` bits starting at bit `bit` of `buf` (bits past the end read as zero).
#[inline]
fn get_bits(buf: &[u8], bit: usize, w: u32) -> u64 {
    if w == 0 {
        return 0;
    }
    let at = bit >> 3;
    let sh = (bit & 7) as u32;
    let load = |at: usize| -> u64 {
        match buf.get(at..at + 8) {
            Some(b) => u64::from_le_bytes(b.try_into().expect("8 bytes")),
            None => {
                let mut b = [0u8; 8];
                let tail = buf.get(at..).unwrap_or(&[]);
                b[..tail.len().min(8)].copy_from_slice(&tail[..tail.len().min(8)]);
                u64::from_le_bytes(b)
            }
        }
    };
    let lo = load(at) >> sh;
    let v = if w + sh <= 64 { lo } else { lo | (load(at + 8) << (64 - sh)) };
    if w == 64 { v } else { v & ((1u64 << w) - 1) }
}

/// Encode up to 128 rows as one block.
pub(crate) fn encode_block(rows: &[RowMeta], out: &mut Vec<u8>) {
    debug_assert!(!rows.is_empty() && rows.len() <= BLOCK);
    let same = rows.iter().all(|m| m.recorded_at == m.valid_at);
    out.push(same as u8);
    let u32_field = |out: &mut Vec<u8>, get: &dyn Fn(&RowMeta) -> u32| {
        let min = rows.iter().map(get).min().unwrap_or(0);
        let max = rows.iter().map(get).max().unwrap_or(0);
        let w = bits_of(u64::from(max - min));
        out.push(w as u8);
        out.extend_from_slice(&min.to_le_bytes());
        (min, w)
    };
    // headers
    let head = out.len();
    let (l_min, l_w) = u32_field(out, &|m| m.length);
    let (s_min, s_w) = u32_field(out, &|m| m.scope);
    let v_min = rows.iter().map(|m| m.valid_at).min().unwrap_or(0);
    let v_w = bits_of(rows.iter().map(|m| m.valid_at.wrapping_sub(v_min) as u64).max().unwrap_or(0));
    out.push(v_w as u8);
    out.extend_from_slice(&v_min.to_le_bytes());
    let r_min = rows.iter().map(|m| m.recorded_at).min().unwrap_or(0);
    let r_w = bits_of(rows.iter().map(|m| m.recorded_at.wrapping_sub(r_min) as u64).max().unwrap_or(0));
    if !same {
        out.push(r_w as u8);
        out.extend_from_slice(&r_min.to_le_bytes());
    }
    let (t_min, t_w) = u32_field(out, &|m| m.text_len);
    let (f_min, f_w) = u32_field(out, &|m| m.flags);
    let i_min = rows.iter().map(|m| m.id_prefix).min().unwrap_or(0);
    let i_or = rows.iter().fold(0u64, |a, m| a | (m.id_prefix - i_min));
    let i_sh = if i_or == 0 { 0 } else { i_or.trailing_zeros() };
    let i_w = bits_of(i_or >> i_sh);
    out.push(i_w as u8);
    out.push(i_sh as u8);
    out.extend_from_slice(&i_min.to_le_bytes());
    debug_assert_eq!(out.len() - head, header_len(same) - 1);
    // values
    pack(rows.iter().map(|m| u64::from(m.length - l_min)), l_w, out);
    pack(rows.iter().map(|m| u64::from(m.scope - s_min)), s_w, out);
    pack(rows.iter().map(|m| m.valid_at.wrapping_sub(v_min) as u64), v_w, out);
    if !same {
        pack(rows.iter().map(|m| m.recorded_at.wrapping_sub(r_min) as u64), r_w, out);
    }
    pack(rows.iter().map(|m| u64::from(m.text_len - t_min)), t_w, out);
    pack(rows.iter().map(|m| u64::from(m.flags - f_min)), f_w, out);
    pack(rows.iter().map(|m| (m.id_prefix - i_min) >> i_sh), i_w, out);
}

/// Bytes of a block header, flags byte included.
const fn header_len(same: bool) -> usize {
    1 + 5 + 5 + 9 + if same { 0 } else { 9 } + 5 + 5 + 10
}

fn bytes_of(k: usize, w: u32) -> usize {
    (k * w as usize).div_ceil(8)
}

/// Where the packed rows of a segment are (offsets into the segment file).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rows {
    /// Section start.
    start: usize,
    /// Offset table position (end of the blocks).
    table: usize,
    pub n: usize,
}

/// Field positions inside one block.
struct Block {
    /// Block start (absolute).
    at: usize,
    k: usize,
    same: bool,
}

impl Rows {
    /// Check the offset table of the section `[start, end)` holding `n` rows.
    pub fn new(buf: &[u8], start: usize, end: usize, n: usize) -> Result<Self> {
        let blocks = n.div_ceil(BLOCK);
        let tab = 8 * (blocks + 1);
        if end < start || end > buf.len() || end - start < tab {
            return Err("packed rows section is too short".into());
        }
        let table = end - tab;
        let r = Rows { start, table, n };
        let mut prev = 0u64;
        for b in 0..=blocks {
            let o = u64_at(buf, table + 8 * b);
            if o < prev || (b == 0 && o != 0) || start as u64 + o > table as u64 {
                return Err("packed rows offsets are inconsistent".into());
            }
            prev = o;
        }
        if start as u64 + prev != table as u64 {
            return Err("packed rows do not fill their section".into());
        }
        // every block holds at least its header
        for b in 0..blocks {
            let (a, z) = (u64_at(buf, table + 8 * b), u64_at(buf, table + 8 * (b + 1)));
            if (z - a) < header_len(true) as u64 {
                return Err("packed rows block is too short".into());
            }
        }
        Ok(r)
    }

    #[inline]
    fn block(&self, buf: &[u8], i: usize) -> (Block, usize) {
        let b = i / BLOCK;
        let off = u64_at(buf, self.table + 8 * b) as usize;
        let at = self.start + off;
        let k = (self.n - b * BLOCK).min(BLOCK);
        (Block { at, k, same: buf[at] & 1 == 1 }, i % BLOCK)
    }

    /// (scope, length) of row `i`.
    #[inline]
    pub fn scope_length(&self, buf: &[u8], i: usize) -> (u32, u32) {
        let (b, j) = self.block(buf, i);
        let h = b.at;
        let (lw, lmin) = (u32::from(buf[h + 1]), u32_at(buf, h + 2));
        let (sw, smin) = (u32::from(buf[h + 6]), u32_at(buf, h + 7));
        let data = h + header_len(b.same);
        let length = lmin + get_bits(buf, data * 8 + j * lw as usize, lw) as u32;
        let scope = smin + get_bits(buf, (data + bytes_of(b.k, lw)) * 8 + j * sw as usize, sw) as u32;
        (scope, length)
    }

    /// valid_at of row `i`.
    #[inline]
    pub fn valid_at(&self, buf: &[u8], i: usize) -> i64 {
        let (b, j) = self.block(buf, i);
        let h = b.at;
        let (lw, sw) = (u32::from(buf[h + 1]), u32::from(buf[h + 6]));
        let (vw, vmin) = (u32::from(buf[h + 11]), u64_at(buf, h + 12) as i64);
        let data = h + header_len(b.same) + bytes_of(b.k, lw) + bytes_of(b.k, sw);
        vmin.wrapping_add(get_bits(buf, data * 8 + j * vw as usize, vw) as i64)
    }

    /// All fields of row `i`.
    pub fn meta(&self, buf: &[u8], i: usize) -> RowMeta {
        let (b, j) = self.block(buf, i);
        let mut h = b.at + 1;
        let mut data = b.at + header_len(b.same);
        let mut field = |wide: bool| -> (u32, u64, usize) {
            // (width, base, bit position of row j)
            let w = u32::from(buf[h]);
            let base = if wide { u64_at(buf, h + 1) } else { u64::from(u32_at(buf, h + 1)) };
            h += if wide { 9 } else { 5 };
            let pos = data * 8 + j * w as usize;
            data += bytes_of(b.k, w);
            (w, base, pos)
        };
        let (lw, lmin, lp) = field(false);
        let (sw, smin, sp) = field(false);
        let (vw, vmin, vp) = field(true);
        let valid_at = (vmin as i64).wrapping_add(get_bits(buf, vp, vw) as i64);
        let recorded_at = if b.same {
            valid_at
        } else {
            let (rw, rmin, rp) = field(true);
            (rmin as i64).wrapping_add(get_bits(buf, rp, rw) as i64)
        };
        let (tw, tmin, tp) = field(false);
        let (fw, fmin, fp) = field(false);
        let iw = u32::from(buf[h]);
        let ish = u32::from(buf[h + 1]);
        let imin = u64_at(buf, h + 2);
        let ip = data * 8 + j * iw as usize;
        RowMeta {
            valid_at,
            recorded_at,
            length: (lmin + get_bits(buf, lp, lw)) as u32,
            scope: (smin + get_bits(buf, sp, sw)) as u32,
            text_len: (tmin + get_bits(buf, tp, tw)) as u32,
            flags: (fmin + get_bits(buf, fp, fw)) as u32,
            id_prefix: imin + (get_bits(buf, ip, iw) << ish),
        }
    }
}

/// Reads (scope, length) of rows visited in ascending order: a block's
/// header is parsed once however many of its rows are asked for.
pub(crate) struct MassCursor<'a> {
    rows: &'a Rows,
    buf: &'a [u8],
    block: usize,
    lw: u32,
    lmin: u32,
    sw: u32,
    smin: u32,
    lbit: usize,
    sbit: usize,
}

impl Rows {
    pub fn mass_cursor<'a>(&'a self, buf: &'a [u8]) -> MassCursor<'a> {
        MassCursor { rows: self, buf, block: usize::MAX, lw: 0, lmin: 0, sw: 0, smin: 0, lbit: 0, sbit: 0 }
    }
}

impl MassCursor<'_> {
    /// (scope, length) of row `i`.
    #[inline]
    pub fn get(&mut self, i: usize) -> (u32, u32) {
        let b = i / BLOCK;
        if b != self.block {
            let (blk, _) = self.rows.block(self.buf, i);
            let h = blk.at;
            (self.lw, self.lmin) = (u32::from(self.buf[h + 1]), u32_at(self.buf, h + 2));
            (self.sw, self.smin) = (u32::from(self.buf[h + 6]), u32_at(self.buf, h + 7));
            let data = h + header_len(blk.same);
            self.lbit = data * 8;
            self.sbit = (data + bytes_of(blk.k, self.lw)) * 8;
            self.block = b;
        }
        let j = i % BLOCK;
        (
            self.smin + get_bits(self.buf, self.sbit + j * self.sw as usize, self.sw) as u32,
            self.lmin + get_bits(self.buf, self.lbit + j * self.lw as usize, self.lw) as u32,
        )
    }
}

/// Encodes a stream of rows into blocks; `finish` returns the offset table.
#[derive(Default)]
pub(crate) struct Encoder {
    pending: Vec<RowMeta>,
    offsets: Vec<u64>,
    at: u64,
    block: Vec<u8>,
}

impl Encoder {
    /// Add a row; returns the bytes of a finished block, if this row completed one.
    pub fn push(&mut self, m: RowMeta) -> Option<&[u8]> {
        self.pending.push(m);
        (self.pending.len() == BLOCK).then(|| self.flush())
    }

    fn flush(&mut self) -> &[u8] {
        self.block.clear();
        encode_block(&self.pending, &mut self.block);
        self.pending.clear();
        self.offsets.push(self.at);
        self.at += self.block.len() as u64;
        &self.block
    }

    /// The last (short) block, if any.
    pub fn last(&mut self) -> Option<&[u8]> {
        (!self.pending.is_empty()).then(|| self.flush())
    }

    /// The offset table: every block start and the end of the blocks.
    pub fn table(mut self) -> Vec<u8> {
        self.offsets.push(self.at);
        self.offsets.iter().flat_map(|o| o.to_le_bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(s: &mut u64) -> u64 {
        *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn section(rows: &[RowMeta]) -> Vec<u8> {
        let mut e = Encoder::default();
        let mut out = Vec::new();
        for m in rows {
            if let Some(b) = e.push(*m) {
                out.extend_from_slice(b);
            }
        }
        if let Some(b) = e.last() {
            out.extend_from_slice(b);
        }
        out.extend_from_slice(&e.table());
        out
    }

    fn check(rows: &[RowMeta]) {
        let sec = section(rows);
        // the file around the section: bytes before and after must not matter
        let mut buf = vec![0xAB; 37];
        let start = buf.len();
        buf.extend_from_slice(&sec);
        let end = buf.len();
        buf.extend_from_slice(&[0xCD; 21]);
        let r = Rows::new(&buf, start, end, rows.len()).unwrap();
        let mut cur = r.mass_cursor(&buf);
        for (i, m) in rows.iter().enumerate() {
            assert_eq!(cur.get(i), (m.scope, m.length), "cursor, row {i}");
            assert_eq!(r.meta(&buf, i), *m, "row {i}");
            assert_eq!(r.scope_length(&buf, i), (m.scope, m.length), "row {i}");
            assert_eq!(r.valid_at(&buf, i), m.valid_at, "row {i}");
        }
    }

    #[test]
    fn the_mass_cursor_follows_any_order_of_rows() {
        let mut s = 9u64;
        let rows = random_rows(&mut s, 1000, 0);
        let sec = section(&rows);
        let r = Rows::new(&sec, 0, sec.len(), rows.len()).unwrap();
        let mut cur = r.mass_cursor(&sec);
        for _ in 0..5000 {
            let i = (rng(&mut s) % 1000) as usize;
            assert_eq!(cur.get(i), (rows[i].scope, rows[i].length));
        }
    }

    fn random_rows(s: &mut u64, n: usize, kind: u64) -> Vec<RowMeta> {
        (0..n)
            .map(|i| {
                let a = rng(s);
                let valid = match kind % 5 {
                    0 => 1_790_000_000_000_000 + (i as i64) * 1000 + (a % 997) as i64,
                    1 => i64::MIN + (a >> 3) as i64,
                    2 => (a as i64) >> (a % 60),
                    3 => 42,
                    _ => -((a >> 20) as i64),
                };
                RowMeta {
                    valid_at: valid,
                    recorded_at: if kind.is_multiple_of(3) { valid } else { valid.wrapping_add((rng(s) >> 30) as i64) },
                    length: if kind.is_multiple_of(7) { 0 } else { (rng(s) as u32) >> (rng(s) % 32) },
                    scope: (rng(s) as u32) >> (rng(s) % 32),
                    text_len: if kind.is_multiple_of(4) { u32::MAX } else { rng(s) as u32 >> (rng(s) % 33).min(31) },
                    flags: (rng(s) % 2) as u32,
                    id_prefix: match kind % 4 {
                        0 => u64::from_be_bytes(*b"1234567\0") + (i as u64) * 256,
                        1 => rng(s),
                        2 => 0,
                        _ => u64::MAX - (rng(s) % 5),
                    },
                }
            })
            .collect()
    }

    #[test]
    fn rows_roundtrip_across_shapes_and_sizes() {
        let mut s = 1u64;
        for kind in 0..35 {
            for n in [0usize, 1, 2, 127, 128, 129, 255, 256, 257, 700] {
                let rows = random_rows(&mut s, n, kind);
                check(&rows);
            }
        }
        // extremes in one block
        let mut rows = random_rows(&mut s, 128, 1);
        rows[3] = RowMeta { valid_at: i64::MAX, recorded_at: i64::MIN, length: u32::MAX, scope: 0, text_len: u32::MAX, flags: u32::MAX, id_prefix: u64::MAX };
        rows[90] = RowMeta { valid_at: i64::MIN, recorded_at: i64::MAX, length: 0, scope: u32::MAX, text_len: 0, flags: 0, id_prefix: 0 };
        check(&rows);
    }

    #[test]
    fn ascending_decimal_ids_and_equal_times_pack_small() {
        let rows: Vec<RowMeta> = (0..128 * 40)
            .map(|i| {
                let id = (100_000 + i).to_string();
                RowMeta {
                    valid_at: 1_790_000_000_000_000 + i as i64 * 1_000,
                    recorded_at: 1_790_000_000_000_000 + i as i64 * 1_000,
                    length: 400 + (i % 50) as u32,
                    scope: 120 + (i % 20) as u32,
                    text_len: 2_000 + (i % 700) as u32,
                    flags: 0,
                    id_prefix: crate::segment::id_prefix(&id),
                }
            })
            .collect();
        check(&rows);
        let per_row = section(&rows).len() as f64 / rows.len() as f64;
        assert!(per_row < 9.0, "{per_row} bytes per row");
    }

    #[test]
    fn a_damaged_table_is_refused() {
        let mut s = 5u64;
        let rows = random_rows(&mut s, 300, 0);
        let sec = section(&rows);
        let n = rows.len();
        assert!(Rows::new(&sec, 0, sec.len(), n).is_ok());
        // wrong row count, truncated section, shifted offset
        assert!(Rows::new(&sec, 0, sec.len(), n + 200).is_err());
        assert!(Rows::new(&sec, 0, sec.len() - 8, n).is_err());
        let mut bad = sec.clone();
        let t = bad.len() - 8 * (n.div_ceil(BLOCK) + 1);
        bad[t + 15] = 0xFF;
        assert!(Rows::new(&bad, 0, bad.len(), n).is_err());
        assert!(Rows::new(&sec, 0, 4, n).is_err());
    }
}
