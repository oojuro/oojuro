//! Explicit local file capture into the sealed raw journal (Unix).
//!
//! The caller chooses an authorized source path and a stable generation id. Policy is
//! checked before opening or even inspecting the source. This module never discovers files,
//! parses/redacts content, publishes it, or chooses a restore destination.
use crate::raw::{Cursor, MAX_BLOCK_BYTES, RawJournal, SourceId};
use oojuro_store::{Error, Result};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

/// Progress is a locally durable byte prefix, not a snapshot of a concurrently mutated file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureReport {
    pub cursor: Cursor,
    pub appended: u64,
    /// Length observed on the open descriptor before capture. Later appends wait for a pass.
    pub observed_length: u64,
}

/// Capture one explicitly chosen regular file. `None` means policy withheld the whole source.
///
/// `id` names a source GENERATION, not a pathname. Persist/reuse it across retries. Replacement,
/// truncation or changed retained bytes fails; the owner must deliberately assign a new id.
/// Callers must authorize the configured path, including parent directories, against their
/// trust boundary. This function refuses a final symlink and compares the opened inode, but
/// does not provide a sandbox against hostile parent-directory replacement.
///
/// Each new block is independently committed. Failure can leave an acknowledged durable prefix;
/// retry safely revalidates all retained bytes, then appends the remaining suffix. The whole
/// file is never claimed atomic. A producer must append only while this reader is active;
/// arbitrary concurrent in-place modification cannot be turned into a consistent snapshot.
/// Policy is per source: a parser's per-record thinking/secret filters do not authorize raw.
pub fn capture_file(
    journal: &mut RawJournal,
    id: SourceId,
    path: &Path,
    mut retain_raw: impl FnMut(&Path) -> bool,
) -> Result<Option<CaptureReport>> {
    if !retain_raw(path) {
        return Ok(None);
    }
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(Error::Invalid(
            "raw source must be a regular file, not a symlink",
        ));
    }
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
        return Err(Error::Invalid("raw source changed while opening"));
    }
    let mut descriptor = b"oojuro-file-source-1\0".to_vec();
    descriptor.extend_from_slice(&opened.dev().to_le_bytes());
    descriptor.extend_from_slice(&opened.ino().to_le_bytes());
    descriptor.extend_from_slice(path.as_os_str().as_bytes());
    // Registration seals path and inode provenance in the same journal; no cursor sidecar.
    journal.register(id, &descriptor)?;
    let start = journal.cursor(id)?.offset;
    if opened.len() < start {
        return Err(Error::Invalid(
            "raw source was truncated; use a new generation id",
        ));
    }
    let mut offset = 0_u64;
    let mut block = vec![0; MAX_BLOCK_BYTES];
    // Revalidate the ENTIRE retained prefix, not only its tail or current file length.
    // New bytes start only after every earlier retained byte has matched.
    while offset < opened.len() {
        let boundary = if offset < start { start } else { opened.len() };
        let n = (boundary - offset).min(MAX_BLOCK_BYTES as u64) as usize;
        file.read_exact(&mut block[..n])?;
        journal.append(id, offset, &block[..n])?;
        offset += n as u64;
    }
    let after = file.metadata()?;
    if after.len() < opened.len() {
        return Err(Error::Invalid(
            "raw source shrank during capture; use a new generation id",
        ));
    }
    let cursor = journal.cursor(id)?;
    Ok(Some(CaptureReport {
        cursor,
        appended: cursor.offset - start,
        observed_length: opened.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "oojuro-file-capture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const KEY: [u8; 32] = [93; 32];
    const A: SourceId = SourceId([17; 16]);
    const B: SourceId = SourceId([18; 16]);

    #[test]
    fn policy_precedes_even_source_inspection_and_never_registers_denied_sources() {
        let t = Temp::new();
        let mut j = RawJournal::open(&t.0.join("journal"), &KEY).unwrap();
        let missing = t.0.join("missing");
        let mut decisions = 0;
        assert!(
            capture_file(&mut j, A, &missing, |p| {
                assert_eq!(p, missing);
                decisions += 1;
                false
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(decisions, 1);
        assert_eq!(j.sources().unwrap().count(), 0);
        assert!(capture_file(&mut j, A, &missing, |_| true).is_err());
    }

    #[test]
    fn byte_exact_partial_tails_reopen_and_append_only_resume() {
        let t = Temp::new();
        let p = t.0.join("source.jsonl");
        let initial = b"{\"synthetic\":true}\r\n\xff\0partial";
        fs::write(&p, initial).unwrap();
        let d = t.0.join("journal");
        let mut j = RawJournal::open(&d, &KEY).unwrap();
        let first = capture_file(&mut j, A, &p, |_| true).unwrap().unwrap();
        assert_eq!(first.appended, initial.len() as u64);
        drop(j);
        File::options()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b" tail\n")
            .unwrap();
        let mut j = RawJournal::open(&d, &KEY).unwrap();
        assert_eq!(
            capture_file(&mut j, A, &p, |_| true)
                .unwrap()
                .unwrap()
                .appended,
            6
        );
        assert_eq!(
            capture_file(&mut j, A, &p, |_| true)
                .unwrap()
                .unwrap()
                .appended,
            0
        );
        let mut restored = Vec::new();
        j.restore(A, &mut restored).unwrap();
        assert_eq!(restored, [initial.as_slice(), b" tail\n"].concat());
        let descriptor = j.descriptor(A).unwrap();
        assert!(descriptor.ends_with(p.as_os_str().as_bytes()));
        assert!(descriptor.starts_with(b"oojuro-file-source-1\0"));
    }

    #[test]
    fn replacement_truncation_and_changed_early_prefix_fail_without_mixing_generations() {
        let t = Temp::new();
        let p = t.0.join("source");
        let mut expected = vec![b'a'; MAX_BLOCK_BYTES + 20];
        expected[4] = b'b';
        fs::write(&p, &expected).unwrap();
        let mut j = RawJournal::open(&t.0.join("journal"), &KEY).unwrap();
        capture_file(&mut j, A, &p, |_| true).unwrap();
        // Same inode and unchanged tail: only full-prefix validation sees this edit.
        let mut changed = expected.clone();
        changed[4] = b'c';
        changed.extend_from_slice(b"new");
        fs::write(&p, &changed).unwrap();
        assert!(capture_file(&mut j, A, &p, |_| true).is_err());
        assert_eq!(j.cursor(A).unwrap().offset, expected.len() as u64);
        fs::write(&p, b"short").unwrap();
        assert!(capture_file(&mut j, A, &p, |_| true).is_err());
        fs::rename(&p, t.0.join("old")).unwrap();
        fs::write(&p, &expected).unwrap();
        assert!(capture_file(&mut j, A, &p, |_| true).is_err());
        capture_file(&mut j, B, &p, |_| true).unwrap();
        let mut original = Vec::new();
        j.restore(A, &mut original).unwrap();
        assert_eq!(original, expected);
    }

    #[test]
    fn final_symlinks_and_non_regular_files_are_refused() {
        let t = Temp::new();
        let p = t.0.join("source");
        fs::write(&p, b"bytes").unwrap();
        let link = t.0.join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        let mut j = RawJournal::open(&t.0.join("journal"), &KEY).unwrap();
        assert!(capture_file(&mut j, A, &link, |_| true).is_err());
        assert!(capture_file(&mut j, A, &t.0, |_| true).is_err());
        assert_eq!(j.sources().unwrap().count(), 0);
    }
}
