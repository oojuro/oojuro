//! Byte grammar for experimental wire 0.1. No application schema lives here.
use crate::{Error, Result};
use chacha20poly1305::{AeadInPlace, Tag, XChaCha20Poly1305, XNonce, aead::KeyInit};
use sha2::{Digest, Sha256};

pub const FILE: usize = 64;
pub const HEAD: usize = 96;
pub const FOOT: usize = 48;
pub const MAX_BODY: usize = 32 << 20;
pub const MAX_STORED: usize = 33 << 20;
pub const MAX_KEY: usize = 65_536;
pub const MAX_VALUE: usize = 16 << 20;
pub const MAX_OPS: usize = 1024;

pub fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut state = Sha256::new();
    for part in parts {
        state.update(part);
    }
    state.finalize().into()
}
pub fn crc(bytes: &[u8]) -> [u8; 4] {
    crc32fast::hash(bytes).to_le_bytes()
}
pub fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
pub fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| Error::Random)?;
    Ok(bytes)
}
fn key_id(key: &[u8; 32]) -> [u8; 8] {
    hash(&[b"ojo/key-id/0.1\0", key])[..8].try_into().unwrap()
}
pub fn new_header(key: Option<&[u8; 32]>) -> Result<[u8; FILE]> {
    let mut head = [0; FILE];
    head[..8].copy_from_slice(b"OJOFILE\0");
    head[10..12].copy_from_slice(&1u16.to_le_bytes());
    head[12..16].copy_from_slice(&u32::from(key.is_some()).to_le_bytes());
    head[16..32].copy_from_slice(&random::<16>()?);
    if let Some(key) = key {
        head[32..40].copy_from_slice(&key_id(key));
    }
    seal_crc(&mut head);
    Ok(head)
}
pub fn cipher(head: &[u8; FILE], key: Option<&[u8; 32]>) -> Result<Option<XChaCha20Poly1305>> {
    if &head[..8] != b"OJOFILE\0" || head[60..] != crc(&head[..60]) {
        return Err(Error::Corrupt("file header"));
    }
    if head[8..12] != [0, 0, 1, 0] || u32_at(head, 12) > 1 || head[40..60] != [0; 20] {
        return Err(Error::Unsupported);
    }
    match (u32_at(head, 12), key) {
        (0, _) if head[32..40] != [0; 8] => Err(Error::Corrupt("plaintext key id")),
        (0, None) => Ok(None),
        (0, Some(_)) => Err(Error::Downgrade),
        (1, None) => Err(Error::KeyRequired),
        (1, Some(key)) if head[32..40] == key_id(key) => {
            Ok(Some(XChaCha20Poly1305::new(key.into())))
        }
        _ => Err(Error::WrongKey),
    }
}
pub fn seal_crc(bytes: &mut [u8]) {
    let n = bytes.len() - 4;
    let checksum = crc(&bytes[..n]);
    bytes[n..].copy_from_slice(&checksum);
}

pub fn validate_head(head: &[u8; HEAD], sequence: u64, previous: &[u8; 32]) -> Result<()> {
    if &head[..8] != b"OJOTXN1\0" || head[92..] != crc(&head[..92]) {
        return Err(Error::Corrupt("transaction header"));
    }
    if head[36] > 1 || head[37..40] != [0; 3] || head[88..92] != [0; 4] {
        return Err(Error::Unsupported);
    }
    if u64_at(head, 8) != sequence || head[40..72] != previous[..] || head[72..88] == [0; 16] {
        return Err(Error::Corrupt("transaction order or request id"));
    }
    if u64_at(head, 16) > MAX_STORED as u64
        || u64_at(head, 24) > MAX_BODY as u64
        || !(1..=MAX_OPS as u32).contains(&u32_at(head, 32))
    {
        return Err(Error::Corrupt("transaction bounds"));
    }
    Ok(())
}

const DOMAIN: &[u8] = b"oojuro/ojo/0.1/transaction\0";
fn aad(file: &[u8; FILE], head: &[u8; HEAD]) -> [u8; DOMAIN.len() + FILE + HEAD] {
    let mut bytes = [0; DOMAIN.len() + FILE + HEAD];
    bytes[..DOMAIN.len()].copy_from_slice(DOMAIN);
    bytes[DOMAIN.len()..DOMAIN.len() + FILE].copy_from_slice(file);
    bytes[DOMAIN.len() + FILE..].copy_from_slice(head);
    bytes
}
/// Encrypt a preallocated nonce-prefix/body slice; the caller appends the returned tag.
pub fn encrypt(
    cipher: &XChaCha20Poly1305,
    file: &[u8; FILE],
    head: &[u8; HEAD],
    stored: &mut [u8],
) -> Result<[u8; 16]> {
    let nonce = random::<24>()?;
    let (prefix, body) = stored.split_at_mut(24);
    prefix.copy_from_slice(&nonce);
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), &aad(file, head), body)
        .map_err(|_| Error::Corrupt("encryption"))?;
    Ok(tag.into())
}

/// An owned decoded transaction with a borrowed view past its nonce prefix.
pub struct Body {
    bytes: Vec<u8>,
    start: usize,
}
impl std::ops::Deref for Body {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes[self.start..]
    }
}
pub fn decode(
    cipher: Option<&XChaCha20Poly1305>,
    file: &[u8; FILE],
    head: &[u8; HEAD],
    mut bytes: Vec<u8>,
) -> Result<Body> {
    let mut start = 0;
    if let Some(cipher) = cipher {
        if bytes.len() < 40 {
            return Err(Error::Corrupt("encrypted body"));
        }
        let nonce: [u8; 24] = bytes[..24].try_into().unwrap();
        let end = bytes.len() - 16;
        let tag: [u8; 16] = bytes[end..].try_into().unwrap();
        cipher
            .decrypt_in_place_detached(
                XNonce::from_slice(&nonce),
                &aad(file, head),
                &mut bytes[24..end],
                Tag::from_slice(&tag),
            )
            .map_err(|_| Error::Corrupt("authentication"))?;
        bytes.truncate(end);
        start = 24;
    }
    if head[36] == 1 {
        #[cfg(not(feature = "archive"))]
        return Err(Error::Unsupported);
        #[cfg(feature = "archive")]
        {
            let packed = &bytes[start..];
            if !packed.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
                || zstd::zstd_safe::find_frame_compressed_size(packed).ok() != Some(packed.len())
                || zstd::zstd_safe::get_dict_id_from_frame(packed).is_some()
            {
                return Err(Error::Corrupt("zstd frame"));
            }
            let mut decoder = zstd::bulk::Decompressor::new()?;
            decoder.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(25))?;
            bytes = decoder.decompress(packed, u64_at(head, 24) as usize)?;
            start = 0;
        }
    }
    if (bytes.len() - start) as u64 != u64_at(head, 24) {
        return Err(Error::Corrupt("decoded length"));
    }
    Ok(Body { bytes, start })
}

/// Visit borrowed values while validating the entire transaction, without copying it per operation.
pub fn walk<'a>(
    body: &'a [u8],
    count: u32,
    mut visit: impl FnMut(u32, &'a [u8], Option<&'a [u8]>),
) -> Result<()> {
    let mut at = 0usize;
    for op in 0..count {
        let h = body
            .get(at..at + 16)
            .ok_or(Error::Corrupt("operation header"))?;
        let key = u32_at(h, 4) as usize;
        let value = u64_at(h, 8);
        if h[0] > 1
            || h[1..4] != [0; 3]
            || key == 0
            || key > MAX_KEY
            || value > MAX_VALUE as u64
            || (h[0] == 1 && value != 0)
        {
            return Err(Error::Corrupt("operation fields"));
        }
        let start = at + 16;
        let end = start + key + value as usize; // bounded by the limits above, on 32-bit too
        let bytes = body
            .get(start..end)
            .ok_or(Error::Corrupt("operation bytes"))?;
        visit(op, &bytes[..key], (h[0] == 0).then_some(&bytes[key..]));
        at = end;
    }
    if at != body.len() {
        return Err(Error::Corrupt("trailing operation bytes"));
    }
    Ok(())
}
