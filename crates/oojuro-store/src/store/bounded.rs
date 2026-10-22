//! Bounded hot recovery and deterministic dictionary sampling; no new storage authority.
use super::*;
use std::{cmp::Ordering, collections::BinaryHeap};
const SCAN_BYTES: usize = 64 << 10;
const SAMPLE_BYTES: usize = 8 << 10;
const SAMPLE_COUNT: usize = 2048;
const TRAIN_BYTES: usize = SAMPLE_BYTES * SAMPLE_COUNT;

/// A valid CRC-framed payload, bounded by the existing wire maximum.
pub(super) fn payload(file: &File, size: u64, at: u64) -> Result<Option<Vec<u8>>> {
    let Some((len, want)) = header(file, size, at)? else {
        return Ok(None);
    };
    let mut bytes = vec![0; len];
    file.read_exact_at(&mut bytes, at + 8)?;
    Ok((frame_crc(&bytes) == want).then_some(bytes))
}
fn header(file: &File, size: u64, at: u64) -> Result<Option<(usize, u32)>> {
    if at.checked_add(8).is_none_or(|end| end > size) {
        return Ok(None);
    }
    let mut head = [0; 8];
    file.read_exact_at(&mut head, at)?;
    Ok(parse_header(&head, size, at))
}
fn parse_header(head: &[u8], size: u64, at: u64) -> Option<(usize, u32)> {
    let len = u32::from_le_bytes(head[..4].try_into().ok()?) as usize;
    let want = u32::from_le_bytes(head[4..8].try_into().ok()?);
    if len == 0 || len > MAX_FRAME + 64 || at.checked_add(8 + len as u64)? > size {
        return None;
    }
    Some((len, want))
}
/// Preserve the old byte-by-byte later-valid-frame test without reading the whole log.
/// Adversarial CRC candidates can still require repeated IO; the workspace is bounded.
pub(super) fn any_after(file: &File, size: u64, from: u64) -> Result<bool> {
    let end = size.saturating_sub(8);
    let mut at = from;
    let mut headers = vec![0; SCAN_BYTES + 7];
    let mut chunk = vec![0; SCAN_BYTES];
    while at < end {
        let candidates = (end - at).min(SCAN_BYTES as u64) as usize;
        file.read_exact_at(&mut headers[..candidates + 7], at)?;
        for i in 0..candidates {
            let start = at + i as u64;
            let Some((len, want)) = parse_header(&headers[i..i + 8], size, start) else {
                continue;
            };
            let mut crc = crc32fast::Hasher::new();
            crc.update(&(len as u32).to_le_bytes());
            let mut read = 0;
            while read < len {
                let n = (len - read).min(chunk.len());
                file.read_exact_at(&mut chunk[..n], start + 8 + read as u64)?;
                crc.update(&chunk[..n]);
                read += n;
            }
            if crc.finalize() == want {
                return Ok(true);
            }
        }
        at += candidates as u64;
    }
    Ok(false)
}

struct Sample {
    priority: u64,
    ordinal: u64,
    bytes: Vec<u8>,
}
impl PartialEq for Sample {
    fn eq(&self, other: &Self) -> bool {
        (self.priority, self.ordinal) == (other.priority, other.ordinal)
    }
}
impl Eq for Sample {}
impl PartialOrd for Sample {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Sample {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.priority, self.ordinal).cmp(&(other.priority, other.ordinal))
    }
}
/// Content-independent deterministic priority reservoir: <=2048 x8KiB selected bytes.
pub(super) struct Training {
    samples: BinaryHeap<Sample>,
    seen: u64,
    total: u64,
}
impl Training {
    pub fn new() -> Self {
        Self {
            samples: BinaryHeap::new(),
            seen: 0,
            total: 0,
        }
    }
    pub fn push(&mut self, text: &[u8]) {
        let ordinal = self.seen;
        self.seen = self.seen.saturating_add(1);
        self.total = self.total.saturating_add(text.len() as u64);
        if text.len() < 8 {
            return;
        }
        // SplitMix64 is a deterministic sample rank, not a secret or data digest.
        let mut x = ordinal.wrapping_add(0x9e3779b97f4a7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        let priority = x ^ (x >> 31);
        if self.samples.len() == SAMPLE_COUNT {
            let last = self.samples.peek().unwrap();
            if (priority, ordinal) >= (last.priority, last.ordinal) {
                return;
            }
            self.samples.pop();
        }
        self.samples.push(Sample {
            priority,
            ordinal,
            bytes: text[..text.len().min(SAMPLE_BYTES)].to_vec(),
        });
    }
    pub fn finish(self, dict_bytes: usize) -> Option<Vec<u8>> {
        let min = dict_bytes.checked_mul(8)?;
        let bytes: usize = self.samples.iter().map(|s| s.bytes.len()).sum();
        if dict_bytes == 0
            || min > TRAIN_BYTES
            || self.total < min as u64
            || bytes < min
            || self.samples.len() < 16
        {
            return None;
        }
        let mut rows = self.samples.into_vec();
        rows.sort_unstable_by_key(|s| s.ordinal);
        let mut samples = Vec::with_capacity(bytes);
        let mut sizes = Vec::with_capacity(rows.len());
        for row in rows {
            sizes.push(row.bytes.len());
            samples.extend(row.bytes);
        }
        zstd::dict::from_continuous(&samples, &sizes, dict_bytes).ok()
    }
}
#[cfg(test)]
mod tests;
