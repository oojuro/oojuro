//! Synthetic local encrypted projection diagnostics. Source callbacks regenerate
//! exact fixture rows; this is not end-to-end .ojo, network, authority or SLA data.
use oojuro_engine::record::{
    self, Filter, Kind, Status,
    projection::{
        Binding, Hit, Query, Row,
        disk::{Appender, Builder, Compression, Index, Limits},
    },
};
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Barrier},
    time::Instant,
};

fn checked<T>(value: record::Result<T>) -> Result<T, Box<dyn std::error::Error>> {
    value.map_err(|error| format!("projection: {error:?}").into())
}
fn binding() -> Binding {
    Binding {
        namespace: [7; 32],
        writer: "projection-scale-synthetic".into(),
        view: [8; 32],
    }
}
fn row(number: usize) -> Row {
    let mut text = format!("Original λ\r\nrecord {number} battery electrode needle{number:08} ");
    if number % 97 == 0 {
        text.push_str("lookup_needle ");
    }
    text.push_str("exact original\0終");
    Row {
        id: format!("{number:064x}"),
        key: Some(format!("memory-{number:064x}")),
        text,
        valid_at: (number % 17) as i64,
        reported_recorded_at: -99,
        kind: if number % 7 == 0 {
            Kind::ToolResult
        } else {
            Kind::Output
        },
        tool: (number % 5 == 0).then(|| "fixture-tool".into()),
        status: Some(if number % 2 == 0 {
            Status::Ok
        } else {
            Status::Error
        }),
        agent_id: format!("agent{}", number % 3),
        indexed: number % 19 != 0,
    }
}
#[derive(Clone)]
struct Case {
    name: &'static str,
    text: String,
    filter: Filter,
    from: i64,
    to: i64,
}
fn cases(count: usize) -> Vec<Case> {
    let middle = (count / 2..count).find(|n| n % 19 != 0).unwrap();
    let mut cases: Vec<_> = [
        ("lexical_unique", format!("needle{middle:08}")),
        ("lexical_common", "battery".into()),
        ("lexical_missing", "unseenword999".into()),
        ("explicit_key", format!("key:{}", row(middle).key.unwrap())),
        ("identifier_anchor", "lookup_needle battery".into()),
    ]
    .into_iter()
    .map(|(name, text)| Case {
        name,
        text,
        filter: Filter::default(),
        from: i64::MIN,
        to: i64::MAX,
    })
    .collect();
    cases.push(Case {
        name: "filtered_common",
        text: "battery".into(),
        filter: Filter {
            kind: Some(Kind::ToolResult),
            tool: Some("fixture-tool".into()),
            status: Some(Status::Ok),
            agent_id: Some("agent1".into()),
        },
        from: 4,
        to: 12,
    });
    if count <= 10000 {
        cases.push(Case {
            name: "literal_phrase",
            text: "\"battery electrode\"".into(),
            filter: Filter::default(),
            from: i64::MIN,
            to: i64::MAX,
        });
    }
    cases
}
fn query(case: &Case) -> Query<'_> {
    Query {
        text: &case.text,
        k: 20,
        from: case.from,
        to: case.to,
        filter: case.filter.clone(),
        excerpt_bytes: 80,
        max_bytes: 1 << 20,
    }
}
fn search(index: &Index, case: &Case) -> record::Result<Vec<Hit>> {
    index.search(&binding(), &query(case), |id| {
        let number = usize::from_str_radix(id, 16).map_err(|_| record::Error::Invalid)?;
        Ok(row(number))
    })
}
fn signature(hits: &[Hit]) -> String {
    let checks: Vec<_> = hits
        .iter()
        .map(|hit| {
            let number = usize::from_str_radix(&hit.id, 16).unwrap();
            let original = row(number);
            assert_eq!(hit.excerpt, original.text[hit.start..hit.end]);
            assert_eq!(hit.valid_at, original.valid_at);
            assert_eq!(hit.reported_recorded_at, original.reported_recorded_at);
            json!([
                hit.id,
                hit.bits.to_bits(),
                hit.continued,
                hit.valid_at,
                hit.reported_recorded_at,
                hit.start,
                hit.end,
                hit.excerpt
            ])
        })
        .collect();
    blake3::hash(&serde_json::to_vec(&checks).unwrap())
        .to_hex()
        .to_string()
}
fn distribution(mut times: Vec<u128>) -> serde_json::Value {
    times.sort_unstable();
    let n = times.len();
    json!({"count":n,"p50_ms":times[(n*50).div_ceil(100)-1] as f64/1e6,
        "p95_ms":times[(n*95).div_ceil(100)-1] as f64/1e6,
        "min_ms":times[0] as f64/1e6,"max_ms":times[n-1] as f64/1e6,"samples_ns":times})
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 7 {
        return Err(
            "profile_disk_projection build|query DIRECTORY ROWS raw|zstd CLIENTS ROUNDS".into(),
        );
    }
    let count: usize = args[3].parse()?;
    let clients: usize = args[5].parse()?;
    let rounds: usize = args[6].parse()?;
    if !(100..=1_000_000).contains(&count)
        || !(1..=32).contains(&clients)
        || !(1..=100).contains(&rounds)
    {
        return Err("bounds: rows100..1M clients1..32 rounds1..100".into());
    }
    let compression = match args[4].as_str() {
        "raw" => Compression::Raw,
        "zstd" => Compression::Zstd,
        _ => return Err("raw|zstd".into()),
    };
    let root = Path::new(&args[2]);
    if args[1] == "build" {
        if root.exists() {
            return Err("build requires a fresh nonexistent directory".into());
        }
        let began = Instant::now();
        let mut builder = checked(Builder::create(
            root,
            &[9; 32],
            binding(),
            Limits {
                compression,
                ..Limits::default()
            },
        ))?;
        for i in 0..count {
            checked(builder.push(row(i)))?;
        }
        let index = checked(builder.finish())?;
        println!(
            "{}",
            json!({"phase":"build","rows":count,"compression":args[4],"ns":began.elapsed().as_nanos(),
            "text_bytes":index.text_bytes(),"index_bytes":index.index_bytes(),"allocated_index_bytes":index.allocated_index_bytes(),
            "estimated_heap_bytes":index.estimated_heap_bytes(),"build_metrics":index.build_metrics(),
            "scope":"synthetic encrypted derived projection; deterministic fixture originals; no authority or end-to-end file latency"})
        );
    } else if args[1] == "append" {
        let began = Instant::now();
        let previous = checked(Index::open(root, &[9; 32], &binding()))?;
        let open_ns = began.elapsed().as_nanos();
        for case in cases(count) {
            checked(search(&previous, &case))?;
        }
        let mut next = binding();
        next.view[0] ^= 1;
        let began = Instant::now();
        let mut appender = checked(Appender::extend_validated_prefix(
            &previous,
            next.clone(),
            Limits {
                compression,
                ..Limits::default()
            },
        ))?;
        for i in count..count + 32 {
            checked(appender.push(row(i)))?;
        }
        let index = checked(appender.finish())?;
        let append_ns = began.elapsed().as_nanos();
        assert_eq!(index.row_count(), (count + 32) as u64);
        let mut signatures = Vec::new();
        for case in cases(count + 32) {
            let hits = checked(index.search(&next, &query(&case), |id| {
                Ok(row(usize::from_str_radix(id, 16).unwrap()))
            }))?;
            signatures.push(json!({"case":case.name,"signature":signature(&hits)}));
        }
        println!(
            "{}",
            json!({"phase":"append","rows":count,"added":32,"compression":args[4],"open_ns":open_ns,"append_ns":append_ns,"index_bytes":index.index_bytes(),"allocated_index_bytes":index.allocated_index_bytes(),"estimated_heap_bytes":index.estimated_heap_bytes(),"build_metrics":index.build_metrics(),"signatures":signatures,"scope":"derived refresh only; caller-asserted unchanged prefix; existing index warmed before timing; source append and authority/provider checks excluded"})
        );
    } else if args[1] == "query" {
        let began = Instant::now();
        let index = Arc::new(checked(Index::open(root, &[9; 32], &binding()))?);
        let open_ns = began.elapsed().as_nanos();
        assert_eq!(index.row_count(), count as u64);
        let cases = cases(count);
        let mut initial = Vec::new();
        let mut signatures = Vec::new();
        for case in &cases {
            let began = Instant::now();
            let hits = checked(search(&index, case))?;
            initial.push(began.elapsed().as_nanos());
            signatures.push(signature(&hits));
        }
        let barrier = Arc::new(Barrier::new(clients + 1));
        let began = Instant::now();
        let workers: Vec<_> = (0..clients)
            .map(|client| {
                let (index, cases, barrier, signatures) = (
                    index.clone(),
                    cases.clone(),
                    barrier.clone(),
                    signatures.clone(),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut times = vec![Vec::new(); cases.len()];
                    for round in 0..rounds {
                        for offset in 0..cases.len() {
                            let i = (offset + client + round) % cases.len();
                            let began = Instant::now();
                            let hits = search(&index, &cases[i]).unwrap();
                            times[i].push(began.elapsed().as_nanos());
                            assert_eq!(signature(&hits), signatures[i]);
                        }
                    }
                    times
                })
            })
            .collect();
        barrier.wait();
        let mut times = vec![Vec::new(); cases.len()];
        for worker in workers {
            for (i, samples) in worker.join().unwrap().into_iter().enumerate() {
                times[i].extend(samples);
            }
        }
        let elapsed_ns = began.elapsed().as_nanos();
        let results: Vec<_> = cases.iter().enumerate().map(|(i, case)| json!({"case":case.name,"initial_ns":initial[i],"signature":signatures[i],"timing":distribution(std::mem::take(&mut times[i]))})).collect();
        println!(
            "{}",
            json!({"phase":"query","rows":count,"compression":args[4],"clients":clients,"rounds":rounds,"open_ns":open_ns,"elapsed_ns":elapsed_ns,
            "index_bytes":index.index_bytes(),"allocated_index_bytes":index.allocated_index_bytes(),"estimated_heap_bytes":index.estimated_heap_bytes(),"queries":results,
            "scope":"local encrypted projection with regenerated exact source callbacks; initial queries warm pages; no end-to-end .ojo/provider freshness acceptance"})
        );
    } else {
        return Err("build|query".into());
    }
    Ok(())
}
