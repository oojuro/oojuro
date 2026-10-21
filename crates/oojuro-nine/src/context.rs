//! Exact text sharing for optional context packets. No source information is inferred.
use oojuro_core::{EvidenceSnippet, MAX_FRAME, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Alias {
    at: usize,
    #[serde(flatten)]
    source: EvidenceSnippet,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Group {
    #[serde(flatten)]
    source: EvidenceSnippet,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    also_from: Vec<Alias>,
}

/// A byte-bounded, lossless packet with a cap on distinct text blocks.
/// Source origins may outnumber blocks; both counts are exposed explicitly.
pub struct ContextPacket {
    groups: Vec<Group>,
    texts: HashMap<String, usize>,
    origins: usize,
    bytes: usize,
    expanded_bytes: usize,
    byte_limit: usize,
    block_limit: usize,
}

impl ContextPacket {
    pub fn new(block_limit: usize, byte_limit: usize) -> Result<Self> {
        if !(1..=10_000).contains(&block_limit) || !(2..=MAX_FRAME).contains(&byte_limit) {
            return Err("invalid shared-context block or byte limit".into());
        }
        Ok(Self {
            groups: Vec::new(),
            texts: HashMap::new(),
            origins: 0,
            bytes: 2,
            expanded_bytes: 2,
            byte_limit,
            block_limit,
        })
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn remaining(&self) -> usize {
        self.byte_limit - self.bytes
    }
    pub fn expanded_bytes(&self) -> usize {
        self.expanded_bytes
    }
    pub fn origin_count(&self) -> usize {
        self.origins
    }
    pub fn block_count(&self) -> usize {
        self.texts.len()
    }
    pub fn at_block_limit(&self) -> bool {
        self.texts.len() == self.block_limit
    }
    pub fn contains_text(&self, text: &str) -> bool {
        self.texts.contains_key(text)
    }

    /// Admit an original-range snippet if its measured representation fits.
    /// False changes nothing; a caller may try a smaller original range next.
    pub fn push(&mut self, mut source: EvidenceSnippet) -> Result<bool> {
        check_range(&source)?;
        if self.origins == 10_000 {
            return Err("shared-context origin limit exceeded".into());
        }
        let flat = serde_json::to_vec(&source)?.len() + usize::from(!self.groups.is_empty());
        if flat > MAX_FRAME - self.expanded_bytes {
            return Ok(false);
        }
        if let Some(&index) = self.texts.get(&source.text) {
            let text = std::mem::take(&mut source.text);
            let alias = Alias {
                at: self.origins,
                source,
            };
            let extra = serde_json::to_vec(&alias)?.len()
                + if self.groups[index].also_from.is_empty() {
                    b",\"also_from\":[".len() + 1
                } else {
                    1
                };
            if extra <= flat && extra <= self.remaining() {
                self.groups[index].also_from.push(alias);
                self.bytes += extra;
                self.expanded_bytes += flat;
                self.origins += 1;
                return Ok(true);
            }
            source = alias.source;
            source.text = text;
        } else if self.at_block_limit() {
            return Ok(false);
        }
        if flat > self.remaining() {
            return Ok(false);
        }
        self.texts
            .entry(source.text.clone())
            .or_insert(self.groups.len());
        self.groups.push(Group {
            source,
            also_from: Vec::new(),
        });
        self.bytes += flat;
        self.expanded_bytes += flat;
        self.origins += 1;
        Ok(true)
    }
    pub fn to_json(&self) -> Result<Vec<u8>> {
        let encoded = serde_json::to_vec(&self.groups)?;
        if encoded.len() != self.bytes || encoded.len() > self.byte_limit {
            return Err("shared-context byte accounting mismatch".into());
        }
        Ok(encoded)
    }
    pub fn expand(&self) -> Result<Vec<EvidenceSnippet>> {
        expand(&self.groups)
    }
}

/// Decode shared or legacy flat packets, restoring all origins in admission order.
pub fn decode_context(encoded: &[u8]) -> Result<Vec<EvidenceSnippet>> {
    if encoded.len() > MAX_FRAME {
        return Err("oversized shared-context packet".into());
    }
    let groups: Vec<Group> = serde_json::from_slice(encoded)?;
    expand(&groups)
}

fn check_range(source: &EvidenceSnippet) -> Result<()> {
    if source
        .source_range
        .is_some_and(|[start, end]| end < start || end - start != source.text.len())
    {
        return Err("context range length does not match original text".into());
    }
    Ok(())
}

fn expand(groups: &[Group]) -> Result<Vec<EvidenceSnippet>> {
    let count = groups
        .iter()
        .try_fold(0usize, |n, g| n.checked_add(1 + g.also_from.len()))
        .filter(|n| *n <= 10_000)
        .ok_or("invalid shared-context origin count")?;
    let mut ordered = vec![None; count];
    let mut expanded_bytes = 2 + count.saturating_sub(1);
    for group in groups {
        let escaped_text = serde_json::to_string(&group.source.text)?.len() - 2;
        expanded_bytes += serde_json::to_vec(&group.source)?.len();
        if expanded_bytes > MAX_FRAME {
            return Err("expanded context exceeds byte limit".into());
        }
        for alias in &group.also_from {
            if !alias.source.text.is_empty() {
                return Err("invalid shared-context alias text".into());
            }
            let cost = serde_json::to_vec(&alias.source)?.len() + escaped_text;
            if cost > MAX_FRAME - expanded_bytes {
                return Err("expanded context exceeds byte limit".into());
            }
            expanded_bytes += cost;
        }
    }
    for group in groups {
        check_range(&group.source)?;
        for alias in &group.also_from {
            if alias.at >= count || ordered[alias.at].is_some() || !alias.source.text.is_empty() {
                return Err("invalid shared-context alias".into());
            }
            let mut source = alias.source.clone();
            source.text = group.source.text.clone();
            check_range(&source)?;
            ordered[alias.at] = Some(source);
        }
    }
    let mut vacant = ordered.iter_mut().filter(|s| s.is_none());
    for group in groups {
        *vacant.next().ok_or("invalid shared-context order")? = Some(group.source.clone());
    }
    Ok(ordered.into_iter().map(Option::unwrap).collect())
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
