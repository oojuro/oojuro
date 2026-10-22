//! DATA roots only. Managed control owns publication, ancestry and original ACKs.
use crate::record::paged::tree;
use serde::{Deserialize, Serialize};

/// Small authenticated bundle; all per-frame/file/dictionary entries remain on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record) struct Roots {
    pub format: u32,
    pub frame: tree::Root,
    pub file: tree::Root,
    pub dictionary: tree::Root,
}
