//! Explicit direct Record namespace. Typed input is caller-authorized data, never instructions.
use super::{Arguments, Error, Result, config, emit, emit_bytes};
use oojuro_engine::{
    capture_log::Identity,
    record::{self, AsOf, Filter, Input, Kind, Limits, RecordLog, Scope, Status, Versions, View},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

const CONFIG: &str = "record.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u32,
    writer: String,
    owner: u32,
    first_id: u64,
    adapter: String,
}
impl Config {
    fn check(&self) -> Result<()> {
        let valid =
            |v: &str, limit| !v.is_empty() && v.len() <= limit && !v.chars().any(char::is_control);
        if self.version != 1
            || !valid(&self.writer, 256)
            || self.owner == 0
            || self.first_id == 0
            || !valid(&self.adapter, 128)
        {
            return Err(Error::Config);
        }
        Ok(())
    }
    fn identity(&self) -> Identity {
        Identity {
            writer: self.writer.clone(),
            agent: self.owner,
            first_id: self.first_id,
            adapter: self.adapter.clone(),
        }
    }
    fn scope(&self) -> Scope<'_> {
        Scope {
            writer: &self.writer,
            owner: self.owner,
        }
    }
}
fn load(root: &Path) -> Result<(Config, [u8; 32])> {
    config::check_root(root)?;
    let cfg: Config = serde_json::from_slice(&config::read_private(&root.join(CONFIG), 4096)?)
        .map_err(|_| Error::Config)?;
    cfg.check()?;
    Ok((cfg, config::load_key(root)?))
}
fn map<T>(result: record::Result<T>) -> Result<T> {
    result.map_err(Error::Record)
}
fn bytes(args: &Arguments) -> Result<usize> {
    let n = args.number("--bytes", 1 << 20)?;
    if !(2..=record::MAX_OUTPUT_BYTES).contains(&n) {
        return Err(Error::Bounds);
    }
    Ok(n)
}
fn limits(args: &Arguments) -> Result<Limits> {
    let entries = args.number("--entries", 100)?;
    if !(1..=1000).contains(&entries) {
        return Err(Error::Bounds);
    }
    Ok(Limits {
        entries,
        bytes: bytes(args)? - 1,
    })
}
fn as_of(args: &Arguments) -> Result<AsOf> {
    let cuts = AsOf {
        from: args.number("--from-us", i64::MIN)?,
        valid_at: args.number("--valid-at-us", i64::MAX)?,
        known_at: args.number("--known-at-us", i64::MAX)?,
    };
    if cuts.from > cuts.valid_at {
        return Err(Error::Bounds);
    }
    Ok(cuts)
}
fn filter(args: &Arguments) -> Result<Filter> {
    let kind = match args.value("--kind") {
        None => None,
        Some(v) => Some(match v {
            "input" => Kind::Input,
            "output" => Kind::Output,
            "thinking" => Kind::Thinking,
            "tool_call" => Kind::ToolCall,
            "tool_result" => Kind::ToolResult,
            "system" => Kind::System,
            "explicit" => Kind::Explicit,
            "raw" => Kind::Raw,
            _ => return Err(Error::Usage),
        }),
    };
    let status = match args.value("--status") {
        None => None,
        Some("ok") => Some(Status::Ok),
        Some("error") => Some(Status::Error),
        _ => return Err(Error::Usage),
    };
    Ok(Filter {
        kind,
        status,
        tool: args.value("--tool").map(str::to_owned),
        agent_id: args.value("--reported-agent").map(str::to_owned),
    })
}
fn versions(args: &Arguments) -> Result<Versions> {
    match args.required("--versions")? {
        "current" => Ok(Versions::Current),
        "all" => Ok(Versions::All),
        _ => Err(Error::Usage),
    }
}
const TIMES: &[&str] = &["--from-us", "--valid-at-us", "--known-at-us"];
const FILTERS: &[&str] = &["--kind", "--tool", "--status", "--reported-agent"];
fn options(args: &Arguments) -> Result<()> {
    let mut values = vec!["--bytes"];
    match args.command.as_str() {
        "record-setup" => values = vec!["--writer", "--owner", "--first-id", "--adapter"],
        "record-append" => {
            values = vec!["--input", "--request", "--input-format"];
            if !matches!(args.value("--input-format"), None | Some("json" | "jsonl")) {
                return Err(Error::Usage);
            }
        }
        "record-get" => {
            values.extend(TIMES);
            values.extend(["--id", "--key"]);
        }
        "record-history" => {
            values.extend(TIMES);
            values.extend(["--entries", "--key"]);
        }
        "record-range" | "record-export" => {
            values.extend(TIMES);
            values.extend(FILTERS);
            values.extend(["--entries", "--versions"]);
        }
        "record-context" => {
            values.extend(TIMES);
            values.extend(FILTERS);
            values.extend([
                "--query",
                "--k",
                "--top-k",
                "--tokens",
                "--context-bytes",
                "--min-bits",
                "--turn-start-us",
                "--session",
                "--turn",
                "--measure",
                "--format",
            ]);
        }
        "record-search" => {
            values.extend(TIMES);
            values.extend(FILTERS);
            values.extend(["--query", "--k"]);
        }
        "record-continue" => values.extend(["--cursor", "--entries"]),
        _ => return Err(Error::Usage),
    }
    args.only(
        &values,
        if args.command == "record-append" {
            &["--allow-unredacted-records"]
        } else {
            &[]
        },
    )
}
pub(super) fn run(root: &Path, args: &Arguments) -> Result<i32> {
    options(args)?;
    // Acknowledgement of exact typed/unredacted retention precedes input inspection.
    if args.command == "record-append" && !args.flags.contains("--allow-unredacted-records") {
        return Err(Error::RecordConsent);
    }
    if args.command == "record-setup" {
        let cfg = Config {
            version: 1,
            writer: args.required("--writer")?.into(),
            owner: args
                .required("--owner")?
                .parse()
                .map_err(|_| Error::Usage)?,
            first_id: args.number("--first-id", 1)?,
            adapter: args.required("--adapter")?.into(),
        };
        cfg.check()?;
        let encoded = serde_json::to_vec(&cfg).map_err(|_| Error::Config)?;
        config::setup_files(root, &[(CONFIG, &encoded)])?;
        let key = config::load_key(root)?;
        let log = map(RecordLog::open(
            &root.join(config::JOURNAL),
            &key,
            cfg.identity(),
        ))?;
        let view = map(View::open(&log, cfg.scope(), AsOf::default()))?;
        emit(
            &json!({"status":"initialized","namespace":"oojuro-record-2","watermark":view.watermark()}),
            4096,
        )?;
        return Ok(0);
    }
    let (cfg, key) = load(root)?;
    let journal = root.join(config::JOURNAL);
    // Reads/append never turn a missing journal into a fresh successful empty authority.
    let metadata = std::fs::symlink_metadata(&journal).map_err(|_| Error::Authority)?;
    if !metadata.is_dir() {
        return Err(Error::Authority);
    }
    let mut log = map(RecordLog::open(&journal, &key, cfg.identity()))?;
    match args.command.as_str() {
        "record-append" => {
            let request = args.required("--request")?;
            let encoded = config::read_private(
                Path::new(args.required("--input")?),
                record::MAX_BATCH_BYTES,
            )?;
            let inputs: Vec<Input> = if args.value("--input-format") == Some("jsonl") {
                let mut inputs = Vec::new();
                // Decode the complete bounded batch before any append. A bad later row
                // must never commit a valid prefix. split_inclusive accepts a final
                // newline without accepting empty rows or a multiline JSON object.
                for line in encoded.split_inclusive(|byte| *byte == b'\n') {
                    if inputs.len() == 1000 {
                        return Err(Error::Bounds);
                    }
                    inputs.push(serde_json::from_slice(line).map_err(|_| Error::Config)?);
                }
                inputs
            } else {
                serde_json::from_slice(&encoded).map_err(|_| Error::Config)?
            };
            let commit = map(log.append(request, &inputs))?;
            emit(&commit, 1 << 20)?;
        }
        "record-continue" => {
            let cap = bytes(args)?;
            let page = map(View::continue_page(
                &log,
                cfg.scope(),
                args.required("--cursor")?,
                limits(args)?,
            ))?;
            emit(&page, cap)?;
        }
        "record-get" => {
            let cap = bytes(args)?;
            let view = map(View::open(&log, cfg.scope(), as_of(args)?))?;
            let found = match (args.value("--id"), args.value("--key")) {
                (Some(id), None) => {
                    map(view.get_id(&log, id.parse().map_err(|_| Error::Usage)?, cap - 1))?
                }
                (None, Some(key)) => map(view.get_key(&log, key, cap - 1))?,
                _ => return Err(Error::Usage),
            };
            emit(&found, cap)?;
        }
        "record-history" | "record-range" | "record-export" => {
            let cap = bytes(args)?;
            let view = map(View::open(&log, cfg.scope(), as_of(args)?))?;
            let bounds = limits(args)?;
            match args.command.as_str() {
                "record-history" => emit(
                    &map(view.history(&log, args.required("--key")?, bounds))?,
                    cap,
                )?,
                "record-export" => emit_bytes(
                    map(view.export(&log, versions(args)?, filter(args)?, bounds))?,
                    cap,
                )?,
                _ => emit(
                    &map(view.range(&log, versions(args)?, filter(args)?, bounds))?,
                    cap,
                )?,
            }
        }
        "record-context" => {
            use record::context;
            // Explicit command and explicit measurement selection; no automatic injection.
            if args.required("--measure")? != "chars4-estimate" {
                return Err(Error::Usage);
            }
            let cap = bytes(args)?;
            let k = args.number("--k", 100)?;
            if !(1..=context::MAX_CANDIDATES).contains(&k) {
                return Err(Error::Bounds);
            }
            let turn_start: i64 = args
                .required("--turn-start-us")?
                .parse()
                .map_err(|_| Error::Usage)?;
            let cutoff = turn_start.checked_sub(1).ok_or(Error::Bounds)?;
            let mut cuts = as_of(args)?;
            cuts.valid_at = cuts.valid_at.min(cutoff);
            cuts.known_at = cuts.known_at.min(cutoff);
            let query = record::Query {
                scope: cfg.scope(),
                as_of: cuts,
                text: args.required("--query")?,
                k,
                filter: filter(args)?,
                max_bytes: record::MAX_OUTPUT_BYTES,
            };
            let index = map(record::Index::rebuild(&log))?;
            let search = map(index.search(&log, &query))?;
            let candidates = search
                .hits
                .iter()
                .map(context::Candidate::lexical)
                .collect::<Vec<_>>();
            struct Estimate;
            impl context::TokenMeasure for Estimate {
                fn id(&self) -> &str {
                    "unicode-chars-div4-estimate-v1"
                }
                fn precision(&self) -> context::Precision {
                    context::Precision::Estimated
                }
                fn measure(&self, s: &str) -> std::result::Result<u64, context::MeasureError> {
                    Ok((s.chars().count() as u64).div_ceil(4))
                }
            }
            let options = context::Options {
                enabled: true,
                min_text_bits: args.number("--min-bits", 8.0)?,
                top_k: args.number("--top-k", 20)?,
                max_tokens: args.number("--tokens", 2500)?,
                max_bytes: args.number("--context-bytes", context::MAX_CONTEXT_BYTES)?,
                bits_per_turn: 0.0,
            };
            let turn = context::Turn {
                session: args.value("--session").unwrap_or(""),
                number: args.number("--turn", 0)?,
                started_us: turn_start,
                prompt: query.text,
            };
            let packed = map(context::pack(&candidates, options, &turn, &Estimate))?;
            match args.value("--format").unwrap_or("context") {
                "context" => emit(
                    &json!({"context":packed.as_ref().map(|p|p.json.as_str()),
                    "items":packed.as_ref().map_or(0,|p|p.items.len()),
                    "estimated_tokens":packed.as_ref().map_or(0,|p|p.measured_tokens),
                    "measurement":"unicode-chars-div4-estimate-v1","hard_provider_token_cap":false,
                    "watermark":search.watermark}),
                    cap,
                )?,
                "claude-hook" => emit(
                    &json!({"hookSpecificOutput":{"hookEventName":"UserPromptSubmit",
                    "additionalContext":packed.as_ref().map_or("",|p|p.json.as_str())},
                    "measurement":"unicode-chars-div4-estimate-v1","hard_provider_token_cap":false}),
                    cap,
                )?,
                _ => return Err(Error::Usage),
            }
        }
        "record-search" => {
            let cap = bytes(args)?;
            let k = args.number("--k", 10)?;
            if !(1..=1000).contains(&k) {
                return Err(Error::Bounds);
            }
            let query = record::Query {
                scope: cfg.scope(),
                as_of: as_of(args)?,
                text: args.required("--query")?,
                k,
                filter: filter(args)?,
                max_bytes: cap - 1,
            };
            let index = map(record::Index::rebuild(&log))?;
            emit(&map(index.search(&log, &query))?, cap)?;
        }
        _ => return Err(Error::Usage),
    }
    Ok(0)
}
