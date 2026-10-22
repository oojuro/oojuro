//! Opt-in RAM rank pipeline. Optional code receives only the current visible writer view.
use super::{
    rank_tables::{self, LifecycleGuard, Loaded},
    *,
};
use crate::lexical::{self, prepared};
mod managed;
mod ram;
pub use managed::ManagedSearch;
use oojuro_rank as rank;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    Expand,
    Candidates,
    Rescore,
}
impl Hook {
    fn name(self) -> &'static str {
        match self {
            Self::Expand => "expand",
            Self::Candidates => "candidates",
            Self::Rescore => "rescore",
        }
    }
}
pub struct Stage<'a> {
    pub hook: Hook,
    pub ranker: &'a dyn rank::Ranker,
    pub table: Option<&'a Loaded>,
    pub weight: f64,
    pub enabled: bool,
}
pub struct Pipeline<'a> {
    pub enabled: bool,
    pub stages: &'a [Stage<'a>],
    pub window: usize,
    pub max_extra: usize,
    pub max_work: usize,
}
impl Default for Pipeline<'_> {
    fn default() -> Self {
        Self {
            enabled: false,
            stages: &[],
            window: 1000,
            max_extra: 256,
            max_work: 1_000_000,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct StageReceipt {
    pub hook: String,
    pub id: String,
    pub version: String,
    pub table_version: Option<String>,
    pub weight: f64,
    pub skipped: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    pub policy: String,
    pub stages: Vec<StageReceipt>,
}
#[derive(Serialize)]
pub struct Hit {
    pub record: Record,
    pub lexical_bits: f64,
    pub final_bits: f64,
    pub continued: bool,
    pub row: u64,
    pub deltas: Vec<f64>,
}
#[derive(Serialize)]
pub struct Search {
    pub watermark: Watermark,
    pub receipt: Receipt,
    pub hits: Vec<Hit>,
}
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
}
fn clean(s: &str) -> String {
    if label(s) { s.into() } else { "invalid".into() }
}
fn reason(reason: &str) -> Option<String> {
    Some(reason.into())
}
fn terms(query: &BTreeMap<String, usize>) -> Result<Vec<rank::Term<'_>>> {
    query
        .iter()
        .map(|(text, n)| {
            Ok(rank::Term {
                text,
                repetitions: NonZeroU32::new(u32::try_from(*n).map_err(|_| Error::Bounds)?)
                    .ok_or(Error::Bounds)?,
            })
        })
        .collect()
}
struct Adapter<'a, 'b> {
    ranker: &'a dyn rank::Ranker,
    cx: rank::Context<'b>,
}
impl rank::Ranker for Adapter<'_, '_> {
    fn identity(&self) -> rank::Identity<'_> {
        self.ranker.identity()
    }
    fn work_units(&self, q: &[rank::Term<'_>], n: usize) -> usize {
        self.ranker.work_units(q, n)
    }
    fn rescore(
        &self,
        q: &[rank::Term<'_>],
        c: &[rank::Candidate<'_>],
        w: &mut rank::WorkBudget,
    ) -> std::result::Result<Vec<f64>, rank::Skip> {
        self.ranker.rescore_context(q, c, &self.cx, w)
    }
}
/// Guard checks are mandatory before and after delivery. A supplied binding is not authority.
/// The caller's managed guard must independently check retained current lifecycle control.
pub fn search(
    index: &Index,
    log: &RecordLog,
    view: &View,
    q: &Query<'_>,
    guard: &dyn LifecycleGuard,
    pipeline: &Pipeline<'_>,
) -> Result<Search> {
    let before = guard.current()?;
    let result = search_inner(index, log, view, q, guard, pipeline)?;
    if guard.current()? != before {
        return Err(Error::Authority);
    }
    Ok(result)
}
fn search_inner(
    index: &Index,
    log: &RecordLog,
    view: &View,
    q: &Query<'_>,
    guard: &dyn LifecycleGuard,
    pipeline: &Pipeline<'_>,
) -> Result<Search> {
    // Disabled ordinary queries retain the original direct path without cloning the visible log.
    if q.k == 0 || !pipeline.enabled || !pipeline.stages.iter().any(|s| s.enabled) {
        let original = index.search_view(log, view, q)?;
        return finish(
            original.watermark,
            ram::Ranked::from_lexical(
                original.hits,
                Receipt {
                    policy: "original-pool-v1/f2-original-v1".into(),
                    stages: vec![],
                },
            ),
            q.max_bytes,
        );
    }
    if index.failed {
        return Err(Error::Unavailable);
    }
    let tip = log.tip()?;
    log.validate(&index.mark)?;
    if tip.sequence != index.mark.sequence || tip.records != index.mark.records {
        return Err(Error::Unavailable);
    }
    let identity = log.identity();
    if q.scope.writer != identity.writer || q.scope.owner != identity.agent {
        return Err(Error::Scope);
    }
    if q.as_of != view.as_of() {
        return Err(Error::Invalid);
    }
    let visible = view.visible(log, &q.filter)?;
    let keys = view.keys(log)?;
    let mask = lexical::RecordVisibility {
        ids: &visible,
        keys: &keys,
    };
    let rows = log
        .records
        .iter()
        .filter(|r| visible.contains(&r.id))
        .map(|r| r.record(q.scope.writer, &identity.adapter))
        .collect::<Vec<_>>();
    let validate =
        |table: &Loaded| rank_tables::validate_loaded(log, view, &q.filter, guard, table);
    let baseline = || index.search_view(log, view, q).map(|result| result.hits);
    let ranked = ram::run(
        &ram::Source {
            index: &index.inner,
            rows: &rows,
            mask: &mask,
            query: q,
            validate: &validate,
            baseline: &baseline,
        },
        pipeline,
    )?;
    finish(view.watermark().clone(), ranked, q.max_bytes)
}
fn finish(watermark: Watermark, ranked: ram::Ranked, max_bytes: usize) -> Result<Search> {
    let result = Search {
        watermark,
        receipt: ranked.receipt,
        hits: ranked.hits,
    };
    json(&result, max_bytes)?;
    Ok(result)
}
impl Index {
    pub fn search_ranked(
        &self,
        log: &RecordLog,
        view: &View,
        query: &Query<'_>,
        guard: &dyn LifecycleGuard,
        pipeline: &Pipeline<'_>,
    ) -> Result<Search> {
        search(self, log, view, query, guard, pipeline)
    }
}
#[cfg(test)]
#[path = "ranking_tests.rs"]
mod tests;
