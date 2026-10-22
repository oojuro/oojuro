//! Complete owned DATA paths; no recursive arbitrary-path deletion.
use super::*;
use driver::{ManagedPagedLog, commitment, data_directory};
use schema::RootName as N;
use std::io::Read;
// At most nine owned directories: namespace, packs, trees and six tree purposes.
// Prepay their visits/removals plus root enumeration conservatively; files are
// charged individually. Empty namespaces therefore cannot evade the work budget.
pub(super) const NAMESPACE_WORK: u64 = 20;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    path: String,
    bytes: u64,
    digest: [u8; 32],
}
pub(super) fn namespace(
    directory: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<()> {
    let m = fs::symlink_metadata(directory).map_err(io)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Authority);
    }
    // Every child must be recognized, including orphan and partial artifacts.
    for item in fs::read_dir(directory).map_err(io)? {
        let item = item.map_err(io)?;
        let p = item.path();
        let m = fs::symlink_metadata(&p).map_err(io)?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(Error::Authority);
        }
        match item.file_name().to_str() {
            Some("packs") => oojuro_store::managed_pack::scan_inventory(
                &p,
                &mut |a: oojuro_store::managed_pack::Artifact| {
                    visit(&a.path)
                        .map_err(|_| oojuro_store::Error::Corrupt("managed pack inventory refused"))
                },
            )
            .map_err(io)?,
            Some("trees") => {
                for child in fs::read_dir(&p).map_err(io)? {
                    let child = child.map_err(io)?;
                    let cp = child.path();
                    let m = fs::symlink_metadata(&cp).map_err(io)?;
                    if !m.is_dir() || m.file_type().is_symlink() {
                        return Err(Error::Authority);
                    }
                    if ![
                        "record",
                        "key-version",
                        "segment",
                        "frame",
                        "file",
                        "dictionary",
                    ]
                    .iter()
                    .any(|n| child.file_name() == *n)
                    {
                        return Err(Error::Authority);
                    }
                    tree::scan_owned_paths(&cp, &mut |a: tree::Artifact| visit(&a.path))?;
                }
            }
            _ => return Err(Error::Authority),
        }
    }
    Ok(())
}
fn root_files(log: &ManagedPagedLog, visit: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    for n in ["MANAGED_FORMAT", "ENROLLMENT", "LOCK"] {
        let p = log.directory.join(n);
        let m = fs::symlink_metadata(&p).map_err(io)?;
        if !m.is_file() || m.file_type().is_symlink() {
            return Err(Error::Authority);
        }
        visit(&p)?;
    }
    Ok(())
}
fn relative(log: &ManagedPagedLog, live: &Path, path: &Path) -> Result<String> {
    if let Ok(p) = path.strip_prefix(live) {
        return Ok(format!("LIVE/{}", p.to_str().ok_or(Error::Authority)?));
    }
    for n in ["MANAGED_FORMAT", "ENROLLMENT", "LOCK"] {
        if path == log.directory.join(n) {
            return Ok(format!("ROOT/{n}"));
        }
    }
    Err(Error::Scope)
}
fn describe(
    log: &ManagedPagedLog,
    live: &Path,
    path: &Path,
    work: &mut proof::Work,
) -> Result<File> {
    work.charge_rows(1)?;
    let m = fs::symlink_metadata(path).map_err(io)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(Error::Authority);
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    let opened = f.metadata().map_err(io)?;
    use std::os::unix::fs::MetadataExt;
    if opened.ino() != m.ino() || opened.dev() != m.dev() {
        return Err(Error::Authority);
    }
    let mut hash = blake3::Hasher::new();
    let mut buf = [0u8; 64 << 10];
    let mut bytes = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        work.charge_bytes(n as u64)?;
        hash.update(&buf[..n]);
        bytes = bytes.checked_add(n as u64).ok_or(Error::Bounds)?;
    }
    let after = fs::symlink_metadata(path).map_err(io)?;
    if bytes != m.len()
        || after.len() != m.len()
        || after.ino() != m.ino()
        || after.dev() != m.dev()
        || after.file_type().is_symlink()
    {
        return Err(Error::Authority);
    }
    Ok(File {
        path: relative(log, live, path)?,
        bytes,
        digest: *hash.finalize().as_bytes(),
    })
}
pub(super) fn build(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    stage: &checkpoint::Stage<'_>,
    work: &mut proof::Work,
) -> Result<(tree::Root, u64, u64)> {
    let live = data_directory(&log.directory, root)?;
    let paths = source::Paths::new(&log.control.directory, &live);
    directory_create(paths.get(N::PruneFiles))?;
    let ctx = paths.ctx(
        N::PruneFiles,
        owner.control_cipher(),
        stage,
        owner.state.identity,
    );
    let empty = tree::Builder::new(ctx.clone())?.finish()?;
    let mut editor = tree::Editor::new(ctx, empty)?;
    let mut count = 0u64;
    let mut total = 0u64;
    let mut add = |p: &Path| {
        log.witness_current(owner)?;
        let f = describe(log, &live, p, work)?;
        total = total.checked_add(f.bytes).ok_or(Error::Bounds)?;
        let key = commitment(&root.intent, &("prune-file-v1", &f.path))?;
        editor.insert_immutable(tree::Entry::new(key.to_vec(), json(&f, 4096)?)?)?;
        count = count.checked_add(1).ok_or(Error::Bounds)?;
        log.witness_current(owner)
    };
    namespace(&live, &mut add)?;
    root_files(log, &mut add)?;
    Ok((editor.finish()?, count, total))
}
pub(super) fn verify(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    authority: &dyn tree::Authority,
    work: &mut proof::Work,
) -> Result<()> {
    let p = root.prune.as_ref().ok_or(Error::Authority)?;
    let files = p.files.as_ref().ok_or(Error::Authority)?;
    let live = data_directory(&log.directory, root)?;
    let paths = source::Paths::new(&log.control.directory, &live);
    let ctx = paths.ctx(
        N::PruneFiles,
        owner.control_cipher(),
        authority,
        owner.state.identity,
    );
    let mut count = 0u64;
    let mut total = 0u64;
    let mut check = |path: &Path| {
        log.witness_current(owner)?;
        let f = describe(log, &live, path, work)?;
        total = total.checked_add(f.bytes).ok_or(Error::Bounds)?;
        let key = commitment(&root.intent, &("prune-file-v1", &f.path))?;
        let bytes = tree::get_ancestor(ctx.clone(), files, &key)?.ok_or(Error::Authority)?;
        let expected: File = serde_json::from_slice(&bytes).map_err(|_| Error::Authority)?;
        if json(&expected, 4096)? != json(&f, 4096)? {
            return Err(Error::Authority);
        }
        count = count.checked_add(1).ok_or(Error::Bounds)?;
        log.witness_current(owner)
    };
    namespace(&live, &mut check)?;
    root_files(log, &mut check)?;
    if count != p.file_count || count != files.count || total != p.file_bytes {
        return Err(Error::Authority);
    }
    Ok(())
}
/// Streams exactly the future live files used by the precommit residue proof.
pub(super) fn proof_files(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<()> {
    let live = data_directory(&log.directory, root)?;
    let mut guarded = |p: &Path| {
        log.witness_current(owner)?;
        visit(p)?;
        log.witness_current(owner)
    };
    namespace(&live, &mut guarded)?;
    root_files(log, &mut guarded)
}
/// Must hold the exclusive data owner and current pending-cleanup control lock.
/// Names are checked before deleting individual files; unknown state remains pending.
pub(super) fn retire(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    work: &mut proof::Work,
) -> Result<()> {
    let live = data_directory(&log.directory, root)?;
    if root.prune.as_ref().ok_or(Error::Authority)?.phase
        != schema::PrunePhase::RewrittenPendingCleanup
    {
        return Err(Error::Authority);
    }
    let mut fence = || log.witness_current(owner);
    owned_namespaces(&log.directory, &mut |p| {
        work.charge_rows(NAMESPACE_WORK)?;
        fence()?;
        if p != live {
            remove_namespace(p, &mut fence, work)?;
            sync_dir(&log.directory)?;
        }
        Ok(())
    })?;
    owned_namespaces(&log.directory, &mut |p| {
        work.charge_rows(1)?;
        fence()?;
        if p != live {
            return Err(Error::Authority);
        }
        Ok(())
    })?;
    fence()
}
/// All DATA-root children are checked, even empty/unreferenced namespaces.
pub(super) fn owned_namespaces(
    directory: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<()> {
    for item in fs::read_dir(directory).map_err(io)? {
        let item = item.map_err(io)?;
        let path = item.path();
        let m = fs::symlink_metadata(&path).map_err(io)?;
        if ["MANAGED_FORMAT", "ENROLLMENT", "LOCK"]
            .iter()
            .any(|n| item.file_name() == *n)
        {
            if !m.is_file() || m.file_type().is_symlink() {
                return Err(Error::Authority);
            }
            continue;
        }
        let name = item.file_name();
        let n = name.to_str().ok_or(Error::Authority)?;
        if n.len() != 69
            || !n.starts_with("data-")
            || !n[5..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !m.is_dir()
            || m.file_type().is_symlink()
        {
            return Err(Error::Authority);
        }
        visit(&path)?;
    }
    Ok(())
}
pub(super) fn remove_namespace(
    path: &Path,
    fence: &mut dyn FnMut() -> Result<()>,
    work: &mut proof::Work,
) -> Result<()> {
    fence()?;
    namespace(path, &mut |p| {
        fence()?;
        work.charge_rows(1)?;
        fs::remove_file(p).map_err(io)?;
        fence()
    })?;
    let trees = path.join("trees");
    if trees.exists() {
        for child in fs::read_dir(&trees).map_err(io)? {
            fence()?;
            fs::remove_dir(child.map_err(io)?.path()).map_err(io)?;
        }
        fs::remove_dir(&trees).map_err(io)?;
    }
    let packs = path.join("packs");
    if packs.exists() {
        fs::remove_dir(packs).map_err(io)?;
    }
    fence()?;
    fs::remove_dir(path).map_err(io)?;
    fence()
}
/// Validate all retired namespaces and exact remaining cleanup work before publication.
pub(super) fn preflight_retire(
    log: &ManagedPagedLog,
    owner: &control::Inner,
    root: &schema::CommitRoot,
    work: &mut proof::Work,
) -> Result<u64> {
    let live = data_directory(&log.directory, root)?;
    // The final absence pass should encounter only the surviving live namespace.
    let mut count = 1u64;
    owned_namespaces(&log.directory, &mut |p| {
        work.charge_rows(NAMESPACE_WORK)?;
        count = count.checked_add(NAMESPACE_WORK).ok_or(Error::Bounds)?;
        log.witness_current(owner)?;
        if p != live {
            namespace(p, &mut |_| {
                log.witness_current(owner)?;
                work.charge_rows(1)?;
                count = count.checked_add(1).ok_or(Error::Bounds)?;
                Ok(())
            })?;
        }
        Ok(())
    })?;
    log.witness_current(owner)?;
    Ok(count)
}
