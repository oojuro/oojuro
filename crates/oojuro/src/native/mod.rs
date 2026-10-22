//! Explicit native command composition; existing recorder/socket/MCP paths are independent.
mod config;
mod policy;
mod records;
use config::{Config, Harness};
use oojuro_capture::{
    ingest::Policy,
    local::{LocalCapture, Options},
};
use oojuro_engine::{
    capture_log::Source,
    lexical::{Query, segments::PersistentIndex},
    receipts::{Limits, MAX_OUTPUT_BYTES, Range, ReceiptView, Scope},
};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum Error {
    Usage,
    Config,
    Key,
    Policy,
    Permissions,
    Consent,
    Stopped,
    Bounds,
    Io,
    Authority,
    Receipt,
    Cache,
    Capture(&'static str),
    RecordConsent,
    Record(oojuro_engine::record::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = match self {
            Self::Usage => "invalid_arguments",
            Self::Config => "invalid_config",
            Self::Key => "invalid_key",
            Self::Policy => "invalid_policy",
            Self::Permissions => "private_owner_permissions_required",
            Self::Consent => "explicit_raw_consent_required",
            Self::Stopped => "recording_stopped",
            Self::Bounds => "output_or_input_bound",
            Self::Io => "local_io_failed",
            Self::Authority => "authority_unavailable",
            Self::Receipt => "receipt_refused",
            Self::Cache => "cache_unavailable",
            Self::Capture(reason) => return write!(f, "capture_refused: {reason}"),
            Self::RecordConsent => "explicit_unredacted_record_consent_required",
            Self::Record(reason) => {
                return write!(
                    f,
                    "record_refused: {}",
                    match reason {
                        oojuro_engine::record::Error::Invalid => "invalid",
                        oojuro_engine::record::Error::Bounds => "bounds",
                        oojuro_engine::record::Error::Scope => "scope",
                        oojuro_engine::record::Error::Authority => "authority",
                        oojuro_engine::record::Error::Conflict => "conflict",
                        oojuro_engine::record::Error::Unavailable => "unavailable",
                    }
                );
            }
        };
        f.write_str(code)
    }
}
type Result<T> = std::result::Result<T, Error>;

struct Arguments {
    command: String,
    values: BTreeMap<String, String>,
    flags: BTreeSet<String>,
}
impl Arguments {
    fn parse() -> Result<Self> {
        let args: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|s| s.into_string().map_err(|_| Error::Usage))
            .collect::<Result<_>>()?;
        if args.is_empty() || args.len() > 48 {
            return Err(Error::Usage);
        }
        let mut parsed = Self {
            command: args[0].clone(),
            values: BTreeMap::new(),
            flags: BTreeSet::new(),
        };
        let mut it = args.into_iter().skip(1);
        while let Some(flag) = it.next() {
            if !flag.starts_with("--")
                || parsed.values.contains_key(&flag)
                || parsed.flags.contains(&flag)
            {
                return Err(Error::Usage);
            }
            if matches!(
                flag.as_str(),
                "--allow-raw-source" | "--allow-raw-export" | "--allow-unredacted-records"
            ) {
                parsed.flags.insert(flag);
                continue;
            }
            let value = it.next().ok_or(Error::Usage)?;
            if value.len() > 65536 {
                return Err(Error::Bounds);
            }
            parsed.values.insert(flag, value);
        }
        Ok(parsed)
    }
    fn only(&self, values: &[&str], flags: &[&str]) -> Result<()> {
        if self
            .values
            .keys()
            .any(|s| s != "--root" && !values.contains(&s.as_str()))
            || self.flags.iter().any(|s| !flags.contains(&s.as_str()))
        {
            return Err(Error::Usage);
        }
        Ok(())
    }
    fn required(&self, name: &str) -> Result<&str> {
        self.values
            .get(name)
            .map(String::as_str)
            .ok_or(Error::Usage)
    }
    fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
    fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T> {
        self.value(name)
            .map(|s| s.parse().map_err(|_| Error::Usage))
            .unwrap_or(Ok(default))
    }
}

fn emit(value: &impl Serialize, limit: usize) -> Result<()> {
    emit_bytes(serde_json::to_vec(value).map_err(|_| Error::Bounds)?, limit)
}
fn emit_bytes(mut bytes: Vec<u8>, limit: usize) -> Result<()> {
    if bytes.len() >= limit {
        return Err(Error::Bounds);
    }
    bytes.push(b'\n');
    std::io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| Error::Io)
}
fn source(value: &str) -> Result<Source> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Usage);
    }
    let mut source = [0; 16];
    for (i, byte) in source.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).map_err(|_| Error::Usage)?;
    }
    Ok(source)
}
fn open(
    root: &Path,
    config: &Config,
    key: &[u8; 32],
    policy: Box<dyn Policy>,
    policy_id: &str,
) -> Result<LocalCapture> {
    let mut options = Options::new(&config.writer, config.agent, policy_id);
    options.first_id = config.first_id;
    options.raw_authorization = "native-cli-explicit-source-1".into();
    let result = match config.harness {
        Harness::Codex => LocalCapture::open(&root.join(config::JOURNAL), key, options, policy),
        Harness::ClaudeCode => {
            LocalCapture::open_claude(&root.join(config::JOURNAL), key, options, policy)
        }
    };
    result.map_err(|_| Error::Authority)
}
fn view(capture: &LocalCapture, config: &Config, args: &Arguments) -> Result<ReceiptView> {
    let log = capture.authority().map_err(|_| Error::Authority)?;
    let scope = Scope {
        writer: &config.writer,
        agent: config.agent,
    };
    let range = Range {
        start: args.number("--start", 0)?,
        end: args.number("--end", u64::MAX)?,
    };
    match args.value("--cursor") {
        Some(cursor) => ReceiptView::resume(log, scope, range, cursor),
        None => ReceiptView::open(log, scope, range),
    }
    .map_err(|_| Error::Receipt)
}
fn byte_limit(args: &Arguments) -> Result<usize> {
    let bytes = args.number("--bytes", 1 << 20)?;
    if !(2..=MAX_OUTPUT_BYTES).contains(&bytes) {
        return Err(Error::Bounds);
    }
    Ok(bytes)
}

pub fn run() -> Result<i32> {
    let args = Arguments::parse()?;
    if matches!(args.command.as_str(), "help" | "--help") {
        args.only(&[], &[])?;
        emit(
            &json!({"platform":"linux","commands":["setup","capture","search","get","timeline","stats","export","restore-raw","cache-merge","record-setup","record-append","record-get","record-history","record-range","record-export","record-continue","record-search"],"required":"--root DIR","raw_capture":"--allow-raw-source","raw_export":"--allow-raw-export"}),
            4096,
        )?;
        return Ok(0);
    }
    let root = std::path::absolute(args.required("--root")?).map_err(|_| Error::Io)?;
    if args.command.starts_with("record-") {
        return records::run(&root, &args);
    }
    if args.command == "setup" {
        args.only(
            &["--harness", "--writer", "--agent", "--first-id", "--policy"],
            &[],
        )?;
        let harness = match args.required("--harness")? {
            "codex" => Harness::Codex,
            "claude-code" => Harness::ClaudeCode,
            _ => return Err(Error::Usage),
        };
        let config = Config {
            version: 1,
            harness,
            writer: args.required("--writer")?.into(),
            agent: args
                .required("--agent")?
                .parse()
                .map_err(|_| Error::Usage)?,
            first_id: args.number("--first-id", 1)?,
        };
        let bytes = config::read_private(Path::new(args.required("--policy")?), 65536)?;
        let policy = policy::FrozenPolicy::parse(&bytes)?;
        config::setup(&root, &config, &bytes)?;
        let (_, key) = config::load(&root)?;
        open(&root, &config, &key, Box::new(policy::ReadOnly), &policy.id)?;
        emit(&json!({"status":"initialized","format":1}), 4096)?;
        return Ok(0);
    }
    // Consent is required before source inspection or raw export destination creation.
    if args.command == "capture" && !args.flags.contains("--allow-raw-source")
        || args.command == "restore-raw" && !args.flags.contains("--allow-raw-export")
    {
        return Err(Error::Consent);
    }
    let allowed = match args.command.as_str() {
        "capture" => (
            &["--source", "--source-id"][..],
            &["--allow-raw-source"][..],
        ),
        "search" => (
            &[
                "--query",
                "--k",
                "--from-us",
                "--to-us",
                "--bytes",
                "--cache",
            ][..],
            &[][..],
        ),
        "get" => (
            &["--id", "--bytes", "--start", "--end", "--cursor"][..],
            &[][..],
        ),
        "timeline" | "export" => (
            &["--entries", "--bytes", "--start", "--end", "--cursor"][..],
            &[][..],
        ),
        "stats" => (&["--bytes", "--start", "--end", "--cursor"][..], &[][..]),
        "restore-raw" => (&["--source-id", "--out"][..], &["--allow-raw-export"][..]),
        "cache-merge" => (&["--cache"][..], &[][..]),
        _ => return Err(Error::Usage),
    };
    args.only(allowed.0, allowed.1)?;
    let (config, key) = config::load(&root)?;
    let frozen = if args.command == "capture" {
        Some(policy::FrozenPolicy::parse(&config::read_private(
            &root.join(config::POLICY),
            65536,
        )?)?)
    } else {
        None
    };
    let policy_id = frozen
        .as_ref()
        .map(|p| p.id.as_str())
        .unwrap_or("native-read-only-1")
        .to_string();
    if frozen.as_ref().is_some_and(|policy| !policy.enabled()) {
        return Err(Error::Stopped);
    }
    let policy: Box<dyn Policy> = match frozen {
        Some(policy) => Box::new(policy),
        None => Box::new(policy::ReadOnly),
    };
    let mut capture = open(&root, &config, &key, policy, &policy_id)?;
    match args.command.as_str() {
        "capture" => {
            let generation = source(args.required("--source-id")?)?;
            match capture.capture_file(generation, Path::new(args.required("--source")?), |_| true)
            {
                Ok(Some(report)) => emit(
                    &json!({"status":"captured","source_generation":generation,"raw_end":report.raw_end,"parsed_end":report.parsed_end,"event_ids":report.event_ids}),
                    MAX_OUTPUT_BYTES,
                )?,
                Ok(None) => return Err(Error::Consent),
                Err(oojuro_capture::local::Error::CommittedUnavailable(outcome)) => {
                    emit(
                        &json!({"status":"committed_unavailable","source_generation":generation,"outcome":outcome}),
                        4096,
                    )?;
                    return Ok(3);
                }
                Err(oojuro_capture::local::Error::Invalid(reason)) => {
                    return Err(Error::Capture(reason));
                }
                Err(_) => return Err(Error::Authority),
            }
        }
        "search" => {
            let query = Query {
                writer: &config.writer,
                agent: config.agent,
                text: args.required("--query")?,
                from: args.number("--from-us", i64::MIN)?,
                to: args.number("--to-us", i64::MAX)?,
                k: args.number("--k", 10)?,
            };
            if query.k == 0 || query.k > 1000 || query.from > query.to {
                return Err(Error::Bounds);
            }
            let log = capture.authority().map_err(|_| Error::Authority)?;
            let (hits, cache) = if let Some(cache) = args.value("--cache") {
                let index =
                    PersistentIndex::open(log, Path::new(cache)).map_err(|_| Error::Cache)?;
                (
                    index.search(log, &query).map_err(|_| Error::Cache)?,
                    Some(format!("{:?}", index.opened())),
                )
            } else {
                (
                    capture
                        .search(query.text, query.from, query.to, query.k)
                        .map_err(|_| Error::Bounds)?
                        .into_iter()
                        .map(|h| h.hit)
                        .collect(),
                    None,
                )
            };
            let hits:Vec<_>=hits.into_iter().map(|hit|json!({"id":hit.id,"score_bits":hit.bits,"continued":hit.continued,"reported_time_us":hit.time,"host_id":hit.row})).collect();
            emit(&json!({"hits":hits,"cache":cache}), byte_limit(&args)?)?;
        }
        "get" | "timeline" | "stats" | "export" => {
            let view = view(&capture, &config, &args)?;
            let log = capture.authority().map_err(|_| Error::Authority)?;
            let bytes = byte_limit(&args)?;
            match args.command.as_str() {
                "get" => emit(
                    &view
                        .get(
                            log,
                            args.required("--id")?.parse().map_err(|_| Error::Usage)?,
                            bytes - 1,
                        )
                        .map_err(|_| Error::Receipt)?,
                    bytes,
                )?,
                "stats" => emit(
                    &view.stats(log, bytes - 1).map_err(|_| Error::Receipt)?,
                    bytes,
                )?,
                "export" => emit_bytes(
                    view.export(
                        log,
                        Limits {
                            entries: args.number("--entries", 100)?,
                            bytes: bytes - 1,
                        },
                    )
                    .map_err(|_| Error::Receipt)?,
                    bytes,
                )?,
                _ => emit(
                    &view
                        .page(
                            log,
                            Limits {
                                entries: args.number("--entries", 100)?,
                                bytes: bytes - 1,
                            },
                        )
                        .map_err(|_| Error::Receipt)?,
                    bytes,
                )?,
            }
        }
        "restore-raw" => {
            let generation = source(args.required("--source-id")?)?;
            let output = PathBuf::from(args.required("--out")?);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&output)
                .map_err(|_| Error::Io)?;
            capture
                .restore_raw(generation, &mut file, true)
                .map_err(|_| Error::Authority)?;
            file.sync_all().map_err(|_| Error::Io)?;
            let parent = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            config::sync_dir(parent)?;
            emit(&json!({"status":"raw_restored"}), 4096)?;
        }
        "cache-merge" => {
            let log = capture.authority().map_err(|_| Error::Authority)?;
            let mut index = PersistentIndex::open(log, Path::new(args.required("--cache")?))
                .map_err(|_| Error::Cache)?;
            index.merge(log).map_err(|_| Error::Cache)?;
            emit(
                &json!({"status":"merged","segments":index.segment_count()}),
                4096,
            )?;
        }
        _ => return Err(Error::Usage),
    }
    Ok(0)
}
