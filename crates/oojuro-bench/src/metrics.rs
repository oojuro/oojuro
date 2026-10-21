//! trec_eval-compatible ranking metrics over the order an arm returned.
//! Gains are the raw qrel grades (trec_eval ndcg_cut convention); grades <= 0
//! are non-relevant. Callers average over every labelled query, scoring a
//! query with no returned documents as 0 (pytrec_eval would drop it).
use std::collections::HashMap;

pub type Qrels = HashMap<String, i64>;

fn gain(rel: &Qrels, id: &str) -> f64 {
    rel.get(id).copied().filter(|g| *g > 0).unwrap_or(0) as f64
}

pub fn ndcg(ranked: &[String], rel: &Qrels, k: usize) -> f64 {
    let discount = |i: usize| 1.0 / ((i + 2) as f64).log2();
    let dcg: f64 = ranked.iter().take(k).enumerate().map(|(i, id)| gain(rel, id) * discount(i)).sum();
    let mut ideal: Vec<f64> = rel.values().filter(|g| **g > 0).map(|g| *g as f64).collect();
    ideal.sort_by(|a, b| b.total_cmp(a));
    let idcg: f64 = ideal.iter().take(k).enumerate().map(|(i, g)| g * discount(i)).sum();
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

pub fn relevant_count(rel: &Qrels) -> usize {
    rel.values().filter(|g| **g > 0).count()
}

pub fn recall(ranked: &[String], rel: &Qrels, k: usize) -> f64 {
    let total = relevant_count(rel);
    if total == 0 {
        return 0.0;
    }
    ranked.iter().take(k).filter(|id| gain(rel, id) > 0.0).count() as f64 / total as f64
}

pub fn mrr(ranked: &[String], rel: &Qrels, k: usize) -> f64 {
    ranked.iter().take(k).position(|id| gain(rel, id) > 0.0).map_or(0.0, |i| 1.0 / (i + 1) as f64)
}

pub fn hit(ranked: &[String], rel: &Qrels, k: usize) -> f64 {
    f64::from(u8::from(ranked.iter().take(k).any(|id| gain(rel, id) > 0.0)))
}

/// The standard per-query metric set, keyed like the scorecard.
pub fn standard(ranked: &[String], rel: &Qrels) -> Vec<(&'static str, f64)> {
    vec![
        ("ndcg@10", ndcg(ranked, rel, 10)),
        ("recall@10", recall(ranked, rel, 10)),
        ("recall@100", recall(ranked, rel, 100)),
        ("mrr@10", mrr(ranked, rel, 10)),
    ]
}

pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() { f64::NAN } else { values.iter().sum::<f64>() / values.len() as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn graded_ndcg_matches_hand_computation() {
        let rel: Qrels = [("a".into(), 2), ("b".into(), 1), ("z".into(), 0)].into_iter().collect();
        let ranked = ids(&["b", "x", "a"]);
        let dcg = 1.0 + 2.0 / 4f64.log2();
        let idcg = 2.0 + 1.0 / 3f64.log2();
        assert!((ndcg(&ranked, &rel, 10) - dcg / idcg).abs() < 1e-12);
        assert_eq!(recall(&ranked, &rel, 2), 0.5);
        assert_eq!(mrr(&ranked, &rel, 10), 1.0);
        assert_eq!(mrr(&ids(&["x", "z", "a"]), &rel, 10), 1.0 / 3.0);
        assert_eq!(ndcg(&ids(&["z"]), &rel, 10), 0.0);
    }
}
