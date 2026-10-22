//! Public Ed25519 audit statements. Keys, durable publication and current witnesses
//! are independent responsibilities; a bundled public key is never a trust root.
use ring::signature::{self, Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};

pub const MAX_EXPORT_BYTES: usize = 16 << 20;
pub const MAX_ENTRIES: usize = 100_000;
const SIGN_DOMAIN: &[u8] = b"oojuro/audit/ed25519/checkpoint/v1\0";
const EVENT_DOMAIN: &[u8] = b"oojuro/audit/content-free/event/v1\0";
const DIGEST_DOMAIN: &[u8] = b"oojuro/audit/signed-checkpoint/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Bounds,
    Signature,
    Scope,
    Chain,
    Witness,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Separate keys and externally pinned identities must be used for these domains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    NativeLifecycle,
    HostedRead,
}
impl Domain {
    fn byte(self) -> u8 {
        match self {
            Self::NativeLifecycle => 1,
            Self::HostedRead => 2,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointBody {
    pub schema: u32,
    pub authority: [u8; 32],
    pub domain: Domain,
    pub scope: [u8; 32],
    /// Checkpoint ordinal, independent of the event count (timer-only checkpoints are valid).
    pub sequence: u64,
    pub events: u64,
    /// Digest of the preceding signed checkpoint, or zero for checkpoint one.
    pub previous: [u8; 32],
    /// Opaque event/ciphertext-chain commitment. Never a raw/context plaintext hash.
    pub head: [u8; 32],
    /// Signer's claimed UTC microseconds; this is not a trusted timestamp service.
    pub issued_at: i64,
}
impl CheckpointBody {
    fn bytes(&self) -> Result<Vec<u8>> {
        if self.schema != 1 || self.sequence == 0 {
            return Err(Error::Invalid);
        }
        let mut bytes = SIGN_DOMAIN.to_vec();
        bytes.extend(self.schema.to_le_bytes());
        bytes.push(self.domain.byte());
        bytes.extend(self.authority);
        bytes.extend(self.scope);
        bytes.extend(self.sequence.to_le_bytes());
        bytes.extend(self.events.to_le_bytes());
        bytes.extend(self.previous);
        bytes.extend(self.head);
        bytes.extend(self.issued_at.to_le_bytes());
        Ok(bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCheckpoint {
    pub body: CheckpointBody,
    pub signature: Vec<u8>,
}
impl SignedCheckpoint {
    pub fn digest(&self) -> Result<[u8; 32]> {
        if self.signature.len() != 64 {
            return Err(Error::Invalid);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(DIGEST_DOMAIN);
        hasher.update(&self.body.bytes()?);
        hasher.update(&self.signature);
        Ok(*hasher.finalize().as_bytes())
    }
    pub fn floor(&self) -> Result<RetainedFloor> {
        Ok(RetainedFloor {
            sequence: self.body.sequence,
            digest: self.digest()?,
        })
    }
}
/// Obtain this from an independent retained witness, never from the bundle being checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedFloor {
    pub sequence: u64,
    pub digest: [u8; 32],
}
/// PKCS8 is supplied by the caller's separate signing authority. No private-key accessor.
pub struct Signer {
    authority: [u8; 32],
    domain: Domain,
    pair: Ed25519KeyPair,
}
impl Signer {
    pub fn from_pkcs8(authority: [u8; 32], domain: Domain, pkcs8: &[u8]) -> Result<Self> {
        if pkcs8.len() > 4096 {
            return Err(Error::Bounds);
        }
        Ok(Self {
            authority,
            domain,
            pair: Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| Error::Invalid)?,
        })
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.pair
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 key size")
    }
    pub fn sign(&self, body: CheckpointBody) -> Result<SignedCheckpoint> {
        if body.authority != self.authority || body.domain != self.domain {
            return Err(Error::Scope);
        }
        let signature = self.pair.sign(&body.bytes()?).as_ref().to_vec();
        Ok(SignedCheckpoint { body, signature })
    }
}
/// Trust is configured externally, not inferred from an exported public key.
pub struct Verifier {
    authority: [u8; 32],
    domain: Domain,
    public_key: [u8; 32],
}
impl Verifier {
    pub fn new(authority: [u8; 32], domain: Domain, public_key: [u8; 32]) -> Self {
        Self {
            authority,
            domain,
            public_key,
        }
    }
    fn signature(&self, checkpoint: &SignedCheckpoint) -> Result<()> {
        if checkpoint.body.authority != self.authority || checkpoint.body.domain != self.domain {
            return Err(Error::Scope);
        }
        if checkpoint.signature.len() != 64 {
            return Err(Error::Signature);
        }
        signature::UnparsedPublicKey::new(&signature::ED25519, self.public_key)
            .verify(&checkpoint.body.bytes()?, &checkpoint.signature)
            .map_err(|_| Error::Signature)
    }
    pub fn verify_checkpoint(
        &self,
        checkpoint: &SignedCheckpoint,
        expected_scope: [u8; 32],
    ) -> Result<()> {
        if checkpoint.body.scope != expected_scope {
            return Err(Error::Scope);
        }
        self.signature(checkpoint)
    }
    /// Verify a contiguous chain ending at the externally trusted CURRENT checkpoint.
    /// A range may start at the retained floor, otherwise it must start at genesis.
    /// Hosted owners separately verify each checkpoint's authorized scope and encrypted head.
    pub fn verify_chain(
        &self,
        checkpoints: &[SignedCheckpoint],
        trusted_current: RetainedFloor,
        retained_floor: Option<RetainedFloor>,
    ) -> Result<()> {
        if checkpoints.is_empty() || checkpoints.len() > MAX_ENTRIES {
            return Err(Error::Bounds);
        }
        let first = &checkpoints[0];
        let starts_at_floor = retained_floor.is_some_and(|f| first.floor() == Ok(f));
        if !starts_at_floor && (first.body.sequence != 1 || first.body.previous != [0; 32]) {
            return Err(Error::Chain);
        }
        let mut found_floor = retained_floor.is_none();
        let mut previous: Option<&SignedCheckpoint> = None;
        for checkpoint in checkpoints {
            self.signature(checkpoint)?;
            if let Some(last) = previous
                && (last.body.sequence.checked_add(1) != Some(checkpoint.body.sequence)
                    || checkpoint.body.previous != last.digest()?
                    || checkpoint.body.events < last.body.events)
            {
                return Err(Error::Chain);
            }
            if retained_floor.is_some_and(|f| checkpoint.floor() == Ok(f)) {
                found_floor = true;
            }
            previous = Some(checkpoint);
        }
        if !found_floor || checkpoints.last().ok_or(Error::Chain)?.floor()? != trusted_current {
            return Err(Error::Witness);
        }
        Ok(())
    }
}
/// These statements deliberately contain no "permanent destruction" assertion:
/// caller-held local keys cannot establish destruction of copied keys or backup wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    LogicalDenialPendingPermanent,
    LiveRewriteVerified,
    RetentionDropVerified,
    CrossPrincipalRead,
    PayloadExport,
}
impl EventKind {
    fn byte(self) -> u8 {
        match self {
            Self::LogicalDenialPendingPermanent => 1,
            Self::LiveRewriteVerified => 2,
            Self::RetentionDropVerified => 3,
            Self::CrossPrincipalRead => 4,
            Self::PayloadExport => 5,
        }
    }
    fn domain(self) -> Domain {
        match self {
            Self::CrossPrincipalRead | Self::PayloadExport => Domain::HostedRead,
            _ => Domain::NativeLifecycle,
        }
    }
}
/// Opaque identifiers must be authority-issued IDs, never hashes of denied/private plaintext.
/// Producers own truthful counts, authenticated actors and before/after ciphertext commitments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEvent {
    pub sequence: u64,
    pub recorded_at: i64,
    pub kind: EventKind,
    pub scope: [u8; 32],
    pub actor: [u8; 32],
    pub records: u64,
    pub bytes: u64,
    pub before: [u8; 32],
    pub after: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    pub event: AuditEvent,
    pub previous: [u8; 32],
}
impl AuditEntry {
    pub fn digest(&self) -> Result<[u8; 32]> {
        if self.event.sequence == 0 {
            return Err(Error::Invalid);
        }
        let e = &self.event;
        let mut hasher = blake3::Hasher::new();
        hasher.update(EVENT_DOMAIN);
        hasher.update(&self.previous);
        hasher.update(&e.sequence.to_le_bytes());
        hasher.update(&e.recorded_at.to_le_bytes());
        hasher.update(&[e.kind.byte()]);
        hasher.update(&e.scope);
        hasher.update(&e.actor);
        hasher.update(&e.records.to_le_bytes());
        hasher.update(&e.bytes.to_le_bytes());
        hasher.update(&e.before);
        hasher.update(&e.after);
        Ok(*hasher.finalize().as_bytes())
    }
}
/// Native content-free chain export. Hosted encrypted-chain bundles reuse CheckpointBody
/// and Verifier but must verify their own encrypted event commitment format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditExport {
    pub schema: u32,
    pub authority: [u8; 32],
    pub domain: Domain,
    pub scope: [u8; 32],
    pub public_key: [u8; 32],
    pub entries: Vec<AuditEntry>,
    pub checkpoints: Vec<SignedCheckpoint>,
}
impl AuditExport {
    pub fn verify(
        &self,
        verifier: &Verifier,
        trusted_current: RetainedFloor,
        retained_floor: Option<RetainedFloor>,
    ) -> Result<()> {
        if self.schema != 1
            || self.authority != verifier.authority
            || self.domain != verifier.domain
            || self.public_key != verifier.public_key
        {
            return Err(Error::Scope);
        }
        if self.entries.len() > MAX_ENTRIES || self.checkpoints.len() > MAX_ENTRIES {
            return Err(Error::Bounds);
        }
        // A native export is complete from genesis. Partial hosted export is a separate protocol.
        if self
            .checkpoints
            .first()
            .is_none_or(|c| c.body.sequence != 1 || c.body.previous != [0; 32])
        {
            return Err(Error::Chain);
        }
        verifier.verify_chain(&self.checkpoints, trusted_current, retained_floor)?;
        let mut heads = Vec::with_capacity(self.entries.len() + 1);
        heads.push([0; 32]);
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.event.sequence != index as u64 + 1
                || entry.previous != *heads.last().ok_or(Error::Chain)?
                || entry.event.kind.domain() != self.domain
            {
                return Err(Error::Chain);
            }
            heads.push(entry.digest()?);
        }
        for checkpoint in &self.checkpoints {
            verifier.verify_checkpoint(checkpoint, self.scope)?;
            let count = usize::try_from(checkpoint.body.events).map_err(|_| Error::Bounds)?;
            if heads.get(count) != Some(&checkpoint.body.head) {
                return Err(Error::Chain);
            }
        }
        if self.checkpoints.last().ok_or(Error::Chain)?.body.events != self.entries.len() as u64 {
            return Err(Error::Chain);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        // A counting writer rejects the bound before materializing an oversized output.
        struct Limited {
            bytes: Vec<u8>,
        }
        impl std::io::Write for Limited {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self
                    .bytes
                    .len()
                    .checked_add(buf.len())
                    .is_none_or(|n| n > MAX_EXPORT_BYTES)
                {
                    return Err(std::io::Error::other("audit export bound"));
                }
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        if self.entries.len() > MAX_ENTRIES || self.checkpoints.len() > MAX_ENTRIES {
            return Err(Error::Bounds);
        }
        let mut out = Limited { bytes: vec![] };
        serde_json::to_writer(&mut out, self).map_err(|_| Error::Bounds)?;
        Ok(out.bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_EXPORT_BYTES {
            return Err(Error::Bounds);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        if value.entries.len() > MAX_ENTRIES || value.checkpoints.len() > MAX_ENTRIES {
            return Err(Error::Bounds);
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests;
