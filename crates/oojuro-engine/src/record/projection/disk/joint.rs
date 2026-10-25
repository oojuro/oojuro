//! Bounded exact numeric groups for repeated unfiltered scope-model queries.
//! Statistics/model arithmetic and source/original checks remain independent.
use super::*;

pub(super) const TOP: usize = 20;
pub(super) const BYTES: usize = 5 << 20;
const MAX_GROUPS: usize = 8192;
pub(super) struct Group {
    pub shape: Vec<u32>,
    pub top: Vec<term_cache::GroupHit>,
    pub rows: u64,
}
pub(super) struct Groups {
    pub terms: Vec<[u8; 32]>,
    pub groups: Vec<Group>,
    pub complete: bool,
    bytes: usize,
}
impl Groups {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    fn base_bytes(terms: usize) -> usize {
        512 + std::mem::size_of::<Self>() + terms * 32
    }
    fn per_group(terms: usize) -> usize {
        std::mem::size_of::<Group>()
            + (terms + 2) * 4
            + TOP * std::mem::size_of::<term_cache::GroupHit>()
            + 128
    }
    pub fn estimate(index: &Index, plan: &scorer::Plan) -> usize {
        (Self::base_bytes(plan.terms.len())
            + (index.manifest.rows as usize).min(MAX_GROUPS) * Self::per_group(plan.terms.len()))
        .min(BYTES)
    }
    pub fn build(
        index: &Index,
        plan: &scorer::Plan,
        budget: usize,
        literal: Option<&[u64]>,
    ) -> Result<Self> {
        if !(plan.scope_model() && literal.is_none() || plan.literal.is_some() && literal.is_some())
            || plan.terms.len() > 16
        {
            return Err(Error::Invalid);
        }
        if literal.is_some_and(|bits| bits.len() != (index.manifest.rows as usize).div_ceil(64)) {
            return Err(Error::Authority);
        }
        let terms: Vec<_> = plan
            .terms
            .iter()
            .map(|(term, _)| *blake3::hash(term.as_bytes()).as_bytes())
            .collect();
        let base = Self::base_bytes(terms.capacity());
        if base > budget {
            return Err(Error::Bounds);
        }
        let mut table =
            lexical::FastMap::<Vec<u32>, (u64, BinaryHeap<term_cache::GroupHit>)>::default();
        let mut shape = Vec::with_capacity(terms.len() + 2);
        let mut rejected = false;
        let walked = index.walk(plan, None, true, None, |ordinal, tf| {
            let column = index.column(ordinal)?;
            if literal.is_some_and(|bits| bits[ordinal as usize / 64] & (1 << (ordinal % 64)) == 0)
                || !plan.candidate_terms(u64::from(ordinal), column.indexed(), column.live(), tf)
            {
                return Ok(());
            }
            shape.clear();
            shape.extend([column.length, column.scope]);
            shape.extend(tf);
            if !table.contains_key(shape.as_slice()) {
                if table.len() >= MAX_GROUPS
                    || base + (table.len() + 1) * Self::per_group(terms.len()) > budget
                {
                    rejected = true;
                    return Err(Error::Bounds);
                }
                table.insert(shape.clone(), (0, BinaryHeap::with_capacity(TOP)));
            }
            let (count, top) = table.get_mut(shape.as_slice()).ok_or(Error::Authority)?;
            *count = count.checked_add(1).ok_or(Error::Bounds)?;
            let hit = term_cache::GroupHit {
                ordinal,
                time: column.valid_at,
                label: column.label,
            };
            if top.len() < TOP {
                top.push(hit);
            } else if top.peek().is_some_and(|worst| hit < *worst) {
                top.pop();
                top.push(hit);
            }
            Ok(())
        });
        if rejected {
            return Ok(Self {
                terms,
                groups: vec![],
                complete: false,
                bytes: base,
            });
        }
        walked?;
        let mut groups = Vec::with_capacity(table.len());
        for (shape, (rows, top)) in table {
            groups.push(Group {
                shape,
                top: top.into_vec(),
                rows,
            });
        }
        let bytes = base
            + groups.capacity() * std::mem::size_of::<Group>()
            + groups
                .iter()
                .map(|group| {
                    group.shape.capacity() * 4
                        + group.top.capacity() * std::mem::size_of::<term_cache::GroupHit>()
                })
                .sum::<usize>();
        if bytes > budget {
            return Err(Error::Bounds);
        }
        Ok(Self {
            terms,
            groups,
            complete: true,
            bytes,
        })
    }
}

pub(super) fn key(plan: &scorer::Plan) -> [u8; 32] {
    let mut state = blake3::Hasher::new_derive_key("oojuro joint numeric shape groups v1");
    state.update(&(plan.terms.len() as u32).to_le_bytes());
    for (term, _) in &plan.terms {
        state.update(&(term.len() as u32).to_le_bytes());
        state.update(term.as_bytes());
    }
    *state.finalize().as_bytes()
}
impl Index {
    pub(super) fn rank_joint(
        &self,
        model: &scorer::Model,
        selection: &Selection,
        limit: usize,
        literal: Option<&[u64]>,
    ) -> Result<Option<Top>> {
        if self.mass.is_none()
            || !selection.unfiltered()
            || model.plan.terms.len() > 16
            || limit > TOP
        {
            return Ok(None);
        }
        let groups = if model.plan.scope_model() {
            self.terms.joint(self, &model.plan)?
        } else if let Some(literal) = literal.filter(|_| model.plan.literal.is_some()) {
            self.terms.literal_groups(self, &model.plan, literal)?
        } else {
            None
        };
        let Some(groups) = groups else {
            return Ok(None);
        };
        if !groups.complete {
            return Ok(None);
        }
        if groups.terms.len() != model.plan.terms.len()
            || groups
                .terms
                .iter()
                .zip(&model.plan.terms)
                .any(|(hash, (term, _))| *hash != *blake3::hash(term.as_bytes()).as_bytes())
        {
            return Err(Error::Authority);
        }
        let mut top = Top::new(limit);
        for group in &groups.groups {
            if group.shape.len() != model.plan.terms.len() + 2
                || group.top.len() > TOP
                || group.rows < group.top.len() as u64
            {
                return Err(Error::Authority);
            }
            let bits = model.score(group.shape[0], group.shape[1], &group.shape[2..]);
            for row in &group.top {
                top.push(Scored {
                    ordinal: row.ordinal,
                    label: row.label,
                    time: row.time,
                    bits,
                });
            }
        }
        Ok(Some(top))
    }
}
