"""RocksDB (via the python `rocksdict` wheel) comparator.

key = seq as 8-byte big-endian, value = UTF-8 text bytes. Only the text is stored
(seq is the key; session/turn/kind/valid_at are not stored -- noted in output).
Variants:
  zstd19-dict-16k / zstd19-dict-64k : lz4 above the bottommost level, zstd level 19 at the
      bottommost level with a dictionary (max_dict_bytes 112 KiB, zstd_max_train_bytes
      100 x 112 KiB), block_size 16 / 64 KiB
  lz4-16k : lz4 at every level, no dictionary, block_size 16 KiB
Build = puts (WriteBatch, 1000 per batch) + flush + compact_range(None, None) with
bottommost_level_compaction=force. Size = du -sb of the DB dir after close
(RocksDB info LOG goes to a separate db_log_dir, not counted).
Phase 1 (size-only, no bench lock): build, size, verify every corpus x variant.
Phase 2: per store, one bench-lock hold around 200 multi_gets of 20 random keys.
Run: venv/bin/python rocksdb_run.py [corpus ...]
"""
import glob, os, re, shutil, sys
sys.path.insert(0, os.path.dirname(__file__))
import common as C
import rocksdict as R

VARIANTS = {
    "zstd19-dict-16k": dict(block=16 * 1024, dict=True),
    "zstd19-dict-64k": dict(block=64 * 1024, dict=True),
    "lz4-16k": dict(block=16 * 1024, dict=False),
}
BASE = os.path.join(C.SYS, "rocksdb")


def opts(v):
    o = R.Options(raw_mode=True)
    o.create_if_missing(True)
    b = R.BlockBasedOptions()
    b.set_block_size(v["block"])
    o.set_block_based_table_factory(b)
    o.set_compression_type(R.DBCompressionType.lz4())
    if v["dict"]:
        o.set_bottommost_compression_type(R.DBCompressionType.zstd())
        o.set_bottommost_compression_options(-14, 19, 0, 112 * 1024, True)
        o.set_bottommost_zstd_max_train_bytes(112 * 1024 * 100, True)
    else:
        o.set_bottommost_compression_type(R.DBCompressionType.lz4())
    o.set_keep_log_file_num(1)
    return o


def key(s):
    return s.to_bytes(8, "big")


def rocks_version(logdir):
    for p in glob.glob(os.path.join(logdir, "*LOG*")):
        with open(p, errors="replace") as f:
            for line in f:
                m = re.search(r"RocksDB version: (\S+)", line)
                if m:
                    return m.group(1)
    return "?"


def build(c, rows):
    recs = C.load(c)
    n, raw = len(recs), C.raw_bytes(c)
    exp_all = C.digests(recs)
    for name, v in VARIANTS.items():
        d, logd = os.path.join(BASE, f"{c}-{name}"), os.path.join(BASE, f"{c}-{name}.log")
        shutil.rmtree(d, ignore_errors=True); shutil.rmtree(logd, ignore_errors=True)
        os.makedirs(logd)
        o = opts(v); o.set_db_log_dir(logd)
        db = R.Rdict(d, o)
        wb = R.WriteBatch(raw_mode=True)
        for i, r in enumerate(recs):
            wb.put(key(r["seq"]), r["text"].encode("utf-8"))
            if i % 1000 == 999:
                db.write(wb); wb = R.WriteBatch(raw_mode=True)
        db.write(wb)
        db.flush()
        co = R.CompactOptions()
        co.set_bottommost_level_compaction(R.BottommostLevelCompaction.force())
        db.compact_range(None, None, co)
        levels = [db.property_int_value(f"rocksdb.num-files-at-level{i}") for i in range(7)]
        db.close()
        disk = C.du_sb(d)
        sst = sum(os.path.getsize(p) for p in glob.glob(os.path.join(d, "*.sst")))
        ver = rocks_version(logd)
        db = R.Rdict(d, opts(v), access_type=R.AccessType.read_only())
        vs = C.verify_set(c, n)
        got = {}
        for i in range(0, len(vs), 500):
            chunk = vs[i:i + 500]
            for s, val in zip(chunk, db.get([key(s) for s in chunk])):
                got[s] = None if val is None else val.decode("utf-8")
        db.close()
        ok, bad = C.compare({s: exp_all[s] for s in vs}, got)
        notes = (f"levels={levels} sst_bytes={sst}; value=text only (seq is key; session/turn/kind/valid_at not stored); "
                 f"read = one multi_get of 20 keys, in-process, warm OS cache, default block cache; "
                 f"build untimed (size-only step outside bench lock)")
        if bad:
            notes += f"; mismatches={len(bad)} first={bad[:3]}"
        rows.append(dict(system="rocksdb", version=f"RocksDB {ver} (rocksdict 0.3.29)", variant=name, corpus=c, raw=raw,
                         disk_bytes=disk, how_measured="du -sb db dir after flush + compact_range(bottommost=force) + close; info LOG in separate dir",
                         exact=not bad, verified_records=ok, load_s=None, notes=notes,
                         _ctx=dict(dir=d, logd=logd, variant=name, n=n)))
        print(rows[-1]["corpus"], name, disk, disk / raw, ok, flush=True)


def reader(r):
    x = r["_ctx"]
    db = R.Rdict(x["dir"], opts(VARIANTS[x["variant"]]), access_type=R.AccessType.read_only())
    try:
        return C.timed_reads(lambda b: {s: v for s, v in zip(b, db.get([key(s) for s in b])) if v is not None},
                             C.read_batches(r["corpus"], x["n"]))
    finally:
        db.close()


if __name__ == "__main__":
    corp, resume, build_only = C.flags(sys.argv)
    os.makedirs(BASE, exist_ok=True)
    rows = C.load_pending("rocksdb") if resume else []
    have = {r["corpus"] for r in rows}
    for c in corp:
        # on --resume, corpora already emitted (absent from the staging file) are not rebuilt
        if c not in have and not (resume and rows):
            build(c, rows)
            C.save_pending("rocksdb", rows)
    if not build_only:
        C.finish("rocksdb", rows, reader, lambda r: (shutil.rmtree(r["_ctx"]["dir"], ignore_errors=True),
                                                    shutil.rmtree(r["_ctx"]["logd"], ignore_errors=True)))
