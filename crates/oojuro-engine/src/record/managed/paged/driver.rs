//! Incremental typed append: one bounded input window, COW roots, one retained publish.
use super::*;
use oojuro_store::managed_pack::{Reader, Reservation, Stage as PackStage};
use schema::RootName as N;
use source::{Paths, Source};
const FORMAT: &[u8] = b"oojuro-managed-record-paged-3\n";
const CHUNK: usize = 4 << 20;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RowRef {
    pub(super) first: u64,
    pub(super) chunks: u32,
    pub(super) bytes: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    fingerprint: [u8; 32],
    first_id: u64,
    first_at: i64,
    count: u32,
    sequence: u64,
    receipt: [u8; 32],
}
impl Receipt {
    fn commit(&self, origin: [u8; 32]) -> Result<OriginalCommit> {
        if self.count == 0 || self.count > 1000 {
            return Err(Error::Authority);
        }
        let ids = (0..self.count)
            .map(|i| self.first_id.checked_add(u64::from(i)).ok_or(Error::Bounds))
            .collect::<Result<Vec<_>>>()?;
        let recorded_at = (0..self.count)
            .map(|i| self.first_at.checked_add(i64::from(i)).ok_or(Error::Bounds))
            .collect::<Result<Vec<_>>>()?;
        Ok(OriginalCommit {
            origin,
            sequence: self.sequence,
            ids,
            recorded_at,
            receipt: self.receipt,
        })
    }
}
/// Explicit fresh paged DATA namespace. Current control is independently retained.
pub struct ManagedPagedLog {
    pub(super) directory: PathBuf,
    _lock: fs::File,
    pub(super) cipher: Arc<FrameCipher>,
    pub(super) identity: Identity,
    pub(super) control: Arc<RetainedControl>,
    enrollment_digest: [u8; 32],
    #[cfg(test)]
    pub(super) hook: Option<Arc<dyn Fn(Phase) + Send + Sync>>,
}
impl ManagedPagedLog {
    pub fn create(
        directory: &Path,
        key: &[u8; 32],
        identity: Identity,
        control: Arc<RetainedControl>,
    ) -> Result<Self> {
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
        let mut owner = control.lock()?;
        if !owner.is_paged()
            || owner.state.status != Status::Active
            || owner.state.identity != identity_binding(key, &identity)?
        {
            return Err(Error::Scope);
        }
        let cipher = Arc::new(FrameCipher::new(key));
        if owner.uses_key(&cipher) {
            return Err(Error::Scope);
        }
        if fresh {
            match fs::symlink_metadata(directory) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                _ => return Err(Error::Conflict),
            }
            if schema::load(&control.directory, owner.control_cipher(), &owner.state)?
                .data
                .is_some()
            {
                return Err(Error::Authority);
            }
            directory_create(directory)?;
        } else {
            let m = fs::symlink_metadata(directory).map_err(io)?;
            if !m.is_dir() || m.file_type().is_symlink() {
                return Err(Error::Authority);
            }
        }
        let directory = directory.canonicalize().map_err(io)?;
        if overlaps(&directory, &control.directory)
            || overlaps(&directory, &control.witness_directory)
        {
            return Err(Error::Scope);
        }
        let lock = owner_lock(&directory)?;
        let expected = owner.enrollment();
        let enrollment = if fresh {
            write_new(&directory.join("MANAGED_FORMAT"), FORMAT)?;
            let e = cipher.seal_dict(ENROLLMENT_AAD, &json(&expected, 4096)?);
            write_new(&directory.join("ENROLLMENT"), &e)?;
            sync_dir(&directory)?;
            sync_dir(directory.parent().ok_or(Error::Invalid)?)?;
            e
        } else {
            if read_regular(&directory.join("MANAGED_FORMAT"), 128)? != FORMAT {
                return Err(Error::Authority);
            }
            let e = read_regular(&directory.join("ENROLLMENT"), 8192)?;
            let actual: control::Enrollment =
                serde_json::from_slice(&cipher.open_dict(ENROLLMENT_AAD, &e).map_err(io)?)
                    .map_err(|_| Error::Authority)?;
            if actual != expected {
                return Err(Error::Authority);
            }
            e
        };
        let value = Self {
            directory,
            _lock: lock,
            cipher,
            identity,
            control: control.clone(),
            enrollment_digest: *blake3::hash(&enrollment).as_bytes(),
            #[cfg(test)]
            hook: None,
        };
        let mut root = schema::load(&control.directory, owner.control_cipher(), &owner.state)?;
        if fresh || root.data.is_none() {
            let birth = next_birth(&owner, &root)?;
            let control_stage = checkpoint::Stage {
                verified: owner.paged_verified()?,
                birth: birth.clone(),
                directory: &control.directory,
                cipher: owner.control_cipher(),
                class: tree::KeyClass::RetainedControl,
            };
            let data_version = oojuro_store::crypto::random_key();
            let mut data_birth = birth.clone();
            data_birth.key_version = data_version;
            let data_stage = checkpoint::Stage {
                verified: owner.paged_verified()?,
                birth: data_birth,
                directory: &control.directory,
                cipher: owner.control_cipher(),
                class: tree::KeyClass::Data,
            };
            root.data_namespace = Some(oojuro_store::crypto::random_key());
            root.enrollment_digest = Some(value.enrollment_digest);
            let data_directory = data_directory(&value.directory, &root)?;
            create_namespace(&data_directory)?;
            directory_create(&data_directory.join("packs"))?;
            directory_create(&data_directory.join("trees"))?;
            let paths = Paths::new(&control.directory, &data_directory);
            paths.create_data()?;
            root.physical = Some(physical::Roots::empty(&paths.physical(
                &value.cipher,
                &data_stage,
                owner.state.identity,
            ))?);
            let build = |n| {
                tree::Builder::new(paths.ctx(n, &value.cipher, &data_stage, owner.state.identity))?
                    .finish()
            };
            root.data = Some(schema::DataRoots {
                record: build(N::Record)?,
                key_version: build(N::KeyVersion)?,
                segment: build(N::Segment)?,
            });
            root.data_key_version = Some(data_version);
            root.next_id = value.identity.first_id;
            advance_index(&paths, &control_stage, &owner, &mut root)?;
            root.birth = birth.encode().map_err(io)?;
            value.publish(&mut owner, root)?;
        } else {
            if root.data.is_none() || root.next_id < value.identity.first_id {
                return Err(Error::Authority);
            }
            value.current(&owner)?;
        }
        drop(owner);
        Ok(value)
    }
    pub(super) fn current(&self, owner: &control::Inner) -> Result<()> {
        owner.recheck()?;
        self.witness_current(owner)
    }
    // Only used while the already fully verified owner is immutably held.
    // No currentness is cached: the independent exact CURRENT witness is read each time.
    pub(super) fn witness_current(&self, owner: &control::Inner) -> Result<()> {
        owner.paged_verified()?.assert_current()?;
        if owner.state.status != Status::Active
            || *blake3::hash(&read_regular(&self.directory.join("ENROLLMENT"), 8192)?).as_bytes()
                != self.enrollment_digest
            || read_regular(&self.directory.join("MANAGED_FORMAT"), 128)? != FORMAT
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub(super) fn source<'a>(
        &'a self,
        owner: &'a control::Inner,
        root: &'a schema::CommitRoot,
    ) -> Result<Source<'a>> {
        if root.enrollment_digest != Some(self.enrollment_digest)
            || root.next_id.checked_sub(self.identity.first_id) != Some(root.original_records)
        {
            return Err(Error::Authority);
        }
        let anchor = owner.checkpoint.floor().map_err(|_| Error::Authority)?;
        let cache = std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::new()));
        let a = |class| source::Authority {
            owner,
            control_directory: &self.control.directory,
            data_directory: &self.directory,
            enrollment_digest: self.enrollment_digest,
            root,
            class,
            anchor,
            cache: cache.clone(),
        };
        Ok(Source {
            paths: Paths::new(
                &self.control.directory,
                &data_directory(&self.directory, root)?,
            ),
            root,
            cipher: self.cipher.clone(),
            data: a(tree::KeyClass::Data),
            control: a(tree::KeyClass::RetainedControl),
        })
    }
    fn publish(&self, owner: &mut control::Inner, root: schema::CommitRoot) -> Result<()> {
        self.publish_event(owner, root, None, false)
    }
    pub(super) fn publish_event(
        &self,
        owner: &mut control::Inner,
        root: schema::CommitRoot,
        event: Option<crate::audit::EventKind>,
        pruned: bool,
    ) -> Result<()> {
        self.current(owner)?;
        let birth = Birth::decode(&root.birth).map_err(io)?;
        let mut state = owner.state.clone();
        state.generation = birth.generation;
        state.logical = root.logical;
        state.live_records = root.live_records;
        state.live_bytes = root.live_bytes;
        let anchor = root.stage(&self.control.directory, owner.control_cipher())?;
        state.physical = anchor.ciphertext_digest;
        state.audit = event
            .map(|kind| {
                owner.event(
                    kind,
                    root.prune.as_ref().map_or(0, |p| p.selected_count),
                    root.prune.as_ref().map_or(0, |p| p.selected_bytes),
                    owner.state.physical,
                    state.physical,
                )
            })
            .transpose()?;
        state.pruned = pruned;
        state.paged = Some(anchor);
        #[cfg(test)]
        self.phase(Phase::AfterStage);
        owner.advance(state)?;
        #[cfg(test)]
        self.phase(Phase::AfterControl);
        self.current(owner)?;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn phase(&self, at: Phase) {
        if let Some(h) = &self.hook {
            h(at);
        }
    }
    pub fn binding(&self) -> Result<Binding> {
        let owner = self.control.lock()?;
        self.current(&owner)?;
        Ok(binding(
            &owner.state,
            owner.checkpoint.floor().map_err(|_| Error::Authority)?,
        ))
    }
    pub fn validate_binding(&self, expected: &Binding) -> Result<()> {
        if self.binding()? != *expected {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub fn get(&self, id: u64, max_bytes: usize) -> Result<Option<Record>> {
        let owner = self.control.lock()?;
        self.current(&owner)?;
        let root = schema::load(
            &self.control.directory,
            owner.control_cipher(),
            &owner.state,
        )?;
        let source = self.source(&owner, &root)?;
        let out = read_assigned(&source, &self.directory, id)?
            .map(|r| r.record(&self.identity.writer, &self.identity.adapter));
        if let Some(r) = &out {
            json(r, max_bytes.min(MAX_OUTPUT_BYTES))?;
        }
        #[cfg(test)]
        self.phase(Phase::BeforeAck);
        self.current(&owner)?;
        Ok(out)
    }
    pub fn append(&mut self, request: &str, inputs: &[Input]) -> Result<AppendResult> {
        if !name(request, 1024) || inputs.is_empty() || inputs.len() > 1000 {
            return Err(Error::Bounds);
        }
        for i in inputs {
            i.validate()?;
        }
        let encoded = json(inputs, MAX_BATCH_BYTES)?;
        let control = self.control.clone();
        let mut owner = control.lock()?;
        self.current(&owner)?;
        let mut root = schema::load(&control.directory, owner.control_cipher(), &owner.state)?;
        if root
            .prune
            .as_ref()
            .is_some_and(|p| p.phase != schema::PrunePhase::Completed)
        {
            return Err(Error::Unavailable);
        }
        let tag = commitment(&root.intent, &("append-request-v3", request))?;
        let fingerprint = *blake3::keyed_hash(&root.intent, &encoded).as_bytes();
        drop(encoded);
        let source = self.source(&owner, &root)?;
        if let Some(bytes) = source.lookup(N::Request, &root.control.request, &tag)? {
            let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
            if receipt.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            let commit = receipt.commit(owner.state.origin)?;
            let mut removed_ids = Vec::new();
            for id in &commit.ids {
                if read_assigned(&source, &self.directory, *id)?.is_none() {
                    removed_ids.push(*id);
                }
            }
            self.current(&owner)?;
            return Ok(AppendResult {
                commit,
                receipt_only: !removed_ids.is_empty(),
                removed_ids,
            });
        }
        drop(source);
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
        let data_directory = data_directory(&self.directory, &root)?;
        let paths = Paths::new(&control.directory, &data_directory);
        let mut physical = physical::Editor::new(
            paths.physical(&self.cipher, &data_stage, owner.state.identity),
            root.physical.take().ok_or(Error::Authority)?,
        )?;
        let data = root.data.as_mut().ok_or(Error::Authority)?;
        let mut records = tree::Editor::new(
            paths.ctx(N::Record, &self.cipher, &data_stage, owner.state.identity),
            data.record.clone(),
        )?;
        let mut keys = tree::Editor::new(
            paths.ctx(
                N::KeyVersion,
                &self.cipher,
                &data_stage,
                owner.state.identity,
            ),
            data.key_version.clone(),
        )?;
        let mut segments = tree::Editor::new(
            paths.ctx(N::Segment, &self.cipher, &data_stage, owner.state.identity),
            data.segment.clone(),
        )?;
        let mut pack = PackStage::new(
            &data_directory.join("packs"),
            data_birth,
            Reservation {
                file_id: root.next_file,
                first_physical_seq: root.next_physical,
            },
            self.cipher.clone(),
            oojuro_store::managed_pack::Options::default(),
        )
        .map_err(io)?;
        let first_id = root.next_id;
        let first_at = match root.last_at {
            Some(at) => at.checked_add(1).ok_or(Error::Bounds)?.max(now()?),
            None => now()?,
        };
        let mut physical_seq = root.next_physical;
        let mut bytes_added = 0u64;
        for (i, input) in inputs.iter().enumerate() {
            let row = Assigned {
                id: first_id.checked_add(i as u64).ok_or(Error::Bounds)?,
                recorded_at: first_at.checked_add(i as i64).ok_or(Error::Bounds)?,
                input: input.clone(),
            };
            let bytes = json(&row, MAX_BATCH_BYTES)?;
            let reference = RowRef {
                first: physical_seq,
                chunks: bytes.len().div_ceil(CHUNK) as u32,
                bytes: bytes.len() as u32,
            };
            for chunk in bytes.chunks(CHUNK) {
                pack.push(&[carrier(chunk)], &mut |l| {
                    physical.insert_frame(&l).map_err(|_| {
                        oojuro_store::Error::Corrupt("managed locator insertion failed")
                    })
                })
                .map_err(io)?;
                physical_seq = physical_seq.checked_add(1).ok_or(Error::Bounds)?;
            }
            records.insert_immutable(tree::Entry::new(
                row.id.to_be_bytes().to_vec(),
                json(&reference, 1024)?,
            )?)?;
            if let Some(key) = &input.key {
                let mut k = commitment(&root.intent, &("key-v3", key))?.to_vec();
                k.extend((input.valid_at as u64 ^ (1 << 63)).to_be_bytes());
                k.extend(row.id.to_be_bytes());
                keys.insert_immutable(tree::Entry::new(k, row.id.to_be_bytes().to_vec())?)?;
            }
            bytes_added = bytes_added
                .checked_add(bytes.len() as u64)
                .ok_or(Error::Bounds)?;
        }
        let pack = pack.finish().map_err(io)?;
        if pack.record_count != physical_seq - root.next_physical
            || pack.first_physical_seq != root.next_physical
        {
            return Err(Error::Authority);
        }
        physical.insert_pack(&pack)?;
        segments.insert_immutable(tree::Entry::new(
            first_id.to_be_bytes().to_vec(),
            pack.file_id.to_be_bytes().to_vec(),
        )?)?;
        data.record = records.finish()?;
        data.key_version = keys.finish()?;
        data.segment = segments.finish()?;
        root.physical = Some(physical.finish()?);
        root.next_id = first_id
            .checked_add(inputs.len() as u64)
            .ok_or(Error::Bounds)?;
        root.last_at = Some(
            first_at
                .checked_add(inputs.len() as i64 - 1)
                .ok_or(Error::Bounds)?,
        );
        root.next_physical = physical_seq;
        root.next_file = root.next_file.checked_add(1).ok_or(Error::Bounds)?;
        root.append_sequence = root.append_sequence.checked_add(1).ok_or(Error::Bounds)?;
        let receipt_tag = commitment(
            &root.intent,
            &(
                "logical-append-v3",
                owner.state.origin,
                root.logical,
                root.append_sequence,
                tag,
                fingerprint,
                first_id,
                first_at,
                inputs.len(),
            ),
        )?;
        let receipt = Receipt {
            fingerprint,
            first_id,
            first_at,
            count: inputs.len() as u32,
            sequence: root.append_sequence,
            receipt: receipt_tag,
        };
        let mut requests = tree::Editor::new(
            paths.ctx(
                N::Request,
                owner.control_cipher(),
                &control_stage,
                owner.state.identity,
            ),
            root.control.request,
        )?;
        requests.insert_immutable(tree::Entry::new(tag.to_vec(), json(&receipt, 4096)?)?)?;
        root.control.request = requests.finish()?;
        let mut prefixes = tree::Editor::new(
            paths.ctx(
                N::Prefix,
                owner.control_cipher(),
                &control_stage,
                owner.state.identity,
            ),
            root.control.prefix,
        )?;
        prefixes.insert_immutable(tree::Entry::new(
            root.append_sequence.to_be_bytes().to_vec(),
            receipt_tag.to_vec(),
        )?)?;
        root.control.prefix = prefixes.finish()?;
        advance_index(&paths, &control_stage, &owner, &mut root)?;
        root.birth = birth.encode().map_err(io)?;
        root.logical = receipt_tag;
        root.prune = None;
        root.original_records = root
            .original_records
            .checked_add(inputs.len() as u64)
            .ok_or(Error::Bounds)?;
        root.original_bytes = root
            .original_bytes
            .checked_add(bytes_added)
            .ok_or(Error::Bounds)?;
        root.live_records = root
            .live_records
            .checked_add(inputs.len() as u64)
            .ok_or(Error::Bounds)?;
        root.live_bytes = root
            .live_bytes
            .checked_add(bytes_added)
            .ok_or(Error::Bounds)?;
        // Fully read back the actual staged tail via authenticated candidate roots before publication.
        verify_tail(self, &owner, &root, &data_stage, first_id, inputs)?;
        self.publish(&mut owner, root)?;
        Ok(AppendResult {
            commit: receipt.commit(owner.state.origin)?,
            receipt_only: false,
            removed_ids: Vec::new(),
        })
    }
}
pub(super) fn commitment<T: Serialize + ?Sized>(key: &[u8; 32], value: &T) -> Result<[u8; 32]> {
    Ok(*blake3::keyed_hash(key, &json(value, MAX_BATCH_BYTES)?).as_bytes())
}
pub(super) fn next_birth(owner: &control::Inner, root: &schema::CommitRoot) -> Result<Birth> {
    let mut b = Birth::decode(&root.birth).map_err(io)?;
    b.generation = owner.state.generation.checked_add(1).ok_or(Error::Bounds)?;
    b.epoch = oojuro_store::crypto::random_key();
    b.parent_checkpoint = owner.checkpoint.digest().map_err(|_| Error::Authority)?;
    b.predecessor_logical = owner.state.logical;
    Ok(b)
}
pub(super) fn advance_index(
    paths: &Paths,
    stage: &checkpoint::Stage<'_>,
    owner: &control::Inner,
    root: &mut schema::CommitRoot,
) -> Result<()> {
    let mut index = tree::Editor::new(
        paths.ctx(
            N::Checkpoint,
            owner.control_cipher(),
            stage,
            owner.state.identity,
        ),
        root.control.checkpoint.clone(),
    )?;
    index.insert_immutable(tree::Entry::new(
        owner.checkpoint.body.sequence.to_be_bytes().to_vec(),
        owner
            .checkpoint
            .digest()
            .map_err(|_| Error::Authority)?
            .to_vec(),
    )?)?;
    root.control.checkpoint = index.finish()?;
    Ok(())
}
pub(super) fn read_assigned(
    source: &Source<'_>,
    directory: &Path,
    id: u64,
) -> Result<Option<Assigned>> {
    let mut erased = vec![0];
    erased.extend(id.to_be_bytes());
    if source
        .lookup(N::Erased, &source.root.control.erased, &erased)?
        .is_some()
    {
        return Ok(None);
    }
    let data = source.root.data.as_ref().ok_or(Error::Authority)?;
    let Some(bytes) = source.lookup(N::Record, &data.record, &id.to_be_bytes())? else {
        return Ok(None);
    };
    let r: RowRef = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
    let reader = Reader::new(
        &data_directory(directory, source.root)?.join("packs"),
        source,
        4 << 20,
    )
    .map_err(io)?;
    decode_row(&reader, &r, id)
}
pub(super) fn decode_row<C: oojuro_store::managed_pack::Catalog>(
    reader: &Reader<C>,
    r: &RowRef,
    id: u64,
) -> Result<Option<Assigned>> {
    if r.bytes == 0
        || r.bytes as usize > MAX_BATCH_BYTES
        || r.chunks == 0
        || r.chunks as usize != (r.bytes as usize).div_ceil(CHUNK)
    {
        return Err(Error::Authority);
    }
    let mut bytes = Vec::with_capacity(r.bytes as usize);
    for i in 0..r.chunks {
        let chunk = reader
            .get(r.first.checked_add(u64::from(i)).ok_or(Error::Bounds)?)
            .map_err(io)?;
        let expected = CHUNK.min(r.bytes as usize - bytes.len());
        if chunk.len() != expected {
            return Err(Error::Authority);
        }
        bytes.extend(chunk);
    }
    let row: Assigned = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
    if row.id != id {
        return Err(Error::Authority);
    }
    row.input.validate()?;
    Ok(Some(row))
}

// Not exported: this capability can only verify an exact candidate tail under the
// currently held predecessor. It is never a serving source and cannot issue an ACK.
struct StagedTail<'a> {
    owner: &'a control::Inner,
    stage: &'a checkpoint::Stage<'a>,
    paths: &'a Paths,
    roots: &'a physical::Roots,
    cipher: Arc<FrameCipher>,
    first: u64,
    end: u64,
    enrollment_directory: &'a Path,
    enrollment_digest: [u8; 32],
}
impl StagedTail<'_> {
    fn contexts(&self) -> physical::Contexts<'_> {
        self.paths
            .physical(&self.cipher, self.stage, self.owner.state.identity)
    }
    fn check(&self) -> oojuro_store::Result<Vec<u8>> {
        self.owner
            .paged_verified()
            .and_then(|v| v.assert_current())
            .map_err(|_| oojuro_store::Error::Corrupt("candidate predecessor unavailable"))?;
        if *blake3::hash(
            &read_regular(&self.enrollment_directory.join("ENROLLMENT"), 8192)
                .map_err(|_| oojuro_store::Error::Corrupt("candidate enrollment unavailable"))?,
        )
        .as_bytes()
            != self.enrollment_digest
        {
            return Err(oojuro_store::Error::Corrupt("candidate enrollment changed"));
        }
        tree::Authority::current(self.stage)
            .map_err(|_| oojuro_store::Error::Corrupt("candidate birth unavailable"))
    }
}
impl oojuro_store::managed_pack::Catalog for &StagedTail<'_> {
    fn current(&self) -> oojuro_store::Result<Vec<u8>> {
        self.check()
    }
    fn validate_birth(&self, b: &Birth) -> oojuro_store::Result<()> {
        self.check()?;
        tree::Authority::validate_birth(
            self.stage,
            &b.encode()?,
            tree::Purpose::Frame,
            tree::KeyClass::Data,
        )
        .map_err(|_| oojuro_store::Error::Corrupt("invalid candidate pack birth"))
    }
    fn locate(&self, id: u64) -> oojuro_store::Result<oojuro_store::managed_pack::Locator> {
        self.check()?;
        if id < self.first || id >= self.end {
            return Err(oojuro_store::Error::Corrupt("candidate tail range"));
        }
        self.roots
            .locate(&self.contexts(), id)
            .map_err(|_| oojuro_store::Error::Corrupt("candidate locator invalid"))
    }
    fn pack(&self, id: u64) -> oojuro_store::Result<oojuro_store::managed_pack::PackRef> {
        self.check()?;
        self.roots
            .pack(&self.contexts(), id)
            .map_err(|_| oojuro_store::Error::Corrupt("candidate pack invalid"))
    }
    fn dictionary(
        &self,
        id: u32,
    ) -> oojuro_store::Result<Option<oojuro_store::managed_pack::DictionaryRef>> {
        self.check()?;
        self.roots
            .dictionary(&self.contexts(), id)
            .map_err(|_| oojuro_store::Error::Corrupt("candidate dictionary invalid"))
    }
    fn cipher(&self, key: &[u8; 32]) -> oojuro_store::Result<Arc<FrameCipher>> {
        self.check()?;
        if key != &self.stage.birth.key_version {
            return Err(oojuro_store::Error::Corrupt("candidate key version"));
        }
        Ok(self.cipher.clone())
    }
}
fn verify_tail(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    stage: &checkpoint::Stage<'_>,
    first_id: u64,
    inputs: &[Input],
) -> Result<()> {
    let namespace = data_directory(&log.directory, root)?;
    let paths = Paths::new(&log.control.directory, &namespace);
    let data = root.data.as_ref().ok_or(Error::Authority)?;
    let mut first = None;
    // Index roots are verified under the private stage capability; actual bytes are
    // decoded through the production pack Reader before the independent publication.
    for (i, input) in inputs.iter().enumerate() {
        let id = first_id.checked_add(i as u64).ok_or(Error::Bounds)?;
        let value = tree::get_ancestor(
            paths.ctx(N::Record, &log.cipher, stage, owner.state.identity),
            &data.record,
            &id.to_be_bytes(),
        )?
        .ok_or(Error::Authority)?;
        let r: RowRef = serde_json::from_slice(&value).map_err(|_| Error::Authority)?;
        let start = *first.get_or_insert(r.first);
        let candidate = StagedTail {
            owner,
            stage,
            paths: &paths,
            roots: root.physical.as_ref().ok_or(Error::Authority)?,
            cipher: log.cipher.clone(),
            first: start,
            end: root.next_physical,
            enrollment_directory: &log.directory,
            enrollment_digest: log.enrollment_digest,
        };
        let reader = Reader::new(&namespace.join("packs"), &candidate, 4 << 20).map_err(io)?;
        let row = decode_row(&reader, &r, id)?.ok_or(Error::Authority)?;
        let expected_at = root
            .last_at
            .ok_or(Error::Authority)?
            .checked_sub((inputs.len() - 1 - i) as i64)
            .ok_or(Error::Bounds)?;
        if row.recorded_at != expected_at
            || json(&row.input, MAX_BATCH_BYTES)? != json(input, MAX_BATCH_BYTES)?
        {
            return Err(Error::Authority);
        }
    }
    owner.recheck()
}

pub(super) fn data_directory(directory: &Path, root: &schema::CommitRoot) -> Result<PathBuf> {
    Ok(directory.join(format!(
        "data-{}",
        schema::token_hex(&root.data_namespace.ok_or(Error::Authority)?)
    )))
}

pub(super) fn candidate_row(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    stage: &checkpoint::Stage<'_>,
    id: u64,
) -> Result<Assigned> {
    let namespace = data_directory(&log.directory, root)?;
    let paths = Paths::new(&log.control.directory, &namespace);
    let data = root.data.as_ref().ok_or(Error::Authority)?;
    let value = tree::get_ancestor(
        paths.ctx(N::Record, &log.cipher, stage, owner.state.identity),
        &data.record,
        &id.to_be_bytes(),
    )?
    .ok_or(Error::Authority)?;
    let r: RowRef = serde_json::from_slice(&value).map_err(|_| Error::Authority)?;
    let candidate = StagedTail {
        owner,
        stage,
        paths: &paths,
        roots: root.physical.as_ref().ok_or(Error::Authority)?,
        cipher: log.cipher.clone(),
        first: 0,
        end: root.next_physical,
        enrollment_directory: &log.directory,
        enrollment_digest: log.enrollment_digest,
    };
    let reader = Reader::new(&namespace.join("packs"), &candidate, 4 << 20).map_err(io)?;
    decode_row(&reader, &r, id)?.ok_or(Error::Authority)
}

pub(super) fn create_namespace(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path).map_err(io)?;
    sync_dir(path.parent().ok_or(Error::Invalid)?)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::record::managed::tests::{Fixture, identity, input};
    const DATA: [u8; 32] = [51; 32];
    const CONTROL: [u8; 32] = [52; 32];
    const AUTH: [u8; 32] = [53; 32];
    pub(in crate::record::managed::paged) fn control(
        f: &Fixture,
        create: bool,
    ) -> Arc<RetainedControl> {
        let witness = FileWitness::open(&f.root.join("paged-witness")).unwrap();
        let open = if create {
            RetainedControl::create_paged
        } else {
            RetainedControl::open_paged
        };
        Arc::new(
            open(
                &f.root.join("paged-control"),
                &CONTROL,
                f.signer(),
                AUTH,
                witness,
                identity_binding(&DATA, &identity()).unwrap(),
            )
            .unwrap(),
        )
    }
    pub(in crate::record::managed::paged) fn log(
        f: &Fixture,
        c: Arc<RetainedControl>,
        create: bool,
    ) -> ManagedPagedLog {
        let open = if create {
            ManagedPagedLog::create
        } else {
            ManagedPagedLog::open
        };
        open(&f.root.join("paged-data"), &DATA, identity(), c).unwrap()
    }
    #[test]
    fn real_pack_append_exact_ack_tail_reopen_and_control_fencing() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c.clone(), true);
        let mut rows = vec![
            input("alpha", "real first typed original"),
            input("beta", "real second"),
        ];
        rows[0].valid_at = i64::MIN;
        rows[1].valid_at = i64::MAX;
        rows[1].indexed = false;
        rows[0].key = Some("actual-key".into());
        let first = l.append("original-request", &rows).unwrap();
        assert_eq!(first.commit.ids, vec![9, 10]);
        assert_eq!(l.get(9, 4096).unwrap().unwrap().valid_at, i64::MIN);
        assert!(!l.get(10, 4096).unwrap().unwrap().indexed);
        let before = l.binding().unwrap();
        assert_eq!(
            l.append("original-request", &rows).unwrap().commit,
            first.commit
        );
        assert_eq!(l.binding().unwrap(), before);
        assert!(
            l.append("original-request", &[input("alpha", "changed")])
                .is_err()
        );
        let second = l.append("later", &[input("gamma", "third")]).unwrap();
        assert_eq!(second.commit.ids, vec![11]);
        assert_eq!(l.get(9, 4096).unwrap().unwrap().text, rows[0].text);
        assert!(l.validate_binding(&before).is_err());
        drop(l);
        drop(c);
        let c = control(&f, false);
        let mut l = log(&f, c.clone(), false);
        assert_eq!(
            l.append("original-request", &rows).unwrap().commit,
            first.commit
        );
        assert_eq!(
            l.get(11, 4096).unwrap().unwrap().recorded_at,
            second.commit.recorded_at[0]
        );
        c.deny().unwrap();
        assert!(l.get(9, 4096).is_err());
        assert!(l.append("after-denial", &rows).is_err());
    }
    #[test]
    fn legal_large_typed_input_is_chunked_and_never_changes_original_time() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c, true);
        let mut row = input("large", "");
        row.text = "z".repeat(MAX_TEXT_BYTES);
        let ack = l.append("large", &[row.clone()]).unwrap();
        let read = l.get(9, MAX_OUTPUT_BYTES).unwrap().unwrap();
        assert_eq!(read.text, row.text);
        assert_eq!(read.recorded_at, ack.commit.recorded_at[0]);
        let owner = l.control.lock().unwrap();
        let root =
            schema::load(&l.control.directory, owner.control_cipher(), &owner.state).unwrap();
        assert!(root.next_physical > 1);
        assert_eq!(root.next_id, 10);
    }
    #[test]
    fn worst_escaping_legal_text_refuses_serialized_window_before_any_publication() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c, true);
        let mut row = input("escaped", "");
        row.text = "\0".repeat(MAX_TEXT_BYTES);
        row.validate().unwrap();
        let before = l.binding().unwrap();
        assert!(matches!(
            l.append("escaped-window", &[row]),
            Err(Error::Bounds)
        ));
        assert_eq!(l.binding().unwrap(), before);
        assert!(l.get(9, 4096).unwrap().is_none());
    }

    #[test]
    fn full_end_guard_refuses_bundle_corruption_and_external_witness_change_after_read() {
        for change_bundle in [true, false] {
            let f = Fixture::new();
            let c = control(&f, true);
            let mut l = log(&f, c.clone(), true);
            l.append("original", &[input("private", "authentic original")])
                .unwrap();
            let owner = c.lock().unwrap();
            let anchor = owner.state.paged.as_ref().unwrap().clone();
            let path = if change_bundle {
                c.directory
                    .join("paged-roots")
                    .join(format!("root-{}.bin", schema::token_hex(&anchor.token)))
            } else {
                c.witness_directory.join("CURRENT")
            };
            drop(owner);
            l.hook = Some(Arc::new(move |at| {
                if at == Phase::BeforeAck {
                    let mut bytes = fs::read(&path).unwrap();
                    bytes[0] ^= 1;
                    fs::write(&path, bytes).unwrap();
                }
            }));
            assert!(l.get(9, 4096).is_err());
        }
    }
    #[test]
    fn wrong_fresh_control_and_snapshot_layout_cannot_reenroll_existing_paged_data() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c.clone(), true);
        l.append("original", &[input("alpha", "must survive")])
            .unwrap();
        drop(l);
        drop(c);
        let wrong = Arc::new(
            RetainedControl::create_paged(
                &f.root.join("other-control"),
                &CONTROL,
                f.signer(),
                AUTH,
                FileWitness::open(&f.root.join("other-witness")).unwrap(),
                identity_binding(&DATA, &identity()).unwrap(),
            )
            .unwrap(),
        );
        assert!(
            ManagedPagedLog::open(&f.root.join("paged-data"), &DATA, identity(), wrong).is_err()
        );
        let c = control(&f, false);
        let l = log(&f, c.clone(), false);
        assert_eq!(l.get(9, 4096).unwrap().unwrap().text, "must survive");
        drop(l);
        assert!(ManagedLog::open(&f.root.join("paged-data"), &DATA, identity(), c).is_err());
    }
}
