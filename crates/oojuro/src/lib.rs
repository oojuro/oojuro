//! Oojuro v1 front door: use the journal like SQLite (`docs/FRONT_DOOR.md`).
//!
//! The recorder (`oojurod`) journals what agents see, think and do. This
//! crate is the explicit side: a library, the `oojuro` CLI and an MCP
//! server with `remember`, `search`, `get`, `history`, `range`, `forget`,
//! `stats` and `export`, plus the settings (`config.toml`, `.oojuro.toml`,
//! `[agents."<writer>"]`).
//!
//! - In-process, owning the store: [`Journal`] (writes) and [`Reader`].
//! - From any process: [`Oojuro`] goes through the recorder's socket when
//!   it serves the journal and opens the journal directly otherwise.
//!
//! ```no_run
//! use oojuro::{Oojuro, Paths};
//! let oj = Oojuro::open(Paths::resolve(None, None, None, None)?);
//! let r = oj.remember("human:me", "staging deploys need the VPN", Some("deploy-notes"), None)?;
//! for hit in oj.search_simple("how do staging deploys work", 5, 65_536)? {
//!     println!("{} {:.1} bits {}", hit.id, hit.score_bits, hit.excerpt);
//! }
//! # let _ = r;
//! # Ok::<(), oojuro::Error>(())
//! ```
pub mod append;
pub mod client;
pub mod config;
pub mod exclude;
pub mod hook;
pub mod hosted;
pub mod hosted_capture;
pub mod journal;
pub mod mcp;
pub mod otel;
pub mod paths;
pub mod periods;
pub mod policy;
pub mod proto;
pub mod proxy_sink;
pub mod server;
pub mod timearg;
pub mod types;
pub mod when;

pub use client::{Oojuro, Via};
pub use config::{Config, Settings};
pub use journal::{Journal, Reader};
pub use paths::Paths;
pub use types::{Filters, ForgetReport, Proof, Record, Ref, Remembered, SearchArgs, Stats, Target, WriterStats};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Results of an explicit search when the caller does not say.
pub const DEFAULT_K: usize = 10;
/// Excerpt size around the best match when the caller does not say.
pub const DEFAULT_EXCERPT_BYTES: usize = 320;
