//! Immutable encrypted DATA packs staged below the managed authority.
//!
//! These references are candidates until the independently retained managed
//! control commits their exact inventory. This module never returns a commit ACK.
mod codec;
mod reader;
use crate::{Error, Result};
pub use codec::{Options, Stage, stage_dictionary};
pub use reader::{CacheUsage, Catalog, Reader};

pub const FORMAT: u32 = 1;
pub const MAX_PACK_BYTES: u64 = 64 << 20;
pub const MAX_FRAME_ROWS: usize = 1024;
pub const MAX_FRAME_BYTES: usize = 16 << 20;

/// Pre-commit identity; final control/root hashes are intentionally absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Birth {
    pub format: u32,
    pub stable_origin: [u8; 32],
    pub identity: [u8; 32],
    pub epoch: [u8; 32],
    pub generation: u64,
    pub predecessor_logical: [u8; 32],
    pub parent_checkpoint: [u8; 32],
    pub key_version: [u8; 32],
}
impl Birth {
    pub const ENCODED_BYTES: usize = 204;
    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT
            || [
                self.stable_origin,
                self.identity,
                self.epoch,
                self.key_version,
            ]
            .iter()
            .any(|v| *v == [0; 32])
        {
            return Err(Error::Invalid("invalid managed pack birth"));
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::with_capacity(Self::ENCODED_BYTES);
        out.extend(self.format.to_le_bytes());
        out.extend(self.stable_origin);
        out.extend(self.identity);
        out.extend(self.epoch);
        out.extend(self.generation.to_le_bytes());
        out.extend(self.predecessor_logical);
        out.extend(self.parent_checkpoint);
        out.extend(self.key_version);
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::ENCODED_BYTES {
            return Err(Error::Corrupt("invalid managed birth length"));
        }
        let mut at = 0;
        let v = Self {
            format: u32::from_le_bytes(take(bytes, &mut at)?),
            stable_origin: take(bytes, &mut at)?,
            identity: take(bytes, &mut at)?,
            epoch: take(bytes, &mut at)?,
            generation: u64::from_le_bytes(take(bytes, &mut at)?),
            predecessor_logical: take(bytes, &mut at)?,
            parent_checkpoint: take(bytes, &mut at)?,
            key_version: take(bytes, &mut at)?,
        };
        v.validate()
            .map_err(|_| Error::Corrupt("invalid managed pack birth"))?;
        Ok(v)
    }
}
/// Reserved physical numbering is independent of original logical record IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub file_id: u64,
    pub first_physical_seq: u64,
}
/// One actual frame emitted by staging; offset starts at its CRC/length carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Locator {
    pub file_id: u64,
    pub frame_ordinal: u64,
    pub first_physical_seq: u64,
    pub count: u32,
    pub offset: u64,
    pub payload_len: u32,
    pub raw_len: u32,
    pub dictionary_id: u32,
    pub valid_lo: i64,
    pub valid_hi: i64,
}
impl Locator {
    pub const ENCODED_BYTES: usize = 64;
    pub fn validate(&self) -> Result<()> {
        if self.file_id == 0
            || self.count == 0
            || self.count as usize > MAX_FRAME_ROWS
            || self.payload_len == 0
            || self.payload_len as u64 > MAX_PACK_BYTES
            || self.raw_len as usize > MAX_FRAME_BYTES
            || self.valid_lo > self.valid_hi
            || self
                .offset
                .checked_add(8 + u64::from(self.payload_len))
                .is_none_or(|n| n > MAX_PACK_BYTES)
            || self
                .first_physical_seq
                .checked_add(u64::from(self.count))
                .is_none()
        {
            return Err(Error::Corrupt("invalid managed frame locator"));
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut b = Vec::with_capacity(Self::ENCODED_BYTES);
        for n in [self.file_id, self.frame_ordinal, self.first_physical_seq] {
            b.extend(n.to_le_bytes());
        }
        b.extend(self.count.to_le_bytes());
        b.extend(self.offset.to_le_bytes());
        for n in [self.payload_len, self.raw_len, self.dictionary_id] {
            b.extend(n.to_le_bytes());
        }
        b.extend(self.valid_lo.to_le_bytes());
        b.extend(self.valid_hi.to_le_bytes());
        Ok(b)
    }
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != Self::ENCODED_BYTES {
            return Err(Error::Corrupt("invalid managed locator length"));
        }
        let mut at = 0;
        let v = Self {
            file_id: u64::from_le_bytes(take(b, &mut at)?),
            frame_ordinal: u64::from_le_bytes(take(b, &mut at)?),
            first_physical_seq: u64::from_le_bytes(take(b, &mut at)?),
            count: u32::from_le_bytes(take(b, &mut at)?),
            offset: u64::from_le_bytes(take(b, &mut at)?),
            payload_len: u32::from_le_bytes(take(b, &mut at)?),
            raw_len: u32::from_le_bytes(take(b, &mut at)?),
            dictionary_id: u32::from_le_bytes(take(b, &mut at)?),
            valid_lo: i64::from_le_bytes(take(b, &mut at)?),
            valid_hi: i64::from_le_bytes(take(b, &mut at)?),
        };
        v.validate()?;
        Ok(v)
    }
}
/// An immutable sealed dictionary; only the trusted committed catalog may admit it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryRef {
    pub id: u32,
    pub token: [u8; 32],
    pub birth: Birth,
    pub byte_len: u64,
    pub ciphertext_digest: [u8; 32],
    pub raw_bytes: u32,
}
/// Exact immutable pack identity. No plaintext digest or raw DEK is persisted here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackRef {
    pub format: u32,
    pub token: [u8; 32],
    pub birth: Birth,
    pub file_id: u64,
    pub byte_len: u64,
    pub ciphertext_digest: [u8; 32],
    pub first_physical_seq: u64,
    pub record_count: u64,
    pub frame_count: u64,
    pub dictionary: Option<DictionaryRef>,
}
impl PackRef {
    pub fn validate(&self) -> Result<()> {
        self.birth.validate()?;
        if self.format != FORMAT
            || self.token == [0; 32]
            || self.file_id == 0
            || self.byte_len == 0
            || self.byte_len > MAX_PACK_BYTES
            || self.ciphertext_digest == [0; 32]
            || self.frame_count == 0
            || self.record_count < self.frame_count
            || self.record_count
                > self
                    .frame_count
                    .checked_mul(MAX_FRAME_ROWS as u64)
                    .ok_or(Error::Corrupt("pack count overflow"))?
            || self
                .first_physical_seq
                .checked_add(self.record_count)
                .is_none()
        {
            return Err(Error::Corrupt("invalid managed pack reference"));
        }
        if let Some(d) = &self.dictionary {
            d.validate()?;
            if d.birth.stable_origin != self.birth.stable_origin
                || d.birth.identity != self.birth.identity
                || d.birth.key_version != self.birth.key_version
            {
                return Err(Error::Corrupt("dictionary outside managed scope"));
            }
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = b"OJMPREF1".to_vec();
        out.extend(self.format.to_le_bytes());
        out.extend(self.token);
        out.extend(self.birth.encode()?);
        for n in [self.file_id, self.byte_len] {
            out.extend(n.to_le_bytes());
        }
        out.extend(self.ciphertext_digest);
        for n in [self.first_physical_seq, self.record_count, self.frame_count] {
            out.extend(n.to_le_bytes());
        }
        out.push(u8::from(self.dictionary.is_some()));
        if let Some(d) = &self.dictionary {
            out.extend(d.encode()?);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 1024 || bytes.get(..8) != Some(b"OJMPREF1") {
            return Err(Error::Corrupt("invalid managed pack reference encoding"));
        }
        let mut at = 8;
        let format = u32::from_le_bytes(take(bytes, &mut at)?);
        let token = take(bytes, &mut at)?;
        let birth = Birth::decode(slice(bytes, &mut at, Birth::ENCODED_BYTES)?)?;
        let file_id = u64::from_le_bytes(take(bytes, &mut at)?);
        let byte_len = u64::from_le_bytes(take(bytes, &mut at)?);
        let ciphertext_digest = take(bytes, &mut at)?;
        let first_physical_seq = u64::from_le_bytes(take(bytes, &mut at)?);
        let record_count = u64::from_le_bytes(take(bytes, &mut at)?);
        let frame_count = u64::from_le_bytes(take(bytes, &mut at)?);
        let dictionary = match take::<1>(bytes, &mut at)?[0] {
            0 => None,
            1 => Some(DictionaryRef::decode(slice(
                bytes,
                &mut at,
                DictionaryRef::ENCODED_BYTES,
            )?)?),
            _ => return Err(Error::Corrupt("invalid dictionary flag")),
        };
        if at != bytes.len() {
            return Err(Error::Corrupt("trailing managed reference bytes"));
        }
        let v = Self {
            format,
            token,
            birth,
            file_id,
            byte_len,
            ciphertext_digest,
            first_physical_seq,
            record_count,
            frame_count,
            dictionary,
        };
        v.validate()?;
        Ok(v)
    }
}
impl DictionaryRef {
    const ENCODED_BYTES: usize = 284;
    pub fn validate(&self) -> Result<()> {
        self.birth.validate()?;
        if self.id == 0
            || self.token == [0; 32]
            || self.ciphertext_digest == [0; 32]
            || self.raw_bytes == 0
            || self.raw_bytes > 16 << 20
            || self.byte_len == 0
            || self.byte_len > (16 << 20) + 4096
        {
            return Err(Error::Corrupt("invalid managed dictionary reference"));
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut v = Vec::with_capacity(Self::ENCODED_BYTES);
        v.extend(self.id.to_le_bytes());
        v.extend(self.token);
        v.extend(self.birth.encode()?);
        v.extend(self.byte_len.to_le_bytes());
        v.extend(self.ciphertext_digest);
        v.extend(self.raw_bytes.to_le_bytes());
        Ok(v)
    }
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != Self::ENCODED_BYTES {
            return Err(Error::Corrupt("invalid managed dictionary length"));
        }
        let mut at = 0;
        let v = Self {
            id: u32::from_le_bytes(take(b, &mut at)?),
            token: take(b, &mut at)?,
            birth: Birth::decode(slice(b, &mut at, Birth::ENCODED_BYTES)?)?,
            byte_len: u64::from_le_bytes(take(b, &mut at)?),
            ciphertext_digest: take(b, &mut at)?,
            raw_bytes: u32::from_le_bytes(take(b, &mut at)?),
        };
        v.validate()?;
        Ok(v)
    }
}
fn slice<'a>(b: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = at
        .checked_add(n)
        .ok_or(Error::Corrupt("managed reference length overflow"))?;
    let v = b
        .get(*at..end)
        .ok_or(Error::Corrupt("truncated managed reference"))?;
    *at = end;
    Ok(v)
}
fn take<const N: usize>(b: &[u8], at: &mut usize) -> Result<[u8; N]> {
    Ok(slice(b, at, N)?.try_into().unwrap())
}

#[cfg(test)]
mod types_tests {
    use super::*;
    #[test]
    fn canonical_pack_types_bind_parent_key_and_physical_inventory() {
        let birth = Birth {
            format: FORMAT,
            stable_origin: [1; 32],
            identity: [2; 32],
            epoch: [3; 32],
            generation: 7,
            predecessor_logical: [4; 32],
            parent_checkpoint: [5; 32],
            key_version: [6; 32],
        };
        assert_eq!(Birth::decode(&birth.encode().unwrap()).unwrap(), birth);
        let dictionary = DictionaryRef {
            id: 9,
            token: [7; 32],
            birth: birth.clone(),
            byte_len: 400,
            ciphertext_digest: [8; 32],
            raw_bytes: 300,
        };
        assert_eq!(
            dictionary.encode().unwrap().len(),
            DictionaryRef::ENCODED_BYTES
        );
        let p = PackRef {
            format: FORMAT,
            token: [9; 32],
            birth,
            file_id: 1,
            byte_len: 1024,
            ciphertext_digest: [10; 32],
            first_physical_seq: 42,
            record_count: 4,
            frame_count: 2,
            dictionary: Some(dictionary),
        };
        let mut bytes = p.encode().unwrap();
        assert_eq!(PackRef::decode(&bytes).unwrap(), p);
        bytes.push(0);
        assert!(PackRef::decode(&bytes).is_err());
        let mut bad = p.clone();
        bad.dictionary.as_mut().unwrap().birth.key_version = [11; 32];
        assert!(bad.validate().is_err());
        let mut bad = p;
        bad.first_physical_seq = u64::MAX;
        assert!(bad.validate().is_err());
    }
}

#[cfg(test)]
mod tests;
