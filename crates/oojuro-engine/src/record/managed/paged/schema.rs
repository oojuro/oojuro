//! Small signed anchor and one bounded immutable encrypted root bundle.
use super::*;
const ROOT_AAD: &[u8] = b"oojuro/managed/paged/control-root/v1\0";
pub(in crate::record::managed) const ROOT_BYTES: usize = 64 << 10;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record::managed) struct Anchor {
    pub format: u32,
    pub birth: Vec<u8>,
    pub generation: u64,
    pub key_version: [u8; 32],
    pub token: [u8; 32],
    pub ciphertext_digest: [u8; 32],
    pub bytes: u32,
    pub details: control::ReceiptDetails,
}
impl Anchor {
    pub fn validate(&self, origin: [u8; 32], identity: [u8; 32], generation: u64) -> Result<Birth> {
        let birth = Birth::decode(&self.birth).map_err(io)?;
        if self.format != 1
            || self.generation != generation
            || birth.generation != generation
            || birth.stable_origin != origin
            || birth.identity != identity
            || birth.key_version != self.key_version
            || self.token == [0; 32]
            || self.ciphertext_digest == [0; 32]
            || self.bytes as usize <= oojuro_store::crypto::SEAL_OVERHEAD
            || self.bytes as usize > ROOT_BYTES
        {
            return Err(Error::Authority);
        }
        Ok(birth)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record::managed) struct ControlRoots {
    pub checkpoint: tree::Root,
    pub prefix: tree::Root,
    pub request: tree::Root,
    pub erased: tree::Root,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record::managed) struct DataRoots {
    pub record: tree::Root,
    pub key_version: tree::Root,
    pub segment: tree::Root,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::record::managed) enum PrunePhase {
    DeniedPendingRewrite,
    RewrittenPendingCleanup,
    Completed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record::managed) struct Prune {
    pub tag: [u8; 32],
    pub fingerprint: [u8; 32],
    pub phase: PrunePhase,
    pub selected_count: u64,
    pub selected_bytes: u64,
    pub id_commitment: [u8; 32],
    pub recorded_before: Option<i64>,
    pub decoded_occurrences: u64,
    pub encoded_occurrences: u64,
    pub operation_sequence: u64,
    pub verified_generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record::managed) struct CommitRoot {
    pub format: u32,
    /// Secret retry/receipt commitment key; never exported with public metadata.
    pub intent: [u8; 32],
    /// CONTROL birth, not DATA-key material and not a final checkpoint hash.
    pub birth: Vec<u8>,
    pub data_key_version: Option<[u8; 32]>,
    pub next_id: u64,
    pub last_at: Option<i64>,
    pub append_sequence: u64,
    pub next_file: u64,
    pub next_physical: u64,
    pub logical: [u8; 32],
    pub original_records: u64,
    pub original_bytes: u64,
    pub live_records: u64,
    pub live_bytes: u64,
    pub physical: Option<physical::Roots>,
    pub data: Option<DataRoots>,
    pub control: ControlRoots,
    pub prune: Option<Prune>,
}
#[derive(Clone, Copy)]
pub(in crate::record::managed) enum RootName {
    Checkpoint,
    Prefix,
    Request,
    Erased,
    Record,
    KeyVersion,
    Segment,
    Frame,
    File,
    Dictionary,
}
impl RootName {
    pub fn name(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::Prefix => "prefix",
            Self::Request => "request",
            Self::Erased => "erased",
            Self::Record => "record",
            Self::KeyVersion => "key-version",
            Self::Segment => "segment",
            Self::Frame => "frame",
            Self::File => "file",
            Self::Dictionary => "dictionary",
        }
    }
    pub fn purpose(self) -> tree::Purpose {
        match self {
            Self::Checkpoint | Self::Prefix => tree::Purpose::Prefix,
            Self::Request | Self::Erased => tree::Purpose::Request,
            Self::Record => tree::Purpose::Record,
            Self::KeyVersion => tree::Purpose::KeyVersion,
            Self::Segment => tree::Purpose::Segment,
            Self::Frame => tree::Purpose::Frame,
            Self::File => tree::Purpose::File,
            Self::Dictionary => tree::Purpose::Dictionary,
        }
    }
    pub fn class(self) -> tree::KeyClass {
        match self {
            Self::Checkpoint | Self::Prefix | Self::Request | Self::Erased => {
                tree::KeyClass::RetainedControl
            }
            _ => tree::KeyClass::Data,
        }
    }
}
/// Fixed root namespace separation, containing only opaque identity plus static names.
pub(in crate::record::managed) fn root_identity(identity: [u8; 32], name: RootName) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"oojuro/managed/paged/tree-scope/v1\0");
    h.update(&identity);
    h.update(name.name().as_bytes());
    *h.finalize().as_bytes()
}
pub(in crate::record::managed) fn token_hex(token: &[u8; 32]) -> String {
    token.iter().map(|v| format!("{v:02x}")).collect()
}
fn bundle_path(control: &Path, token: &[u8; 32]) -> PathBuf {
    control
        .join("paged-roots")
        .join(format!("root-{}.bin", token_hex(token)))
}
fn aad(anchor: &Anchor) -> Vec<u8> {
    let mut bytes = ROOT_AAD.to_vec();
    bytes.extend(&anchor.birth);
    bytes.extend(anchor.token);
    bytes
}
impl CommitRoot {
    pub fn receipt_details(&self) -> control::ReceiptDetails {
        self.prune
            .as_ref()
            .map(|p| control::ReceiptDetails {
                record_ids: Some(p.id_commitment),
                recorded_before: p.recorded_before,
                decoded_occurrences: p.decoded_occurrences,
                encoded_occurrences: p.encoded_occurrences,
            })
            .unwrap_or_default()
    }
    pub fn validate(&self, anchor: &Anchor, state: &control::State) -> Result<Birth> {
        let birth = anchor.validate(state.origin, state.identity, state.generation)?;
        if self.format != 1
            || self.intent == [0; 32]
            || anchor.details != self.receipt_details()
            || self.birth != anchor.birth
            || self.logical != state.logical
            || self.live_records != state.live_records
            || self.live_bytes != state.live_bytes
            || self.next_file == 0
            || self.original_records > MAX_RECORDS as u64
            || self.original_bytes > MAX_STATE_BYTES as u64
            || self.live_records > self.original_records
        {
            return Err(Error::Authority);
        }
        let enrolled = self.data_key_version.is_some();
        if enrolled != self.physical.is_some()
            || enrolled != self.data.is_some()
            || self.data_key_version == Some([0; 32])
            || (!enrolled
                && (self.next_id != 0 || self.original_records != 0 || self.prune.is_some()))
        {
            return Err(Error::Authority);
        }
        for (name, root) in [
            (RootName::Checkpoint, &self.control.checkpoint),
            (RootName::Prefix, &self.control.prefix),
            (RootName::Request, &self.control.request),
            (RootName::Erased, &self.control.erased),
        ] {
            if root.format != 1 || root.purpose != name.purpose() {
                return Err(Error::Authority);
            }
        }
        if let Some(physical) = &self.physical {
            if physical.format != 1 {
                return Err(Error::Authority);
            }
        }
        Ok(birth)
    }
    /// The returned immutable object remains a candidate until control+witness publication.
    pub fn stage(&self, directory: &Path, cipher: &FrameCipher) -> Result<Anchor> {
        let birth = Birth::decode(&self.birth).map_err(io)?;
        let plain = json(self, ROOT_BYTES - oojuro_store::crypto::SEAL_OVERHEAD)?;
        let mut anchor = Anchor {
            format: 1,
            birth: self.birth.clone(),
            generation: birth.generation,
            key_version: birth.key_version,
            token: oojuro_store::crypto::random_key(),
            ciphertext_digest: [0; 32],
            bytes: 0,
            details: self.receipt_details(),
        };
        let sealed = cipher.seal_dict(&aad(&anchor), &plain);
        anchor.bytes = u32::try_from(sealed.len()).map_err(io)?;
        anchor.ciphertext_digest = *blake3::hash(&sealed).as_bytes();
        directory_create(&directory.join("paged-roots"))?;
        write_new(&bundle_path(directory, &anchor.token), &sealed)?;
        sync_dir(&directory.join("paged-roots"))?;
        Ok(anchor)
    }
}
pub(in crate::record::managed) fn load(
    directory: &Path,
    cipher: &FrameCipher,
    state: &control::State,
) -> Result<CommitRoot> {
    let anchor = state.paged.as_ref().ok_or(Error::Authority)?;
    anchor.validate(state.origin, state.identity, state.generation)?;
    let sealed = read_regular(&bundle_path(directory, &anchor.token), ROOT_BYTES)?;
    if sealed.len() != anchor.bytes as usize
        || blake3::hash(&sealed).as_bytes() != &anchor.ciphertext_digest
        || state.physical != anchor.ciphertext_digest
    {
        return Err(Error::Authority);
    }
    let plain = cipher.open_dict(&aad(anchor), &sealed).map_err(io)?;
    let root: CommitRoot = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
    root.validate(anchor, state)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::managed::tests::Fixture;
    struct Fresh(Vec<u8>);
    impl tree::Authority for Fresh {
        fn current(&self) -> Result<Vec<u8>> {
            Ok(self.0.clone())
        }
        fn validate_birth(&self, birth: &[u8], _: tree::Purpose, _: tree::KeyClass) -> Result<()> {
            if birth != self.0 {
                return Err(Error::Authority);
            }
            Ok(())
        }
    }
    fn empty(f: &Fixture, state: &control::State, birth: &Birth) -> CommitRoot {
        let authority = Fresh(birth.encode().unwrap());
        let cipher = FrameCipher::new(&[52; 32]);
        let build = |name: RootName| {
            let path = f.root.join(name.name());
            directory_create(&path).unwrap();
            tree::Builder::new(tree::Context {
                directory: &path,
                cipher: &cipher,
                authority: &authority,
                purpose: name.purpose(),
                identity: root_identity(state.identity, name),
                class: name.class(),
                sort_of: None,
                cache: None,
            })
            .unwrap()
            .finish()
            .unwrap()
        };
        CommitRoot {
            format: 1,
            intent: [95; 32],
            birth: birth.encode().unwrap(),
            data_key_version: None,
            next_id: 0,
            last_at: None,
            append_sequence: 0,
            next_file: 1,
            next_physical: 0,
            logical: [0; 32],
            original_records: 0,
            original_bytes: 0,
            live_records: 0,
            live_bytes: 0,
            physical: None,
            data: None,
            control: ControlRoots {
                checkpoint: build(RootName::Checkpoint),
                prefix: build(RootName::Prefix),
                request: build(RootName::Request),
                erased: build(RootName::Erased),
            },
            prune: None,
        }
    }
    #[test]
    fn optional_layout_keeps_original_snapshot_serialized_state_absent() {
        let f = Fixture::new();
        let owner = f.control();
        let state = owner.lock().unwrap().state.clone();
        let value = serde_json::to_value(&state).unwrap();
        assert!(value.get("paged").is_none());
        let encoded = serde_json::to_vec(&state).unwrap();
        let decoded: control::State = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.paged.is_none());
    }
    #[test]
    fn immutable_control_root_bundle_is_bounded_and_cross_bound_to_signed_anchor() {
        let f = Fixture::new();
        let owner = f.control();
        let mut state = owner.lock().unwrap().state.clone();
        let birth = Birth {
            format: 1,
            stable_origin: state.origin,
            identity: state.identity,
            epoch: [91; 32],
            generation: state.generation,
            predecessor_logical: state.logical,
            parent_checkpoint: [0; 32],
            key_version: [92; 32],
        };
        let root = empty(&f, &state, &birth);
        let cipher = FrameCipher::new(&[52; 32]);
        let anchor = root.stage(&f.root, &cipher).unwrap();
        assert!(anchor.bytes as usize <= ROOT_BYTES);
        assert!(json(&anchor, 4096).is_ok());
        state.physical = anchor.ciphertext_digest;
        state.paged = Some(anchor.clone());
        assert_eq!(
            load(&f.root, &cipher, &state)
                .unwrap()
                .control
                .checkpoint
                .count,
            0
        );
        let mut bad = state.clone();
        bad.paged.as_mut().unwrap().generation += 1;
        assert!(load(&f.root, &cipher, &bad).is_err());
        let mut bad = state.clone();
        bad.paged.as_mut().unwrap().key_version = [93; 32];
        assert!(load(&f.root, &cipher, &bad).is_err());
        assert!(load(&f.root, &FrameCipher::new(&[94; 32]), &state).is_err());
        let mut huge = root;
        huge.control.prefix.binding = vec![1; ROOT_BYTES];
        let before = fs::read_dir(f.root.join("paged-roots")).unwrap().count();
        assert!(matches!(huge.stage(&f.root, &cipher), Err(Error::Bounds)));
        assert_eq!(
            fs::read_dir(f.root.join("paged-roots")).unwrap().count(),
            before
        );
        let path = bundle_path(&f.root, &anchor.token);
        let mut bytes = fs::read(&path).unwrap();
        bytes[5] ^= 1;
        fs::write(path, bytes).unwrap();
        assert!(load(&f.root, &cipher, &state).is_err());
    }
}
