//! CONTROL-only bounded export of signed original removed-ID membership.
use super::*;
use driver::commitment;
use schema::RootName as N;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrunedIds {
    pub schema: u32,
    pub identity: [u8; 32],
    /// Exact independently retained current tip shared by all pages.
    pub current: RetainedFloor,
    /// Historical signed operation evidence, not a new durability receipt.
    pub signed_receipt: LifecycleReceipt,
    pub start: u64,
    pub ids: Vec<u64>,
    pub next: Option<u64>,
}
impl RetainedControl {
    /// Explicit numeric inclusive ID range. To verify the complete operation, begin
    /// at zero and feed every page into record_ids_verifier, then finish.
    /// Works after denial and loss of DATA; never requires the DATA key.
    pub fn pruned_ids(
        &self,
        expected_current: RetainedFloor,
        operation: &str,
        start: u64,
        limits: ReadLimits,
    ) -> Result<PrunedIds> {
        if !name(operation, 1024)
            || limits.entries == 0
            || limits.entries > 1000
            || limits.bytes == 0
            || limits.bytes > 16 << 20
            || limits.work == 0
            || limits.work > 1_000_000
        {
            return Err(Error::Bounds);
        }
        let owner = self.lock()?;
        if !owner.is_paged() {
            return Err(Error::Scope);
        }
        let current = owner.checkpoint.floor().map_err(|_| Error::Authority)?;
        if current != expected_current {
            return Err(Error::Conflict);
        }
        let root = schema::load(&self.directory, owner.control_cipher(), &owner.state)?;
        if start > root.next_id {
            return Err(Error::Bounds);
        }
        let tag = commitment(&root.intent, &("prune-request-v3", operation))?;
        let authority = checkpoint::Stage {
            verified: owner.paged_verified()?,
            birth: Birth::decode(&root.birth).map_err(io)?,
            directory: &self.directory,
            cipher: owner.control_cipher(),
            class: tree::KeyClass::RetainedControl,
        };
        let paths = source::Paths::new(&self.directory, &self.directory);
        let ctx = paths.ctx(
            N::Erased,
            owner.control_cipher(),
            &authority,
            owner.state.identity,
        );
        let raw = tree::get_ancestor(ctx.clone(), &root.control.erased, &prune::final_key(tag))?
            .ok_or(Error::Unavailable)?;
        let op: prune::Operation = serde_json::from_slice(&raw).map_err(|_| Error::Authority)?;
        let signed_receipt = owner.receipt_at(op.final_sequence.ok_or(Error::Authority)?)?;
        let mut page = PrunedIds {
            schema: 1,
            identity: owner.state.identity,
            current,
            signed_receipt,
            start,
            ids: Vec::new(),
            next: None,
        };
        if page.signed_receipt.event.records != 0 {
            let mut key = vec![0];
            key.extend(start.to_be_bytes());
            let mut cursor = tree::Cursor::seek_ancestor(ctx, &root.control.erased, &key)?;
            let mut next_id = start;
            let mut done = false;
            for _ in 0..limits.work {
                let Some(e) = cursor.next()? else {
                    done = true;
                    break;
                };
                if e.key.first() != Some(&0) {
                    done = true;
                    break;
                }
                if e.key.len() != 9 || e.value.len() != 32 {
                    return Err(Error::Authority);
                }
                let id = u64::from_be_bytes(e.key[1..].try_into().map_err(|_| Error::Authority)?);
                if id >= root.next_id {
                    return Err(Error::Authority);
                }
                next_id = id.checked_add(1).ok_or(Error::Bounds)?;
                if e.value.as_slice() == tag {
                    page.ids.push(id);
                    if page.ids.len() == limits.entries {
                        break;
                    }
                }
            }
            if !done && next_id < root.next_id {
                page.next = Some(next_id);
            }
        }
        json(&page, limits.bytes)?;
        owner.recheck()?;
        Ok(page)
    }
}
#[cfg(test)]
mod tests;
