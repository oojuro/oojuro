//! redb arm (`redb`): the trace layout of [`super::kv`] in eight redb tables
//! of one file (copy-on-write B-trees, one writer at a time).
//!
//! Durability: every `append` is one write transaction committed with
//! `Durability::Immediate` (redb's default: 1-phase commit with checksums,
//! one fsync per commit, durable on return). Bulk-load chunks commit with
//! `Durability::None`; `finish_load` commits an empty `Immediate`
//! transaction, which makes them durable. redb has one writer at a time, so
//! concurrent appends queue on it and each pays its own fsync (no group
//! commit). `reclaim` = `Database::compact` until it reports no progress.
//!
//! Params: `cache_mb=N` (default 256; redb's own default is 1 GiB),
//! `quick_repair=on` (redb saves allocator state each commit and uses
//! 2-phase commit: slower commits, near-instant recovery after a crash; off
//! by default, as in redb, so reopening after a crash runs a full repair),
//! `existing=on` (open the file a previous run left).
use super::Params;
use super::kv::{DEFAULT_CACHE_MB, Engine, KEYSPACES, Ks, KvStore, Op, Reader};
use crate::{Result, TraceStore};
use redb::{
    Builder, Database, Durability, ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTableMetadata,
    TableDefinition,
};
use std::{
    cell::OnceCell,
    path::{Path, PathBuf},
    sync::RwLock,
};

type Def = TableDefinition<'static, &'static [u8], &'static [u8]>;

const TABLES: [Def; 8] = [
    TableDefinition::new(KEYSPACES[0]),
    TableDefinition::new(KEYSPACES[1]),
    TableDefinition::new(KEYSPACES[2]),
    TableDefinition::new(KEYSPACES[3]),
    TableDefinition::new(KEYSPACES[4]),
    TableDefinition::new(KEYSPACES[5]),
    TableDefinition::new(KEYSPACES[6]),
    TableDefinition::new(KEYSPACES[7]),
];

pub struct Redb {
    /// Shared by reads and writes; `compact` needs it exclusively.
    db: RwLock<Database>,
    dir: PathBuf,
    cache_mb: usize,
    quick_repair: bool,
}

pub fn open(work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let dir = work.join("redb");
    let path = dir.join("trace.redb");
    let existing = params.flag("existing", false);
    let cache_mb = params.num("cache_mb", DEFAULT_CACHE_MB);
    let quick_repair = params.flag("quick_repair", false);
    let mut b = Builder::new();
    b.set_cache_size(cache_mb << 20);
    let db = if existing {
        if !path.exists() {
            return Err(format!("no existing store at {}", path.display()).into());
        }
        b.open(&path)?
    } else {
        std::fs::create_dir_all(&dir)?;
        let db = b.create(&path)?;
        // create every table, so read transactions can open them
        let tx = db.begin_write()?;
        for t in TABLES {
            tx.open_table(t)?;
        }
        tx.commit()?;
        db
    };
    Ok(Box::new(KvStore::new(Redb { db: RwLock::new(db), dir, cache_mb, quick_repair })))
}

struct View {
    tx: ReadTransaction,
    tables: [OnceCell<ReadOnlyTable<&'static [u8], &'static [u8]>>; 8],
}

impl View {
    fn table(&self, ks: Ks) -> Result<&ReadOnlyTable<&'static [u8], &'static [u8]>> {
        let i = ks as usize;
        if self.tables[i].get().is_none() {
            let t = self.tx.open_table(TABLES[i])?;
            let _ = self.tables[i].set(t);
        }
        Ok(self.tables[i].get().expect("opened"))
    }
}

impl Reader for View {
    fn get(&self, ks: Ks, key: &[u8], f: &mut dyn FnMut(&[u8])) -> Result<bool> {
        match redb::ReadableTable::get(self.table(ks)?, key)? {
            Some(v) => {
                f(v.value());
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn scan(&self, ks: Ks, from: &[u8], to: &[u8], f: &mut dyn FnMut(&[u8], &[u8]) -> bool) -> Result<()> {
        if from >= to {
            return Ok(());
        }
        for kv in self.table(ks)?.range::<&[u8]>(from..to)? {
            let (k, v) = kv?;
            if !f(k.value(), v.value()) {
                break;
            }
        }
        Ok(())
    }
}

impl Engine for Redb {
    fn name(&self) -> String {
        format!(
            "redb 4.4.0 (Durability::Immediate per append, cache {} MiB{})",
            self.cache_mb,
            if self.quick_repair { ", quick_repair" } else { "" }
        )
    }

    fn read(&self, f: &mut dyn FnMut(&dyn Reader) -> Result<()>) -> Result<()> {
        let db = self.db.read().unwrap();
        let v = View { tx: db.begin_read()?, tables: Default::default() };
        f(&v)
    }

    fn write(&self, ops: Vec<Op>, sync: bool) -> Result<()> {
        let db = self.db.read().unwrap();
        let mut tx = db.begin_write()?;
        tx.set_durability(if sync { Durability::Immediate } else { Durability::None })?;
        if self.quick_repair {
            tx.set_quick_repair(true);
        }
        {
            let mut tables: [Option<redb::Table<&[u8], &[u8]>>; 8] = Default::default();
            for op in &ops {
                let (ks, k, v) = match op {
                    Op::Put(ks, k, v) => (*ks, k, Some(v)),
                    Op::Del(ks, k) => (*ks, k, None),
                };
                let i = ks as usize;
                if tables[i].is_none() {
                    tables[i] = Some(tx.open_table(TABLES[i])?);
                }
                let t = tables[i].as_mut().expect("opened");
                match v {
                    Some(v) => {
                        t.insert(k.as_slice(), v.as_slice())?;
                    }
                    None => {
                        t.remove(k.as_slice())?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// An empty `Immediate` commit persists every `Durability::None` commit
    /// before it.
    fn settle(&self) -> Result<()> {
        self.write(Vec::new(), true)
    }

    fn compact(&self) -> Result<()> {
        let mut db = self.db.write().unwrap();
        for _ in 0..64 {
            if !db.compact()? {
                break;
            }
        }
        Ok(())
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn details(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        let db = self.db.read().unwrap();
        if let Ok(tx) = db.begin_read() {
            for (i, t) in TABLES.iter().enumerate() {
                if let Ok(tab) = tx.open_table(*t)
                    && let Ok(s) = tab.stats()
                {
                    out.insert(
                        KEYSPACES[i].into(),
                        serde_json::json!({"stored_bytes": s.stored_bytes(), "metadata_bytes": s.metadata_bytes(),
                                           "fragmented_bytes": s.fragmented_bytes(), "leaf_pages": s.leaf_pages()}),
                    );
                }
            }
        }
        serde_json::Value::Object(out)
    }
}
