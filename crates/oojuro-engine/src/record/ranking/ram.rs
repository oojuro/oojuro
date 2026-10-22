//! Shared post-visibility RAM pipeline; wrappers retain distinct real authority types.
use super::*;
pub(super) struct Source<'a, 'q> {
    pub index: &'a lexical::Index,
    pub rows: &'a [Record],
    pub mask: &'a lexical::RecordVisibility<'a>,
    pub query: &'a Query<'q>,
    pub validate: &'a dyn Fn(&Loaded) -> Result<()>,
    pub baseline: &'a dyn Fn() -> Result<Vec<super::super::Hit>>,
}
#[derive(Serialize)]
pub(super) struct Ranked {
    pub receipt: Receipt,
    pub hits: Vec<Hit>,
}
impl Ranked {
    pub(super) fn from_lexical(hits: Vec<super::super::Hit>, receipt: Receipt) -> Self {
        let width = receipt.stages.len();
        Self {
            receipt,
            hits: hits
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
        }
    }
}
fn base(source: &Source<'_, '_>, receipt: Receipt) -> Result<Ranked> {
    Ok(Ranked::from_lexical((source.baseline)()?, receipt))
}
pub(super) fn run(source: &Source<'_, '_>, pipeline: &Pipeline<'_>) -> Result<Ranked> {
    let q = source.query;
    let mut receipt = Receipt {
        policy: "original-pool-v1/f2-original-v1".into(),
        stages: vec![],
    };
    if q.k == 0 || !pipeline.enabled || !pipeline.stages.iter().any(|s| s.enabled) {
        return base(source, receipt);
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
        return base(source, receipt);
    }
    if q.k > 10_000
        || q.text.len() > lexical::MAX_QUERY_BYTES
        || q.max_bytes == 0
        || q.max_bytes > MAX_OUTPUT_BYTES
    {
        return Err(Error::Bounds);
    }
    let rows = source.rows.iter().filter(|r| r.indexed).collect::<Vec<_>>();
    let ids = rows.iter().map(|r| r.id.to_string()).collect::<Vec<_>>();
    let context = rows
        .iter()
        .zip(&ids)
        .map(|(r, id)| rank::Visible {
            id,
            writer: q.scope.writer,
            time: r.valid_at,
            text: &r.text,
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
            (source.validate)(table).map_err(|_| "stale_table")?;
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
        (source.validate)(table)?;
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
        (source.validate)(table)?;
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
            source.index,
            &query_model,
            source.mask,
            &prepared::Plan {
                terms: query.clone(),
                extra_ids: extra,
            },
            want,
        )
    } else {
        prepared::original(source.index, &query_model, source.mask, want)
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
        (source.validate)(table)?;
        if window == 0 {
            receipt.stages[position].skipped = reason("empty_window");
            continue;
        }
        let check = || (source.validate)(table);
        let adapter = Adapter {
            ranker: stage.ranker,
            cx: rank::Context {
                visible: &context,
                table: &table.payload,
            },
        };
        let result = rank::rescore_checked(
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
            || check().map_err(|_| rank::Skip::Failed),
        );
        check()?;
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
        return base(source, receipt);
    }
    let lookup = source
        .rows
        .iter()
        .map(|r| (r.id, r))
        .collect::<BTreeMap<_, _>>();
    let mut output = hits
        .into_iter()
        .zip(final_bits)
        .map(|((id, (h, deltas)), final_bits)| {
            Ok(Hit {
                record: (*lookup.get(&id).ok_or(Error::Authority)?).clone(),
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
    let result = Ranked {
        receipt,
        hits: output,
    };
    json(&result, q.max_bytes)?;
    Ok(result)
}
