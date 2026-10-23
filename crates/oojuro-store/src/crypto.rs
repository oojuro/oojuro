//! Frame encryption: XChaCha20-Poly1305 over the packed (already compressed) body of a frame, so the
//! compression ratio is kept and a frame is authenticated with its head.
//!
//! A sealed body is `nonce (24 B) | ciphertext | tag (16 B)`. The associated data is the frame head
//! (`format::put_head` output, which carries the sequence numbers and lengths) behind a domain
//! separator, so a frame cannot be moved to another place or have its head edited.
//! One key protects one store (the raw layer keeps one store per writer).
use crate::{Error, Result};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};

pub const KEY_BYTES: usize = 32;
pub const NONCE_BYTES: usize = 24;
pub const TAG_BYTES: usize = 16;
/// Bytes a sealed body adds to the packed body.
pub const SEAL_OVERHEAD: usize = NONCE_BYTES + TAG_BYTES;

const AAD_FRAME: &[u8] = b"oojuro/frame/v1\0";
const AAD_DICT: &[u8] = b"oojuro/dict/v1\0";

/// The key of a store plus its public key id (never the key itself).
#[derive(Clone)]
pub struct FrameCipher {
    aead: XChaCha20Poly1305,
    id: [u8; 8],
}

impl std::fmt::Debug for FrameCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FrameCipher(key id {})", hex8(&self.id))
    }
}

fn hex8(b: &[u8; 8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl FrameCipher {
    pub fn new(key: &[u8; KEY_BYTES]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"oojuro/key-id/v1\0");
        h.update(key);
        let id: [u8; 8] = h.finalize().as_bytes()[..8].try_into().unwrap();
        Self { aead: XChaCha20Poly1305::new(key.into()), id }
    }

    /// First 8 bytes of a hash of the key: names the key without revealing it.
    pub fn key_id(&self) -> [u8; 8] {
        self.id
    }

    fn seal_with(&self, domain: &[u8], aad: &[u8], plain: &[u8]) -> Vec<u8> {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut ad = Vec::with_capacity(domain.len() + aad.len());
        ad.extend_from_slice(domain);
        ad.extend_from_slice(aad);
        let ct = self.aead.encrypt(&nonce, Payload { msg: plain, aad: &ad }).expect("XChaCha20-Poly1305 encryption cannot fail");
        let mut out = Vec::with_capacity(NONCE_BYTES + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    fn open_with(&self, domain: &[u8], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < SEAL_OVERHEAD {
            return Err(Error::Corrupt("sealed body too short"));
        }
        let (nonce, ct) = sealed.split_at(NONCE_BYTES);
        let mut ad = Vec::with_capacity(domain.len() + aad.len());
        ad.extend_from_slice(domain);
        ad.extend_from_slice(aad);
        self.aead.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &ad }).map_err(|_| Error::Crypto("authentication failed (wrong key or damaged data)"))
    }

    /// Seal a frame's packed body; `head` is the encoded frame head (associated data).
    pub fn seal_frame(&self, head: &[u8], packed: &[u8]) -> Vec<u8> {
        self.seal_with(AAD_FRAME, head, packed)
    }

    pub fn open_frame(&self, head: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        self.open_with(AAD_FRAME, head, sealed)
    }

    /// Seal a dictionary file body; `head` binds it to its id and length.
    pub fn seal_dict(&self, head: &[u8], body: &[u8]) -> Vec<u8> {
        self.seal_with(AAD_DICT, head, body)
    }

    pub fn open_dict(&self, head: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        self.open_with(AAD_DICT, head, sealed)
    }
}

/// A fresh random key.
pub fn random_key() -> [u8; KEY_BYTES] {
    use chacha20poly1305::aead::rand_core::RngCore;
    let mut k = [0u8; KEY_BYTES];
    OsRng.fill_bytes(&mut k);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_and_tamper() {
        let c = FrameCipher::new(&random_key());
        let s = c.seal_frame(b"head", b"packed body bytes");
        assert_eq!(s.len(), b"packed body bytes".len() + SEAL_OVERHEAD);
        assert_eq!(c.open_frame(b"head", &s).unwrap(), b"packed body bytes");
        assert!(c.open_frame(b"other head", &s).is_err(), "the head is authenticated");
        let mut bad = s.clone();
        bad[30] ^= 1;
        assert!(c.open_frame(b"head", &bad).is_err());
        let other = FrameCipher::new(&random_key());
        assert!(other.open_frame(b"head", &s).is_err());
        assert!(c.open_dict(b"head", &s).is_err(), "frames and dictionaries do not mix");
        assert_ne!(c.seal_frame(b"head", b"x"), c.seal_frame(b"head", b"x"), "random nonces");
    }
}
