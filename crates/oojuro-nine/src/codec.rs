//! Binary encodings shared by the v4 journal and archive: LEB128 varints,
//! zigzag integers, 128-bit identity fingerprints and the record frame.
//!
//! Record frame (all integers LEB128):
//!   flags u8   bit0 verified, bit1 key equals id, bit2 has metadata,
//!              bit3 id derived from (row, valid_at, text) by `derived_id`,
//!              bit4 stored only (exact and readable, never indexed)
//!   id         len + bytes, absent when bit3 is set
//!   key        len + bytes, absent when bit1 is set
//!   valid_at   zigzag
//!   recorded_at - valid_at   zigzag, wrapping
//!   metadata   count, then (len + key bytes, len + value bytes) pairs
//!   text       remaining bytes (UTF-8, validated on decode)
use oojuro_core::{Record, Result};
use std::collections::BTreeMap;

const VERIFIED: u8 = 1;
const KEY_IS_ID: u8 = 2;
const METADATA: u8 = 4;
const DERIVED_ID: u8 = 8;
const UNINDEXED: u8 = 16;

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn get_varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or("truncated varint")?;
        *pos += 1;
        if shift == 63 && b > 1 {
            return Err("varint overflows u64".into());
        }
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err("varint overflows u64".into())
}

pub(crate) fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub(crate) fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn get_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    let n = usize::try_from(get_varint(buf, pos)?)?;
    let end = pos.checked_add(n).ok_or("length overflow")?;
    let b = buf.get(*pos..end).ok_or("truncated field")?;
    *pos = end;
    Ok(b)
}

fn get_str(buf: &[u8], pos: &mut usize) -> Result<String> {
    Ok(std::str::from_utf8(get_bytes(buf, pos)?)?.to_owned())
}

/// The identifier `put` assigns: BLAKE3 of row, event time and text (as v3).
pub(crate) fn derived_id(row: u64, at: i64, text: &str) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(&row.to_le_bytes());
    hash.update(&at.to_le_bytes());
    hash.update(text.as_bytes());
    hash.finalize().to_hex().to_string()
}

/// 128-bit identity of an id or key. Equal fingerprints are treated as equal
/// keys (collision bound ~n²/2¹²⁹); id lookups also compare the decoded id.
pub(crate) fn fingerprint(s: &str) -> u128 {
    let h = blake3::Hasher::new_derive_key("oojuro nine v4 identity")
        .update(s.as_bytes())
        .finalize();
    u128::from_le_bytes(h.as_bytes()[..16].try_into().expect("16 bytes"))
}

/// Encode one indexed record. `row` is its global ordinal; it lets an id
/// produced by `put` be recomputed instead of stored.
#[cfg(test)]
pub(crate) fn encode_record(r: &Record, row: u64, out: &mut Vec<u8>) {
    encode_record_with(r, row, true, out)
}

/// Encode one record; `indexed` false marks it as stored only.
pub(crate) fn encode_record_with(r: &Record, row: u64, indexed: bool, out: &mut Vec<u8>) {
    let derived = r.id.len() == 64 && r.id == derived_id(row, r.valid_at, &r.text);
    let mut flags = 0;
    if r.verified {
        flags |= VERIFIED;
    }
    if r.key == r.id {
        flags |= KEY_IS_ID;
    }
    if !r.metadata.is_empty() {
        flags |= METADATA;
    }
    if derived {
        flags |= DERIVED_ID;
    }
    if !indexed {
        flags |= UNINDEXED;
    }
    out.push(flags);
    if !derived {
        put_bytes(out, r.id.as_bytes());
    }
    if r.key != r.id {
        put_bytes(out, r.key.as_bytes());
    }
    put_varint(out, zigzag(r.valid_at));
    put_varint(out, zigzag(r.recorded_at.wrapping_sub(r.valid_at)));
    if !r.metadata.is_empty() {
        put_varint(out, r.metadata.len() as u64);
        for (k, v) in &r.metadata {
            put_bytes(out, k.as_bytes());
            put_bytes(out, v.as_bytes());
        }
    }
    out.extend_from_slice(r.text.as_bytes());
}

pub(crate) fn decode_record(buf: &[u8], row: u64) -> Result<Record> {
    Ok(decode_record_with(buf, row)?.0)
}

/// Decode one record and whether it is indexed.
pub(crate) fn decode_record_with(buf: &[u8], row: u64) -> Result<(Record, bool)> {
    let flags = *buf.first().ok_or("empty record frame")?;
    if flags & !(VERIFIED | KEY_IS_ID | METADATA | DERIVED_ID | UNINDEXED) != 0 {
        return Err("unknown record flags".into());
    }
    let mut pos = 1;
    let id = if flags & DERIVED_ID == 0 {
        Some(get_str(buf, &mut pos)?)
    } else {
        None
    };
    let key = if flags & KEY_IS_ID == 0 {
        Some(get_str(buf, &mut pos)?)
    } else {
        None
    };
    let valid_at = unzigzag(get_varint(buf, &mut pos)?);
    let recorded_at = valid_at.wrapping_add(unzigzag(get_varint(buf, &mut pos)?));
    let mut metadata = BTreeMap::new();
    if flags & METADATA != 0 {
        let n = get_varint(buf, &mut pos)?;
        for _ in 0..n {
            let k = get_str(buf, &mut pos)?;
            let v = get_str(buf, &mut pos)?;
            if metadata.insert(k, v).is_some() {
                return Err("duplicate metadata key".into());
            }
        }
        if metadata.is_empty() {
            return Err("empty metadata with metadata flag".into());
        }
    }
    let text = std::str::from_utf8(&buf[pos..])?.to_owned();
    let id = id.unwrap_or_else(|| derived_id(row, valid_at, &text));
    let key = key.unwrap_or_else(|| id.clone());
    if flags & KEY_IS_ID == 0 && key == id {
        return Err("non-canonical record frame".into());
    }
    let record = Record {
        id,
        key,
        text,
        valid_at,
        recorded_at,
        verified: flags & VERIFIED != 0,
        embedding: None,
        metadata,
    };
    Ok((record, flags & UNINDEXED == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_zigzag_roundtrip_extremes() {
        for v in [0u64, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut b = Vec::new();
            put_varint(&mut b, v);
            let mut pos = 0;
            assert_eq!(get_varint(&b, &mut pos).unwrap(), v);
            assert_eq!(pos, b.len());
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -64, 64] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
        let mut pos = 0;
        assert!(get_varint(&[0xff; 11], &mut pos).is_err());
        let mut pos = 0;
        assert!(get_varint(&[0x80], &mut pos).is_err());
    }

    #[test]
    fn record_frames_roundtrip_exact_fields_and_derive_put_ids() {
        let mut meta = Record::new("custom-id", "a key", "東京🦀 text\r\n\0end", -5);
        meta.recorded_at = i64::MAX;
        meta.verified = true;
        meta.metadata.insert("k".into(), "v".repeat(300));
        meta.metadata.insert("".into(), "".into());
        let id = derived_id(7, i64::MIN, "put text");
        let mut put = Record::new(&id, &id, "put text", i64::MIN);
        put.verified = false;
        let empty = Record::new("e", "e", "", 0);
        for (r, row) in [(meta, 3u64), (put, 7), (empty, 0)] {
            let mut b = Vec::new();
            encode_record(&r, row, &mut b);
            let back = decode_record(&b, row).unwrap();
            assert_eq!(serde_json::to_vec(&back).unwrap(), serde_json::to_vec(&r).unwrap());
        }
        // A derived id costs no bytes but only reproduces at its own row.
        let mut b = Vec::new();
        let r = Record::new(&id, &id, "put text", i64::MIN);
        encode_record(&r, 7, &mut b);
        // flags + 10-byte valid_at + 1-byte recorded delta + text, no id or key.
        assert_eq!(b.len(), 1 + 10 + 1 + "put text".len());
        assert_ne!(decode_record(&b, 8).unwrap().id, id);
        assert!(decode_record(&[0x80], 0).is_err());
        assert!(decode_record(&[0, 5, b'a'], 0).is_err());
    }
}
