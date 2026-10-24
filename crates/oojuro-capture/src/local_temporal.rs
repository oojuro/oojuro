//! Explicit Temporal event-stream v1. No filesystem source, plaintext spool or cloud transport.
use super::*;
use crate::{Emitter, status};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    pub namespace: String,
    pub workflow_id: String,
    pub run_id: String,
    /// Caller-owned stable stream generation. Never silently replace after a source fork.
    pub generation: String,
}
impl Stream {
    pub fn source_id(&self) -> Result<Source> {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.bytes()?)[..16]
            .try_into()
            .map_err(|_| Error::Corrupt)
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        for value in [
            &self.namespace,
            &self.workflow_id,
            &self.run_id,
            &self.generation,
        ] {
            if !valid_name(value) {
                return Err(Error::Invalid("temporal stream identity"));
            }
        }
        serde_json::to_vec(self).map_err(storage)
    }
}
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub version: u32,
    pub id: String,
    pub sequence: u32,
    pub valid_us: i64,
    pub kind: Kind,
    pub tool: Option<String>,
    pub operation_id: Option<String>,
    pub status: Option<State>,
    pub payload: serde_json::Value,
    pub attributes: std::collections::BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Input,
    Output,
    Thinking,
    System,
    ToolCall,
    ToolResult,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Ok,
    Error,
}
impl Envelope {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_LINE_BYTES
            || bytes.last() != Some(&b'\n')
            || bytes[..bytes.len() - 1].contains(&b'\n')
        {
            return Err(Error::Invalid("one bounded temporal frame required"));
        }
        let e: Self =
            serde_json::from_slice(bytes).map_err(|_| Error::Invalid("temporal frame schema"))?;
        if e.version != 1
            || !valid_name(&e.id)
            || e.sequence == 0
            || e.valid_us.unsigned_abs() > 9_007_199_254_740_991
        {
            return Err(Error::Invalid("temporal event identity"));
        }
        if serde_json::to_vec(&e.attributes).map_err(storage)?.len() > 4096 {
            return Err(Error::Invalid("temporal attributes bound"));
        }
        if e.tool.as_ref().is_some_and(|v| !valid_name(v))
            || e.operation_id.as_ref().is_some_and(|v| !valid_name(v))
        {
            return Err(Error::Invalid("temporal operation identity"));
        }
        match e.kind {
            Kind::ToolCall | Kind::ToolResult if e.tool.is_none() || e.operation_id.is_none() => {
                return Err(Error::Invalid("temporal operation required"));
            }
            Kind::ToolCall if !e.payload.is_object() => {
                return Err(Error::Invalid("temporal tool arguments"));
            }
            Kind::ToolResult if e.status.is_none() => {
                return Err(Error::Invalid("temporal result status"));
            }
            _ => (),
        }
        Ok(e)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StreamReceipt {
    pub event_id: String,
    pub sequence: u32,
    pub cursor: u64,
    pub end: u64,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StreamPosition {
    pub cursor: u64,
    pub sequence: u32,
    pub existing: Option<StreamReceipt>,
}
fn context(stream: &Stream, writer: &str) -> Result<FileCtx> {
    let descriptor = stream.bytes()?;
    Ok(FileCtx {
        writer: writer.into(),
        session: crate::hash64(&String::from_utf8(descriptor.clone()).map_err(storage)?),
        // These legacy-labelled slots are opaque Temporal state in this new adapter identity:
        // parent_thread = exact stream descriptor; user_from = last event ID; inherit_below = sequence.
        // They do not assert agent or workflow ancestry.
        parent_thread: String::from_utf8(descriptor).map_err(storage)?,
        cwd: "temporal-stream".into(),
        inherit_below: 0,
        ..FileCtx::default()
    })
}
pub(super) fn validate_log(log: &CaptureLog, writer: &str) -> Result<()> {
    for source in log.sources() {
        let descriptor = log
            .source_descriptor(source)
            .map_err(authority)?
            .ok_or(Error::Corrupt)?;
        let stream: Stream = serde_json::from_slice(descriptor).map_err(|_| Error::Corrupt)?;
        if stream.bytes()? != descriptor || stream.source_id()? != source {
            return Err(Error::Corrupt);
        }
        for bytes in [
            log.initial_context(source).map_err(authority)?,
            &log.progress(source).map_err(authority)?.context,
        ] {
            let c = decode_context(bytes)?;
            if c.writer != writer
                || c.parent_thread.as_bytes() != descriptor
                || c.inherit_below < 0
                || (c.inherit_below == 0 && !c.user_from.is_empty())
                || (c.inherit_below > 0 && !valid_name(&c.user_from))
            {
                return Err(Error::Corrupt);
            }
        }
    }
    Ok(())
}
impl LocalCapture {
    /// Local owner-only position/receipt metadata. No event payload or parser context is returned.
    pub fn temporal_position(
        &self,
        source: Source,
        stream: &Stream,
        event_id: &str,
    ) -> Result<StreamPosition> {
        if !matches!(self.format, Adapter::Temporal) || !valid_name(event_id) {
            return Err(Error::Invalid("temporal position identity"));
        }
        self.ready()?;
        let descriptor = stream.bytes()?;
        if stream.source_id()? != source {
            return Err(Error::Invalid("temporal source identity differs"));
        }
        let Some(old) = self.log.source_descriptor(source).map_err(authority)? else {
            return Ok(StreamPosition {
                cursor: 0,
                sequence: 0,
                existing: None,
            });
        };
        if old != descriptor {
            return Err(Error::Invalid("temporal source generation differs"));
        }
        let progress = self.log.progress(source).map_err(authority)?;
        let c = decode_context(&progress.context)?;
        let sequence = u32::try_from(c.inherit_below).map_err(|_| Error::Corrupt)?;
        let mut existing = None;
        for batch in self
            .log
            .batches()
            .map_err(authority)?
            .iter()
            .filter(|b| b.source == source)
        {
            let context = decode_context(&batch.context)?;
            if context.user_from == event_id {
                existing = Some(StreamReceipt {
                    event_id: event_id.into(),
                    sequence: u32::try_from(context.inherit_below).map_err(|_| Error::Corrupt)?,
                    cursor: batch.raw_start,
                    end: batch.outcome.raw_end,
                    outcome: batch.outcome.clone(),
                });
                break;
            }
        }
        Ok(StreamPosition {
            cursor: progress.raw_end,
            sequence,
            existing,
        })
    }
    /// Source identity, cursor and original frame bytes are immutable retry intent.
    /// Checks original committed receipts before refreshing/applying changed policy.
    pub fn temporal_receipt(
        &self,
        source: Source,
        stream: &Stream,
        cursor: u64,
        frame: &[u8],
        retain_raw: bool,
    ) -> Result<Option<StreamReceipt>> {
        if !retain_raw {
            return Err(Error::Invalid("temporal raw consent withheld"));
        }
        if !matches!(self.format, Adapter::Temporal) {
            return Err(Error::Invalid("temporal adapter required"));
        }
        self.ready()?;
        let descriptor = stream.bytes()?;
        if stream.source_id()? != source {
            return Err(Error::Invalid("temporal source identity differs"));
        }
        let e = Envelope::parse(frame)?;
        let Some(old) = self.log.source_descriptor(source).map_err(authority)? else {
            return Ok(None);
        };
        if old != descriptor {
            return Err(Error::Invalid("temporal source generation differs"));
        }
        for batch in self
            .log
            .batches()
            .map_err(authority)?
            .iter()
            .filter(|b| b.source == source)
        {
            let c = decode_context(&batch.context)?;
            if c.user_from == e.id {
                let len = usize::try_from(batch.outcome.raw_end - batch.raw_start)
                    .map_err(|_| Error::Corrupt)?;
                if batch.raw_start != cursor
                    || self.log.read_raw(source, cursor, len).map_err(authority)? != frame
                {
                    return Err(Error::Invalid("temporal event intent differs"));
                }
                return Ok(Some(StreamReceipt {
                    event_id: e.id,
                    sequence: e.sequence,
                    cursor,
                    end: batch.outcome.raw_end,
                    outcome: batch.outcome.clone(),
                }));
            }
        }
        Ok(None)
    }
    /// Atomic encrypted stream frame plus its authorized parsed receipts. No temporary source file.
    pub fn append_temporal(
        &mut self,
        source: Source,
        stream: &Stream,
        cursor: u64,
        frame: &[u8],
        retain_raw: bool,
    ) -> Result<StreamReceipt> {
        if let Some(original) = self.temporal_receipt(source, stream, cursor, frame, retain_raw)? {
            return Ok(original);
        }
        let e = Envelope::parse(frame)?;
        let descriptor = stream.bytes()?;
        let initial = encode_context(&context(stream, &self.options.writer)?)?;
        // A new source is registered only after its candidate sequence/cursor are checked.
        let exists = self
            .log
            .source_descriptor(source)
            .map_err(authority)?
            .is_some();
        if exists {
            if self.log.progress(source).map_err(authority)?.raw_end != cursor {
                return Err(Error::Invalid("temporal cursor differs"));
            }
        } else if cursor != 0 || e.sequence != 1 {
            return Err(Error::Invalid("temporal initial cursor or sequence"));
        }
        self.policy.refresh();
        if !exists {
            self.log
                .register(source, &descriptor, &initial)
                .map_err(authority)?;
        }
        let batch = self.plan(source, Path::new("temporal-stream"), frame)?;
        let outcome = self.log.commit(batch).map_err(authority)?;
        let receipt = StreamReceipt {
            event_id: e.id,
            sequence: e.sequence,
            cursor,
            end: outcome.raw_end,
            outcome: outcome.clone(),
        };
        self.finish_append(outcome)?;
        Ok(receipt)
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_line(
    ctx: &mut FileCtx,
    line: &[u8],
    offset: u64,
    red: &redact::Redactor,
    rc: &mut redact::Counts,
    pc: &mut ParseCounts,
    out: &mut Vec<Rec>,
) -> Result<()> {
    let mut frame = line.to_vec();
    frame.push(b'\n');
    let e = Envelope::parse(&frame)?;
    if ctx.inherit_below.checked_add(1) != Some(i64::from(e.sequence))
        || ctx.seq == u32::MAX
        || ctx.turn == u32::MAX
    {
        return Err(Error::Invalid("temporal sequence differs"));
    }
    let stream: Stream = serde_json::from_str(&ctx.parent_thread).map_err(|_| Error::Corrupt)?;
    stream.bytes()?;
    let tool = e.tool.as_deref().unwrap_or("");
    let call = e.operation_id.as_deref().unwrap_or("");
    let mut fields = vec![
        ("adapter", "temporal-stream-v1".into()),
        ("event_id", e.id.clone()),
        ("namespace", stream.namespace),
        ("workflow_id", stream.workflow_id),
        ("run_id", stream.run_id),
        ("source_generation", stream.generation),
        (
            "attributes",
            serde_json::to_string(&e.attributes).map_err(storage)?,
        ),
    ];
    let text = match &e.payload {
        serde_json::Value::String(v) => v.clone(),
        v => v.to_string(),
    };
    let kind = match e.kind {
        Kind::Input => kind::USER_INPUT,
        Kind::Output => kind::ASSISTANT,
        Kind::Thinking => kind::THINKING,
        Kind::System => kind::SYSTEM,
        Kind::ToolCall => kind::TOOL_CALL,
        Kind::ToolResult => kind::TOOL_RESULT,
    };
    if kind == kind::TOOL_CALL {
        if ctx.calls.len() >= FileCtx::MAX_OPEN_CALLS || ctx.calls.iter().any(|c| c.0 == call) {
            return Err(Error::Invalid("temporal open call identity or capacity"));
        }
        if crate::marker::is_oojuro_call(tool, "", &text) {
            if ctx.lookups.len() >= FileCtx::MAX_OPEN_CALLS {
                return Err(Error::Invalid("temporal lookup capacity"));
            }
            ctx.open_lookup(call);
            crate::marker::mark(&mut fields, crate::marker::CALL);
        }
    }
    if kind == kind::TOOL_RESULT {
        let old = ctx
            .calls
            .iter()
            .find(|c| c.0 == call)
            .ok_or(Error::Invalid("temporal result without retained call"))?;
        if old.1 != tool {
            return Err(Error::Invalid("temporal result tool differs"));
        }
        if ctx.close_lookup(call) {
            crate::marker::mark(&mut fields, crate::marker::RESULT);
        }
    }
    let empty = text.is_empty();
    if empty {
        fields.push(("empty_payload", "true".into()));
    }
    let mut emitter = Emitter {
        red,
        rc,
        pc,
        out,
        line: offset,
        k: 0,
    };
    emitter.emit(
        ctx,
        e.valid_us,
        kind,
        tool,
        if empty {
            "[empty temporal payload]"
        } else {
            &text
        },
        fields,
        match e.status {
            Some(State::Ok) => status::OK,
            Some(State::Error) => status::ERROR,
            None => status::NONE,
        },
        call,
    );
    ctx.user_from = e.id;
    ctx.inherit_below = i64::from(e.sequence);
    Ok(())
}
