
#[test]
#[ignore]
fn record_append_phase_probe() {
    use std::{sync::Mutex, time::Instant};
    let case = std::env::var("OOJURO_TAG_PROBE_CASE").unwrap();
    let (rounds, batch, bytes) = match case.as_str() {
        "small" => (64, 8, 1024),
        "large" => (6, 1000, 16384),
        "escaped" => (4, 1, 4 << 20),
        _ => panic!("unknown case"),
    };
    let f = Fixture::new();
    let mut log = f.clock(100);
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    log.hook = Some(Box::new(move |phase| {
        observed.lock().unwrap().push((phase, Instant::now()));
    }));
    let inputs: Vec<_> = (0..batch)
        .map(|i| {
            let text = if case == "escaped" {
                "\0".repeat(bytes)
            } else {
                format!("row{i} battery λ🦀 ").repeat(bytes / 8)
            };
            let mut r = input(&text, None, i as i64);
            let mut end = bytes;
            while !r.text.is_char_boundary(end) {
                end -= 1;
            }
            r.text.truncate(end);
            r.indexed = false;
            r
        })
        .collect();
    let mut samples = vec![];
    for n in 0..rounds {
        events.lock().unwrap().clear();
        let start = Instant::now();
        let ack = log.append(&format!("probe-{n}"), &inputs).unwrap();
        let done = Instant::now();
        let observed = events.lock().unwrap();
        let [(authority::Phase::BeforeStore, prepared),
            (authority::Phase::AfterStore, stored),
            (authority::Phase::BeforeAck, replayed)] = observed.as_slice() else { panic!("missing phase") };
        assert_eq!(ack.ids.len(), batch);
        samples.push(serde_json::json!({
            "prepare_us":prepared.duration_since(start).as_secs_f64()*1e6,
            "store_us":stored.duration_since(*prepared).as_secs_f64()*1e6,
            "replay_us":replayed.duration_since(*stored).as_secs_f64()*1e6,
            "total_us":done.duration_since(start).as_secs_f64()*1e6
        }));
    }
    let peak_rss = || {
        std::fs::read_to_string("/proc/self/status").unwrap().lines().find_map(|line| line.strip_prefix("VmHWM:")).unwrap().split_whitespace().next().unwrap().parse::<u64>().unwrap() * 1024
    };
    let append_peak_rss_bytes = peak_rss();
    let exact = |records: &[Assigned]| {
        assert_eq!(records.len(), rounds * batch);
        for (i, row) in records.iter().enumerate() {
            assert_eq!(row.input, inputs[i % batch]);
        }
        let mut hash = blake3::Hasher::new();
        serde_json::to_writer(&mut hash, records).unwrap();
        hash.finalize().to_hex().to_string()
    };
    let signature = exact(&log.records);
    drop(log);
    let reopened = f.clock(100);
    assert_eq!(signature, exact(&reopened.records));
    println!("{}", serde_json::json!({"diagnostic":"record-tag-append", "case":case,"rounds":rounds,"batch":batch,"max_source_text_bytes_per_row":bytes,"source_text_bytes_per_batch":inputs.iter().map(|r|r.text.len()).sum::<usize>(),"samples":samples,"exact_reopen":true,"all_assigned_signature":signature,"append_peak_rss_bytes":append_peak_rss_bytes,"append_and_reopen_peak_rss_bytes":peak_rss(),"scope":"local synthetic durable append phase diagnostic; no provider or hosted claim"}));
}
