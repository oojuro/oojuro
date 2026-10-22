use super::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

const CONFIG: &str = "native.json";
pub(super) const POLICY: &str = "policy.json";
const KEY: &str = "key";
pub(super) const JOURNAL: &str = "journal";

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Harness {
    Codex,
    ClaudeCode,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub version: u32,
    pub harness: Harness,
    pub writer: String,
    pub agent: u32,
    pub first_id: u64,
}
impl Config {
    pub fn check(&self) -> Result<()> {
        if self.version != 1
            || self.writer.is_empty()
            || self.writer.len() > 256
            || self.writer.chars().any(char::is_control)
            || self.agent == 0
            || self.first_id == 0
        {
            return Err(Error::Config);
        }
        Ok(())
    }
}

fn private(metadata: &fs::Metadata, mode: u32) -> Result<()> {
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o777 != mode
    {
        return Err(Error::Permissions);
    }
    Ok(())
}
pub(super) fn check_root(root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(root).map_err(|_| Error::Io)?;
    if !metadata.is_dir() {
        return Err(Error::Permissions);
    }
    private(&metadata, 0o700)
}
pub(super) fn read_private(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::Io)?;
    let metadata = file.metadata().map_err(|_| Error::Io)?;
    if !metadata.is_file() {
        return Err(Error::Permissions);
    }
    private(&metadata, 0o600)?;
    if metadata.len() > limit as u64 {
        return Err(Error::Bounds);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > limit {
        return Err(Error::Bounds);
    }
    Ok(bytes)
}
pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Io)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Io)
}
pub(super) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| Error::Io)
}
pub(super) fn load(root: &Path) -> Result<(Config, [u8; 32])> {
    check_root(root)?;
    let config: Config = serde_json::from_slice(&read_private(&root.join(CONFIG), 4096)?)
        .map_err(|_| Error::Config)?;
    config.check()?;
    let bytes = read_private(&root.join(KEY), 32)?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Key)?;
    Ok((config, key))
}
pub(super) fn setup(root: &Path, config: &Config, policy: &[u8]) -> Result<()> {
    config.check()?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(root)
        .map_err(|_| Error::Io)?;
    let mut key = [0u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut key))
        .map_err(|_| Error::Key)?;
    write_new(&root.join(KEY), &key)?;
    write_new(
        &root.join(CONFIG),
        &serde_json::to_vec(config).map_err(|_| Error::Config)?,
    )?;
    write_new(&root.join(POLICY), policy)?;
    sync_dir(root)?;
    if let Some(parent) = root.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}
