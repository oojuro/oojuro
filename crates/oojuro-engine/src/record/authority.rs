//! One sealed Store append commits content, assigned times/IDs and exact retry receipt.
use super::*;
use crate::capture_log::Identity;
use oojuro_store::{NewRecord, Options, Store, crypto::FrameCipher};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
pub(super) const FORMAT: &[u8] = b"oojuro-record-2\n";
const PREFIX_AAD: &[u8] = b"oojuro/record-prefix/v1\0";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watermark {
    pub schema: u32,
    pub origin: [u8; 32],
    pub identity: [u8; 32],
    pub sequence: u64,
    pub records: u64,
    pub commitment: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub ids: Vec<u64>,
    pub recorded_at: Vec<i64>,
    pub watermark: Watermark,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Batch {
    pub(super) schema: u32,
    pub(super) request: [u8; 32],
    pub(super) fingerprint: [u8; 32],
    pub(super) assigned: Vec<Assigned>,
    pub(super) watermark: Watermark,
}
#[derive(Serialize)]
pub(super) struct Unsigned<'a> {
    pub(super) schema: u32,
    pub(super) request: [u8; 32],
    pub(super) fingerprint: [u8; 32],
    pub(super) assigned: &'a [Assigned],
}
struct Prefix {
    digest: [u8; 32],
    records: u64,
}
struct Receipt {
    fingerprint: [u8; 32],
    commit: Commit,
}
struct IntentKey([u8; 32]);
impl Drop for IntentKey {
    fn drop(&mut self) {
        for b in &mut self.0 {
            unsafe { std::ptr::write_volatile(b, 0) }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}
/// Direct SDK record namespace. V1 CaptureLog, raw capture and receipt formats are untouched.
pub struct RecordLog {
    store: Store,
    cipher: Arc<FrameCipher>,
    intent: IntentKey,
    identity: Identity,
    origin: [u8; 32],
    identity_tag: [u8; 32],
    prefixes: Vec<Prefix>,
    pub(super) records: Vec<Assigned>,
    receipts: BTreeMap<[u8; 32], Receipt>,
    bytes: usize,
    failed: bool,
    clock: Arc<dyn Fn() -> Result<i64> + Send + Sync>,
    #[cfg(test)]
    pub(super) hook: Option<Box<dyn Fn(Phase)>>,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) enum Phase {
    BeforeStore,
    AfterStore,
    BeforeAck,
}
impl RecordLog {
    pub fn open(dir: &Path, key: &[u8; 32], identity: Identity) -> Result<Self> {
        Self::open_clock(dir, key, identity, Arc::new(now))
    }
    pub(super) fn open_clock(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        clock: Arc<dyn Fn() -> Result<i64> + Send + Sync>,
    ) -> Result<Self> {
        if !name(&identity.writer, 256)
            || identity.agent == 0
            || identity.first_id == 0
            || !name(&identity.adapter, 128)
        {
            return Err(Error::Invalid);
        }
        let parent = dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.is_dir() {
            return Err(Error::Invalid);
        }
        namespace(dir)?;
        let cipher = Arc::new(FrameCipher::new(key));
        let mut store = Store::open(
            dir,
            Options {
                cipher: Some(cipher.clone()),
                require_sealed: true,
                sync: true,
                compact_threads: 1,
                ..Options::default()
            },
        )
        .map_err(|_| Error::Unavailable)?;
        let mut header = FORMAT.to_vec();
        header.extend(serde_json::to_vec(&identity).map_err(|_| Error::Invalid)?);
        if store.is_empty() {
            store
                .append(&[carrier(&header)])
                .map_err(|_| Error::Unavailable)?;
        } else if store.get(0).map_err(|_| Error::Unavailable)? != header {
            return Err(Error::Authority);
        }
        let origin = store
            .sealed_origin_digest()
            .map_err(|_| Error::Unavailable)?;
        let intent = IntentKey(blake3::derive_key("oojuro direct record intent v1", key));
        let identity_tag = *blake3::keyed_hash(&intent.0, &header).as_bytes();
        let bytes = header.len();
        let mut log = Self {
            store,
            cipher,
            intent,
            identity,
            origin,
            identity_tag,
            prefixes: vec![Prefix {
                digest: *blake3::hash(&header).as_bytes(),
                records: 0,
            }],
            records: vec![],
            receipts: BTreeMap::new(),
            bytes,
            failed: false,
            clock,
            #[cfg(test)]
            hook: None,
        };
        for sequence in 1..log.store.len() {
            let encoded = log.store.get(sequence).map_err(|_| Error::Unavailable)?;
            log.replay(sequence, &encoded)?;
        }
        // Also on retry after an interrupted initialization, before acknowledging open.
        fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
        Ok(log)
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub(super) fn healthy(&self) -> Result<()> {
        if self.failed {
            Err(Error::Unavailable)
        } else {
            Ok(())
        }
    }
    fn tag<T: Serialize + ?Sized>(&self, domain: &str, value: &T) -> Result<[u8; 32]> {
        let encoded = bounded_json(
            &(domain, value),
            MAX_BATCH_BYTES,
            blake3::Hasher::new_keyed(&self.intent.0),
        )?;
        Ok(*encoded.writer.finalize().as_bytes())
    }
    pub fn append(&mut self, request: &str, inputs: &[Input]) -> Result<Commit> {
        self.healthy()?;
        if !name(request, 1024) || inputs.is_empty() || inputs.len() > 1000 {
            return Err(Error::Bounds);
        }
        for input in inputs {
            input.validate()?;
        }
        let request = self.tag("request", request)?;
        let fingerprint = self.tag("typed-input-v1", inputs)?;
        if let Some(old) = self.receipts.get(&request) {
            return if old.fingerprint == fingerprint {
                Ok(old.commit.clone())
            } else {
                Err(Error::Conflict)
            };
        }
        if self
            .records
            .len()
            .checked_add(inputs.len())
            .is_none_or(|n| n > MAX_RECORDS)
        {
            return Err(Error::Bounds);
        }
        let mut id = self
            .identity
            .first_id
            .checked_add(self.records.len() as u64)
            .ok_or(Error::Bounds)?;
        let mut previous = self.records.last().map(|r| r.recorded_at);
        let now = (self.clock)()?;
        let mut assigned = Vec::new();
        for input in inputs {
            let recorded_at = match previous {
                Some(last) => now.max(last.checked_add(1).ok_or(Error::Bounds)?),
                None => now,
            };
            assigned.push(Assigned {
                id,
                recorded_at,
                input: input.clone(),
            });
            // Reserve the successor before committing; IDs are never wrapped/reused.
            id = id.checked_add(1).ok_or(Error::Bounds)?;
            previous = Some(recorded_at);
        }
        let sequence = self.store.len();
        let unsigned = Unsigned {
            schema: 2,
            request,
            fingerprint,
            assigned: &assigned,
        };
        let canonical = json(&unsigned, MAX_BATCH_BYTES)?;
        if canonical.len() > MAX_BATCH_BYTES {
            return Err(Error::Bounds);
        }
        let digest = chain(
            self.prefixes.last().ok_or(Error::Authority)?.digest,
            &canonical,
        );
        let watermark = self.seal_prefix(
            sequence,
            (self.records.len() + assigned.len()) as u64,
            digest,
        )?;
        let batch = Batch {
            schema: 2,
            request,
            fingerprint,
            assigned,
            watermark,
        };
        let encoded = json(&batch, MAX_BATCH_BYTES)?;
        self.capacity(encoded.len())?;
        #[cfg(test)]
        if let Some(h) = &self.hook {
            h(Phase::BeforeStore);
        }
        self.failed = true;
        let actual = self
            .store
            .append(&[carrier(&encoded)])
            .map_err(|_| Error::Unavailable)?;
        if actual != sequence {
            return Err(Error::Authority);
        }
        #[cfg(test)]
        if let Some(h) = &self.hook {
            h(Phase::AfterStore);
        }
        self.replay(sequence, &encoded)?;
        self.failed = false;
        #[cfg(test)]
        if let Some(h) = &self.hook {
            h(Phase::BeforeAck);
        }
        Ok(self
            .receipts
            .get(&request)
            .ok_or(Error::Authority)?
            .commit
            .clone())
    }
    fn capacity(&self, additional: usize) -> Result<()> {
        if additional > MAX_BATCH_BYTES
            || self
                .bytes
                .checked_add(additional)
                .is_none_or(|n| n > MAX_STATE_BYTES)
        {
            return Err(Error::Bounds);
        }
        Ok(())
    }
    fn replay(&mut self, sequence: u64, encoded: &[u8]) -> Result<()> {
        self.capacity(encoded.len())?;
        let batch: Batch = serde_json::from_slice(encoded).map_err(|_| Error::Authority)?;
        if batch.schema != 2
            || sequence != self.prefixes.len() as u64
            || batch.assigned.is_empty()
            || batch.assigned.len() > 1000
            || self.receipts.contains_key(&batch.request)
            || self.records.len() + batch.assigned.len() > MAX_RECORDS
        {
            return Err(Error::Authority);
        }
        let mut id = self
            .identity
            .first_id
            .checked_add(self.records.len() as u64)
            .ok_or(Error::Authority)?;
        let mut previous = self.records.last().map(|r| r.recorded_at);
        for row in &batch.assigned {
            row.input.validate().map_err(|_| Error::Authority)?;
            if row.id != id || previous.is_some_and(|p| row.recorded_at <= p) {
                return Err(Error::Authority);
            }
            id = id.checked_add(1).ok_or(Error::Authority)?;
            previous = Some(row.recorded_at);
        }
        let input: Vec<_> = batch.assigned.iter().map(|r| &r.input).collect();
        if self.tag("typed-input-v1", &input)? != batch.fingerprint {
            return Err(Error::Authority);
        }
        let canonical = serde_json::to_vec(&Unsigned {
            schema: 2,
            request: batch.request,
            fingerprint: batch.fingerprint,
            assigned: &batch.assigned,
        })
        .map_err(|_| Error::Authority)?;
        let digest = chain(
            self.prefixes.last().ok_or(Error::Authority)?.digest,
            &canonical,
        );
        let count = (self.records.len() + batch.assigned.len()) as u64;
        self.check_prefix(&batch.watermark, sequence, count, digest)?;
        let commit = Commit {
            ids: batch.assigned.iter().map(|r| r.id).collect(),
            recorded_at: batch.assigned.iter().map(|r| r.recorded_at).collect(),
            watermark: batch.watermark,
        };
        self.receipts.insert(
            batch.request,
            Receipt {
                fingerprint: batch.fingerprint,
                commit,
            },
        );
        self.records.extend(batch.assigned);
        self.prefixes.push(Prefix {
            digest,
            records: count,
        });
        self.bytes += encoded.len();
        Ok(())
    }
    fn seal_prefix(&self, sequence: u64, records: u64, digest: [u8; 32]) -> Result<Watermark> {
        let mut watermark = Watermark {
            schema: 1,
            origin: self.origin,
            identity: self.identity_tag,
            sequence,
            records,
            commitment: vec![],
        };
        watermark.commitment = self.cipher.seal_frame(&aad(&watermark)?, &digest);
        Ok(watermark)
    }
    fn check_prefix(
        &self,
        w: &Watermark,
        sequence: u64,
        records: u64,
        digest: [u8; 32],
    ) -> Result<()> {
        if w.schema != 1
            || w.origin != self.origin
            || w.identity != self.identity_tag
            || w.sequence != sequence
            || w.records != records
            || w.commitment.len() > 256
        {
            return Err(Error::Authority);
        }
        let plain = self
            .cipher
            .open_frame(&aad(w)?, &w.commitment)
            .map_err(|_| Error::Authority)?;
        if plain.as_slice() != digest {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub(super) fn tip(&self) -> Result<Watermark> {
        self.healthy()?;
        let prefix = self.prefixes.last().ok_or(Error::Authority)?;
        self.seal_prefix(
            (self.prefixes.len() - 1) as u64,
            prefix.records,
            prefix.digest,
        )
    }
    pub(super) fn validate(&self, w: &Watermark) -> Result<usize> {
        self.healthy()?;
        let prefix = self
            .prefixes
            .get(usize::try_from(w.sequence).map_err(|_| Error::Authority)?)
            .ok_or(Error::Authority)?;
        self.check_prefix(w, w.sequence, prefix.records, prefix.digest)?;
        usize::try_from(prefix.records).map_err(|_| Error::Authority)
    }
    pub(super) fn seal(&self, aad: &[u8], plain: &[u8]) -> Vec<u8> {
        self.cipher.seal_frame(aad, plain)
    }
    pub(super) fn unseal(&self, aad: &[u8], cipher: &[u8]) -> Result<Vec<u8>> {
        self.cipher
            .open_frame(aad, cipher)
            .map_err(|_| Error::Authority)
    }
}
fn carrier(bytes: &[u8]) -> NewRecord<'_> {
    NewRecord {
        kind: 7,
        valid_at: 0,
        recorded_at: 0,
        key: b"",
        attrs: b"",
        text: bytes,
    }
}
pub(super) fn chain(previous: [u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"oojuro/record-chain/v1\0");
    h.update(&previous);
    h.update(bytes);
    *h.finalize().as_bytes()
}
pub(super) fn aad(w: &Watermark) -> Result<Vec<u8>> {
    let mut bytes = PREFIX_AAD.to_vec();
    bytes.extend(
        serde_json::to_vec(&(w.schema, w.origin, w.identity, w.sequence, w.records))
            .map_err(|_| Error::Authority)?,
    );
    Ok(bytes)
}
fn now() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Unavailable)?
            .as_micros(),
    )
    .map_err(|_| Error::Bounds)
}
fn namespace(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::Invalid);
        }
    } else {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(|_| Error::Unavailable)?;
    }
    let marker = dir.join("RECORD_FORMAT");
    if let Ok(meta) = fs::symlink_metadata(&marker) {
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || fs::read(&marker).map_err(|_| Error::Unavailable)? != FORMAT
        {
            return Err(Error::Authority);
        }
    } else {
        if fs::read_dir(dir)
            .map_err(|_| Error::Unavailable)?
            .next()
            .is_some()
        {
            return Err(Error::Authority);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&marker)
            .map_err(|_| Error::Unavailable)?;
        file.write_all(FORMAT)
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::Unavailable)?;
        fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
    }
    Ok(())
}
