//! Derived tables under an authentic held managed generation and actual data DEK.
use super::*;
use crate::record::managed::ManagedRankView;
const PURPOSE: &str = "rank-tables-v1";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedEntry {
    id: String,
    version: String,
    table_version: String,
    content_version: String,
    payload: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedRegistry {
    format: u32,
    binding: crate::record::managed::Binding,
    stamp: [u8; 32],
    entries: Vec<ManagedEntry>,
}
fn read(view: &ManagedRankView<'_>) -> Result<ManagedRegistry> {
    view.validate_current()?;
    let sealed = read_sealed(view.directory())?;
    let plain = view.open_derived(PURPOSE, &sealed)?;
    let r: ManagedRegistry = serde_json::from_slice(&plain).map_err(|_| Error::Invalid)?;
    if r.format != 1
        || r.binding != *view.binding()
        || r.stamp != view.stamp()
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
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || e.payload.len() > PAYLOAD
            || !ids.insert(&e.id)
        {
            return Err(Error::Invalid);
        }
    }
    view.validate_current()?;
    Ok(r)
}
/// Explicit build only. No callback or query can synthesize this authority capability.
pub fn build(
    view: &ManagedRankView<'_>,
    options: Build<'_>,
    builder: impl FnOnce(&[Source<'_>], &mut Writer, &mut WorkBudget) -> Result<()>,
) -> Result<()> {
    let Build {
        id,
        version,
        table_version,
        work_units,
    } = options;
    if !label(id) || !label(version) || !label(table_version) || work_units > 1_000_000 {
        return Err(Error::Bounds);
    }
    view.validate_current()?;
    let dir = view.create_derived_directory()?;
    let _lock = lock(dir, true)?;
    let rows = view
        .records()
        .iter()
        .map(|r| Source {
            id: r.id,
            valid_at: r.valid_at,
            recorded_at: r.recorded_at,
            text: &r.text,
        })
        .collect::<Vec<_>>();
    let mut writer = Writer {
        bytes: vec![],
        full: false,
    };
    let mut budget = WorkBudget::new(work_units);
    view.validate_current()?;
    builder(&rows, &mut writer, &mut budget)?;
    view.validate_current()?;
    if writer.full || budget.exhausted() {
        return Err(Error::Bounds);
    }
    // Explicit rebuilding may replace missing/corrupt/stale disposable data.
    let mut registry = read(view).unwrap_or(ManagedRegistry {
        format: 1,
        binding: view.binding().clone(),
        stamp: view.stamp(),
        entries: vec![],
    });
    registry.entries.retain(|e| e.id != id);
    if registry.entries.len() >= TABLES {
        return Err(Error::Bounds);
    }
    registry.entries.push(ManagedEntry {
        id: id.into(),
        version: version.into(),
        table_version: table_version.into(),
        content_version: oojuro_store::crypto::random_key()
            .iter()
            .map(|c| format!("{c:02x}"))
            .collect(),
        payload: writer.bytes,
    });
    let plain = json(&registry, REGISTRY)?;
    let sealed = view.seal_derived(PURPOSE, &plain)?;
    publish_sealed(dir, &sealed, || view.validate_current())
}
/// Existing-file-only load. Missing tables never create a generation directory or lock.
pub fn load(
    view: &ManagedRankView<'_>,
    id: &str,
    version: &str,
    table_version: &str,
) -> Result<Loaded> {
    if !label(id) || !label(version) || !label(table_version) {
        return Err(Error::Bounds);
    }
    view.validate_current()?;
    let _lock = lock(view.directory(), false)?;
    let mut registry = read(view)?;
    let at = registry
        .entries
        .iter()
        .position(|e| e.id == id && e.version == version && e.table_version == table_version)
        .ok_or(Error::Unavailable)?;
    let e = registry.entries.swap_remove(at);
    view.validate_current()?;
    Ok(Loaded {
        id: e.id,
        version: e.version,
        table_version: e.table_version,
        content_version: e.content_version,
        snapshot: Snapshot::Managed {
            binding: view.binding().clone(),
            stamp: view.stamp(),
        },
        payload: e.payload,
    })
}
pub(in crate::record) fn validate_loaded(view: &ManagedRankView<'_>, table: &Loaded) -> Result<()> {
    view.validate_current()?;
    match &table.snapshot {
        Snapshot::Managed { binding, stamp }
            if *binding == *view.binding() && *stamp == view.stamp() =>
        {
            Ok(())
        }
        _ => Err(Error::Authority),
    }
}
/// Only this current generation's registered disposable files; lifecycle retires older directories.
pub fn remove_all(view: &ManagedRankView<'_>) -> Result<()> {
    view.validate_current()?;
    let _lock = lock(view.directory(), false)?;
    remove_registered(view.directory())?;
    view.validate_current()
}
