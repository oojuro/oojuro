//! Built-in arms. Each path may add its own arm file here, or ship any
//! executable that speaks the protocol and plug it in as `cmd:<argv>`.
use crate::{Arm, Result};
use std::collections::HashMap;

pub mod nine;
pub mod nine_v3;
#[cfg(feature = "sqlite-arm")]
pub mod sqlite;
#[cfg(feature = "tantivy-arm")]
pub mod tantivy;

/// Commit of the source tree this binary was built from (see build.rs).
pub const COMMIT: &str = env!("OOJURO_BENCH_COMMIT");

pub const BUILTIN: &[&str] = &["nine", "nine-v3", "tantivy", "sqlite"];

pub fn builtin(name: &str) -> Result<Box<dyn Arm>> {
    match name {
        "nine" => Ok(Box::new(nine::NineArm::default())),
        "nine-v3" => Ok(Box::new(nine_v3::NineV3Arm::default())),
        #[cfg(feature = "tantivy-arm")]
        "tantivy" => Ok(Box::new(tantivy::TantivyArm::default())),
        #[cfg(feature = "sqlite-arm")]
        "sqlite" | "sqlite-fts5" => Ok(Box::new(sqlite::SqliteArm::default())),
        _ => Err(format!("unknown built-in arm {name:?}; known: {BUILTIN:?} or cmd:<argv>").into()),
    }
}

/// As-of revision visibility for arms whose index cannot express "latest
/// visible revision per key" natively. Mirrors oojuro_core::visible_latest:
/// the visible revision is the greatest (valid_at, recorded_at, seq) with
/// valid_at <= query valid_at and recorded_at <= known_at.
#[derive(Default)]
pub struct Revisions {
    by_key: HashMap<String, Vec<(i64, i64, u64)>>,
    by_id: HashMap<String, (String, i64, i64, u64)>,
    next: u64,
}

impl Revisions {
    pub fn insert(&mut self, id: &str, key: &str, valid_at: i64, recorded_at: i64) {
        let seq = self.next;
        self.next += 1;
        let chain = self.by_key.entry(key.to_string()).or_default();
        let entry = (valid_at, recorded_at, seq);
        let at = chain.partition_point(|e| *e <= entry);
        chain.insert(at, entry);
        self.by_id.insert(id.to_string(), (key.to_string(), valid_at, recorded_at, seq));
    }
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }
    /// Whether `id` is the latest revision of its key visible as of the query.
    pub fn visible(&self, id: &str, valid_at: i64, known_at: i64) -> bool {
        let Some((key, v, r, seq)) = self.by_id.get(id) else { return false };
        if *v > valid_at || *r > known_at {
            return false;
        }
        let chain = &self.by_key[key];
        if chain.len() == 1 {
            return true;
        }
        chain
            .iter()
            .rev()
            .find(|(cv, cr, _)| *cv <= valid_at && *cr <= known_at)
            .is_some_and(|(_, _, s)| s == seq)
    }
    pub fn len(&self) -> usize {
        self.by_id.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}
