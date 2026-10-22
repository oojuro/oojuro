//! Retained control and its witness live outside the managed data backup.
//! This authority enforces current logical denial, not destruction of caller-held keys.
use super::*;
use crate::audit::{
    AuditEntry, AuditEvent, CheckpointBody, Domain, EventKind, RetainedFloor, SignedCheckpoint,
    Signer, Verifier,
};
use std::sync::{Mutex, MutexGuard};
const CONTROL_AAD: &[u8] = b"oojuro/managed/control/v1\0";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    DeniedPendingPermanent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    pub origin: [u8; 32],
    pub identity: [u8; 32],
    pub generation: u64,
    pub physical: [u8; 32],
    pub logical: [u8; 32],
    pub status: Status,
    pub pruned: bool,
    pub prune: Option<PruneEvidence>,
    pub live_records: u64,
    pub live_bytes: u64,
    pub audit: Option<AuditEvent>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PruneEvidence {
    pub tag: [u8; 32],
    pub generation: u64,
    pub decoded_matches: u64,
    pub encoded_matches: u64,
    pub before: [u8; 32],
    pub recorded_before: Option<i64>,
    pub record_ids: [u8; 32],
}
#[derive(Clone)]
pub(super) struct PruneProof {
    pub evidence: PruneEvidence,
    pub complete: bool,
    pub checkpoint: SignedCheckpoint,
    pub receipt: Option<LifecycleReceipt>,
}
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptDetails {
    /// Commits opaque numeric record IDs, never text/context fingerprints.
    pub record_ids: Option<[u8; 32]>,
    /// Original engine-ingestion cutoff, not a claimed event-time retention rule.
    pub recorded_before: Option<i64>,
    pub decoded_occurrences: u64,
    pub encoded_occurrences: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    sealed: Vec<u8>,
    event: Option<AuditEvent>,
    details: ReceiptDetails,
    checkpoint: SignedCheckpoint,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleReceipt {
    pub event: AuditEvent,
    pub details: ReceiptDetails,
    pub control_ciphertext_digest: [u8; 32],
    pub checkpoint: SignedCheckpoint,
}
impl LifecycleReceipt {
    pub fn verify_record_ids(&self, ids: &[u64]) -> Result<()> {
        if ids.len() as u64 != self.event.records
            || self.details.record_ids != Some(ids_digest(ids)?)
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        json(self, 64 << 10)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 << 10 {
            return Err(Error::Bounds);
        }
        serde_json::from_slice(bytes).map_err(|_| Error::Authority)
    }
    pub fn verify(&self, verifier: &Verifier, expected_scope: [u8; 32]) -> Result<()> {
        verifier
            .verify_checkpoint(&self.checkpoint, expected_scope)
            .map_err(audit_error)?;
        if self.checkpoint.body.domain != Domain::NativeLifecycle
            || self.event.scope != expected_scope
            || self.event.actor != self.checkpoint.body.authority
            || self.event.sequence != self.checkpoint.body.events
            || (matches!(self.event.kind, EventKind::RetentionDropVerified)
                != self.details.recorded_before.is_some())
            || matches!(
                self.event.kind,
                EventKind::CrossPrincipalRead | EventKind::PayloadExport
            )
            || self.checkpoint.body.head
                != receipt_head(
                    self.control_ciphertext_digest,
                    Some(&self.event),
                    self.checkpoint.body.previous,
                    &self.details,
                )?
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
fn receipt_head(
    ciphertext: [u8; 32],
    event: Option<&AuditEvent>,
    previous: [u8; 32],
    details: &ReceiptDetails,
) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"oojuro/managed/control-and-receipt/v1\0");
    hasher.update(&ciphertext);
    match details.record_ids {
        Some(digest) => {
            hasher.update(&[1]);
            hasher.update(&digest);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    match details.recorded_before {
        Some(cutoff) => {
            hasher.update(&[1]);
            hasher.update(&cutoff.to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&details.decoded_occurrences.to_le_bytes());
    hasher.update(&details.encoded_occurrences.to_le_bytes());
    match event {
        Some(event) => {
            hasher.update(&[1]);
            hasher.update(
                &AuditEntry {
                    event: event.clone(),
                    previous,
                }
                .digest()
                .map_err(audit_error)?,
            );
        }
        None => {
            hasher.update(&[0]);
        }
    }
    Ok(*hasher.finalize().as_bytes())
}
fn public_receipt(envelope: &Envelope) -> Option<LifecycleReceipt> {
    envelope.event.as_ref().map(|event| LifecycleReceipt {
        event: event.clone(),
        details: envelope.details.clone(),
        control_ciphertext_digest: *blake3::hash(&envelope.sealed).as_bytes(),
        checkpoint: envelope.checkpoint.clone(),
    })
}
fn receipt_details(state: &State) -> ReceiptDetails {
    if state.audit.is_none() {
        return ReceiptDetails::default();
    }
    match &state.prune {
        Some(proof) => ReceiptDetails {
            record_ids: Some(proof.record_ids),
            recorded_before: if state
                .audit
                .as_ref()
                .is_some_and(|e| e.kind == EventKind::RetentionDropVerified)
            {
                proof.recorded_before
            } else {
                None
            },
            decoded_occurrences: proof.decoded_matches,
            encoded_occurrences: proof.encoded_matches,
        },
        None => ReceiptDetails::default(),
    }
}
/// This independent witness must itself be excluded from all data/control restores.
/// A filesystem witness is a durable logical trust boundary, not rollback-proof hardware.
pub struct FileWitness {
    directory: PathBuf,
    _lock: fs::File,
}
impl FileWitness {
    pub fn open(directory: &Path) -> Result<Self> {
        directory_create(directory)?;
        let lock = owner_lock(directory)?;
        Ok(Self {
            directory: directory.canonicalize().map_err(io)?,
            _lock: lock,
        })
    }
    fn current(&self) -> Result<Option<RetainedFloor>> {
        let path = self.directory.join("CURRENT");
        match read_regular(&path, 4096) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| Error::Authority),
            Err(Error::Unavailable) if !path.exists() => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn advance(&self, expected: Option<RetainedFloor>, next: RetainedFloor) -> Result<()> {
        if self.current()? != expected {
            return Err(Error::Authority);
        }
        let bytes = json(&next, 4096)?;
        let staged = self.directory.join("NEXT");
        // An interrupted stage has no authority; remove only this owned regular file.
        if staged.exists() {
            remove_regular(&staged)?;
        }
        write_new(&staged, &bytes)?;
        fs::rename(&staged, self.directory.join("CURRENT")).map_err(io)?;
        sync_dir(&self.directory)
    }
}
pub struct RetainedControl {
    pub(super) directory: PathBuf,
    pub(super) witness_directory: PathBuf,
    inner: Mutex<Inner>,
}
pub(super) struct Inner {
    store: Store,
    cipher: Arc<FrameCipher>,
    signer: Signer,
    authority: [u8; 32],
    witness: FileWitness,
    pub state: State,
    pub checkpoint: SignedCheckpoint,
    failed: bool,
    pub prunes: BTreeMap<[u8; 32], PruneProof>,
    #[cfg(test)]
    pub hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl RetainedControl {
    /// Signing key, control key, control directory and witness are independent of data.
    /// Existing control requires its retained witness; missing witness never bootstraps it.
    pub fn open(
        directory: &Path,
        key: &[u8; 32],
        signer: Signer,
        authority: [u8; 32],
        witness: FileWitness,
        identity: [u8; 32],
    ) -> Result<Self> {
        directory_create(directory)?;
        let directory = directory.canonicalize().map_err(io)?;
        if overlaps(&directory, &witness.directory) {
            return Err(Error::Invalid);
        }
        let cipher = Arc::new(FrameCipher::new(key));
        let mut store = open_store(&directory, cipher.clone())?;
        let public = signer.public_key();
        let mut state = None;
        let mut checkpoint: Option<SignedCheckpoint> = None;
        let retained = witness.current()?;
        if !store.is_empty() && retained.is_none() {
            return Err(Error::Authority);
        }
        let mut found_floor = retained.is_none();
        let verifier = Verifier::new(authority, Domain::NativeLifecycle, public);
        let mut prunes = BTreeMap::new();
        for sequence in 0..store.len() {
            let bytes = store.get(sequence).map_err(io)?;
            if bytes.len() > 32 << 10 {
                return Err(Error::Bounds);
            }
            let envelope: Envelope =
                serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
            let cp = &envelope.checkpoint;
            verifier
                .verify_checkpoint(cp, identity)
                .map_err(audit_error)?;
            let expected_previous = checkpoint
                .as_ref()
                .map(|c| c.digest().map_err(audit_error))
                .transpose()?
                .unwrap_or([0; 32]);
            if cp.body.previous != expected_previous || cp.body.events != sequence + 1 {
                return Err(Error::Authority);
            }
            if retained.is_some_and(|floor| cp.floor() == Ok(floor)) {
                found_floor = true;
            }
            if cp.body.sequence != sequence + 1
                || cp.body.scope != identity
                || cp.body.head
                    != receipt_head(
                        *blake3::hash(&envelope.sealed).as_bytes(),
                        envelope.event.as_ref(),
                        cp.body.previous,
                        &envelope.details,
                    )?
            {
                return Err(Error::Authority);
            }
            let mut aad = CONTROL_AAD.to_vec();
            aad.extend(sequence.to_le_bytes());
            let plain = cipher.open_dict(&aad, &envelope.sealed).map_err(io)?;
            let next: State = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
            if receipt_details(&next) != envelope.details {
                return Err(Error::Authority);
            }
            if next.audit != envelope.event {
                return Err(Error::Authority);
            }
            if next.identity != identity {
                return Err(Error::Authority);
            }
            if let Some(previous) = &state {
                validate_transition(previous, &next)?;
            } else if next.generation != 0 || next.status != Status::Active {
                return Err(Error::Authority);
            }
            if let Some(evidence) = &next.prune {
                prunes.insert(
                    evidence.tag,
                    PruneProof {
                        evidence: evidence.clone(),
                        complete: next.pruned,
                        checkpoint: envelope.checkpoint.clone(),
                        receipt: public_receipt(&envelope),
                    },
                );
            }
            state = Some(next);
            checkpoint = Some(envelope.checkpoint);
        }
        if store.is_empty() {
            if witness.current()?.is_some() {
                return Err(Error::Authority);
            }
            let state = State {
                origin: oojuro_store::crypto::random_key(),
                identity,
                generation: 0,
                physical: [0; 32],
                logical: [0; 32],
                status: Status::Active,
                pruned: false,
                prune: None,
                live_records: 0,
                live_bytes: 0,
                audit: None,
            };
            let initial_checkpoint =
                persist(&mut store, &cipher, &signer, authority, &state, None)?;
            witness.advance(None, initial_checkpoint.floor().map_err(audit_error)?)?;
            checkpoint = Some(initial_checkpoint);
        } else {
            if !found_floor {
                return Err(Error::Authority);
            }
            let floor = retained.ok_or(Error::Authority)?;
            let current = checkpoint
                .as_ref()
                .ok_or(Error::Authority)?
                .floor()
                .map_err(audit_error)?;
            // Finish a control append whose durable witness update was interrupted.
            if floor != current {
                witness.advance(Some(floor), current)?;
            }
        }
        let checkpoint = checkpoint.ok_or(Error::Authority)?;
        // Newly created origin is inside the authenticated envelope, not reconstructed.
        let last = store.get(store.len() - 1).map_err(io)?;
        let envelope: Envelope = serde_json::from_slice(&last).map_err(|_| Error::Authority)?;
        let mut aad = CONTROL_AAD.to_vec();
        aad.extend((store.len() - 1).to_le_bytes());
        let state: State =
            serde_json::from_slice(&cipher.open_dict(&aad, &envelope.sealed).map_err(io)?)
                .map_err(|_| Error::Authority)?;
        let witness_directory = witness.directory.clone();
        Ok(Self {
            directory,
            witness_directory,
            inner: Mutex::new(Inner {
                store,
                cipher,
                signer,
                authority,
                witness,
                state,
                checkpoint,
                failed: false,
                prunes,
                #[cfg(test)]
                hook: None,
            }),
        })
    }
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, Inner>> {
        let guard = self.inner.lock().map_err(|_| Error::Unavailable)?;
        guard.current()?;
        Ok(guard)
    }
    /// Administrative logical denial fences even an already-open data owner.
    /// It never asserts that a copied data key or backup wrap was destroyed.
    pub fn deny(&self) -> Result<LifecycleReceipt> {
        let mut owner = self.lock()?;
        if owner.state.status == Status::DeniedPendingPermanent {
            return owner.receipt();
        }
        let mut state = owner.state.clone();
        state.status = Status::DeniedPendingPermanent;
        state.pruned = false;
        state.prune = None;
        state.audit = Some(owner.event(
            EventKind::LogicalDenialPendingPermanent,
            state.live_records,
            state.live_bytes,
            state.physical,
            state.physical,
        )?);
        owner.advance(state)?;
        owner.receipt()
    }
    pub fn current_floor(&self) -> Result<RetainedFloor> {
        self.lock()?.checkpoint.floor().map_err(audit_error)
    }
}
impl Inner {
    pub fn uses_key(&self, cipher: &FrameCipher) -> bool {
        self.cipher.key_id() == cipher.key_id()
    }
    pub fn event(
        &self,
        kind: EventKind,
        records: u64,
        bytes: u64,
        before: [u8; 32],
        after: [u8; 32],
    ) -> Result<AuditEvent> {
        Ok(AuditEvent {
            sequence: self.store.len().checked_add(1).ok_or(Error::Bounds)?,
            recorded_at: now()?,
            kind,
            scope: self.state.identity,
            actor: self.authority,
            records,
            bytes,
            before,
            after,
        })
    }
    pub fn receipt(&self) -> Result<LifecycleReceipt> {
        let bytes = self
            .store
            .get(self.store.len().checked_sub(1).ok_or(Error::Authority)?)
            .map_err(io)?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
        public_receipt(&envelope).ok_or(Error::Authority)
    }
    fn current(&self) -> Result<()> {
        if self.failed
            || self.witness.current()? != Some(self.checkpoint.floor().map_err(audit_error)?)
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub fn advance(&mut self, state: State) -> Result<SignedCheckpoint> {
        self.current()?;
        validate_transition(&self.state, &state)?;
        self.failed = true;
        let checkpoint = persist(
            &mut self.store,
            &self.cipher,
            &self.signer,
            self.authority,
            &state,
            Some(&self.checkpoint),
        )?;
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook();
        }
        self.witness.advance(
            Some(self.checkpoint.floor().map_err(audit_error)?),
            checkpoint.floor().map_err(audit_error)?,
        )?;
        self.state = state;
        self.checkpoint = checkpoint.clone();
        if let Some(evidence) = &self.state.prune {
            self.prunes.insert(
                evidence.tag,
                PruneProof {
                    evidence: evidence.clone(),
                    complete: self.state.pruned,
                    checkpoint: checkpoint.clone(),
                    receipt: if self.state.audit.is_some() {
                        Some(self.receipt()?)
                    } else {
                        None
                    },
                },
            );
        }
        self.failed = false;
        Ok(checkpoint)
    }
}
fn validate_transition(previous: &State, next: &State) -> Result<()> {
    if next
        .prune
        .as_ref()
        .is_some_and(|proof| proof.generation != next.generation)
        || previous.origin != next.origin
        || previous.identity != next.identity
        || next.generation < previous.generation
        || (previous.status == Status::DeniedPendingPermanent && next.status != previous.status)
        || (next.generation == previous.generation
            && (next.physical != previous.physical || next.logical != previous.logical))
    {
        return Err(Error::Authority);
    }
    Ok(())
}
fn persist(
    store: &mut Store,
    cipher: &FrameCipher,
    signer: &Signer,
    authority: [u8; 32],
    state: &State,
    previous: Option<&SignedCheckpoint>,
) -> Result<SignedCheckpoint> {
    let sequence = store.len();
    let mut aad = CONTROL_AAD.to_vec();
    aad.extend(sequence.to_le_bytes());
    let sealed = cipher.seal_dict(&aad, &json(state, 16 << 10)?);
    let previous_digest = previous
        .map(|cp| cp.digest().map_err(audit_error))
        .transpose()?
        .unwrap_or([0; 32]);
    let checkpoint = signer
        .sign(CheckpointBody {
            schema: 1,
            authority,
            domain: Domain::NativeLifecycle,
            scope: state.identity,
            sequence: sequence.checked_add(1).ok_or(Error::Bounds)?,
            events: sequence + 1,
            previous: previous
                .map(|cp| cp.digest().map_err(audit_error))
                .transpose()?
                .unwrap_or([0; 32]),
            head: receipt_head(
                *blake3::hash(&sealed).as_bytes(),
                state.audit.as_ref(),
                previous_digest,
                &receipt_details(state),
            )?,
            issued_at: now()?,
        })
        .map_err(audit_error)?;
    let envelope = json(
        &Envelope {
            sealed,
            event: state.audit.clone(),
            details: receipt_details(state),
            checkpoint: checkpoint.clone(),
        },
        32 << 10,
    )?;
    if store.append(&[carrier(&envelope)]).map_err(io)? != sequence {
        return Err(Error::Authority);
    }
    Ok(checkpoint)
}
fn audit_error(_: crate::audit::Error) -> Error {
    Error::Authority
}

pub(super) fn ids_digest(ids: &[u64]) -> Result<[u8; 32]> {
    if ids.len() > MAX_RECORDS || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(Error::Bounds);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"oojuro/managed/opaque-record-ids/v1\0");
    hasher.update(&(ids.len() as u64).to_le_bytes());
    for id in ids {
        hasher.update(&id.to_le_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}
