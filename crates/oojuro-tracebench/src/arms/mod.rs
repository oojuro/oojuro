//! Arms. Spec strings: `kind[:key=value,...]`, for example `pg`,
//! `pg:sync=off`, `oojuro:dedupe=off`. Every arm documents its settings in
//! `docs/TRACEBENCH.md`. Common params: `existing=on` (open the store a
//! previous run left; see [`reopen`]), `keep=on` (server arms: leave the
//! container running on drop).
use crate::{Result, TraceStore};
use std::{collections::BTreeMap, path::Path};

pub mod ch;
pub(crate) mod docjson;
pub(crate) mod http;
pub mod kv;
#[cfg(feature = "arm-fjall")]
pub mod kv_fjall;
#[cfg(feature = "arm-redb")]
pub mod kv_redb;
#[cfg(feature = "arm-rocksdb")]
pub mod kv_rocks;
#[cfg(feature = "arm-lance")]
pub mod lance;
pub mod meili;
pub mod oojuro;
pub mod openobserve;
pub mod oracle;
pub mod pg;
pub mod qdrant;
pub mod quickwit;
pub mod sqlite;
#[cfg(feature = "arm-tantivy")]
pub mod tantivy;

#[derive(Clone, Debug, Default)]
pub struct Params(pub BTreeMap<String, String>);

impl Params {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }
    pub fn flag(&self, k: &str, default: bool) -> bool {
        match self.get(k) {
            Some("on" | "true" | "1" | "yes") => true,
            Some("off" | "false" | "0" | "no") => false,
            _ => default,
        }
    }
    pub fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> T {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

pub fn parse(spec: &str) -> (String, Params) {
    let (kind, rest) = spec.split_once(':').unwrap_or((spec, ""));
    let mut p = BTreeMap::new();
    for kv in rest.split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, "on"));
        p.insert(k.to_string(), v.to_string());
    }
    (kind.to_string(), Params(p))
}

/// Open a fresh, empty store for one run. `work` is a private directory the
/// arm may use (embedded arms keep their files there). `oracle` is the owning
/// in-memory reference ([`oracle::OwnedOracle`]).
pub fn open(spec: &str, work: &Path) -> Result<Box<dyn TraceStore>> {
    let (kind, params) = parse(spec);
    std::fs::create_dir_all(work)?;
    match kind.as_str() {
        "oojuro" => oojuro::open(work, &params),
        "pg" => pg::open(work, &params),
        "ch" => ch::open(work, &params),
        "sqlite" => sqlite::open(work, &params),
        #[cfg(feature = "arm-tantivy")]
        "tantivy" => tantivy::open(work, &params),
        #[cfg(not(feature = "arm-tantivy"))]
        "tantivy" => Err("the tantivy arm is not built: enable the `arm-tantivy` feature".into()),
        #[cfg(feature = "arm-lance")]
        "lance" => lance::open(work, &params),
        #[cfg(not(feature = "arm-lance"))]
        "lance" => Err("the lance arm is not built: build with `--features arm-lance` (needs protoc)".into()),
        "meili" => meili::open(work, &params),
        "qdrant" => qdrant::open(work, &params),
        "quickwit" => quickwit::open(work, &params),
        "openobserve" => openobserve::open(work, &params),
        #[cfg(feature = "arm-redb")]
        "redb" => kv_redb::open(work, &params),
        #[cfg(not(feature = "arm-redb"))]
        "redb" => Err("the redb arm is not built: enable the `arm-redb` feature".into()),
        #[cfg(feature = "arm-fjall")]
        "fjall" => kv_fjall::open(work, &params),
        #[cfg(not(feature = "arm-fjall"))]
        "fjall" => Err("the fjall arm is not built: enable the `arm-fjall` feature".into()),
        #[cfg(feature = "arm-rocksdb")]
        "rocksdb" => kv_rocks::open(work, &params),
        #[cfg(not(feature = "arm-rocksdb"))]
        "rocksdb" => Err("the rocksdb arm is not built: enable the `arm-rocksdb` feature (needs libclang)".into()),
        "oracle" => Ok(Box::new(oracle::OwnedOracle::new())),
        _ => Err(format!("unknown arm {kind}").into()),
    }
}

/// `spec` with one more `key=value` parameter (`pg` + `keep=on` = `pg:keep=on`).
pub fn with_param(spec: &str, kv: &str) -> String {
    if spec.contains(':') { format!("{spec},{kv}") } else { format!("{spec}:{kv}") }
}

/// Server arms run in a docker container (`scripts/tracedb/<kind>.sh`).
pub fn is_server(spec: &str) -> bool {
    matches!(parse(spec).0.as_str(), "pg" | "ch" | "meili" | "qdrant" | "quickwit" | "openobserve")
}

/// The in-memory reference keeps nothing on disk: no reopen, no recovery.
pub fn is_memory(spec: &str) -> bool {
    parse(spec).0 == "oracle"
}

/// Open the store a previous `open` of the same spec left in `work` (an
/// embedded arm's files, a server arm's data directory). The caller drops
/// the previous store first; for a server arm, `halt` or `kill_engine` it too.
pub fn reopen(spec: &str, work: &Path) -> Result<Box<dyn TraceStore>> {
    if is_memory(spec) {
        return Err("the in-memory oracle cannot be reopened".into());
    }
    open(&with_param(spec, "existing=on"), work)
}

fn server_script(spec: &str, cmd: &str) -> Result<()> {
    if is_server(spec) {
        pg::run_script(&format!("{}.sh", parse(spec).0), cmd)?;
    }
    Ok(())
}

/// Clean shutdown of a server arm's engine, data kept (no-op for embedded
/// arms: dropping the store closes them).
pub fn halt(spec: &str) -> Result<()> {
    server_script(spec, "halt")
}

/// SIGKILL a server arm's engine, data kept (no-op for embedded arms: they
/// die with the process that holds them).
pub fn kill_engine(spec: &str) -> Result<()> {
    server_script(spec, "kill")
}

/// Remove a server arm's container (what dropping a store without `keep=on`
/// does); `TRACEDB_KEEP_CONTAINER=1` keeps it, as it does for that drop.
pub fn stop(spec: &str) -> Result<()> {
    if std::env::var_os("TRACEDB_KEEP_CONTAINER").is_some() {
        return Ok(());
    }
    server_script(spec, "stop")
}
