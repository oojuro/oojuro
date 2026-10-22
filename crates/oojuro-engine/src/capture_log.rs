//! Native atomic raw + authorized parsed authority over the existing sealed Store.
//! No adapter, filesystem discovery, query engine, key persistence or second WAL lives here.
//! The format is distinct from raw::RawJournal; neither silently upgrades the other's files.
pub use crate::raw::MAX_BLOCK_BYTES;
/// Explicit direct-record namespace; never opens or converts a v1 capture log.
pub use crate::record::RecordLog;
pub mod messages;

use oojuro_store::{Error, NewRecord, Options, Result, Store, crypto::FrameCipher};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc};

const FORMAT: &[u8] = b"oojuro-capture-1\n";
/// Authenticated raw coverage contract. Existing line roots never accept messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Framing {
    #[serde(rename = "lines_v1")]
    Lines,
    #[serde(rename = "messages_v1")]
    Messages,
}

pub const MAX_LINE_BYTES: usize = 1 << 20;
pub const MAX_CONTEXT_BYTES: usize = 128 << 10;
pub const MAX_BATCH_BYTES: usize = 8 << 20;
pub const MAX_EVENTS: usize = 100_000;
/// Aggregate serialized registration and parsed-receipt metadata; raw bytes excluded.
/// This bounds replay state, not the process's exact resident memory.
pub const MAX_PARSED_BYTES: usize = 128 << 20;
pub type Source = [u8; 16];

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub writer: String,
    pub agent: u32,
    pub first_id: u64,
    pub adapter: String,
}
/// Complete owned EventIn fields, including eligibility and replay-only fields.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: u64,
    pub session: u64,
    pub parent_session: u64,
    pub agent: u32,
    pub turn: u32,
    pub seq: u32,
    pub parent: u64,
    pub revises: u64,
    pub kind: u8,
    pub status: u8,
    pub exit_code: Option<i32>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub ts_us: i64,
    pub duration_us: i64,
    pub tool: String,
    pub args: String,
    pub text: String,
    pub indexed: bool,
    pub line_start: u64,
    pub line_end: u64,
    pub emission: u32,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum Decision {
    Stored { id: u64, indexed: bool },
    Withheld { reason: String },
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LineKind {
    Parsed,
    Blank,
    Malformed,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Line {
    pub start: u64,
    pub end: u64,
    pub kind: LineKind,
    pub emissions: Vec<Decision>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyReceipt {
    pub policy_id: String,
    pub raw_authorization: String,
    pub lines: Vec<Line>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub raw_end: u64,
    pub parsed_end: u64,
    pub first_id: Option<u64>,
    pub stored: u64,
    pub next_id: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedBatch {
    pub source: Source,
    pub raw_start: u64,
    pub parsed_before: u64,
    pub outcome: Outcome,
    pub policy: PolicyReceipt,
    /// Opaque adapter state, SEALED with raw and receipts; never a plaintext sidecar.
    pub context: Vec<u8>,
    pub events: Vec<Event>,
}
pub struct Batch {
    pub parsed: ParsedBatch,
    pub raw: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    source: Source,
    descriptor: Vec<u8>,
    context: Vec<u8>,
}
struct Block {
    seq: u64,
    offset: u64,
    len: usize,
    at: usize,
}
struct SourceState {
    initial_context: Vec<u8>,
    descriptor: Vec<u8>,
    context: Vec<u8>,
    raw_end: u64,
    parsed_end: u64,
    file_no: u32,
    tail_policy: Option<String>,
    blocks: Vec<Block>,
}
#[derive(Clone)]
pub struct Progress {
    pub raw_end: u64,
    pub parsed_end: u64,
    pub file_no: u32,
    pub tail_policy: Option<String>,
    pub context: Vec<u8>,
}
/// Checked committed record prefix, including header, registrations, raw bytes
/// and exact authorized parsed receipts. Not an independent durable authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CachePrefix {
    pub records: u64,
    pub batches: usize,
    pub digest: [u8; 32],
}
pub struct CaptureLog {
    store: Store,
    cipher: Arc<FrameCipher>,
    ingest_binding: [u8; 32],
    prefixes: Vec<CachePrefix>,
    instance: u64,
    identity: Identity,
    sources: BTreeMap<Source, SourceState>,
    batches: Vec<ParsedBatch>,
    ids: BTreeMap<u64, (usize, usize)>,
    next_id: u64,
    metadata_bytes: usize,
    failed: bool,
    framing: Framing,
    message_meta: Vec<messages::Meta>,
}
impl CaptureLog {
    pub fn open(dir: &Path, key: &[u8; 32], identity: Identity) -> Result<Self> {
        Self::open_framed(dir, key, identity, Framing::Lines, true)
    }
    /// Existing namespace only; recovery is permitted, automatic bootstrap is not.
    pub fn open_existing(dir: &Path, key: &[u8; 32], identity: Identity) -> Result<Self> {
        crate::existing::check(dir, None)
            .map_err(|_| Error::Invalid("existing capture authority required"))?;
        Self::open_framed(dir, key, identity, Framing::Lines, false)
    }
    fn open_framed(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        framing: Framing,
        initialize: bool,
    ) -> Result<Self> {
        if identity.writer.is_empty()
            || identity.writer.len() > 256
            || identity.agent == 0
            || identity.first_id == 0
            || identity.adapter.is_empty()
            || identity.adapter.len() > 128
        {
            return Err(Error::Invalid("invalid capture identity"));
        }
        let parent = dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.is_dir() {
            return Err(Error::Invalid("capture parent must exist"));
        }
        if dir.is_dir()
            && !dir.join("MANIFEST").exists()
            && (framing == Framing::Lines && std::fs::read_dir(dir)?.next().is_some()
                || framing == Framing::Messages && !messages::only_marker(dir)?)
        {
            return Err(Error::Invalid(
                "capture authority requires a dedicated directory",
            ));
        }
        let cipher = Arc::new(match framing {
            Framing::Lines => FrameCipher::new(key),
            Framing::Messages => messages::cipher(key, &identity)?,
        });
        let store = Store::open(
            dir,
            Options {
                cipher: Some(cipher.clone()),
                require_sealed: true,
                sync: true,
                compact_threads: 1,
                ..Options::default()
            },
        )?;
        let mut log = Self {
            store,
            cipher,
            ingest_binding: [0; 32],
            prefixes: Vec::new(),
            instance: next_instance()?,
            next_id: identity.first_id,
            identity,
            sources: BTreeMap::new(),
            batches: Vec::new(),
            ids: BTreeMap::new(),
            metadata_bytes: 0,
            failed: false,
            framing,
            message_meta: Vec::new(),
        };
        let mut header = match framing {
            Framing::Lines => FORMAT,
            Framing::Messages => messages::FORMAT,
        }
        .to_vec();
        header.extend(
            serde_json::to_vec(&log.identity).map_err(|_| Error::Invalid("capture identity"))?,
        );
        if log.store.is_empty() {
            if !initialize {
                return Err(Error::Corrupt(match framing {
                    Framing::Lines => "existing capture header required",
                    Framing::Messages => "missing enrolled capture header",
                }));
            }
            log.write(&header)?;
        } else if log.store.get(0)? != header {
            return Err(Error::Corrupt("capture format or identity mismatch"));
        }
        // Stable source namespace, not fresh-store enrollment or a cache incarnation.
        // Same key/header/generation means SAME source; changed input must conflict.
        let mut binding = b"oojuro/capture/ingest-binding/lines-v1\0".to_vec();
        binding.extend_from_slice(&header);
        log.ingest_binding = *blake3::keyed_hash(key, &binding).as_bytes();
        log.add_prefix(0, &header)?;
        for seq in 1..log.store.len() {
            let bytes = log.store.get(seq)?;
            log.replay(seq, &bytes)?;
        }
        std::fs::File::open(parent)?.sync_all()?;
        Ok(log)
    }
    /// Engine-private typed Capture1 identity; no key getter or public plaintext hash.
    pub(crate) fn ingest_binding(&self) -> Result<[u8; 32]> {
        self.healthy()?;
        Ok(self.ingest_binding)
    }
    pub(crate) fn cache_cipher(&self) -> Arc<FrameCipher> {
        self.cipher.clone()
    }
    pub(crate) fn cache_origin(&self) -> Result<[u8; 32]> {
        self.healthy()?;
        self.store.sealed_origin_digest()
    }
    pub(crate) fn cache_prefix(&self, records: u64) -> Result<CachePrefix> {
        self.healthy()?;
        self.prefixes
            .get(
                records
                    .checked_sub(1)
                    .ok_or(Error::Invalid("empty capture prefix"))? as usize,
            )
            .cloned()
            .ok_or(Error::Invalid("capture prefix beyond authority"))
    }
    pub(crate) fn cache_tip(&self) -> Result<CachePrefix> {
        self.healthy()?;
        if self.prefixes.len() as u64 != self.store.len() {
            return Err(Error::Corrupt("capture prefix coverage"));
        }
        self.prefixes
            .last()
            .cloned()
            .ok_or(Error::Corrupt("capture prefix missing"))
    }
    fn add_prefix(&mut self, seq: u64, bytes: &[u8]) -> Result<()> {
        if seq != self.prefixes.len() as u64 {
            return Err(Error::Corrupt("capture prefix sequence"));
        }
        let mut h = blake3::Hasher::new();
        h.update(b"oojuro/capture-prefix/v1\0");
        h.update(&self.prefixes.last().map_or([0; 32], |p| p.digest));
        h.update(&seq.to_le_bytes());
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
        self.prefixes.push(CachePrefix {
            records: seq + 1,
            batches: self.batches.len(),
            digest: *h.finalize().as_bytes(),
        });
        Ok(())
    }
    fn healthy(&self) -> Result<()> {
        if self.failed {
            Err(Error::Invalid("capture authority failed; reopen required"))
        } else {
            Ok(())
        }
    }
    fn write(&mut self, bytes: &[u8]) -> Result<u64> {
        self.healthy()?;
        let result = self.store.append(&[NewRecord {
            kind: 7,
            valid_at: 0,
            recorded_at: 0,
            key: b"",
            attrs: b"",
            text: bytes,
        }]);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn next_id(&self) -> u64 {
        self.next_id
    }
    pub fn framing(&self) -> Framing {
        self.framing
    }
    pub fn batches(&self) -> Result<&[ParsedBatch]> {
        if self.framing != Framing::Lines {
            return Err(Error::Invalid(
                "message receipts require MessageReceiptView",
            ));
        }
        self.normalized_batches()
    }
    pub(crate) fn normalized_batches(&self) -> Result<&[ParsedBatch]> {
        self.healthy()?;
        Ok(&self.batches)
    }
    pub(crate) fn instance(&self) -> u64 {
        self.instance
    }
    pub fn sources(&self) -> impl Iterator<Item = Source> + '_ {
        self.sources.keys().copied()
    }
    /// Original opaque adapter receipt, retained for strict adapter replay validation.
    pub fn initial_context(&self, source: Source) -> Result<&[u8]> {
        self.healthy()?;
        Ok(&self
            .sources
            .get(&source)
            .ok_or(Error::NotFound)?
            .initial_context)
    }
    /// Opaque adapter descriptor for the caller-owned local authority; never an access grant.
    pub fn source_descriptor(&self, source: Source) -> Result<Option<&[u8]>> {
        self.healthy()?;
        Ok(self.sources.get(&source).map(|s| s.descriptor.as_slice()))
    }
    pub fn progress(&self, source: Source) -> Result<Progress> {
        self.healthy()?;
        let s = self.sources.get(&source).ok_or(Error::NotFound)?;
        Ok(Progress {
            raw_end: s.raw_end,
            parsed_end: s.parsed_end,
            file_no: s.file_no,
            tail_policy: s.tail_policy.clone(),
            context: s.context.clone(),
        })
    }
    pub fn event(&self, id: u64) -> Result<Option<(Source, &Event)>> {
        self.healthy()?;
        Ok(self
            .ids
            .get(&id)
            .map(|&(b, e)| (self.batches[b].source, &self.batches[b].events[e])))
    }
    pub fn register(&mut self, source: Source, descriptor: &[u8], context: &[u8]) -> Result<()> {
        self.healthy()?;
        if let Some(old) = self.sources.get(&source) {
            return if old.descriptor == descriptor {
                Ok(())
            } else {
                Err(Error::Invalid("source generation descriptor changed"))
            };
        }
        valid_context(context)?;
        if descriptor.len() > crate::raw::MAX_DESCRIPTOR_BYTES {
            return Err(Error::Invalid("source descriptor bound"));
        }
        let record = Registration {
            source,
            descriptor: descriptor.to_vec(),
            context: context.to_vec(),
        };
        let mut bytes = vec![1];
        bytes.extend(
            serde_json::to_vec(&record).map_err(|_| Error::Invalid("source registration"))?,
        );
        self.metadata_end(bytes.len() - 1)?;
        let seq = self.write(&bytes)?;
        if let Err(e) = self.replay(seq, &bytes) {
            self.failed = true;
            return Err(e);
        }
        Ok(())
    }
    pub fn read_raw(&self, source: Source, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.healthy()?;
        if len > MAX_LINE_BYTES + MAX_BLOCK_BYTES {
            return Err(Error::Invalid("raw read bound"));
        }
        let s = self.sources.get(&source).ok_or(Error::NotFound)?;
        let end = offset
            .checked_add(len as u64)
            .ok_or(Error::Invalid("raw range overflow"))?;
        if end > s.raw_end {
            return Err(Error::Invalid("raw range beyond retained prefix"));
        }
        let mut out = Vec::with_capacity(len);
        for b in s
            .blocks
            .iter()
            .skip_while(|b| b.offset + b.len as u64 <= offset)
            .take_while(|b| b.offset < end)
        {
            let bytes = self.store.get(b.seq)?;
            let start = offset.saturating_sub(b.offset) as usize;
            let stop = (end - b.offset).min(b.len as u64) as usize;
            out.extend_from_slice(&bytes[b.at + start..b.at + stop]);
        }
        if out.len() != len {
            return Err(Error::Corrupt("raw block coverage"));
        }
        Ok(out)
    }
    /// Native authority commit: one bounded sealed Store frame; no cache side effects.
    pub fn commit(&mut self, batch: Batch) -> Result<Outcome> {
        self.healthy()?;
        if self.framing != Framing::Lines {
            return Err(Error::Invalid("message commit required"));
        }
        let meta = serde_json::to_vec(&batch.parsed)
            .map_err(|_| Error::Invalid("capture batch encoding"))?;
        if meta.len() > MAX_BATCH_BYTES {
            return Err(Error::Invalid("parsed batch bound"));
        }
        self.validate(&batch.parsed, &batch.raw, meta.len())?;
        let mut bytes = Vec::with_capacity(5 + meta.len() + batch.raw.len());
        bytes.push(2);
        bytes.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes.extend(meta);
        bytes.extend(batch.raw);
        let seq = self.write(&bytes)?;
        if let Err(e) = self.replay(seq, &bytes) {
            self.failed = true;
            return Err(e);
        }
        Ok(batch.parsed.outcome)
    }
    fn metadata_end(&self, additional: usize) -> Result<usize> {
        let end = self
            .metadata_bytes
            .checked_add(additional)
            .ok_or(Error::Invalid("capture metadata size overflow"))?;
        if end > MAX_PARSED_BYTES {
            return Err(Error::Invalid("capture replay metadata capacity"));
        }
        Ok(end)
    }
    fn validate(&self, p: &ParsedBatch, raw: &[u8], encoded_len: usize) -> Result<()> {
        valid_context(&p.context)?;
        let s = self
            .sources
            .get(&p.source)
            .ok_or(Error::Invalid("unregistered capture source"))?;
        let end = s
            .raw_end
            .checked_add(raw.len() as u64)
            .ok_or(Error::Invalid("raw offset overflow"))?;
        if raw.len() > MAX_BLOCK_BYTES
            || p.raw_start != s.raw_end
            || p.parsed_before != s.parsed_end
            || p.outcome.raw_end != end
            || p.outcome.parsed_end < s.parsed_end
            || p.outcome.parsed_end > end
            || end - p.outcome.parsed_end > MAX_LINE_BYTES as u64
            || s.tail_policy
                .as_ref()
                .is_some_and(|id| id != &p.policy.policy_id)
            || p.policy.policy_id.is_empty()
            || p.policy.policy_id.len() > 128
            || p.policy.raw_authorization.is_empty()
            || p.policy.raw_authorization.len() > 128
        {
            return Err(Error::Invalid("capture progress or policy receipt"));
        }
        if self.framing == Framing::Messages
            && (s.raw_end != s.parsed_end
                || p.outcome.parsed_end != end
                || p.policy.lines.is_empty()
                || p.policy.lines.len() > messages::MAX_UNITS)
        {
            return Err(Error::Invalid("message coverage or progress"));
        }
        let processed = p.outcome.parsed_end - s.parsed_end;
        if processed > (MAX_LINE_BYTES + MAX_BLOCK_BYTES) as u64 {
            return Err(Error::Invalid("parse work bound"));
        }
        let mut input =
            self.read_raw(p.source, s.parsed_end, (s.raw_end - s.parsed_end) as usize)?;
        input.extend_from_slice(raw);
        let mut at = s.parsed_end;
        let mut events = p.events.iter();
        let mut next = self.next_id;
        for line in &p.policy.lines {
            if line.start != at
                || (if self.framing == Framing::Lines {
                    line.end <= at
                } else {
                    line.end < at || (line.end == at && !line.emissions.is_empty())
                })
                || line.end > p.outcome.parsed_end
                || line.end - at > MAX_LINE_BYTES as u64
                || (self.framing == Framing::Lines
                    && (input[(line.end - s.parsed_end - 1) as usize] != b'\n'
                        || input[(line.start - s.parsed_end) as usize
                            ..(line.end - s.parsed_end - 1) as usize]
                            .contains(&b'\n')))
                || (!matches!(line.kind, LineKind::Parsed) && !line.emissions.is_empty())
            {
                return Err(Error::Invalid("capture line receipt coverage"));
            }
            for (k, decision) in line.emissions.iter().enumerate() {
                match decision {
                    Decision::Stored { id, indexed } => {
                        let event = events
                            .next()
                            .ok_or(Error::Invalid("missing authorized parsed output"))?;
                        if *id != next
                            || event.id != *id
                            || event.indexed != *indexed
                            || event.agent != self.identity.agent
                            || event.line_start != at
                            || event.line_end != line.end
                            || event.emission as usize != k
                            || event.kind > 6
                            || (event.parent != 0
                                && (event.parent < self.identity.first_id || event.parent >= next))
                            || event.revises != 0
                        {
                            return Err(Error::Invalid("parsed identity or eligibility mismatch"));
                        }
                        next = next
                            .checked_add(1)
                            .ok_or(Error::Invalid("event ID exhaustion"))?;
                    }
                    Decision::Withheld { reason } if !reason.is_empty() && reason.len() <= 128 => {}
                    _ => return Err(Error::Invalid("withholding receipt missing")),
                }
            }
            at = line.end;
        }
        if (self.framing == Framing::Lines && input[processed as usize..].contains(&b'\n'))
            || at != p.outcome.parsed_end
            || events.next().is_some()
            || p.outcome.stored != p.events.len() as u64
            || p.outcome.next_id != next
            || p.outcome.first_id != p.events.first().map(|e| e.id)
        {
            return Err(Error::Invalid("capture outcome mismatch"));
        }
        if self.ids.len() + p.events.len() > MAX_EVENTS {
            return Err(Error::Invalid("parsed event capacity"));
        }
        self.metadata_end(encoded_len)?;
        Ok(())
    }
    fn replay(&mut self, seq: u64, original: &[u8]) -> Result<()> {
        let bytes = original;
        match bytes.first() {
            Some(1) => {
                let metadata_end = self
                    .metadata_end(bytes.len() - 1)
                    .map_err(|_| Error::Corrupt("capture replay metadata capacity"))?;
                let r: Registration = serde_json::from_slice(&bytes[1..])
                    .map_err(|_| Error::Corrupt("source registration"))?;
                valid_context(&r.context).map_err(|_| Error::Corrupt("source context"))?;
                if r.descriptor.len() > crate::raw::MAX_DESCRIPTOR_BYTES
                    || self.sources.contains_key(&r.source)
                {
                    return Err(Error::Corrupt("duplicate or oversized source"));
                }
                self.sources.insert(
                    r.source,
                    SourceState {
                        initial_context: r.context.clone(),
                        descriptor: r.descriptor,
                        context: r.context,
                        raw_end: 0,
                        parsed_end: 0,
                        file_no: u32::try_from(self.sources.len() + 1)
                            .map_err(|_| Error::Corrupt("source ordinal exhaustion"))?,
                        tail_policy: None,
                        blocks: Vec::new(),
                    },
                );
                self.metadata_bytes = metadata_end;
            }
            Some(tag @ (2 | 3)) if bytes.len() >= 5 => {
                if (*tag == 2) != (self.framing == Framing::Lines) {
                    return Err(Error::Corrupt("capture framing carrier mismatch"));
                }
                let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
                if len > MAX_BATCH_BYTES || 5 + len > bytes.len() {
                    return Err(Error::Corrupt("capture batch length"));
                }
                let message = if self.framing == Framing::Messages {
                    let value: messages::ParsedMessage = serde_json::from_slice(&bytes[5..5 + len])
                        .map_err(|_| Error::Corrupt("missing message policy/context/output"))?;
                    self.validate_request(&value, &bytes[5 + len..])
                        .map_err(|_| Error::Corrupt("message original intent or sequence"))?;
                    Some(value)
                } else {
                    None
                };
                let p: ParsedBatch = if let Some(message) = &message {
                    message.normalized()
                } else {
                    serde_json::from_slice(&bytes[5..5 + len])
                        .map_err(|_| Error::Corrupt("missing capture policy/context/output"))?
                };
                let raw = &bytes[5 + len..];
                self.validate(&p, raw, len)
                    .map_err(|_| Error::Corrupt("capture receipt validation"))?;
                let s = self.sources.get_mut(&p.source).unwrap();
                if !raw.is_empty() {
                    s.blocks.push(Block {
                        seq,
                        offset: s.raw_end,
                        len: raw.len(),
                        at: 5 + len,
                    });
                }
                s.raw_end = p.outcome.raw_end;
                s.parsed_end = p.outcome.parsed_end;
                s.tail_policy = (s.raw_end > s.parsed_end).then(|| p.policy.policy_id.clone());
                s.context = p.context.clone();
                for (e, event) in p.events.iter().enumerate() {
                    self.ids.insert(event.id, (self.batches.len(), e));
                }
                self.metadata_bytes += len;
                self.next_id = p.outcome.next_id;
                self.batches.push(p);
                if let Some(message) = message {
                    self.message_meta
                        .push(messages::Meta::new(&message, self.batches.len() - 1));
                }
            }
            _ => return Err(Error::Corrupt("unknown capture authority record")),
        }
        self.add_prefix(seq, original)
    }
    pub fn compact(&mut self) -> Result<()> {
        self.healthy()?;
        if let Err(e) = self.store.compact() {
            self.failed = true;
            return Err(e);
        }
        Ok(())
    }
}
fn valid_context(context: &[u8]) -> Result<()> {
    if context.is_empty()
        || context.len() > MAX_CONTEXT_BYTES
        || !serde_json::from_slice::<serde_json::Value>(context).is_ok_and(|v| v.is_object())
    {
        return Err(Error::Invalid("missing or oversized sealed parser context"));
    }
    Ok(())
}

// An in-memory projection may only follow this particular open authority handle.
// Reopening (including the same path/key) requires rebuilding from receipts.
fn next_instance() -> Result<u64> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut current = NEXT.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(Error::Invalid("authority instance exhaustion"))?;
        match NEXT.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(value) => return Ok(value),
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    const SOURCE: Source = [6; 16];
    const KEY: [u8; 32] = [91; 32];
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "capture-authority-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn identity() -> Identity {
        Identity {
            writer: "synthetic-writer".into(),
            agent: 1,
            first_id: 9,
            adapter: "synthetic-adapter-v1".into(),
        }
    }
    fn open(dir: &Path) -> CaptureLog {
        CaptureLog::open(dir, &KEY, identity()).unwrap()
    }
    fn batch() -> Batch {
        Batch {
            raw: b"{}\n".to_vec(),
            parsed: ParsedBatch {
                source: SOURCE,
                raw_start: 0,
                parsed_before: 0,
                outcome: Outcome {
                    raw_end: 3,
                    parsed_end: 3,
                    first_id: None,
                    stored: 0,
                    next_id: 9,
                },
                policy: PolicyReceipt {
                    policy_id: "deny-all".into(),
                    raw_authorization: "explicit-source".into(),
                    lines: vec![Line {
                        start: 0,
                        end: 3,
                        kind: LineKind::Parsed,
                        emissions: vec![Decision::Withheld {
                            reason: "synthetic-policy".into(),
                        }],
                    }],
                },
                context: br#"{"seq":1}"#.to_vec(),
                events: vec![],
            },
        }
    }
    #[test]
    fn capture_authority_zero_output_progress_reopens_and_compacts_without_sidecars() {
        let t = Temp::new();
        let mut log = open(&t.0);
        log.register(SOURCE, b"synthetic-private-path-marker", br#"{"seq":0}"#)
            .unwrap();
        let expected = log.commit(batch()).unwrap();
        log.compact().unwrap();
        assert_eq!(log.read_raw(SOURCE, 0, 3).unwrap(), b"{}\n");
        drop(log);
        let log = open(&t.0);
        assert_eq!(log.progress(SOURCE).unwrap().parsed_end, 3);
        assert_eq!(log.batches().unwrap()[0].outcome, expected);
        assert_eq!(log.next_id(), 9);
        assert_eq!(log.progress(SOURCE).unwrap().context, br#"{"seq":1}"#);
        assert!(crate::raw::RawJournal::open(&t.0, &KEY).is_err());
        for entry in std::fs::read_dir(&t.0).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let bytes = std::fs::read(entry.path()).unwrap();
                assert!(
                    !bytes
                        .windows(b"synthetic-private-path-marker".len())
                        .any(|v| v == b"synthetic-private-path-marker")
                );
            }
        }
    }
    #[test]
    fn capture_capacity_counts_args_context_policy_and_preserves_committed_prefix() {
        for stored in [false, true] {
            let t = Temp::new();
            let mut log = open(&t.0);
            log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
            let initial_bytes = log.metadata_bytes;
            let initial_len = log.store.len();
            assert_eq!(initial_bytes, log.store.get(1).unwrap().len() - 1);
            let mut b = batch();
            b.parsed.context = serde_json::to_vec(&serde_json::json!({
                "synthetic_state": "c".repeat(4096)
            }))
            .unwrap();
            if stored {
                b.parsed.events.push(Event {
                    id: 9,
                    session: 1,
                    parent_session: 0,
                    agent: 1,
                    turn: 0,
                    seq: 0,
                    parent: 0,
                    revises: 0,
                    kind: 0,
                    status: 0,
                    exit_code: None,
                    tokens_in: 0,
                    tokens_out: 0,
                    ts_us: 1,
                    duration_us: 0,
                    tool: "synthetic-tool".into(),
                    args: serde_json::json!({"input": "a".repeat(8192)}).to_string(),
                    text: String::new(),
                    indexed: true,
                    line_start: 0,
                    line_end: 3,
                    emission: 0,
                });
                b.parsed.policy.lines[0].emissions = vec![Decision::Stored {
                    id: 9,
                    indexed: true,
                }];
                b.parsed.outcome.first_id = Some(9);
                b.parsed.outcome.stored = 1;
                b.parsed.outcome.next_id = 10;
            }
            let encoded = serde_json::to_vec(&b.parsed).unwrap();
            assert!(b.parsed.events.iter().all(|e| e.text.is_empty()));
            // Place this small synthetic fixture at the capacity boundary without
            // allocating 128 MiB. Its non-text metadata must still refuse pre-append.
            log.metadata_bytes = MAX_PARSED_BYTES - encoded.len() + 1;
            let candidate = Batch {
                parsed: b.parsed.clone(),
                raw: b.raw.clone(),
            };
            assert!(matches!(log.commit(candidate), Err(Error::Invalid(_))));
            assert_eq!(log.store.len(), initial_len);
            assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
            assert_eq!(log.progress(SOURCE).unwrap().context, br#"{"seq":0}"#);
            assert!(log.batches().unwrap().is_empty());
            assert_eq!(log.next_id(), 9);
            log.metadata_bytes = initial_bytes;
            let expected = log.commit(b).unwrap();
            assert_eq!(log.metadata_bytes, initial_bytes + encoded.len());
            drop(log);
            let log = open(&t.0);
            assert_eq!(log.metadata_bytes, initial_bytes + encoded.len());
            assert_eq!(log.progress(SOURCE).unwrap().parsed_end, 3);
            assert_eq!(log.read_raw(SOURCE, 0, 3).unwrap(), b"{}\n");
            assert_eq!(log.batches().unwrap()[0].outcome, expected);
            assert_eq!(log.event(9).unwrap().is_some(), stored);
        }
    }
    #[test]
    fn capture_registration_capacity_refuses_before_append_and_replay_publication() {
        let t = Temp::new();
        let mut log = open(&t.0);
        let context = br#"{"seq":0}"#;
        log.register(SOURCE, b"synthetic", context).unwrap();
        let initial_bytes = log.metadata_bytes;
        let initial_len = log.store.len();
        let source = [7; 16];
        log.metadata_bytes = MAX_PARSED_BYTES;
        assert!(log.register(source, b"other-source", context).is_err());
        assert_eq!(log.store.len(), initial_len);
        assert_eq!(log.sources().count(), 1);
        let registration = log.store.get(1).unwrap();
        assert!(matches!(
            log.replay(2, &registration),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(log.sources().count(), 1);
        log.metadata_bytes = initial_bytes;
        drop(log);
        let log = open(&t.0);
        assert_eq!(log.sources().count(), 1);
        assert_eq!(log.metadata_bytes, initial_bytes);
    }
    #[test]
    fn capture_replay_capacity_rejects_before_exposing_raw_or_parsed_output() {
        let a = Temp::new();
        let mut written = open(&a.0);
        written
            .register(SOURCE, b"synthetic", br#"{"seq":0}"#)
            .unwrap();
        written.commit(batch()).unwrap();
        let encoded = written.store.get(2).unwrap();
        let t = Temp::new();
        let mut log = open(&t.0);
        log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
        log.metadata_bytes = MAX_PARSED_BYTES;
        assert!(matches!(log.replay(2, &encoded), Err(Error::Corrupt(_))));
        assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
        assert_eq!(log.progress(SOURCE).unwrap().parsed_end, 0);
        assert_eq!(log.next_id(), 9);
        assert!(log.batches().unwrap().is_empty());
    }
    #[test]
    fn capture_authority_torn_or_uncertain_write_never_advances_acknowledged_memory() {
        for cut in [9, usize::MAX] {
            let t = Temp::new();
            let mut log = open(&t.0);
            log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
            log.store.fail_next_append_after = Some(cut);
            assert!(log.commit(batch()).is_err());
            assert!(log.progress(SOURCE).is_err());
            drop(log);
            let log = open(&t.0);
            let progress = log.progress(SOURCE).unwrap();
            assert_eq!(progress.parsed_end, if cut == 9 { 0 } else { 3 });
            assert_eq!(log.next_id(), 9);
        }
    }
    #[test]
    fn capture_authority_missing_policy_context_and_contradictory_offsets_fail_closed() {
        for field in ["policy", "context"] {
            let t = Temp::new();
            let mut log = open(&t.0);
            log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
            let b = batch();
            let mut value = serde_json::to_value(&b.parsed).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let encoded = serde_json::to_vec(&value).unwrap();
            let mut bytes = vec![2];
            bytes.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            bytes.extend(encoded);
            bytes.extend(b.raw);
            log.write(&bytes).unwrap(); // valid sealed Store frame, invalid native receipt
            drop(log);
            assert!(matches!(
                CaptureLog::open(&t.0, &KEY, identity()),
                Err(Error::Corrupt(_))
            ));
        }
        let t = Temp::new();
        let mut log = open(&t.0);
        log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
        let mut b = batch();
        b.parsed.outcome.next_id = 10;
        assert!(log.commit(b).is_err());
        assert_eq!(log.progress(SOURCE).unwrap().raw_end, 0);
    }
    #[test]
    fn capture_authority_rejects_unknown_stored_and_withheld_decision_fields() {
        for decision in [
            serde_json::json!({"Stored":{"id":9,"indexed":false,"extension":true}}),
            serde_json::json!({"Withheld":{"reason":"synthetic","extension":true}}),
        ] {
            let t = Temp::new();
            let mut log = open(&t.0);
            log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
            let b = batch();
            let mut value = serde_json::to_value(&b.parsed).unwrap();
            value["policy"]["lines"][0]["emissions"][0] = decision;
            let encoded = serde_json::to_vec(&value).unwrap();
            let mut bytes = vec![2];
            bytes.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            bytes.extend(encoded);
            bytes.extend(b.raw);
            log.write(&bytes).unwrap();
            drop(log);
            assert!(matches!(
                CaptureLog::open(&t.0, &KEY, identity()),
                Err(Error::Corrupt(_))
            ));
        }
    }
}
