//! Structural paging prototype: no retained Record payload population or hydrated postings.
//! Existing Record2 limits remain; O(batch) locators and streamed key scans are intermediate.
mod cache;
mod reader;
mod tree;
use super::*;
pub use cache::{Index, Opened};
pub use reader::{Reader, Snapshot};
#[cfg(test)]
mod tests;
