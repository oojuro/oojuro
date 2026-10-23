//! Opt-in RAM rank pipeline. Optional code receives only the current visible writer view.
use super::{
    rank_tables::{self, LifecycleGuard, Loaded},
    *,
};
use crate::lexical::{self, prepared};
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
fn base(
    index: &Index,
    log: &RecordLog,
    view: &View,
    q: &Query<'_>,
    receipt: Receipt,
) -> Result<Search> {
    let result = index.search_view(log, view, q)?;
    let width = receipt.stages.len();
    let result = Search {
        watermark: result.watermark,
        receipt,
        hits: result
            .hits
            .into_iter()
            .map(|h| Hit {
                record: h.record,
                lexical_bits: h.bits,
                final_bits: h.bits,
                continued: h.continued,
                row: h.row,
                deltas: vec![0.0; width],
            })
            .collect(),
    };
    json(&result, q.max_bytes)?;
    Ok(result)
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
    let mut receipt = Receipt {
        policy: "original-pool-v1/f2-original-v1".into(),
        stages: vec![],
    };
    if q.k == 0 || !pipeline.enabled || !pipeline.stages.iter().any(|s| s.enabled) {
        return base(index, log, view, q, receipt);
    }
    if pipeline.stages.len() > 8
        || pipeline.window == 0
        || pipeline.window > 1000
        || pipeline.max_extra > 1000
        || pipeline.max_work > 1_000_000
    {
        receipt.stages.push(StageReceipt {
            hook: "pipeline".into(),
            id: "configuration".into(),
            version: "1".into(),
            table_version: None,
            weight: 0.0,
            skipped: reason("bounds"),
        });
        return base(index, log, view, q, receipt);
    }
    // Validate ordinary index/scope/current View even when no optional stage succeeds.
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
    if q.as_of != view.as_of()
        || q.k > 10_000
        || q.text.len() > lexical::MAX_QUERY_BYTES
        || q.max_bytes == 0
        || q.max_bytes > MAX_OUTPUT_BYTES
    {
        return Err(Error::Bounds);
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
        .filter(|r| {
            visible.contains(&r.id)
                && r.input.indexed
                && r.input.valid_at >= q.as_of.from
                && r.input.valid_at <= q.as_of.valid_at
        })
        .collect::<Vec<_>>();
    let ids = rows.iter().map(|r| r.id.to_string()).collect::<Vec<_>>();
    let context = rows
        .iter()
        .zip(&ids)
        .map(|(r, id)| rank::Visible {
            id,
            writer: q.scope.writer,
            time: r.input.valid_at,
            text: &r.input.text,
        })
        .collect::<Vec<_>>();
    let allowed = rows.iter().map(|r| r.id).collect::<BTreeSet<_>>();
    let original = prepared::terms(q.text);
    let mut query = original.clone();
    let mut extra = BTreeSet::new();
    let mut changed = false;
    let mut rescoring = vec![];
    let unsupported = q.text.trim().is_empty()
        || q.text.trim().starts_with('"')
        || q.text.starts_with("key:")
        || q.text.parse::<u64>().is_ok();
    let mut last_hook = 0u8;
    for stage in pipeline.stages.iter().filter(|s| s.enabled) {
        let id = stage.ranker.identity();
        let mut item = StageReceipt {
            hook: stage.hook.name().into(),
            id: clean(id.id),
            version: clean(id.version),
            table_version: id.table_version.map(clean),
            weight: if stage.weight.is_finite() {
                stage.weight
            } else {
                0.0
            },
            skipped: None,
        };
        let position = receipt.stages.len();
        let ordinal = match stage.hook {
            Hook::Expand => 0,
            Hook::Candidates => 1,
            Hook::Rescore => 2,
        };
        let checked = (|| -> std::result::Result<&Loaded, &'static str> {
            if !label(id.id) || !label(id.version) || id.table_version.is_none_or(|v| !label(v)) {
                return Err("identity");
            }
            if !stage.weight.is_finite() || (stage.hook != Hook::Rescore && stage.weight != 1.0) {
                return Err("weight");
            }
            if ordinal < last_hook {
                return Err("hook_order");
            }
            let table = stage.table.ok_or("missing_table")?;
            if table.id != id.id
                || table.version != id.version
                || Some(table.table_version.as_str()) != id.table_version
            {
                return Err("table_identity");
            }
            rank_tables::validate_loaded(log, view, &q.filter, guard, table)
                .map_err(|_| "stale_table")?;
            Ok(table)
        })();
        let table = match checked {
            Ok(t) => t,
            Err(e) => {
                item.skipped = reason(e);
                receipt.stages.push(item);
                continue;
            }
        };
        item.table_version = Some(table.content_version.clone());
        last_hook = ordinal;
        if stage.hook == Hook::Rescore {
            rescoring.push((stage, table, position));
            receipt.stages.push(item);
            continue;
        }
        if unsupported {
            item.skipped = reason("literal_or_direct_query");
            receipt.stages.push(item);
            continue;
        }
        let old_query = query.clone();
        let old_extra = extra.clone();
        let old_changed = changed;
        let query_terms = terms(&query)?;
        if query_terms.len() > 256 {
            item.skipped = reason("query_bound");
            receipt.stages.push(item);
            continue;
        }
        let declared = stage.ranker.work_units(&query_terms, rows.len());
        if declared > pipeline.max_work {
            item.skipped = reason("work_budget");
            receipt.stages.push(item);
            continue;
        }
        let mut work = rank::WorkBudget::new(declared);
        let cx = rank::Context {
            visible: &context,
            table: &table.payload,
        };
        let succeeded = match stage.hook {
            Hook::Expand => match stage.ranker.expand(&query_terms, &cx, &mut work) {
                Ok(values) if !values.is_empty() && values.len() <= 256 && !work.exhausted() => {
                    let mut next = query.clone();
                    let mut valid = true;
                    for value in values {
                        let term = match prepared::expansion(&value.text) {
                            Ok(t) => t,
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        };
                        let n = next.entry(term).or_default();
                        match n.checked_add(value.repetitions.get() as usize) {
                            Some(v) if v <= 1_000_000 => *n = v,
                            _ => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    if valid
                        && next.len() <= 256
                        && next.keys().map(String::len).sum::<usize>() <= 65536
                        && next.values().sum::<usize>() <= 1_000_000
                    {
                        changed |= next != query;
                        query = next;
                        true
                    } else {
                        item.skipped = reason("invalid_expansion");
                        false
                    }
                }
                Ok(values) if values.is_empty() && !work.exhausted() => {
                    item.skipped = reason("empty");
                    false
                }
                _ => {
                    item.skipped = reason("output_or_work_bound");
                    false
                }
            },
            Hook::Candidates => {
                match stage
                    .ranker
                    .candidates(&query_terms, &cx, pipeline.max_extra, &mut work)
                {
                    Ok(values)
                        if !values.is_empty()
                            && values.len() <= pipeline.max_extra
                            && !work.exhausted() =>
                    {
                        let mut next = extra.clone();
                        let mut seen = BTreeSet::new();
                        let mut valid = true;
                        for value in values {
                            let id = match value.parse::<u64>() {
                                Ok(n) if n.to_string() == value => n,
                                _ => {
                                    valid = false;
                                    break;
                                }
                            };
                            if !allowed.contains(&id) || !seen.insert(id) {
                                valid = false;
                                break;
                            }
                            next.insert(id);
                        }
                        if valid && next.len() <= pipeline.max_extra {
                            changed |= next != extra;
                            extra = next;
                            true
                        } else {
                            item.skipped = reason("foreign_duplicate_or_bound");
                            false
                        }
                    }
                    Ok(values) if values.is_empty() && !work.exhausted() => {
                        item.skipped = reason("empty");
                        false
                    }
                    _ => {
                        item.skipped = reason("output_or_work_bound");
                        false
                    }
                }
            }
            Hook::Rescore => unreachable!(),
        };
        if succeeded {
            let latest = stage.ranker.identity();
            if latest.id != id.id
                || latest.version != id.version
                || latest.table_version != id.table_version
            {
                query = old_query;
                extra = old_extra;
                changed = old_changed;
                item.skipped = reason("identity_changed");
            }
        }
        receipt.stages.push(item);
    }
    if changed {
        receipt.policy = "expanded-pool-v1/f2-original-v1".into();
    }
    let query_model = lexical::Query {
        writer: q.scope.writer,
        agent: q.scope.owner,
        text: q.text,
        from: q.as_of.from,
        to: q.as_of.valid_at,
        k: q.k,
    };
    // Window plus the complete possible final-k tail; no unrescored contender is dropped.
    let want = pipeline.window.saturating_add(q.k).min(MAX_RECORDS);
    let lexical = if changed {
        prepared::search(
            &index.inner,
            &query_model,
            &mask,
            &prepared::Plan {
                terms: query.clone(),
                extra_ids: extra,
            },
            want,
        )
    } else {
        prepared::original(&index.inner, &query_model, &mask, want)
    }
    .map_err(|_| Error::Unavailable)?;
    let width = receipt.stages.len();
    let all = lexical
        .into_iter()
        .map(|h| (h.id, (h, vec![0.0; width])))
        .collect::<Vec<_>>();
    let mut hits = all;
    let window = pipeline.window.min(hits.len());
    let names = hits[..window]
        .iter()
        .map(|(id, _)| id.to_string())
        .collect::<Vec<_>>();
    let candidates = hits[..window]
        .iter()
        .zip(&names)
        .map(|((_, (h, _)), id)| rank::Candidate {
            id,
            writer: q.scope.writer,
            time: h.time,
            lexical_bits: h.bits,
        })
        .collect::<Vec<_>>();
    let mut final_bits = hits.iter().map(|(_, (h, _))| h.bits).collect::<Vec<_>>();
    let normalized = terms(&query)?;
    let mut touched = false;
    for (stage, table, position) in rescoring {
        rank_tables::validate_loaded(log, view, &q.filter, guard, table)?;
        if window == 0 {
            receipt.stages[position].skipped = reason("empty_window");
            continue;
        }
        let adapter = Adapter {
            ranker: stage.ranker,
            cx: rank::Context {
                visible: &context,
                table: &table.payload,
            },
        };
        let result = rank::rescore(
            q.scope.writer,
            &normalized,
            &candidates,
            &[rank::Stage {
                ranker: &adapter,
                weight: stage.weight,
                enabled: true,
            }],
            rank::Limits {
                max_candidates: pipeline.window,
                max_work_per_stage: pipeline.max_work,
                ..Default::default()
            },
        );
        match result {
            Ok(result) if result.receipts[0].skipped.is_none() => {
                let latest = stage.ranker.identity();
                if latest.id != receipt.stages[position].id
                    || latest.version != receipt.stages[position].version
                    || latest.table_version != Some(table.table_version.as_str())
                {
                    receipt.stages[position].skipped = reason("identity_changed");
                    continue;
                }
                let actual = &result.receipts[0];
                let expected = &receipt.stages[position];
                if actual.id != expected.id
                    || actual.version != expected.version
                    || actual.table_version != Some(table.table_version.clone())
                {
                    receipt.stages[position].skipped = reason("identity_changed");
                    continue;
                }
                let deltas = result
                    .hits
                    .iter()
                    .map(|h| (h.candidate.id, h.deltas[0]))
                    .collect::<BTreeMap<_, _>>();
                let values = names
                    .iter()
                    .map(|id| deltas[id.as_str()])
                    .collect::<Vec<_>>();
                if values
                    .iter()
                    .enumerate()
                    .any(|(i, d)| !(final_bits[i] + stage.weight * d).is_finite())
                {
                    receipt.stages[position].skipped = reason("non_finite");
                    continue;
                }
                for (i, delta) in values.into_iter().enumerate() {
                    final_bits[i] += stage.weight * delta;
                    hits[i].1.1[position] = delta;
                }
                touched = true;
            }
            Ok(result) => {
                receipt.stages[position].skipped =
                    reason(match result.receipts[0].skipped.unwrap() {
                        rank::Skip::MissingTable => "missing_table",
                        rank::Skip::InvalidIdentity => "identity",
                        rank::Skip::InvalidWeight => "weight",
                        rank::Skip::WorkBudget => "work_budget",
                        rank::Skip::Failed => "failed",
                        rank::Skip::WrongLength => "wrong_length",
                        rank::Skip::NonFinite => "non_finite",
                    })
            }
            Err(_) => receipt.stages[position].skipped = reason("invalid_input"),
        }
    }
    if !changed && !touched {
        return base(index, log, view, q, receipt);
    }
    let lookup = log
        .records
        .iter()
        .map(|r| (r.id, r))
        .collect::<BTreeMap<_, _>>();
    let mut output = hits
        .into_iter()
        .zip(final_bits)
        .map(|((id, (h, deltas)), final_bits)| {
            Ok(Hit {
                record: lookup
                    .get(&id)
                    .ok_or(Error::Authority)?
                    .record(q.scope.writer, &identity.adapter),
                lexical_bits: h.bits,
                final_bits,
                continued: h.continued,
                row: h.row,
                deltas,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    output.sort_by(|a, b| {
        a.continued
            .cmp(&b.continued)
            .then_with(|| b.final_bits.total_cmp(&a.final_bits))
            .then_with(|| b.record.valid_at.cmp(&a.record.valid_at))
            .then_with(|| a.record.id.to_string().cmp(&b.record.id.to_string()))
    });
    output.truncate(q.k);
    let result = Search {
        watermark: view.watermark().clone(),
        receipt,
        hits: output,
    };
    json(&result, q.max_bytes)?;
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
