//! Crash-safe capture state: per-file offsets and parse context, the writer
//! registry, and the next record id.
//!
//! `capture-state.json` always describes **committed** data: an offset only
//! moves past a line once every record of that line is durable in the store.
//! It is replaced atomically (write temp, fsync, rename, fsync dir). Saving it
//! after every commit is not required for correctness: on open, records the
//! store holds beyond `next_id` are found and their source positions are
//! skipped when their lines are read again (see `ingest::Capture::open`).
//! New files and writers are saved *before* any record that refers to them.
use crate::{FileCtx, Format, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::Path,
};

pub const STATE_FILE: &str = "capture-state.json";
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileState {
    /// Short file number (record source positions refer to it).
    pub no: u32,
    pub format: Format,
    /// Byte offset just after the last committed line.
    pub offset: u64,
    /// File identity (device, inode); a new identity at the same path is a new file.
    pub dev: u64,
    pub ino: u64,
    pub ctx: FileCtx,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// First record id not known to be committed.
    pub next_id: u64,
    pub next_file_no: u32,
    /// Writer ID -> store number (`oojuro_trace` agent).
    pub writers: BTreeMap<String, u32>,
    /// Path -> file state.
    pub files: BTreeMap<String, FileState>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: VERSION, next_id: 1, next_file_no: 1, writers: BTreeMap::new(), files: BTreeMap::new() }
    }
}

impl State {
    pub fn load(dir: &Path) -> Result<Self> {
        let p = dir.join(STATE_FILE);
        if !p.exists() {
            return Ok(Self::default());
        }
        let s: State = serde_json::from_slice(&fs::read(&p)?)?;
        if s.version != VERSION {
            return Err(format!("capture state version {} (expected {VERSION})", s.version).into());
        }
        Ok(s)
    }

    /// Atomic replace: temp file, fsync, rename, fsync the directory.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(self)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, dir.join(STATE_FILE))?;
        File::open(dir)?.sync_all()?;
        Ok(())
    }

    /// Store number of a writer, registering it if new (returns true if new).
    pub fn writer_agent(&mut self, writer: &str) -> (u32, bool) {
        if let Some(a) = self.writers.get(writer) {
            return (*a, false);
        }
        let a = self.writers.values().copied().max().unwrap_or(0) + 1;
        self.writers.insert(writer.to_string(), a);
        (a, true)
    }

    pub fn file_no_owner(&self, no: u32) -> Option<&str> {
        self.files.iter().find(|(_, f)| f.no == no).map(|(p, _)| p.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip_is_atomic() {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("oojuro-capture-state-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let mut s = State::default();
        assert_eq!(s.writer_agent("claude-code:-x"), (1, true));
        assert_eq!(s.writer_agent("codex:-x"), (2, true));
        assert_eq!(s.writer_agent("claude-code:-x"), (1, false));
        s.files.insert(
            "/x/a.jsonl".into(),
            FileState { no: 1, format: Format::ClaudeCode, offset: 42, dev: 1, ino: 2, ctx: FileCtx::default() },
        );
        s.next_id = 99;
        s.save(&d).unwrap();
        assert!(!d.join(format!("{STATE_FILE}.tmp")).exists());
        assert_eq!(State::load(&d).unwrap(), s);
        fs::remove_dir_all(&d).unwrap();
    }
}
