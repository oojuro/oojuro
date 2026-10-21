//! Append-only compressed original store for agent text.
//!
//! Narrow interface for the engine: [`Store::append`] a turn batch durably,
//! [`Store::get`] / [`Store::get_range`] by sequence number, [`Store::scan_time`],
//! and [`Store::compact`] to rewrite the hot per-turn log into the cold tier.
pub mod corpus;
pub mod format;
mod pack;
mod reader;
pub mod run;
pub mod starter;
pub mod store;

pub use store::{CompactStats, CompactionJob, NewRecord, Options, PreparedSegment, Record, Stats, Store};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Stored bytes failed a structural or checksum check.
    Corrupt(&'static str),
    /// The caller passed an invalid argument or the store is in use.
    Invalid(&'static str),
    Codec(&'static str),
    NotFound,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Corrupt(m) => write!(f, "corrupt store: {m}"),
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Codec(m) => write!(f, "codec: {m}"),
            Error::NotFound => write!(f, "no such record"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Small deterministic helpers shared by the benchmarks.
pub mod util {
    /// SplitMix64: deterministic, seedable, dependency-free.
    pub struct Rng(u64);
    impl Rng {
        pub fn new(seed: u64) -> Self {
            Self(seed)
        }
        pub fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        /// Uniform in `0..n` (n > 0).
        pub fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
    }
    /// Nearest-rank percentile; sorts `v` in place. `q` in 0..=1.
    pub fn percentile(v: &mut [f64], q: f64) -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
        v[i]
    }
}
