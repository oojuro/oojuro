#[test]
#[ignore = "bounded local sparse-candidate diagnostic, never a hosted latency claim"]
fn sparse_query_probe() {
    let index = Index::from_record_rows("probe", 1, (1..=100_000).map(|id| (id, 0, "common", true))).unwrap();
    let ids = BTreeSet::from([1]);
    let keys = BTreeMap::from([("first".to_string(), 1)]);
    let view = RecordVisibility { ids: &ids, keys: &keys };
    for (label, text, scoped, expected) in [
        ("literal_miss", "\"absent\"", false, None),
        ("first_direct", "key:1", false, Some(1)),
        ("current_first_direct", "key:first", true, Some(1)),
    ] {
        let q = Query { writer: "probe", agent: 1, text, from: 0, to: 1, k: 1 };
        let run = || {
            let q = std::hint::black_box(&q);
            if scoped { index.search_record_view(q, &view).unwrap() } else { index.search(q).unwrap() }
        };
        for _ in 0..100 { std::hint::black_box(run()); }
        let mut ns = Vec::with_capacity(2000);
        let mut signature = None;
        for _ in 0..2000 {
            let start = std::time::Instant::now();
            let hits = run();
            ns.push(start.elapsed().as_nanos() as u64);
            assert_eq!(hits.first().map(|h| h.id), expected);
            assert_eq!(hits.len(), usize::from(expected.is_some()));
            let got: Vec<_> = hits.iter().map(|h| (h.id, h.bits.to_bits(), h.time, h.continued)).collect();
            if let Some(old) = &signature { assert_eq!(&got, old); } else { signature = Some(got); }
            std::hint::black_box(hits);
        }
        let mut sorted = ns.clone(); sorted.sort_unstable();
        println!("{}", serde_json::json!({"diagnostic":"sparse-query", "rows":100000,"label":label,"samples":ns,"p50_ns":sorted[999],"p95_ns":sorted[1899],"p99_ns":sorted[1979],"signature":signature,"scope":"component only; 2000 samples after100 warmups; no ingest/I/O/provider/e2e claim"}));
    }
}
