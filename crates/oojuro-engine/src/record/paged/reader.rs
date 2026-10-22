use super::*;
use crate::capture_log::Identity;
use crate::record::authority::{self, Batch, Unsigned};
use oojuro_store::{Options, Store, crypto::FrameCipher};
use std::{
    collections::{BTreeSet, BinaryHeap},
    fs,
    path::Path,
    sync::Arc,
};
const CURSOR_AAD: &[u8] = b"oojuro/record-paged/cursor/v1\0";
const MAX_CURSOR: usize = 16 << 10;
struct Intent([u8; 32]);
impl Drop for Intent {
    fn drop(&mut self) {
        for b in &mut self.0 {
            unsafe { std::ptr::write_volatile(b, 0) }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}
struct Ref {
    records: u64,
    last_at: Option<i64>,
    digest: [u8; 32],
    request: [u8; 32],
}
/// Owning/recovering Store reader, not a filesystem read-only open. No data writes/ACK API.
pub struct Reader {
    store: Store,
    cipher: Arc<FrameCipher>,
    intent: Intent,
    identity: Identity,
    origin: [u8; 32],
    identity_tag: [u8; 32],
    refs: Vec<Ref>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    watermark: Watermark,
    writer: String,
    owner: u32,
    as_of: AsOf,
}
impl Snapshot {
    pub fn watermark(&self) -> &Watermark {
        &self.watermark
    }
    pub(super) fn as_of(&self) -> AsOf {
        self.as_of
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "read", deny_unknown_fields)]
enum Mode {
    History { key: String },
    Range { versions: Versions, filter: Filter },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    format: u32,
    snapshot: Snapshot,
    mode: Mode,
    after: (i64, i64, u64),
}
impl Reader {
    pub fn open(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        minimum: Option<&Watermark>,
    ) -> Result<Self> {
        if !name(&identity.writer, 256)
            || identity.agent == 0
            || identity.first_id == 0
            || !name(&identity.adapter, 128)
        {
            return Err(Error::Invalid);
        }
        let metadata = fs::symlink_metadata(dir).map_err(|_| Error::Unavailable)?;
        let marker = dir.join("RECORD_FORMAT");
        let m = fs::symlink_metadata(&marker).map_err(|_| Error::Authority)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !m.is_file()
            || m.file_type().is_symlink()
            || fs::read(marker).map_err(|_| Error::Unavailable)? != authority::FORMAT
        {
            return Err(Error::Authority);
        }
        let cipher = Arc::new(FrameCipher::new(key));
        let store = Store::open(
            dir,
            Options {
                cipher: Some(cipher.clone()),
                require_sealed: true,
                sync: true,
                compact_threads: 1,
                cache_bytes: 1 << 20,
                ..Options::default()
            },
        )
        .map_err(|_| Error::Unavailable)?;
        if store.is_empty() {
            return Err(Error::Authority);
        }
        let mut header = authority::FORMAT.to_vec();
        header.extend(serde_json::to_vec(&identity).map_err(|_| Error::Invalid)?);
        if store.get(0).map_err(|_| Error::Unavailable)? != header {
            return Err(Error::Authority);
        }
        let origin = store
            .sealed_origin_digest()
            .map_err(|_| Error::Unavailable)?;
        let intent = Intent(blake3::derive_key("oojuro direct record intent v1", key));
        let identity_tag = *blake3::keyed_hash(&intent.0, &header).as_bytes();
        let bytes = header.len();
        let mut reader = Self {
            store,
            cipher,
            intent,
            identity,
            origin,
            identity_tag,
            refs: vec![Ref {
                records: 0,
                last_at: None,
                digest: *blake3::hash(&header).as_bytes(),
                request: [0; 32],
            }],
        };
        let mut total = bytes;
        let mut requests = BTreeSet::new();
        for sequence in 1..reader.store.len() {
            let encoded = reader.store.get(sequence).map_err(|_| Error::Unavailable)?;
            total = total.checked_add(encoded.len()).ok_or(Error::Bounds)?;
            if total > MAX_STATE_BYTES {
                return Err(Error::Bounds);
            }
            let (batch, digest) = reader.verify(sequence, &encoded)?;
            if !requests.insert(batch.request) {
                return Err(Error::Authority);
            }
            let previous = reader.refs.last().ok_or(Error::Authority)?;
            reader.refs.push(Ref {
                records: previous.records + batch.assigned.len() as u64,
                last_at: batch.assigned.last().map(|r| r.recorded_at),
                digest,
                request: batch.request,
            });
        }
        if let Some(w) = minimum {
            reader.validate(w)?;
        }
        Ok(reader)
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn locator_count(&self) -> usize {
        self.refs.len()
    }

    fn tag<T: Serialize + ?Sized>(&self, domain: &str, value: &T) -> Result<[u8; 32]> {
        let mut bytes = json(&(domain, value), MAX_BATCH_BYTES)?;
        let out = *blake3::keyed_hash(&self.intent.0, &bytes).as_bytes();
        for b in &mut bytes {
            unsafe { std::ptr::write_volatile(b, 0) }
        }
        Ok(out)
    }
    fn verify(&self, sequence: u64, encoded: &[u8]) -> Result<(Batch, [u8; 32])> {
        if encoded.len() > MAX_BATCH_BYTES {
            return Err(Error::Bounds);
        }
        let batch: Batch = serde_json::from_slice(encoded).map_err(|_| Error::Authority)?;
        let previous = self
            .refs
            .get(sequence.checked_sub(1).ok_or(Error::Authority)? as usize)
            .ok_or(Error::Authority)?;
        if batch.schema != 2
            || batch.assigned.is_empty()
            || batch.assigned.len() > 1000
            || previous
                .records
                .checked_add(batch.assigned.len() as u64)
                .is_none_or(|n| n > MAX_RECORDS as u64)
        {
            return Err(Error::Authority);
        }
        let mut id = self
            .identity
            .first_id
            .checked_add(previous.records)
            .ok_or(Error::Authority)?;
        let mut last = previous.last_at;
        for row in &batch.assigned {
            row.input.validate().map_err(|_| Error::Authority)?;
            if row.id != id || last.is_some_and(|p| row.recorded_at <= p) {
                return Err(Error::Authority);
            }
            id = id.checked_add(1).ok_or(Error::Authority)?;
            last = Some(row.recorded_at);
        }
        let inputs: Vec<_> = batch.assigned.iter().map(|r| &r.input).collect();
        if self.tag("typed-input-v1", &inputs)? != batch.fingerprint {
            return Err(Error::Authority);
        }
        let canonical = json(
            &Unsigned {
                schema: 2,
                request: batch.request,
                fingerprint: batch.fingerprint,
                assigned: &batch.assigned,
            },
            MAX_BATCH_BYTES,
        )?;
        let digest = authority::chain(previous.digest, &canonical);
        self.check(
            &batch.watermark,
            sequence,
            previous.records + batch.assigned.len() as u64,
            digest,
        )?;
        Ok((batch, digest))
    }
    fn batch(&self, sequence: usize) -> Result<Batch> {
        let encoded = self
            .store
            .get(sequence as u64)
            .map_err(|_| Error::Unavailable)?;
        let (batch, digest) = self.verify(sequence as u64, &encoded)?;
        let expected = self.refs.get(sequence).ok_or(Error::Authority)?;
        if expected.digest != digest || expected.request != batch.request {
            return Err(Error::Authority);
        }
        Ok(batch)
    }
    fn check(&self, w: &Watermark, sequence: u64, records: u64, digest: [u8; 32]) -> Result<()> {
        if w.schema != 1
            || w.origin != self.origin
            || w.identity != self.identity_tag
            || w.sequence != sequence
            || w.records != records
            || w.commitment.len() > 256
        {
            return Err(Error::Authority);
        }
        if self
            .cipher
            .open_frame(&authority::aad(w)?, &w.commitment)
            .map_err(|_| Error::Authority)?
            .as_slice()
            != digest
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub fn watermark(&self) -> Result<Watermark> {
        let sequence = self.refs.len() - 1;
        let r = &self.refs[sequence];
        let mut w = Watermark {
            schema: 1,
            origin: self.origin,
            identity: self.identity_tag,
            sequence: sequence as u64,
            records: r.records,
            commitment: vec![],
        };
        w.commitment = self.cipher.seal_frame(&authority::aad(&w)?, &r.digest);
        Ok(w)
    }
    pub(super) fn validate(&self, w: &Watermark) -> Result<usize> {
        let r = self
            .refs
            .get(usize::try_from(w.sequence).map_err(|_| Error::Authority)?)
            .ok_or(Error::Authority)?;
        self.check(w, w.sequence, r.records, r.digest)?;
        usize::try_from(r.records).map_err(|_| Error::Bounds)
    }
    pub(super) fn seal(&self, aad: &[u8], bytes: &[u8]) -> Vec<u8> {
        self.cipher.seal_frame(aad, bytes)
    }
    pub(super) fn unseal(&self, aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
        self.cipher
            .open_frame(aad, bytes)
            .map_err(|_| Error::Authority)
    }
    pub fn receipt(&self, request: &str) -> Result<Option<Commit>> {
        if !name(request, 1024) {
            return Err(Error::Bounds);
        }
        let tag = self.tag("request", request)?;
        if let Some(sequence) = self
            .refs
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, r)| (r.request == tag).then_some(i))
        {
            let b = self.batch(sequence)?;
            return Ok(Some(Commit {
                ids: b.assigned.iter().map(|r| r.id).collect(),
                recorded_at: b.assigned.iter().map(|r| r.recorded_at).collect(),
                watermark: b.watermark,
            }));
        }
        Ok(None)
    }
    pub fn view(&self, scope: Scope<'_>, as_of: AsOf) -> Result<Snapshot> {
        if scope.writer != self.identity.writer || scope.owner != self.identity.agent {
            return Err(Error::Scope);
        }
        Ok(Snapshot {
            watermark: self.watermark()?,
            writer: scope.writer.into(),
            owner: scope.owner,
            as_of,
        })
    }
    pub(super) fn snapshot(&self, s: &Snapshot) -> Result<usize> {
        if s.writer != self.identity.writer || s.owner != self.identity.agent {
            return Err(Error::Scope);
        }
        self.validate(&s.watermark)
    }
    pub(super) fn scan(
        &self,
        w: &Watermark,
        start: usize,
        end: usize,
        mut visit: impl FnMut(&Assigned) -> Result<bool>,
    ) -> Result<()> {
        let count = self.validate(w)?;
        if start > end || end > count {
            return Err(Error::Bounds);
        }
        let mut sequence = self
            .refs
            .partition_point(|r| r.records <= start as u64)
            .max(1);
        while sequence <= w.sequence as usize && sequence < self.refs.len() {
            let before = self.refs[sequence - 1].records as usize;
            if before >= end {
                break;
            }
            let batch = self.batch(sequence)?;
            for (offset, row) in batch.assigned.iter().enumerate() {
                let at = before + offset;
                if at >= end {
                    return Ok(());
                }
                if at >= start && !visit(row)? {
                    return Ok(());
                }
            }
            sequence += 1;
        }
        Ok(())
    }
    pub(super) fn assigned(&self, w: &Watermark, id: u64) -> Result<Option<Assigned>> {
        let count = self.validate(w)?;
        let Some(at) = id
            .checked_sub(self.identity.first_id)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < count)
        else {
            return Ok(None);
        };
        let sequence = self.refs.partition_point(|r| r.records <= at as u64);
        let batch = self.batch(sequence)?;
        let offset = at - self.refs[sequence - 1].records as usize;
        Ok(Some(
            batch.assigned.get(offset).ok_or(Error::Authority)?.clone(),
        ))
    }
    pub(super) fn winner(&self, s: &Snapshot, key: &str) -> Result<Option<u64>> {
        let end = self.snapshot(s)?;
        let mut winner = None;
        self.scan(&s.watermark, 0, end, |r| {
            if r.input.key.as_deref() == Some(key)
                && r.input.valid_at <= s.as_of.valid_at
                && r.recorded_at <= s.as_of.known_at
            {
                let at = (r.input.valid_at, r.recorded_at, r.id);
                if winner.is_none_or(|old| at > old) {
                    winner = Some(at);
                }
            }
            Ok(true)
        })?;
        Ok(winner.map(|(_, _, id)| id))
    }
    pub(super) fn visible(&self, s: &Snapshot, r: &Assigned, filter: &Filter) -> Result<bool> {
        if r.input.valid_at > s.as_of.valid_at || r.recorded_at > s.as_of.known_at {
            return Ok(false);
        }
        if let Some(key) = &r.input.key
            && self.winner(s, key)? != Some(r.id)
        {
            return Ok(false);
        }
        Ok(r.input.valid_at >= s.as_of.from
            && filter.kind.is_none_or(|v| v == r.input.kind)
            && filter
                .tool
                .as_ref()
                .is_none_or(|v| Some(v) == r.input.tool.as_ref())
            && filter.status.is_none_or(|v| Some(v) == r.input.status)
            && filter
                .agent_id
                .as_ref()
                .is_none_or(|v| v == &r.input.agent_id))
    }
    pub fn get_id(&self, s: &Snapshot, id: u64, bytes: usize) -> Result<Option<Record>> {
        self.snapshot(s)?;
        check_bytes(bytes)?;
        let record = self
            .assigned(&s.watermark, id)?
            .filter(|r| {
                r.input.valid_at >= s.as_of.from
                    && r.input.valid_at <= s.as_of.valid_at
                    && r.recorded_at <= s.as_of.known_at
            })
            .map(|r| r.record(&self.identity.writer, &self.identity.adapter));
        json(&record, bytes)?;
        Ok(record)
    }
    pub fn get_key(&self, s: &Snapshot, key: &str, bytes: usize) -> Result<Option<Record>> {
        if !name(key, 1024) {
            return Err(Error::Bounds);
        }
        check_bytes(bytes)?;
        let id = self.winner(s, key)?;
        if let Some(id) = id {
            self.get_id(s, id, bytes)
        } else {
            json(&Option::<Record>::None, bytes)?;
            Ok(None)
        }
    }
    pub fn history(&self, s: &Snapshot, key: &str, limits: Limits) -> Result<Page> {
        if !name(key, 1024) {
            return Err(Error::Bounds);
        }
        self.page(s, Mode::History { key: key.into() }, None, limits)
    }
    pub fn range(
        &self,
        s: &Snapshot,
        versions: Versions,
        filter: Filter,
        limits: Limits,
    ) -> Result<Page> {
        filter.validate()?;
        self.page(s, Mode::Range { versions, filter }, None, limits)
    }
    pub fn continue_page(&self, scope: Scope<'_>, cursor: &str, limits: Limits) -> Result<Page> {
        let encoded = unhex(cursor)?;
        let cursor: Cursor = serde_json::from_slice(&self.unseal(CURSOR_AAD, &encoded)?)
            .map_err(|_| Error::Invalid)?;
        if cursor.format != 1
            || cursor.snapshot.writer != scope.writer
            || cursor.snapshot.owner != scope.owner
        {
            return Err(Error::Invalid);
        }
        self.page(&cursor.snapshot, cursor.mode, Some(cursor.after), limits)
    }
    fn page(
        &self,
        s: &Snapshot,
        mode: Mode,
        after: Option<(i64, i64, u64)>,
        limits: Limits,
    ) -> Result<Page> {
        let end = self.snapshot(s)?;
        check_bytes(limits.bytes)?;
        if limits.entries == 0 || limits.entries > 1000 {
            return Err(Error::Bounds);
        }
        match &mode {
            Mode::History { key } if !name(key, 1024) => return Err(Error::Bounds),
            Mode::Range { filter, .. } => filter.validate()?,
            _ => (),
        };
        let mut selected = BinaryHeap::new();
        let mut seen = 0usize;
        self.scan(&s.watermark, 0, end, |r| {
            let keep = match &mode {
                Mode::History { key } => {
                    r.input.key.as_ref() == Some(key)
                        && r.input.valid_at >= s.as_of.from
                        && r.input.valid_at <= s.as_of.valid_at
                        && r.recorded_at <= s.as_of.known_at
                }
                Mode::Range {
                    versions: Versions::Current,
                    filter,
                } => self.visible(s, r, filter)?,
                Mode::Range {
                    versions: Versions::All,
                    filter,
                } => {
                    r.input.valid_at >= s.as_of.from
                        && r.input.valid_at <= s.as_of.valid_at
                        && r.recorded_at <= s.as_of.known_at
                        && filter.kind.is_none_or(|v| v == r.input.kind)
                        && filter
                            .tool
                            .as_ref()
                            .is_none_or(|v| Some(v) == r.input.tool.as_ref())
                        && filter.status.is_none_or(|v| Some(v) == r.input.status)
                        && filter
                            .agent_id
                            .as_ref()
                            .is_none_or(|v| v == &r.input.agent_id)
                }
            };
            let key = (r.input.valid_at, r.recorded_at, r.id);
            if keep && after.is_none_or(|old| key > old) {
                seen = seen.checked_add(1).ok_or(Error::Bounds)?;
                selected.push(key);
                if selected.len() > limits.entries {
                    selected.pop();
                }
            }
            Ok(true)
        })?;
        let selected = selected.into_sorted_vec();
        let mut rows = Vec::new();
        let mut serialized = 0usize;
        for (_, _, id) in &selected {
            let r = self
                .assigned(&s.watermark, *id)?
                .ok_or(Error::Authority)?
                .record(&self.identity.writer, &self.identity.adapter);
            let n = json(&r, MAX_OUTPUT_BYTES)?.len();
            if serialized.checked_add(n).is_none_or(|n| n > limits.bytes) {
                break;
            }
            serialized += n;
            rows.push(r);
        }
        if !selected.is_empty() && rows.is_empty() {
            return Err(Error::Bounds);
        }
        loop {
            let more = seen > rows.len();
            let next = if more {
                let r = rows.last().ok_or(Error::Bounds)?;
                let c = Cursor {
                    format: 1,
                    snapshot: s.clone(),
                    mode: mode.clone(),
                    after: (r.valid_at, r.recorded_at, r.id),
                };
                let bytes = json(&c, MAX_CURSOR / 2)?;
                let encoded = hex(&self.seal(CURSOR_AAD, &bytes));
                if encoded.len() > MAX_CURSOR {
                    return Err(Error::Bounds);
                }
                Some(encoded)
            } else {
                None
            };
            let page = Page {
                watermark: s.watermark.clone(),
                records: rows,
                next,
            };
            match json(&page, limits.bytes) {
                Ok(_) => return Ok(page),
                Err(Error::Bounds) if !page.records.is_empty() => {
                    rows = page.records;
                    rows.pop();
                    if rows.is_empty() {
                        return Err(Error::Bounds);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}
fn check_bytes(bytes: usize) -> Result<()> {
    if bytes == 0 || bytes > MAX_OUTPUT_BYTES {
        Err(Error::Bounds)
    } else {
        Ok(())
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.is_empty() || s.len() > MAX_CURSOR || !s.len().is_multiple_of(2) {
        return Err(Error::Invalid);
    }
    let digit = |c| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(Error::Invalid),
    };
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Ok(digit(p[0])? * 16 + digit(p[1])?))
        .collect()
}
