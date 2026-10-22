//! Synthetic bitwise differential oracle; production engine never imports trace/nine.
use super::*;
use crate::capture_log::{
    Batch, Decision, Identity, Line, LineKind, Outcome, PolicyReceipt, Source,
};
use oojuro_trace::{EventIn, TraceDb};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const SOURCE: Source = [14; 16];
const KEY: [u8; 32] = [28; 32];
const WRITER: &str = "native-synthetic-owner";
struct Fixture {
    disk: PathBuf,
    ram: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "native-lexical-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let disk = std::env::current_dir().unwrap().join("target").join(&name);
        let ram = PathBuf::from("/dev/shm").join(&name);
        fs::create_dir_all(&disk).unwrap();
        fs::create_dir(&ram).unwrap();
        Self { disk, ram }
    }
    fn open(&self) -> CaptureLog {
        CaptureLog::open(&self.disk.join("authority"), &KEY, identity()).unwrap()
    }
    fn oracle(&self) -> TraceDb {
        TraceDb::open(
            &self.ram.join("oracle"),
            oojuro_trace::Options {
                flush_text_bytes: 1 << 20,
                ..Default::default()
            },
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.disk);
        let _ = fs::remove_dir_all(&self.ram);
    }
}
fn identity() -> Identity {
    Identity {
        writer: WRITER.into(),
        agent: 1,
        first_id: 9,
        adapter: "native-test-v1".into(),
    }
}
fn event(text: &str, ts: i64, kind: u8, indexed: bool) -> Event {
    Event {
        id: 0,
        session: 77,
        parent_session: 0,
        agent: 1,
        turn: 1,
        seq: 0,
        parent: 0,
        revises: 0,
        kind,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: ts,
        duration_us: 0,
        tool: String::new(),
        args: String::new(),
        text: text.into(),
        indexed,
        line_start: 0,
        line_end: 0,
        emission: 0,
    }
}
fn batch(log: &CaptureLog, events: Vec<Event>) -> Batch {
    let p = log.progress(SOURCE).unwrap();
    let next = log.next_id();
    let n = events.len();
    let events: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(i, mut e)| {
            e.id = next + i as u64;
            e.seq = (e.id - 9) as u32;
            e.line_start = p.raw_end + 3 * i as u64;
            e.line_end = e.line_start + 3;
            if e.args.is_empty() {
                e.args = format!(r#"{{"src":"1:{}:0","rt":100,"keep":"same"}}"#, e.line_start);
            }
            e
        })
        .collect();
    Batch {
        raw: b"{}\n".repeat(n),
        parsed: ParsedBatch {
            source: SOURCE,
            raw_start: p.raw_end,
            parsed_before: p.parsed_end,
            outcome: Outcome {
                raw_end: p.raw_end + 3 * n as u64,
                parsed_end: p.raw_end + 3 * n as u64,
                first_id: events.first().map(|e| e.id),
                stored: n as u64,
                next_id: next + n as u64,
            },
            policy: PolicyReceipt {
                policy_id: "test-policy-v1".into(),
                raw_authorization: "synthetic-consent".into(),
                lines: events
                    .iter()
                    .map(|e| Line {
                        start: e.line_start,
                        end: e.line_end,
                        kind: LineKind::Parsed,
                        emissions: vec![Decision::Stored {
                            id: e.id,
                            indexed: e.indexed,
                        }],
                    })
                    .collect(),
            },
            context: br#"{"complete":"synthetic-parser-receipt"}"#.to_vec(),
            events,
        },
    }
}
fn append_oracle(db: &TraceDb, b: &ParsedBatch) {
    let input: Vec<_> = b
        .events
        .iter()
        .map(|e| EventIn {
            id: e.id,
            session: e.session,
            parent_session: e.parent_session,
            agent: e.agent,
            turn: e.turn,
            seq: e.seq,
            parent: e.parent,
            revises: e.revises,
            kind: e.kind,
            status: e.status,
            exit_code: e.exit_code,
            tokens_in: e.tokens_in,
            tokens_out: e.tokens_out,
            ts_us: e.ts_us,
            duration_us: e.duration_us,
            tool: &e.tool,
            args: &e.args,
            text: &e.text,
            indexed: e.indexed,
        })
        .collect();
    db.append(&input).unwrap();
}
fn native_sig(hits: Vec<Hit>) -> Vec<(u64, u64, bool, i64, u64)> {
    hits.into_iter()
        .map(|h| (h.id, h.bits.to_bits(), h.continued, h.time, h.row))
        .collect()
}
fn compare(index: &Index, oracle: &TraceDb) {
    for text in [
        "",
        "battery",
        "battery battery electrode",
        "lookup_needle battery",
        "absent_name battery",
        "\"battery electrode\"",
        "\"the\"",
        "the and",
        "running runners",
        "naïve 東京",
        "___",
        "123",
        "\"lookup_needle\"",
        "key:9",
        "9",
        "key:absent",
        "\"Battery\"",
        "file_9",
        "identifier____tail",
    ] {
        for (from, to) in [(i64::MIN, i64::MAX), (-5, 0), (1, 3), (7, 11), (1000, 2000)] {
            for k in [1, 3, 10, 1000] {
                for filtered in [false, true] {
                    let q = Query {
                        writer: WRITER,
                        agent: 1,
                        text,
                        from,
                        to,
                        k,
                    };
                    let expected = oracle
                        .search_where(1, text, from, to, k, &mut |e| {
                            !filtered || (e.kind == 3 && e.id % 3 != 0)
                        })
                        .unwrap()
                        .into_iter()
                        .map(|h| (h.id, h.bits.to_bits(), h.continued, h.time, h.row))
                        .collect::<Vec<_>>();
                    let actual = index
                        .search_where(&q, &mut |e| !filtered || (e.kind == 3 && e.id % 3 != 0))
                        .unwrap();
                    assert_eq!(
                        native_sig(actual),
                        expected,
                        "query={text:?} range={from}..{to} k={k} filtered={filtered}"
                    );
                }
            }
        }
    }
}
#[test]
fn native_bitwise_oracle_port_covers_time_phrases_identifiers_dedupe_filters_and_pruning() {
    let f = Fixture::new();
    let mut log = f.open();
    log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
    let oracle = f.oracle();
    let first = batch(
        &log,
        vec![
            event("battery electrode zebra", 0, 3, true),
            event("zebra electrode battery", 0, 3, true),
            event("lookup_needle anchor battery", 1, 3, true),
            event("lookup needle fallback battery", 1, 3, true),
            event("STOREDONLY battery", 2, 3, false),
            event("", 2, 3, true),
            event("the and", 3, 0, false),
        ],
    );
    append_oracle(&oracle, &first.parsed);
    log.commit(first).unwrap();
    let mut index = Index::rebuild(&log).unwrap();
    compare(&index, &oracle);
    let mut events = Vec::new();
    for i in 0..360 {
        let text = match i % 9 {
            0 => format!(
                "battery electrode many {} running",
                "battery ".repeat(i % 11)
            ),
            1 => "lookup_needle battery".into(),
            2 => "lookup needle battery".into(),
            3 => "naïve 東京 running runners".into(),
            4 => "the and".into(),
            5 => "identifier____tail file_9 123".into(),
            6 => "Battery electrode zebra".into(),
            7 => format!("novel passage {}", "filler ".repeat(i % 5)),
            _ => "battery electrode zebra".into(),
        };
        let mut e = event(
            &text,
            (i % 17) as i64 - 5,
            if i % 13 == 0 { 2 } else { 3 },
            i % 13 != 0 && i % 23 != 0,
        );
        // Some duplicates compare equal; tool/args/class differences remain separate.
        if i % 7 == 0 {
            e.tool = "shell".into();
        }
        if i % 19 == 0 {
            e.args = r#"{"src":"1:99:0","rt":101,"keep":"different"}"#.into();
        }
        events.push(e);
    }
    // Multiple committed batches exercise the same cross-batch dedupe mapping.
    for chunk in events.chunks(37) {
        let b = batch(&log, chunk.to_vec());
        append_oracle(&oracle, &b.parsed);
        log.commit(b).unwrap();
        index.refresh(&log).unwrap();
    }
    compare(&index, &oracle);
    for text in ["battery", "lookup_needle battery", "\"the\"", ""] {
        for k in [1, 3, 10] {
            let q = Query {
                writer: WRITER,
                agent: 1,
                text,
                from: i64::MIN,
                to: i64::MAX,
                k,
            };
            assert_eq!(
                native_sig(
                    index
                        .continued(&q, k, true)
                        .into_iter()
                        .map(|(_, h)| h)
                        .collect()
                ),
                native_sig(
                    index
                        .continued(&q, k, false)
                        .into_iter()
                        .map(|(_, h)| h)
                        .collect()
                )
            );
        }
    }
    drop(index);
    log.compact().unwrap();
    drop(log);
    let log = f.open();
    let index = Index::rebuild(&log).unwrap();
    compare(&index, &oracle);
    let q = Query {
        writer: "other-writer",
        agent: 1,
        text: "battery",
        from: 0,
        to: i64::MAX,
        k: 10,
    };
    assert_eq!(index.search(&q).unwrap_err(), Error::Authority);
}
#[test]
fn rejected_authority_commit_does_not_project_and_reopen_requires_fresh_binding() {
    let f = Fixture::new();
    let mut log = f.open();
    log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
    let mut index = Index::rebuild(&log).unwrap();
    let mut b = batch(&log, vec![event("UNCOMMITTED", 1, 3, true)]);
    b.parsed.policy.lines[0].emissions.clear();
    assert!(log.commit(b).is_err());
    index.refresh(&log).unwrap();
    let q = Query {
        writer: WRITER,
        agent: 1,
        text: "UNCOMMITTED",
        from: 0,
        to: i64::MAX,
        k: 10,
    };
    assert!(index.search(&q).unwrap().is_empty());
    drop(log);
    let mut log = f.open();
    assert_eq!(index.refresh(&log), Err(Error::Authority));
    let b = batch(&log, vec![event("COMMITTED", 1, 3, true)]);
    log.commit(b).unwrap();
    let index = Index::rebuild(&log).unwrap();
    let q = Query {
        text: "COMMITTED",
        ..q
    };
    assert_eq!(index.search(&q).unwrap()[0].id, 9);
}

#[test]
fn native_above_floor_large_candidate_pruning_and_full_decimal_prefix_ties_match_oracle() {
    let f = Fixture::new();
    let mut log = f.open();
    log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
    let oracle = f.oracle();
    let events = (0..1200)
        .map(|i| {
            let mut text = format!("battery shared row{i} ");
            text.push_str(&"battery ".repeat(i % 13));
            for j in 0..12 {
                text.push_str(&format!("unique{i}x{j} "));
            }
            event(&text, (i % 19) as i64, 3, true)
        })
        .collect();
    let b = batch(&log, events);
    append_oracle(&oracle, &b.parsed);
    log.commit(b).unwrap();
    let index = Index::rebuild(&log).unwrap();
    assert_eq!(index.documents.len(), 1200);
    assert!(
        index
            .documents
            .iter()
            .map(|d| u64::from(d.scope))
            .sum::<u64>() as f64
            > FLOOR_RECORDS * MU
    );
    for text in [
        "battery",
        "battery shared",
        "row1 battery",
        "absent_name battery",
    ] {
        for k in [1, 7, 1000] {
            let q = Query {
                writer: WRITER,
                agent: 1,
                text,
                from: i64::MIN,
                to: i64::MAX,
                k,
            };
            let expected = oracle
                .search_where(1, text, q.from, q.to, k, &mut |_| true)
                .unwrap()
                .into_iter()
                .map(|h| (h.id, h.bits.to_bits(), h.continued, h.time, h.row))
                .collect::<Vec<_>>();
            assert_eq!(
                native_sig(index.search(&q).unwrap()),
                expected,
                "{text},k={k}"
            );
            assert_eq!(
                native_sig(
                    index
                        .continued(&q, k, true)
                        .into_iter()
                        .map(|(_, h)| h)
                        .collect()
                ),
                native_sig(
                    index
                        .continued(&q, k, false)
                        .into_iter()
                        .map(|(_, h)| h)
                        .collect()
                )
            );
        }
    }
    let q = Query {
        writer: WRITER,
        agent: 1,
        text: "battery",
        from: 0,
        to: i64::MAX,
        k: 0,
    };
    assert!(index.search(&q).unwrap().is_empty()); // Oracle's k=0 TopK path panics; not a parity domain.
    let q = Query {
        text: &"x".repeat(MAX_QUERY_BYTES + 1),
        k: 1,
        ..q
    };
    assert_eq!(index.search(&q).unwrap_err(), Error::Invalid);
    let f = Fixture::new();
    let mut id = identity();
    id.first_id = 100_000_000_000;
    let mut log = CaptureLog::open(&f.disk.join("authority"), &KEY, id).unwrap();
    log.register(SOURCE, b"synthetic", br#"{"seq":0}"#).unwrap();
    // TraceDb's columns allocate by numeric ID; test long IDs through the
    // direct Nine oracle to avoid its unrelated dense column allocation.
    let oracle = oojuro_nine::Nine::open_with(
        &f.ram.join("nine"),
        oojuro_nine::Options {
            query_threads: 1,
            routing_entropy: oojuro_nine::RoutingEntropy::TopK,
            ..Default::default()
        },
    )
    .unwrap();
    let b = batch(
        &log,
        vec![
            event("zebra battery electrode", 5, 3, true),
            event("battery electrode zebra", 5, 3, true),
            event("electrode zebra battery", 5, 3, true),
        ],
    );
    oracle
        .insert_batch(
            b.parsed
                .events
                .iter()
                .map(|e| {
                    let id = e.id.to_string();
                    oojuro_core::Record::new(&id, &id, &e.text, e.ts_us)
                })
                .collect(),
        )
        .unwrap();
    log.commit(b).unwrap();
    let index = Index::rebuild(&log).unwrap();
    for k in [1, 2, 10] {
        let q = Query {
            writer: WRITER,
            agent: 1,
            text: "zebra",
            from: 0,
            to: 10,
            k,
        };
        let expected = oracle
            .find("zebra", 0, 10, k)
            .unwrap()
            .matches
            .into_iter()
            .map(|h| {
                let id = h.id.parse::<u64>().unwrap();
                (id, h.score_bits.to_bits(), h.continued, h.time, id)
            })
            .collect::<Vec<_>>();
        assert_eq!(native_sig(index.search(&q).unwrap()), expected);
    }
}
