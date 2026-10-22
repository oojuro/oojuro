//! Explicit durable Capture1 -> managed Record bridge. No raw reparse or second WAL.
//! Progress is sealed advisory state: only current managed commits authorize writes/retries.
use crate::{
    capture_log::{CachePrefix, CaptureLog, Decision, Event, Identity},
    record::{
        self, AsOf, Filter, Input, Kind, Scope, Status,
        managed::{AppendResult, Binding, ManagedLog, ManagedPagedLog},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};
const AAD: &[u8] = b"oojuro/capture-record/progress/v1\0";
const PROGRESS_BYTES: usize = 8192;
const MAX_STEPS: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Scope,
    Source,
    Authority,
    Checkpoint,
    Target(record::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy)]
pub struct Limits {
    pub receipts: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            receipts: 128,
            bytes: 32 << 20,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Mapping {
    pub source_id: u64,
    pub canonical_id: u64,
    pub recorded_at: i64,
    pub receipt_only: bool,
}
#[derive(Default, Serialize)]
pub struct Report {
    pub scanned: usize,
    pub withheld: usize,
    pub no_output: usize,
    pub stored: usize,
    pub new: usize,
    pub already_committed: usize,
    pub removed: usize,
    pub mappings: Vec<Mapping>,
    /// More receipts in this checked prefix; false never promises the source cannot append.
    pub more: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    batch: usize,
    line: usize,
    emission: usize,
    ordinal: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    schema: u32,
    source_binding: [u8; 32],
    source_identity: Identity,
    prefix: CachePrefix,
    target_origin: [u8; 32],
    target_identity: [u8; 32],
    next: Position,
}
fn source<T>(r: oojuro_store::Result<T>) -> Result<T> {
    r.map_err(|_| Error::Source)
}
fn snapshot_binding(log: &ManagedLog, scope: Scope<'_>) -> record::Result<Binding> {
    log.with_rank_view(scope, AsOf::default(), Filter::default(), |view| {
        view.validate_current()?;
        Ok(view.binding().clone())
    })
}
// Only these two actual engine owners can authorize this bridge. No caller-supplied trait.
enum Target<'a> {
    Snapshot(&'a mut ManagedLog),
    Paged(&'a mut ManagedPagedLog),
}
impl Target<'_> {
    fn binding(&self, scope: Scope<'_>, expected: Option<&Binding>) -> Result<Binding> {
        let binding = match self {
            Self::Snapshot(log) => snapshot_binding(log, scope),
            Self::Paged(log) => log.scoped_binding(scope),
        }
        .map_err(Error::Target)?;
        if expected.is_some_and(|old| {
            old.stable_origin != binding.stable_origin || old.identity != binding.identity
        }) {
            return Err(Error::Authority);
        }
        Ok(binding)
    }
    fn append(&mut self, operation: &str, input: Input) -> record::Result<AppendResult> {
        match self {
            Self::Snapshot(log) => log.append(operation, &[input]),
            Self::Paged(log) => log.append(operation, &[input]),
        }
    }
}
fn check_parent(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::Checkpoint);
    }
    let parent = path.parent().ok_or(Error::Checkpoint)?;
    let uid = unsafe { libc::geteuid() };
    let mut at = PathBuf::new();
    for component in parent.components() {
        at.push(component);
        let m = fs::symlink_metadata(&at).map_err(|_| Error::Checkpoint)?;
        if !m.is_dir()
            || m.file_type().is_symlink()
            || !(m.uid() == 0 || m.uid() == uid)
            || !(m.mode() & 0o022 == 0 || (m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(Error::Checkpoint);
        }
    }
    let m = fs::symlink_metadata(parent).map_err(|_| Error::Checkpoint)?;
    if m.uid() != uid
        || m.mode() & 0o777 != 0o700
        || fs::canonicalize(parent).map_err(|_| Error::Checkpoint)? != parent
    {
        return Err(Error::Checkpoint);
    }
    if path.file_name().is_none() {
        return Err(Error::Checkpoint);
    }
    Ok(parent.to_owned())
}
fn load(path: &Path) -> Result<Option<Vec<u8>>> {
    check_parent(path)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Checkpoint),
    };
    let m = file.metadata().map_err(|_| Error::Checkpoint)?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o777 != 0o600
        || m.len() > PROGRESS_BYTES as u64
    {
        return Err(Error::Checkpoint);
    }
    let mut bytes = vec![];
    file.take(PROGRESS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Checkpoint)?;
    if bytes.len() > PROGRESS_BYTES {
        return Err(Error::Checkpoint);
    }
    Ok(Some(bytes))
}
fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = check_parent(path)?;
    load(path)?;
    let random = oojuro_store::crypto::random_key();
    let temporary = parent.join(format!(".capture-progress-{}.tmp", hex(&random)));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|_| Error::Checkpoint)?;
        created = true;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::Checkpoint)?;
        fs::rename(&temporary, path).map_err(|_| Error::Checkpoint)?;
        File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|_| Error::Checkpoint)
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn request(
    origin: [u8; 32],
    identity: &Identity,
    source: [u8; 16],
    event: &Event,
) -> Result<String> {
    // Engine-private typed source MAC stays stable across physical compaction.
    // Exact text/policy conflict checking is separately keyed by ManagedLog::append.
    let bytes = serde_json::to_vec(&(
        "capture-record-emission-v1",
        origin,
        identity,
        source,
        event.line_start,
        event.line_end,
        event.emission,
    ))
    .map_err(|_| Error::Source)?;
    Ok(format!(
        "capture-record-v1:{}",
        blake3::hash(&bytes).to_hex()
    ))
}
fn input(
    identity: &Identity,
    batch: &crate::capture_log::ParsedBatch,
    event: &Event,
) -> Result<Input> {
    let kind = match event.kind {
        0 => Kind::System,
        1 => Kind::Input,
        2 => Kind::Thinking,
        3 => Kind::Output,
        4 => Kind::ToolCall,
        5 => Kind::ToolResult,
        6 => Kind::Explicit,
        _ => return Err(Error::Source),
    };
    let status = match event.status {
        0 => None,
        2 => Some(Status::Ok),
        3 => Some(Status::Error),
        _ => return Err(Error::Source),
    };
    // In particular args.rt is NEVER the new ingest clock.
    let mut reported = serde_json::to_value(event).map_err(|_| Error::Source)?;
    reported
        .as_object_mut()
        .ok_or(Error::Source)?
        .remove("text");
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "capture".into(),
        json!({
            "bridge_schema":1, "identity":identity, "source_generation":batch.source,
            "policy_id":batch.policy.policy_id, "raw_authorization":batch.policy.raw_authorization,
        "batch_raw":{"start":batch.raw_start,"end":batch.outcome.raw_end},
        "batch_parsed":{"start":batch.parsed_before,"end":batch.outcome.parsed_end},
            "reported":reported
        }),
    );
    let row = Input {
        agent_id: format!("capture:{}/session:{}", event.agent, event.session),
        parent_id: (event.parent_session != 0)
            .then(|| format!("capture:{}/session:{}", event.agent, event.parent_session)),
        valid_at: event.ts_us,
        kind,
        tool: (!event.tool.is_empty()).then(|| event.tool.clone()),
        status,
        exit: event.exit_code,
        key: None,
        text: event.text.clone(),
        attrs,
        indexed: event.indexed,
    };
    row.validate().map_err(|_| Error::Bounds)?;
    Ok(row)
}
/// Caller opens real authorities with keys/retained floor; no enrollment or consent is inferred.
/// Keep the private checkpoint directory OUTSIDE authority directories. Missing/old progress
/// safely replays exact original operations. This function supplies no key getter or network.
pub fn ingest(
    source_log: &CaptureLog,
    destination: &mut ManagedLog,
    scope: Scope<'_>,
    checkpoint: &Path,
    limits: Limits,
) -> Result<Report> {
    run(
        source_log,
        destination,
        scope,
        checkpoint,
        limits,
        &mut || Ok(()),
    )
}
/// Explicit existing paged target. No conversion, enrollment or source policy reinterpretation.
pub fn ingest_paged(
    source_log: &CaptureLog,
    destination: &mut ManagedPagedLog,
    scope: Scope<'_>,
    checkpoint: &Path,
    limits: Limits,
) -> Result<Report> {
    run_target(
        source_log,
        Target::Paged(destination),
        scope,
        checkpoint,
        limits,
        &mut || Ok(()),
    )
}
fn run(
    source_log: &CaptureLog,
    destination: &mut ManagedLog,
    scope: Scope<'_>,
    checkpoint: &Path,
    limits: Limits,
    after_append: &mut dyn FnMut() -> Result<()>,
) -> Result<Report> {
    run_target(
        source_log,
        Target::Snapshot(destination),
        scope,
        checkpoint,
        limits,
        after_append,
    )
}
fn run_target(
    source_log: &CaptureLog,
    mut destination: Target<'_>,
    scope: Scope<'_>,
    checkpoint: &Path,
    limits: Limits,
    after_append: &mut dyn FnMut() -> Result<()>,
) -> Result<Report> {
    if limits.receipts == 0
        || limits.receipts > 1000
        || limits.bytes == 0
        || limits.bytes > record::MAX_BATCH_BYTES
    {
        return Err(Error::Bounds);
    }
    let identity = source_log.identity();
    if identity.writer != scope.writer || identity.agent != scope.owner {
        return Err(Error::Scope);
    }
    let initial = destination.binding(scope, None)?;
    let origin = source(source_log.ingest_binding())?;
    let tip = source(source_log.cache_tip())?;
    let mut position = Position::default();
    if let Some(sealed) = load(checkpoint)? {
        let plain = source_log
            .cache_cipher()
            .open_frame(AAD, &sealed)
            .map_err(|_| Error::Checkpoint)?;
        let prior: Progress = serde_json::from_slice(&plain).map_err(|_| Error::Checkpoint)?;
        if prior.schema != 1
            || prior.source_binding != origin
            || prior.source_identity != *identity
            || prior.target_origin != initial.stable_origin
            || prior.target_identity != initial.identity
            || source(source_log.cache_prefix(prior.prefix.records))? != prior.prefix
            || prior.next.batch > prior.prefix.batches
        {
            return Err(Error::Authority);
        }
        position = prior.next;
    }
    let batches = source(source_log.batches())?;
    let mut report = Report::default();
    let mut bytes = 0usize;
    let mut steps = 0usize;
    while position.batch < tip.batches && report.scanned < limits.receipts && steps < MAX_STEPS {
        steps += 1;
        let batch = batches.get(position.batch).ok_or(Error::Authority)?;
        if position.line == batch.policy.lines.len() {
            position.batch += 1;
            position.line = 0;
            position.emission = 0;
            continue;
        }
        let line = batch
            .policy
            .lines
            .get(position.line)
            .ok_or(Error::Authority)?;
        if position.emission >= line.emissions.len().max(1) {
            return Err(Error::Authority);
        }
        if let Some(decision) = line.emissions.get(position.emission) {
            match decision {
                Decision::Stored { id, indexed } => {
                    let (event_source, event) =
                        source(source_log.event(*id))?.ok_or(Error::Authority)?;
                    if event_source != batch.source
                        || event.indexed != *indexed
                        || event.line_start != line.start
                        || event.line_end != line.end
                        || event.emission as usize != position.emission
                    {
                        return Err(Error::Authority);
                    }
                    let row = input(identity, batch, event)?;
                    let size = serde_json::to_vec(&row).map_err(|_| Error::Bounds)?.len();
                    if size > limits.bytes.saturating_sub(bytes) {
                        if report.scanned == 0 {
                            return Err(Error::Bounds);
                        }
                        break;
                    }
                    let before = destination.binding(scope, Some(&initial))?;
                    let operation = request(origin, identity, batch.source, event)?;
                    let result = destination.append(&operation, row).map_err(Error::Target)?;
                    after_append()?;
                    let after = destination.binding(scope, Some(&initial))?;
                    if result.commit.origin != initial.stable_origin
                        || result.commit.ids.len() != 1
                        || result.commit.recorded_at.len() != 1
                        || result.removed_ids.len() > 1
                    {
                        return Err(Error::Authority);
                    }
                    report.stored += 1;
                    report.removed += result.removed_ids.len();
                    if before.generation == after.generation {
                        report.already_committed += 1;
                    } else {
                        report.new += 1;
                    }
                    report.mappings.push(Mapping {
                        source_id: *id,
                        canonical_id: result.commit.ids[0],
                        recorded_at: result.commit.recorded_at[0],
                        receipt_only: result.receipt_only,
                    });
                    bytes += size;
                }
                Decision::Withheld { .. } => {
                    report.withheld += 1;
                }
            }
        } else {
            report.no_output += 1;
        }
        report.scanned += 1;
        position.ordinal = position.ordinal.checked_add(1).ok_or(Error::Bounds)?;
        position.emission += 1;
        if position.emission == line.emissions.len().max(1) {
            position.line += 1;
            position.emission = 0;
        }
    }
    while position.batch < tip.batches
        && steps < MAX_STEPS
        && position.line == batches[position.batch].policy.lines.len()
    {
        steps += 1;
        position.batch += 1;
        position.line = 0;
        position.emission = 0;
    }
    report.more = position.batch < tip.batches;
    destination.binding(scope, Some(&initial))?;
    let progress = Progress {
        schema: 1,
        source_binding: origin,
        source_identity: identity.clone(),
        prefix: tip,
        target_origin: initial.stable_origin,
        target_identity: initial.identity,
        next: position,
    };
    let plain = serde_json::to_vec(&progress).map_err(|_| Error::Checkpoint)?;
    let sealed = source_log.cache_cipher().seal_frame(AAD, &plain);
    if sealed.len() > PROGRESS_BYTES {
        return Err(Error::Checkpoint);
    }
    save(checkpoint, &sealed)?;
    destination.binding(scope, Some(&initial))?;
    Ok(report)
}
#[cfg(test)]
mod tests;
