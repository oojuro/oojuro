//! Disposable, sealed, writer/view-bound rank tables. No key or commit authority.
use super::*;
use oojuro_rank::WorkBudget;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};
const PAYLOAD: usize = 2 << 20;
const REGISTRY: usize = 16 << 20;
const TABLES: usize = 8;
const FILE: &str = "RANK_TABLE";
pub mod managed;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub namespace: [u8; 32],
    pub generation: u64,
    pub epoch: [u8; 32],
}
/// Implemented by the trusted owning lifecycle, not a request object.
/// Managed wrappers MUST check independently retained current control here.
/// Binding structs do not themselves prove freshness, authentication or erasure.
pub trait LifecycleGuard {
    fn current(&self) -> Result<Binding>;
    fn directory(&self) -> &Path;
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    version: String,
    table_version: String,
    content_version: String,
    prefix: Watermark,
    as_of: AsOf,
    filter: Filter,
    payload: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    format: u32,
    origin: [u8; 32],
    identity: [u8; 32],
    binding: Binding,
    entries: Vec<Entry>,
}
/// No Debug/Serialize/export; plaintext derived data stays in the query process.
pub struct Loaded {
    pub(super) id: String,
    pub(super) version: String,
    pub(super) table_version: String,
    pub(super) content_version: String,
    snapshot: Snapshot,
    pub(super) payload: Vec<u8>,
}
enum Snapshot {
    Direct {
        binding: Binding,
        prefix: Watermark,
        as_of: AsOf,
        filter: Filter,
    },
    Managed {
        binding: super::managed::Binding,
        stamp: [u8; 32],
    },
}
pub struct Source<'a> {
    pub id: u64,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub text: &'a str,
}
pub struct Writer {
    bytes: Vec<u8>,
    full: bool,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > PAYLOAD)
        {
            self.full = true;
            return Err(std::io::Error::other("table bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
}
fn binding(log: &RecordLog, guard: &dyn LifecycleGuard) -> Result<Binding> {
    log.healthy()?;
    let b = guard.current()?;
    if b.generation == 0 {
        return Err(Error::Authority);
    }
    Ok(b)
}
fn unchanged(log: &RecordLog, guard: &dyn LifecycleGuard, b: &Binding) -> Result<()> {
    if binding(log, guard)? != *b {
        return Err(Error::Authority);
    }
    Ok(())
}
fn prefix_eq(a: &Watermark, b: &Watermark) -> bool {
    a.origin == b.origin
        && a.identity == b.identity
        && a.sequence == b.sequence
        && a.records == b.records
}
fn filter_eq(a: &Filter, b: &Filter) -> Result<bool> {
    Ok(json(a, 4096)? == json(b, 4096)?)
}
fn aad(origin: [u8; 32], identity: [u8; 32], b: &Binding) -> Result<Vec<u8>> {
    json(&("oojuro/native-rank-tables/v1", origin, identity, b), 4096)
}
fn known(name: &str) -> bool {
    name == FILE
        || name == "LOCK"
        || name
            .strip_prefix("tmp-")
            .and_then(|s| s.strip_suffix(".bin"))
            .is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
}
fn lock(dir: &Path, create: bool) -> Result<File> {
    match fs::symlink_metadata(dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => return Err(Error::Invalid),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir(dir).map_err(|_| Error::Unavailable)?
        }
        Err(_) => return Err(Error::Unavailable),
    }
    if create {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Unavailable)?;
    }
    for entry in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let e = entry.map_err(|_| Error::Unavailable)?;
        if !known(e.file_name().to_str().ok_or(Error::Invalid)?)
            || !e.file_type().map_err(|_| Error::Unavailable)?.is_file()
        {
            return Err(Error::Invalid);
        }
    }
    let file = OpenOptions::new()
        .create(create)
        .truncate(false)
        .read(true)
        .write(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("LOCK"))
        .map_err(|_| Error::Unavailable)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Unavailable);
    }
    Ok(file)
}
fn read_sealed(dir: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(FILE))
        .map_err(|_| Error::Unavailable)?;
    let meta = file.metadata().map_err(|_| Error::Unavailable)?;
    if !meta.is_file() || meta.len() > REGISTRY as u64 + 128 {
        return Err(Error::Bounds);
    }
    let mut sealed = Vec::new();
    file.take(REGISTRY as u64 + 129)
        .read_to_end(&mut sealed)
        .map_err(|_| Error::Unavailable)?;
    if sealed.len() > REGISTRY + 128 {
        return Err(Error::Bounds);
    }
    Ok(sealed)
}
fn read(log: &RecordLog, guard: &dyn LifecycleGuard, b: &Binding) -> Result<Registry> {
    let sealed = read_sealed(guard.directory())?;
    let tip = log.tip()?;
    let plain = log.unseal(&aad(tip.origin, tip.identity, b)?, &sealed)?;
    let r: Registry = serde_json::from_slice(&plain).map_err(|_| Error::Invalid)?;
    if r.format != 1
        || r.origin != tip.origin
        || r.identity != tip.identity
        || r.binding != *b
        || r.entries.len() > TABLES
    {
        return Err(Error::Authority);
    }
    let mut ids = std::collections::BTreeSet::new();
    for e in &r.entries {
        if !label(&e.id)
            || !label(&e.version)
            || !label(&e.table_version)
            || e.content_version.len() != 64
            || !e
                .content_version
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || e.payload.len() > PAYLOAD
            || !ids.insert(e.id.as_str())
        {
            return Err(Error::Invalid);
        }
    }
    Ok(r)
}
fn sync(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::Unavailable)
}
fn reap(dir: &Path) -> Result<()> {
    for e in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let e = e.map_err(|_| Error::Unavailable)?;
        let name = e.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if !known(name) || !e.file_type().map_err(|_| Error::Unavailable)?.is_file() {
            return Err(Error::Invalid);
        }
        if name.starts_with("tmp-") {
            fs::remove_file(e.path()).map_err(|_| Error::Unavailable)?;
        }
    }
    sync(dir)
}
fn publish_sealed(dir: &Path, sealed: &[u8], check: impl Fn() -> Result<()>) -> Result<()> {
    check()?;
    let name = format!(
        "tmp-{}.bin",
        oojuro_store::crypto::random_key()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let path = dir.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| Error::Unavailable)?;
    file.write_all(sealed)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Unavailable)?;
    check()?;
    fs::rename(&path, dir.join(FILE)).map_err(|_| Error::Unavailable)?;
    sync(dir)?;
    check()?;
    reap(dir)
}
fn publish(log: &RecordLog, guard: &dyn LifecycleGuard, b: &Binding, r: &Registry) -> Result<()> {
    unchanged(log, guard, b)?;
    let plain = json(r, REGISTRY)?;
    let sealed = log.seal(&aad(r.origin, r.identity, b)?, &plain);
    publish_sealed(guard.directory(), &sealed, || unchanged(log, guard, b))
}
/// Explicit background/caller build; never invoked by search or append.
pub struct Build<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub table_version: &'a str,
    pub work_units: usize,
}
pub fn build(
    log: &RecordLog,
    view: &View,
    filter: &Filter,
    guard: &dyn LifecycleGuard,
    options: Build<'_>,
    builder: impl FnOnce(&[Source<'_>], &mut Writer, &mut WorkBudget) -> Result<()>,
) -> Result<()> {
    let Build {
        id,
        version,
        table_version,
        work_units: units,
    } = options;
    if !label(id) || !label(version) || !label(table_version) || units > 1_000_000 {
        return Err(Error::Bounds);
    }
    let b = binding(log, guard)?;
    let _lock = lock(guard.directory(), true)?;
    log.validate(view.watermark())?;
    let visible = view.visible(log, filter)?;
    let rows = log
        .records
        .iter()
        .filter(|r| visible.contains(&r.id) && r.input.indexed)
        .map(|r| Source {
            id: r.id,
            valid_at: r.input.valid_at,
            recorded_at: r.recorded_at,
            text: &r.input.text,
        })
        .collect::<Vec<_>>();
    let mut writer = Writer {
        bytes: vec![],
        full: false,
    };
    let mut work = WorkBudget::new(units);
    builder(&rows, &mut writer, &mut work)?;
    if writer.full || work.exhausted() {
        return Err(Error::Bounds);
    }
    unchanged(log, guard, &b)?;
    let tip = log.tip()?;
    // A cache is disposable: explicit rebuilding may replace missing/corrupt/old-generation data.
    let mut r = read(log, guard, &b).unwrap_or(Registry {
        format: 1,
        origin: tip.origin,
        identity: tip.identity,
        binding: b.clone(),
        entries: vec![],
    });
    r.entries.retain(|e| e.id != id);
    if r.entries.len() >= TABLES {
        return Err(Error::Bounds);
    }
    r.entries.push(Entry {
        id: id.into(),
        version: version.into(),
        table_version: table_version.into(),
        content_version: oojuro_store::crypto::random_key()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        prefix: view.watermark().clone(),
        as_of: view.as_of(),
        filter: filter.clone(),
        payload: writer.bytes,
    });
    publish(log, guard, &b, &r)
}
pub fn load(
    log: &RecordLog,
    view: &View,
    filter: &Filter,
    guard: &dyn LifecycleGuard,
    id: &str,
    version: &str,
    table_version: &str,
) -> Result<Loaded> {
    if !label(id) || !label(version) || !label(table_version) {
        return Err(Error::Bounds);
    }
    let b = binding(log, guard)?;
    let _lock = lock(guard.directory(), false)?;
    let mut r = read(log, guard, &b)?;
    let at = r
        .entries
        .iter()
        .position(|e| e.id == id && e.version == version && e.table_version == table_version)
        .ok_or(Error::Unavailable)?;
    let e = r.entries.swap_remove(at);
    log.validate(&e.prefix)?;
    log.validate(view.watermark())?;
    if !prefix_eq(&e.prefix, view.watermark())
        || e.as_of != view.as_of()
        || !filter_eq(&e.filter, filter)?
    {
        return Err(Error::Authority);
    }
    unchanged(log, guard, &b)?;
    Ok(Loaded {
        id: e.id,
        version: e.version,
        table_version: e.table_version,
        content_version: e.content_version,
        snapshot: Snapshot::Direct {
            binding: b,
            prefix: e.prefix,
            as_of: e.as_of,
            filter: e.filter,
        },
        payload: e.payload,
    })
}
pub(super) fn validate_loaded(
    log: &RecordLog,
    view: &View,
    filter: &Filter,
    guard: &dyn LifecycleGuard,
    t: &Loaded,
) -> Result<()> {
    let Snapshot::Direct {
        binding,
        prefix,
        as_of,
        filter: saved_filter,
    } = &t.snapshot
    else {
        return Err(Error::Authority);
    };
    unchanged(log, guard, binding)?;
    log.validate(prefix)?;
    log.validate(view.watermark())?;
    if !prefix_eq(prefix, view.watermark())
        || *as_of != view.as_of()
        || !filter_eq(saved_filter, filter)?
    {
        return Err(Error::Authority);
    }
    Ok(())
}
/// Deletes only registered disposable files, under the current owning guard.
/// Managed retired-generation cleanup additionally retires the enclosing owner directory.
fn remove_registered(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let e = entry.map_err(|_| Error::Unavailable)?;
        let name = e.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if !known(name) || !e.file_type().map_err(|_| Error::Unavailable)?.is_file() {
            return Err(Error::Invalid);
        }
        if name != "LOCK" {
            fs::remove_file(e.path()).map_err(|_| Error::Unavailable)?;
        }
    }
    sync(dir)?;
    Ok(())
}
pub fn remove_all(log: &RecordLog, guard: &dyn LifecycleGuard) -> Result<()> {
    let b = binding(log, guard)?;
    let _lock = lock(guard.directory(), false)?;
    remove_registered(guard.directory())?;
    unchanged(log, guard, &b)
}
