//! Explicit flat-record namespace. Harness CaptureLog/receipts remain a separate unchanged format.
//! Reported agent identities and stored data are not verified truth or instructions.
mod authority;
pub mod context;
mod index;
pub mod paged;
pub mod persistent;
pub mod projection;
pub mod rank_tables;
pub mod ranking;
mod view;
pub use authority::{Commit, RecordLog, Watermark};
pub use index::{Hit, Index, Query, Search};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use view::{AsOf, Filter, Limits, MAX_OUTPUT_BYTES, Page, Scope, Stats, Versions, View};
pub const MAX_TEXT_BYTES: usize = 16 << 20;
pub const MAX_ATTR_BYTES: usize = 4096;
pub const MAX_RECORDS: usize = 100_000;
pub const MAX_STATE_BYTES: usize = 128 << 20;
pub const MAX_BATCH_BYTES: usize = 32 << 20;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Input,
    Output,
    Thinking,
    ToolCall,
    ToolResult,
    System,
    Explicit,
    Raw,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub agent_id: String,
    pub parent_id: Option<String>,
    /// Event/valid time in UTC microseconds; it never sets the engine ingest clock.
    pub valid_at: i64,
    pub kind: Kind,
    pub tool: Option<String>,
    pub status: Option<Status>,
    pub exit: Option<i32>,
    #[serde(deserialize_with = "present_option")]
    pub key: Option<String>,
    pub text: String,
    pub attrs: BTreeMap<String, serde_json::Value>,
    /// Initial caller eligibility, retained separately from current key visibility.
    pub indexed: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Record {
    pub schema: u32,
    pub id: u64,
    pub writer: String,
    /// Header-bound caller-configured source label, not independent harness attestation.
    pub adapter: String,
    pub agent_id: String,
    pub parent_id: Option<String>,
    pub valid_at: i64,
    /// First successful authority append's engine-assigned UTC microseconds.
    pub recorded_at: i64,
    pub kind: Kind,
    pub tool: Option<String>,
    pub status: Option<Status>,
    pub exit: Option<i32>,
    pub key: Option<String>,
    pub text: String,
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub indexed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Bounds,
    Scope,
    Authority,
    Conflict,
    Unavailable,
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Assigned {
    id: u64,
    recorded_at: i64,
    input: Input,
}
impl Assigned {
    pub(crate) fn record(&self, writer: &str, adapter: &str) -> Record {
        let v = &self.input;
        Record {
            schema: 1,
            id: self.id,
            writer: writer.into(),
            adapter: adapter.into(),
            agent_id: v.agent_id.clone(),
            parent_id: v.parent_id.clone(),
            valid_at: v.valid_at,
            recorded_at: self.recorded_at,
            kind: v.kind,
            tool: v.tool.clone(),
            status: v.status,
            exit: v.exit,
            key: v.key.clone(),
            text: v.text.clone(),
            attrs: v.attrs.clone(),
            indexed: v.indexed,
        }
    }
}
fn name(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
impl Input {
    /// Pure schema and work-bound validation. Authorization and redaction remain caller duties.
    pub fn validate(&self) -> Result<()> {
        if !name(&self.agent_id, 256)
            || self.parent_id.as_ref().is_some_and(|s| !name(s, 256))
            || self.tool.as_ref().is_some_and(|s| !name(s, 1024))
            || self.key.as_ref().is_some_and(|s| !name(s, 1024))
            || self.text.len() > MAX_TEXT_BYTES
        {
            return Err(Error::Bounds);
        }
        // Check nesting/work before serde serialization; programmatic Value trees
        // need the same depth/size bounds as parsed JSON, without a recursive walk.
        if self.attrs.len() > 1024 || self.attrs.keys().any(|k| k.len() > MAX_ATTR_BYTES) {
            return Err(Error::Bounds);
        }
        let mut stack: Vec<_> = self.attrs.values().map(|v| (v, 0usize)).collect();
        let mut nodes = 0usize;
        while let Some((value, depth)) = stack.pop() {
            nodes += 1;
            if nodes > 4096 || depth > 32 {
                return Err(Error::Bounds);
            }
            match value {
                serde_json::Value::Array(v) => {
                    if v.len() > 1024 {
                        return Err(Error::Bounds);
                    }
                    stack.extend(v.iter().map(|v| (v, depth + 1)));
                }
                serde_json::Value::Object(v) => {
                    if v.len() > 1024 || v.keys().any(|k| k.len() > MAX_ATTR_BYTES) {
                        return Err(Error::Bounds);
                    }
                    stack.extend(v.values().map(|v| (v, depth + 1)));
                }
                serde_json::Value::String(s) if s.len() > MAX_ATTR_BYTES => {
                    return Err(Error::Bounds);
                }
                _ => {}
            }
        }
        json(&self.attrs, MAX_ATTR_BYTES)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests;

fn present_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

// All caller-derived serialization is bounded before copying/allocating a batch.
fn json<T: Serialize + ?Sized>(value: &T, limit: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
        full: bool,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(buf.len())
                .is_none_or(|n| n > self.limit)
            {
                self.full = true;
                return Err(std::io::Error::other("serialized bound"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: vec![],
        limit,
        full: false,
    };
    if serde_json::to_writer(&mut out, value).is_err() {
        return Err(if out.full {
            Error::Bounds
        } else {
            Error::Invalid
        });
    }
    Ok(out.bytes)
}

/// Explicit opt-in retained-control lifecycle namespace; no implicit v2 conversion.
pub mod managed;
