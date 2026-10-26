//! Exact exhaustive scoring over caller-provided bounded pages. No population/candidate vectors.
//! Arithmetic order deliberately matches lexical::Index; pivot pruning is not required for exactness.
use super::*;
use std::{cmp::Ordering, collections::BinaryHeap};
pub(crate) fn is_literal(text: &str) -> bool {
    literal_query(text).is_some()
}
pub(crate) fn terms(text: &str) -> BTreeMap<String, usize> {
    query_terms(text)
}
pub(crate) fn possible_terms(text: &str) -> BTreeSet<String> {
    let raw = query_terms(text);
    let mut terms: BTreeSet<_> = raw.keys().cloned().collect();
    for term in raw.keys().filter(|t| t.contains('_')) {
        terms.extend(query_terms(&term.replace('_', " ")).into_keys());
    }
    terms
}
#[derive(Clone)]
pub(crate) struct Plan {
    pub terms: Vec<(String, usize)>,
    pub anchored: bool,
    pub literal: Option<String>,
    direct: Option<Option<u64>>,
    scan: bool,
    scope_model: bool,
    rarest: Option<usize>,
    empty: bool,
}
impl Plan {
    pub fn new(
        text: &str,
        split: bool,
        direct: Option<Option<u64>>,
        present: &BTreeSet<String>,
        df: &BTreeMap<String, u64>,
    ) -> Self {
        let literal = literal_query(text).map(str::to_owned);
        let mut query = query_terms(text);
        let mut backoff = vec![];
        for (term, count) in &query {
            if term.contains('_') && (split || !present.contains(term)) {
                backoff.push((term.clone(), *count));
            }
        }
        for (term, count) in backoff {
            query.remove(&term);
            for (part, n) in query_terms(&term.replace('_', " ")) {
                *query.entry(part).or_default() += n * count;
            }
        }
        let anchored = direct.is_none() && query.keys().any(|t| t.contains('_'));
        let scope_model = direct.is_none() && !anchored && literal.is_none() && !text.is_empty();
        let scan = text.is_empty() || (literal.is_some() && query.is_empty());
        let terms: Vec<_> = query.into_iter().collect();
        let rarest = literal.as_ref().and_then(|_| {
            terms
                .iter()
                .enumerate()
                .min_by_key(|(_, (term, _))| df.get(term.as_str()).copied().unwrap_or(0))
                .map(|(i, _)| i)
        });
        Self {
            terms,
            anchored,
            literal,
            direct,
            scan,
            scope_model,
            rarest,
            empty: text.is_empty(),
        }
    }
    pub fn candidate(&self, id: u64, indexed: bool, text: &str, visible: bool, tf: &[u32]) -> bool {
        self.candidate_terms(id, indexed, visible, tf)
            && self
                .literal
                .as_deref()
                .is_none_or(|p| phrase_start(text, p).is_some())
    }
    /// Candidate eligibility without opening an original merely to reject a
    /// missing term. A caller with a literal must still verify its exact bytes.
    pub fn candidate_terms(&self, id: u64, indexed: bool, visible: bool, tf: &[u32]) -> bool {
        if !visible || (!self.empty && self.direct.is_none() && !indexed) {
            return false;
        }
        if let Some(direct) = self.direct {
            return direct == Some(id);
        }
        if self.scan {
            return true;
        }
        tf.iter().enumerate().any(|(i, n)| {
            *n > 0
                && self
                    .rarest
                    .map_or(!self.anchored || self.terms[i].0.contains('_'), |r| r == i)
        })
    }
    fn mass(&self, length: u32, scope: u32) -> u64 {
        u64::from(if self.scope_model { scope } else { length })
    }
    pub fn continues(&self) -> bool {
        self.anchored && self.literal.is_none()
    }
    pub fn scope_model(&self) -> bool {
        self.scope_model
    }
    pub fn literal_driver(&self) -> Option<usize> {
        self.literal.as_ref().and(self.rarest)
    }
    pub fn anchor_driver(&self, at: usize) -> bool {
        self.anchored && self.literal.is_none() && self.terms[at].0.contains('_')
    }
    pub fn anchored_driver(&self) -> bool {
        self.anchored && self.literal.is_none()
    }
}
pub(crate) struct Stats {
    collection: Vec<u64>,
    view: Vec<u64>,
    total: u64,
    view_total: u64,
}
impl Stats {
    pub fn new(plan: &Plan) -> Self {
        Self {
            collection: vec![0; plan.terms.len()],
            view: vec![0; plan.terms.len()],
            total: 0,
            view_total: 0,
        }
    }
    pub fn single_scope(plan: &Plan, matches: u64, scope: u64, view_scope: u64) -> Result<Self> {
        if !plan.scope_model || plan.terms.len() != 1 || plan.scan || plan.direct.is_some() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            collection: vec![matches],
            view: vec![matches],
            total: scope,
            view_total: view_scope,
        })
    }
    /// Exact scope counts from authenticated posting/selection intersections.
    /// All integer totals precede the unchanged floating-point model arithmetic.
    pub fn scope_counts(plan: &Plan, counts: Vec<u64>, mass: u64, view_mass: u64) -> Result<Self> {
        if !plan.scope_model
            || plan.scan
            || plan.direct.is_some()
            || counts.len() != plan.terms.len()
            || mass > view_mass
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            collection: counts.clone(),
            view: counts,
            total: mass,
            view_total: view_mass,
        })
    }
    /// Exact integer counts under a complete literal-position proof. The
    /// unchanged finish/score methods retain the original arithmetic order.
    pub fn literal_counts(
        plan: &Plan,
        collection: Vec<u64>,
        view: Vec<u64>,
        total: u64,
        view_total: u64,
    ) -> Result<Self> {
        if plan.literal.is_none()
            || plan.scope_model
            || plan.direct.is_some()
            || collection.len() != plan.terms.len()
            || view.len() != plan.terms.len()
            || total > view_total
            || collection.iter().zip(&view).any(|(a, b)| a > b)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            collection,
            view,
            total,
            view_total,
        })
    }
    /// Sparse postings visits already cover every visible term occurrence.
    /// Only a small candidate collection needs mass from nonmatching rows.
    pub fn needs_view_total(&self) -> bool {
        self.collection.iter().any(|n| *n > 0) && (self.total as f64) < FLOOR_RECORDS * MU
    }
    pub fn set_view_total(&mut self, total: u64) {
        self.view_total = total;
    }
    pub fn reset_view(&mut self) {
        self.view.fill(0);
        self.view_total = 0;
    }
    pub fn visit(
        &mut self,
        plan: &Plan,
        length: u32,
        scope: u32,
        tf: &[u32],
        visible: bool,
        candidate: bool,
    ) -> Result<()> {
        if tf.len() != self.collection.len() {
            return Err(Error::Invalid);
        }
        let add = |a: u64, b: u64| a.checked_add(b).ok_or(Error::Invalid);
        if visible {
            self.view_total = add(self.view_total, plan.mass(length, scope))?;
        }
        if candidate {
            self.total = add(self.total, plan.mass(length, scope))?;
        }
        for (i, n) in tf.iter().enumerate() {
            let n = if plan.scope_model {
                u64::from(*n > 0)
            } else {
                u64::from(*n)
            };
            if visible {
                self.view[i] = add(self.view[i], n)?;
            }
            if candidate && plan.direct.is_none() && !plan.scan {
                self.collection[i] = add(self.collection[i], n)?;
            }
        }
        Ok(())
    }
    pub fn finish(self, plan: Plan) -> Model {
        let total = self.total as f64;
        let floor = FLOOR_RECORDS * MU;
        let any = self.collection.iter().any(|n| *n > 0);
        let pad = (any && total < floor && self.view_total > 0)
            .then_some((floor - total, self.view_total as f64));
        let background = plan
            .terms
            .iter()
            .enumerate()
            .map(|(i, (term, _))| match pad {
                None => (self.collection[i] as f64 + literal_probability(term)) / (total + 1.0),
                Some((r, view_total)) => {
                    (self.collection[i] as f64
                        + r * if self.collection[i] > 0 {
                            self.view[i] as f64
                        } else {
                            0.0
                        } / view_total
                        + literal_probability(term))
                        / (total + r + 1.0)
                }
            })
            .collect();
        let query_count = plan
            .terms
            .iter()
            .enumerate()
            .filter(|(i, _)| self.collection[*i] > 0)
            .map(|(_, (_, n))| *n)
            .sum::<usize>() as f64;
        Model {
            plan,
            background,
            query_count,
        }
    }
}
pub(crate) struct Model {
    pub plan: Plan,
    background: Vec<f64>,
    query_count: f64,
}
impl Model {
    /// Literal gain uses TF directly; only its negative penalty depends on
    /// length. Independent minimum length and maximum TF are therefore safe.
    pub fn literal_bound(&self, minimum_length: u32, maximum_tf: &[u32]) -> Option<f64> {
        if self.plan.literal.is_none()
            || self.plan.scope_model
            || maximum_tf.len() != self.plan.terms.len()
        {
            return None;
        }
        let score = self.score(minimum_length, 0, maximum_tf);
        score.is_finite().then_some(score)
    }
    /// Independent optimistic penalty/gain bounds for heterogeneous scopes.
    /// Each maximum is computed with the exact score expression TF*scope/length,
    /// not a separately rounded ratio. Different rows may maximize each term;
    /// that only loosens the bound. Missing/nonfinite proofs refuse pruning.
    pub fn scope_bound(&self, minimum_scope: u32, maximum_mass: &[f64]) -> Option<f64> {
        if !self.plan.scope_model || maximum_mass.len() != self.plan.terms.len() {
            return None;
        }
        let mut gain = -self.query_count * (1. + minimum_scope as f64 / MU).log2();
        for (i, mass) in maximum_mass.iter().copied().enumerate() {
            if !mass.is_finite() || mass < 0. {
                return None;
            }
            if mass == 0. {
                continue;
            }
            let scale = MU * self.background[i];
            let count = self.plan.terms[i].1 as f64;
            gain += count * (1. + mass / scale).log2();
        }
        gain += 0.;
        gain.is_finite().then_some(gain)
    }
    pub fn score(&self, length: u32, scope: u32, tf: &[u32]) -> f64 {
        let mut gain = -self.query_count * (1. + self.plan.mass(length, scope) as f64 / MU).log2();
        if self.plan.direct.is_some() || self.plan.scan {
            return gain + 0.0;
        }
        for (i, n) in tf.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            let scale = MU * self.background[i];
            let count = self.plan.terms[i].1 as f64;
            let mass = if self.plan.scope_model {
                *n as f64 * scope as f64 / length as f64
            } else {
                *n as f64
            };
            gain += count * (1. + mass / scale).log2();
        }
        gain + 0.0
    }
}
struct Ranked(Hit);
fn compare(a: &Hit, b: &Hit) -> Ordering {
    b.bits
        .total_cmp(&a.bits)
        .then_with(|| b.time.cmp(&a.time))
        .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
}
impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        compare(&self.0, &other.0) == Ordering::Equal
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        compare(&self.0, &other.0)
    }
}
pub(crate) struct Top {
    heap: BinaryHeap<Ranked>,
    limit: usize,
}
impl Top {
    pub fn new(limit: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            limit,
        }
    }
    pub fn push(&mut self, id: u64, time: i64, bits: f64) {
        if self.limit == 0 {
            return;
        }
        let row = Ranked(Hit {
            id,
            bits,
            continued: false,
            time,
            row: id,
        });
        if self.heap.len() < self.limit {
            self.heap.push(row);
        } else if self
            .heap
            .peek()
            .is_some_and(|worst| row.cmp(worst) == Ordering::Less)
        {
            self.heap.pop();
            self.heap.push(row);
        }
    }
    pub fn finish(self) -> Vec<Hit> {
        let mut rows: Vec<_> = self.heap.into_iter().map(|r| r.0).collect();
        rows.sort_by(compare);
        rows
    }
}
