//! RocksDB arm (`rocksdb`): the trace layout of [`super::kv`] in eight column
//! families of one RocksDB (rust-rocksdb `rocksdb` crate 0.25, bundled
//! RocksDB 11.8.1 built from source).
//!
//! Durability: every `append` is one `WriteBatch` (atomic across column
//! families) written with `WriteOptions::set_sync(true)`: the WAL is
//! fdatasync'ed before the write returns. Concurrent writers are group
//! committed by RocksDB (one WAL sync can cover several writers' batches).
//! Bulk-load chunks are written without sync; `finish_load` syncs the WAL,
//! flushes every memtable and waits until compactions are idle (at most 15
//! min). `reclaim` = flush every column family, then a full
//! `compact_range` with `BottommostLevelCompaction::Force` per column
//! family (rewrites every SST without the tombstoned keys); the WALs that
//! held deleted data become obsolete once every memtable is flushed and are
//! deleted by RocksDB.
//!
//! Options (the rest are RocksDB defaults): LZ4 for every level, block
//! cache `cache_mb` MiB shared by all column families, full bloom filters
//! (10 bits/key), `db_write_buffer_size` 256 MiB across all memtables.
//! Params: `cache_mb=N` (default 256), `existing=on`.
use super::Params;
use super::kv::{DEFAULT_CACHE_MB, Engine, KEYSPACES, Ks, KvStore, Op, Reader};
use crate::{Result, TraceStore};
use rocksdb::{
    BlockBasedOptions, BottommostLevelCompaction, Cache, ColumnFamily, ColumnFamilyDescriptor, CompactOptions,
    DBCompressionType, DB, Options, ReadOptions, SnapshotWithThreadMode, WriteBatch, WriteOptions,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const WRITE_BUFFERS_MB: usize = 256;

pub struct Rocks {
    db: DB,
    dir: PathBuf,
    cache_mb: usize,
    _cache: Cache,
}

pub fn open(work: &Path, params: &Params) -> Result<Box<dyn TraceStore>> {
    let dir = work.join("rocksdb");
    let existing = params.flag("existing", false);
    if existing && !dir.exists() {
        return Err(format!("no existing store at {}", dir.display()).into());
    }
    let cache_mb = params.num("cache_mb", DEFAULT_CACHE_MB);
    let cache = Cache::new_lru_cache(cache_mb << 20);
    let mut bbo = BlockBasedOptions::default();
    bbo.set_block_cache(&cache);
    bbo.set_bloom_filter(10.0, false);
    let mut cf = Options::default();
    cf.set_compression_type(DBCompressionType::Lz4);
    cf.set_block_based_table_factory(&bbo);
    let mut opts = cf.clone();
    opts.create_if_missing(!existing);
    opts.create_missing_column_families(!existing);
    opts.set_db_write_buffer_size(WRITE_BUFFERS_MB << 20);
    let cfs = KEYSPACES.iter().map(|n| ColumnFamilyDescriptor::new(*n, cf.clone()));
    let db = DB::open_cf_descriptors(&opts, &dir, cfs)?;
    Ok(Box::new(KvStore::new(Rocks { db, dir, cache_mb, _cache: cache })))
}

impl Rocks {
    fn cf(&self, ks: Ks) -> &ColumnFamily {
        self.db.cf_handle(KEYSPACES[ks as usize]).expect("column family")
    }

    fn prop(&self, ks: Ks, name: &str) -> u64 {
        self.db.property_int_value_cf(self.cf(ks), name).ok().flatten().unwrap_or(0)
    }

    /// Wait until no compaction is pending or running (or `max` passes).
    fn wait_idle(&self, max: Duration) {
        let all = [Ks::Meta, Ks::Body, Ks::Sess, Ks::Time, Ks::Kind, Ks::Agent, Ks::Tool, Ks::Status];
        let t0 = Instant::now();
        let mut quiet = 0;
        while quiet < 4 && t0.elapsed() < max {
            let busy: u64 = all
                .iter()
                .map(|&k| {
                    self.prop(k, "rocksdb.compaction-pending")
                        + self.prop(k, "rocksdb.num-running-compactions")
                        + self.prop(k, "rocksdb.mem-table-flush-pending")
                        + self.prop(k, "rocksdb.num-running-flushes")
                })
                .sum();
            quiet = if busy == 0 { quiet + 1 } else { 0 };
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn flush_all(&self) -> Result<()> {
        for n in KEYSPACES {
            self.db.flush_cf(self.db.cf_handle(n).expect("column family"))?;
        }
        Ok(())
    }
}

struct View<'a> {
    r: &'a Rocks,
    snap: SnapshotWithThreadMode<'a, DB>,
}

impl View<'_> {
    fn ro(&self) -> ReadOptions {
        let mut ro = ReadOptions::default();
        ro.set_snapshot(&self.snap);
        ro
    }
}

impl Reader for View<'_> {
    fn get(&self, ks: Ks, key: &[u8], f: &mut dyn FnMut(&[u8])) -> Result<bool> {
        match self.r.db.get_pinned_cf_opt(self.r.cf(ks), key, &self.ro())? {
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
        let mut ro = self.ro();
        ro.set_iterate_upper_bound(to.to_vec());
        let mut it = self.r.db.raw_iterator_cf_opt(self.r.cf(ks), ro);
        it.seek(from);
        while it.valid() {
            let (Some(k), Some(v)) = (it.key(), it.value()) else { break };
            if !f(k, v) {
                break;
            }
            it.next();
        }
        it.status()?;
        Ok(())
    }
}

impl Engine for Rocks {
    fn name(&self) -> String {
        format!("rocksdb 11.8.1 (WriteBatch, sync=true per append, LZ4, bloom 10, cache {} MiB)", self.cache_mb)
    }

    fn read(&self, f: &mut dyn FnMut(&dyn Reader) -> Result<()>) -> Result<()> {
        f(&View { r: self, snap: self.db.snapshot() })
    }

    fn write(&self, ops: Vec<Op>, sync: bool) -> Result<()> {
        let mut wb = WriteBatch::default();
        for op in ops {
            match op {
                Op::Put(ks, k, v) => wb.put_cf(self.cf(ks), k, v),
                Op::Del(ks, k) => wb.delete_cf(self.cf(ks), k),
            }
        }
        let mut wo = WriteOptions::default();
        wo.set_sync(sync);
        self.db.write_opt(wb, &wo)?;
        Ok(())
    }

    fn settle(&self) -> Result<()> {
        self.db.flush_wal(true)?;
        self.flush_all()?;
        self.wait_idle(Duration::from_secs(900));
        Ok(())
    }

    fn compact(&self) -> Result<()> {
        self.flush_all()?;
        let mut co = CompactOptions::default();
        co.set_bottommost_level_compaction(BottommostLevelCompaction::Force);
        for n in KEYSPACES {
            self.db.compact_range_cf_opt(self.db.cf_handle(n).expect("column family"), None::<&[u8]>, None::<&[u8]>, &co);
        }
        self.db.flush_wal(true)?;
        self.wait_idle(Duration::from_secs(600));
        Ok(())
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn details(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        let all = [Ks::Meta, Ks::Body, Ks::Sess, Ks::Time, Ks::Kind, Ks::Agent, Ks::Tool, Ks::Status];
        for k in all {
            out.insert(
                KEYSPACES[k as usize].into(),
                serde_json::json!({"sst_bytes": self.prop(k, "rocksdb.total-sst-files-size"),
                                   "memtable_bytes": self.prop(k, "rocksdb.size-all-mem-tables"),
                                   "keys_estimate": self.prop(k, "rocksdb.estimate-num-keys")}),
            );
        }
        serde_json::Value::Object(out)
    }
}
