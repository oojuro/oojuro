//! Explicit fresh paged managed layout under the same retained control authority.
use super::*;
use crate::record::paged::tree;
use oojuro_store::managed_pack::Birth;
pub(super) mod checkpoint;
pub(super) mod physical;
pub(super) mod schema;
pub(super) use checkpoint::digest as checkpoint_digest;
