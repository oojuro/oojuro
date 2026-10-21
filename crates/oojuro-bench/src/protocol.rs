//! JSON-lines engine-arm protocol (v1). One request object per line on the
//! arm's stdin, one response object per line on its stdout, strictly in order.
//! Times are integer UNIX seconds. Text is UTF-8 and must round-trip exactly.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROTOCOL: &str = "oojuro-arm-v1";

/// One immutable record. `key` names a revision chain (empty means `id`):
/// a later record with the same key supersedes earlier ones as of its times.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Doc {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    pub text: String,
    pub valid_at: i64,
    pub recorded_at: i64,
    /// Free-form attributes, e.g. `session`, `kind` (task|call|result|thinking|
    /// turn), `turn`, `path`, `role`. Arms may use or ignore them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
}

impl Doc {
    pub fn key(&self) -> &str {
        if self.key.is_empty() { &self.id } else { &self.key }
    }
}

/// An as-of search. `context` carries caller state an arm may use for priors
/// (`session`, `turn`, `task`); it never contains labels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchReq {
    pub text: String,
    pub valid_at: i64,
    pub known_at: i64,
    pub k: usize,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hit {
    pub id: String,
    pub score: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArmInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// Source commit the arm binary was built from.
    #[serde(default)]
    pub commit: String,
    /// What `append_batch` guarantees on return, e.g. "fsync per record".
    #[serde(default)]
    pub durability: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello,
    /// Open (creating if absent) the store in `dir`. Reopening an existing
    /// directory must recover every acknowledged batch.
    Open {
        dir: String,
        #[serde(default)]
        config: BTreeMap<String, String>,
    },
    AppendBatch { docs: Vec<Doc> },
    Search { query: SearchReq },
    Get { ids: Vec<String> },
    GetRange { from: i64, to: i64, limit: usize },
    Stats,
    /// Flush and release the store; the process may then exit on EOF.
    Close,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<ArmInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hits: Option<Vec<Hit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texts: Option<Vec<Option<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<Vec<Doc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<serde_json::Value>,
    /// Microseconds spent inside the arm on this request.
    #[serde(default)]
    pub elapsed_us: u64,
}

impl Response {
    pub fn error(message: String) -> Self {
        Self { ok: false, error: Some(message), ..Self::default() }
    }
}
