//! Native sealed raw/parsed capture authority and bounded in-memory lexical retrieval.
//! Disk index/merge, access policy, erasure/audit and hosted replicated commits remain separate milestones.
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

/// Opt-in content-free signed audit wire; persistence and current witnesses are external.
pub mod audit;

/// Explicit opt-in durable Capture1 to managed Record ingestion.
#[cfg(unix)]
pub mod capture_record;
