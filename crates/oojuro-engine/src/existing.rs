//! Existing-authority preflight only; never creates, truncates or repairs files.
use std::{fs, io::Read, os::unix::fs::OpenOptionsExt, path::Path};
fn regular(path: &Path, empty: bool) -> Result<(), ()> {
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    let m = f.metadata().map_err(|_| ())?;
    if !m.is_file() || (!empty && m.len() == 0) {
        return Err(());
    }
    Ok(())
}
pub(crate) fn check(dir: &Path, record_format: Option<&[u8]>) -> Result<(), ()> {
    let m = fs::symlink_metadata(dir).map_err(|_| ())?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(());
    }
    for (name, empty) in [("MANIFEST", false), ("ENCRYPTED", false), ("LOCK", true)] {
        regular(&dir.join(name), empty)?;
    }
    if let Some(expected) = record_format {
        let f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("RECORD_FORMAT"))
            .map_err(|_| ())?;
        if !f.metadata().map_err(|_| ())?.is_file() {
            return Err(());
        }
        let mut bytes = Vec::new();
        f.take(expected.len() as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes != expected {
            return Err(());
        }
    }
    let mut payload = false;
    for item in fs::read_dir(dir).map_err(|_| ())? {
        let item = item.map_err(|_| ())?;
        let name = item.file_name();
        let name = name.to_str().ok_or(())?;
        if (name.starts_with("hot-") && name.ends_with(".ojl"))
            || (name.starts_with("cold-") && name.ends_with(".ojc"))
        {
            regular(&item.path(), false)?;
            payload = true;
        }
    }
    if !payload {
        return Err(());
    }
    Ok(())
}
