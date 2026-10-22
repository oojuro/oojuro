//! Fresh opt-in snapshot lifecycle. O(live state) mutation is deliberate in this
//! bounded protocol; it is not the paged/incremental production-scale writer.
use super::*;
use crate::capture_log::Identity;
use oojuro_store::{NewRecord, Options, Store, crypto::FrameCipher};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
mod control;
use crate::audit::{RetainedFloor, SignedCheckpoint};
use control::State;
pub use control::{FileWitness, LifecycleReceipt, ReceiptDetails, RetainedControl, Status};
const FORMAT: &[u8] = b"oojuro-managed-record-3\n";
const ENROLLMENT_AAD: &[u8] = b"oojuro/managed/data-root-enrollment/v1\0";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalCommit {
    pub origin: [u8; 32],
    pub sequence: u64,
    pub ids: Vec<u64>,
    pub recorded_at: Vec<i64>,
    /// Keyed logical lineage commitment, not a public hash of record plaintext.
    pub receipt: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendResult {
    pub commit: OriginalCommit,
    pub receipt_only: bool,
    pub removed_ids: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub format: u32,
    pub stable_origin: [u8; 32],
    pub identity: [u8; 32],
    pub generation: u64,
    pub physical_digest: [u8; 32],
    pub logical_receipt_head: [u8; 32],
    pub control: RetainedFloor,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Selection {
    Ids(Vec<u64>),
    Agent(String),
    Key(String),
    RecordedBefore(i64),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retry {
    tag: [u8; 32],
    fingerprint: [u8; 32],
    commit: OriginalCommit,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Erased {
    tag: [u8; 32],
    fingerprint: [u8; 32],
    ids: Vec<u64>,
    bytes: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    origin: [u8; 32],
    identity: Identity,
    intent: [u8; 32],
    generation: u64,
    next_id: u64,
    last_at: Option<i64>,
    logical: [u8; 32],
    rows: Vec<Assigned>,
    retries: Vec<Retry>,
    erased: Vec<Erased>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermanentErasure {
    PendingUnsupportedLocalKeyAuthority,
}
#[derive(Clone)]
pub struct PruneReport {
    pub removed_ids: Vec<u64>,
    pub selected_bytes: u64,
    pub decoded_shared_occurrences: u64,
    pub encoded_occurrences: u64,
    pub all_owned_prior_generations_removed: bool,
    /// Means current tracked bytes passed both complete scans, never backup/freed-block erasure.
    pub local_live_absence_verified: bool,
    pub permanent: PermanentErasure,
    pub checkpoint: SignedCheckpoint,
    pub signed_receipt: LifecycleReceipt,
    pub verified_generation: u64,
    pub historical: bool,
}
pub struct ManagedLog {
    directory: PathBuf,
    _lock: fs::File,
    cipher: Arc<FrameCipher>,
    control: Arc<RetainedControl>,
    snapshot: Snapshot,
    enrollment_digest: [u8; 32],
    #[cfg(test)]
    hook: Option<Arc<dyn Fn(Phase) + Send + Sync>>,
}
pub fn identity_binding(key: &[u8; 32], identity: &Identity) -> Result<[u8; 32]> {
    Ok(*blake3::keyed_hash(key, &json(&("managed-identity-v3", identity), 4096)?).as_bytes())
}
impl ManagedLog {
    /// Explicit fresh enrollment. Existing directories are never reset or re-enrolled.
    pub fn create(
        directory: &Path,
        key: &[u8; 32],
        identity: Identity,
        control: Arc<RetainedControl>,
    ) -> Result<Self> {
        match fs::symlink_metadata(directory) {
            Ok(_) => return Err(Error::Conflict),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Unavailable),
        }
        Self::open_mode(directory, key, identity, control, true)
    }
    pub fn open(
        directory: &Path,
        key: &[u8; 32],
        identity: Identity,
        control: Arc<RetainedControl>,
    ) -> Result<Self> {
        Self::open_mode(directory, key, identity, control, false)
    }
    fn open_mode(
        directory: &Path,
        key: &[u8; 32],
        identity: Identity,
        control: Arc<RetainedControl>,
        fresh: bool,
    ) -> Result<Self> {
        if !name(&identity.writer, 256)
            || identity.agent == 0
            || identity.first_id == 0
            || !name(&identity.adapter, 128)
        {
            return Err(Error::Invalid);
        }
        if !fresh {
            let meta = fs::symlink_metadata(directory).map_err(io)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(Error::Authority);
            }
        }
        directory_create(directory)?;
        let directory = directory.canonicalize().map_err(io)?;
        if overlaps(&directory, &control.directory)
            || overlaps(&directory, &control.witness_directory)
        {
            return Err(Error::Invalid);
        }
        let lock = owner_lock(&directory)?;
        let cipher = Arc::new(FrameCipher::new(key));
        let mut owner = control.lock()?;
        if owner.uses_key(&cipher) {
            return Err(Error::Scope);
        }
        if owner.state.identity != identity_binding(key, &identity)?
            || owner.state.status != Status::Active
        {
            return Err(Error::Scope);
        }
        let marker = directory.join("MANAGED_FORMAT");
        let enrollment_path = directory.join("ENROLLMENT");
        let expected_enrollment = owner.enrollment();
        let enrollment_bytes = if fresh {
            if owner.state.generation != 0 {
                return Err(Error::Authority);
            }
            if fs::read_dir(&directory)
                .map_err(io)?
                .any(|item| item.map(|item| item.file_name() != "LOCK").unwrap_or(true))
            {
                return Err(Error::Authority);
            }
            write_new(&marker, FORMAT)?;
            let bytes = cipher.seal_dict(ENROLLMENT_AAD, &json(&expected_enrollment, 4096)?);
            write_new(&enrollment_path, &bytes)?;
            sync_dir(&directory)?;
            sync_dir(directory.parent().ok_or(Error::Invalid)?)?;
            bytes
        } else {
            if read_regular(&marker, 128)? != FORMAT {
                return Err(Error::Authority);
            }
            let bytes = read_regular(&enrollment_path, 8192)?;
            let decoded = cipher.open_dict(ENROLLMENT_AAD, &bytes).map_err(io)?;
            let enrolled: control::Enrollment =
                serde_json::from_slice(&decoded).map_err(|_| Error::Authority)?;
            if enrolled != expected_enrollment {
                return Err(Error::Authority);
            }
            bytes
        };
        let enrollment_digest = *blake3::hash(&enrollment_bytes).as_bytes();
        let snapshot = if owner.state.generation == 0 {
            Snapshot {
                schema: 3,
                origin: owner.state.origin,
                identity: identity.clone(),
                intent: oojuro_store::crypto::random_key(),
                generation: 0,
                next_id: identity.first_id,
                last_at: None,
                logical: [0; 32],
                rows: vec![],
                retries: vec![],
                erased: vec![],
            }
        } else {
            read_snapshot(&directory, &owner.state, cipher.clone())?
        };
        // The first ID is supplied by the sealed identity, never reconstructed from row count.
        let mut log = Self {
            directory,
            _lock: lock,
            cipher,
            control: control.clone(),
            snapshot,
            enrollment_digest,
            #[cfg(test)]
            hook: None,
        };
        if log.snapshot.generation == 0 {
            log.snapshot.next_id = log.snapshot.identity.first_id;
            let mut initial = log.snapshot.clone();
            log.publish(&mut owner, &mut initial)?;
        } else if log.snapshot.identity != identity {
            return Err(Error::Scope);
        }
        drop(owner);
        Ok(log)
    }
    #[cfg(test)]
    fn phase(&self, phase: Phase) {
        if let Some(hook) = &self.hook {
            hook(phase);
        }
    }
    fn check_enrollment(&self) -> Result<()> {
        if *blake3::hash(&read_regular(&self.directory.join("ENROLLMENT"), 8192)?).as_bytes()
            != self.enrollment_digest
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    fn current(&self, state: &State) -> Result<()> {
        self.check_enrollment()?;
        if state.status != Status::Active
            || state.origin != self.snapshot.origin
            || state.generation != self.snapshot.generation
            || state.logical != self.snapshot.logical
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub fn binding(&self) -> Result<Binding> {
        let owner = self.control.lock()?;
        self.current(&owner.state)?;
        Ok(binding(
            &owner.state,
            owner.checkpoint.floor().map_err(|_| Error::Authority)?,
        ))
    }
    pub fn derived_directory(&self) -> Result<(Binding, PathBuf)> {
        let owner = self.control.lock()?;
        self.current(&owner.state)?;
        let path = self
            .directory
            .join(format!("d-{}", self.snapshot.generation));
        directory_create(&path)?;
        Ok((
            binding(
                &owner.state,
                owner.checkpoint.floor().map_err(|_| Error::Authority)?,
            ),
            path,
        ))
    }
    pub fn validate_binding(&self, expected: &Binding) -> Result<()> {
        if &self.binding()? != expected {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub fn get(&self, id: u64, max_bytes: usize) -> Result<Option<Record>> {
        let owner = self.control.lock()?;
        self.current(&owner.state)?;
        let row = self.snapshot.rows.iter().find(|r| r.id == id).map(|r| {
            r.record(
                &self.snapshot.identity.writer,
                &self.snapshot.identity.adapter,
            )
        });
        if let Some(row) = &row {
            json(row, max_bytes.min(MAX_OUTPUT_BYTES))?;
        }
        Ok(row)
    }
    pub fn append(&mut self, request: &str, inputs: &[Input]) -> Result<AppendResult> {
        if !name(request, 1024) || inputs.is_empty() || inputs.len() > 1000 {
            return Err(Error::Bounds);
        }
        for input in inputs {
            input.validate()?;
        }
        let tag = self.tag(&("append-request-v3", request))?;
        let fingerprint = self.tag(&("typed-input-v3", inputs))?;
        let control = self.control.clone();
        let mut owner = control.lock()?;
        self.current(&owner.state)?;
        if owner.state.prune.is_some() && !owner.state.pruned {
            return Err(Error::Unavailable);
        }
        if let Some(retry) = self.snapshot.retries.iter().find(|r| r.tag == tag) {
            if retry.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            let removed_ids: Vec<_> = retry
                .commit
                .ids
                .iter()
                .copied()
                .filter(|id| !self.snapshot.rows.iter().any(|r| r.id == *id))
                .collect();
            return Ok(AppendResult {
                commit: retry.commit.clone(),
                receipt_only: !removed_ids.is_empty(),
                removed_ids,
            });
        }
        let mut next = self.snapshot.clone();
        if next
            .next_id
            .checked_sub(next.identity.first_id)
            .and_then(|n| n.checked_add(inputs.len() as u64))
            .is_none_or(|n| n > MAX_RECORDS as u64)
        {
            return Err(Error::Bounds);
        }
        if next
            .rows
            .len()
            .checked_add(inputs.len())
            .is_none_or(|n| n > MAX_RECORDS)
        {
            return Err(Error::Bounds);
        }
        let clock = now()?;
        let mut ids = vec![];
        let mut recorded_at = vec![];
        for input in inputs {
            let id = next.next_id;
            next.next_id = id.checked_add(1).ok_or(Error::Bounds)?;
            let at = next
                .last_at
                .map(|last| {
                    last.checked_add(1)
                        .map(|v| v.max(clock))
                        .ok_or(Error::Bounds)
                })
                .transpose()?
                .unwrap_or(clock);
            next.last_at = Some(at);
            ids.push(id);
            recorded_at.push(at);
            next.rows.push(Assigned {
                id,
                recorded_at: at,
                input: input.clone(),
            });
        }
        let sequence = (next.retries.len() as u64)
            .checked_add(1)
            .ok_or(Error::Bounds)?;
        let receipt = self.tag(&(
            "logical-append-v3",
            next.origin,
            next.logical,
            sequence,
            tag,
            fingerprint,
            &ids,
            &recorded_at,
        ))?;
        let commit = OriginalCommit {
            origin: next.origin,
            sequence,
            ids,
            recorded_at,
            receipt,
        };
        next.logical = receipt;
        next.retries.push(Retry {
            tag,
            fingerprint,
            commit: commit.clone(),
        });
        self.publish(&mut owner, &mut next)?;
        cleanup(&self.directory, Some(next.generation))?;
        Ok(AppendResult {
            commit,
            receipt_only: false,
            removed_ids: vec![],
        })
    }
    /// Exact operation retries never resurrect content. No selection/raw bytes are retained in receipts.
    pub fn prune(&mut self, operation: &str, selection: &Selection) -> Result<PruneReport> {
        if !name(operation, 1024) {
            return Err(Error::Bounds);
        }
        validate_selection(selection)?;
        let tag = self.tag(&("prune-request-v3", operation))?;
        let fingerprint = self.tag(&("prune-selection-v3", selection))?;
        let control = self.control.clone();
        let mut owner = control.lock()?;
        self.current(&owner.state)?;
        if owner
            .state
            .prune
            .as_ref()
            .is_some_and(|pending| !owner.state.pruned && pending.tag != tag)
        {
            return Err(Error::Unavailable);
        }
        let receipt = if let Some(prior) = self.snapshot.erased.iter().find(|r| r.tag == tag) {
            if prior.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            prior.clone()
        } else {
            let mut next = self.snapshot.clone();
            let id_set = match selection {
                Selection::Ids(ids) => Some(
                    ids.iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                ),
                _ => None,
            };
            let removed: Vec<_> = next
                .rows
                .iter()
                .filter(|r| selected(r, selection, id_set.as_ref()))
                .cloned()
                .collect();
            let needles = needles(&removed)?;
            next.rows
                .retain(|r| !selected(r, selection, id_set.as_ref()));
            let erased = Erased {
                tag,
                fingerprint,
                ids: removed.iter().map(|r| r.id).collect(),
                bytes: record_bytes(&removed)?,
            };
            next.erased.push(erased);
            let (mut state, staged) = stage(
                &self.directory,
                &mut next,
                self.cipher.clone(),
                owner.state.identity,
            )?;
            // Scan the exact fully staged snapshot, including its final generation and receipt metadata.
            let decoded_matches = decoded_occurrences(&next, &needles)?;
            // Scan the EXACT ciphertext generation published below, not a re-encrypted candidate.
            let encoded_matches = scan_files(&self.directory, &staged, &needles)?;
            state.prune = Some(control::PruneEvidence {
                tag,
                generation: next.generation,
                decoded_matches,
                encoded_matches,
                before: owner.state.physical,
                record_ids: control::ids_digest(&next.erased.last().ok_or(Error::Authority)?.ids)?,
                recorded_before: match selection {
                    Selection::RecordedBefore(at) => Some(*at),
                    _ => None,
                },
            });
            #[cfg(test)]
            self.phase(Phase::AfterStage);
            owner.advance(state)?;
            #[cfg(test)]
            self.phase(Phase::AfterControl);
            self.snapshot = next;
            self.snapshot.erased.last().ok_or(Error::Authority)?.clone()
        };
        let proof = owner.prunes.get(&tag).ok_or(Error::Authority)?.clone();
        let historical = proof.evidence.generation != self.snapshot.generation;
        let (checkpoint, signed_receipt, complete) = if historical {
            (
                proof.checkpoint,
                proof.receipt.ok_or(Error::Unavailable)?,
                proof.complete,
            )
        } else {
            cleanup(&self.directory, Some(self.snapshot.generation))?;
            #[cfg(test)]
            self.phase(Phase::AfterCleanup);
            let mut state = owner.state.clone();
            self.check_enrollment()?;
            if physical(
                &self
                    .directory
                    .join(format!("g-{}", self.snapshot.generation)),
            )? != owner.state.physical
            {
                return Err(Error::Authority);
            }
            state.pruned = true;
            let kind = if matches!(selection, Selection::RecordedBefore(_)) {
                crate::audit::EventKind::RetentionDropVerified
            } else {
                crate::audit::EventKind::LiveRewriteVerified
            };
            state.audit = Some(owner.event(
                kind,
                receipt.ids.len() as u64,
                receipt.bytes,
                proof.evidence.before,
                owner.state.physical,
            )?);
            let checkpoint = owner.advance(state)?;
            (checkpoint, owner.receipt()?, true)
        };
        #[cfg(test)]
        self.phase(Phase::BeforeAck);
        Ok(PruneReport {
            removed_ids: receipt.ids.clone(),
            selected_bytes: receipt.bytes,
            decoded_shared_occurrences: proof.evidence.decoded_matches,
            encoded_occurrences: proof.evidence.encoded_matches,
            all_owned_prior_generations_removed: complete,
            local_live_absence_verified: !historical
                && complete
                && proof.evidence.decoded_matches == 0
                && proof.evidence.encoded_matches == 0,
            permanent: PermanentErasure::PendingUnsupportedLocalKeyAuthority,
            checkpoint,
            signed_receipt,
            verified_generation: proof.evidence.generation,
            historical,
        })
    }
    /// Immediate logical denial is committed before cleanup. Local caller keys still decrypt backups.
    pub fn deny_writer(&mut self) -> Result<LifecycleReceipt> {
        self.check_enrollment()?;
        let control = self.control.clone();
        let mut owner = control.lock()?;
        if owner.state.origin != self.snapshot.origin {
            return Err(Error::Scope);
        }
        if owner.state.status == Status::Active {
            self.current(&owner.state)?;
            let mut state = owner.state.clone();
            state.status = Status::DeniedPendingPermanent;
            state.pruned = false;
            state.prune = None;
            state.audit = Some(owner.event(
                crate::audit::EventKind::LogicalDenialPendingPermanent,
                state.live_records,
                state.live_bytes,
                state.physical,
                state.physical,
            )?);
            owner.advance(state)?;
        }
        cleanup(&self.directory, None)?;
        let mut state = owner.state.clone();
        state.pruned = true;
        state.audit = Some(owner.event(
            crate::audit::EventKind::LiveRewriteVerified,
            state.live_records,
            state.live_bytes,
            state.physical,
            [0; 32],
        )?);
        owner.advance(state)?;
        owner.receipt()
    }
    pub fn finalize_permanent_destruction(&self) -> Result<()> {
        Err(Error::Authority)
    }
    fn tag<T: Serialize + ?Sized>(&self, value: &T) -> Result<[u8; 32]> {
        Ok(*blake3::keyed_hash(&self.snapshot.intent, &json(value, MAX_BATCH_BYTES)?).as_bytes())
    }
    fn publish(&mut self, owner: &mut control::Inner, next: &mut Snapshot) -> Result<()> {
        let (state, _) = stage(
            &self.directory,
            next,
            self.cipher.clone(),
            owner.state.identity,
        )?;
        owner.advance(state)?;
        self.snapshot = next.clone();
        Ok(())
    }
}
fn binding(state: &State, control: RetainedFloor) -> Binding {
    Binding {
        format: 3,
        stable_origin: state.origin,
        identity: state.identity,
        generation: state.generation,
        physical_digest: state.physical,
        logical_receipt_head: state.logical,
        control,
    }
}
fn io<T>(_: T) -> Error {
    Error::Unavailable
}
fn now() -> Result<i64> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io)?
            .as_micros(),
    )
    .map_err(io)
}
fn carrier(bytes: &[u8]) -> NewRecord<'_> {
    NewRecord {
        kind: 0,
        valid_at: 0,
        recorded_at: 0,
        key: &[],
        attrs: &[],
        text: bytes,
    }
}
fn open_store(path: &Path, cipher: Arc<FrameCipher>) -> Result<Store> {
    Store::open(
        path,
        Options {
            cipher: Some(cipher),
            require_sealed: true,
            sync: true,
            compact_threads: 1,
            ..Options::default()
        },
    )
    .map_err(io)
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn directory_create(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(Error::Invalid),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(path).map_err(io)
        }
        Err(_) => Err(Error::Unavailable),
    }
}
fn owner_lock(directory: &Path) -> Result<fs::File> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("LOCK"))
        .map_err(io)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Unavailable);
    }
    Ok(file)
}
fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    let meta = file.metadata().map_err(io)?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return Err(Error::Bounds);
    }
    let mut bytes = vec![];
    std::io::Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > limit {
        return Err(Error::Bounds);
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(io)
}
fn remove_regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).map_err(io)?.is_file()
        || fs::symlink_metadata(path)
            .map_err(io)?
            .file_type()
            .is_symlink()
    {
        return Err(Error::Authority);
    }
    fs::remove_file(path).map_err(io)
}
fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path).and_then(|f| f.sync_all()).map_err(io)
}

fn generation(path: &Path) -> Result<u64> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(Error::Authority)?;
    name.strip_prefix("g-")
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(Error::Authority)?
        .parse()
        .map_err(|_| Error::Authority)
}
fn root_entries(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = vec![];
    for item in fs::read_dir(directory).map_err(io)? {
        let item = item.map_err(io)?;
        let path = item.path();
        let name = item.file_name();
        let name = name.to_str().ok_or(Error::Authority)?;
        let meta = fs::symlink_metadata(&path).map_err(io)?;
        if meta.file_type().is_symlink() {
            return Err(Error::Authority);
        }
        if name == "LOCK" || name == "MANAGED_FORMAT" || name == "ENROLLMENT" {
            if !meta.is_file()
                || (name == "LOCK" && meta.len() != 0)
                || (name == "MANAGED_FORMAT" && read_regular(&path, 128)? != FORMAT)
            {
                return Err(Error::Authority);
            }
        } else {
            let suffix = name
                .strip_prefix("g-")
                .or_else(|| name.strip_prefix("d-"))
                .ok_or(Error::Authority)?;
            if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) || !meta.is_dir() {
                return Err(Error::Authority);
            }
            suffix.parse::<u64>().map_err(|_| Error::Authority)?;
        }
        entries.push(path);
    }
    entries.sort();
    Ok(entries)
}
fn store_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = vec![];
    for item in fs::read_dir(directory).map_err(io)? {
        let item = item.map_err(io)?;
        let name = item.file_name();
        let name = name.to_str().ok_or(Error::Authority)?;
        let regular = ["LOCK", "MANIFEST", "ENCRYPTED"].contains(&name)
            || [("hot-", ".ojl"), ("cold-", ".ojc"), ("dict-", ".ojd")]
                .iter()
                .any(|(prefix, suffix)| {
                    name.strip_prefix(prefix)
                        .and_then(|s| s.strip_suffix(suffix))
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                });
        let meta = fs::symlink_metadata(item.path()).map_err(io)?;
        if !regular || !meta.is_file() || meta.file_type().is_symlink() {
            return Err(Error::Authority);
        }
        files.push(item.path());
    }
    files.sort();
    Ok(files)
}
fn physical(directory: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"oojuro/managed/live-files/v3\0");
    let mut total = 0usize;
    for file in store_files(directory)? {
        let name = file
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(Error::Authority)?
            .as_bytes();
        let bytes = read_regular(&file, MAX_STATE_BYTES * 2)?;
        total = total.checked_add(bytes.len()).ok_or(Error::Bounds)?;
        if total > MAX_STATE_BYTES * 2 + (1 << 20) {
            return Err(Error::Bounds);
        }
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(*hasher.finalize().as_bytes())
}
fn stage(
    directory: &Path,
    snapshot: &mut Snapshot,
    cipher: Arc<FrameCipher>,
    identity: [u8; 32],
) -> Result<(State, PathBuf)> {
    let mut largest = snapshot.generation;
    for path in root_entries(directory)? {
        if let Some(name) = path.file_name().and_then(|s| s.to_str())
            && let Some(number) = name.strip_prefix("g-").or_else(|| name.strip_prefix("d-"))
        {
            largest = largest.max(number.parse::<u64>().map_err(|_| Error::Authority)?);
        }
    }
    snapshot.generation = largest.checked_add(1).ok_or(Error::Bounds)?;
    validate_snapshot(snapshot)?;
    let bytes = json(snapshot, MAX_STATE_BYTES)?;
    let path = directory.join(format!("g-{}", snapshot.generation));
    directory_create(&path)?;
    let mut store = open_store(&path, cipher.clone())?;
    for chunk in bytes.chunks(16 << 20) {
        store.append(&[carrier(chunk)]).map_err(io)?;
    }
    drop(store);
    sync_dir(&path)?;
    sync_dir(directory)?;
    let state = State {
        origin: snapshot.origin,
        identity,
        generation: snapshot.generation,
        physical: physical(&path)?,
        logical: snapshot.logical,
        status: Status::Active,
        pruned: false,
        prune: None,
        live_records: snapshot.rows.len() as u64,
        live_bytes: record_bytes(&snapshot.rows)?,
        audit: None,
    };
    let checked = read_snapshot(directory, &state, cipher)?;
    if json(&checked, MAX_STATE_BYTES)? != bytes {
        return Err(Error::Authority);
    }
    Ok((state, path))
}
fn read_snapshot(directory: &Path, state: &State, cipher: Arc<FrameCipher>) -> Result<Snapshot> {
    let path = directory.join(format!("g-{}", state.generation));
    if physical(&path)? != state.physical {
        return Err(Error::Authority);
    }
    let store = open_store(&path, cipher)?;
    let mut bytes = vec![];
    for sequence in 0..store.len() {
        let chunk = store.get(sequence).map_err(io)?;
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|n| n > MAX_STATE_BYTES)
        {
            return Err(Error::Bounds);
        }
        bytes.extend(chunk);
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
    if snapshot.schema != 3
        || snapshot.origin != state.origin
        || snapshot.generation != state.generation
        || snapshot.logical != state.logical
    {
        return Err(Error::Authority);
    }
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}
fn validate_snapshot(s: &Snapshot) -> Result<()> {
    if s.schema != 3
        || s.rows.len() > MAX_RECORDS
        || s.retries.len() > MAX_RECORDS
        || s.erased.len() > MAX_RECORDS
    {
        return Err(Error::Bounds);
    }
    let mut committed = BTreeMap::new();
    let mut logical = [0; 32];
    let mut next_id = s.identity.first_id;
    let mut last_at = None;
    let mut tags = std::collections::BTreeSet::new();
    for (index, retry) in s.retries.iter().enumerate() {
        let c = &retry.commit;
        if !tags.insert(retry.tag)
            || c.origin != s.origin
            || c.sequence != index as u64 + 1
            || c.ids.is_empty()
            || c.ids.len() > 1000
            || c.ids.len() != c.recorded_at.len()
        {
            return Err(Error::Authority);
        }
        let expected = *blake3::keyed_hash(
            &s.intent,
            &json(
                &(
                    "logical-append-v3",
                    s.origin,
                    logical,
                    c.sequence,
                    retry.tag,
                    retry.fingerprint,
                    &c.ids,
                    &c.recorded_at,
                ),
                MAX_BATCH_BYTES,
            )?,
        )
        .as_bytes();
        if expected != c.receipt {
            return Err(Error::Authority);
        }
        logical = expected;
        for (&id, &at) in c.ids.iter().zip(&c.recorded_at) {
            if id != next_id || last_at.is_some_and(|last| at <= last) {
                return Err(Error::Authority);
            }
            next_id = next_id.checked_add(1).ok_or(Error::Bounds)?;
            if committed.len() >= MAX_RECORDS {
                return Err(Error::Bounds);
            }
            committed.insert(id, at);
            last_at = Some(at);
        }
    }
    if logical != s.logical || next_id != s.next_id || last_at != s.last_at {
        return Err(Error::Authority);
    }
    let mut live = std::collections::BTreeSet::new();
    let mut previous = None;
    for row in &s.rows {
        row.input.validate()?;
        if !live.insert(row.id)
            || previous.is_some_and(|last| row.id <= last)
            || committed.get(&row.id) != Some(&row.recorded_at)
        {
            return Err(Error::Authority);
        }
        previous = Some(row.id);
    }
    let mut removed = std::collections::BTreeSet::new();
    let mut prune_tags = std::collections::BTreeSet::new();
    for erased in &s.erased {
        if !prune_tags.insert(erased.tag) {
            return Err(Error::Authority);
        }
        for id in &erased.ids {
            if !committed.contains_key(id) || live.contains(id) || !removed.insert(*id) {
                return Err(Error::Authority);
            }
        }
    }
    if live.len().checked_add(removed.len()) != Some(committed.len()) {
        return Err(Error::Authority);
    }
    Ok(())
}
fn validate_selection(selection: &Selection) -> Result<()> {
    match selection {
        Selection::Ids(ids) if ids.is_empty() || ids.len() > MAX_RECORDS => Err(Error::Bounds),
        Selection::Agent(name) | Selection::Key(name) if !super::name(name, 1024) => {
            Err(Error::Bounds)
        }
        _ => Ok(()),
    }
}
fn selected(
    row: &Assigned,
    selection: &Selection,
    ids: Option<&std::collections::BTreeSet<u64>>,
) -> bool {
    match selection {
        Selection::Ids(_) => ids.is_some_and(|ids| ids.contains(&row.id)),
        Selection::Agent(agent) => &row.input.agent_id == agent,
        Selection::Key(key) => row.input.key.as_ref() == Some(key),
        Selection::RecordedBefore(at) => row.recorded_at < *at,
    }
}
fn row_strings(row: &Assigned) -> std::collections::BTreeSet<Vec<u8>> {
    let mut strings = std::collections::BTreeSet::new();
    for value in [&row.input.agent_id, &row.input.text]
        .into_iter()
        .chain(row.input.parent_id.iter())
        .chain(row.input.tool.iter())
        .chain(row.input.key.iter())
    {
        if !value.is_empty() {
            strings.insert(value.as_bytes().to_vec());
        }
    }
    for (key, value) in &row.input.attrs {
        if !key.is_empty() {
            strings.insert(key.as_bytes().to_vec());
        }
        collect_values(value, &mut strings);
    }
    strings.remove(&Vec::new());
    strings
}
fn needles(rows: &[Assigned]) -> Result<Option<aho_corasick::AhoCorasick>> {
    let mut needles = std::collections::BTreeSet::new();
    for row in rows {
        needles.extend(row_strings(row));
    }
    if needles.is_empty() {
        Ok(None)
    } else {
        aho_corasick::AhoCorasick::new(needles)
            .map(Some)
            .map_err(io)
    }
}
fn collect_values(value: &serde_json::Value, strings: &mut std::collections::BTreeSet<Vec<u8>>) {
    if !value.is_string() {
        strings.insert(value.to_string().into_bytes());
    }
    match value {
        serde_json::Value::String(s) => {
            strings.insert(s.as_bytes().to_vec());
        }
        serde_json::Value::Array(items) => {
            for value in items {
                collect_values(value, strings);
            }
        }
        serde_json::Value::Object(items) => {
            for (key, value) in items {
                strings.insert(key.as_bytes().to_vec());
                collect_values(value, strings);
            }
        }
        _ => {}
    }
}
fn occurrences(bytes: &[u8], needles: &Option<aho_corasick::AhoCorasick>) -> u64 {
    needles
        .as_ref()
        .map(|matcher| matcher.find_overlapping_iter(bytes).count() as u64)
        .unwrap_or(0)
}
fn decoded_occurrences(
    snapshot: &Snapshot,
    needles: &Option<aho_corasick::AhoCorasick>,
) -> Result<u64> {
    let value = serde_json::to_value(snapshot).map_err(io)?;
    let mut stack = vec![&value];
    let mut count = 0u64;
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::String(value) => {
                count = count
                    .checked_add(occurrences(value.as_bytes(), needles))
                    .ok_or(Error::Bounds)?;
            }
            serde_json::Value::Array(values) => {
                if values.is_empty() {
                    count = count
                        .checked_add(occurrences(b"[]", needles))
                        .ok_or(Error::Bounds)?;
                }
                stack.extend(values);
            }
            serde_json::Value::Object(values) => {
                if values.is_empty() {
                    count = count
                        .checked_add(occurrences(b"{}", needles))
                        .ok_or(Error::Bounds)?;
                }
                for (key, value) in values {
                    count = count
                        .checked_add(occurrences(key.as_bytes(), needles))
                        .ok_or(Error::Bounds)?;
                    stack.push(value);
                }
            }
            scalar => {
                count = count
                    .checked_add(occurrences(scalar.to_string().as_bytes(), needles))
                    .ok_or(Error::Bounds)?;
            }
        }
    }
    Ok(count)
}
fn scan_files(
    root: &Path,
    directory: &Path,
    needles: &Option<aho_corasick::AhoCorasick>,
) -> Result<u64> {
    let mut count = 0u64;
    let mut files = store_files(directory)?;
    files.push(root.join("LOCK"));
    files.push(root.join("MANAGED_FORMAT"));
    files.push(root.join("ENROLLMENT"));
    for file in files {
        count = count
            .checked_add(occurrences(
                &read_regular(&file, MAX_STATE_BYTES * 2)?,
                needles,
            ))
            .ok_or(Error::Bounds)?;
    }
    Ok(count)
}
fn remove_generation(path: &Path) -> Result<()> {
    for file in store_files(path)? {
        remove_regular(&file)?;
    }
    sync_dir(path)?;
    fs::remove_dir(path).map_err(io)?;
    sync_dir(path.parent().ok_or(Error::Invalid)?)
}
fn cleanup(directory: &Path, keep: Option<u64>) -> Result<()> {
    for path in root_entries(directory)? {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(Error::Authority)?;
        if name.starts_with("g-") {
            if Some(generation(&path)?) != keep {
                remove_generation(&path)?;
            }
        } else if let Some(suffix) = name.strip_prefix("d-") {
            let number: u64 = suffix.parse().map_err(|_| Error::Authority)?;
            if Some(number) == keep {
                continue;
            }
            for entry in fs::read_dir(&path).map_err(io)? {
                let entry = entry.map_err(io)?;
                let filename = entry.file_name();
                let filename = filename.to_str().ok_or(Error::Authority)?;
                let temp = filename
                    .strip_prefix("tmp-")
                    .and_then(|s| s.strip_suffix(".bin"))
                    .is_some_and(|s| {
                        s.len() == 64
                            && s.bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    });
                if filename != "LOCK" && filename != "RANK_TABLE" && !temp {
                    return Err(Error::Authority);
                }
                remove_regular(&entry.path())?;
            }
            sync_dir(&path)?;
            fs::remove_dir(&path).map_err(io)?;
        }
    }
    sync_dir(directory)
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    AfterStage,
    AfterControl,
    AfterCleanup,
    BeforeAck,
}
#[cfg(test)]
mod tests;

fn record_bytes(rows: &[Assigned]) -> Result<u64> {
    rows.iter().try_fold(0u64, |total, row| {
        total
            .checked_add(json(row, MAX_BATCH_BYTES)?.len() as u64)
            .ok_or(Error::Bounds)
    })
}
