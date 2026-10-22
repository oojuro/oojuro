//! Explicit existing native authority only. No legacy reader, write operation or wire-selected path.
use super::{Config, Error, Harness, LocalCapture, Options, config, policy, records};
use oojuro_engine::{receipts, record};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::Write, path::Path};
pub(super) const REQUEST_BYTES: usize = 64 << 10;
pub(super) const OUTPUT_BYTES: usize = 16 << 20;
pub(super) type Failure = &'static str;
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
pub(super) enum Namespace {
    #[serde(rename = "record-v2")]
    Record,
    #[serde(rename = "capture-v1")]
    Capture,
}
impl Namespace {
    pub fn parse(s: &str) -> Result<Self, Failure> {
        match s {
            "record-v2" => Ok(Self::Record),
            "capture-v1" => Ok(Self::Capture),
            _ => Err("invalid_namespace"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Record => "record-v2",
            Self::Capture => "capture-v1",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    namespace: Namespace,
    request: Value,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Op {
    Search,
    Get,
    History,
    Range,
    Export,
    Stats,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    op: Op,
    query: Option<String>,
    id: Option<u64>,
    key: Option<String>,
    k: Option<usize>,
    from_us: Option<i64>,
    valid_at_us: Option<i64>,
    known_at_us: Option<i64>,
    to_us: Option<i64>,
    kind: Option<record::Kind>,
    status: Option<record::Status>,
    tool: Option<String>,
    reported_agent: Option<String>,
    versions: Option<record::Versions>,
    start: Option<u64>,
    end: Option<u64>,
    cursor: Option<String>,
    entries: Option<usize>,
    bytes: Option<usize>,
}
fn label(value: &Option<String>, max: usize) -> bool {
    value
        .as_ref()
        .is_none_or(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
impl Params {
    fn parse(value: Value, namespace: Namespace) -> Result<Self, Failure> {
        let map = value.as_object().ok_or("invalid_request")?;
        let op: Op = serde_json::from_value(map.get("op").ok_or("invalid_request")?.clone())
            .map_err(|_| "invalid_operation")?;
        let common = ["op", "bytes"];
        let cuts = ["from_us", "valid_at_us", "known_at_us"];
        let filters = ["kind", "status", "tool", "reported_agent"];
        let allowed = |name: &str| {
            if common.contains(&name) {
                return true;
            }
            match (namespace, op) {
                (Namespace::Record, Op::Search) => {
                    ["query", "k"].contains(&name)
                        || cuts.contains(&name)
                        || filters.contains(&name)
                }
                (Namespace::Record, Op::Get) => {
                    ["id", "key"].contains(&name) || cuts.contains(&name)
                }
                (Namespace::Record, Op::History) => {
                    ["key", "entries"].contains(&name) || cuts.contains(&name)
                }
                (Namespace::Record, Op::Range | Op::Export) => {
                    ["versions", "entries", "cursor"].contains(&name)
                        || cuts.contains(&name)
                        || filters.contains(&name)
                }
                (Namespace::Record, Op::Stats) => {
                    name == "versions" || cuts.contains(&name) || filters.contains(&name)
                }
                (Namespace::Capture, Op::Search) => {
                    ["query", "k", "from_us", "to_us"].contains(&name)
                }
                (Namespace::Capture, Op::Get) => ["id", "start", "end", "cursor"].contains(&name),
                (Namespace::Capture, Op::Range | Op::Export) => {
                    ["start", "end", "cursor", "entries"].contains(&name)
                }
                (Namespace::Capture, Op::Stats) => ["start", "end", "cursor"].contains(&name),
                _ => false,
            }
        };
        if map.keys().any(|name| !allowed(name)) {
            return Err("invalid_arguments");
        }
        if namespace == Namespace::Record
            && map.get("cursor").is_some_and(|v| !v.is_null())
            && map
                .keys()
                .any(|name| !["op", "cursor", "entries", "bytes"].contains(&name.as_str()))
        {
            return Err("cursor_arguments_conflict");
        }
        let p: Self = serde_json::from_value(value).map_err(|_| "invalid_arguments")?;
        if !label(&p.key, 1024)
            || !label(&p.tool, 1024)
            || !label(&p.reported_agent, 256)
            || p.query.as_ref().is_some_and(|q| q.len() > REQUEST_BYTES)
            || p.cursor
                .as_ref()
                .is_some_and(|c| c.is_empty() || c.len() > 16 << 10)
            || p.k.is_some_and(|k| k == 0 || k > 1000)
            || p.entries.is_some_and(|n| n == 0 || n > 1000)
            || p.id == Some(0)
        {
            return Err("argument_bound");
        }
        let bytes = p.bytes.unwrap_or(1 << 20);
        if !(2..=OUTPUT_BYTES).contains(&bytes) {
            return Err("output_bound");
        }
        Ok(p)
    }
    fn cuts(&self) -> Result<record::AsOf, Failure> {
        let c = record::AsOf {
            from: self.from_us.unwrap_or(i64::MIN),
            valid_at: self.valid_at_us.unwrap_or(i64::MAX),
            known_at: self.known_at_us.unwrap_or(i64::MAX),
        };
        if c.from > c.valid_at {
            return Err("invalid_time_range");
        }
        Ok(c)
    }
    fn filter(&self) -> record::Filter {
        record::Filter {
            kind: self.kind,
            tool: self.tool.clone(),
            status: self.status,
            agent_id: self.reported_agent.clone(),
        }
    }
    fn cap(&self) -> usize {
        self.bytes.unwrap_or(1 << 20).saturating_sub(128)
    }
    fn limits(&self) -> record::Limits {
        record::Limits {
            entries: self.entries.unwrap_or(100),
            bytes: self.cap(),
        }
    }
}
pub(super) enum Owner {
    Record {
        config: records::Config,
        log: record::RecordLog,
        index: record::Index,
    },
    Capture {
        config: Config,
        capture: LocalCapture,
    },
}
impl Owner {
    pub fn open(root: &Path, namespace: Namespace) -> Result<Self, Error> {
        config::check_private_path(root)?;
        match namespace {
            Namespace::Record => {
                let (config, key) = records::load(root)?;
                let log = record::RecordLog::open_existing(
                    &root.join(config::JOURNAL),
                    &key,
                    config.identity(),
                )
                .map_err(Error::Record)?;
                let index = record::Index::rebuild(&log).map_err(Error::Record)?;
                Ok(Self::Record { config, log, index })
            }
            Namespace::Capture => {
                let (config, key) = config::load(root)?;
                let mut options = Options::new(&config.writer, config.agent, "native-read-only-1");
                options.first_id = config.first_id;
                options.raw_authorization = "native-cli-explicit-source-1".into();
                let journal = root.join(config::JOURNAL);
                let capture = match config.harness {
                    Harness::Codex => LocalCapture::open_existing(
                        &journal,
                        &key,
                        options,
                        Box::new(policy::ReadOnly),
                    ),
                    Harness::ClaudeCode => LocalCapture::open_claude_existing(
                        &journal,
                        &key,
                        options,
                        Box::new(policy::ReadOnly),
                    ),
                    Harness::Pi => LocalCapture::open_pi_existing(
                        &journal,
                        &key,
                        options,
                        Box::new(policy::ReadOnly),
                    ),
                    Harness::Temporal => LocalCapture::open_temporal_existing(
                        &journal,
                        &key,
                        options,
                        Box::new(policy::ReadOnly),
                    ),
                }
                .map_err(|_| Error::Authority)?;
                Ok(Self::Capture { config, capture })
            }
        }
    }
    pub fn namespace(&self) -> Namespace {
        match self {
            Self::Record { .. } => Namespace::Record,
            Self::Capture { .. } => Namespace::Capture,
        }
    }
    pub fn execute(&self, envelope: Value) -> Result<Value, Failure> {
        let envelope: Envelope = serde_json::from_value(envelope).map_err(|_| "invalid_request")?;
        if envelope.version != 1 || envelope.namespace != self.namespace() {
            return Err("namespace_or_version_refused");
        }
        let p = Params::parse(envelope.request, self.namespace())?;
        let result=match self {
            Self::Record{config,log,index}=>{
                let cap=p.cap();
                if let Some(cursor)=&p.cursor {
                    serde_json::to_value(record::View::continue_page(log,config.scope(),cursor,p.limits()).map_err(|_|"cursor_refused")?)
                } else {
                    let cuts=p.cuts()?;
                    let view=record::View::open(log,config.scope(),cuts).map_err(|_|"authority_refused")?;
                    match p.op {
                        Op::Search=>serde_json::to_value(index.search_view(log,&view,&record::Query{
                            scope:config.scope(),as_of:cuts,text:p.query.as_deref().ok_or("query_required")?,
                            k:p.k.unwrap_or(10),filter:p.filter(),max_bytes:cap,
                        }).map_err(|_|"query_refused")?),
                        Op::Get=> {
                            let record=match (p.id,p.key.as_deref()) {
                                (Some(id),None)=>view.get_id(log,id,cap),
                                (None,Some(key))=>view.get_key(log,key,cap),
                                _=>return Err("id_or_key_required"),
                            }.map_err(|_|"read_refused")?;
                            serde_json::to_value(json!({"watermark":view.watermark(),"record":record}))
                        }
                        Op::History=>serde_json::to_value(view.history(log,p.key.as_deref().ok_or("key_required")?,p.limits()).map_err(|_|"read_refused")?),
                        Op::Range|Op::Export=>serde_json::to_value(view.range(log,p.versions.unwrap_or(record::Versions::Current),p.filter(),p.limits()).map_err(|_|"read_refused")?),
                        Op::Stats=>serde_json::to_value(view.stats(log,p.versions.unwrap_or(record::Versions::Current),p.filter(),cap).map_err(|_|"read_refused")?),
                    }
                }
            }
            Self::Capture{config,capture}=>{
                if matches!(p.op,Op::History){return Err("unsupported_operation");}
                let log=capture.authority().map_err(|_|"authority_refused")?;
                let scope=receipts::Scope{writer:&config.writer,agent:config.agent};
                let range=receipts::Range{start:p.start.unwrap_or(0),end:p.end.unwrap_or(u64::MAX)};
                let view=match &p.cursor {
                    Some(cursor)=>receipts::ReceiptView::resume(log,scope,range,cursor),
                    None=>receipts::ReceiptView::open(log,scope,range),
                }.map_err(|_|"cursor_or_range_refused")?;
                let cap=p.cap();
                match p.op {
                    Op::Search=>{
                        let from=p.from_us.unwrap_or(i64::MIN);let to=p.to_us.unwrap_or(i64::MAX);
                        if from>to{return Err("invalid_time_range");}
                        let hits=capture.search(p.query.as_deref().ok_or("query_required")?,from,to,p.k.unwrap_or(10))
                            .map_err(|_|"query_refused")?;
                        let mut out=vec![];let mut used=0usize;
                        for h in hits {
                            let receipt=view.get(log,h.hit.id,cap.saturating_sub(used)).map_err(|_|"output_bound")?;
                            let item=json!({"id":h.hit.id,"score_bits":h.hit.bits,"continued":h.hit.continued,
                                "host_id":h.hit.row,"receipt":receipt});
                            used=used.checked_add(encode(&item,cap.saturating_sub(used))?.len()).ok_or("output_bound")?;
                            out.push(item);
                        }
                        Ok(json!({"watermark":view.watermark(),"hits":out}))
                    }
                    Op::Get=>serde_json::to_value(json!({"watermark":view.watermark(),
                        "receipt":view.get(log,p.id.ok_or("id_required")?,cap).map_err(|_|"read_refused")?})),
                    Op::Range|Op::Export=>serde_json::to_value(view.page(log,receipts::Limits{entries:p.entries.unwrap_or(100),bytes:cap}).map_err(|_|"read_refused")?),
                    Op::Stats=>serde_json::to_value(view.stats(log,cap).map_err(|_|"read_refused")?),
                    Op::History=>unreachable!(),
                }
            }
        }.map_err(|_|"output_unavailable")?;
        encode(&result, p.cap())?;
        Ok(result)
    }
}
pub(super) fn encode(value: &Value, limit: usize) -> Result<Vec<u8>, Failure> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(b.len())
                .is_none_or(|n| n > self.limit)
            {
                return Err(std::io::Error::other("bound"));
            }
            self.bytes.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: vec![],
        limit,
    };
    serde_json::to_writer(&mut out, value).map_err(|_| "output_bound")?;
    Ok(out.bytes)
}
