//! Fixed synthetic durable-source/cache phase probe, including total cache loss.
//! This does not change the default durable locator/index contract.
use oojuro_engine::{
    capture_log::Identity,
    record::{
        self, AsOf, Filter, Input, Kind, Limits, Query, Scope,
        paged::{Append, CacheDurability, DiskLimits, Index, Writer},
    },
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Instant};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const KEY: [u8; 32] = [42; 32];
const WRITER: &str = "disk-write-phase-profile-v1";
fn checked<T>(value: record::Result<T>) -> Result<T> {
    value.map_err(|e| format!("record {e:?}").into())
}
fn identity() -> Identity {
    Identity {
        writer: WRITER.into(),
        agent: 1,
        first_id: 1,
        adapter: "fixed-write-phase-v1".into(),
    }
}
fn policy() -> DiskLimits {
    DiskLimits::new(2_000_000, 4usize << 30, 4usize << 30, 10000).unwrap()
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 1,
    }
}
fn input(n: usize) -> Input {
    Input {
        agent_id: format!("source-{}", n % 10),
        parent_id: None,
        valid_at: n as i64,
        kind: Kind::Explicit,
        tool: None,
        status: None,
        exit: None,
        key: None,
        indexed: true,
        text: format!(
            "synthetic database row needle{n:09} {} λ\n",
            blake3::hash(format!("write-phase-v1:{n}").as_bytes()).to_hex()
        ),
        attrs: BTreeMap::from([("number".into(), json!(n))]),
    }
}
fn open(root: &Path, minimum: Option<&record::Watermark>, mode: CacheDurability) -> Result<Writer> {
    checked(Writer::open_disk_with_cache_policy(
        &root.join("source"),
        &KEY,
        identity(),
        &root.join("directory"),
        minimum,
        policy(),
        mode,
    ))
}
fn digest(writer: &Writer, count: usize) -> Result<String> {
    let reader = checked(writer.reader())?;
    let snapshot = checked(reader.view(scope(), AsOf::default()))?;
    if snapshot.watermark().records != count as u64 {
        return Err("source count mismatch".into());
    }
    let mut hash = blake3::Hasher::new();
    for start in (0..count).step_by(1000) {
        let rows = checked(reader.read_ordinals(
            &snapshot,
            start,
            Limits {
                entries: 1000,
                bytes: record::MAX_OUTPUT_BYTES,
            },
        ))?;
        if rows.len() != count.min(start + 1000) - start {
            return Err("original window count mismatch".into());
        }
        for (n, row) in (start..).zip(rows) {
            let expected = input(n);
            if row.id != n as u64 + 1
                || row.text != expected.text
                || row.attrs != expected.attrs
                || row.agent_id != expected.agent_id
                || row.valid_at != expected.valid_at
                || row.kind != expected.kind
                || !row.indexed
                || row.recorded_at <= 0
                || row.parent_id.is_some()
                || row.key.is_some()
                || row.tool.is_some()
                || row.status.is_some()
                || row.exit.is_some()
                || row.schema != 1
                || row.writer != WRITER
                || row.adapter != "fixed-write-phase-v1"
            {
                return Err("complete original mismatch".into());
            }
            hash.update(&serde_json::to_vec(&row)?);
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}
fn footprint(path: &Path) -> Result<serde_json::Value> {
    use std::os::unix::fs::MetadataExt;
    fn walk(path: &Path) -> Result<(u64, u64, u64)> {
        let mut result = (0, 0, 0);
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            let meta = path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err("unexpected fixture link".into());
            }
            let value = if meta.is_dir() {
                walk(&path)?
            } else {
                (meta.len(), meta.blocks() * 512, 1)
            };
            result.0 += value.0;
            result.1 += value.1;
            result.2 += value.2;
        }
        Ok(result)
    }
    let (apparent, allocated, files) = walk(path)?;
    Ok(json!({"apparent_bytes":apparent,"allocated_bytes":allocated,"files":files}))
}
fn flush_cache(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        let meta = path.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err("unexpected fixture link".into());
        }
        if meta.is_dir() {
            flush_cache(&path)?
        } else {
            std::fs::File::open(path)?.sync_all()?
        }
    }
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if !(5..=6).contains(&args.len()) {
        return Err(
            "profile_record_write NEW_DIRECTORY BASE_ROWS GROUP_SIZE GROUPS [durable|rebuildable]"
                .into(),
        );
    }
    let root = Path::new(&args[1]);
    let base: usize = args[2].parse()?;
    let size: usize = args[3].parse()?;
    let groups: usize = args[4].parse()?;
    if root.exists()
        || !(1000..=1_000_000).contains(&base)
        || !(1..=10).contains(&size)
        || !(1..=100).contains(&groups)
    {
        return Err("new directory; 1k..1M rows, 1..10 requests/group, 1..100 groups".into());
    }
    let mode = match args.get(5).map(String::as_str).unwrap_or("durable") {
        "durable" => CacheDurability::Durable,
        "rebuildable" => CacheDurability::Rebuildable,
        _ => return Err("invalid cache policy".into()),
    };
    std::fs::create_dir(root)?;
    let mut writer = open(root, None, mode)?;
    let mut index = checked(Index::open_disk(
        writer.reader().map_err(|e| format!("{e:?}"))?,
        &root.join("lexical"),
    ))?;
    for start in (0..base).step_by(1000) {
        if start % 100000 == 0 {
            eprintln!("synthetic base progress {start}/{base}");
        }
        let rows = (start..base.min(start + 1000))
            .map(input)
            .collect::<Vec<_>>();
        checked(writer.append_indexed(&mut index, &format!("base-{start}"), &rows))?;
    }
    let old = checked(writer.reader())?
        .view(scope(), AsOf::default())
        .map_err(|e| format!("{e:?}"))?;
    let mut receipts = Vec::new();
    let mut profiles = Vec::new();
    for group in 0..groups {
        let numbers = (base + group * size..base + (group + 1) * size).collect::<Vec<_>>();
        let names = numbers
            .iter()
            .map(|n| format!("request-{n}"))
            .collect::<Vec<_>>();
        let rows = numbers.iter().map(|n| vec![input(*n)]).collect::<Vec<_>>();
        let requests = names
            .iter()
            .zip(&rows)
            .map(|(request, inputs)| Append { request, inputs })
            .collect::<Vec<_>>();
        let (acks, work) = checked(writer.append_group_indexed_profiled(&mut index, &requests))?;
        for ((name, n), receipt) in names.into_iter().zip(numbers).zip(acks) {
            receipts.push((name, n, receipt));
        }
        profiles.push(work);
    }
    let count = base + size * groups;
    let minimum = checked(writer.reader())?
        .watermark()
        .map_err(|e| format!("{e:?}"))?;
    let original_digest = digest(&writer, count)?;
    let dirty_footprint = footprint(root)?;
    let began = Instant::now();
    flush_cache(&root.join("directory"))?;
    flush_cache(&root.join("lexical"))?;
    let deferred_cache_checkpoint_ns = began.elapsed().as_nanos();
    let flushed_footprint = footprint(root)?;
    drop(index);
    drop(writer);
    // Only these newly generated, dedicated disposable cache directories are removed.
    std::fs::remove_dir_all(root.join("directory"))?;
    std::fs::remove_dir_all(root.join("lexical"))?;
    let began = Instant::now();
    let mut writer = open(root, Some(&minimum), mode)?;
    let mut index = checked(Index::open_disk(
        checked(writer.reader())?,
        &root.join("lexical"),
    ))?;
    let recovery_ns = began.elapsed().as_nanos();
    if digest(&writer, count)? != original_digest {
        return Err("cache-loss original/clock digest mismatch".into());
    }
    let reader = checked(writer.reader())?;
    if checked(reader.get_id(&old, (base + 1) as u64, 10000))?.is_some() {
        return Err("old prefix adopted future record".into());
    }
    let future = format!("needle{base:09}");
    let future_query = Query {
        scope: scope(),
        as_of: AsOf::default(),
        text: &future,
        k: 20,
        filter: Filter::default(),
        max_bytes: record::MAX_OUTPUT_BYTES,
    };
    if !checked(index.search_view(reader, &old, &future_query))?
        .hits
        .is_empty()
    {
        return Err("rebuilt index adopted future rows into old prefix".into());
    }
    for (name, _, receipt) in &receipts {
        if checked(reader.receipt(name))?.as_ref() != Some(receipt) {
            return Err("cache-loss receipt mismatch".into());
        }
    }
    for chunk in receipts.chunks(50) {
        let text = chunk
            .iter()
            .map(|(_, n, _)| format!("needle{n:09}"))
            .collect::<Vec<_>>()
            .join(" ");
        let found = checked(index.search(
            reader,
            &Query {
                scope: scope(),
                as_of: AsOf::default(),
                text: &text,
                k: chunk.len(),
                filter: Filter::default(),
                max_bytes: record::MAX_OUTPUT_BYTES,
            },
        ))?;
        let expected = chunk
            .iter()
            .map(|(_, _, receipt)| (receipt.ids[0], receipt.recorded_at[0]))
            .collect::<BTreeMap<_, _>>();
        if found.hits.len() != expected.len() {
            return Err("cache-loss search count mismatch".into());
        }
        for hit in found.hits {
            if expected.get(&hit.record.id) != Some(&hit.record.recorded_at) {
                return Err("cache-loss search visibility/clock mismatch".into());
            }
        }
    }
    for (name, n, receipt) in &receipts {
        if checked(writer.append_indexed(&mut index, name, &[input(*n)]))? != *receipt {
            return Err("cache-loss exact retry mismatch".into());
        }
    }
    let recovered_dirty_footprint = footprint(root)?;
    let began = Instant::now();
    flush_cache(&root.join("directory"))?;
    flush_cache(&root.join("lexical"))?;
    let recovery_checkpoint_ns = began.elapsed().as_nanos();
    let recovered_flushed_footprint = footprint(root)?;
    println!(
        "{}",
        json!({"scope":"synthetic one-writer phase probe; source always sync=true, cache mode explicit; not an SLA",
        "cache_durability":mode,"base_rows":base,"group_size":size,"groups":groups,"profiles":profiles,"acknowledged_rows":size*groups,
        "complete_originals_after_total_cache_loss":count,"complete_original_clock_digest":original_digest,
        "cache_loss_rebuild_ns":recovery_ns,"minimum_watermark_and_old_prefix_verified":true,
        "dirty_footprint":dirty_footprint,"flushed_footprint":flushed_footprint,"deferred_cache_checkpoint_ns":deferred_cache_checkpoint_ns,
        "recovered_dirty_footprint":recovered_dirty_footprint,"recovered_flushed_footprint":recovered_flushed_footprint,"recovery_checkpoint_ns":recovery_checkpoint_ns})
    );
    Ok(())
}
