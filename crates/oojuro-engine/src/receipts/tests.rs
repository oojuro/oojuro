use super::*;
use crate::capture_log::{Batch, Identity, Line, Outcome, PolicyReceipt};
use crate::lexical::{Index, Query, segments::PersistentIndex};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const KEY: [u8; 32] = [37; 32];
const SOURCE: Source = [12; 16];
const WRITER: &str = "receipt-synthetic-owner";
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::current_dir()
            .unwrap()
            .join(".scratch")
            .join(format!(
                "receipts-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn log(&self) -> CaptureLog {
        CaptureLog::open(&self.root.join("authority"), &KEY, identity()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn identity() -> Identity {
    Identity {
        writer: WRITER.into(),
        agent: 1,
        first_id: 9,
        adapter: "synthetic-adapter-v1".into(),
    }
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        agent: 1,
    }
}
fn view(log: &CaptureLog) -> ReceiptView {
    ReceiptView::open(log, scope(), Range::default()).unwrap()
}
fn prepare(f: &Fixture) -> CaptureLog {
    let mut log = f.log();
    log.register(
        SOURCE,
        b"private_source_descriptor_do_not_export",
        br#"{"private_parser_state":"initial_denied_value"}"#,
    )
    .unwrap();
    log
}
fn event(id: u64, at: u64, text: &str, indexed: bool) -> Event {
    Event {
        id,
        session: 77,
        parent_session: 0,
        agent: 1,
        turn: 1,
        seq: (id - 9) as u32,
        parent: 0,
        revises: 0,
        kind: 3,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: -123,
        duration_us: 0,
        tool: "Read".into(),
        args: "{}".into(),
        text: text.into(),
        indexed,
        line_start: at,
        line_end: at + 3,
        emission: 0,
    }
}
fn append(log: &mut CaptureLog, text: &str, indexed: bool) -> u64 {
    let progress = log.progress(SOURCE).unwrap();
    let id = log.next_id();
    let at = progress.raw_end;
    let e = event(id, at, text, indexed);
    let b = ParsedBatch {
        source: SOURCE,
        raw_start: at,
        parsed_before: progress.parsed_end,
        outcome: Outcome {
            raw_end: at + 3,
            parsed_end: at + 3,
            first_id: Some(id),
            stored: 1,
            next_id: id + 1,
        },
        policy: PolicyReceipt {
            policy_id: "record-v1".into(),
            raw_authorization: "explicit-local-consent".into(),
            lines: vec![Line {
                start: at,
                end: at + 3,
                kind: LineKind::Parsed,
                emissions: vec![Decision::Stored { id, indexed }],
            }],
        },
        context: br#"{"private_parser_state":"denied_tool_payload","version":"arbitrary_secret"}"#
            .to_vec(),
        events: vec![e],
    };
    let expected = b.outcome.clone();
    assert_eq!(
        log.commit(Batch {
            raw: b"{}\n".to_vec(),
            parsed: b
        })
        .unwrap(),
        expected
    );
    id
}
fn withheld_blank_malformed(log: &mut CaptureLog) {
    let progress = log.progress(SOURCE).unwrap();
    let at = progress.raw_end;
    let raw = b"denied_secret_payload\n\ninvalid_secret_line\n";
    let first = at + 22;
    assert_eq!(raw[21], b'\n');
    let lines = vec![
        Line {
            start: at,
            end: first,
            kind: LineKind::Parsed,
            emissions: vec![Decision::Withheld {
                reason: "policy_denied".into(),
            }],
        },
        Line {
            start: first,
            end: first + 1,
            kind: LineKind::Blank,
            emissions: vec![],
        },
        Line {
            start: first + 1,
            end: at + raw.len() as u64,
            kind: LineKind::Malformed,
            emissions: vec![],
        },
    ];
    let b = ParsedBatch {
        source: SOURCE,
        raw_start: at,
        parsed_before: progress.parsed_end,
        outcome: Outcome {
            raw_end: at + raw.len() as u64,
            parsed_end: at + raw.len() as u64,
            first_id: None,
            stored: 0,
            next_id: log.next_id(),
        },
        policy: PolicyReceipt {
            policy_id: "withhold-v2".into(),
            raw_authorization: "explicit-local-consent".into(),
            lines,
        },
        context: br#"{"private_parser_state":"withheld_context_secret"}"#.to_vec(),
        events: vec![],
    };
    log.commit(Batch {
        raw: raw.to_vec(),
        parsed: b,
    })
    .unwrap();
}
fn content(page: &Page) -> Vec<serde_json::Value> {
    page.entries
        .iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect()
}
fn limits(n: usize) -> Limits {
    Limits {
        entries: n,
        bytes: MAX_OUTPUT_BYTES,
    }
}

#[test]
fn exact_original_receipts_and_private_state_boundary() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    let first = append(&mut log, "alpha original", true);
    let stored = append(&mut log, "stored but not indexed", false);
    withheld_blank_malformed(&mut log);
    let v = view(&log);
    let p = v.page(&log, limits(100)).unwrap();
    assert_eq!(p.entries.len(), 5);
    assert!(p.next.is_none());
    let r = &p.entries[0];
    assert_eq!(
        (r.ordinal, r.batch, r.source_generation, r.emission),
        (0, 0, SOURCE, Some(0))
    );
    assert_eq!(r.line, Span { start: 0, end: 3 });
    assert_eq!(r.batch_raw, Span { start: 0, end: 3 });
    assert_eq!(r.policy_id, "record-v1");
    assert_eq!(r.adapter, "synthetic-adapter-v1");
    let Output::Stored { event } = &r.output else {
        panic!()
    };
    assert!(event == log.event(first).unwrap().unwrap().1);
    let Output::Stored { event } = v
        .get(&log, stored, MAX_OUTPUT_BYTES)
        .unwrap()
        .unwrap()
        .output
    else {
        panic!()
    };
    assert!(!event.indexed);
    assert_eq!(event.ts_us, -123);
    assert!(matches!(&p.entries[2].output,Output::Withheld { reason } if reason=="policy_denied"));
    assert!(matches!(
        &p.entries[3].output,
        Output::NoOutput {
            line_kind: LineKind::Blank
        }
    ));
    assert!(matches!(
        &p.entries[4].output,
        Output::NoOutput {
            line_kind: LineKind::Malformed
        }
    ));
    let json = String::from_utf8(v.export(&log, limits(100)).unwrap()).unwrap();
    for private in [
        "private_source_descriptor_do_not_export",
        "private_parser_state",
        "initial_denied_value",
        "denied_tool_payload",
        "arbitrary_secret",
        "denied_secret_payload",
        "invalid_secret_line",
        "withheld_context_secret",
    ] {
        assert!(!json.contains(private), "private bytes escaped: {private}");
    }
    let s = v.stats(&log, MAX_OUTPUT_BYTES).unwrap();
    assert_eq!(
        (
            s.receipts,
            s.stored,
            s.indexed,
            s.withheld,
            s.blank,
            s.malformed
        ),
        (5, 2, 1, 1, 1, 1)
    );
}
#[test]
fn pages_no_skips_or_duplicates_and_retry_keeps_original_snapshot_after_append_reopen() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    for i in 0..7 {
        append(&mut log, &format!("alpha item_{i}"), true);
    }
    withheld_blank_malformed(&mut log);
    let v = view(&log);
    let expected = content(&v.page(&log, limits(100)).unwrap());
    let first = v.page(&log, limits(2)).unwrap();
    let cursor = first.next.clone().unwrap();
    let retry = ReceiptView::resume(&log, scope(), Range::default(), &cursor).unwrap();
    let retry_page = retry.page(&log, limits(2)).unwrap();
    assert_eq!(
        content(&retry_page),
        content(
            &ReceiptView::resume(&log, scope(), Range::default(), &cursor)
                .unwrap()
                .page(&log, limits(2))
                .unwrap()
        )
    );
    let last_id = append(&mut log, "newly acknowledged outside old snapshot", true);
    assert!(v.get(&log, last_id, MAX_OUTPUT_BYTES).unwrap().is_none());
    assert!(
        view(&log)
            .get(&log, last_id, MAX_OUTPUT_BYTES)
            .unwrap()
            .is_some()
    );
    let watermark = v.watermark().clone();
    drop(log);
    let log = f.log();
    let mut all = content(&first);
    let mut next = Some(cursor);
    while let Some(token) = next {
        let resumed = ReceiptView::resume(&log, scope(), Range::default(), &token).unwrap();
        assert_eq!(&watermark, resumed.watermark());
        let page = resumed.page(&log, limits(2)).unwrap();
        all.extend(content(&page));
        next = page.next;
    }
    assert_eq!(all, expected);
    assert_eq!(all.len(), 10);
    assert_eq!(v.stats(&log, MAX_OUTPUT_BYTES).unwrap().receipts, 10);
}
#[test]
fn ordinal_range_includes_unknown_time_receipts_without_inventing_timestamps() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    append(&mut log, "alpha", true);
    withheld_blank_malformed(&mut log);
    let range = Range { start: 1, end: 3 };
    let v = ReceiptView::open(&log, scope(), range).unwrap();
    let p = v.page(&log, limits(1)).unwrap();
    assert_eq!(p.entries[0].ordinal, 1);
    let next = ReceiptView::resume(&log, scope(), range, p.next.as_ref().unwrap())
        .unwrap()
        .page(&log, limits(1))
        .unwrap();
    assert_eq!(next.entries[0].ordinal, 2);
    assert!(next.next.is_none());
    assert!(v.get(&log, 9, MAX_OUTPUT_BYTES).unwrap().is_none());
    assert_eq!(v.stats(&log, MAX_OUTPUT_BYTES).unwrap().receipts, 2);
    for r in p.entries.iter().chain(&next.entries) {
        let json = serde_json::to_value(r).unwrap();
        assert!(json.get("time").is_none());
        assert!(json["output"].get("ts_us").is_none());
    }
    let empty = ReceiptView::open(
        &log,
        scope(),
        Range {
            start: 100,
            end: 101,
        },
    )
    .unwrap()
    .page(&log, limits(10))
    .unwrap();
    assert!(empty.entries.is_empty() && empty.next.is_none());
}
#[test]
fn refuses_scope_cursor_tamper_wrong_range_domain_ahead_and_inconsistent_stamps() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    for _ in 0..3 {
        append(&mut log, "alpha", true);
    }
    assert!(matches!(
        ReceiptView::open(
            &log,
            Scope {
                writer: "other",
                agent: 1
            },
            Range::default()
        ),
        Err(Error::Scope)
    ));
    assert!(matches!(
        ReceiptView::open(
            &log,
            Scope {
                writer: WRITER,
                agent: 2
            },
            Range::default()
        ),
        Err(Error::Scope)
    ));
    let v = view(&log);
    let token = v.page(&log, limits(1)).unwrap().next.unwrap();
    assert!(ReceiptView::resume(&log, scope(), Range { start: 1, end: 10 }, &token).is_err());
    let mut corrupt = token.clone().into_bytes();
    let n = corrupt.len() / 2;
    corrupt[n] = if corrupt[n] == b'0' { b'1' } else { b'0' };
    assert!(
        ReceiptView::resume(
            &log,
            scope(),
            Range::default(),
            std::str::from_utf8(&corrupt).unwrap()
        )
        .is_err()
    );
    for bad in ["", "g0", "0", &"a".repeat(CURSOR_BOUND + 2)] {
        assert!(ReceiptView::resume(&log, scope(), Range::default(), bad).is_err());
    }
    let plain = encoded(&decode(&log, &token).unwrap()).unwrap();
    let bytes = log
        .cache_cipher()
        .seal_frame(b"oojuro/receipt-cursor/other-domain", &plain);
    let wrong: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(ReceiptView::resume(&log, scope(), Range::default(), &wrong).is_err());
    assert!(
        ReceiptView::resume(&log, scope(), Range::default(), &v.cursor(&log, 4).unwrap()).is_err()
    );
    let mut future = view(&log);
    future.watermark.records += 1;
    assert!(future.page(&log, limits(1)).is_err());
    let mut bad = view(&log);
    bad.watermark.commitment.replace_range(0..2, "zz");
    assert!(bad.page(&log, limits(1)).is_err());
    assert!(matches!(
        ReceiptView::open(&log, scope(), Range { start: 2, end: 1 }),
        Err(Error::Bounds)
    ));
}
#[test]
fn independent_origin_and_copied_origin_fork_cannot_resume_or_read_old_view() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    append(&mut log, "common", true);
    drop(log);
    let fork = f.root.join("fork");
    fs::create_dir(&fork).unwrap();
    for entry in fs::read_dir(f.root.join("authority")).unwrap() {
        let e = entry.unwrap();
        fs::copy(e.path(), fork.join(e.file_name())).unwrap();
    }
    let mut log = f.log();
    append(&mut log, "original branch", true);
    let v = view(&log);
    let token = v.page(&log, limits(1)).unwrap().next.unwrap();
    drop(log);
    let mut branch = CaptureLog::open(&fork, &KEY, identity()).unwrap();
    append(&mut branch, "fork branch", true);
    assert!(v.page(&branch, limits(10)).is_err());
    assert!(ReceiptView::resume(&branch, scope(), Range::default(), &token).is_err());
    let g = Fixture::new();
    let mut other = prepare(&g);
    append(&mut other, "other", true);
    append(&mut other, "same highwater", true);
    assert!(v.get(&other, 9, MAX_OUTPUT_BYTES).is_err());
    assert!(ReceiptView::resume(&other, scope(), Range::default(), &token).is_err());
}
#[test]
fn exact_encoded_bounds_refuse_unsplittable_output_and_invalid_limits() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    append(&mut log, &"alpha ".repeat(20_000), true);
    append(&mut log, &"small ".repeat(1000), true);
    let v = view(&log);
    assert!(matches!(
        v.page(
            &log,
            Limits {
                entries: 1,
                bytes: 1000
            }
        ),
        Err(Error::Bounds)
    ));
    assert!(matches!(v.get(&log, 9, 1000), Err(Error::Bounds)));
    for bad in [
        Limits {
            entries: 0,
            bytes: 1000,
        },
        Limits {
            entries: MAX_ENTRIES + 1,
            bytes: 1000,
        },
        Limits {
            entries: 1,
            bytes: 0,
        },
        Limits {
            entries: 1,
            bytes: MAX_OUTPUT_BYTES + 1,
        },
    ] {
        assert!(matches!(v.export(&log, bad), Err(Error::Bounds)));
    }
    let one = v.page(&log, limits(1)).unwrap();
    let exact = encoded(&one).unwrap().len();
    // Cursor nonces differ, but the hex envelope has identical byte length.
    let bytes = v
        .export(
            &log,
            Limits {
                entries: 1,
                bytes: exact,
            },
        )
        .unwrap();
    assert_eq!(bytes.len(), exact);
    assert!(matches!(
        v.page(
            &log,
            Limits {
                entries: 1,
                bytes: exact - 1
            }
        ),
        Err(Error::Bounds)
    ));
    let bounded = v
        .page(
            &log,
            Limits {
                entries: 1000,
                bytes: exact,
            },
        )
        .unwrap();
    assert_eq!(bounded.entries.len(), 1);
    let last = ReceiptView::resume(
        &log,
        scope(),
        Range::default(),
        bounded.next.as_ref().unwrap(),
    )
    .unwrap()
    .export(
        &log,
        Limits {
            entries: 1,
            bytes: MAX_OUTPUT_BYTES,
        },
    )
    .unwrap();
    assert!(last.len() <= MAX_OUTPUT_BYTES);
    assert!(matches!(v.stats(&log, 1), Err(Error::Bounds)));
}
#[test]
fn native_memory_and_persistent_search_hits_resolve_exact_original_ids_without_dedupe_loss() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    let a = append(&mut log, "alpha original", true);
    let duplicate = append(&mut log, "alpha original", true);
    let private = append(&mut log, "alpha stored-only", false);
    let q = Query {
        writer: WRITER,
        agent: 1,
        text: "alpha",
        from: i64::MIN,
        to: i64::MAX,
        k: 100,
    };
    let memory = Index::rebuild(&log).unwrap();
    let disk = PersistentIndex::open(&log, &f.root.join("index")).unwrap();
    let first = memory.search(&q).unwrap();
    let second = disk.search(&log, &q).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, a);
    assert_eq!(first[0].bits.to_bits(), second[0].bits.to_bits());
    let v = view(&log);
    for id in [a, duplicate, private] {
        let Output::Stored { event } = v.get(&log, id, MAX_OUTPUT_BYTES).unwrap().unwrap().output
        else {
            panic!()
        };
        assert!(&event == log.event(id).unwrap().unwrap().1);
    }
    assert_eq!(v.stats(&log, MAX_OUTPUT_BYTES).unwrap().stored, 3);
    drop(disk);
    drop(log);
    let log = f.log();
    let reopened = PersistentIndex::open(&log, &f.root.join("index")).unwrap();
    assert_eq!(reopened.search(&log, &q).unwrap()[0].id, a);
    assert!(v.get(&log, duplicate, MAX_OUTPUT_BYTES).unwrap().is_some());
}

#[test]
fn public_watermark_never_exposes_plaintext_raw_context_prefix_digest() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    append(&mut log, "alpha", true);
    withheld_blank_malformed(&mut log);
    let v = view(&log);
    let json = serde_json::to_value(v.page(&log, limits(1)).unwrap()).unwrap();
    let sealed = json["watermark"]["commitment"]
        .as_str()
        .expect("prefix commitment must be encrypted, not a public plaintext hash");
    assert_eq!(sealed.len(), 144);
    let actual = log.cache_tip().unwrap().digest;
    assert_ne!(
        sealed,
        actual
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_ne!(
        json["watermark"]["commitment"],
        serde_json::to_value(actual).unwrap()
    );
}

#[test]
fn absent_get_exact_null_bound_and_compaction_invalidates_existing_snapshots() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    append(&mut log, "alpha", true);
    append(&mut log, "beta", true);
    let v = view(&log);
    let token = v.page(&log, limits(1)).unwrap().next.unwrap();
    assert!(matches!(v.get(&log, u64::MAX, 3), Err(Error::Bounds)));
    assert!(v.get(&log, u64::MAX, 4).unwrap().is_none());
    log.compact().unwrap();
    assert!(matches!(v.page(&log, limits(1)), Err(Error::Authority)));
    assert!(ReceiptView::resume(&log, scope(), Range::default(), &token).is_err());
    assert_eq!(view(&log).stats(&log, MAX_OUTPUT_BYTES).unwrap().stored, 2);
}
#[test]
fn public_prefix_commitment_is_bound_to_counts_identity_and_domain() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    append(&mut log, "alpha", true);
    let v = view(&log);
    let w = v.watermark();
    let bytes = unhex(&w.commitment, 144).unwrap();
    let digest = log
        .cache_cipher()
        .open_frame(&prefix_aad(w), &bytes)
        .unwrap();
    assert_eq!(digest, log.cache_tip().unwrap().digest);
    let mut changed = w.clone();
    changed.batches += 1;
    assert!(
        log.cache_cipher()
            .open_frame(&prefix_aad(&changed), &bytes)
            .is_err()
    );
    changed = w.clone();
    changed.identity[0] ^= 1;
    assert!(
        log.cache_cipher()
            .open_frame(&prefix_aad(&changed), &bytes)
            .is_err()
    );
    assert!(log.cache_cipher().open_frame(CURSOR_AAD, &bytes).is_err());
}

#[test]
fn interleaved_withheld_and_stored_emissions_keep_original_sparse_emission_positions() {
    let f = Fixture::new();
    let mut log = prepare(&f);
    let a = event(9, 0, "alpha first output", true);
    let mut b = event(10, 0, "second stored-only output", false);
    b.emission = 2;
    let parsed = ParsedBatch {
        source: SOURCE,
        raw_start: 0,
        parsed_before: 0,
        outcome: Outcome {
            raw_end: 3,
            parsed_end: 3,
            first_id: Some(9),
            stored: 2,
            next_id: 11,
        },
        policy: PolicyReceipt {
            policy_id: "mixed-emissions-v1".into(),
            raw_authorization: "explicit-local-consent".into(),
            lines: vec![Line {
                start: 0,
                end: 3,
                kind: LineKind::Parsed,
                emissions: vec![
                    Decision::Stored {
                        id: 9,
                        indexed: true,
                    },
                    Decision::Withheld {
                        reason: "thinking_disabled".into(),
                    },
                    Decision::Stored {
                        id: 10,
                        indexed: false,
                    },
                ],
            }],
        },
        context: br#"{"private_parser_state":"not_for_readers"}"#.to_vec(),
        events: vec![a, b],
    };
    log.commit(Batch {
        raw: b"{}\n".to_vec(),
        parsed,
    })
    .unwrap();
    let v = view(&log);
    let first = v.page(&log, limits(1)).unwrap();
    let middle = ReceiptView::resume(
        &log,
        scope(),
        Range::default(),
        first.next.as_ref().unwrap(),
    )
    .unwrap()
    .page(&log, limits(1))
    .unwrap();
    assert!(
        matches!(&middle.entries[0].output,Output::Withheld {reason} if reason=="thinking_disabled")
    );
    let last = ReceiptView::resume(
        &log,
        scope(),
        Range::default(),
        middle.next.as_ref().unwrap(),
    )
    .unwrap()
    .page(&log, limits(1))
    .unwrap();
    let r = &last.entries[0];
    assert_eq!((r.ordinal, r.emission), (2, Some(2)));
    assert_eq!(r.line, Span { start: 0, end: 3 });
    let Output::Stored { event } = &r.output else {
        panic!()
    };
    assert_eq!((event.id, event.emission, event.indexed), (10, 2, false));
    assert!(last.next.is_none());
    assert_eq!(
        v.get(&log, 10, MAX_OUTPUT_BYTES).unwrap().unwrap().emission,
        Some(2)
    );
}
