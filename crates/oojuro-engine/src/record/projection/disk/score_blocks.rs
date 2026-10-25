//! Optional exact pruning under concrete numeric shapes. Failure to obtain a
//! tight bounded proof falls back to the complete scorer, never an approximation.
use super::*;
const EXAMINED_ROWS: usize = 16_384;
impl Index {
    pub(super) fn rank_compact_blocks(
        &self,
        model: &scorer::Model,
        selection: &Selection,
        limit: usize,
        literal: Option<&[u64]>,
    ) -> Result<Option<Top>> {
        let Some(mass) = self.mass.as_ref().filter(|_| {
            (model.plan.scope_model() || (model.plan.literal.is_some() && literal.is_some()))
                && model.plan.terms.len() <= 16
                && self.manifest.rows > 4096
        }) else {
            return Ok(None);
        };
        let mut lists = Vec::with_capacity(model.plan.terms.len());
        for (term, _) in &model.plan.terms {
            let Some(list) = self
                .terms
                .get(self, *blake3::hash(term.as_bytes()).as_bytes())?
            else {
                return Ok(None);
            };
            lists.push(list);
        }
        let mut order: Vec<_> = (0..mass.shapes.len()).collect();
        order.sort_unstable_by(|a, b| {
            mass.shapes[*b]
                .time
                .cmp(&mass.shapes[*a].time)
                .then_with(|| mass.shapes[*a].label.cmp(&mass.shapes[*b].label))
        });
        let mut top = Top::new(limit);
        let mut tf = vec![0u32; lists.len()];
        let mut maximum_mass = vec![0.0f64; lists.len()];
        let mut scores = compact::Scores::new(lists.len()).unwrap();
        let mut examined = 0;
        for block in order {
            let base = u32::try_from(block * 256).map_err(|_| Error::Bounds)?;
            for (at, list) in lists.iter().enumerate() {
                tf[at] = list.pairs.maximum(base).ok_or(Error::Authority)?;
                maximum_mass[at] = list
                    .pairs
                    .maximum_scope_mass(base)
                    .ok_or(Error::Authority)?;
            }
            if !lists.is_empty() && tf.iter().all(|tf| *tf == 0) {
                continue;
            }
            if literal.is_some_and(|bits| {
                (block * 4..((block + 1) * 4).min(bits.len())).all(|at| bits[at] == 0)
            }) {
                continue;
            }
            let shape = &mass.shapes[block];
            let shaped = compact::shape_bound(model, shape.values(), &tf);
            let scoped = if model.plan.scope_model() {
                model.scope_bound(shape.minimum_scope, &maximum_mass)
            } else {
                model.literal_bound(mass.minimum_length(base)?, &tf)
            };
            let bound = match (shaped, scoped) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                _ => None,
            };
            if let Some(bound) = bound
                && !top.admits_bound(bound, shape.time, shape.label)
            {
                continue;
            }
            for word in block * 4..((block + 1) * 4).min(mass.live.len()) {
                let mut active = mass.live[word] & literal.map_or(u64::MAX, |bits| bits[word]);
                while active != 0 {
                    let bit = active.trailing_zeros();
                    active &= active - 1;
                    examined += 1;
                    if examined > EXAMINED_ROWS {
                        return Ok(None);
                    }
                    let ordinal =
                        u32::try_from(word * 64 + bit as usize).map_err(|_| Error::Bounds)?;
                    let column = self.column(ordinal)?;
                    if !column.visible(selection) {
                        continue;
                    }
                    for (at, list) in lists.iter().enumerate() {
                        tf[at] = list.pairs.tf(ordinal).ok_or(Error::Authority)?;
                    }
                    if !model
                        .plan
                        .candidate_terms(u64::from(ordinal), column.indexed(), true, &tf)
                    {
                        continue;
                    }
                    top.push(Scored {
                        ordinal,
                        label: column.label,
                        time: column.valid_at,
                        bits: scores.score(model, column.length, column.scope, &tf),
                    });
                }
            }
        }
        Ok(Some(top))
    }
}
