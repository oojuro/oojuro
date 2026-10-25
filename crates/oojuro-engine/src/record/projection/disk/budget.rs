use super::*;
use std::{fs, os::unix::fs::MetadataExt, sync::Mutex};

/// Peak construction footprint of the new generation, including encrypted
/// spool/sort runs. Existing generations remain charged to their owner.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildMetrics {
    pub peak_logical_bytes: u64,
    pub peak_allocated_bytes_observed: u64,
    pub peak_files: usize,
}
impl BuildMetrics {
    /// Conservative operator reservation input, not an arbitrary-filesystem
    /// hard quota. Include filesystem allocation/preallocation slack per file
    /// and directory separately; enforce a filesystem quota for hard caps.
    pub fn reservation(
        logical_limit: u64,
        file_limit: usize,
        per_file_slack: u64,
        directory_slack: u64,
    ) -> Option<u64> {
        logical_limit
            .checked_add((file_limit as u64).checked_mul(per_file_slack)?)?
            .checked_add(directory_slack)
    }
}
#[derive(Default)]
struct State {
    files: BTreeMap<PathBuf, (u64, u64)>,
    logical: u64,
    allocated: u64,
    metrics: BuildMetrics,
    directories: Vec<PathBuf>,
}
pub(super) struct Budget {
    limit: u64,
    state: Mutex<State>,
}
impl Budget {
    pub fn new(root: &Path, directory: &Path, limit: u64) -> Self {
        Self {
            limit,
            state: Mutex::new(State {
                directories: vec![root.to_owned(), directory.to_owned()],
                ..State::default()
            }),
        }
    }
    pub fn reserve(&self, path: &Path, length: u64) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let previous = state.files.get(path).map_or(0, |p| p.0);
        let length = length.max(previous);
        let logical = state
            .logical
            .checked_add(length - previous)
            .ok_or(Error::Bounds)?;
        if logical > self.limit || (state.files.len() >= 65536 && !state.files.contains_key(path)) {
            return Err(Error::Bounds);
        }
        if let Some(entry) = state.files.get_mut(path) {
            entry.0 = length;
        } else {
            state.files.insert(path.to_owned(), (length, 0));
        }
        state.logical = logical;
        state.metrics.peak_logical_bytes = state.metrics.peak_logical_bytes.max(logical);
        state.metrics.peak_files = state.metrics.peak_files.max(state.files.len());
        Ok(())
    }
    fn observe_file(state: &mut State, path: &Path) -> Result<()> {
        // A reserved descriptor can be charged before its inode is created.
        if !path.exists() {
            return Ok(());
        }
        let allocated = fs::symlink_metadata(path)
            .map_err(|_| Error::Unavailable)?
            .blocks()
            .checked_mul(512)
            .ok_or(Error::Bounds)?;
        if let Some((_, previous)) = state.files.get_mut(path) {
            state.allocated = state
                .allocated
                .checked_sub(*previous)
                .and_then(|v| v.checked_add(allocated))
                .ok_or(Error::Bounds)?;
            *previous = allocated;
        }
        Ok(())
    }
    fn sample(&self, state: &mut State) -> Result<()> {
        let mut total = state.allocated;
        for directory in &state.directories {
            if directory.exists() {
                total = total
                    .checked_add(
                        fs::metadata(directory)
                            .map_err(|_| Error::Unavailable)?
                            .blocks()
                            .checked_mul(512)
                            .ok_or(Error::Bounds)?,
                    )
                    .ok_or(Error::Bounds)?;
            }
        }
        state.metrics.peak_allocated_bytes_observed =
            state.metrics.peak_allocated_bytes_observed.max(total);
        Ok(())
    }
    pub fn observe(&self) -> Result<BuildMetrics> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let paths: Vec<_> = state.files.keys().cloned().collect();
        for path in paths {
            Self::observe_file(&mut state, &path)?;
        }
        self.sample(&mut state)?;
        Ok(state.metrics)
    }
    pub fn relocate(&self, source: &Path, target: &Path) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let old = std::mem::take(&mut state.files);
        for (path, entry) in old {
            let path = path
                .strip_prefix(source)
                .map_or_else(|_| path.clone(), |suffix| target.join(suffix));
            state.files.insert(path, entry);
        }
        for directory in &mut state.directories {
            if directory == source {
                *directory = target.to_owned();
            }
        }
        Ok(())
    }
    pub fn rename(&self, source: &Path, target: &Path) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        Self::observe_file(&mut state, source)?;
        self.sample(&mut state)?;
        fs::rename(source, target).map_err(|_| Error::Unavailable)?;
        if let Some(entry) = state.files.remove(source) {
            // A newly published manifest replaces the prior generation's
            // descriptor, which is charged separately to that owner.
            if state.files.insert(target.to_owned(), entry).is_some() {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    pub fn remove(&self, path: &Path) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        Self::observe_file(&mut state, path)?;
        self.sample(&mut state)?;
        fs::remove_file(path).map_err(|_| Error::Unavailable)?;
        if let Some((logical, allocated)) = state.files.remove(path) {
            state.logical -= logical;
            state.allocated -= allocated;
        }
        Ok(())
    }
}
