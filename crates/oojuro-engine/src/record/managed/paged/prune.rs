//! Streamed live rewrite and crash-resumable local retirement. Not backup cryptoerasure.
use super::*;
use crate::audit::EventKind;
use driver::{
    ManagedPagedLog, advance_index, candidate_row, commitment, data_directory, next_birth,
    read_assigned,
};
use schema::RootName as N;
#[derive(Clone, Copy)]
pub struct PruneLimits {
    pub bytes: u64,
    pub rows: u64,
}
impl Default for PruneLimits {
    fn default() -> Self {
        Self {
            bytes: 1 << 30,
            rows: 1_000_000,
        }
    }
}
#[derive(Clone)]
pub struct PagedPruneReport {
    pub selected_count: u64,
    pub selected_bytes: u64,
    pub decoded_shared_occurrences: u64,
    pub encoded_occurrences: u64,
    pub local_live_absence_verified: bool,
    pub all_owned_prior_namespaces_removed: bool,
    pub permanent: PermanentErasure,
    pub signed_receipt: LifecycleReceipt,
    pub historical: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
    pub fingerprint: [u8; 32],
    pub final_sequence: Option<u64>,
}
fn op_key(tag: [u8; 32]) -> Vec<u8> {
    let mut b = vec![1];
    b.extend(tag);
    b
}
pub(super) fn final_key(tag: [u8; 32]) -> Vec<u8> {
    let mut b = vec![2];
    b.extend(tag);
    b
}
fn id_key(id: u64) -> Vec<u8> {
    let mut b = vec![0];
    b.extend(id.to_be_bytes());
    b
}
fn matches(row: &Assigned, s: &Selection) -> bool {
    match s {
        Selection::Ids(ids) => ids.binary_search(&row.id).is_ok(),
        Selection::Agent(a) => row.input.agent_id == *a,
        Selection::Key(k) => row.input.key.as_ref() == Some(k),
        Selection::RecordedBefore(t) => row.recorded_at < *t,
    }
}
impl ManagedPagedLog {
    pub fn prune(
        &mut self,
        operation: &str,
        selection: &Selection,
        limits: PruneLimits,
    ) -> Result<PagedPruneReport> {
        if !name(operation, 1024) {
            return Err(Error::Bounds);
        }
        validate_selection(selection)?;
        if let Selection::Ids(ids) = selection
            && (ids.len() > 1000 || ids.windows(2).any(|p| p[0] >= p[1]))
        {
            return Err(Error::Bounds);
        }
        let mut work = proof::Work::new(limits.bytes, limits.rows)?;
        let control = self.control.clone();
        let mut owner = control.lock()?;
        self.current(&owner)?;
        let mut root = schema::load(&control.directory, owner.control_cipher(), &owner.state)?;
        let tag = commitment(&root.intent, &("prune-request-v3", operation))?;
        let fingerprint = commitment(&root.intent, &("prune-selection-v3", selection))?;
        if root
            .prune
            .as_ref()
            .is_some_and(|p| p.phase != schema::PrunePhase::Completed && p.tag != tag)
        {
            return Err(Error::Unavailable);
        }
        let source = self.source(&owner, &root)?;
        let prior = match source.lookup(N::Erased, &root.control.erased, &final_key(tag))? {
            Some(value) => Some(value),
            None => source.lookup(N::Erased, &root.control.erased, &op_key(tag))?,
        };
        if let Some(prior) = prior {
            let op: Operation = serde_json::from_slice(&prior).map_err(|_| Error::Authority)?;
            if op.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            if let Some(sequence) = op.final_sequence {
                let receipt = owner.receipt_at(sequence)?;
                self.witness_current(&owner)?;
                return report(receipt, true);
            }
            let p = root.prune.as_ref().ok_or(Error::Authority)?;
            if p.tag != tag
                || p.fingerprint != fingerprint
                || p.phase != schema::PrunePhase::RewrittenPendingCleanup
            {
                return Err(Error::Authority);
            }
            drop(source);
            return self.finish_prune(&mut owner, root, &mut work);
        }
        drop(source);
        // Rewrite and proof are complete before logical publication: an insufficient
        // work budget creates no tombstone/positive receipt and cannot silently skip evidence.
        let source_sequence = owner.checkpoint.body.sequence;
        let birth = next_birth(&owner, &root)?;
        let control_stage = checkpoint::Stage {
            verified: owner.paged_verified()?,
            birth: birth.clone(),
            directory: &control.directory,
            cipher: owner.control_cipher(),
            class: tree::KeyClass::RetainedControl,
        };
        let mut data_birth = birth.clone();
        data_birth.key_version = root.data_key_version.ok_or(Error::Authority)?;
        let data_stage = checkpoint::Stage {
            verified: owner.paged_verified()?,
            birth: data_birth.clone(),
            directory: &control.directory,
            cipher: owner.control_cipher(),
            class: tree::KeyClass::Data,
        };
        let old = root.clone();
        let source = self.source(&owner, &old)?;
        root.data_namespace = Some(oojuro_store::crypto::random_key());
        let target = data_directory(&self.directory, &root)?;
        driver::create_namespace(&target)?;
        directory_create(&target.join("packs"))?;
        directory_create(&target.join("trees"))?;
        let paths = source::Paths::new(&control.directory, &target);
        paths.create_data()?;
        let empty = |n| {
            tree::Builder::new(paths.ctx(n, &self.cipher, &data_stage, owner.state.identity))?
                .finish()
        };
        let mut records = tree::Editor::new(
            paths.ctx(N::Record, &self.cipher, &data_stage, owner.state.identity),
            empty(N::Record)?,
        )?;
        let mut keys = tree::Editor::new(
            paths.ctx(
                N::KeyVersion,
                &self.cipher,
                &data_stage,
                owner.state.identity,
            ),
            empty(N::KeyVersion)?,
        )?;
        let mut segments = tree::Editor::new(
            paths.ctx(N::Segment, &self.cipher, &data_stage, owner.state.identity),
            empty(N::Segment)?,
        )?;
        let physical_ctx = paths.physical(&self.cipher, &data_stage, owner.state.identity);
        let mut physical =
            physical::Editor::new(physical_ctx.clone(), physical::Roots::empty(&physical_ctx)?)?;
        let mut erased = tree::Editor::new(
            paths.ctx(
                N::Erased,
                owner.control_cipher(),
                &control_stage,
                owner.state.identity,
            ),
            root.control.erased.clone(),
        )?;
        let mut cursor = tree::Cursor::seek_ancestor(
            source.ctx(N::Record),
            &old.data.as_ref().ok_or(Error::Authority)?.record,
            &[],
        )?;
        let mut selected_count = 0u64;
        let mut selected_bytes = 0u64;
        let mut live_records = 0u64;
        let mut live_bytes = 0u64;
        let mut packs = codec::Packs::new(
            target.join("packs"),
            data_birth,
            self.cipher.clone(),
            root.format,
            &mut physical,
            &mut segments,
            root.next_file,
            0,
            true,
        );
        while let Some(e) = cursor.next()? {
            self.witness_current(&owner)?;
            work.charge_rows(1)?;
            let id = u64::from_be_bytes(e.key.try_into().map_err(|_| Error::Authority)?);
            let Some(row) = read_assigned(&source, &self.directory, id)? else {
                continue;
            };
            let bytes = codec::encoded_len(&row, root.format)?;
            work.charge_bytes(bytes as u64)?;
            if matches(&row, selection) {
                selected_count = selected_count.checked_add(1).ok_or(Error::Bounds)?;
                selected_bytes = selected_bytes
                    .checked_add(bytes as u64)
                    .ok_or(Error::Bounds)?;
                erased.insert_immutable(tree::Entry::new(id_key(id), tag.to_vec())?)?;
                continue;
            }
            let reference = packs.row(row.id, row.recorded_at, &row.input)?;
            records.insert_immutable(tree::Entry::new(
                id.to_be_bytes().to_vec(),
                json(&reference, 1024)?,
            )?)?;
            if let Some(key) = &row.input.key {
                let mut k = commitment(&root.intent, &("key-v3", key))?.to_vec();
                k.extend((row.input.valid_at as u64 ^ (1 << 63)).to_be_bytes());
                k.extend(id.to_be_bytes());
                keys.insert_immutable(tree::Entry::new(k, id.to_be_bytes().to_vec())?)?;
            }
            live_records = live_records.checked_add(1).ok_or(Error::Bounds)?;
            live_bytes = live_bytes.checked_add(bytes as u64).ok_or(Error::Bounds)?;
        }
        let (file_id, physical_seq) = packs.finish()?;
        root.data = Some(schema::DataRoots {
            record: records.finish()?,
            key_version: keys.finish()?,
            segment: segments.finish()?,
        });
        root.physical = Some(physical.finish()?);
        root.next_file = file_id;
        root.next_physical = physical_seq;
        root.live_records = live_records;
        root.live_bytes = live_bytes;
        root.prune = Some(schema::Prune {
            tag,
            fingerprint,
            phase: schema::PrunePhase::RewrittenPendingCleanup,
            selected_count,
            selected_bytes,
            id_commitment: [0; 32],
            recorded_before: match selection {
                Selection::RecordedBefore(t) => Some(*t),
                _ => None,
            },
            decoded_occurrences: 0,
            encoded_occurrences: 0,
            operation_sequence: owner
                .checkpoint
                .body
                .sequence
                .checked_add(1)
                .ok_or(Error::Bounds)?,
            verified_generation: birth.generation,
            source_sequence,
            files: None,
            file_count: 0,
            file_bytes: 0,
        });
        let mut ids = blake3::Hasher::new();
        ids.update(b"oojuro/managed/opaque-record-ids/v1\0");
        ids.update(&selected_count.to_le_bytes());
        let metadata = serde_json::json!({"identity":self.identity,"format":std::str::from_utf8(driver::marker(root.format)?).map_err(|_|Error::Authority)?});
        let mut decoded = 0u64;
        let mut encoded = 0u64;
        let mut cursor = tree::Cursor::seek_ancestor(
            source.ctx(N::Record),
            &old.data.as_ref().ok_or(Error::Authority)?.record,
            &[],
        )?;
        while let Some(e) = cursor.next()? {
            self.witness_current(&owner)?;
            work.charge_rows(1)?;
            let id = u64::from_be_bytes(e.key.try_into().map_err(|_| Error::Authority)?);
            let Some(row) = read_assigned(&source, &self.directory, id)? else {
                continue;
            };
            if matches(&row, selection) {
                ids.update(&id.to_le_bytes());
                let mut live = |visit: &mut dyn FnMut(&Assigned) -> Result<()>| {
                    let mut c = tree::Cursor::seek_ancestor(
                        paths.ctx(N::Record, &self.cipher, &data_stage, owner.state.identity),
                        &root.data.as_ref().ok_or(Error::Authority)?.record,
                        &[],
                    )?;
                    while let Some(e) = c.next()? {
                        self.witness_current(&owner)?;
                        let id =
                            u64::from_be_bytes(e.key.try_into().map_err(|_| Error::Authority)?);
                        let candidate = candidate_row(self, &owner, &root, &data_stage, id)?;
                        visit(&candidate)?;
                        self.witness_current(&owner)?;
                    }
                    Ok(())
                };
                let mut files = |visit: &mut dyn FnMut(&Path) -> Result<()>| {
                    inventory::proof_files(self, &owner, &root, visit)
                };
                let count = proof::scan_one(&row, &metadata, &mut live, &mut files, &mut work)?;
                decoded = decoded.checked_add(count.decoded).ok_or(Error::Bounds)?;
                encoded = encoded.checked_add(count.encoded).ok_or(Error::Bounds)?;
            }
        }
        // Verify exact original survivors, not only patterns or valid AEAD.
        let mut originals = tree::Cursor::seek_ancestor(
            source.ctx(N::Record),
            &old.data.as_ref().ok_or(Error::Authority)?.record,
            &[],
        )?;
        while let Some(e) = originals.next()? {
            work.charge_rows(1)?;
            let id = u64::from_be_bytes(e.key.try_into().map_err(|_| Error::Authority)?);
            if let Some(row) = read_assigned(&source, &self.directory, id)?
                && !matches(&row, selection)
            {
                let actual = candidate_row(self, &owner, &root, &data_stage, id)?;
                if codec::digest(&actual, &root.intent, root.format)?
                    != codec::digest(&row, &root.intent, root.format)?
                {
                    return Err(Error::Authority);
                }
            }
        }
        let (files, file_count, file_bytes) =
            inventory::build(self, &owner, &root, &control_stage, &mut work)?;
        let p = root.prune.as_mut().ok_or(Error::Authority)?;
        p.id_commitment = *ids.finalize().as_bytes();
        p.decoded_occurrences = decoded;
        p.encoded_occurrences = encoded;
        p.files = Some(files);
        p.file_count = file_count;
        p.file_bytes = file_bytes;
        erased.insert_immutable(tree::Entry::new(
            op_key(tag),
            json(
                &Operation {
                    fingerprint,
                    final_sequence: None,
                },
                1024,
            )?,
        )?)?;
        root.control.erased = erased.finish()?;
        advance_index(&paths, &control_stage, &owner, &mut root)?;
        root.birth = birth.encode().map_err(io)?;
        inventory::verify(self, &owner, &root, &control_stage, &mut work)?;
        let retired = inventory::preflight_retire(self, &owner, &root, &mut work)?;
        let future_bytes = file_bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(work.charged_bytes()))
            .ok_or(Error::Bounds)?;
        let future_rows = file_count
            .checked_mul(2)
            .and_then(|n| n.checked_add(retired))
            .and_then(|n| n.checked_add(work.charged_rows()))
            .ok_or(Error::Bounds)?;
        if future_bytes > limits.bytes || future_rows > limits.rows {
            return Err(Error::Bounds);
        }
        drop(source);
        self.publish_event(&mut owner, root.clone(), None, false)?;
        self.finish_prune(&mut owner, root, &mut work)
    }
    fn finish_prune(
        &self,
        owner: &mut control::Inner,
        mut root: schema::CommitRoot,
        work: &mut proof::Work,
    ) -> Result<PagedPruneReport> {
        self.current(owner)?;
        {
            let source = self.source(owner, &root)?;
            inventory::verify(self, owner, &root, &source.control, work)?;
        }
        inventory::retire(self, owner, &root, work)?;
        {
            let source = self.source(owner, &root)?;
            inventory::verify(self, owner, &root, &source.control, work)?;
        }
        #[cfg(test)]
        self.phase(Phase::AfterCleanup);
        let birth = next_birth(owner, &root)?;
        let control_stage = checkpoint::Stage {
            verified: owner.paged_verified()?,
            birth: birth.clone(),
            directory: &self.control.directory,
            cipher: owner.control_cipher(),
            class: tree::KeyClass::RetainedControl,
        };
        let paths = source::Paths::new(
            &self.control.directory,
            &data_directory(&self.directory, &root)?,
        );
        let p = root.prune.as_mut().ok_or(Error::Authority)?;
        p.phase = schema::PrunePhase::Completed;
        let kind = if p.recorded_before.is_some() {
            EventKind::RetentionDropVerified
        } else {
            EventKind::LiveRewriteVerified
        };
        let mut erased = tree::Editor::new(
            paths.ctx(
                N::Erased,
                owner.control_cipher(),
                &control_stage,
                owner.state.identity,
            ),
            root.control.erased,
        )?;
        erased.insert_immutable(tree::Entry::new(
            final_key(p.tag),
            json(
                &Operation {
                    fingerprint: p.fingerprint,
                    final_sequence: Some(
                        owner
                            .checkpoint
                            .body
                            .sequence
                            .checked_add(1)
                            .ok_or(Error::Bounds)?,
                    ),
                },
                1024,
            )?,
        )?)?;
        root.control.erased = erased.finish()?;
        advance_index(&paths, &control_stage, owner, &mut root)?;
        root.birth = birth.encode().map_err(io)?;
        self.publish_event(owner, root, Some(kind), true)?;
        #[cfg(test)]
        self.phase(Phase::BeforeAck);
        self.current(owner)?;
        report(owner.receipt()?, false)
    }
}
fn report(signed_receipt: LifecycleReceipt, historical: bool) -> Result<PagedPruneReport> {
    Ok(PagedPruneReport {
        selected_count: signed_receipt.event.records,
        selected_bytes: signed_receipt.event.bytes,
        decoded_shared_occurrences: signed_receipt.details.decoded_occurrences,
        encoded_occurrences: signed_receipt.details.encoded_occurrences,
        local_live_absence_verified: signed_receipt.details.decoded_occurrences == 0
            && signed_receipt.details.encoded_occurrences == 0,
        all_owned_prior_namespaces_removed: true,
        permanent: PermanentErasure::PendingUnsupportedLocalKeyAuthority,
        signed_receipt,
        historical,
    })
}

#[cfg(test)]
mod tests;
