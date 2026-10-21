//! `nine-maxdf:<frac>`: the `nine` arm with very frequent terms stopped on
//! the query side, an exact simulation of an index that does not store the
//! postings of terms whose df exceeds `frac` of the indexed records but
//! keeps every row's length (the index layout study).
//!
//! The arm tracks the df of every normalized term over the records it
//! indexed (the engine's own analysis, `oojuro_nine::index_terms`). Before a
//! search, every query word whose normalized term has df > frac x indexed
//! records is blanked out of the query text (same byte length, so nothing
//! else about the query changes); stop words are left alone; at least one
//! non-stop word always stays (the one with the lowest df); quoted literal
//! queries are not rewritten. Everything else is the `nine` arm.
use super::nine::NineArm;
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub struct NineMaxDfArm {
    inner: NineArm,
    frac: f64,
    store_thinking: bool,
    df: HashMap<String, u64>,
    indexed: u64,
    /// queries seen, queries rewritten, words dropped, queries whose every
    /// non-stop word was frequent
    counts: [u64; 4],
}

impl NineMaxDfArm {
    pub fn new(frac: f64) -> Self {
        Self { inner: NineArm::default(), frac, store_thinking: false, df: HashMap::new(), indexed: 0, counts: [0; 4] }
    }
}

/// Blank out query words whose normalized term `df` says is above `limit`.
/// Returns the rewritten text, the number of words blanked, and whether
/// every non-stop word was above the limit (the rarest one was kept; a
/// pruned index would hold no postings for it).
pub fn drop_frequent_words(text: &str, df: &dyn Fn(&str) -> u64, limit: f64) -> (String, usize, bool) {
    let t = text.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return (text.to_string(), 0, false);
    }
    // (start, end, term, stop word)
    let mut words: Vec<(usize, usize, String, bool)> = Vec::new();
    let mut start: Option<usize> = None;
    let push = |s: usize, e: usize, words: &mut Vec<(usize, usize, String, bool)>| {
        let w = &text[s..e];
        if !w.bytes().any(|b| b != b'_') {
            return;
        }
        let lower = w.to_lowercase();
        let stop = lower.len() <= 6 && oojuro_nine::QUERY_STOP_WORDS.contains(&lower.as_str());
        words.push((s, e, oojuro_nine::query_term(&lower), stop));
    };
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            push(s, i, &mut words);
        }
    }
    if let Some(s) = start {
        push(s, text.len(), &mut words);
    }
    let mut dfs: HashMap<&str, u64> = HashMap::new();
    for w in &words {
        if !w.3 {
            dfs.entry(w.2.as_str()).or_insert_with(|| df(&w.2));
        }
    }
    if dfs.is_empty() {
        return (text.to_string(), 0, false);
    }
    let mut drop: Vec<&str> = dfs.iter().filter(|(_, d)| **d as f64 > limit).map(|(t, _)| *t).collect();
    let all_frequent = drop.len() == dfs.len();
    if all_frequent {
        // keep the rarest term (ties: the smallest term, for determinism)
        let keep = dfs.iter().min_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0))).map(|x| *x.0).unwrap();
        drop.retain(|t| *t != keep);
    }
    if drop.is_empty() {
        return (text.to_string(), 0, all_frequent);
    }
    let mut out = text.as_bytes().to_vec();
    let mut n = 0;
    for w in &words {
        if !w.3 && drop.contains(&w.2.as_str()) {
            out[w.0..w.1].fill(b' ');
            n += 1;
        }
    }
    (String::from_utf8(out).expect("blanked whole chars"), n, all_frequent)
}

impl Arm for NineMaxDfArm {
    fn info(&self) -> ArmInfo {
        let mut i = self.inner.info();
        i.name = format!("nine-maxdf:{}", self.frac);
        i.notes.push(format!(
            "query-side simulation of stopping terms with df > {} of indexed records (rows keep their lengths)",
            self.frac
        ));
        i
    }
    fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<()> {
        self.store_thinking = config.get("thinking").is_some_and(|v| v == "store");
        self.df.clear();
        self.indexed = 0;
        self.inner.open(dir, config)
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        for d in docs {
            if self.store_thinking && d.meta.get("kind").is_some_and(|k| k == "thinking") {
                continue;
            }
            self.indexed += 1;
            for (term, _) in oojuro_nine::index_terms(&d.text) {
                *self.df.entry(term).or_default() += 1;
            }
        }
        self.inner.append_batch(docs)
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        let limit = self.frac * self.indexed as f64;
        let df = |t: &str| self.df.get(t).copied().unwrap_or(0);
        let (text, n, all) = drop_frequent_words(&query.text, &df, limit);
        self.counts[0] += 1;
        self.counts[3] += all as u64;
        if n > 0 {
            self.counts[1] += 1;
            self.counts[2] += n as u64;
        }
        let mut q = query.clone();
        q.text = text;
        self.inner.search(&q)
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        self.inner.get(ids)
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        self.inner.get_range(from, to, limit)
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        let mut s = self.inner.stats()?;
        s["maxdf"] = serde_json::json!({"frac": self.frac, "indexed": self.indexed, "terms": self.df.len(),
            "queries": self.counts[0], "rewritten": self.counts[1], "words_dropped": self.counts[2],
            "all_frequent": self.counts[3]});
        Ok(s)
    }
    fn close(&mut self) -> Result<()> {
        if self.counts[0] > 0 {
            eprintln!(
                "nine-maxdf:{} queries {} rewritten {} words_dropped {} all_frequent {}",
                self.frac, self.counts[0], self.counts[1], self.counts[2], self.counts[3]
            );
        }
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::drop_frequent_words;

    #[test]
    fn drops_frequent_keeps_one() {
        let df = |t: &str| match t {
            "error" => 900,
            "file" => 800,
            "parser_state" => 3,
            _ => 1,
        };
        let (t, n, _) = drop_frequent_words("Error: file parser_state", &df, 100.0);
        assert_eq!(t, "     : file parser_state".replace("file", "    "));
        assert_eq!(n, 2);
        // all frequent: the rarest stays
        let (t, n, all) = drop_frequent_words("error file", &df, 100.0);
        assert_eq!(t, "      file");
        assert_eq!(n, 1);
        assert!(all);
        // stop words are never the survivor
        let (t, _, _) = drop_frequent_words("the error of file", &df, 100.0);
        assert_eq!(t, "the       of file");
        // literal queries are untouched
        let (t, n, all) = drop_frequent_words("\"error file\"", &df, 100.0);
        assert_eq!((t.as_str(), n, all), ("\"error file\"", 0, false));
    }
}
