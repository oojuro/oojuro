//! Full owned namespace inventory, including orphan candidates and temporary names.
//! Read-only enumeration never establishes current fencing or unlink permission.
use super::{Error, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    Pack,
    Dictionary,
}
#[derive(Clone, Debug)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: ArtifactKind,
    pub temporary: bool,
    pub byte_len: u64,
}
/// O(1) entries retained; order is the filesystem's directory order.
/// Requires exclusive fenced namespace ownership for a stable complete inventory.
/// Unknown files, directories, symlinks and noncanonical names refuse. Recognized
/// corrupt/truncated/oversized files are still enumerated for complete retirement.
pub fn scan_inventory(
    directory: &Path,
    mut visit: impl FnMut(Artifact) -> Result<()>,
) -> Result<()> {
    super::codec::prepare(directory)?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or(Error::Invalid("foreign managed pack filename"))?;
        let (kind, rest) = if let Some(v) = name.strip_prefix("data-") {
            (ArtifactKind::Pack, v)
        } else if let Some(v) = name.strip_prefix("dict-") {
            (ArtifactKind::Dictionary, v)
        } else {
            return Err(Error::Invalid("foreign managed pack file"));
        };
        let (token, temporary) = if let Some(v) = rest.strip_suffix(".bin") {
            (v, false)
        } else if let Some(v) = rest.strip_suffix(".stage") {
            (v, true)
        } else {
            return Err(Error::Invalid("foreign managed pack suffix"));
        };
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("noncanonical managed pack token"));
        }
        let path = entry.path();
        let m = fs::symlink_metadata(&path)?;
        if !m.is_file() || m.file_type().is_symlink() {
            return Err(Error::Invalid("foreign managed pack entry type"));
        }
        visit(Artifact {
            path,
            kind,
            temporary,
            byte_len: m.len(),
        })?;
    }
    Ok(())
}
