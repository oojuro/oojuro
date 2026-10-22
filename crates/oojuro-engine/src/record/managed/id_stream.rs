//! Incremental verification of opaque original IDs in a signed lifecycle receipt.
//! This checks historical membership, not current durability or permanent key erasure.
use super::*;
use crate::audit::Verifier;

pub struct RecordIdsVerifier {
    hash: blake3::Hasher,
    expected: [u8; 32],
    count: u64,
    pushed: u64,
    last: Option<u64>,
    failed: bool,
}
impl LifecycleReceipt {
    /// The authority/public key and scope come from independently trusted configuration.
    /// Call push in ascending order from the first page, then finish; skipping a page fails.
    pub fn record_ids_verifier(
        &self,
        verifier: &Verifier,
        expected_scope: [u8; 32],
    ) -> Result<RecordIdsVerifier> {
        self.verify(verifier, expected_scope)?;
        let expected = self.details.record_ids.ok_or(Error::Authority)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"oojuro/managed/opaque-record-ids/v1\0");
        hash.update(&self.event.records.to_le_bytes());
        Ok(RecordIdsVerifier {
            hash,
            expected,
            count: self.event.records,
            pushed: 0,
            last: None,
            failed: false,
        })
    }
}
impl RecordIdsVerifier {
    pub fn push(&mut self, id: u64) -> Result<()> {
        if self.failed || self.pushed >= self.count || self.last.is_some_and(|last| id <= last) {
            self.failed = true;
            return Err(Error::Authority);
        }
        self.hash.update(&id.to_le_bytes());
        self.pushed += 1;
        self.last = Some(id);
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        if self.failed
            || self.pushed != self.count
            || *self.hash.finalize().as_bytes() != self.expected
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
