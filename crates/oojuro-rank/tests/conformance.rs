use oojuro_rank::*;
use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixed {
    deltas: Vec<f64>,
    table: Option<&'static str>,
    declared: usize,
    charge: usize,
    fail: bool,
    calls: AtomicUsize,
}
impl Fixed {
    fn new(deltas: Vec<f64>) -> Self {
        Self {
            deltas,
            table: Some("synthetic-1"),
            declared: 2,
            charge: 2,
            fail: false,
            calls: AtomicUsize::new(0),
        }
    }
}
impl Ranker for Fixed {
    fn identity(&self) -> Identity<'_> {
        Identity {
            id: "test.fixed",
            version: "1",
            table_version: self.table,
        }
    }
    fn work_units(&self, _: &[Term<'_>], _: usize) -> usize {
        self.declared
    }
    fn rescore(
        &self,
        _: &[Term<'_>],
        candidates: &[Candidate<'_>],
        work: &mut WorkBudget,
    ) -> Result<Vec<f64>, Skip> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert!(candidates.iter().all(|c| c.writer == "alice"));
        // Deliberately ignore a failed charge to prove the pipeline still discards output.
        let _ = work.charge(self.charge);
        if self.fail {
            Err(Skip::Failed)
        } else {
            Ok(self.deltas.clone())
        }
    }
}
fn input() -> Vec<Candidate<'static>> {
    vec![
        Candidate {
            id: "z",
            writer: "alice",
            time: 1,
            lexical_bits: -0.0,
        },
        Candidate {
            id: "b",
            writer: "alice",
            time: 2,
            lexical_bits: 2.0,
        },
        Candidate {
            id: "a",
            writer: "alice",
            time: 2,
            lexical_bits: 1.0,
        },
    ]
}
fn query() -> Vec<Term<'static>> {
    vec![Term {
        text: "synthetic",
        repetitions: NonZeroU32::new(1).unwrap(),
    }]
}
fn run<'a>(rows: &[Candidate<'a>], ranker: &dyn Ranker) -> Ranked<'a> {
    rescore(
        "alice",
        &query(),
        rows,
        &[Stage {
            ranker,
            weight: 1.0,
            enabled: true,
        }],
        Limits::default(),
    )
    .unwrap()
}
fn exact_lexical(rows: &[Candidate<'_>], result: &Ranked<'_>) {
    assert_eq!(rows.len(), result.hits.len());
    for (row, hit) in rows.iter().zip(&result.hits) {
        assert_eq!(row, &hit.candidate);
        assert_eq!(row.lexical_bits.to_bits(), hit.final_bits.to_bits());
    }
}

#[test]
fn disabled_is_exact_and_does_not_call_ranker() {
    let rows = input();
    let ranker = Fixed::new(vec![100.0; rows.len()]);
    let result = rescore(
        "alice",
        &query(),
        &rows,
        &[Stage {
            ranker: &ranker,
            weight: f64::NAN,
            enabled: false,
        }],
        Limits::default(),
    )
    .unwrap();
    exact_lexical(&rows, &result);
    assert!(result.receipts.is_empty());
    assert_eq!(ranker.calls.load(Ordering::Relaxed), 0);
    exact_lexical(
        &rows,
        &rescore("alice", &query(), &rows, &[], Limits::default()).unwrap(),
    );
}

#[test]
fn finite_fusion_is_deterministic_with_time_then_text_id_ties() {
    let rows = input();
    let ranker = Fixed::new(vec![2.0, 0.0, 1.0]);
    let first = run(&rows, &ranker);
    let second = run(&rows, &ranker);
    assert_eq!(first, second);
    assert_eq!(
        first
            .hits
            .iter()
            .map(|h| h.candidate.id)
            .collect::<Vec<_>>(),
        ["a", "b", "z"]
    );
    assert_eq!(
        first.receipts[0].table_version.as_deref(),
        Some("synthetic-1")
    );
    assert!(first.receipts[0].skipped.is_none());
}

#[test]
fn invalid_outputs_skip_whole_stage_and_keep_lexical_exact() {
    let rows = input();
    for deltas in [
        vec![],
        vec![0.0; 4],
        vec![1.0, f64::NAN, 1.0],
        vec![1.0, f64::INFINITY, 1.0],
        vec![1.0, f64::NEG_INFINITY, 1.0],
    ] {
        let expected = if deltas.len() != rows.len() {
            Skip::WrongLength
        } else {
            Skip::NonFinite
        };
        let result = run(&rows, &Fixed::new(deltas));
        exact_lexical(&rows, &result);
        assert_eq!(result.receipts[0].skipped, Some(expected));
        assert!(result.hits.iter().all(|h| h.deltas == [0.0]));
    }
}

#[test]
fn missing_table_failure_and_budgets_fail_open_without_partial_scores() {
    let rows = input();
    for reason in [Skip::MissingTable, Skip::Failed, Skip::WorkBudget] {
        let mut ranker = Fixed::new(vec![1.0; rows.len()]);
        match reason {
            Skip::MissingTable => ranker.table = None,
            Skip::Failed => ranker.fail = true,
            Skip::WorkBudget => ranker.charge = ranker.declared + 1,
            _ => unreachable!(),
        }
        let result = run(&rows, &ranker);
        exact_lexical(&rows, &result);
        assert_eq!(result.receipts[0].skipped, Some(reason));
    }
    let mut ranker = Fixed::new(vec![1.0; rows.len()]);
    ranker.declared = Limits::default().max_work_per_stage + 1;
    let result = run(&rows, &ranker);
    exact_lexical(&rows, &result);
    assert_eq!(result.receipts[0].skipped, Some(Skip::WorkBudget));
    assert_eq!(ranker.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn later_invalid_stage_cannot_erase_a_previous_valid_stage() {
    let rows = input();
    let valid = Fixed::new(vec![2.0, 0.0, 1.0]);
    let invalid = Fixed::new(vec![1.0, f64::NAN, 1.0]);
    let result = rescore(
        "alice",
        &query(),
        &rows,
        &[
            Stage {
                ranker: &valid,
                weight: 1.0,
                enabled: true,
            },
            Stage {
                ranker: &invalid,
                weight: 1.0,
                enabled: true,
            },
        ],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .hits
            .iter()
            .map(|h| h.candidate.id)
            .collect::<Vec<_>>(),
        ["a", "b", "z"]
    );
    assert!(
        result
            .hits
            .iter()
            .all(|h| h.final_bits == 2.0 && h.deltas[1] == 0.0)
    );
    assert_eq!(result.receipts[1].skipped, Some(Skip::NonFinite));
}

#[test]
fn weighting_and_overflow_are_checked_before_commit() {
    let rows = input();
    for weight in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        let ranker = Fixed::new(vec![2.0; rows.len()]);
        let result = rescore(
            "alice",
            &query(),
            &rows,
            &[Stage {
                ranker: &ranker,
                weight,
                enabled: true,
            }],
            Limits::default(),
        )
        .unwrap();
        exact_lexical(&rows, &result);
        assert_eq!(
            result.receipts[0].skipped,
            Some(if weight.is_finite() {
                Skip::NonFinite
            } else {
                Skip::InvalidWeight
            })
        );
    }
    let ranker = Fixed::new(vec![1.0; rows.len()]);
    let result = rescore(
        "alice",
        &query(),
        &rows,
        &[Stage {
            ranker: &ranker,
            weight: 2.0,
            enabled: true,
        }],
        Limits::default(),
    )
    .unwrap();
    assert!(
        result
            .hits
            .iter()
            .all(|h| h.final_bits == h.candidate.lexical_bits + 2.0)
    );
}

#[test]
fn visibility_boundary_rejects_wrong_writer_duplicate_ids_and_invalid_scores() {
    let mut rows = input();
    rows[1].writer = "bob";
    assert_eq!(
        rescore("alice", &query(), &rows, &[], Limits::default()),
        Err(InputError::WrongWriter)
    );
    rows[1].writer = "alice";
    rows[1].id = rows[0].id;
    assert_eq!(
        rescore("alice", &query(), &rows, &[], Limits::default()),
        Err(InputError::DuplicateId)
    );
    rows[1].id = "b";
    rows[1].lexical_bits = f64::NAN;
    assert_eq!(
        rescore("alice", &query(), &rows, &[], Limits::default()),
        Err(InputError::NonFinite)
    );
}

#[test]
fn bounded_inputs_and_empty_candidates() {
    let rows = input();
    for limits in [
        Limits {
            max_candidates: 2,
            ..Limits::default()
        },
        Limits {
            max_query_terms: 0,
            ..Limits::default()
        },
        Limits {
            max_query_bytes: 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            rescore("alice", &query(), &rows, &[], limits),
            Err(InputError::Limit)
        );
    }
    let ranker = Fixed::new(vec![]);
    let stages = [Stage {
        ranker: &ranker,
        weight: 1.0,
        enabled: true,
    }];
    assert_eq!(
        rescore(
            "alice",
            &query(),
            &rows,
            &stages,
            Limits {
                max_stages: 0,
                ..Limits::default()
            }
        ),
        Err(InputError::Limit)
    );
    assert!(run(&[], &ranker).hits.is_empty());
}
