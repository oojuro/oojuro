use super::*;
use std::mem::size_of;

#[test]
fn retained_allocation_includes_source_metadata_spare_capacity_without_changing_results() {
    let binding = Binding {
        namespace: [1; 32],
        writer: "writer".into(),
        view: [2; 32],
    };
    let row = Row {
        id: "row".into(),
        key: Some("key".into()),
        text: "quartzneedle exact original".into(),
        valid_at: 1,
        reported_recorded_at: 2,
        kind: Kind::Output,
        tool: Some("tool".into()),
        status: None,
        agent_id: "agent".into(),
        indexed: true,
    };
    let mut index = Index::build(binding.clone(), vec![row]).unwrap();
    let query = Query {
        text: "quartzneedle",
        k: 10,
        from: i64::MIN,
        to: i64::MAX,
        filter: Filter::default(),
        excerpt_bytes: 320,
        max_bytes: 1 << 20,
    };
    let expected = serde_json::to_vec(&index.search(&binding, &query).unwrap()).unwrap();
    let before = index.estimated_heap_bytes();
    assert!(before > index.text_bytes());
    let old_rows = index.rows.capacity();
    index.rows.reserve(100);
    let old_ids = index.ids.capacity();
    index.ids.reserve(100);
    let old_name = index.rows[0].agent_id.capacity();
    index.rows[0].agent_id.reserve(100);
    let growth = (index.rows.capacity() - old_rows) * size_of::<Row>()
        + (index.ids.capacity() - old_ids) * size_of::<String>()
        + index.rows[0].agent_id.capacity()
        - old_name;
    assert_eq!(index.estimated_heap_bytes() - before, growth);
    assert_eq!(
        serde_json::to_vec(&index.search(&binding, &query).unwrap()).unwrap(),
        expected
    );
}

#[test]
#[ignore = "explicit synthetic retained-allocation estimate and admission overhead measurement"]
fn projection_allocation_measurement() {
    for count in [32usize, 4096] {
        let binding = Binding {
            namespace: [1; 32],
            writer: "writer".into(),
            view: [2; 32],
        };
        let rows = (0..count)
            .map(|n| Row {
                id: format!("row-{n}"),
                key: Some(format!("key-{n}")),
                text: (0..32)
                    .map(|term| format!("word{}z ", n * 32 + term))
                    .collect(),
                valid_at: n as i64,
                reported_recorded_at: n as i64,
                kind: Kind::Output,
                tool: Some("fixture-tool".into()),
                status: None,
                agent_id: "fixture-agent".into(),
                indexed: true,
            })
            .collect();
        let index = Index::build(binding, rows).unwrap();
        let mut times = Vec::new();
        let estimated = index.estimated_heap_bytes();
        for _ in 0..101 {
            let start = std::time::Instant::now();
            assert_eq!(
                std::hint::black_box(&index).estimated_heap_bytes(),
                estimated
            );
            times.push(start.elapsed().as_nanos());
        }
        times.sort_unstable();
        println!(
            "PROJECTION_ALLOCATION {}",
            serde_json::json!({"rows": count,
            "source_text_bytes": index.text_bytes(), "estimated_heap_bytes": estimated,
            "estimate_ns_p50": times[50], "estimate_ns_p95": times[95], "estimate_ns_p99": times[99],
            "samples": times.len(), "scope": "synthetic owned capacities plus structural charge; not RSS or hosted capacity"})
        );
    }
}
