//! The first working slice of the new engine: sealed, byte-exact local raw sources.
//! Parsed/index/query/access-policy/audit and hosted replicated commits are separate milestones.
pub mod raw;

#[cfg(unix)]
pub mod file_capture;
