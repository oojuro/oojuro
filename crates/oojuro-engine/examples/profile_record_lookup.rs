//! Explicit synthetic local diagnostic of the public Record API.
//! No hosted, concurrent, cold-cache, representative-corpus or SLA claim.
use oojuro_engine::{capture_log::Identity, record::{self, AsOf, Filter, Index, Input, Kind,
    Limits, Query, RecordLog, Scope, Versions, View}};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WRITER: &str = "record-lookup-profile";
const FIRST: u64 = 1009;
const KEY: [u8; 32] = [42; 32]; // Public fixture key, never production confidentiality.
fn checked<T>(value: record::Result<T>) -> Result<T> {
    value.map_err(|error| format!("record: {error:?}").into())
}
fn scope() -> Scope<'static> { Scope { writer: WRITER, owner: 1 } }
fn text(number: usize) -> String {
    let digest = blake3::hash(format!("record-lookup-profile-v1:{number}").as_bytes());
    format!("synthetic database row value needle{number:08} verified original {} λ\n", digest.to_hex())
}
fn input(number: usize) -> Input {
    Input { agent_id: "fixture".into(), parent_id: None, valid_at: number as i64,
        kind: Kind::Explicit, tool: None, status: None, exit: None, key: None,
        text: text(number), attrs: BTreeMap::from([("number".into(), json!(number))]), indexed: true }
}
fn distribution(mut values: Vec<u128>) -> serde_json::Value {
    values.sort_unstable();
    let at = |p: usize| values[(values.len()*p).div_ceil(100).saturating_sub(1)] as f64/1000.;
    json!({"samples": values.len(), "p50_us": at(50), "p95_us": at(95), "p99_us": at(99)})
}
fn inventory(root: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let mut apparent=0; let mut allocated=0;
    for entry in std::fs::read_dir(root)? {
        let entry=entry?; let meta=entry.path().symlink_metadata()?;
        if meta.file_type().is_symlink() { return Err("unexpected fixture link".into()); }
        if meta.is_dir() { let (a,b)=inventory(&entry.path())?;apparent+=a;allocated+=b; }
        else if meta.is_file() { apparent+=meta.len();allocated+=meta.blocks()*512; }
    }
    Ok((apparent,allocated))
}
fn originals(log: &RecordLog, count: usize) -> Result<String> {
    let view=checked(View::open(log,scope(),AsOf::default()))?;
    let limits=Limits { entries:1000,bytes:record::MAX_OUTPUT_BYTES };
    let mut page=checked(view.range(log,Versions::All,Filter::default(),limits))?;
    let mut seen=0;let mut hash=blake3::Hasher::new();
    loop {
        for row in page.records {
            assert_eq!(row.id,FIRST+seen as u64);assert_eq!(row.text,text(seen));
            assert_eq!(row.valid_at,seen as i64);assert_eq!(row.attrs["number"],json!(seen));
            hash.update(&row.id.to_le_bytes());hash.update(row.text.as_bytes());seen+=1;
        }
        let Some(cursor)=page.next else {break};
        page=checked(View::continue_page(log,scope(),&cursor,limits))?;
    }
    assert_eq!(seen,count);Ok(hash.finalize().to_hex().to_string())
}
fn main() -> Result<()> {
    let args=std::env::args().collect::<Vec<_>>();
    if args.len()!=5 { return Err("profile_record_lookup NEW_DIRECTORY COUNT LABEL SEARCH_SAMPLES".into()); }
    let root=Path::new(&args[1]);let count:usize=args[2].parse()?;let search_samples:usize=args[4].parse()?;
    if !(1000..=100000).contains(&count) || !(10..=1000).contains(&search_samples) || root.exists() {
        return Err("new directory, 1k..100k records and 10..1000 search samples required".into());
    }
    std::fs::create_dir(root)?;
    let identity=Identity { writer:WRITER.into(),agent:1,first_id:FIRST,adapter:"public-api-profile-v1".into() };
    let mut log=checked(RecordLog::open(&root.join("store"),&KEY,identity.clone()))?;
    let mut appends=Vec::new();
    for start in (0..count).step_by(1000) {
        let values=(start..(start+1000).min(count)).map(input).collect::<Vec<_>>();
        let began=Instant::now();let ack=checked(log.append(&format!("batch-{start}"),&values))?;
        appends.push(began.elapsed().as_nanos());assert_eq!(ack.ids.len(),values.len());
    }
    let began=Instant::now();let index=checked(Index::rebuild(&log))?;let index_build_ns=began.elapsed().as_nanos();
    let mut get1=Vec::new();let mut get20=Vec::new();
    for iteration in 0..2032 {
        let width=if iteration%2==0 {1} else {20};
        let ids=(0..width).map(|j| (iteration*104729+j*8191)%count).collect::<Vec<_>>();
        let began=Instant::now();let view=checked(View::open(&log,scope(),AsOf::default()))?;
        let rows=ids.iter().map(|id| checked(view.get_id(&log,FIRST+*id as u64,4096)))
            .collect::<Result<Vec<_>>>()?;
        let elapsed=began.elapsed().as_nanos();
        for (row,id) in rows.into_iter().zip(ids) { assert_eq!(row.unwrap().text,text(id)); }
        if iteration>=32 { if width==1 {get1.push(elapsed)} else {get20.push(elapsed)} }
    }
    let mut search=Vec::new();let mut signature=blake3::Hasher::new();
    for iteration in 0..search_samples+1 {
        let number=(iteration*104729)%count;let needle=format!("needle{number:08}");
        let query=Query {scope:scope(),as_of:AsOf::default(),text:&needle,k:20,
            filter:Filter::default(),max_bytes:record::MAX_OUTPUT_BYTES};
        let began=Instant::now();let found=checked(index.search(&log,&query))?;let elapsed=began.elapsed().as_nanos();
        assert_eq!(found.hits.len(),1);let hit=&found.hits[0];
        assert_eq!(hit.record.id,FIRST+number as u64);assert_eq!(hit.record.text,text(number));
        if iteration>0 { search.push(elapsed);signature.update(&hit.record.id.to_le_bytes());signature.update(&hit.bits.to_bits().to_le_bytes()); }
    }
    let original_digest=originals(&log,count)?;drop(index);drop(log);
    let footprint=inventory(&root.join("store"))?;
    let began=Instant::now();let log=checked(RecordLog::open(&root.join("store"),&KEY,identity))?;
    let reopen_ns=began.elapsed().as_nanos();assert_eq!(originals(&log,count)?,original_digest);
    println!("{}",json!({"label":args[3],"records":count,"scope":"Public Record API, synthetic local warm serial diagnostic; no cloud or 1M/concurrency acceptance",
        "get_1":distribution(get1),"get_20":distribution(get20),"search_unique":distribution(search),
        "append_batches_1000":distribution(appends),"index_build_us":index_build_ns as f64/1000.,
        "reopen_us":reopen_ns as f64/1000.,"apparent_bytes":footprint.0,"allocated_bytes":footprint.1,
        "originals_verified":count,"original_digest":original_digest,"ranked_signature":signature.finalize().to_hex().to_string()}));
    Ok(())
}
