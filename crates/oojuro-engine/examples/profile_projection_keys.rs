//! Synthetic public projection API comparison; no Store, remote authority or provider.
use oojuro_engine::record::{
    self, Filter, Kind, Status,
    projection::{Binding, Hit, Index, Query, Row},
};
use serde_json::json;
use std::{hint::black_box, time::Instant};

fn checked<T>(value: record::Result<T>) -> Result<T, Box<dyn std::error::Error>> {
    value.map_err(|error| format!("projection: {error:?}").into())
}

fn row(number: usize, changed: bool) -> Row {
    let key = match number {
        3 => "key:fixed-special".to_owned(),
        4 => "fixed-special".to_owned(),
        5 => "λ-key".to_owned(),
        _ => format!("memory-{number:064x}"),
    };
    let mut text = format!("Original λ\r\nrecord {number} battery electrode needle{number:08} ");
    if number % 97 == 0 { text.push_str("lookup_needle "); }
    text.push_str("exact original\0終");
    if changed && number == 1 { text.push_str(" refreshed original"); }
    Row {
        id: format!("{number:064x}"), key: Some(key), text,
        valid_at: (number % 17) as i64, reported_recorded_at: -99,
        kind: if number % 7 == 0 { Kind::ToolResult } else { Kind::Output },
        tool: (number % 5 == 0).then(|| "fixture-tool".into()),
        status: Some(if number % 2 == 0 { Status::Ok } else { Status::Error }),
        agent_id: format!("agent{}", number % 3), indexed: number % 19 != 0,
    }
}
struct Case {
    name: &'static str, text: String, filter: Filter, from: i64, to: i64,
}
fn cases(count: usize) -> Vec<Case> {
    let middle = (count / 2..count).find(|n| n % 19 != 0).unwrap();
    let last = (0..count).rev().find(|n| n % 19 != 0).unwrap();
    let mut cases: Vec<_> = [
        ("lexical_unique", format!("needle{middle:08}")),
        ("lexical_common", "battery".into()),
        ("lexical_missing", "unseenword999".into()),
        ("bare_key_first", row(1, false).key.unwrap()),
        ("explicit_key_last", format!("key:{}", row(last, false).key.unwrap())),
        ("nested_key_prefix", "key:key:fixed-special".into()),
        ("missing_explicit_key", "key:missing".into()),
        ("stored_only_key", format!("key:{}", row(0, false).key.unwrap())),
        ("unicode_key", "key:λ-key".into()),
        ("literal_phrase", "\"battery electrode\"".into()),
        ("identifier_anchor", "lookup_needle battery".into()),
    ].into_iter().map(|(name, text)| Case {
        name, text, filter: Filter::default(), from: i64::MIN, to: i64::MAX,
    }).collect();
    let mut filter = Filter::default();
    filter.kind = Some(Kind::ToolResult);
    filter.tool = Some("fixture-tool".into());
    filter.status = Some(Status::Ok);
    filter.agent_id = Some("agent1".into());
    cases.push(Case { name: "filtered_common", text: "battery".into(), filter, from: 4, to: 12 });
    cases
}
fn query(case: &Case) -> Query<'_> {
    Query { text: &case.text, k: 20, from: case.from, to: case.to,
        filter: case.filter.clone(), excerpt_bytes: 80, max_bytes: 1 << 20 }
}
fn signature(hits: &[Hit], changed: bool) -> String {
    let mut checks = Vec::new();
    for hit in hits {
        let number = usize::from_str_radix(&hit.id, 16).unwrap();
        let original = row(number, changed);
        assert!(original.text.is_char_boundary(hit.start) && original.text.is_char_boundary(hit.end));
        assert_eq!(hit.excerpt, original.text[hit.start..hit.end]);
        assert_eq!(hit.valid_at, original.valid_at);
        assert_eq!(hit.reported_recorded_at, original.reported_recorded_at);
        checks.push(json!([hit.id, hit.bits.to_bits(), hit.continued, hit.valid_at,
            hit.reported_recorded_at, hit.start, hit.end, hit.excerpt]));
    }
    blake3::hash(&serde_json::to_vec(&checks).unwrap()).to_hex().to_string()
}
fn distribution(mut nanos: Vec<u128>) -> serde_json::Value {
    nanos.sort_unstable();
    let count = nanos.len();
    json!({"count":count,"p50_us":nanos[count / 2] as f64 / 1000.,
        "p95_us":nanos[(count * 95).div_ceil(100) - 1] as f64 / 1000.,
        "min_us":nanos[0] as f64 / 1000.,"max_us":nanos[count - 1] as f64 / 1000.,
        "sorted_samples_ns":nanos})
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 { return Err("profile_projection_keys COUNT LABEL".into()); }
    let count: usize = args[1].parse()?;
    if ![1000, 10000, 100000].contains(&count) { return Err("bounded 1k/10k/100k required".into()); }
    let binding = Binding { namespace: [7;32], writer: "projection-synthetic".into(), view: [8;32] };
    let rows: Vec<_> = (0..count).map(|n| row(n, false)).collect();
    let expected_text_bytes: usize = rows.iter().map(|r| r.text.len()).sum();
    let began = Instant::now();
    let index = checked(Index::build(binding.clone(), rows))?;
    let build_ns = began.elapsed().as_nanos();
    assert_eq!(index.row_count(), count);
    assert_eq!(index.text_bytes(), expected_text_bytes);
    let heap = index.estimated_heap_bytes();
    let mut estimated = Vec::new();
    for _ in 0..21 {
        let began = Instant::now();
        assert_eq!(black_box(&index).estimated_heap_bytes(), heap);
        estimated.push(began.elapsed().as_nanos());
    }
    let mut results = Vec::new();
    for case in cases(count) {
        let q = query(&case);
        let expected = signature(&checked(index.search(&binding, &q))?, false);
        let mut times = Vec::new();
        let mut result_count = 0;
        for _ in 0..20 {
            let began = Instant::now();
            let hits = checked(black_box(&index).search(black_box(&binding), black_box(&q)))?;
            times.push(began.elapsed().as_nanos());
            assert_eq!(signature(&hits, false), expected);
            result_count = hits.len();
        }
        results.push(json!({"case":case.name,"hits":result_count,"signature":expected,"timing":distribution(times)}));
    }
    assert_eq!(index.estimated_heap_bytes(), heap);
    let mut next = binding.clone(); next.view[0] ^= 1;
    let next_count = (count + 32).min(100000);
    let changed = count == 100000;
    let new_rows: Vec<_> = (0..next_count).rev().map(|n| row(n, changed)).collect();
    let began = Instant::now();
    let refreshed = checked(index.refresh(next.clone(), new_rows))?;
    let refresh_ns = began.elapsed().as_nanos();
    assert!(refreshed.search(&binding, &query(&cases(count).remove(0))).is_err());
    assert_eq!(refreshed.row_count(), next_count);
    let refreshed_signatures: Vec<_> = cases(count).iter().map(|case| {
        let hits = refreshed.search(&next, &query(case)).unwrap();
        json!({"case":case.name,"signature":signature(&hits, changed),"hits":hits.len()})
    }).collect();
    println!("{}",json!({"label":args[2],"rows":count,"scope":"pure local cached public projection API; no remote freshness proof",
        "build_ns_one_observation":build_ns,"refresh_ns_one_observation":refresh_ns,
        "refresh_mode":if changed {"replacement+reorder"} else {"append32+reorder"},
        "text_bytes":expected_text_bytes,"estimated_retained_heap_bytes":heap,
        "estimated_heap_after_queries":heap,"heap_estimate":distribution(estimated),
        "refreshed_rows":next_count,"refreshed_estimated_heap_bytes":refreshed.estimated_heap_bytes(),
        "queries":results,"refreshed_signatures":refreshed_signatures}));
    Ok(())
}
