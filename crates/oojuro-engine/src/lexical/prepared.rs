//! One shared RAM scorer. The original path supplies no optional plan.
use super::*;
/// Engine-normalized terms and already-authorized original IDs; never wire authority.
pub(crate) struct Plan {
    pub terms: BTreeMap<String, usize>,
    pub extra_ids: BTreeSet<u64>,
}
pub(super) struct Options<'a> {
    pub split: bool,
    pub prune: bool,
    pub view: Option<&'a RecordVisibility<'a>>,
    pub original_ids: Option<&'a [String]>,
    pub plan: Option<&'a Plan>,
}
pub(super) fn rank(
    index: &Index,
    q: &Query<'_>,
    k: usize,
    options: Options<'_>,
) -> (Vec<(usize, Hit)>, bool) {
    let Options {
        split,
        prune,
        view,
        original_ids,
        plan,
    } = options;
    let literal = literal_query(q.text);
    let original = query_terms(q.text);
    let original_anchors: BTreeSet<_> = if plan.is_some() {
        original
            .keys()
            .filter(|term| {
                term.contains('_')
                    && index.postings.get(term.as_str()).is_some_and(|list| {
                        list.iter().any(|(row, _)| index.visible(*row as usize, q, view))
                    })
            })
            .cloned()
            .collect()
    } else {
        BTreeSet::new()
    };
    let mut query = plan.map(|p| p.terms.clone()).unwrap_or(original);
    let mut backoff = Vec::new();
    for (term, count) in &query {
        if term.contains('_')
            && (split
                || !index
                    .postings
                    .get(term)
                    .is_some_and(|list| list.iter().any(|(row, _)| index.visible(*row as usize, q, view))))
        {
            backoff.push((term.clone(), *count));
        }
    }
    for (term, count) in backoff {
        query.remove(&term);
        for (part, n) in query_terms(&term.replace('_', " ")) {
            *query.entry(part).or_default() += n * count;
        }
    }
    // Trace records have key = decimal event ID, recorded_at = valid_at.
    let key_row = |key: &str| {
        if let Some(view) = view {
            return view
                .keys
                .get(key)
                .and_then(|id| index.documents.iter().position(|d| d.event.id == *id));
        }
        index
            .documents
            .iter()
            .position(|d| d.event.id.to_string() == key && d.event.ts_us <= q.to)
    };
    let direct = literal
        .is_none()
        .then(|| {
            q.text
                .strip_prefix("key:")
                .map(key_row)
                .or_else(|| key_row(q.text).map(Some))
        })
        .flatten();
    let anchored = direct.is_none()
        && match plan {
            None => query.keys().any(|t| t.contains('_')),
            Some(_) => !split && !original_anchors.is_empty(),
        };
    let scope_model = direct.is_none() && !anchored && literal.is_none() && !q.text.is_empty();
    let scan_time = q.text.is_empty() || (literal.is_some() && query.is_empty());
    let terms: Vec<_> = query.iter().map(|(term, n)| (term, *n)).collect();
    let eligible = |row| {
        index.visible(row, q, view)
                // Record-only correction: a nonword literal has no postings
                // and scans text, but must still honor initial eligibility.
                // Empty timelines and actual-key reads retain stored-only rows.
                && (view.is_none()
                    || direct.is_some()
                    || q.text.is_empty()
                    || index.documents[row].event.indexed)
                && literal.is_none_or(|phrase| {
                    phrase_start(&index.documents[row].event.text, phrase).is_some()
                })
    };
    let mut candidates = BTreeSet::new();
    if let Some(row) = direct {
        if let Some(row) = row.filter(|row| eligible(*row)) {
            candidates.insert(row);
        }
    } else if scan_time {
        candidates.extend((0..index.documents.len()).filter(|row| eligible(*row)));
    } else {
        let rarest = literal.and_then(|_| {
            terms
                .iter()
                .enumerate()
                .min_by_key(|(_, (term, _))| index.postings.get(term.as_str()).map_or(0, Vec::len))
                .map(|(i, _)| i)
        });
        for (i, (term, _)) in terms.iter().enumerate() {
            let first = rarest.map_or(
                if plan.is_some() && anchored {
                    original_anchors.contains(term.as_str())
                } else {
                    !anchored || term.contains('_')
                },
                |r| r == i,
            );
            if first && let Some(list) = index.postings.get(term.as_str()) {
                candidates.extend(
                    list.iter()
                        .map(|(row, _)| *row as usize)
                        .filter(|row| eligible(*row)),
                );
            }
        }
    }
    if let Some(plan) = plan {
        for (row, doc) in index.documents.iter().enumerate() {
            if plan.extra_ids.contains(&doc.event.id)
                && eligible(row)
                && (!anchored
                    || original_anchors.iter().any(|term| {
                        index.postings.get(term).is_some_and(|list| {
                            list.binary_search_by_key(&row, |(r, _)| *r as usize).is_ok()
                        })
                    }))
            {
                candidates.insert(row);
            }
        }
    }
    let rows: Vec<_> = candidates.into_iter().collect();
    // Fixed term order then canonical row order reproduces v4 ChunkOut runs.
    let hits: Vec<Vec<(usize, u32)>> = if direct.is_some() || scan_time {
        vec![vec![]; terms.len()]
    } else {
        terms
            .iter()
            .map(|(term, _)| {
                index
                    .postings
                    .get(term.as_str())
                    .into_iter()
                    .flatten()
                    .filter_map(|(row, tf)| rows.binary_search(&(*row as usize)).ok().map(|i| (i, *tf)))
                    .collect()
            })
            .collect()
    };
    let retained: Vec<_> = hits.iter().map(|h| !h.is_empty()).collect();
    let collection: Vec<usize> = hits
        .iter()
        .map(|h| {
            if scope_model {
                h.len()
            } else {
                h.iter().map(|(_, tf)| *tf as usize).sum()
            }
        })
        .collect();
    let mass = |row: usize| {
        let d = &index.documents[row];
        if scope_model { d.scope } else { d.length }
    };
    let total = rows.iter().map(|row| u64::from(mass(*row))).sum::<u64>() as f64;
    let any_hits = retained.iter().any(|x| *x);
    let floor = FLOOR_RECORDS * MU;
    let pad = if any_hits && total < floor {
        let view_total = (0..index.documents.len())
            .filter(|row| index.visible(*row, q, view))
            .map(|row| u64::from(mass(row)))
            .sum::<u64>();
        let view: Vec<u64> = terms
            .iter()
            .enumerate()
            .map(|(i, (term, _))| {
                if !retained[i] {
                    return 0;
                }
                index
                    .postings
                    .get(term.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|(row, _)| index.visible(*row as usize, q, view))
                    .map(|(_, tf)| if scope_model { 1 } else { u64::from(*tf) })
                    .sum()
            })
            .collect();
        (view_total > 0).then_some((floor - total, view, view_total as f64))
    } else {
        None
    };
    let background: Vec<f64> = terms
        .iter()
        .enumerate()
        .map(|(i, (term, _))| match &pad {
            None => (collection[i] as f64 + literal_probability(term)) / (total + 1.0),
            Some((r, view, view_total)) => {
                (collection[i] as f64 + r * view[i] as f64 / view_total + literal_probability(term))
                    / (total + r + 1.0)
            }
        })
        .collect();
    let query_count = terms
        .iter()
        .enumerate()
        .filter(|(i, _)| retained[*i])
        .map(|(_, (_, count))| *count)
        .sum::<usize>() as f64;
    let mut gains: Vec<f64> = rows
        .iter()
        .map(|row| -query_count * (1. + mass(*row) as f64 / MU).log2())
        .collect();
    let score = |gains: &mut [f64], only: Option<&[bool]>| {
        for (ti, list) in hits.iter().enumerate() {
            let scale = MU * background[ti];
            let count = terms[ti].1 as f64;
            for (at, tf) in list {
                if only.is_some_and(|chosen| !chosen[*at]) {
                    continue;
                }
                let d = &index.documents[rows[*at]];
                let mass = if scope_model {
                    *tf as f64 * d.scope as f64 / d.length as f64
                } else {
                    *tf as f64
                };
                gains[*at] += count * (1. + mass / scale).log2();
            }
        }
    };
    let mut exact = vec![true; rows.len()];
    if !prune || k >= rows.len() {
        score(&mut gains, None);
    } else {
        // Port of v4's conservative term bounds, exact seeds and reach guard.
        let mut bound = gains.clone();
        for (ti, list) in hits.iter().enumerate() {
            let scale = MU * background[ti];
            let count = terms[ti].1 as f64;
            let once = count * log2_upper(1. + 1. / scale);
            for (at, tf) in list {
                bound[*at] += if *tf == 1 {
                    once
                } else {
                    count * log2_upper(1. + *tf as f64 / scale)
                };
            }
        }
        exact.fill(false);
        for at in best_indices(&bound, (2 * k).max(64)) {
            exact[at] = true;
        }
        score(&mut gains, Some(&exact));
        let mut seeded: Vec<_> = gains
            .iter()
            .enumerate()
            .filter(|(i, _)| exact[*i])
            .map(|(_, gain)| *gain + 0.0)
            .collect();
        seeded.sort_unstable_by(|a, b| b.total_cmp(a));
        let theta = seeded[k - 1];
        let reach = theta - 1e-9 * (1.0 + theta.abs());
        let mut more = vec![false; rows.len()];
        for at in 0..rows.len() {
            if !exact[at] && (bound[at] + 0.0).partial_cmp(&reach) != Some(std::cmp::Ordering::Less)
            {
                exact[at] = true;
                more[at] = true;
            }
        }
        score(&mut gains, Some(&more));
    }
    let mut result: Vec<_> = rows
        .into_iter()
        .enumerate()
        .filter(|(i, _)| exact[*i])
        .map(|(i, row)| (row, gains[i] + 0.0))
        .collect();
    // Selection and final order share the full score/time/textual-ID comparator.
    let compare = |(ar, a): &(usize, f64), (br, b): &(usize, f64)| {
        let ae = &index.documents[*ar].event;
        let be = &index.documents[*br].event;
        b.total_cmp(a)
            .then_with(|| be.ts_us.cmp(&ae.ts_us))
            .then_with(|| match original_ids {
                Some(ids) => ids[*ar].cmp(&ids[*br]),
                None => ae.id.to_string().cmp(&be.id.to_string()),
            })
    };
    if result.len() > k {
        result.select_nth_unstable_by(k, compare);
        result.truncate(k);
    }
    result.sort_by(compare);
    let result = result
        .into_iter()
        .map(|(row, bits)| {
            let e = &index.documents[row].event;
            (
                row,
                Hit {
                    id: e.id,
                    bits,
                    continued: false,
                    time: e.ts_us,
                    row: e.id,
                },
            )
        })
        .collect();
    (result, anchored && literal.is_none())
}
/// Enabled RAM pipeline only. Uses one candidate-pool F3 model per F2 group.
/// Extra IDs must be validated before this method; this also checks eligibility.
pub(crate) fn search(
    index: &Index,
    q: &Query<'_>,
    view: &RecordVisibility<'_>,
    plan: &Plan,
    k: usize,
) -> Result<Vec<Hit>> {
    if index.failed
        || q.writer != index.writer
        || q.agent != index.agent
        || q.text.len() > MAX_QUERY_BYTES
        || k > 100_000
    {
        return Err(Error::Invalid);
    }
    if k == 0 || q.from > q.to {
        return Ok(vec![]);
    }
    for id in &plan.extra_ids {
        let row = index
            .documents
            .iter()
            .position(|d| d.event.id == *id)
            .ok_or(Error::Authority)?;
        let e = &index.documents[row].event;
        if !index.visible(row, q, Some(view))
            || !e.indexed
            || literal_query(q.text).is_some_and(|literal| phrase_start(&e.text, literal).is_none())
        {
            return Err(Error::Authority);
        }
    }
    let (mut first, anchored) = rank(
        index,
        q,
        k,
        Options {
            split: false,
            prune: true,
            view: Some(view),
            original_ids: None,
            plan: Some(plan),
        },
    );
    if anchored && first.len() < k {
        let (rest, _) = rank(
            index,
            q,
            k,
            Options {
                split: true,
                prune: true,
                view: Some(view),
                original_ids: None,
                plan: Some(plan),
            },
        );
        let listed: BTreeSet<_> = first.iter().map(|(row, _)| *row).collect();
        for (row, mut hit) in rest {
            if first.len() == k {
                break;
            }
            if !listed.contains(&row) {
                hit.continued = true;
                first.push((row, hit));
            }
        }
    }
    Ok(first.into_iter().map(|(_, hit)| hit).collect())
}

pub(crate) fn original(
    index: &Index,
    q: &Query<'_>,
    view: &RecordVisibility<'_>,
    k: usize,
) -> Result<Vec<Hit>> {
    if index.failed
        || q.writer != index.writer
        || q.agent != index.agent
        || q.text.len() > MAX_QUERY_BYTES
        || k > 100_000
    {
        return Err(Error::Invalid);
    }
    if k == 0 || q.from > q.to {
        return Ok(vec![]);
    }
    Ok(index
        .continued_view(q, k, true, Some(view), None)
        .into_iter()
        .map(|(_, h)| h)
        .collect())
}
pub(crate) fn terms(text: &str) -> BTreeMap<String, usize> {
    query_terms(text)
}
pub(crate) fn expansion(text: &str) -> Result<String> {
    if text.is_empty() || text.len() > 256 || !text.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return Err(Error::Invalid);
    }
    let terms = query_terms(text);
    if terms.len() != 1 {
        return Err(Error::Invalid);
    }
    Ok(terms.into_keys().next().unwrap())
}
