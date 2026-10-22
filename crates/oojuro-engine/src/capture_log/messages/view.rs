//! Explicit message receipt projections. The line receipt API cannot export this framing.
use super::*;
use crate::receipts::{self, Limits, Range, Scope, Span, Watermark};
type ViewResult<T> = receipts::Result<T>;
fn authority(log: &MessageLog) -> ViewResult<&CaptureLog> {
    log.authority().map_err(|_| receipts::Error::Unavailable)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageWatermark {
    pub framing: Framing,
    pub prefix: Watermark,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessageOutput {
    /// Event legacy line_start/end are byte offsets; byte_range and framing are explicit.
    Stored {
        event: Event,
    },
    Withheld {
        reason: String,
    },
    NoOutput {
        message_kind: MessageKind,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageReceipt {
    pub framing: Framing,
    pub ordinal: u64,
    pub batch: usize,
    pub source_generation: Source,
    pub byte_range: Span,
    pub emission: Option<usize>,
    pub batch_raw: Span,
    pub policy_id: String,
    pub raw_authorization: String,
    pub adapter: String,
    pub receipt_schema: u32,
    pub output: MessageOutput,
}
impl MessageReceipt {
    fn from_line(receipt: receipts::Receipt) -> ViewResult<Self> {
        let output = match receipt.output {
            receipts::Output::Stored { event } => MessageOutput::Stored { event },
            receipts::Output::Withheld { reason } => MessageOutput::Withheld { reason },
            receipts::Output::NoOutput { line_kind } => MessageOutput::NoOutput {
                message_kind: match line_kind {
                    LineKind::Parsed => MessageKind::Parsed,
                    LineKind::Malformed => MessageKind::Malformed,
                    LineKind::Blank => return Err(receipts::Error::Authority),
                },
            },
        };
        Ok(Self {
            framing: Framing::Messages,
            ordinal: receipt.ordinal,
            batch: receipt.batch,
            source_generation: receipt.source_generation,
            byte_range: receipt.line,
            emission: receipt.emission,
            batch_raw: receipt.batch_raw,
            policy_id: receipt.policy_id,
            raw_authorization: receipt.raw_authorization,
            adapter: receipt.adapter,
            receipt_schema: 1,
            output,
        })
    }
}
#[derive(Serialize)]
pub struct MessagePage {
    pub framing: Framing,
    pub watermark: MessageWatermark,
    pub range: Range,
    pub entries: Vec<MessageReceipt>,
    pub next: Option<String>,
}
#[derive(Serialize)]
pub struct MessageStats {
    pub framing: Framing,
    pub watermark: MessageWatermark,
    pub range: Range,
    pub receipts: u64,
    pub stored: u64,
    pub indexed: u64,
    pub withheld: u64,
    pub malformed_units: u64,
    pub parsed_units_without_output: u64,
}
pub struct MessageReceiptView {
    inner: receipts::ReceiptView,
}
impl MessageReceiptView {
    pub fn open(log: &MessageLog, scope: Scope<'_>, range: Range) -> ViewResult<Self> {
        Ok(Self {
            inner: receipts::ReceiptView::open_framed(
                authority(log)?,
                scope,
                range,
                Framing::Messages,
            )?,
        })
    }
    pub fn resume(
        log: &MessageLog,
        scope: Scope<'_>,
        range: Range,
        cursor: &str,
    ) -> ViewResult<Self> {
        Ok(Self {
            inner: receipts::ReceiptView::resume_framed(
                authority(log)?,
                scope,
                range,
                cursor,
                Framing::Messages,
            )?,
        })
    }
    pub fn watermark(&self) -> MessageWatermark {
        MessageWatermark {
            framing: Framing::Messages,
            prefix: self.inner.watermark().clone(),
        }
    }
    pub fn page(&self, log: &MessageLog, limits: Limits) -> ViewResult<MessagePage> {
        check_bytes(limits.bytes)?;
        let mut entries = limits.entries;
        loop {
            let old = self.inner.page(
                authority(log)?,
                Limits {
                    entries,
                    bytes: limits.bytes,
                },
            )?;
            let count = old.entries.len();
            let page = MessagePage {
                framing: Framing::Messages,
                watermark: self.watermark(),
                range: old.range,
                entries: old
                    .entries
                    .into_iter()
                    .map(MessageReceipt::from_line)
                    .collect::<ViewResult<_>>()?,
                next: old.next,
            };
            if encoded(&page)?.len() <= limits.bytes {
                return Ok(page);
            }
            if count <= 1 {
                return Err(receipts::Error::Bounds);
            }
            // At most ten bounded attempts for the 1000-entry cap, never an unbounded retry.
            entries = count / 2;
        }
    }
    pub fn export(&self, log: &MessageLog, limits: Limits) -> ViewResult<Vec<u8>> {
        encoded(&self.page(log, limits)?)
    }
    pub fn get(
        &self,
        log: &MessageLog,
        id: u64,
        bytes: usize,
    ) -> ViewResult<Option<MessageReceipt>> {
        check_bytes(bytes)?;
        let found = self
            .inner
            .get(authority(log)?, id, bytes)?
            .map(MessageReceipt::from_line)
            .transpose()?;
        if encoded(&found)?.len() > bytes {
            return Err(receipts::Error::Bounds);
        }
        Ok(found)
    }
    pub fn stats(&self, log: &MessageLog, bytes: usize) -> ViewResult<MessageStats> {
        check_bytes(bytes)?;
        let old = self.inner.stats(authority(log)?, bytes)?;
        if old.blank != 0 {
            return Err(receipts::Error::Authority);
        }
        let stats = MessageStats {
            framing: Framing::Messages,
            watermark: self.watermark(),
            range: old.range.ok_or(receipts::Error::Authority)?,
            receipts: old.receipts,
            stored: old.stored,
            indexed: old.indexed,
            withheld: old.withheld,
            malformed_units: old.malformed,
            parsed_units_without_output: old.parsed_without_output,
        };
        if encoded(&stats)?.len() > bytes {
            return Err(receipts::Error::Bounds);
        }
        Ok(stats)
    }
}
fn check_bytes(bytes: usize) -> ViewResult<()> {
    if bytes == 0 || bytes > receipts::MAX_OUTPUT_BYTES {
        Err(receipts::Error::Bounds)
    } else {
        Ok(())
    }
}
fn encoded<T: Serialize>(value: &T) -> ViewResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| receipts::Error::Unavailable)
}
