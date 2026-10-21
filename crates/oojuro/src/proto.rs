//! Requests, shared by the recorder's socket and direct access.
//!
//! Wire format (Unix socket `~/.oojuro/oojurod.sock`): one JSON request per
//! connection, one line; the answer is one line `{"ok":true,"result":...}`
//! or `{"ok":false,"error":"..."}`. `export` answers with one line per
//! record (`{"ok":true,"item":{...}}`) and a last line with the count.
use crate::{
    Result,
    append::NewRecord,
    journal::{Journal, Owner, Reader},
    types::{Filters, SearchArgs, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Who is serving (protocol version, store, pid).
    Hello,
    Remember {
        writer: String,
        text: String,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        at_us: Option<i64>,
    },
    Search(SearchArgs),
    GetId {
        id: u64,
    },
    GetKey {
        writer: String,
        key: String,
        #[serde(default)]
        as_of_us: Option<i64>,
    },
    History {
        writer: String,
        key: String,
    },
    Range {
        #[serde(default)]
        filters: Filters,
        limit: usize,
    },
    Forget {
        target: Target,
        #[serde(default = "yes")]
        proof: bool,
    },
    /// Records matching the filters (what `range`/`export` would return).
    Count {
        #[serde(default)]
        filters: Filters,
    },
    Stats,
    Export {
        #[serde(default)]
        filters: Filters,
    },
    /// Records captured from no log file (the model API proxy): one
    /// writer's batch, redacted and settings-checked by the store owner.
    Ingest {
        via: String,
        source: String,
        records: Vec<oojuro_capture::DirectRec>,
    },
    /// Records from no log file (Claude Code hooks, OpenTelemetry): see
    /// [`crate::append`].
    Append {
        records: Vec<NewRecord>,
    },
}

impl Request {
    /// Writes go through the journal's single writer.
    pub fn is_write(&self) -> bool {
        matches!(self, Request::Remember { .. } | Request::Forget { .. } | Request::Ingest { .. } | Request::Append { .. })
    }
}

/// Answer a read request.
pub fn read(r: &Reader, req: &Request) -> Result<Value> {
    Ok(match req {
        Request::Search(a) => serde_json::to_value(r.search(a)?)?,
        Request::GetId { id } => serde_json::to_value(r.get_id(*id)?)?,
        Request::GetKey { writer, key, as_of_us } => serde_json::to_value(r.get_key(writer, key, *as_of_us)?)?,
        Request::History { writer, key } => serde_json::to_value(r.history(writer, key)?)?,
        Request::Range { filters, limit } => serde_json::to_value(r.range(filters, *limit)?)?,
        Request::Stats => serde_json::to_value(r.stats()?)?,
        Request::Count { filters } => serde_json::to_value(r.count(filters)?)?,
        Request::Hello
        | Request::Export { .. }
        | Request::Remember { .. }
        | Request::Forget { .. }
        | Request::Ingest { .. }
        | Request::Append { .. } => {
            return Err("not a read request".into());
        }
    })
}

/// Carry out a write request (or a read) as the journal's owner.
pub fn execute_on(o: &mut Owner, req: &Request) -> Result<Value> {
    Ok(match req {
        Request::Remember { writer, text, key, at_us } => {
            serde_json::to_value(o.remember(writer, text, key.as_deref(), *at_us)?)?
        }
        Request::Forget { target, proof } => serde_json::to_value(o.forget(target, *proof)?)?,
        Request::Ingest { via, source, records } => serde_json::to_value(o.ingest(via, source, records.clone())?)?,
        Request::Append { records } => serde_json::to_value(o.append(records)?)?,
        other => read(o.reader, other)?,
    })
}

/// [`execute_on`], taking the request (a batch to ingest is moved, not copied).
pub fn execute_owned(o: &mut Owner, req: Request) -> Result<Value> {
    match req {
        Request::Ingest { via, source, records } => Ok(serde_json::to_value(o.ingest(&via, &source, records)?)?),
        other => execute_on(o, &other),
    }
}

/// [`execute_on`] for a [`Journal`].
pub fn execute(j: &mut Journal, req: &Request) -> Result<Value> {
    execute_on(&mut j.owner(), req)
}
