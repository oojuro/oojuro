//! Storage target: `oojuro_trace::TraceDb` (the v1 record API comes later).
//!
//! Mapping: writer -> one agent store (private, append-only), source
//! transcript -> session, user inputs so far -> turn, record order -> seq.
//! The text is the record text; the tool name rides in `tool`; every other
//! derived field (sender, subtype, model, source position, recorded time)
//! rides in `args` as a small JSON object:
//!
//! `{"src":"<file no>:<line offset>:<index in line>","rt":<recorded µs>,"from":"human",...}`
//!
//! `src` is what makes restarts exact: records beyond the saved state are
//! found by id and their source positions skipped on re-read.
use crate::Rec;
use oojuro_trace::EventIn;

/// The derived-metadata JSON stored with a record.
pub fn args_json(rec: &Rec, file_no: u32, recorded_us: i64) -> String {
    let mut m = serde_json::Map::new();
    m.insert("src".into(), format!("{file_no}:{}:{}", rec.line, rec.k).into());
    m.insert("rt".into(), recorded_us.into());
    for (k, v) in &rec.meta {
        m.insert((*k).into(), v.clone().into());
    }
    serde_json::Value::Object(m).to_string()
}

/// Source position `(file no, line offset, index)` from stored metadata.
pub fn parse_src(args: &str) -> Option<(u32, u64, u32)> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let src = v.get("src")?.as_str()?;
    let mut it = src.split(':');
    let f = it.next()?.parse().ok()?;
    let l = it.next()?.parse().ok()?;
    let k = it.next()?.parse().ok()?;
    Some((f, l, k))
}

/// Whether stored metadata marks a record that comes from no log file
/// (`"via": "proxy"`, `"via": "hook"`, `"via": "otel"`, ...): it has no source position.
pub fn is_external(args: &str) -> bool {
    args.contains("\"via\"")
        && serde_json::from_str::<serde_json::Value>(args).ok().and_then(|v| v.get("via").cloned()).is_some()
}

/// The event for a parsed record; `indexed` comes from the recording policy
/// (by default [`crate::kind::indexed`]).
pub fn to_event<'a>(rec: &'a Rec, id: u64, agent: u32, parent: u64, args: &'a str, indexed: bool) -> EventIn<'a> {
    EventIn {
        id,
        session: rec.session,
        parent_session: rec.parent_session,
        agent,
        turn: rec.turn,
        seq: rec.seq,
        parent,
        revises: 0,
        kind: rec.kind,
        status: rec.status,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: rec.ts_us,
        duration_us: 0,
        tool: &rec.tool,
        args,
        text: &rec.text,
        indexed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn src_roundtrip() {
        let rec = Rec { line: 1234, k: 2, meta: vec![("from", "human".into())], ..Rec::default() };
        let a = args_json(&rec, 7, 99);
        assert_eq!(a, r#"{"from":"human","rt":99,"src":"7:1234:2"}"#);
        assert_eq!(parse_src(&a), Some((7, 1234, 2)));
        assert_eq!(parse_src("{}"), None);
    }
}
