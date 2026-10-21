//! Values the front door returns (library, socket protocol, CLI `--json`,
//! MCP): all plain serde structs.
use serde::{Deserialize, Serialize};

/// What `remember` stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Remembered {
    pub id: u64,
    pub writer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Version number under the key (1 for the first; 0 without a key).
    pub version: u32,
    pub ts_us: i64,
    pub ts: String,
    /// Secrets masked before storing (`[REDACTED:<kind>:<hash8>]`).
    pub redacted: u64,
    pub bytes: usize,
}

/// Filters shared by search, range and export. Times are µs since the epoch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    /// Valid time at or after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_us: Option<i64>,
    /// Valid time at or before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_us: Option<i64>,
    /// The view: only records valid by then exist, and a key's newest
    /// version is the newest by then (default: now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_us: Option<i64>,
    /// Kind names (`explicit`, `user_input`, `assistant`, `thinking`,
    /// `tool_call`, `tool_result`, `system`); empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Only explicit records under this key (of the writers searched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Writer IDs; empty = every writer of the journal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<String>,
    /// Leave out what is still in an agent's context window: the records of
    /// this session (16 hex digits) from its last context boundary (a
    /// compaction) on, or all of them when it has none
    /// ([`crate::Reader::context_start`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_context_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    pub k: usize,
    #[serde(default)]
    pub filters: Filters,
    /// Excerpt size around the best match, in bytes.
    pub excerpt_bytes: usize,
    /// Longest accepted query (the caller's `max_query_bytes` setting).
    pub max_query_bytes: usize,
}

/// A search hit: where the record is and an exact excerpt of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ref {
    pub id: u64,
    pub writer: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub ts_us: i64,
    pub ts: String,
    /// Bits the record saves on the query (higher = more relevant).
    pub score_bits: f64,
    /// Byte range of `excerpt` in the record's text: `text[start..end] == excerpt`.
    pub start: usize,
    pub end: usize,
    /// Length of the whole record text in bytes.
    pub len: usize,
    pub excerpt: String,
    /// Session (16 hex digits) and turn of the record (the recency prior of
    /// the per-turn injection reads them).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    #[serde(default)]
    pub turn: u32,
}

/// A whole record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub writer: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Version under the key (explicit records), else the sequence number
    /// in its session.
    pub seq: u32,
    /// Valid time (when it happened).
    pub ts_us: i64,
    pub ts: String,
    /// When it was recorded, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_us: Option<i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// Session (16 hex digits) and turn.
    pub session: String,
    pub turn: u32,
    /// Inferred and recorded metadata (sender, source position, ...).
    #[serde(default)]
    pub meta: serde_json::Value,
    pub text: String,
}

/// What `forget` removes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Target {
    /// One record.
    Id { id: u64 },
    /// Every version of a key of one writer.
    Key { key: String, writer: String },
    /// Everything with valid time before `before_us` (of one writer, or all).
    OlderThan {
        before_us: i64,
        #[serde(default)]
        writer: Option<String>,
    },
    /// Everything with valid time in `[from_us, to_us]` (`forget --last 1h`).
    Window {
        from_us: i64,
        to_us: i64,
        #[serde(default)]
        writer: Option<String>,
    },
}

/// Erasure proof after the reclaim (`docs/FRONT_DOOR.md`, "forget").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proof {
    /// Texts of forgotten records searched for (all, or a sample of 64).
    pub needles: usize,
    /// Engine rows decoded in the affected writer stores.
    pub rows_scanned: u64,
    /// Rows still holding a needle that nothing live needs (must be 0).
    pub dead_rows_with_text: u64,
    /// Rows holding a needle because another live record has the same text.
    pub live_rows_with_text: u64,
    /// Files of the store read byte for byte.
    pub files_scanned: u64,
    /// Files whose raw bytes contain a needle no live record holds (must be 0).
    pub files_with_text: u64,
    pub proven: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgetReport {
    pub forgotten: usize,
    pub writers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterStats {
    pub writer: String,
    pub records: u64,
    /// Records by kind name (kinds with none are left out).
    pub kinds: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub store: String,
    /// `oojurod (pid N)` when the recorder answered, else `direct`.
    pub served_by: String,
    pub records: u64,
    pub kinds: std::collections::BTreeMap<String, u64>,
    pub writers: Vec<WriterStats>,
    pub disk_bytes: u64,
    pub disk: std::collections::BTreeMap<String, u64>,
}
