//! Fixed synthetic local scale diagnostic. Not an end-to-end hosted benchmark.
//! Run each arm in a fresh process; count every payload/index/WAL/temporary file.
use oojuro_bench::{Arm, Doc, SearchReq, arms};
use oojuro_engine::{
    capture_log::Identity,
    record::{
        self, AsOf, Filter, Input, Kind, Limits, Query, Record, Scope,
        paged::{DiskLimits, Index, Writer},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Barrier, RwLock},
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const WRITER: &str = "disk-scale-profile-v1";
const KEY: [u8; 32] = [42; 32]; // Synthetic public fixture key.
fn check<T>(r: record::Result<T>) -> Result<T> {
    r.map_err(|e| format!("record {e:?}").into())
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 1,
    }
}
fn identity() -> Identity {
    Identity {
        writer: WRITER.into(),
        agent: 1,
        first_id: 1,
        adapter: "fixed-synthetic-v1".into(),
    }
}
fn policy() -> DiskLimits {
    DiskLimits::new(2_000_000, 4usize << 30, 4usize << 30, 10000).unwrap()
}
fn text(n: usize) -> String {
    format!(
        "synthetic database row value group{:03} needle{n:09} verified original {} λ\n",
        n % 128,
        blake3::hash(format!("disk-scale-profile-v1:{n}").as_bytes()).to_hex()
    )
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
        text: text(n),
        attrs: BTreeMap::from([("number".into(), json!(n))]),
        indexed: true,
    }
}
fn doc(n: usize) -> Doc {
    Doc {
        id: format!("row-{n:09}"),
        key: String::new(),
        text: text(n),
        valid_at: n as i64,
        recorded_at: 0,
        meta: BTreeMap::from([
            ("number".into(), n.to_string()),
            ("source".into(), format!("source-{}", n % 10)),
        ]),
    }
}
fn inventory(root: &Path) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    fn walk(path: &Path, files: &mut Vec<Value>, total: &mut (u64, u64)) -> Result<()> {
        for item in std::fs::read_dir(path)? {
            let item = item?;
            let meta = item.path().symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err("unexpected fixture symlink".into());
            }
            if meta.is_dir() {
                walk(&item.path(), files, total)?
            } else if meta.is_file() {
                total.0 += meta.len();
                total.1 += meta.blocks() * 512;
                files.push(json!({"name":item.path().display().to_string(),"apparent":meta.len(),"allocated":meta.blocks()*512}));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let mut total = (0, 0);
    walk(root, &mut files, &mut total)?;
    Ok(json!({"apparent_bytes":total.0,"allocated_bytes":total.1,"files":files.len()}))
}
fn distribution(mut v: Vec<u128>) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_unstable();
    let at = |p: usize| v[(v.len() * p).div_ceil(100).saturating_sub(1)] as f64 / 1_000_000.;
    json!({"samples":v.len(),"p50_ms":at(50),"p95_ms":at(95),"p99_ms":at(99),"max_ms":v[v.len()-1] as f64/1_000_000.})
}
#[derive(Default)]
struct Samples {
    get1: Vec<u128>,
    get20: Vec<u128>,
    search: Vec<u128>,
    queries: Vec<Value>,
    writes: Vec<u128>,
}
impl Samples {
    fn merge(&mut self, mut other: Self) {
        self.get1.append(&mut other.get1);
        self.get20.append(&mut other.get20);
        self.search.append(&mut other.search);
        self.queries.append(&mut other.queries);
        self.writes.append(&mut other.writes);
    }
}
struct Native {
    writer: Writer,
    index: Index,
}
impl Native {
    fn open(root: &Path) -> Result<Self> {
        let writer = check(Writer::open_disk(
            &root.join("source"),
            &KEY,
            identity(),
            &root.join("directory"),
            None,
            policy(),
        ))?;
        let index = check(Index::open_disk(
            writer.reader().map_err(|e| format!("{e:?}"))?,
            &root.join("lexical"),
        ))?;
        Ok(Self { writer, index })
    }
    fn append(&mut self, start: usize, count: usize) -> Result<record::Commit> {
        let rows = (start..start + count).map(input).collect::<Vec<_>>();
        check(
            self.writer
                .append_indexed(&mut self.index, &format!("rows-{start}-{count}"), &rows),
        )
    }
    fn get(&self, ids: &[usize]) -> Result<()> {
        let r = check(self.writer.reader())?;
        let s = check(r.view(scope(), AsOf::default()))?;
        for n in ids {
            let row = check(r.get_id(&s, *n as u64 + 1, 10000))?.ok_or("missing original")?;
            if row.text != text(*n) {
                return Err("original mismatch".into());
            }
        }
        Ok(())
    }
    fn search(&self, text: &str, cutoff: i64) -> Result<Vec<u64>> {
        let r = check(self.writer.reader())?;
        let found = check(self.index.search(
            r,
            &Query {
                scope: scope(),
                as_of: AsOf {
                    valid_at: cutoff,
                    ..AsOf::default()
                },
                text,
                k: 20,
                filter: Filter::default(),
                max_bytes: record::MAX_OUTPUT_BYTES,
            },
        ))?;
        for hit in &found.hits {
            let n = usize::try_from(
                hit.record.attrs["number"]
                    .as_u64()
                    .ok_or("missing original number")?,
            )?;
            if hit.record.text != crate::text(n) || hit.record.valid_at > cutoff {
                return Err("search original/clock mismatch".into());
            }
        }
        Ok(found.hits.into_iter().map(|h| h.record.id).collect())
    }
    fn verify_client_writes(&self, base: usize, clients: usize) -> Result<()> {
        let r = check(self.writer.reader())?;
        let s = check(r.view(scope(), AsOf::default()))?;
        if s.watermark().records != (base + clients * 5) as u64 {
            return Err("acknowledged write count mismatch".into());
        }
        for n in base..base + clients * 5 {
            let ack = check(r.receipt(&format!("rows-{n}-1")))?.ok_or("missing durable receipt")?;
            let row =
                check(r.get_id(&s, ack.ids[0], 10000))?.ok_or("missing durable client original")?;
            if row.text != text(n) || row.attrs["number"] != json!(n) {
                return Err("durable client original changed".into());
            }
        }
        Ok(())
    }
    fn verify(&self, count: usize) -> Result<String> {
        let r = check(self.writer.reader())?;
        let s = check(r.view(scope(), AsOf::default()))?;
        let mut hash = blake3::Hasher::new();
        for start in (0..count).step_by(1000) {
            let rows = check(r.read_ordinals(
                &s,
                start,
                Limits {
                    entries: 1000,
                    bytes: record::MAX_OUTPUT_BYTES,
                },
            ))?;
            if rows.len() != count.min(start + 1000) - start {
                return Err("native original count mismatch".into());
            }
            for (n, row) in (start..count.min(start + 1000)).zip(rows) {
                verify_row(&row, n)?;
                hash.update(&(n as u64).to_le_bytes());
                hash.update(row.text.as_bytes());
            }
        }
        if s.watermark().records != count as u64 {
            return Err("record count mismatch".into());
        }
        Ok(hash.finalize().to_hex().to_string())
    }
}
fn verify_row(row: &Record, n: usize) -> Result<()> {
    if row.id != n as u64 + 1
        || row.text != text(n)
        || row.valid_at != n as i64
        || row.attrs["number"] != json!(n)
    {
        return Err("original fields mismatch".into());
    }
    Ok(())
}
fn verify_arm(arm: &mut dyn Arm, count: usize) -> Result<String> {
    let mut hash = blake3::Hasher::new();
    for start in (0..count).step_by(1000) {
        let rows = arm.get_range(start as i64, count.min(start + 1000) as i64 - 1, 1000)?;
        if rows.len() != count.min(start + 1000) - start {
            return Err("baseline range count mismatch".into());
        }
        for (n, row) in (start..).zip(rows) {
            if row != doc(n) {
                return Err("baseline full original mismatch".into());
            }
            hash.update(&(n as u64).to_le_bytes());
            hash.update(row.text.as_bytes());
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}
fn query(n: usize, count: usize) -> (String, i64, &'static str) {
    match n % 6 {
        0 => (
            format!("needle{:09}", (n * 104729) % count),
            i64::MAX,
            "unique",
        ),
        1 => ("synthetic".into(), i64::MAX, "common_all"),
        2 => ("group063".into(), i64::MAX, "group"),
        3 => ("database row".into(), i64::MAX, "compound_common"),
        4 => ("absenttermzzzz".into(), i64::MAX, "missing"),
        _ => ("synthetic".into(), count as i64 / 2, "time_half"),
    }
}
fn report(
    arm: &str,
    count: usize,
    clients: usize,
    samples: Samples,
    batches: Vec<u128>,
    reopen: u128,
    originals: String,
    before: Value,
    after: Value,
) -> Value {
    json!({"arm":arm,"records_initial":count,"clients":clients,"fixture":"disk-scale-profile-v1",
        "scope":"synthetic local disk scale diagnostic; native genuine ingest clock, all queries latest known-at; native ranking differs from FTS5 BM25; no cloud/SLA claim",
        "read_1":distribution(samples.get1),"read_20":distribution(samples.get20),"search":distribution(samples.search),
        "search_details":samples.queries,"durable_indexed_write_1":distribution(samples.writes),"durable_indexed_write_1000":distribution(batches),
        "reopen_ms":reopen as f64/1_000_000.,"original_digest":originals,"originals_verified_before_and_after_reopen":count,"client_write_records_verified_after_reopen":clients*5,
        "storage_initial":before,"storage_after_client_writes":after})
}
fn main() -> Result<()> {
    let a = std::env::args().collect::<Vec<_>>();
    if a.len() != 6 {
        return Err("profile_disk NEW_DIR native-disk|sqlite|jsonl|jsonl-zstd RECORDS CLIENTS SEARCHES_PER_CLIENT".into());
    }
    let root = PathBuf::from(&a[1]);
    let name = &a[2];
    let count: usize = a[3].parse()?;
    let clients: usize = a[4].parse()?;
    let searches: usize = a[5].parse()?;
    if root.exists()
        || !(1000..=1_000_000).contains(&count)
        || !(1..=10).contains(&clients)
        || !(1..=100).contains(&searches)
    {
        return Err("fresh directory; 1k..1M rows,1..10 clients,1..100 searches/client".into());
    }
    std::fs::create_dir(&root)?;
    let mut batches = Vec::new();
    let report = if name == "native-disk" {
        let mut native = Native::open(&root)?;
        for start in (0..count).step_by(1000) {
            let began = Instant::now();
            native.append(start, count.min(start + 1000) - start)?;
            batches.push(began.elapsed().as_nanos());
        }
        let original = native.verify(count)?;
        let before = inventory(&root)?;
        drop(native);
        let began = Instant::now();
        let native = Native::open(&root)?;
        let reopen = began.elapsed().as_nanos();
        if native.verify(count)? != original {
            return Err("native reopen digest mismatch".into());
        }
        let state = RwLock::new(native);
        let barrier = Barrier::new(clients);
        let all = std::thread::scope(|threads| {
            let handles = (0..clients)
                .map(|client| {
                    let state = &state;
                    let barrier = &barrier;
                    threads.spawn(move || -> std::result::Result<Samples, String> {
                        let mut samples = Samples::default();
                        barrier.wait();
                        for iteration in 0..100 {
                            let width = if iteration % 2 == 0 { 1 } else { 20 };
                            let ids = (0..width)
                                .map(|j| (client * 65537 + iteration * 104729 + j * 8191) % count)
                                .collect::<Vec<_>>();
                            let began = Instant::now();
                            state.read().unwrap().get(&ids).map_err(|e| e.to_string())?;
                            let ns = began.elapsed().as_nanos();
                            if width == 1 {
                                samples.get1.push(ns)
                            } else {
                                samples.get20.push(ns)
                            }
                        }
                        for iteration in 0..searches {
                            let (q, cutoff, kind) = query(client * searches + iteration, count);
                            let began = Instant::now();
                            let ids = state
                                .read()
                                .unwrap()
                                .search(&q, cutoff)
                                .map_err(|e| e.to_string())?;
                            let ns = began.elapsed().as_nanos();
                            if kind == "unique" && ids.len() != 1 {
                                return Err("unique search failed".into());
                            }
                            if kind == "missing" && !ids.is_empty() {
                                return Err("missing search failed".into());
                            }
                            samples.search.push(ns);
                            samples
                                .queries
                                .push(json!({"kind":kind,"ms":ns as f64/1_000_000.,"hits":ids}));
                        }
                        for iteration in 0..5 {
                            let n = count + client * 5 + iteration;
                            let began = Instant::now();
                            let ack = {
                                let mut native = state.write().unwrap();
                                native.append(n, 1).map_err(|e| e.to_string())?
                            };
                            samples.writes.push(began.elapsed().as_nanos());
                            let native = state.read().unwrap();
                            let reader = native.writer.reader().map_err(|e| format!("{e:?}"))?;
                            let snapshot = reader
                                .view(scope(), AsOf::default())
                                .map_err(|e| format!("{e:?}"))?;
                            let original = reader
                                .get_id(&snapshot, ack.ids[0], 10000)
                                .map_err(|e| format!("{e:?}"))?
                                .ok_or("missing acknowledged original")?;
                            if original.text != text(n) {
                                return Err("durable write original mismatch".into());
                            }
                            let ids = native
                                .search(&format!("needle{n:09}"), i64::MAX)
                                .map_err(|e| e.to_string())?;
                            if ids != vec![ack.ids[0]] {
                                return Err("durable write search visibility failed".into());
                            }
                        }
                        Ok(samples)
                    })
                })
                .collect::<Vec<_>>();
            let mut result = Samples::default();
            for h in handles {
                result.merge(h.join().unwrap().unwrap())
            }
            result
        });
        state.read().unwrap().verify_client_writes(count, clients)?;
        drop(state);
        let recovered = Native::open(&root)?;
        recovered.verify_client_writes(count, clients)?;
        drop(recovered);
        report(
            name,
            count,
            clients,
            all,
            batches,
            reopen,
            original,
            before,
            inventory(&root)?,
        )
    } else {
        let mut arm = arms::builtin(name)?;
        arm.open(&root, &BTreeMap::new())?;
        for start in (0..count).step_by(1000) {
            let docs = (start..count.min(start + 1000))
                .map(doc)
                .collect::<Vec<_>>();
            let began = Instant::now();
            arm.append_batch(&docs)?;
            batches.push(began.elapsed().as_nanos());
        }
        let original = verify_arm(arm.as_mut(), count)?;
        let before = inventory(&root)?;
        arm.close()?;
        drop(arm);
        let began = Instant::now();
        let mut arm = arms::builtin(name)?;
        arm.open(&root, &BTreeMap::new())?;
        let reopen = began.elapsed().as_nanos();
        if verify_arm(arm.as_mut(), count)? != original {
            return Err("baseline reopen digest mismatch".into());
        }
        arm.close()?;
        drop(arm);
        let barrier = Barrier::new(clients);
        let all = std::thread::scope(|threads| {
            let handles = (0..clients)
                .map(|client| {
                    let barrier = &barrier;
                    let root = &root;
                    let name = name.as_str();
                    threads.spawn(move || -> std::result::Result<Samples, String> {
                        let run = || -> Result<Samples> {
                            let mut arm = arms::builtin(name)?;
                            arm.open(root, &BTreeMap::new())?;
                            let mut samples = Samples::default();
                            barrier.wait();
                            for iteration in 0..100 {
                                let width = if iteration % 2 == 0 { 1 } else { 20 };
                                let ns = (0..width)
                                    .map(|j| {
                                        (client * 65537 + iteration * 104729 + j * 8191) % count
                                    })
                                    .collect::<Vec<_>>();
                                let ids =
                                    ns.iter().map(|n| format!("row-{n:09}")).collect::<Vec<_>>();
                                let began = Instant::now();
                                let texts = arm.get(&ids)?;
                                let elapsed = began.elapsed().as_nanos();
                                for (n, t) in ns.into_iter().zip(texts) {
                                    if t != Some(text(n)) {
                                        return Err("baseline read mismatch".into());
                                    }
                                }
                                if width == 1 {
                                    samples.get1.push(elapsed)
                                } else {
                                    samples.get20.push(elapsed)
                                }
                            }
                            for iteration in 0..searches {
                                let (q, cutoff, kind) = query(client * searches + iteration, count);
                                let began = Instant::now();
                                let hits = arm.search(&SearchReq {
                                    text: q,
                                    valid_at: cutoff,
                                    known_at: i64::MAX,
                                    k: 20,
                                    context: BTreeMap::new(),
                                })?;
                                let ns = began.elapsed().as_nanos();
                                if kind == "unique" && hits.len() != 1 {
                                    return Err("baseline unique search failed".into());
                                }
                                if kind == "missing" && !hits.is_empty() {
                                    return Err("baseline missing search failed".into());
                                }
                                samples.search.push(ns);
                                samples.queries.push(
                                    json!({"kind":kind,"ms":ns as f64/1_000_000.,"hits":hits}),
                                );
                            }
                            for iteration in 0..5 {
                                let n = count + client * 5 + iteration;
                                let began = Instant::now();
                                arm.append_batch(&[doc(n)])?;
                                samples.writes.push(began.elapsed().as_nanos());
                                if arm.get(&[format!("row-{n:09}")])? != vec![Some(text(n))] {
                                    return Err("baseline write read visibility failed".into());
                                }
                                let found = arm.search(&SearchReq {
                                    text: format!("needle{n:09}"),
                                    valid_at: i64::MAX,
                                    known_at: i64::MAX,
                                    k: 20,
                                    context: BTreeMap::new(),
                                })?;
                                if found.len() != 1 || found[0].id != format!("row-{n:09}") {
                                    return Err("baseline write search visibility failed".into());
                                }
                            }
                            arm.close()?;
                            Ok(samples)
                        };
                        run().map_err(|e| e.to_string())
                    })
                })
                .collect::<Vec<_>>();
            let mut result = Samples::default();
            for h in handles {
                result.merge(h.join().unwrap().unwrap())
            }
            result
        });
        let mut recovered = arms::builtin(name)?;
        recovered.open(&root, &BTreeMap::new())?;
        verify_arm(recovered.as_mut(), count + clients * 5)?;
        recovered.close()?;
        drop(recovered);
        report(
            name,
            count,
            clients,
            all,
            batches,
            reopen,
            original,
            before,
            inventory(&root)?,
        )
    };
    println!("{report}");
    Ok(())
}
