//! Optional, writer-scoped rescoring foundation. Nothing invokes this from the engine yet.
//!
//! The caller supplies candidates after authorization, time/version filtering and erasure.
//! Rankers see only that bounded slice and cannot insert rows. Implementations are trusted,
//! deterministic, in-process code: the cooperative work budget is not a sandbox or timeout.

use std::{collections::BTreeSet, num::NonZeroU32};

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate<'a> {
    pub id: &'a str,
    pub writer: &'a str,
    pub time: i64,
    pub lexical_bits: f64,
}

#[derive(Clone, Debug)]
pub struct Term<'a> {
    pub text: &'a str,
    pub repetitions: NonZeroU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub table_version: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    MissingTable,
    InvalidIdentity,
    InvalidWeight,
    WorkBudget,
    Failed,
    WrongLength,
    NonFinite,
}

/// Cooperative units chosen and documented by each ranker, e.g. term/candidate pairs.
/// Charge before work. Even if a ranker ignores an error, its output will be discarded.
#[derive(Debug)]
pub struct WorkBudget {
    remaining: usize,
    exhausted: bool,
}

impl WorkBudget {
    pub fn charge(&mut self, units: usize) -> Result<(), Skip> {
        if let Some(remaining) = self.remaining.checked_sub(units) {
            self.remaining = remaining;
            Ok(())
        } else {
            self.exhausted = true;
            Err(Skip::WorkBudget)
        }
    }
}

pub trait Ranker: Send + Sync {
    /// Changing the implementation or table contents must change its version.
    fn identity(&self) -> Identity<'_>;
    /// Declared worst-case work for this query and bounded candidate slice.
    fn work_units(&self, query: &[Term<'_>], candidates: usize) -> usize;
    /// One delta in bits per candidate, in the same order. No network, clock,
    /// randomness, writes or data belonging to another writer are permitted.
    fn rescore(
        &self,
        query: &[Term<'_>],
        candidates: &[Candidate<'_>],
        work: &mut WorkBudget,
    ) -> Result<Vec<f64>, Skip>;
}

pub struct Stage<'a> {
    pub ranker: &'a dyn Ranker,
    pub weight: f64,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_candidates: usize,
    pub max_query_terms: usize,
    pub max_query_bytes: usize,
    pub max_stages: usize,
    pub max_work_per_stage: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_candidates: 1_000,
            max_query_terms: 256,
            max_query_bytes: 65_536,
            max_stages: 8,
            max_work_per_stage: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Limit,
    WrongWriter,
    DuplicateId,
    NonFinite,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit<'a> {
    pub candidate: Candidate<'a>,
    pub final_bits: f64,
    /// Raw deltas in receipt order; skipped stages contribute zero.
    pub deltas: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Receipt {
    pub weight: f64,
    pub id: String,
    pub version: String,
    pub table_version: Option<String>,
    pub skipped: Option<Skip>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ranked<'a> {
    pub hits: Vec<Hit<'a>>,
    /// Enabled stages only, in configured order. Disabled rankers are never called.
    pub receipts: Vec<Receipt>,
}

/// Rescore one writer's already-visible lexical top-N. Limits reject oversized input;
/// the caller must choose N before invoking this foundation. Invalid plugin output
/// skips the entire plugin transactionally, retaining all earlier valid stages.
///
/// With no successful stage, input order and lexical score bits are preserved exactly.
/// On success, order is score descending, time descending, then id ascending as text.
/// Each stage sees the original lexical candidates, regardless of earlier adjustments.
pub fn rescore<'a>(
    writer: &str,
    query: &[Term<'_>],
    lexical: &[Candidate<'a>],
    stages: &[Stage<'_>],
    limits: Limits,
) -> Result<Ranked<'a>, InputError> {
    if lexical.len() > limits.max_candidates
        || query.len() > limits.max_query_terms
        || stages.len() > limits.max_stages
        || query
            .iter()
            .try_fold(0usize, |n, t| n.checked_add(t.text.len()))
            .is_none_or(|n| n > limits.max_query_bytes)
    {
        return Err(InputError::Limit);
    }
    let mut ids = BTreeSet::new();
    for candidate in lexical {
        if candidate.writer != writer {
            return Err(InputError::WrongWriter);
        }
        if !candidate.lexical_bits.is_finite() {
            return Err(InputError::NonFinite);
        }
        if !ids.insert(candidate.id) {
            return Err(InputError::DuplicateId);
        }
    }
    let mut result = Ranked {
        hits: lexical
            .iter()
            .map(|candidate| Hit {
                candidate: candidate.clone(),
                final_bits: candidate.lexical_bits,
                deltas: Vec::new(),
            })
            .collect(),
        receipts: Vec::new(),
    };
    let mut touched = false;
    for stage in stages.iter().filter(|s| s.enabled) {
        let identity = stage.ranker.identity();
        let mut receipt = Receipt {
            weight: stage.weight,
            id: identity.id.into(),
            version: identity.version.into(),
            table_version: identity.table_version.map(Into::into),
            skipped: None,
        };
        let deltas = (|| {
            if identity.id.is_empty()
                || identity.version.is_empty()
                || identity.table_version == Some("")
            {
                return Err(Skip::InvalidIdentity);
            }
            if !stage.weight.is_finite() {
                return Err(Skip::InvalidWeight);
            }
            if identity.table_version.is_none() {
                return Err(Skip::MissingTable);
            }
            let declared = stage.ranker.work_units(query, lexical.len());
            if declared > limits.max_work_per_stage {
                return Err(Skip::WorkBudget);
            }
            let mut work = WorkBudget {
                remaining: declared,
                exhausted: false,
            };
            let deltas = stage.ranker.rescore(query, lexical, &mut work)?;
            if work.exhausted {
                return Err(Skip::WorkBudget);
            }
            if deltas.len() != lexical.len() {
                return Err(Skip::WrongLength);
            }
            if deltas.iter().zip(&result.hits).any(|(delta, hit)| {
                !delta.is_finite() || !(hit.final_bits + stage.weight * delta).is_finite()
            }) {
                return Err(Skip::NonFinite);
            }
            Ok(deltas)
        })();
        match deltas {
            Ok(deltas) => {
                touched = true;
                for (hit, delta) in result.hits.iter_mut().zip(deltas) {
                    hit.final_bits += stage.weight * delta;
                    hit.deltas.push(delta);
                }
            }
            Err(reason) => {
                receipt.skipped = Some(reason);
                for hit in &mut result.hits {
                    hit.deltas.push(0.0);
                }
            }
        }
        result.receipts.push(receipt);
    }
    if touched {
        result.hits.sort_by(|a, b| {
            // Finite validation above makes numeric comparison total, including signed zero.
            b.final_bits
                .partial_cmp(&a.final_bits)
                .unwrap()
                .then_with(|| b.candidate.time.cmp(&a.candidate.time))
                .then_with(|| a.candidate.id.cmp(b.candidate.id))
        });
    }
    Ok(result)
}
