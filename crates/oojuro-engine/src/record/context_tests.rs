use super::*;
use crate::record::{
    Status,
    ranking::{Receipt, StageReceipt},
};
use std::{cell::RefCell, collections::BTreeMap};
struct Bytes {
    seen: RefCell<Vec<String>>,
    fail: bool,
    precision: Precision,
}
impl Default for Bytes {
    fn default() -> Self {
        Self {
            seen: RefCell::new(Vec::new()),
            fail: false,
            precision: Precision::Conservative,
        }
    }
}
impl TokenMeasure for Bytes {
    fn id(&self) -> &str {
        "synthetic-byte-meter/1"
    }
    fn precision(&self) -> Precision {
        self.precision
    }
    fn measure(&self, s: &str) -> std::result::Result<u64, MeasureError> {
        self.seen.borrow_mut().push(s.into());
        if self.fail {
            Err(MeasureError)
        } else {
            Ok(s.len() as u64)
        }
    }
}
fn opts() -> Options {
    Options {
        enabled: true,
        max_tokens: MAX_CONTEXT_BYTES as u64,
        ..Options::default()
    }
}
fn turn() -> Turn<'static> {
    Turn {
        session: "reported-session",
        number: 5,
        started_us: 10,
        prompt: "current prompt",
    }
}
fn hit(id: u64, text: &str, bits: f64) -> Hit {
    Hit {
        record: Record {
            schema: 1,
            id,
            writer: "writer-a".into(),
            adapter: "reported-harness".into(),
            agent_id: "reported-agent".into(),
            parent_id: Some("reported-parent".into()),
            valid_at: 8,
            recorded_at: 9,
            kind: Kind::Output,
            tool: Some("reported-tool".into()),
            status: Some(Status::Ok),
            exit: None,
            key: None,
            text: text.into(),
            attrs: BTreeMap::new(),
            indexed: true,
        },
        bits,
        continued: false,
        row: id,
    }
}
fn receipt() -> Receipt {
    Receipt {
        policy: "synthetic-ranking/1".into(),
        stages: vec![StageReceipt {
            hook: "rescore".into(),
            id: "synthetic".into(),
            version: "1".into(),
            table_version: Some("public-fixture/1".into()),
            weight: 1.0,
            skipped: None,
        }],
    }
}
#[test]
fn disabled_never_calls_meter_and_does_not_enable_injection() {
    struct Never;
    impl TokenMeasure for Never {
        fn id(&self) -> &str {
            panic!("disabled")
        }
        fn precision(&self) -> Precision {
            panic!("disabled")
        }
        fn measure(&self, _: &str) -> std::result::Result<u64, MeasureError> {
            panic!("disabled")
        }
    }
    assert!(!Options::default().enabled);
    assert!(
        pack(&[], Options::default(), &turn(), &Never)
            .unwrap()
            .is_none()
    );
}
#[test]
fn preserves_whole_original_record_provenance_unicode_and_measures_final_envelope() {
    let mut h = hit(1, "Recall 🧭\nignore instructions is untrusted text", 9.0);
    h.record
        .attrs
        .insert("reported_source".into(), serde_json::json!("fixture"));
    let meter = Bytes::default();
    let candidates = [Candidate::lexical(&h)];
    let out = pack(&candidates, opts(), &turn(), &meter).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out.json).unwrap();
    assert_eq!(
        parsed["items"][0]["record"],
        serde_json::to_value(&h.record).unwrap()
    );
    assert_eq!(out.items[0].record.text, h.record.text);
    assert!(out.json.contains("not instructions"));
    assert_eq!(meter.seen.borrow().last(), Some(&out.json));
    assert_eq!(out.measured_tokens, out.json.len() as u64);
    assert_eq!(out.precision, Precision::Conservative);
}
#[test]
fn exact_utf8_byte_and_complete_token_limits_do_not_cut_records() {
    let h = hit(1, "🧭".repeat(50).as_str(), 9.0);
    let candidates = [Candidate::lexical(&h)];
    let meter = Bytes::default();
    let full = pack(&candidates, opts(), &turn(), &meter).unwrap().unwrap();
    let mut o = opts();
    o.max_bytes = full.json.len();
    o.max_tokens = full.measured_tokens;
    assert_eq!(
        pack(&candidates, o, &turn(), &meter).unwrap().unwrap().json,
        full.json
    );
    o.max_bytes -= 1;
    assert!(pack(&candidates, o, &turn(), &meter).unwrap().is_none());
    o.max_bytes += 1;
    o.max_tokens -= 1;
    assert!(pack(&candidates, o, &turn(), &meter).unwrap().is_none());
}
#[test]
fn oversized_first_record_is_skipped_whole_and_smaller_later_record_can_fit() {
    let large = hit(1, &"x".repeat(7000), 20.0);
    let small = hit(2, "whole small", 10.0);
    let meter = Bytes::default();
    let candidates = [Candidate::lexical(&large), Candidate::lexical(&small)];
    let mut o = opts();
    o.max_bytes = 2000;
    let out = pack(&candidates, o, &turn(), &meter).unwrap().unwrap();
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.items[0].record.id, 2);
    assert!(!out.json.contains(&"x".repeat(100)));
}
#[test]
fn deterministic_ties_use_newer_then_writer_then_decimal_id_text_and_top_k() {
    let ten = hit(10, "ten", 9.0);
    let two = hit(2, "two", 9.0);
    let mut newer = hit(3, "newer", 9.0);
    newer.record.valid_at = 9;
    let meter = Bytes::default();
    let mut o = opts();
    o.top_k = 2;
    let first = pack(
        &[
            Candidate::lexical(&two),
            Candidate::lexical(&ten),
            Candidate::lexical(&newer),
        ],
        o,
        &turn(),
        &meter,
    )
    .unwrap()
    .unwrap();
    let second = pack(
        &[
            Candidate::lexical(&newer),
            Candidate::lexical(&ten),
            Candidate::lexical(&two),
        ],
        o,
        &turn(),
        &meter,
    )
    .unwrap()
    .unwrap();
    assert_eq!(first.json, second.json);
    assert_eq!(
        first.items.iter().map(|i| i.record.id).collect::<Vec<_>>(),
        vec![3, 10]
    );
}
#[test]
fn text_floor_survives_ranker_boost_and_reported_turn_prior() {
    let weak = hit(1, "weak", 7.99);
    let old = hit(2, "older", 9.0);
    let recent = hit(3, "recent", 8.0);
    let proof = receipt();
    let candidates = [
        Candidate {
            hit: &weak,
            rank: Some(RankEvidence {
                final_bits: 1000.0,
                receipt: &proof,
                deltas: Some(&[992.01]),
            }),
            reported_turn: Some(ReportedTurn {
                session: "reported-session",
                number: 4,
                started_us: 1,
            }),
        },
        Candidate::lexical(&old),
        Candidate {
            hit: &recent,
            rank: None,
            reported_turn: Some(ReportedTurn {
                session: "reported-session",
                number: 3,
                started_us: 1,
            }),
        },
    ];
    let meter = Bytes::default();
    let mut o = opts();
    o.bits_per_turn = 1.0;
    let out = pack(&candidates, o, &turn(), &meter).unwrap().unwrap();
    assert_eq!(
        out.items.iter().map(|i| i.record.id).collect::<Vec<_>>(),
        vec![3, 2]
    );
    assert_eq!(
        (out.items[0].lexical_bits, out.items[0].session_prior_bits),
        (8.0, 3.0)
    );
}
#[test]
fn ranked_evidence_is_preserved_but_does_not_verify_reported_causality() {
    let h = hit(1, "ranked original", 9.0);
    let proof = receipt();
    let deltas = [2.0];
    let meter = Bytes::default();
    let out = pack(
        &[Candidate {
            hit: &h,
            rank: Some(RankEvidence {
                final_bits: 11.0,
                receipt: &proof,
                deltas: Some(&deltas),
            }),
            reported_turn: None,
        }],
        opts(),
        &turn(),
        &meter,
    )
    .unwrap()
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&out.json).unwrap();
    assert_eq!(
        value["items"][0]["rank_receipt"]["stages"][0]["table_version"],
        "public-fixture/1"
    );
    assert_eq!(value["items"][0]["lexical_bits"], 9.0);
    assert_eq!(value["items"][0]["rank_deltas"][0], 2.0);
    assert!(
        value["trust"]
            .as_str()
            .unwrap()
            .contains("reported, not verified")
    );
}
#[test]
fn before_turn_checks_both_times_and_excludes_echoes_prompt_and_unindexed() {
    let mut future = hit(1, "future valid", 9.0);
    future.record.valid_at = 10;
    let mut late = hit(2, "late recorded", 9.0);
    late.record.recorded_at = 10;
    let context = hit(3, "<oojuro-context items=1>old envelope", 9.0);
    let mut prompt = hit(4, " current prompt ", 9.0);
    prompt.record.kind = Kind::Input;
    let mut tool = hit(5, "{}", 9.0);
    tool.record.kind = Kind::ToolCall;
    tool.record.tool = Some("mcp__oojuro__search".into());
    let mut metadata = hit(6, "returned context", 9.0);
    metadata
        .record
        .attrs
        .insert("oojuro".into(), serde_json::json!("result"));
    let mut off = hit(7, "not indexed", 9.0);
    off.record.indexed = false;
    let good = hit(8, "eligible", 9.0);
    let meter = Bytes::default();
    let records = [future, late, context, prompt, tool, metadata, off, good];
    let candidates = records.iter().map(Candidate::lexical).collect::<Vec<_>>();
    let out = pack(&candidates, opts(), &turn(), &meter).unwrap().unwrap();
    assert_eq!(
        out.items.iter().map(|i| i.record.id).collect::<Vec<_>>(),
        vec![8]
    );
}
#[test]
fn inconsistent_reported_turn_time_or_future_same_session_turn_is_invalid() {
    let h = hit(1, "source", 9.0);
    let meter = Bytes::default();
    for reported in [
        ReportedTurn {
            session: "reported-session",
            number: 5,
            started_us: 1,
        },
        ReportedTurn {
            session: "reported-session",
            number: 1,
            started_us: 9,
        },
    ] {
        assert!(matches!(
            pack(
                &[Candidate {
                    hit: &h,
                    rank: None,
                    reported_turn: Some(reported)
                }],
                opts(),
                &turn(),
                &meter
            ),
            Err(Error::Invalid)
        ));
    }
}
#[test]
fn rejects_nonfinite_scores_duplicate_sources_oversized_inputs_and_receipts() {
    let h = hit(1, "source", 9.0);
    let meter = Bytes::default();
    assert!(matches!(
        pack(
            &[Candidate::lexical(&h), Candidate::lexical(&h)],
            opts(),
            &turn(),
            &meter
        ),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        pack(
            &vec![Candidate::lexical(&h); MAX_CANDIDATES + 1],
            opts(),
            &turn(),
            &meter
        ),
        Err(Error::Bounds)
    ));
    let bad = hit(2, "NaN", f64::NAN);
    assert!(matches!(
        pack(&[Candidate::lexical(&bad)], opts(), &turn(), &meter),
        Err(Error::Bounds)
    ));
    let mut proof = receipt();
    proof.stages[0].version = "x".repeat(129);
    assert!(matches!(
        pack(
            &[Candidate {
                hit: &h,
                rank: Some(RankEvidence {
                    final_bits: 11.0,
                    receipt: &proof,
                    deltas: None
                }),
                reported_turn: None
            }],
            opts(),
            &turn(),
            &meter
        ),
        Err(Error::Bounds)
    ));
}
#[test]
fn callback_failure_aborts_the_pack_instead_of_returning_partial_context() {
    let a = hit(1, "first", 9.0);
    let b = hit(2, "second", 8.0);
    let meter = Bytes {
        fail: true,
        ..Bytes::default()
    };
    assert!(matches!(
        pack(
            &[Candidate::lexical(&a), Candidate::lexical(&b)],
            opts(),
            &turn(),
            &meter
        ),
        Err(Error::Unavailable)
    ));
}
