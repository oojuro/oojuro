//! Explicit fresh paged managed layout under the same retained control authority.
use super::*;
use crate::record::paged::tree;
use oojuro_store::managed_pack::Birth;
pub(super) mod checkpoint;
pub(super) mod physical;
pub(super) mod schema;
pub(super) use checkpoint::digest as checkpoint_digest;
mod driver;
mod source;
pub use driver::ManagedPagedLog;
mod read;
pub use read::{PagedRecords, ReadLimits};
mod inventory;
mod proof;
mod prune;
pub use prune::{PagedPruneReport, PruneLimits};
mod denied;
pub use denied::DeniedCleanup;

mod ids;
pub use ids::PrunedIds;
