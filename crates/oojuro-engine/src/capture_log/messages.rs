//! Explicit byte-message authority. This never interprets arbitrary protocol bodies as source lines.
//! The caller retains prepared bytes/metadata after any refusal or unknown outcome; no hidden spool.
use super::*;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
};
pub(super) const FORMAT: &[u8] = b"oojuro-capture-messages-1\n";
const MARKER: &str = "MESSAGE_FORMAT";
const KEY_DOMAIN: &[u8] = b"oojuro/capture-messages/store-key/v1\0";
pub const MAX_UNITS: usize = 1024;
pub const MAX_REQUEST_METADATA: usize = 4096;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub sequence: u64,
    /// Immutable protocol metadata and body boundaries, not public evidence.
    pub metadata: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageKind {
    Parsed,
    Malformed,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageUnit {
    pub start: u64,
    pub end: u64,
    pub kind: MessageKind,
    pub emissions: Vec<Decision>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagePolicy {
    pub policy_id: String,
    pub raw_authorization: String,
    pub messages: Vec<MessageUnit>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedMessage {
    pub source: Source,
    pub raw_start: u64,
    pub request: Request,
    pub outcome: Outcome,
    pub policy: MessagePolicy,
    pub context: Vec<u8>,
    /// Legacy line_start/end fields are source BYTE offsets in this framing.
    pub events: Vec<Event>,
}
impl ParsedMessage {
    pub(super) fn normalized(&self) -> ParsedBatch {
        ParsedBatch {
            source: self.source,
            raw_start: self.raw_start,
            parsed_before: self.raw_start,
            outcome: self.outcome.clone(),
            policy: PolicyReceipt {
                policy_id: self.policy.policy_id.clone(),
                raw_authorization: self.policy.raw_authorization.clone(),
                lines: self
                    .policy
                    .messages
                    .iter()
                    .map(|unit| Line {
                        start: unit.start,
                        end: unit.end,
                        kind: match unit.kind {
                            MessageKind::Parsed => LineKind::Parsed,
                            MessageKind::Malformed => LineKind::Malformed,
                        },
                        emissions: unit.emissions.clone(),
                    })
                    .collect(),
            },
            context: self.context.clone(),
            events: self.events.clone(),
        }
    }
}
pub struct MessageBatch {
    pub parsed: ParsedMessage,
    pub raw: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageAck {
    pub request_id: String,
    pub sequence: u64,
    pub raw_start: u64,
    pub outcome: Outcome,
    /// Historical exact ACK; current namespace progress may have advanced.
    pub already_committed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessagePosition {
    pub cursor: u64,
    pub sequence: u64,
    pub existing: Option<MessageAck>,
}
pub(super) struct Meta {
    source: Source,
    request: Request,
    raw_start: u64,
    batch: usize,
}
impl Meta {
    pub(super) fn new(p: &ParsedMessage, batch: usize) -> Self {
        Self {
            source: p.source,
            request: p.request.clone(),
            raw_start: p.raw_start,
            batch,
        }
    }
    fn ack(&self, log: &CaptureLog, already_committed: bool) -> Result<MessageAck> {
        Ok(MessageAck {
            request_id: self.request.id.clone(),
            sequence: self.request.sequence,
            raw_start: self.raw_start,
            outcome: log
                .batches
                .get(self.batch)
                .ok_or(Error::Corrupt("message receipt index"))?
                .outcome
                .clone(),
            already_committed,
        })
    }
}
/// A separate fresh mode over the same sealed Store/commit authority, never a second WAL.
pub struct MessageLog {
    inner: CaptureLog,
    #[cfg(test)]
    hook: Option<Box<dyn Fn(Phase)>>,
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    BeforeWrite,
    AfterWrite,
    BeforeAck,
}

impl MessageLog {
    pub fn create(dir: &Path, key: &[u8; 32], identity: Identity) -> Result<Self> {
        match fs::symlink_metadata(dir) {
            Ok(_) => return Err(Error::Invalid("message create requires a new directory")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Invalid identity/options must not leave a newly enrolled root.
        validate_identity(&identity)?;
        fs::DirBuilder::new().mode(0o700).create(dir)?;
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(MARKER))?;
        marker.write_all(FORMAT)?;
        marker.sync_all()?;
        fs::File::open(dir)?.sync_all()?;
        fs::File::open(dir.parent().ok_or(Error::Invalid("message parent"))?)?.sync_all()?;
        Ok(Self {
            inner: CaptureLog::open_framed(dir, key, identity, Framing::Messages, true)?,
            #[cfg(test)]
            hook: None,
        })
    }
    /// Requires the original marker and Store manifest. Missing/partial roots never initialize.
    pub fn open(dir: &Path, key: &[u8; 32], identity: Identity) -> Result<Self> {
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::Invalid("message directory"));
        }
        let marker = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join(MARKER))?;
        if !marker.metadata()?.is_file() || marker.metadata()?.len() > 128 {
            return Err(Error::Invalid("message marker"));
        }
        let mut bytes = vec![];
        marker.take(129).read_to_end(&mut bytes)?;
        if bytes != FORMAT {
            return Err(Error::Invalid("message marker mismatch"));
        }
        let manifest = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("MANIFEST"))?;
        if !manifest.metadata()?.is_file() {
            return Err(Error::Invalid("message manifest"));
        }
        Ok(Self {
            inner: CaptureLog::open_framed(dir, key, identity, Framing::Messages, false)?,
            #[cfg(test)]
            hook: None,
        })
    }
    pub fn authority(&self) -> Result<&CaptureLog> {
        self.inner.healthy()?;
        Ok(&self.inner)
    }
    pub fn register(&mut self, source: Source, descriptor: &[u8], context: &[u8]) -> Result<()> {
        self.inner.register(source, descriptor, context)
    }
    pub fn progress(&self, source: Source) -> Result<Progress> {
        self.inner.progress(source)
    }
    pub fn read_raw(&self, source: Source, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.inner.read_raw(source, offset, len)
    }
    pub fn position(&self, source: Source, request_id: &str) -> Result<MessagePosition> {
        self.inner.healthy()?;
        if !valid_id(request_id) {
            return Err(Error::Invalid("message request ID"));
        }
        let cursor = match self.inner.sources.get(&source) {
            Some(s) => s.raw_end,
            None => 0,
        };
        let sequence = self
            .inner
            .message_meta
            .iter()
            .rev()
            .find(|m| m.source == source)
            .map_or(0, |m| m.request.sequence);
        let existing = self
            .inner
            .message_meta
            .iter()
            .find(|m| m.source == source && m.request.id == request_id)
            .map(|m| m.ack(&self.inner, true))
            .transpose()?;
        Ok(MessagePosition {
            cursor,
            sequence,
            existing,
        })
    }
    /// Checks original intent without asking the current parser or policy to interpret anything.
    pub fn lookup(
        &self,
        source: Source,
        raw_start: u64,
        request: &Request,
        raw: &[u8],
    ) -> Result<Option<MessageAck>> {
        self.inner.healthy()?;
        validate_intent(request, raw)?;
        let Some(old) = self
            .inner
            .message_meta
            .iter()
            .find(|m| m.source == source && m.request.id == request.id)
        else {
            return Ok(None);
        };
        let ack = old.ack(&self.inner, true)?;
        let length = usize::try_from(
            ack.outcome
                .raw_end
                .checked_sub(old.raw_start)
                .ok_or(Error::Corrupt("message raw receipt"))?,
        )
        .map_err(|_| Error::Invalid("message length"))?;
        if old.request != *request
            || old.raw_start != raw_start
            || length != raw.len()
            || self.inner.read_raw(source, raw_start, length)? != raw
        {
            return Err(Error::Invalid("message original intent differs"));
        }
        Ok(Some(ack))
    }
    pub fn commit(&mut self, batch: MessageBatch) -> Result<MessageAck> {
        if let Some(old) = self.lookup(
            batch.parsed.source,
            batch.parsed.raw_start,
            &batch.parsed.request,
            &batch.raw,
        )? {
            return Ok(old);
        }
        self.inner.validate_request(&batch.parsed, &batch.raw)?;
        valid_context(&batch.parsed.context)?;
        let metadata = bounded_json(&batch.parsed, MAX_BATCH_BYTES)?;
        self.inner
            .validate(&batch.parsed.normalized(), &batch.raw, metadata.len())?;
        let mut carrier = Vec::with_capacity(5 + metadata.len() + batch.raw.len());
        carrier.push(3);
        carrier.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        carrier.extend(metadata);
        carrier.extend(batch.raw);
        // Store compresses this complete carrier before its scoped-key frame AEAD.
        // The Store head authenticates physical sequence/ranges; the KDF binds framing+Identity.
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(Phase::BeforeWrite);
        }
        let seq = self.inner.write(&carrier)?;
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(Phase::AfterWrite);
        }
        if let Err(error) = self.inner.replay(seq, &carrier) {
            self.inner.failed = true;
            return Err(error);
        }
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(Phase::BeforeAck);
        }
        self.inner
            .message_meta
            .last()
            .ok_or(Error::Corrupt("missing message ACK"))?
            .ack(&self.inner, false)
    }
    pub fn compact(&mut self) -> Result<()> {
        self.inner.compact()
    }
}
fn bounded_json<T: Serialize + ?Sized>(value: &T, maximum: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        maximum: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.maximum)
            {
                return Err(std::io::Error::other("message metadata bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: vec![],
        maximum,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| Error::Invalid("message metadata encoding or bound"))?;
    Ok(writer.bytes)
}
fn validate_identity(identity: &Identity) -> Result<()> {
    if identity.writer.is_empty()
        || identity.writer.len() > 256
        || identity.agent == 0
        || identity.first_id == 0
        || identity.adapter.is_empty()
        || identity.adapter.len() > 128
    {
        return Err(Error::Invalid("invalid capture identity"));
    }
    Ok(())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
}
fn validate_intent(request: &Request, raw: &[u8]) -> Result<()> {
    if !valid_id(&request.id)
        || request.sequence == 0
        || request.metadata.len() > MAX_REQUEST_METADATA
        || raw.len() > MAX_BLOCK_BYTES
    {
        return Err(Error::Invalid("message intent or byte bound"));
    }
    if !serde_json::from_slice::<serde_json::Value>(&request.metadata).is_ok_and(|v| v.is_object())
    {
        return Err(Error::Invalid("message metadata object"));
    }
    Ok(())
}
pub(super) fn only_marker(dir: &Path) -> Result<bool> {
    let mut entries = fs::read_dir(dir)?;
    let Some(entry) = entries.next() else {
        return Ok(false);
    };
    let entry = entry?;
    Ok(entry.file_name() == MARKER && entry.file_type()?.is_file() && entries.next().is_none())
}
pub(super) fn cipher(key: &[u8; 32], identity: &Identity) -> Result<FrameCipher> {
    let mut purpose = KEY_DOMAIN.to_vec();
    purpose.extend(serde_json::to_vec(identity).map_err(|_| Error::Invalid("message identity"))?);
    let mut derived = *blake3::keyed_hash(key, &purpose).as_bytes();
    let cipher = FrameCipher::new(&derived);
    derived.fill(0);
    std::hint::black_box(&derived);
    Ok(cipher)
}
impl CaptureLog {
    pub(super) fn validate_request(&self, p: &ParsedMessage, raw: &[u8]) -> Result<()> {
        self.healthy()?;
        validate_intent(&p.request, raw)?;
        if self.framing != Framing::Messages
            || p.policy.messages.is_empty()
            || p.policy.messages.len() > MAX_UNITS
            || self
                .message_meta
                .iter()
                .any(|m| m.source == p.source && m.request.id == p.request.id)
        {
            return Err(Error::Invalid("message source request or units"));
        }
        let prior = self
            .message_meta
            .iter()
            .rev()
            .find(|m| m.source == p.source)
            .map_or(0, |m| m.request.sequence);
        if prior.checked_add(1) != Some(p.request.sequence) {
            return Err(Error::Invalid("message sequence"));
        }
        Ok(())
    }
}
#[path = "messages/view.rs"]
mod view;
pub use view::{
    MessageOutput, MessagePage, MessageReceipt, MessageReceiptView, MessageStats, MessageWatermark,
};
#[cfg(test)]
#[path = "messages/tests.rs"]
mod tests;
