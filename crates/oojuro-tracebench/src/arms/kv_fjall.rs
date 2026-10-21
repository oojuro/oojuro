//! fjall arm (`fjall`): the trace layout of [`super::kv`] in eight fjall
//! keyspaces (one LSM-tree each, one shared journal), fjall 3.1 defaults
//! (LZ4 blocks, bloom filters, leveled compaction).
//!
//! Durability: every `append` is one `WriteBatch` (atomic across
//! keyspaces) committed with `durability(Some(PersistMode::SyncAll))`: the
//! journal is written and fsynced (data + metadata) before the commit
//! returns. The journal writer is a mutex, so concurrent appends take turns
//! (each its own fsync). Bulk-load chunks commit without a persist;
//! `finish_load` persists the journal with `SyncAll`, flushes every
//! memtable and waits until flushes and compactions are idle (the settled
//! state, at most 15 min). Journals are preallocated (64 MiB, sparse):
//! `disk_bytes` counts allocated blocks, `details` the logical size too.
//! `reclaim` = rotate and flush every memtable, `major_compact` every
//! keyspace, then wait until sealed journals are gone. Erasure gap: fjall
//! rotates its journal only once it passes 64 MB (no API rotates it on
//! demand), so the active journal keeps deleted text until enough later
//! writes push it out; the purge suite's needle scan shows it.
//!
//! Compression is fjall's default policy: data blocks uncompressed in L0
//! and L1, LZ4 from L2 down (`compression=lz4` makes every level LZ4, like
//! the `rocksdb` arm).
//!
//! Params: `cache_mb=N` (block cache, default 256), `compression=lz4`,
//! `existing=on`.
use super::Params;
use super::kv::{DEFAULT_CACHE_MB, Engine, KEYSPACES, Ks, KvStore, Op, Reader};
use crate::{Result, TraceStore};
use fjall::{
    CompressionType, Database, Keyspace, KeyspaceCreateOptions, PersistMode, Readable, Snapshot,
    config::CompressionPolicy,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub struct Fjall {
    db: Database,
    ks: Vec<Keyspace>,
    dir: PathBuf,
    cache_mb: usize,
    lz4_all: bool,
}

pub fn open(work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let dir = work.join("fjall");
    if params.flag("existing", false) && !dir.exists() {
        return Err(format!("no existing store at {}", dir.display()).into());
    }
    let cache_mb = params.num("cache_mb", DEFAULT_CACHE_MB);
    let db = Database::builder(&dir).cache_size((cache_mb as u64) << 20).open()?;
    let lz4_all = params.get("compression") == Some("lz4");
    let opts = || {
        let o = KeyspaceCreateOptions::default();
        if lz4_all { o.data_block_compression_policy(CompressionPolicy::all(CompressionType::Lz4)) } else { o }
    };
    let ks = KEYSPACES.iter().map(|n| db.keyspace(n, opts)).collect::<fjall::Result<Vec<_>>>()?;
    Ok(Box::new(KvStore::new(Fjall { db, ks, dir, cache_mb, lz4_all })))
}

struct View<'a> {
    snap: Snapshot,
    ks: &'a [Keyspace],
}

impl Reader for View<'_> {
    fn get(&self, ks: Ks, key: &[u8], f: &mut dyn FnMut(&[u8])) -> Result<bool> {
        match self.snap.get(&self.ks[ks as usize], key)? {
            Some(v) => {
                f(&v);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn scan(&self, ks: Ks, from: &[u8], to: &[u8], f: &mut dyn FnMut(&[u8], &[u8]) -> bool) -> Result<()> {
        if from >= to {
            return Ok(());
        }
        for g in self.snap.range(&self.ks[ks as usize], from..to) {
            let (k, v) = g.into_inner()?;
            if !f(&k, &v) {
                break;
            }
        }
        Ok(())
    }
}

impl Fjall {
    /// Wait until no flush or compaction is running (or `max` passes).
    fn wait_idle(&self, max: Duration) {
        let t0 = Instant::now();
        let mut quiet = 0;
        while quiet < 4 && t0.elapsed() < max {
            let busy = self.db.outstanding_flushes() + self.db.active_compactions();
            quiet = if busy == 0 { quiet + 1 } else { 0 };
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Engine for Fjall {
    fn name(&self) -> String {
        let comp = if self.lz4_all { "LZ4 all levels" } else { "default compression: L0-L1 none, L2+ LZ4" };
        format!("fjall 3.1.12 (WriteBatch + PersistMode::SyncAll per append, {comp}, cache {} MiB)", self.cache_mb)
    }

    fn read(&self, f: &mut dyn FnMut(&dyn Reader) -> Result<()>) -> Result<()> {
        f(&View { snap: self.db.snapshot(), ks: &self.ks })
    }

    fn write(&self, ops: Vec<Op>, sync: bool) -> Result<()> {
        let mut b = self.db.batch();
        if sync {
            b = b.durability(Some(PersistMode::SyncAll));
        }
        for op in ops {
            match op {
                Op::Put(ks, k, v) => b.insert(&self.ks[ks as usize], k, v),
                Op::Del(ks, k) => b.remove(&self.ks[ks as usize], k),
            }
        }
        b.commit()?;
        Ok(())
    }

    /// Durable (journal fsync), then flush every memtable into tables (the
    /// LSM's checkpoint: the journals they came from can be dropped) and wait
    /// until flushes and compactions are idle.
    fn settle(&self) -> Result<()> {
        self.db.persist(PersistMode::SyncAll)?;
        for ks in &self.ks {
            ks.rotate_memtable_and_wait()?;
        }
        self.wait_idle(Duration::from_secs(900));
        Ok(())
    }

    fn compact(&self) -> Result<()> {
        for ks in &self.ks {
            ks.rotate_memtable_and_wait()?;
        }
        for ks in &self.ks {
            ks.major_compact()?;
        }
        self.db.persist(PersistMode::SyncAll)?;
        self.wait_idle(Duration::from_secs(600));
        // journals whose data is all flushed are deleted by fjall; wait for it
        let t0 = Instant::now();
        while self.db.journal_count() > 1 && t0.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn details(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        for (i, ks) in self.ks.iter().enumerate() {
            out.insert(
                KEYSPACES[i].into(),
                serde_json::json!({"disk_space": ks.disk_space(), "tables": ks.table_count()}),
            );
        }
        out.insert("journal_count".into(), serde_json::json!(self.db.journal_count()));
        out.insert("journal_bytes".into(), serde_json::json!(self.db.journal_disk_space().unwrap_or(0)));
        serde_json::Value::Object(out)
    }
}
