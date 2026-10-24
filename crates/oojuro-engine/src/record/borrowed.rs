//! Read-only originals borrowed from a scope-checked immutable RecordLog prefix.
use super::*;

/// Same fields and JSON bytes as Record; only the ownership differs.
#[derive(Clone, Copy, Serialize)]
pub struct RecordRef<'a> {
    pub schema: u32,
    pub id: u64,
    pub writer: &'a str,
    pub adapter: &'a str,
    pub agent_id: &'a str,
    pub parent_id: Option<&'a str>,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub kind: Kind,
    pub tool: Option<&'a str>,
    pub status: Option<Status>,
    pub exit: Option<i32>,
    pub key: Option<&'a str>,
    pub text: &'a str,
    pub attrs: &'a BTreeMap<String, serde_json::Value>,
    pub indexed: bool,
}
impl RecordRef<'_> {
    pub fn into_owned(self) -> Record {
        let Self {
            schema, id, writer, adapter, agent_id, parent_id, valid_at, recorded_at,
            kind, tool, status, exit, key, text, attrs, indexed,
        } = self;
        Record {
            schema, id, writer: writer.into(), adapter: adapter.into(), agent_id: agent_id.into(),
            parent_id: parent_id.map(str::to_owned), valid_at, recorded_at, kind,
            tool: tool.map(str::to_owned), status, exit, key: key.map(str::to_owned),
            text: text.into(), attrs: attrs.clone(), indexed,
        }
    }
}
impl Assigned {
    pub(crate) fn borrowed<'a>(&'a self, writer: &'a str, adapter: &'a str) -> RecordRef<'a> {
        let Input {
            agent_id, parent_id, valid_at, kind, tool, status, exit, key, text, attrs, indexed,
        } = &self.input;
        RecordRef {
            schema: 1, id: self.id, writer, adapter, agent_id,
            parent_id: parent_id.as_deref(), valid_at: *valid_at, recorded_at: self.recorded_at,
            kind: *kind, tool: tool.as_deref(), status: *status, exit: *exit,
            key: key.as_deref(), text, attrs, indexed: *indexed,
        }
    }
}
