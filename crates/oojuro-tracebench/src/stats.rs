//! Latency recording and summaries.
use serde_json::{Value, json};
use std::time::Instant;

#[derive(Default, Clone)]
pub struct Lat {
    ns: Vec<u64>,
    pub items: u64,
    pub bytes: u64,
}

impl Lat {
    pub fn add(&mut self, start: Instant) {
        self.ns.push(start.elapsed().as_nanos() as u64);
    }
    pub fn add_ns(&mut self, ns: u64) {
        self.ns.push(ns);
    }
    pub fn merge(&mut self, o: &Lat) {
        self.ns.extend_from_slice(&o.ns);
        self.items += o.items;
        self.bytes += o.bytes;
    }
    pub fn len(&self) -> usize {
        self.ns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ns.is_empty()
    }
    /// Nearest-rank percentile in milliseconds.
    pub fn pct(&self, q: f64) -> f64 {
        if self.ns.is_empty() {
            return f64::NAN;
        }
        let mut v = self.ns.clone();
        v.sort_unstable();
        let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
        v[i] as f64 / 1e6
    }
    pub fn mean_ms(&self) -> f64 {
        self.ns.iter().sum::<u64>() as f64 / self.ns.len().max(1) as f64 / 1e6
    }
    /// Summary; `wall_s` turns counts into throughput when given.
    pub fn summary(&self, wall_s: Option<f64>) -> Value {
        let mut v = json!({
            "n": self.ns.len(), "p50_ms": self.pct(0.5), "p90_ms": self.pct(0.9), "p95_ms": self.pct(0.95),
            "p99_ms": self.pct(0.99), "max_ms": self.pct(1.0), "mean_ms": self.mean_ms(),
        });
        if let Some(w) = wall_s {
            v["wall_s"] = json!(w);
            v["ops_per_s"] = json!(self.ns.len() as f64 / w);
            if self.items > 0 {
                v["items_per_s"] = json!(self.items as f64 / w);
            }
            if self.bytes > 0 {
                v["mb_per_s"] = json!(self.bytes as f64 / w / 1e6);
            }
        }
        if self.items > 0 {
            v["items"] = json!(self.items);
        }
        if self.bytes > 0 {
            v["bytes"] = json!(self.bytes);
        }
        v
    }
}

/// Median and spread of one metric over runs.
pub fn median_spread(xs: &[f64]) -> Value {
    let mut v: Vec<f64> = xs.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let med = if v.len() % 2 == 1 { v[v.len() / 2] } else { (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0 };
    json!({"median": med, "min": v[0], "max": v[v.len() - 1], "runs": v.len()})
}
