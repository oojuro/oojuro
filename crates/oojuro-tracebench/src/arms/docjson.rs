//! One event as a flat JSON document, shared by the document-store arms
//! (`meili` documents, `qdrant` payloads). Text of indexed kinds goes into
//! `text` (the only full-text field); thinking and system text into `raw`
//! (stored, never indexed), so the indexing rule of `docs/TRACEBENCH.md`
//! holds without filtering by kind at query time. `text_len` (UTF-8 bytes)
//! is stored for the arms that can sum it.
use crate::events::indexed_kind;
use crate::{Result, Row};
use serde_json::{Map, Value, json};

pub(crate) fn doc(r: &Row, with_id: bool) -> Map<String, Value> {
    let mut m = Map::new();
    if with_id {
        m.insert("id".into(), json!(r.id));
    }
    m.insert("session".into(), json!(r.session));
    m.insert("agent".into(), json!(r.agent));
    m.insert("turn".into(), json!(r.turn));
    m.insert("seq".into(), json!(r.seq));
    m.insert("parent".into(), json!(r.parent));
    m.insert("kind".into(), json!(r.kind));
    m.insert("status".into(), json!(r.status));
    m.insert("exit_code".into(), json!(r.exit_code));
    m.insert("tokens_in".into(), json!(r.tokens_in));
    m.insert("tokens_out".into(), json!(r.tokens_out));
    m.insert("ts".into(), json!(r.ts_us));
    m.insert("duration".into(), json!(r.duration_us));
    m.insert("tool".into(), json!(r.tool));
    m.insert("args".into(), json!(r.args));
    m.insert(if indexed_kind(r.kind) { "text" } else { "raw" }.into(), json!(r.text));
    m.insert("text_len".into(), json!(r.text.len()));
    m
}

/// The status fields a revision changes.
pub(crate) fn status_fields(status: u8, exit_code: Option<i32>, duration_us: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), json!(status));
    m.insert("exit_code".into(), json!(exit_code));
    m.insert("duration".into(), json!(duration_us));
    m
}

fn u(v: &Value, k: &str) -> Result<u64> {
    v.get(k).and_then(Value::as_u64).ok_or_else(|| format!("document field {k} missing or not a u64").into())
}

fn i(v: &Value, k: &str) -> Result<i64> {
    v.get(k).and_then(Value::as_i64).ok_or_else(|| format!("document field {k} missing or not an i64").into())
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// A row from a document (`id` given when the store keeps it outside the
/// document, as Qdrant's point id).
pub(crate) fn row(v: &Value, id: Option<u64>) -> Result<Row> {
    Ok(Row {
        id: match id {
            Some(x) => x,
            None => u(v, "id")?,
        },
        session: u(v, "session")?,
        agent: u(v, "agent")? as u32,
        turn: u(v, "turn")? as u32,
        seq: u(v, "seq")? as u32,
        parent: u(v, "parent")?,
        kind: u(v, "kind")? as u8,
        status: u(v, "status")? as u8,
        exit_code: v.get("exit_code").and_then(Value::as_i64).map(|x| x as i32),
        tokens_in: u(v, "tokens_in")? as u32,
        tokens_out: u(v, "tokens_out")? as u32,
        ts_us: i(v, "ts")?,
        duration_us: i(v, "duration")?,
        tool: s(v, "tool"),
        args: s(v, "args"),
        text: match v.get("text").and_then(Value::as_str) {
            Some(t) => t.to_string(),
            None => s(v, "raw"),
        },
    })
}

/// Bytes `start..end` of a row's text (clamped), as `get_range` returns them.
pub(crate) fn slice(text: &str, start: usize, end: usize) -> Vec<u8> {
    let t = text.as_bytes();
    let e = end.min(t.len());
    t[start.min(e)..e].to_vec()
}

/// Lower and upper time bound of a filter, or `None` when it is open
/// (the runner passes `i64::MIN / 2` and `i64::MAX / 2` for "any time").
pub(crate) fn time_bounds(from_us: i64, to_us: i64) -> (Option<i64>, Option<i64>) {
    let lo = (from_us > i64::MIN / 4).then_some(from_us);
    let hi = (to_us < i64::MAX / 4).then_some(to_us);
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{THINKING, TOOL_CALL};

    fn sample(kind: u8, exit_code: Option<i32>) -> Row {
        Row {
            id: 7,
            session: 3,
            agent: 2,
            turn: 1,
            seq: 4,
            parent: 6,
            kind,
            status: 1,
            exit_code,
            tokens_in: 10,
            tokens_out: 20,
            ts_us: 1_787_616_000_000_001,
            duration_us: 0,
            tool: "bash".into(),
            args: "{\"argv\":\"ls -la\"}".into(),
            text: "héllo \"quoted\"\n\ttabs".into(),
        }
    }

    #[test]
    fn document_round_trip_and_text_routing() {
        for (kind, field, exit) in [(TOOL_CALL, "text", Some(-1)), (THINKING, "raw", None)] {
            let r = sample(kind, exit);
            let d = Value::Object(doc(&r, true));
            assert!(d.get(field).is_some(), "{field} holds the text of kind {kind}");
            assert_eq!(d["text_len"], json!(r.text.len()));
            assert_eq!(super::row(&d, None).unwrap(), r);
            let mut no_id = doc(&r, false);
            assert!(!no_id.contains_key("id"));
            no_id.insert("x".into(), json!(1));
            assert_eq!(super::row(&Value::Object(no_id), Some(7)).unwrap(), r);
        }
        assert_eq!(slice("abcdef", 2, 100), b"cdef");
        assert_eq!(slice("abcdef", 9, 3), b"");
        assert_eq!(time_bounds(i64::MIN / 2, i64::MAX / 2), (None, None));
        assert_eq!(time_bounds(5, 9), (Some(5), Some(9)));
    }

    #[test]
    fn unsupported_is_recognised() {
        let e = crate::unsupported::<()>("agg_tools", "no sums").unwrap_err();
        assert_eq!(crate::unsupported_reason(&e).as_deref(), Some("no sums"));
        let other: crate::Error = "boom".into();
        assert_eq!(crate::unsupported_reason(&other), None);
    }
}
