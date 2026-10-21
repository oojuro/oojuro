//! One shape for OTLP spans and log records, decoded from protobuf
//! (`application/x-protobuf`) or OTLP/JSON (`application/json`: lowerCamelCase
//! field names, ids in hex, 64-bit integers as strings or numbers; the
//! protobuf field names are accepted too).
use super::pb;
use prost::Message;
use serde_json::Value;

/// An attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum Any {
    Str(String),
    Bool(bool),
    Int(i64),
    Double(f64),
    Array(Vec<Any>),
    Map(Vec<(String, Any)>),
    Bytes(usize),
    Empty,
}

pub type Attrs = Vec<(String, Any)>;

impl Any {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Any::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Any::Int(i) => Some(*i),
            Any::Double(d) if d.fract() == 0.0 => Some(*d as i64),
            Any::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// As JSON (maps keep their order).
    pub fn to_json(&self) -> Value {
        match self {
            Any::Str(s) => Value::String(s.clone()),
            Any::Bool(b) => Value::Bool(*b),
            Any::Int(i) => Value::from(*i),
            Any::Double(d) => serde_json::Number::from_f64(*d).map(Value::Number).unwrap_or(Value::Null),
            Any::Array(a) => Value::Array(a.iter().map(Any::to_json).collect()),
            Any::Map(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
            Any::Bytes(n) => Value::String(format!("[bytes:{n}]")),
            Any::Empty => Value::Null,
        }
    }

    /// Text of a value: a string as it is, anything else as JSON.
    pub fn text(&self) -> String {
        match self {
            Any::Str(s) => s.clone(),
            Any::Empty => String::new(),
            other => other.to_json().to_string(),
        }
    }
}

pub fn get<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a Any> {
    attrs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

pub fn get_str<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a str> {
    get(attrs, key).and_then(Any::as_str).filter(|s| !s.is_empty())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    pub time_ns: u64,
    pub name: String,
    pub attrs: Attrs,
    /// Log record body (span events have none).
    pub body: Option<Any>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: String,
    pub name: String,
    pub start_ns: u64,
    pub end_ns: u64,
    pub attrs: Attrs,
    pub events: Vec<Event>,
    /// 0 unset, 1 ok, 2 error.
    pub status_code: i32,
    pub status_message: String,
}

/// A log record (an event emitted through the logs signal).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Log {
    pub trace_id: String,
    pub span_id: String,
    pub event: Event,
}

/// Spans (or log records) of one resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch<T> {
    pub resource: Attrs,
    pub scope: String,
    pub items: Vec<T>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_pb_any(v: &pb::AnyValue) -> Any {
    use pb::any_value::Value as V;
    match &v.value {
        Some(V::StringValue(s)) => Any::Str(s.clone()),
        Some(V::BoolValue(b)) => Any::Bool(*b),
        Some(V::IntValue(i)) => Any::Int(*i),
        Some(V::DoubleValue(d)) => Any::Double(*d),
        Some(V::ArrayValue(a)) => Any::Array(a.values.iter().map(from_pb_any).collect()),
        Some(V::KvlistValue(l)) => Any::Map(from_pb_attrs(&l.values)),
        Some(V::BytesValue(b)) => Any::Bytes(b.len()),
        None => Any::Empty,
    }
}

fn from_pb_attrs(kv: &[pb::KeyValue]) -> Attrs {
    kv.iter().map(|x| (x.key.clone(), x.value.as_ref().map_or(Any::Empty, from_pb_any))).collect()
}

/// Decode an `ExportTraceServiceRequest` (protobuf).
pub fn traces_from_pb(body: &[u8]) -> Result<Vec<Batch<Span>>, String> {
    let req = pb::ExportTraceServiceRequest::decode(body).map_err(|e| format!("bad protobuf: {e}"))?;
    let mut out = Vec::new();
    for rs in &req.resource_spans {
        let resource = rs.resource.as_ref().map(|r| from_pb_attrs(&r.attributes)).unwrap_or_default();
        for ss in &rs.scope_spans {
            let items = ss
                .spans
                .iter()
                .map(|s| Span {
                    trace_id: hex(&s.trace_id),
                    span_id: hex(&s.span_id),
                    parent_span_id: hex(&s.parent_span_id),
                    name: s.name.clone(),
                    start_ns: s.start_time_unix_nano,
                    end_ns: s.end_time_unix_nano,
                    attrs: from_pb_attrs(&s.attributes),
                    events: s
                        .events
                        .iter()
                        .map(|e| Event {
                            time_ns: e.time_unix_nano,
                            name: e.name.clone(),
                            attrs: from_pb_attrs(&e.attributes),
                            body: None,
                        })
                        .collect(),
                    status_code: s.status.as_ref().map_or(0, |x| x.code),
                    status_message: s.status.as_ref().map(|x| x.message.clone()).unwrap_or_default(),
                })
                .collect();
            let scope = ss.scope.as_ref().map(|x| x.name.clone()).unwrap_or_default();
            out.push(Batch { resource: resource.clone(), scope, items });
        }
    }
    Ok(out)
}

/// Decode an `ExportLogsServiceRequest` (protobuf).
pub fn logs_from_pb(body: &[u8]) -> Result<Vec<Batch<Log>>, String> {
    let req = pb::ExportLogsServiceRequest::decode(body).map_err(|e| format!("bad protobuf: {e}"))?;
    let mut out = Vec::new();
    for rl in &req.resource_logs {
        let resource = rl.resource.as_ref().map(|r| from_pb_attrs(&r.attributes)).unwrap_or_default();
        for sl in &rl.scope_logs {
            let items = sl
                .log_records
                .iter()
                .map(|l| Log {
                    trace_id: hex(&l.trace_id),
                    span_id: hex(&l.span_id),
                    event: Event {
                        time_ns: if l.time_unix_nano > 0 { l.time_unix_nano } else { l.observed_time_unix_nano },
                        name: l.event_name.clone(),
                        attrs: from_pb_attrs(&l.attributes),
                        body: l.body.as_ref().map(from_pb_any),
                    },
                })
                .collect();
            let scope = sl.scope.as_ref().map(|x| x.name.clone()).unwrap_or_default();
            out.push(Batch { resource: resource.clone(), scope, items });
        }
    }
    Ok(out)
}

/// A field by its JSON name or its protobuf name.
fn field<'a>(v: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| v.get(snake)).filter(|x| !x.is_null())
}

fn arr<'a>(v: &'a Value, camel: &str, snake: &str) -> Result<&'a [Value], String> {
    match field(v, camel, snake) {
        None => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(_) => Err(format!("`{camel}` must be an array")),
    }
}

fn u64_of(v: &Value, camel: &str, snake: &str) -> Result<u64, String> {
    match field(v, camel, snake) {
        None => Ok(0),
        Some(Value::String(s)) => s.parse().map_err(|_| format!("`{camel}` is not an integer")),
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| format!("`{camel}` is not an unsigned integer")),
        Some(_) => Err(format!("`{camel}` is not an integer")),
    }
}

fn str_of(v: &Value, camel: &str, snake: &str) -> String {
    field(v, camel, snake).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Ids are hex in OTLP/JSON; lowercased, anything else rejected.
fn id_of(v: &Value, camel: &str, snake: &str) -> Result<String, String> {
    let s = str_of(v, camel, snake);
    if s.len() > 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("`{camel}` must be hex"));
    }
    Ok(s.to_ascii_lowercase())
}

fn any_from_json(v: &Value, depth: usize) -> Result<Any, String> {
    if depth > 64 {
        return Err("attribute values nest too deep".into());
    }
    let Some(o) = v.as_object() else { return Err("an AnyValue must be an object".into()) };
    if let Some(s) = field(v, "stringValue", "string_value") {
        return s.as_str().map(|s| Any::Str(s.into())).ok_or_else(|| "stringValue must be a string".into());
    }
    if let Some(b) = field(v, "boolValue", "bool_value") {
        return b.as_bool().map(Any::Bool).ok_or_else(|| "boolValue must be a boolean".into());
    }
    if let Some(i) = field(v, "intValue", "int_value") {
        return match i {
            Value::String(s) => s.parse().map(Any::Int).map_err(|_| "intValue is not an integer".into()),
            Value::Number(n) => n.as_i64().map(Any::Int).ok_or_else(|| "intValue is not an integer".into()),
            _ => Err("intValue is not an integer".into()),
        };
    }
    if let Some(d) = field(v, "doubleValue", "double_value") {
        return match d {
            Value::Number(n) => n.as_f64().map(Any::Double).ok_or_else(|| "doubleValue is not a number".into()),
            Value::String(s) => s.parse().map(Any::Double).map_err(|_| "doubleValue is not a number".into()),
            _ => Err("doubleValue is not a number".into()),
        };
    }
    if let Some(a) = field(v, "arrayValue", "array_value") {
        let vals = arr(a, "values", "values")?;
        return Ok(Any::Array(vals.iter().map(|x| any_from_json(x, depth + 1)).collect::<Result<_, _>>()?));
    }
    if let Some(m) = field(v, "kvlistValue", "kvlist_value") {
        return Ok(Any::Map(attrs_from_json(arr(m, "values", "values")?, depth + 1)?));
    }
    if let Some(b) = field(v, "bytesValue", "bytes_value") {
        return Ok(Any::Bytes(b.as_str().map_or(0, |s| s.len() * 3 / 4)));
    }
    if o.is_empty() {
        return Ok(Any::Empty);
    }
    Err("unknown AnyValue".into())
}

fn attrs_from_json(kvs: &[Value], depth: usize) -> Result<Attrs, String> {
    kvs.iter()
        .map(|kv| {
            let key = kv.get("key").and_then(Value::as_str).ok_or("an attribute needs a string `key`")?;
            let value = match kv.get("value") {
                None | Some(Value::Null) => Any::Empty,
                Some(v) => any_from_json(v, depth)?,
            };
            Ok((key.to_string(), value))
        })
        .collect()
}

fn resource_of(v: &Value) -> Result<Attrs, String> {
    match field(v, "resource", "resource") {
        Some(r) => attrs_from_json(arr(r, "attributes", "attributes")?, 0),
        None => Ok(Vec::new()),
    }
}

fn scope_of(v: &Value) -> String {
    field(v, "scope", "scope").map(|s| str_of(s, "name", "name")).unwrap_or_default()
}

/// Decode an OTLP/JSON traces request.
pub fn traces_from_json(body: &[u8]) -> Result<Vec<Batch<Span>>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad JSON: {e}"))?;
    if !v.is_object() {
        return Err("the body must be a JSON object".into());
    }
    let mut out = Vec::new();
    for rs in arr(&v, "resourceSpans", "resource_spans")? {
        let resource = resource_of(rs)?;
        for ss in arr(rs, "scopeSpans", "scope_spans")? {
            let mut items = Vec::new();
            for s in arr(ss, "spans", "spans")? {
                let mut events = Vec::new();
                for e in arr(s, "events", "events")? {
                    events.push(Event {
                        time_ns: u64_of(e, "timeUnixNano", "time_unix_nano")?,
                        name: str_of(e, "name", "name"),
                        attrs: attrs_from_json(arr(e, "attributes", "attributes")?, 0)?,
                        body: None,
                    });
                }
                let status = field(s, "status", "status");
                items.push(Span {
                    trace_id: id_of(s, "traceId", "trace_id")?,
                    span_id: id_of(s, "spanId", "span_id")?,
                    parent_span_id: id_of(s, "parentSpanId", "parent_span_id")?,
                    name: str_of(s, "name", "name"),
                    start_ns: u64_of(s, "startTimeUnixNano", "start_time_unix_nano")?,
                    end_ns: u64_of(s, "endTimeUnixNano", "end_time_unix_nano")?,
                    attrs: attrs_from_json(arr(s, "attributes", "attributes")?, 0)?,
                    events,
                    status_code: status.and_then(|x| x.get("code")).and_then(Value::as_i64).unwrap_or(0) as i32,
                    status_message: status.map(|x| str_of(x, "message", "message")).unwrap_or_default(),
                });
            }
            out.push(Batch { resource: resource.clone(), scope: scope_of(ss), items });
        }
    }
    Ok(out)
}

/// Decode an OTLP/JSON logs request.
pub fn logs_from_json(body: &[u8]) -> Result<Vec<Batch<Log>>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad JSON: {e}"))?;
    if !v.is_object() {
        return Err("the body must be a JSON object".into());
    }
    let mut out = Vec::new();
    for rl in arr(&v, "resourceLogs", "resource_logs")? {
        let resource = resource_of(rl)?;
        for sl in arr(rl, "scopeLogs", "scope_logs")? {
            let mut items = Vec::new();
            for l in arr(sl, "logRecords", "log_records")? {
                let t = u64_of(l, "timeUnixNano", "time_unix_nano")?;
                items.push(Log {
                    trace_id: id_of(l, "traceId", "trace_id")?,
                    span_id: id_of(l, "spanId", "span_id")?,
                    event: Event {
                        time_ns: if t > 0 { t } else { u64_of(l, "observedTimeUnixNano", "observed_time_unix_nano")? },
                        name: str_of(l, "eventName", "event_name"),
                        attrs: attrs_from_json(arr(l, "attributes", "attributes")?, 0)?,
                        body: field(l, "body", "body").map(|b| any_from_json(b, 0)).transpose()?,
                    },
                });
            }
            out.push(Batch { resource: resource.clone(), scope: scope_of(sl), items });
        }
    }
    Ok(out)
}
