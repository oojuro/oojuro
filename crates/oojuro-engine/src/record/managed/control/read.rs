//! Content-free current retained-authority reads; never a copy of encrypted control state.
use super::*;
const MAX_AUDIT_PAGE: usize = 16 << 20;
const MAX_AUDIT_ENTRIES: usize = 1000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlStatus {
    pub schema: u32,
    pub origin: [u8; 32],
    pub identity: [u8; 32],
    pub status: Status,
    pub generation: u64,
    /// Evidence retained by the control owner, not live data after denial.
    pub retained_snapshot_records: u64,
    pub retained_snapshot_bytes: u64,
    pub local_cleanup_completed: bool,
    pub current_floor: RetainedFloor,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlAuditEntry {
    pub event: Option<AuditEvent>,
    pub details: ReceiptDetails,
    pub ciphertext_digest: [u8; 32],
    pub checkpoint: SignedCheckpoint,
}
impl ControlAuditEntry {
    /// Signature and public-head binding. Freshness/contiguous chain requires the
    /// independently retained CURRENT floor, never this entry's self-asserted tip.
    pub fn verify(&self, verifier: &Verifier, expected_identity: [u8; 32]) -> Result<()> {
        if self.checkpoint.body.events != self.checkpoint.body.sequence {
            return Err(Error::Authority);
        }
        if let Some(event) = &self.event {
            return LifecycleReceipt {
                event: event.clone(),
                details: self.details.clone(),
                control_ciphertext_digest: self.ciphertext_digest,
                checkpoint: self.checkpoint.clone(),
            }
            .verify(verifier, expected_identity);
        }
        verifier
            .verify_checkpoint(&self.checkpoint, expected_identity)
            .map_err(audit_error)?;
        if self.checkpoint.body.domain != Domain::NativeLifecycle
            || self.details != ReceiptDetails::default()
            || self.checkpoint.body.head
                != receipt_head(
                    self.ciphertext_digest,
                    None,
                    self.checkpoint.body.previous,
                    &self.details,
                )?
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlAuditPage {
    pub schema: u32,
    pub identity: [u8; 32],
    /// All pages must be read against this SAME independently retained tip.
    pub current: RetainedFloor,
    /// Zero-based retained control ordinal, not a record ID or count of visible events.
    pub start: u64,
    pub entries: Vec<ControlAuditEntry>,
    pub next: Option<u64>,
}
impl RetainedControl {
    /// Reads retained evidence even after data denial; does not create/open data.
    pub fn status(&self) -> Result<ControlStatus> {
        let owner = self.lock()?;
        let state = &owner.state;
        let status = ControlStatus {
            schema: 1,
            origin: state.origin,
            identity: state.identity,
            status: state.status,
            generation: state.generation,
            retained_snapshot_records: state.live_records,
            retained_snapshot_bytes: state.live_bytes,
            local_cleanup_completed: state.pruned,
            current_floor: owner.checkpoint.floor().map_err(audit_error)?,
        };
        owner.current()?;
        Ok(status)
    }
    /// A bounded page pinned to an exact current tip. Numeric continuation is only
    /// valid with unchanged expected_current; append/denial makes old paging refuse.
    /// The caller verifies every signature/link and the exact terminal trusted floor.
    pub fn export_audit(
        &self,
        expected_current: RetainedFloor,
        start: u64,
        limits: Limits,
    ) -> Result<ControlAuditPage> {
        if limits.entries == 0
            || limits.entries > MAX_AUDIT_ENTRIES
            || limits.bytes == 0
            || limits.bytes > MAX_AUDIT_PAGE
        {
            return Err(Error::Bounds);
        }
        let owner = self.lock()?;
        let current = owner.checkpoint.floor().map_err(audit_error)?;
        if current != expected_current {
            return Err(Error::Conflict);
        }
        let end = owner.store.len();
        if start >= end {
            return Err(Error::Bounds);
        }
        let verifier = Verifier::new(
            owner.authority,
            Domain::NativeLifecycle,
            owner.signer.public_key(),
        );
        let mut page = ControlAuditPage {
            schema: 1,
            identity: owner.state.identity,
            current,
            start,
            entries: Vec::new(),
            next: Some(start),
        };
        // Each attempt serializes at most one bounded page, never lifelong history.
        let stop = end.min(
            start
                .checked_add(limits.entries as u64)
                .ok_or(Error::Bounds)?,
        );
        for ordinal in start..stop {
            let bytes = owner.store.get(ordinal).map_err(io)?;
            if bytes.len() > 32 << 10 {
                return Err(Error::Bounds);
            }
            let envelope: Envelope =
                serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
            let entry = ControlAuditEntry {
                event: envelope.event,
                details: envelope.details,
                ciphertext_digest: *blake3::hash(&envelope.sealed).as_bytes(),
                checkpoint: envelope.checkpoint,
            };
            entry.verify(&verifier, page.identity)?;
            if entry.checkpoint.body.sequence != ordinal.checked_add(1).ok_or(Error::Bounds)? {
                return Err(Error::Authority);
            }
            if let Some(prior) = page.entries.last()
                && entry.checkpoint.body.previous
                    != prior.checkpoint.digest().map_err(audit_error)?
            {
                return Err(Error::Authority);
            }
            page.entries.push(entry);
            page.next = if ordinal + 1 == end {
                None
            } else {
                Some(ordinal + 1)
            };
            if json(&page, MAX_AUDIT_PAGE + (64 << 10))?.len() > limits.bytes {
                page.entries.pop();
                page.next = Some(ordinal);
                break;
            }
        }
        if page.entries.is_empty() {
            return Err(Error::Bounds);
        }
        json(&page, limits.bytes)?;
        owner.current()?;
        Ok(page)
    }
}
#[cfg(test)]
mod tests;
