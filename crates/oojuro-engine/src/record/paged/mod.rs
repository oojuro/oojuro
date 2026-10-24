//! Structural paging prototype: no retained Record payload population or hydrated postings.
//! Legacy constructors retain their Record2 limits. Explicit disk limits bound stored data,
//! while batches, decoded pages and working caches retain their independent bounds.
mod cache;
mod reader;
mod tree;
use super::*;
pub use cache::{Index, Opened};
pub use reader::{Reader, Snapshot, Writer};
/// Trusted caller policy for the explicit disk-backed path, never loaded from a cache.
/// These are disk admission limits, not permission to retain a population in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskLimits {
    records: usize,
    source_bytes: usize,
    index_bytes: usize,
    segments: usize,
}
impl DiskLimits {
    pub fn new(
        records: usize,
        source_bytes: usize,
        index_bytes: usize,
        segments: usize,
    ) -> Result<Self> {
        if records == 0
            || records > u32::MAX as usize
            || source_bytes == 0
            || index_bytes == 0
            || segments == 0
            || segments > u32::MAX as usize
        {
            return Err(Error::Bounds);
        }
        Ok(Self {
            records,
            source_bytes,
            index_bytes,
            segments,
        })
    }
    const LEGACY: Self = Self {
        records: MAX_RECORDS,
        source_bytes: MAX_STATE_BYTES,
        index_bytes: 128 << 20,
        segments: 1024,
    };
}
#[cfg(test)]
mod tests;
