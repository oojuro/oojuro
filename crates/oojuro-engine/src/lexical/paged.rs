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
