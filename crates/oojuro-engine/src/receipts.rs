//! Opt-in, scoped reads of original committed receipts, not a history database.
//! No raw bytes, descriptors or parser context cross this boundary.
use crate::capture_log::{CachePrefix, CaptureLog, Decision, Event, LineKind, ParsedBatch, Source};
use serde::{Deserialize, Serialize};

pub const MAX_ENTRIES: usize = 1000;
pub const MAX_OUTPUT_BYTES: usize = 16 << 20;
const CURSOR_BOUND: usize = 8192;
const CURSOR_AAD: &[u8] = b"oojuro/receipt-cursor/v1\0";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Scope,
    Authority,
    Cursor,
    Bounds,
    Unavailable,
}
pub type Result<T> = std::result::Result<T, Error>;
/// Local scope binding. The caller still owns authentication and authorization.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub writer: &'a str,
    pub agent: u32,
}
/// Zero-based receipt ordinals, start inclusive and end exclusive. A stored or
/// withheld emission occupies one ordinal; a line with no emissions occupies one.
/// No time is inferred for a withheld, blank or malformed line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}
impl Default for Range {
    fn default() -> Self {
        Self {
            start: 0,
            end: u64::MAX,
        }
    }
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 100,
            bytes: 1 << 20,
        }
    }
}
/// AEAD-encrypted existing receipt-prefix digest, not a public plaintext hash
/// of raw/context bytes or an independently current anti-rollback witness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watermark {
    pub format: u32,
    pub origin: [u8; 32],
    pub identity: [u8; 32],
    pub records: u64,
    pub batches: usize,
    pub commitment: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Output {
    Stored { event: Event },
    Withheld { reason: String },
    NoOutput { line_kind: LineKind },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub ordinal: u64,
    pub batch: usize,
    pub source_generation: Source,
    pub line: Span,
    pub emission: Option<usize>,
    pub batch_raw: Span,
    pub batch_parsed: Span,
    pub policy_id: String,
    pub raw_authorization: String,
    pub adapter: String,
    pub receipt_schema: u32,
    pub output: Output,
}
#[derive(Serialize)]
pub struct Page {
    pub watermark: Watermark,
    pub range: Range,
    pub entries: Vec<Receipt>,
    pub next: Option<String>,
}
#[derive(Default, Serialize)]
pub struct Stats {
    pub watermark: Option<Watermark>,
    pub range: Option<Range>,
    pub receipts: u64,
    pub stored: u64,
    /// The exact original boolean, not current search/dedupe visibility.
    pub indexed: u64,
    pub withheld: u64,
    pub blank: u64,
    pub malformed: u64,
    pub parsed_without_output: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    watermark: Watermark,
    writer: String,
    agent: u32,
    range: Range,
    next: u64,
}
/// An immutable prefix selection. It borrows no open log; every operation takes
/// and validates the independently opened authority. Append is allowed but hidden.
pub struct ReceiptView {
    watermark: Watermark,
    range: Range,
    next: u64,
    framing: crate::capture_log::Framing,
}
impl ReceiptView {
    pub fn open(log: &CaptureLog, scope: Scope<'_>, range: Range) -> Result<Self> {
        Self::open_framed(log, scope, range, crate::capture_log::Framing::Lines)
    }
    pub(crate) fn open_framed(
        log: &CaptureLog,
        scope: Scope<'_>,
        range: Range,
        framing: crate::capture_log::Framing,
    ) -> Result<Self> {
        if log.framing() != framing {
            return Err(Error::Scope);
        }
        check_scope(log, scope)?;
        if range.start > range.end {
            return Err(Error::Bounds);
        }
        let p = log.cache_tip().map_err(|_| Error::Unavailable)?;
        let mut watermark = Watermark {
            format: 1,
            origin: log.cache_origin().map_err(|_| Error::Unavailable)?,
            identity: identity(log)?,
            records: p.records,
            batches: p.batches,
            commitment: String::new(),
        };
        watermark.commitment = hex(&log
            .cache_cipher()
            .seal_frame(&prefix_aad(&watermark), &p.digest));
        Ok(Self {
            watermark,
            range,
            next: range.start,
            framing,
        })
    }
    pub fn resume(log: &CaptureLog, scope: Scope<'_>, range: Range, cursor: &str) -> Result<Self> {
        Self::resume_framed(
            log,
            scope,
            range,
            cursor,
            crate::capture_log::Framing::Lines,
        )
    }
    pub(crate) fn resume_framed(
        log: &CaptureLog,
        scope: Scope<'_>,
        range: Range,
        cursor: &str,
        framing: crate::capture_log::Framing,
    ) -> Result<Self> {
        if log.framing() != framing {
            return Err(Error::Scope);
        }
        check_scope(log, scope)?;
        let c = decode(log, cursor)?;
        if c.writer != scope.writer || c.agent != scope.agent || c.range != range {
            return Err(Error::Cursor);
        }
        let view = Self {
            watermark: c.watermark,
            range,
            next: c.next,
            framing,
        };
        view.validate(log)?;
        if c.next < range.start
            || c.next >= range.end
            || c.next >= count(log, view.watermark.batches)?
        {
            return Err(Error::Cursor);
        }
        Ok(view)
    }
    pub fn watermark(&self) -> &Watermark {
        &self.watermark
    }
    fn validate(&self, log: &CaptureLog) -> Result<()> {
        if self.watermark.format != 1
            || self.range.start > self.range.end
            || self.framing != log.framing()
        {
            return Err(Error::Authority);
        }
        let prefix = log
            .cache_prefix(self.watermark.records)
            .map_err(|_| Error::Authority)?;
        let sealed = unhex(&self.watermark.commitment, 144).map_err(|_| Error::Authority)?;
        let digest = log
            .cache_cipher()
            .open_frame(&prefix_aad(&self.watermark), &sealed)
            .map_err(|_| Error::Authority)?;
        let digest: [u8; 32] = digest.try_into().map_err(|_| Error::Authority)?;
        if identity(log)? != self.watermark.identity
            || log.cache_origin().map_err(|_| Error::Unavailable)? != self.watermark.origin
            || prefix
                != (CachePrefix {
                    records: self.watermark.records,
                    batches: self.watermark.batches,
                    digest,
                })
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    fn cursor(&self, log: &CaptureLog, next: u64) -> Result<String> {
        let c = Cursor {
            watermark: self.watermark.clone(),
            writer: log.identity().writer.clone(),
            agent: log.identity().agent,
            range: self.range,
            next,
        };
        let plain = serde_json::to_vec(&c).map_err(|_| Error::Unavailable)?;
        let bytes = log.cache_cipher().seal_frame(CURSOR_AAD, &plain);
        Ok(hex(&bytes))
    }
    pub fn page(&self, log: &CaptureLog, limits: Limits) -> Result<Page> {
        self.validate(log)?;
        if limits.entries == 0 || limits.entries > MAX_ENTRIES {
            return Err(Error::Bounds);
        }
        check_bytes(limits.bytes)?;
        let total = count(log, self.watermark.batches)?.min(self.range.end);
        let mut page = Page {
            watermark: self.watermark.clone(),
            range: self.range,
            entries: vec![],
            next: None,
        };
        let mut entry_bytes = 0usize;
        walk(
            log,
            self.watermark.batches,
            &mut |ordinal, batch, line, emission, output| {
                if ordinal < self.next || ordinal >= self.range.end {
                    return Ok(true);
                }
                if page.entries.len() == limits.entries {
                    return Ok(false);
                }
                let entry = receipt(log, ordinal, batch, line, emission, output)?;
                let next = if ordinal + 1 < total {
                    Some(self.cursor(log, ordinal + 1)?)
                } else {
                    None
                };
                let added = encoded(&entry)?.len() + usize::from(!page.entries.is_empty());
                let envelope = Page {
                    watermark: self.watermark.clone(),
                    range: self.range,
                    entries: vec![],
                    next: next.clone(),
                };
                if encoded(&envelope)?.len() + entry_bytes + added > limits.bytes {
                    if page.entries.is_empty() {
                        return Err(Error::Bounds);
                    }
                    return Ok(false);
                }
                entry_bytes += added;
                page.entries.push(entry);
                page.next = next;
                Ok(true)
            },
        )?;
        if encoded(&page)?.len() > limits.bytes {
            return Err(Error::Bounds);
        }
        Ok(page)
    }
    /// JSON page export, including its authenticated next cursor and watermark.
    pub fn export(&self, log: &CaptureLog, limits: Limits) -> Result<Vec<u8>> {
        encoded(&self.page(log, limits)?)
    }
    /// Original stored event by ID, including events absent from lexical search.
    /// A range selection also applies to get; a later append is invisible.
    pub fn get(&self, log: &CaptureLog, id: u64, bytes: usize) -> Result<Option<Receipt>> {
        self.validate(log)?;
        check_bytes(bytes)?;
        let mut found = None;
        walk(
            log,
            self.watermark.batches,
            &mut |ordinal, batch, line, emission, output| {
                if ordinal >= self.range.start
                    && ordinal < self.range.end
                    && matches!(&output, Borrowed::Stored(e) if e.id == id)
                {
                    let value = receipt(log, ordinal, batch, line, emission, output)?;
                    if encoded(&value)?.len() > bytes {
                        return Err(Error::Bounds);
                    }
                    found = Some(value);
                    return Ok(false);
                }
                Ok(true)
            },
        )?;
        if encoded(&found)?.len() > bytes {
            return Err(Error::Bounds);
        }
        Ok(found)
    }
    pub fn stats(&self, log: &CaptureLog, bytes: usize) -> Result<Stats> {
        self.validate(log)?;
        check_bytes(bytes)?;
        let mut s = Stats {
            watermark: Some(self.watermark.clone()),
            range: Some(self.range),
            ..Stats::default()
        };
        walk(
            log,
            self.watermark.batches,
            &mut |ordinal, _, _, _, output| {
                if ordinal < self.range.start || ordinal >= self.range.end {
                    return Ok(true);
                }
                s.receipts += 1;
                match output {
                    Borrowed::Stored(e) => {
                        s.stored += 1;
                        s.indexed += u64::from(e.indexed);
                    }
                    Borrowed::Withheld(_) => s.withheld += 1,
                    Borrowed::NoOutput(LineKind::Blank) => s.blank += 1,
                    Borrowed::NoOutput(LineKind::Malformed) => s.malformed += 1,
                    Borrowed::NoOutput(LineKind::Parsed) => s.parsed_without_output += 1,
                }
                Ok(true)
            },
        )?;
        if encoded(&s)?.len() > bytes {
            return Err(Error::Bounds);
        }
        Ok(s)
    }
}
fn check_scope(log: &CaptureLog, scope: Scope<'_>) -> Result<()> {
    log.normalized_batches().map_err(|_| Error::Unavailable)?;
    if log.identity().writer != scope.writer || log.identity().agent != scope.agent {
        return Err(Error::Scope);
    }
    Ok(())
}
fn check_bytes(n: usize) -> Result<()> {
    if n == 0 || n > MAX_OUTPUT_BYTES {
        Err(Error::Bounds)
    } else {
        Ok(())
    }
}
fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| Error::Unavailable)
}
fn identity(log: &CaptureLog) -> Result<[u8; 32]> {
    Ok(*blake3::hash(&encoded(log.identity())?).as_bytes())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str, bound: usize) -> Result<Vec<u8>> {
    if text.len() > bound || !text.len().is_multiple_of(2) {
        return Err(Error::Cursor);
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(Error::Cursor),
        };
        bytes.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    Ok(bytes)
}
fn prefix_aad(w: &Watermark) -> Vec<u8> {
    let mut aad = b"oojuro/receipt-prefix/v1\0".to_vec();
    aad.extend(w.format.to_le_bytes());
    aad.extend(w.origin);
    aad.extend(w.identity);
    aad.extend(w.records.to_le_bytes());
    aad.extend((w.batches as u64).to_le_bytes());
    aad
}
fn decode(log: &CaptureLog, text: &str) -> Result<Cursor> {
    let bytes = unhex(text, CURSOR_BOUND)?;
    let plain = log
        .cache_cipher()
        .open_frame(CURSOR_AAD, &bytes)
        .map_err(|_| Error::Cursor)?;
    serde_json::from_slice(&plain).map_err(|_| Error::Cursor)
}

enum Borrowed<'a> {
    Stored(&'a Event),
    Withheld(&'a str),
    NoOutput(&'a LineKind),
}
fn count(log: &CaptureLog, batches: usize) -> Result<u64> {
    log.normalized_batches()
        .map_err(|_| Error::Unavailable)?
        .get(..batches)
        .ok_or(Error::Authority)?
        .iter()
        .flat_map(|b| &b.policy.lines)
        .try_fold(0u64, |n, l| {
            n.checked_add(l.emissions.len().max(1) as u64)
                .ok_or(Error::Bounds)
        })
}
type Visitor<'a> = dyn FnMut(u64, usize, &crate::capture_log::Line, Option<usize>, Borrowed<'_>) -> Result<bool>
    + 'a;
fn walk(log: &CaptureLog, batches: usize, f: &mut Visitor<'_>) -> Result<()> {
    let all = log
        .normalized_batches()
        .map_err(|_| Error::Unavailable)?
        .get(..batches)
        .ok_or(Error::Authority)?;
    let mut ordinal = 0u64;
    for (batch, b) in all.iter().enumerate() {
        let mut events = b.events.iter();
        for line in &b.policy.lines {
            if line.emissions.is_empty() {
                if !f(ordinal, batch, line, None, Borrowed::NoOutput(&line.kind))? {
                    return Ok(());
                }
                ordinal = ordinal.checked_add(1).ok_or(Error::Bounds)?;
            } else {
                for (emission, decision) in line.emissions.iter().enumerate() {
                    let output = match decision {
                        Decision::Stored { .. } => {
                            Borrowed::Stored(events.next().ok_or(Error::Authority)?)
                        }
                        Decision::Withheld { reason } => Borrowed::Withheld(reason),
                    };
                    if !f(ordinal, batch, line, Some(emission), output)? {
                        return Ok(());
                    }
                    ordinal = ordinal.checked_add(1).ok_or(Error::Bounds)?;
                }
            }
        }
    }
    Ok(())
}
fn receipt(
    log: &CaptureLog,
    ordinal: u64,
    batch: usize,
    line: &crate::capture_log::Line,
    emission: Option<usize>,
    output: Borrowed<'_>,
) -> Result<Receipt> {
    let b: &ParsedBatch = log
        .normalized_batches()
        .map_err(|_| Error::Unavailable)?
        .get(batch)
        .ok_or(Error::Authority)?;
    Ok(Receipt {
        ordinal,
        batch,
        source_generation: b.source,
        line: Span {
            start: line.start,
            end: line.end,
        },
        emission,
        batch_raw: Span {
            start: b.raw_start,
            end: b.outcome.raw_end,
        },
        batch_parsed: Span {
            start: b.parsed_before,
            end: b.outcome.parsed_end,
        },
        policy_id: b.policy.policy_id.clone(),
        raw_authorization: b.policy.raw_authorization.clone(),
        adapter: log.identity().adapter.clone(),
        receipt_schema: 1,
        output: match output {
            Borrowed::Stored(e) => Output::Stored { event: e.clone() },
            Borrowed::Withheld(reason) => Output::Withheld {
                reason: reason.into(),
            },
            Borrowed::NoOutput(kind) => Output::NoOutput {
                line_kind: kind.clone(),
            },
        },
    })
}
#[cfg(test)]
mod tests;
