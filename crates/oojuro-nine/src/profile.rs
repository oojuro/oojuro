//! Optional phase timers (thread CPU time) and work counters for the query
//! path (cargo feature `phase-profile`; docs/PERF_SEARCH.md). Without the
//! feature every call is an empty inline function and [`snapshot`] returns
//! nothing. With query threads > 1 only the calling thread's share of the
//! scan is counted.

/// Phase timers (nanoseconds) and work counters, by index.
pub const NAMES: [&str; 25] = [
    "parse",
    "backoff",
    "direct",
    "learned",
    "dict_lookup",
    "scan",
    "merge_outs",
    "background",
    "gains_init",
    "hit_scoring",
    "scored_build",
    "entropy",
    "select_top",
    "rank_total",
    "search_refs_total",
    "n_chunks",
    "n_terms",
    "n_candidates",
    "n_visited_postings",
    "n_skipped_postings",
    "n_first_touch",
    "n_hits",
    "scan_pass1",
    "scan_pass2",
    "scan_order",
];

pub(crate) const PARSE: usize = 0;
pub(crate) const BACKOFF: usize = 1;
pub(crate) const DIRECT: usize = 2;
pub(crate) const LEARNED: usize = 3;
pub(crate) const DICT: usize = 4;
pub(crate) const SCAN: usize = 5;
pub(crate) const MERGE: usize = 6;
pub(crate) const BACKGROUND: usize = 7;
pub(crate) const GAINS: usize = 8;
pub(crate) const HITS: usize = 9;
pub(crate) const SCORED: usize = 10;
pub(crate) const ENTROPY: usize = 11;
pub(crate) const SELECT: usize = 12;
pub(crate) const RANK: usize = 13;
pub(crate) const SEARCH: usize = 14;
pub(crate) const N_CHUNKS: usize = 15;
pub(crate) const N_TERMS: usize = 16;
pub(crate) const N_CANDIDATES: usize = 17;
pub(crate) const N_VISITED: usize = 18;
pub(crate) const N_SKIPPED: usize = 19;
pub(crate) const N_FIRST_TOUCH: usize = 20;
pub(crate) const N_HITS: usize = 21;
pub(crate) const SCAN_PASS1: usize = 22;
pub(crate) const SCAN_PASS2: usize = 23;
pub(crate) const SCAN_ORDER: usize = 24;

#[cfg(feature = "phase-profile")]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static COUNTERS: [AtomicU64; super::NAMES.len()] =
        [const { AtomicU64::new(0) }; super::NAMES.len()];

    /// Thread CPU time (Linux CLOCK_THREAD_CPUTIME_ID), so phase shares stay
    /// meaningful on a loaded machine.
    #[derive(Clone, Copy)]
    pub struct Lap(u64);

    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
    }
    fn now() -> u64 {
        let mut t = Timespec { sec: 0, nsec: 0 };
        // SAFETY: valid out pointer; clock id 3 is CLOCK_THREAD_CPUTIME_ID.
        unsafe { clock_gettime(3, &mut t) };
        t.sec as u64 * 1_000_000_000 + t.nsec as u64
    }

    #[inline]
    pub fn start() -> Lap {
        Lap(now())
    }
    #[inline]
    pub fn lap(t: &mut Lap, i: usize) {
        let n = now();
        COUNTERS[i].fetch_add(n.saturating_sub(t.0), Ordering::Relaxed);
        t.0 = n;
    }
    #[inline]
    pub fn since(t: &Lap, i: usize) {
        COUNTERS[i].fetch_add(now().saturating_sub(t.0), Ordering::Relaxed);
    }
    #[inline]
    pub fn count(i: usize, n: u64) {
        COUNTERS[i].fetch_add(n, Ordering::Relaxed);
    }
    pub fn snapshot() -> Vec<(&'static str, u64)> {
        super::NAMES
            .iter()
            .zip(COUNTERS.iter())
            .map(|(n, c)| (*n, c.load(Ordering::Relaxed)))
            .collect()
    }
    pub fn reset() {
        COUNTERS.iter().for_each(|c| c.store(0, Ordering::Relaxed));
    }
}

#[cfg(not(feature = "phase-profile"))]
mod imp {
    #[derive(Clone, Copy)]
    pub struct Lap;

    #[inline(always)]
    pub fn start() -> Lap {
        Lap
    }
    #[inline(always)]
    pub fn lap(_: &mut Lap, _: usize) {}
    #[inline(always)]
    pub fn since(_: &Lap, _: usize) {}
    #[inline(always)]
    pub fn count(_: usize, _: u64) {}
    pub fn snapshot() -> Vec<(&'static str, u64)> {
        Vec::new()
    }
    pub fn reset() {}
}

pub(crate) use imp::{count, lap, since, start};
pub use imp::{reset, snapshot};
