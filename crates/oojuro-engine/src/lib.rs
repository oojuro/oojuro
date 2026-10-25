//! Sealed sources and exact lexical retrieval with disposable memory/disk projections.
//! The event adapter accepts opaque cursors, timestamps and exact data. Callers
//! validate current source snapshots and permissions before using a projection.
pub mod raw;

#[cfg(unix)]
pub mod file_capture;

/// Atomic sealed raw/parsed authority; legacy raw-only format remains independent.
pub mod capture_log;

/// Native derived lexical postings/query over committed authorized receipts.
pub mod lexical;

/// Scoped original committed-receipt timeline and bounded export (opt-in).
pub mod receipts;

/// Direct flat-record/time namespace; never an implicit harness-format conversion.
pub mod record;
pub use record::projection::disk::{
    EventAppender, EventBuilder, EventHit, EventIndex, EventQuery, EventRanked, EventRow,
};

/// Opt-in content-free signed audit wire; persistence and current witnesses are external.
pub mod audit;
