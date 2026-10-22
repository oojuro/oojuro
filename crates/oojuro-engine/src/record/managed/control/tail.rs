//! Immutable bounded control tail. CURRENT is the sole publication authority.
use super::*;
use std::fs::File;
const MODE: &[u8] = b"oojuro/managed/control/paged/1\n";
const MARKER: &str = "PAGED_CONTROL";
const OBJECT_BYTES: usize = 32 << 10;
/// Constructor is private: the indexed bootstrap receives a verified CURRENT witness,
/// not a caller-asserted root or a wire-selected authenticated flag.
pub(in crate::record::managed) struct Verified {
    state: State,
    checkpoint: SignedCheckpoint,
    witness_directory: PathBuf,
    public_key: [u8; 32],
}
impl Verified {
    pub(in crate::record::managed) fn state(&self) -> &State {
        &self.state
    }
    pub(in crate::record::managed) fn checkpoint(&self) -> &SignedCheckpoint {
        &self.checkpoint
    }
    pub(in crate::record::managed) fn historical_state(
        &self,
        directory: &Path,
        cipher: &FrameCipher,
        sequence: u64,
        digest: [u8; 32],
    ) -> Result<State> {
        self.assert_current()?;
        if sequence == 0 || sequence >= self.checkpoint.body.sequence {
            return Err(Error::Bounds);
        }
        let bytes = read_regular(&path(directory, &digest), OBJECT_BYTES)?;
        let old = decode(
            &bytes,
            RetainedFloor { sequence, digest },
            cipher,
            self.public_key,
            self.checkpoint.body.authority,
            self.state.identity,
            &self.witness_directory,
        )?;
        self.assert_current()?;
        Ok(old.state)
    }
    pub(in crate::record::managed) fn assert_current(&self) -> Result<()> {
        self.state.validate_layout()?;
        let bytes = read_regular(&self.witness_directory.join("CURRENT"), 4096)?;
        let actual: RetainedFloor = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
        if actual != self.checkpoint.floor().map_err(audit_error)? {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
pub(super) enum Journal {
    Snapshot(Box<Store>),
    Paged {
        directory: PathBuf,
        cipher: Arc<FrameCipher>,
        verified: Option<Box<Verified>>,
        _lock: File,
        witness_directory: PathBuf,
    },
}
fn path(directory: &Path, digest: &[u8; 32]) -> PathBuf {
    directory.join("checkpoints").join(format!(
        "cp-{}.bin",
        super::super::paged::schema::token_hex(digest)
    ))
}
fn decode(
    bytes: &[u8],
    expected: RetainedFloor,
    cipher: &FrameCipher,
    public_key: [u8; 32],
    authority: [u8; 32],
    identity: [u8; 32],
    witness_directory: &Path,
) -> Result<Verified> {
    if bytes.len() > OBJECT_BYTES {
        return Err(Error::Bounds);
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| Error::Authority)?;
    let cp = &envelope.checkpoint;
    Verifier::new(authority, Domain::NativeLifecycle, public_key)
        .verify_checkpoint(cp, identity)
        .map_err(audit_error)?;
    if cp.floor().map_err(audit_error)? != expected
        || cp.body.events != cp.body.sequence
        || (cp.body.sequence == 1 && cp.body.previous != [0; 32])
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
    aad.extend(
        expected
            .sequence
            .checked_sub(1)
            .ok_or(Error::Authority)?
            .to_le_bytes(),
    );
    let plain = cipher.open_dict(&aad, &envelope.sealed).map_err(io)?;
    let state: State = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
    state.validate_layout()?;
    if state.identity != identity
        || state.audit != envelope.event
        || state.generation.checked_add(1) != Some(expected.sequence)
        || state.prune.is_some()
        || state.paged.is_none()
        || receipt_details(&state) != envelope.details
    {
        return Err(Error::Authority);
    }
    Ok(Verified {
        state,
        checkpoint: envelope.checkpoint,
        witness_directory: witness_directory.to_path_buf(),
        public_key,
    })
}
impl Journal {
    pub fn snapshot(directory: &Path, cipher: Arc<FrameCipher>) -> Result<Self> {
        match fs::symlink_metadata(directory.join(MARKER)) {
            Ok(_) => return Err(Error::Authority),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(Error::Unavailable),
        }
        Ok(Self::Snapshot(Box::new(open_store(directory, cipher)?)))
    }
    pub fn create_paged(
        directory: &Path,
        cipher: Arc<FrameCipher>,
        witness_directory: &Path,
    ) -> Result<Self> {
        directory_create(directory)?;
        if fs::read_dir(directory).map_err(io)?.next().is_some() {
            return Err(Error::Authority);
        }
        let lock = owner_lock(directory)?;
        write_new(&directory.join(MARKER), MODE)?;
        directory_create(&directory.join("checkpoints"))?;
        sync_dir(directory)?;
        Ok(Self::Paged {
            directory: directory.to_path_buf(),
            cipher,
            verified: None,
            _lock: lock,
            witness_directory: witness_directory.to_path_buf(),
        })
    }
    pub fn open_paged(
        directory: &Path,
        cipher: Arc<FrameCipher>,
        expected: RetainedFloor,
        public_key: [u8; 32],
        authority: [u8; 32],
        identity: [u8; 32],
        witness_directory: &Path,
    ) -> Result<Self> {
        if read_regular(&directory.join(MARKER), 128)? != MODE {
            return Err(Error::Authority);
        }
        // Reads do not initialize a missing/partial authority or recovery lock.
        read_regular(&directory.join("LOCK"), 4096)?;
        let lock = owner_lock(directory)?;
        let bytes = read_regular(&path(directory, &expected.digest), OBJECT_BYTES)?;
        let verified = decode(
            &bytes,
            expected,
            &cipher,
            public_key,
            authority,
            identity,
            witness_directory,
        )?;
        // The signed root pointer must be available before any status/read/serving use.
        super::super::paged::schema::load(directory, &cipher, verified.state())?;
        Ok(Self::Paged {
            directory: directory.to_path_buf(),
            cipher,
            verified: Some(Box::new(verified)),
            _lock: lock,
            witness_directory: witness_directory.to_path_buf(),
        })
    }
    pub fn len(&self) -> u64 {
        match self {
            Self::Snapshot(store) => store.len(),
            Self::Paged { verified, .. } => {
                verified.as_ref().map_or(0, |v| v.checkpoint.body.sequence)
            }
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, ordinal: u64) -> Result<Vec<u8>> {
        match self {
            Self::Snapshot(store) => store.get(ordinal).map_err(io),
            Self::Paged {
                directory,
                cipher,
                verified,
                ..
            } => {
                let current = verified.as_ref().ok_or(Error::Authority)?;
                let sequence = ordinal.checked_add(1).ok_or(Error::Bounds)?;
                if sequence > current.checkpoint.body.sequence {
                    return Err(Error::Bounds);
                }
                let digest = if sequence == current.checkpoint.body.sequence {
                    current.checkpoint.digest().map_err(audit_error)?
                } else {
                    super::super::paged::checkpoint_digest(directory, cipher, current, sequence)?
                };
                let bytes = read_regular(&path(directory, &digest), OBJECT_BYTES)?;
                let envelope: Envelope =
                    serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
                if envelope.checkpoint.body.sequence != sequence
                    || envelope.checkpoint.digest().map_err(audit_error)? != digest
                {
                    return Err(Error::Authority);
                }
                Ok(bytes)
            }
        }
    }
    pub fn append(
        &mut self,
        bytes: &[u8],
        checkpoint: &SignedCheckpoint,
        state: &State,
        public_key: [u8; 32],
    ) -> Result<u64> {
        state.validate_layout()?;
        let ordinal = self.len();
        match self {
            Self::Snapshot(store) => {
                let n = store.append(&[carrier(bytes)]).map_err(io)?;
                if n != ordinal {
                    return Err(Error::Authority);
                }
            }
            Self::Paged {
                directory,
                verified,
                witness_directory,
                ..
            } => {
                if bytes.len() > OBJECT_BYTES
                    || checkpoint.body.sequence != ordinal.checked_add(1).ok_or(Error::Bounds)?
                    || checkpoint.body.previous
                        != verified
                            .as_ref()
                            .map(|v| v.checkpoint.digest().map_err(audit_error))
                            .transpose()?
                            .unwrap_or([0; 32])
                    || state.paged.is_none()
                    || state.prune.is_some()
                {
                    return Err(Error::Authority);
                }
                let digest = checkpoint.digest().map_err(audit_error)?;
                let object = path(directory, &digest);
                if object.exists() {
                    if read_regular(&object, OBJECT_BYTES)? != bytes {
                        return Err(Error::Conflict);
                    }
                } else {
                    write_new(&object, bytes)?;
                    sync_dir(&directory.join("checkpoints"))?;
                }
                // This staged object is not an ACK. Inner.failed stays latched until
                // independent CURRENT compare-and-swap and fsync have succeeded.
                *verified = Some(Box::new(Verified {
                    state: state.clone(),
                    checkpoint: checkpoint.clone(),
                    witness_directory: witness_directory.clone(),
                    public_key,
                }));
            }
        }
        Ok(ordinal)
    }
    pub fn check_current_root(&self) -> Result<()> {
        if let Self::Paged {
            directory,
            cipher,
            verified: Some(v),
            ..
        } = self
        {
            v.assert_current()?;
            super::super::paged::schema::load(directory, cipher, v.state())?;
        }
        Ok(())
    }
    pub fn directory(&self) -> Result<PathBuf> {
        match self {
            Self::Paged { directory, .. } => Ok(directory.clone()),
            _ => Err(Error::Authority),
        }
    }
    pub fn is_paged(&self) -> bool {
        matches!(self, Self::Paged { .. })
    }
    pub fn verified(&self) -> Result<&Verified> {
        match self {
            Self::Paged {
                verified: Some(v), ..
            } => Ok(v),
            _ => Err(Error::Authority),
        }
    }
}
