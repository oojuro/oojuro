//! The traditional-database suite (`tracebench ycsb`): YCSB's core
//! workloads A-F run through [`TraceStore`], so the same command times every
//! arm. Spec: "Traditional suite (ycsb)" in `docs/TRACEBENCH.md`.
//!
//! Mapping onto the trace contract
//! - A YCSB record is one event: a `tool_call` (the kind whose status the
//!   contract revises), id = key + 1 for loaded keys, in a session of 1-100
//!   records (seeded sizes), sessions dealt round-robin to 16 agents, event
//!   time `T0 + key * 2 s` (a time-range scan over `len` keys is a key-range
//!   scan). Payload: a real event's text, tool and args from the `.tbe` data
//!   (default; cycles if more records than events), or YCSB's synthetic
//!   record: 10 fields x 100 random printable bytes as JSON in `args`
//!   (unindexed, like a YCSB table; `text` empty).
//! - read = `get`; update = an appended status revision (new status, exit
//!   code, `duration_us` = the revision's id, i.e. a new version; the store
//!   is append-only); insert = a new record; scan = `time_range` over 1-100
//!   keys or `session`; read-modify-write = `get`, then a revision derived
//!   from what was read (status flipped, exit code + 1).
//! - Request distributions as in YCSB's CoreWorkload: scrambled zipfian
//!   (theta 0.99, FNV-hashed over a 10^10-item zipfian), uniform over the
//!   loaded keys, and "latest" (zipfian over recency) for D; inserted keys
//!   become readable once acknowledged (YCSB's acknowledged counter).
use crate::{
    Event, Result, Row, TraceStore, arms,
    arms::oracle::OwnedOracle,
    events::{ST_ERROR, ST_OK, ST_PENDING, TOOL_CALL, Tier},
    stats::Lat,
    sys,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashSet},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// ---------------------------------------------------------------- random numbers

/// SplitMix64: small, fast, seedable; every draw of the suite comes from one.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

fn mix(a: u64, b: u64) -> u64 {
    Rng(a ^ b.wrapping_mul(0xD6E8_FEB8_6659_FD93)).next_u64()
}

// ---------------------------------------------------------------- YCSB generators

/// YCSB's default zipfian constant.
pub const ZIPFIAN_CONSTANT: f64 = 0.99;
/// ScrambledZipfianGenerator draws from a zipfian over this many items and
/// hashes into the key range; zeta for it is precomputed (YCSB constants).
pub const SCRAMBLED_ITEM_COUNT: u64 = 10_000_000_000;
pub const SCRAMBLED_ZETAN: f64 = 26.469_028_201_783_02;

/// zeta(n) = sum_{i=st+1..n} 1/i^theta, plus `initial` (YCSB's `zeta`).
pub fn zeta(st: u64, n: u64, theta: f64, initial: f64) -> f64 {
    let mut sum = initial;
    for i in st..n {
        sum += 1.0 / ((i + 1) as f64).powf(theta);
    }
    sum
}

/// YCSB's ZipfianGenerator (Gray et al., "Quickly generating billion-record
/// synthetic databases"): item `base` is the most popular. The item count
/// may grow between draws; zeta is then extended incrementally.
#[derive(Clone, Debug)]
pub struct Zipfian {
    base: u64,
    items: u64,
    theta: f64,
    alpha: f64,
    zeta2theta: f64,
    zetan: f64,
    eta: f64,
    count_for_zeta: u64,
}

impl Zipfian {
    /// Items `min..=max`, popularity falling with distance from `min`.
    pub fn new(min: u64, max: u64, theta: f64) -> Self {
        let items = max - min + 1;
        Self::with_zetan(min, max, theta, zeta(0, items, theta, 0.0))
    }

    pub fn with_zetan(min: u64, max: u64, theta: f64, zetan: f64) -> Self {
        let items = max - min + 1;
        let mut z = Self {
            base: min,
            items,
            theta,
            alpha: 1.0 / (1.0 - theta),
            zeta2theta: zeta(0, 2, theta, 0.0),
            zetan,
            eta: 0.0,
            count_for_zeta: items,
        };
        z.eta = z.eta_for(items);
        z
    }

    fn eta_for(&self, n: u64) -> f64 {
        (1.0 - (2.0 / n as f64).powf(1.0 - self.theta)) / (1.0 - self.zeta2theta / self.zetan)
    }

    pub fn zetan(&self) -> f64 {
        self.zetan
    }

    pub fn next(&mut self, rng: &mut Rng) -> u64 {
        let n = self.items;
        self.next_n(rng, n)
    }

    /// A draw from `base..base + n` (YCSB's `nextLong(itemcount)`).
    pub fn next_n(&mut self, rng: &mut Rng, n: u64) -> u64 {
        let n = n.max(1);
        if n != self.count_for_zeta {
            self.zetan = if n > self.count_for_zeta {
                zeta(self.count_for_zeta, n, self.theta, self.zetan)
            } else {
                zeta(0, n, self.theta, 0.0)
            };
            self.count_for_zeta = n;
            self.eta = self.eta_for(n);
        }
        let u = rng.f64();
        let uz = u * self.zetan;
        if uz < 1.0 {
            return self.base;
        }
        if uz < 1.0 + 0.5f64.powf(self.theta) {
            return self.base + 1;
        }
        let v = (n as f64 * (self.eta * u - self.eta + 1.0).powf(self.alpha)) as u64;
        self.base + v.min(n - 1)
    }
}

/// YCSB's `Utils.fnvhash64` (FNV-1a over the 8 bytes, low first; abs).
pub fn fnvhash64(mut v: u64) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for _ in 0..8 {
        h ^= v & 0xff;
        h = h.wrapping_mul(1_099_511_628_211);
        v >>= 8;
    }
    (h as i64).unsigned_abs()
}

/// YCSB's ScrambledZipfianGenerator: zipfian popularity, but the popular
/// items are spread over the key range instead of clustered at its start.
#[derive(Clone, Debug)]
pub struct ScrambledZipfian {
    min: u64,
    items: u64,
    zipf: Zipfian,
}

impl ScrambledZipfian {
    pub fn new(min: u64, max: u64) -> Self {
        Self {
            min,
            items: max - min + 1,
            zipf: Zipfian::with_zetan(0, SCRAMBLED_ITEM_COUNT, ZIPFIAN_CONSTANT, SCRAMBLED_ZETAN),
        }
    }
    pub fn next(&mut self, rng: &mut Rng) -> u64 {
        self.min + fnvhash64(self.zipf.next(rng)) % self.items
    }
}

/// YCSB's SkewedLatestGenerator: `max - zipfian(max)`, so the newest keys
/// are the most popular; `max` is the newest acknowledged key.
#[derive(Clone, Debug)]
pub struct Latest {
    zipf: Zipfian,
}

impl Latest {
    pub fn new(items: u64) -> Self {
        Self { zipf: Zipfian::new(0, items.max(1) - 1, ZIPFIAN_CONSTANT) }
    }
    pub fn next(&mut self, rng: &mut Rng, max: u64) -> u64 {
        max - self.zipf.next_n(rng, max.max(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dist {
    Zipfian,
    Uniform,
}

impl Dist {
    pub fn name(self) -> &'static str {
        match self {
            Dist::Zipfian => "zipfian",
            Dist::Uniform => "uniform",
        }
    }
}

/// The key chooser of one cell (YCSB `CoreWorkload.nextKeynum`).
#[derive(Clone, Debug)]
pub enum Chooser {
    /// Uniform over the keys present when the cell started.
    Uniform(u64),
    /// Scrambled zipfian over present keys + expected inserts; draws above
    /// the newest acknowledged key are redrawn.
    Zipf(ScrambledZipfian),
    Latest(Latest),
}

impl Chooser {
    pub fn next(&mut self, rng: &mut Rng, keys: &Keys) -> u64 {
        let last = keys.last();
        match self {
            Chooser::Uniform(n) => rng.below(*n),
            Chooser::Zipf(z) => loop {
                let k = z.next(rng);
                if k <= last {
                    return k;
                }
            },
            Chooser::Latest(l) => l.next(rng, last),
        }
    }
}

// ---------------------------------------------------------------- keys

/// Key space: keys `0..loaded` are the loaded records (id = key + 1); later
/// keys are inserts with ids from the shared event-id counter (status
/// revisions take ids from it too). Inserted keys become choosable once they
/// and every key before them are acknowledged (YCSB's
/// AcknowledgedCounterGenerator).
pub struct Keys {
    loaded: u64,
    next_key: AtomicU64,
    limit: AtomicU64,
    pending: Mutex<BTreeSet<u64>>,
    next_id: AtomicU64,
    /// Event id of inserted key `loaded + i`, in lazily allocated chunks.
    ins_ids: Vec<std::sync::OnceLock<Box<[AtomicU64]>>>,
}

const ID_CHUNK: usize = 1 << 16;

impl Keys {
    pub fn new(loaded: u64, insert_capacity: u64) -> Self {
        Self {
            loaded,
            next_key: AtomicU64::new(loaded),
            limit: AtomicU64::new(loaded),
            pending: Mutex::new(BTreeSet::new()),
            next_id: AtomicU64::new(loaded + 1),
            ins_ids: (0..(insert_capacity as usize).div_ceil(ID_CHUNK)).map(|_| std::sync::OnceLock::new()).collect(),
        }
    }
    fn slot(&self, i: u64) -> &AtomicU64 {
        let i = i as usize;
        let chunk = self.ins_ids.get(i / ID_CHUNK).expect("insert capacity exhausted");
        &chunk.get_or_init(|| (0..ID_CHUNK).map(|_| AtomicU64::new(0)).collect())[i % ID_CHUNK]
    }
    /// Keys acknowledged so far (all of `0..count`).
    pub fn count(&self) -> u64 {
        self.limit.load(Ordering::Acquire)
    }
    /// The newest acknowledged key.
    pub fn last(&self) -> u64 {
        self.count().saturating_sub(1)
    }
    /// A new key and its event id.
    pub fn alloc_insert(&self) -> (u64, u64) {
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        let slot = self.slot(key - self.loaded);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        slot.store(id, Ordering::Release);
        (key, id)
    }
    /// A new event id (a status revision).
    pub fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
    pub fn ack(&self, key: u64) {
        let mut p = self.pending.lock().unwrap();
        let mut l = self.limit.load(Ordering::Relaxed);
        if key != l {
            p.insert(key);
            return;
        }
        l += 1;
        while p.remove(&l) {
            l += 1;
        }
        self.limit.store(l, Ordering::Release);
    }
    pub fn id_of(&self, key: u64) -> u64 {
        if key < self.loaded { key + 1 } else { self.slot(key - self.loaded).load(Ordering::Acquire) }
    }
    pub fn loaded(&self) -> u64 {
        self.loaded
    }
    /// Event ids handed out so far (records and revisions).
    pub fn ids(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed) - 1
    }
}

// ---------------------------------------------------------------- record layout

/// 2026-09-01T00:00:00Z, the data's epoch.
pub const T0_US: i64 = 1_788_220_800_000_000;
/// Event time between consecutive keys (1.3M keys span 30 days, like the data).
pub const STEP_US: i64 = 2_000_000;
pub const AGENTS: u64 = 16;
pub const MAX_SCAN: u64 = 100;

/// Sessions (seeded sizes 1-100), agents and times of every key.
pub struct Layout {
    starts: Vec<u64>,
}

impl Layout {
    /// Covers keys `0..keys`.
    pub fn new(keys: u64, seed: u64) -> Self {
        let mut rng = Rng(mix(seed, 0x5E55_1015));
        let mut starts = Vec::new();
        let mut k = 0;
        while k < keys.max(1) {
            starts.push(k);
            k += 1 + rng.below(MAX_SCAN);
        }
        starts.push(k);
        Self { starts }
    }
    fn idx(&self, key: u64) -> usize {
        let i = self.starts.partition_point(|&s| s <= key);
        assert!(i < self.starts.len(), "key {key} beyond the layout");
        i - 1
    }
    pub fn session(&self, key: u64) -> u64 {
        self.idx(key) as u64 + 1
    }
    pub fn agent(&self, key: u64) -> u32 {
        (self.idx(key) as u64 % AGENTS) as u32
    }
    pub fn seq(&self, key: u64) -> u32 {
        (key - self.starts[self.idx(key)]) as u32
    }
    /// Keys of a session (1-based id).
    pub fn session_keys(&self, session: u64) -> std::ops::Range<u64> {
        let i = session as usize - 1;
        self.starts[i]..self.starts[i + 1]
    }
    pub fn ts(&self, key: u64) -> i64 {
        T0_US + key as i64 * STEP_US
    }
    pub fn sessions(&self) -> usize {
        self.starts.len() - 1
    }
}

pub const FIELDS: usize = 10;
pub const FIELD_BYTES: usize = 100;

/// Printable ASCII without `"` and `\` (YCSB fills fields with random
/// printable bytes; these two would need JSON escapes).
fn alphabet() -> &'static [u8] {
    static A: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    A.get_or_init(|| (0x20u8..=0x7E).filter(|c| *c != b'"' && *c != b'\\').collect())
}

/// YCSB's synthetic record for `key`: `{"field0":"<100 bytes>",...,"field9":...}`,
/// deterministic in (seed, key).
pub fn synthetic_payload(seed: u64, key: u64, out: &mut String) {
    let a = alphabet();
    let mut rng = Rng(mix(seed ^ 0x4C5B_F1E1, key));
    let mut bytes = Vec::with_capacity(FIELDS * (FIELD_BYTES + 12) + 2);
    bytes.push(b'{');
    for f in 0..FIELDS {
        if f > 0 {
            bytes.push(b',');
        }
        bytes.extend_from_slice(format!("\"field{f}\":\"").as_bytes());
        let mut left = FIELD_BYTES;
        while left > 0 {
            let mut x = rng.next_u64();
            for _ in 0..8.min(left) {
                bytes.push(a[((x & 0xff) as usize * a.len()) >> 8]);
                x >>= 8;
                left -= 1;
            }
        }
        bytes.push(b'"');
    }
    bytes.push(b'}');
    out.clear();
    out.push_str(std::str::from_utf8(&bytes).expect("ascii"));
}

/// Where record payloads come from.
pub enum Source<'t> {
    /// Real events of the tier, in canonical order (revisions skipped):
    /// (plan entry, position in its batch).
    Real { tier: &'t Tier<'t>, index: Vec<(u32, u32)> },
    Synthetic { seed: u64 },
}

/// Per-thread decode cache for real payloads.
pub struct Cursor<'t> {
    last: usize,
    evs: Vec<Event<'t>>,
}

impl Default for Cursor<'_> {
    fn default() -> Self {
        Self { last: usize::MAX, evs: Vec::new() }
    }
}

impl<'t> Source<'t> {
    pub fn real(tier: &'t Tier<'t>) -> Self {
        let mut index = Vec::new();
        let mut b = Vec::new();
        for i in 0..tier.plan.len() {
            tier.batch(i, &mut b);
            for (k, e) in b.iter().enumerate() {
                if !e.is_revision() {
                    index.push((i as u32, k as u32));
                }
            }
        }
        Source::Real { tier, index }
    }
    pub fn synthetic(seed: u64) -> Self {
        Source::Synthetic { seed }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Source::Real { .. } => "real",
            Source::Synthetic { .. } => "synthetic",
        }
    }
    /// Distinct real payloads (None = synthetic, unbounded).
    pub fn distinct(&self) -> Option<u64> {
        match self {
            Source::Real { index, .. } => Some(index.len() as u64),
            Source::Synthetic { .. } => None,
        }
    }
    /// The real event whose payload record `key` carries (None: synthetic).
    pub fn event(&self, key: u64, cur: &mut Cursor<'t>) -> Option<Event<'t>> {
        match self {
            Source::Synthetic { .. } => None,
            Source::Real { tier, index } => {
                let (i, k) = index[(key % index.len() as u64) as usize];
                if cur.last != i as usize {
                    tier.batch(i as usize, &mut cur.evs);
                    cur.last = i as usize;
                }
                Some(cur.evs[k as usize])
            }
        }
    }
    /// Fill `buf` with the synthetic payload of `key` (no-op for real data).
    pub fn fill(&self, key: u64, buf: &mut String) {
        if let Source::Synthetic { seed } = self {
            synthetic_payload(*seed, key, buf);
        }
    }
}

/// The record event of `key` (payload from `real`, or `synth` as args).
pub fn record<'a>(lay: &Layout, key: u64, id: u64, real: Option<Event<'a>>, synth: &'a str) -> Event<'a> {
    let (tool, args, text) = match real {
        Some(e) => (e.tool, e.args, e.text),
        None => ("ycsb", synth, ""),
    };
    Event {
        id,
        session: lay.session(key),
        agent: lay.agent(key),
        turn: 0,
        seq: lay.seq(key),
        parent: 0,
        revises: 0,
        kind: TOOL_CALL,
        status: ST_PENDING,
        exit_code: None,
        tokens_in: 0,
        tokens_out: (text.len() + args.len()).div_ceil(4) as u32,
        ts_us: lay.ts(key),
        duration_us: 0,
        tool,
        args,
        text,
    }
}

/// A status revision of record `key` (call id `call`): the update of YCSB,
/// a new version (`duration_us` = the revision's id).
pub fn revision(lay: &Layout, key: u64, call: u64, rev: u64, status: u8, exit: i32) -> Event<'static> {
    Event {
        id: rev,
        session: lay.session(key),
        agent: lay.agent(key),
        turn: 0,
        seq: 0,
        parent: call,
        revises: call,
        kind: TOOL_CALL,
        status,
        exit_code: Some(exit),
        tokens_in: 0,
        tokens_out: 0,
        ts_us: lay.ts(key) + 1,
        duration_us: rev as i64,
        tool: "",
        args: "",
        text: "",
    }
}

/// The row `key` reads back right after the load (no revision yet).
pub fn expected_row<'t>(lay: &Layout, src: &Source<'t>, key: u64, id: u64, cur: &mut Cursor<'t>) -> Row {
    let mut buf = String::new();
    src.fill(key, &mut buf);
    let real = src.event(key, cur);
    Row::from_event(&record(lay, key, id, real, &buf))
}

// ---------------------------------------------------------------- workloads

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Update,
    Insert,
    Scan,
    Rmw,
}

const LAT_NAMES: [&str; 6] = ["read", "update", "insert", "scan", "read_modify_write", "commit"];

impl Op {
    fn lat(self) -> usize {
        match self {
            Op::Read => 0,
            Op::Update => 1,
            Op::Insert => 2,
            Op::Scan => 3,
            Op::Rmw => 4,
        }
    }
}

/// One of YCSB's core workloads (operation mix; D reads the latest keys).
#[derive(Clone, Copy, Debug)]
pub struct Workload {
    pub name: char,
    pub read: f64,
    pub update: f64,
    pub insert: f64,
    pub scan: f64,
    pub rmw: f64,
    pub latest: bool,
}

impl Workload {
    pub fn get(name: char) -> Option<Self> {
        let w = Workload { name, read: 0.0, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.0, latest: false };
        Some(match name.to_ascii_lowercase() {
            'a' => Workload { read: 0.5, update: 0.5, ..w },
            'b' => Workload { read: 0.95, update: 0.05, ..w },
            'c' => Workload { read: 1.0, ..w },
            'd' => Workload { read: 0.95, insert: 0.05, latest: true, ..w },
            'e' => Workload { scan: 0.95, insert: 0.05, ..w },
            'f' => Workload { read: 0.5, rmw: 0.5, ..w },
            _ => return None,
        })
    }
    pub fn pick(&self, u: f64) -> Op {
        let mut c = self.read;
        if u < c {
            return Op::Read;
        }
        c += self.update;
        if u < c {
            return Op::Update;
        }
        c += self.insert;
        if u < c {
            return Op::Insert;
        }
        c += self.scan;
        if u < c {
            return Op::Scan;
        }
        if self.rmw > 0.0 { Op::Rmw } else { Op::Read }
    }
    pub fn writes(&self) -> bool {
        self.update + self.insert + self.rmw > 0.0
    }
    pub fn mix(&self) -> String {
        let mut v = Vec::new();
        for (p, n) in [
            (self.read, if self.latest { "read latest" } else { "read" }),
            (self.update, "update"),
            (self.insert, "insert"),
            (self.scan, "scan"),
            (self.rmw, "read-modify-write"),
        ] {
            if p > 0.0 {
                v.push(format!("{}% {n}", (p * 100.0).round()));
            }
        }
        v.join(" / ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanKind {
    Time,
    Session,
}

// ---------------------------------------------------------------- configuration

#[derive(Clone, Debug)]
pub struct Config {
    /// Records to load (default: every real event of the tier; 1M synthetic).
    pub records: Option<u64>,
    pub synthetic: bool,
    /// Operations per cell (workload x distribution x threads x batch).
    pub ops: u64,
    pub workloads: Vec<char>,
    pub threads: Vec<usize>,
    /// Writes per commit: 1 = every write durable on its own; >1 = batched.
    pub batches: Vec<usize>,
    pub dists: Vec<Dist>,
    pub scan: ScanKind,
    pub writers: Vec<usize>,
    pub writer_ops: u64,
    pub phases: Vec<String>,
    /// Compare every read and scan with the in-memory oracle (forces one
    /// client thread in the workload cells; timings are then not results).
    pub check: bool,
    /// Stop a cell (and each writer-sweep point) after this many seconds
    /// (0 = run all its ops).
    pub cell_seconds: f64,
    pub crash_acks: u64,
    pub crash_writers: usize,
    /// Records the crash child would write if never killed (default 50 x acks).
    pub crash_records: u64,
    pub crash_wait_s: f64,
    pub seed: u64,
    pub load_threads: usize,
    pub data: PathBuf,
    pub tier: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            records: None,
            synthetic: false,
            ops: 10_000,
            workloads: "abcfde".chars().collect(),
            threads: vec![1, 2, 4, 8, 16],
            batches: vec![1, 10],
            dists: vec![Dist::Zipfian],
            scan: ScanKind::Time,
            writers: vec![1, 2, 4, 8, 16],
            writer_ops: 0,
            phases: ["load", "open", "run", "writers", "crash"].map(String::from).to_vec(),
            check: false,
            cell_seconds: 0.0,
            crash_acks: 2_000,
            crash_writers: 4,
            crash_records: 0,
            crash_wait_s: 120.0,
            seed: 1,
            load_threads: 4,
            data: PathBuf::new(),
            tier: 1,
        }
    }
}

fn list<T: std::str::FromStr>(v: &str, what: &str) -> Vec<T> {
    v.split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or_else(|_| panic!("bad {what} value {s:?}")))
        .collect()
}

impl Config {
    /// Parse one `--ycsb-*` flag; false if the flag is not ours.
    pub fn set(&mut self, flag: &str, v: &str) -> bool {
        match flag {
            "--ycsb-records" => self.records = Some(v.parse().expect("--ycsb-records")),
            "--ycsb-data" => {
                self.synthetic = match v {
                    "real" => false,
                    "synthetic" => true,
                    _ => panic!("--ycsb-data real|synthetic"),
                }
            }
            "--ycsb-ops" => self.ops = v.parse().expect("--ycsb-ops"),
            "--ycsb-workloads" => {
                self.workloads = v.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()).collect();
                for w in &self.workloads {
                    assert!(Workload::get(*w).is_some(), "unknown workload {w}");
                }
            }
            "--ycsb-threads" => self.threads = list(v, "--ycsb-threads"),
            "--ycsb-batch" => self.batches = list(v, "--ycsb-batch"),
            "--ycsb-dist" => {
                self.dists = v
                    .split(',')
                    .map(|d| match d {
                        "zipfian" => Dist::Zipfian,
                        "uniform" => Dist::Uniform,
                        _ => panic!("--ycsb-dist zipfian,uniform"),
                    })
                    .collect()
            }
            "--ycsb-scan" => {
                self.scan = match v {
                    "time" => ScanKind::Time,
                    "session" => ScanKind::Session,
                    _ => panic!("--ycsb-scan time|session"),
                }
            }
            "--ycsb-writers" => self.writers = list(v, "--ycsb-writers"),
            "--ycsb-writer-ops" => self.writer_ops = v.parse().expect("--ycsb-writer-ops"),
            "--ycsb-phases" => self.phases = v.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
            "--ycsb-cell-seconds" => self.cell_seconds = v.parse().expect("--ycsb-cell-seconds"),
            "--ycsb-crash-acks" => self.crash_acks = v.parse().expect("--ycsb-crash-acks"),
            "--ycsb-crash-writers" => self.crash_writers = v.parse().expect("--ycsb-crash-writers"),
            "--ycsb-crash-records" => self.crash_records = v.parse().expect("--ycsb-crash-records"),
            "--ycsb-crash-wait-s" => self.crash_wait_s = v.parse().expect("--ycsb-crash-wait-s"),
            _ => return false,
        }
        true
    }

    pub fn phase(&self, p: &str) -> bool {
        self.phases.iter().any(|x| x == p)
    }

    fn writer_ops(&self) -> u64 {
        if self.writer_ops == 0 { self.ops } else { self.writer_ops }
    }

    fn crash_records(&self) -> u64 {
        if self.crash_records == 0 { self.crash_acks * 50 } else { self.crash_records }
    }

    /// The workload cells in run order.
    fn cells(&self) -> Vec<(Workload, Dist, usize, usize)> {
        let threads = if self.check { vec![1] } else { self.threads.clone() };
        let mut v = Vec::new();
        for &w in &self.workloads {
            let wl = Workload::get(w).expect("workload");
            // D always draws from the latest distribution: one pass
            let dists = if wl.latest { vec![Dist::Zipfian] } else { self.dists.clone() };
            for &d in &dists {
                for &b in &self.batches {
                    if b > 1 && !wl.writes() {
                        continue; // nothing to batch
                    }
                    for &t in &threads {
                        v.push((wl, d, t, b));
                    }
                }
            }
        }
        v
    }

    fn to_json(&self, records: u64, src: &Source) -> Value {
        let threads = if self.check { vec![1] } else { self.threads.clone() };
        let scan = if self.scan == ScanKind::Time { "time" } else { "session" };
        json!({
            "records": records, "data": src.name(), "distinct_payloads": src.distinct(),
            "ops_per_cell": self.ops, "workloads": self.workloads.iter().collect::<String>(),
            "threads": threads, "batches": self.batches,
            "dists": self.dists.iter().map(|d| d.name()).collect::<Vec<_>>(),
            "scan": scan,
            "writers": self.writers, "writer_ops": self.writer_ops(), "phases": self.phases, "check": self.check,
            "cell_seconds": self.cell_seconds, "crash_acks": self.crash_acks, "crash_writers": self.crash_writers,
            "seed": self.seed, "load_threads": self.load_threads,
            "layout": {"t0_us": T0_US, "step_us": STEP_US, "agents": AGENTS, "session_records": "1-100 (seeded)"},
        })
    }
}

// ---------------------------------------------------------------- running ops

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim().to_string()
}

struct Ctx<'a, 't> {
    store: &'a dyn TraceStore,
    lay: &'a Layout,
    src: &'a Source<'t>,
    keys: &'a Keys,
    oracle: Option<&'a OwnedOracle>,
    scan: ScanKind,
}

/// A write waiting for its commit.
struct Pending {
    op: Op,
    key: u64,
    /// Record id (insert) or the revised call's id (update, rmw).
    id: u64,
    rev: u64,
    status: u8,
    exit: i32,
    start: Instant,
}

#[derive(Default)]
struct Out {
    lat: [Lat; 6],
    misses: u64,
    checks: u64,
    fails: u64,
    failures: Vec<String>,
}

impl Out {
    fn check<T: PartialEq>(&mut self, got: &T, want: &T, what: impl FnOnce() -> String) {
        self.checks += 1;
        if got != want {
            self.fails += 1;
            if self.failures.len() < 10 {
                self.failures.push(what());
            }
        }
    }
    fn merge(&mut self, o: Out) {
        for (a, b) in self.lat.iter_mut().zip(o.lat.iter()) {
            a.merge(b);
        }
        self.misses += o.misses;
        self.checks += o.checks;
        self.fails += o.fails;
        for f in o.failures {
            if self.failures.len() < 20 {
                self.failures.push(f);
            }
        }
    }
}

/// Claim one op of a shared budget.
fn claim(budget: &AtomicU64) -> bool {
    let mut r = budget.load(Ordering::Relaxed);
    while r > 0 {
        match budget.compare_exchange_weak(r, r - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(x) => r = x,
        }
    }
    false
}

/// Commit pending writes: one `append` (batch 1) or one `append_many`;
/// each write's latency runs from its op start until the commit returns.
fn commit<'t>(ctx: &Ctx<'_, 't>, pend: &mut Vec<Pending>, cur: &mut Cursor<'t>, out: &mut Out) -> Result<()> {
    if pend.is_empty() {
        return Ok(());
    }
    let mut bufs: Vec<String> = vec![String::new(); pend.len()];
    let mut reals: Vec<Option<Event<'t>>> = vec![None; pend.len()];
    for (i, p) in pend.iter().enumerate() {
        if p.op == Op::Insert {
            ctx.src.fill(p.key, &mut bufs[i]);
            reals[i] = ctx.src.event(p.key, cur);
        }
    }
    let batches: Vec<Vec<Event>> = pend
        .iter()
        .enumerate()
        .map(|(i, p)| {
            vec![match p.op {
                Op::Insert => record(ctx.lay, p.key, p.id, reals[i], &bufs[i]),
                _ => revision(ctx.lay, p.key, p.id, p.rev, p.status, p.exit),
            }]
        })
        .collect();
    let t = Instant::now();
    if batches.len() == 1 {
        ctx.store.append(&batches[0])?;
    } else {
        ctx.store.append_many(&batches)?;
        out.lat[5].add(t);
        out.lat[5].items += batches.len() as u64;
    }
    for p in pend.iter() {
        out.lat[p.op.lat()].add(p.start);
    }
    if let Some(o) = ctx.oracle {
        o.append_many(&batches)?;
    }
    for p in pend.iter() {
        if p.op == Op::Insert {
            ctx.keys.ack(p.key);
        }
    }
    pend.clear();
    Ok(())
}

/// One client thread of a cell: closed loop over the shared op budget.
fn client<'t>(
    ctx: &Ctx<'_, 't>,
    wl: Workload,
    mut chooser: Chooser,
    batch: usize,
    budget: &AtomicU64,
    deadline: Option<Instant>,
    seed: u64,
) -> Result<Out> {
    let mut rng = Rng(seed);
    let mut out = Out::default();
    let mut pend: Vec<Pending> = Vec::new();
    let mut cur = Cursor::default();
    let (lay, keys, store) = (ctx.lay, ctx.keys, ctx.store);
    while claim(budget) {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let op = wl.pick(rng.f64());
        let t = Instant::now();
        match op {
            Op::Read => {
                let k = chooser.next(&mut rng, keys);
                let id = keys.id_of(k);
                let r = store.get(id)?;
                out.lat[0].add(t);
                out.misses += r.is_none() as u64;
                if let Some(o) = ctx.oracle {
                    out.check(&r, &o.get(id)?, || format!("{}: read key {k} (id {id})", wl.name));
                }
            }
            Op::Scan => {
                let k = chooser.next(&mut rng, keys);
                let len = 1 + rng.below(MAX_SCAN);
                let (rows, want) = match ctx.scan {
                    ScanKind::Time => {
                        let (from, to) = (lay.ts(k), lay.ts(k) + len as i64 * STEP_US);
                        let r = store.time_range(from, to, len as usize)?;
                        out.lat[3].add(t);
                        (r, ctx.oracle.map(|o| o.time_range(from, to, len as usize)).transpose()?)
                    }
                    ScanKind::Session => {
                        let s = lay.session(k);
                        let r = store.session(s)?;
                        out.lat[3].add(t);
                        (r, ctx.oracle.map(|o| o.session(s)).transpose()?)
                    }
                };
                out.lat[3].items += rows.len() as u64;
                out.misses += rows.is_empty() as u64;
                if let Some(w) = want {
                    out.check(&rows, &w, || format!("{}: scan from key {k}, {len} keys", wl.name));
                }
            }
            Op::Update => {
                let k = chooser.next(&mut rng, keys);
                let rev = keys.alloc_id();
                pend.push(Pending {
                    op,
                    key: k,
                    id: keys.id_of(k),
                    rev,
                    status: ST_OK,
                    exit: (rev % 251) as i32,
                    start: t,
                });
            }
            Op::Insert => {
                let (k, id) = keys.alloc_insert();
                pend.push(Pending { op, key: k, id, rev: 0, status: ST_PENDING, exit: 0, start: t });
            }
            Op::Rmw => {
                let k = chooser.next(&mut rng, keys);
                let id = keys.id_of(k);
                let r = store.get(id)?;
                if let Some(o) = ctx.oracle {
                    out.check(&r, &o.get(id)?, || format!("{}: rmw read key {k} (id {id})", wl.name));
                }
                let Some(r) = r else {
                    out.misses += 1;
                    continue;
                };
                let status = if r.status == ST_OK { ST_ERROR } else { ST_OK };
                let exit = r.exit_code.unwrap_or(0).wrapping_add(1);
                pend.push(Pending { op, key: k, id, rev: keys.alloc_id(), status, exit, start: t });
            }
        }
        if pend.len() >= batch {
            commit(ctx, &mut pend, &mut cur, &mut out)?;
        }
    }
    commit(ctx, &mut pend, &mut cur, &mut out)?;
    Ok(out)
}

/// Removes a server arm's container when dropped, also on an early error
/// return (stores are opened with `keep=on`, so their own drop leaves it).
struct ServerGuard<'a>(&'a str);

impl Drop for ServerGuard<'_> {
    fn drop(&mut self) {
        if arms::is_server(self.0) {
            let _ = arms::stop(self.0);
        }
    }
}

/// Kills and reaps the crash child when dropped (an error return must not
/// leave it writing).
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn sampler(store: &Arc<dyn TraceStore>) -> sys::PeakSampler {
    let s = Arc::clone(store);
    sys::PeakSampler::start(Arc::new(move || s.mem_bytes().unwrap_or(0)))
}

fn durability(batch: usize) -> String {
    if batch <= 1 {
        "durable per write: every update/insert is its own commit, durable on return".into()
    } else {
        format!(
            "BATCHED: {batch} writes per commit (append_many), durable when the commit returns; \
             write latency = op start until its commit returns"
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn run_cell(
    store: &Arc<dyn TraceStore>,
    ctx: &Ctx,
    wl: Workload,
    dist: Dist,
    threads: usize,
    batch: usize,
    cfg: &Config,
    cell: usize,
) -> Result<Value> {
    let n = ctx.keys.count();
    let expected_new = (cfg.ops as f64 * wl.insert * 2.0) as u64;
    let chooser = if wl.latest {
        Chooser::Latest(Latest::new(n))
    } else {
        match dist {
            Dist::Uniform => Chooser::Uniform(n),
            Dist::Zipfian => Chooser::Zipf(ScrambledZipfian::new(0, n + expected_new)),
        }
    };
    let seed = mix(cfg.seed, 0xCE11 + cell as u64);
    let disk_before = store.disk_bytes()?;
    let load_before = loadavg();
    let mem = sampler(store);
    let budget = AtomicU64::new(cfg.ops);
    let deadline = (cfg.cell_seconds > 0.0).then(|| Instant::now() + Duration::from_secs_f64(cfg.cell_seconds));
    let t0 = Instant::now();
    let outs: Vec<Result<Out>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|w| {
                let (chooser, budget) = (chooser.clone(), &budget);
                s.spawn(move || client(ctx, wl, chooser, batch, budget, deadline, mix(seed, w as u64)))
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("client thread")).collect()
    });
    let wall = t0.elapsed().as_secs_f64();
    let mem_peak = mem.finish();
    let mut all = Out::default();
    for o in outs {
        all.merge(o?);
    }
    let done: u64 = all.lat[..5].iter().map(|l| l.len() as u64).sum();
    let dist_name = if wl.latest { "latest" } else { dist.name() };
    let mut v = json!({
        "workload": wl.name.to_string(), "mix": wl.mix(),
        "dist": dist_name, "threads": threads, "batch": batch,
        "durability": durability(batch), "ops": done, "wall_s": wall, "ops_per_s": done as f64 / wall,
        "read_misses": all.misses, "records_before": n, "records_after": ctx.keys.count(),
        "event_ids_after": ctx.keys.ids(),
        "disk_bytes_before": disk_before, "disk_bytes": store.disk_bytes()?,
        "mem_bytes": store.mem_bytes()?, "mem_peak": mem_peak,
        "loadavg_before": load_before, "loadavg_after": loadavg(),
    });
    for (k, l) in all.lat.iter().enumerate() {
        if !l.is_empty() {
            v[LAT_NAMES[k]] = l.summary(None);
        }
    }
    if ctx.oracle.is_some() {
        v["check"] = json!({"checks": all.checks, "failures": all.fails, "first_failures": all.failures});
    }
    eprintln!(
        "ycsb {} {:>7} t={:<2} b={:<2} {:>9.0} ops/s  misses {}{}",
        wl.name,
        dist_name,
        threads,
        batch,
        done as f64 / wall,
        all.misses,
        if ctx.oracle.is_some() { format!("  checks {} failures {}", all.checks, all.fails) } else { String::new() }
    );
    Ok(v)
}

/// Bulk load keys `0..n` (`load` in chunks of 1,000 records from
/// `threads` loaders, then `finish_load`).
fn load_phase(store: &Arc<dyn TraceStore>, ctx: &Ctx, n: u64, threads: usize) -> Result<Value> {
    const CHUNK: u64 = 1000;
    let chunks = n.div_ceil(CHUNK);
    let threads = threads.max(1);
    let load_before = loadavg();
    let mem = sampler(store);
    let raw = AtomicU64::new(0);
    let t0 = Instant::now();
    let res: Vec<Result<()>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads as u64)
            .map(|w| {
                let raw = &raw;
                s.spawn(move || -> Result<()> {
                    let mut cur = Cursor::default();
                    let mut c = w;
                    while c < chunks {
                        let keys = c * CHUNK..((c + 1) * CHUNK).min(n);
                        let mut bufs = vec![String::new(); (keys.end - keys.start) as usize];
                        let mut reals = Vec::with_capacity(bufs.len());
                        for (i, k) in keys.clone().enumerate() {
                            ctx.src.fill(k, &mut bufs[i]);
                            reals.push(ctx.src.event(k, &mut cur));
                        }
                        let evs: Vec<Event> =
                            keys.clone().enumerate().map(|(i, k)| record(ctx.lay, k, k + 1, reals[i], &bufs[i])).collect();
                        raw.fetch_add(evs.iter().map(|e| e.raw_bytes()).sum::<u64>(), Ordering::Relaxed);
                        let batches = [evs];
                        ctx.store.load(&batches)?;
                        if let Some(o) = ctx.oracle {
                            o.load(&batches)?;
                        }
                        c += threads as u64;
                    }
                    Ok(())
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("loader")).collect()
    });
    for r in res {
        r?;
    }
    let load_s = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    store.finish_load()?;
    let finish_s = t1.elapsed().as_secs_f64();
    let raw = raw.into_inner();
    let disk = store.disk_bytes()?;
    eprintln!("ycsb load {n} records in {:.1} s (+{:.1} s settle)", load_s, finish_s);
    Ok(json!({
        "records": n, "raw_bytes": raw, "avg_record_bytes": raw as f64 / n.max(1) as f64,
        "load_threads": threads, "load_s": load_s, "finish_s": finish_s,
        "records_per_s": n as f64 / (load_s + finish_s), "mb_per_s": raw as f64 / (load_s + finish_s) / 1e6,
        "disk_bytes": disk, "bytes_per_raw": disk as f64 / raw.max(1) as f64,
        "mem_peak": mem.finish(), "mem_bytes": store.mem_bytes()?,
        "loadavg_before": load_before, "loadavg_after": loadavg(),
    }))
}

/// Check `sample` random loaded keys against the rows they were loaded with
/// (valid before any update).
fn check_loaded(store: &dyn TraceStore, lay: &Layout, src: &Source, n: u64, sample: usize, seed: u64) -> Result<(u64, Vec<u64>)> {
    let mut rng = Rng(seed);
    let mut cur = Cursor::default();
    let (mut checks, mut bad) = (0u64, Vec::new());
    for _ in 0..sample {
        let k = rng.below(n);
        let want = expected_row(lay, src, k, k + 1, &mut cur);
        checks += 1;
        if store.get(k + 1)?.as_ref() != Some(&want) && bad.len() < 20 {
            bad.push(k);
        }
    }
    Ok((checks, bad))
}

/// Drop the store, shut a server arm down cleanly, then time opening the
/// existing store until its first read returns.
#[allow(clippy::too_many_arguments)]
fn open_phase(
    spec: &str,
    keep_spec: &str,
    work: &Path,
    store: Arc<dyn TraceStore>,
    lay: &Layout,
    src: &Source,
    n: u64,
    seed: u64,
) -> Result<(Arc<dyn TraceStore>, Value)> {
    let disk = store.disk_bytes()?;
    // reopening while another handle keeps the store open would measure (or
    // corrupt) the wrong thing
    if Arc::strong_count(&store) != 1 {
        return Err("open phase: the store is still referenced".into());
    }
    let t = Instant::now();
    drop(store);
    arms::halt(spec)?;
    let close_s = t.elapsed().as_secs_f64();
    let load_before = loadavg();
    let t = Instant::now();
    let reopened: Arc<dyn TraceStore> = Arc::from(arms::reopen(keep_spec, work)?);
    let open_s = t.elapsed().as_secs_f64();
    let k = Rng(mix(seed, 0x0BE4)).below(n.max(1));
    let t1 = Instant::now();
    let first = reopened.get(k + 1)?;
    let first_get_ms = t1.elapsed().as_secs_f64() * 1e3;
    let open_to_first_read_s = t.elapsed().as_secs_f64();
    let (checks, bad) = check_loaded(reopened.as_ref(), lay, src, n, 200, mix(seed, 0x0BE5))?;
    eprintln!("ycsb open: {open_to_first_read_s:.3} s to the first read ({} bad of {checks})", bad.len());
    let v = json!({
        "close_s": close_s, "open_s": open_s, "first_get_ms": first_get_ms,
        "open_to_first_read_s": open_to_first_read_s, "first_read_found": first.is_some(),
        "disk_bytes": disk, "mem_bytes_after_open": reopened.mem_bytes()?,
        "checks": checks, "bad_keys": bad,
        "loadavg_before": load_before, "loadavg_after": loadavg(),
        "note": "embedded arms: process-level open (OS file cache warm); server arms: clean `halt`, then container start + recovery + first query",
    });
    Ok((reopened, v))
}

/// Append-only writers: `w` threads, closed loop, one new record per commit.
fn writers_phase(store: &Arc<dyn TraceStore>, ctx: &Ctx, w: usize, ops: u64, cell_seconds: f64) -> Result<Value> {
    let budget = AtomicU64::new(ops);
    let deadline = (cell_seconds > 0.0).then(|| Instant::now() + Duration::from_secs_f64(cell_seconds));
    let load_before = loadavg();
    let mem = sampler(store);
    let t0 = Instant::now();
    let outs: Vec<Result<Lat>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..w)
            .map(|_| {
                let budget = &budget;
                s.spawn(move || -> Result<Lat> {
                    let mut lat = Lat::default();
                    let (mut cur, mut buf) = (Cursor::default(), String::new());
                    while claim(budget) {
                        if deadline.is_some_and(|d| Instant::now() >= d) {
                            break;
                        }
                        let (k, id) = ctx.keys.alloc_insert();
                        ctx.src.fill(k, &mut buf);
                        let ev = [record(ctx.lay, k, id, ctx.src.event(k, &mut cur), &buf)];
                        let t = Instant::now();
                        ctx.store.append(&ev)?;
                        lat.add(t);
                        lat.items += 1;
                        lat.bytes += ev[0].raw_bytes();
                        if let Some(o) = ctx.oracle {
                            o.append(&ev)?;
                        }
                        ctx.keys.ack(k);
                    }
                    Ok(lat)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("writer")).collect()
    });
    let wall = t0.elapsed().as_secs_f64();
    let mut all = Lat::default();
    for l in outs {
        all.merge(&l?);
    }
    let mut v = all.summary(Some(wall));
    v["writers"] = json!(w);
    v["commits_per_s"] = json!(all.len() as f64 / wall);
    v["durability"] = json!("durable per commit; one new record per commit");
    v["disk_bytes"] = json!(store.disk_bytes()?);
    v["mem_bytes"] = json!(store.mem_bytes()?);
    v["mem_peak"] = json!(mem.finish());
    v["loadavg_before"] = json!(load_before);
    v["loadavg_after"] = json!(loadavg());
    eprintln!("ycsb writers {w:>2}: {:>9.0} commits/s, p99 {:.2} ms", all.len() as f64 / wall, all.pct(0.99));
    Ok(v)
}

/// Check mode: every key's row, and sampled scans, equal the oracle's.
fn final_check(store: &dyn TraceStore, oracle: &OwnedOracle, keys: &Keys, lay: &Layout, seed: u64) -> Result<Value> {
    let mut out = Out::default();
    let n = keys.count();
    let step = (n / 100_000).max(1);
    for k in (0..n).step_by(step as usize) {
        let id = keys.id_of(k);
        out.check(&store.get(id)?, &oracle.get(id)?, || format!("final: key {k} (id {id})"));
    }
    let mut rng = Rng(mix(seed, 0xF1A1));
    for _ in 0..200 {
        let k = rng.below(n);
        let s = lay.session(k);
        out.check(&store.session(s)?, &oracle.session(s)?, || format!("final: session {s}"));
        let len = 1 + rng.below(MAX_SCAN);
        let (from, to) = (lay.ts(k), lay.ts(k) + len as i64 * STEP_US);
        out.check(&store.time_range(from, to, len as usize)?, &oracle.time_range(from, to, len as usize)?, || {
            format!("final: time scan from key {k}, {len} keys")
        });
    }
    eprintln!("ycsb final check: {} checks, {} failures", out.checks, out.fails);
    Ok(json!({"keys": n, "key_step": step, "checks": out.checks, "failures": out.fails, "first_failures": out.failures}))
}

/// The whole suite on a fresh store in `work`: load, open, workload cells,
/// writer sweep, (check), then crash recovery on a second fresh store.
pub fn run(spec: &str, work: &Path, tier: &Tier, cfg: &Config, rec: &mut Value) -> Result<()> {
    let src = if cfg.synthetic { Source::synthetic(cfg.seed) } else { Source::real(tier) };
    let n = cfg.records.unwrap_or(src.distinct().unwrap_or(1_000_000));
    if n == 0 {
        return Err("ycsb needs at least one record".into());
    }
    let cells = cfg.cells();
    let run_cells = cfg.phase("run");
    let writers = if cfg.phase("writers") { cfg.writers.clone() } else { Vec::new() };
    // inserts are bounded by the ops of the cells that insert, plus the writers
    let inserting = cells.iter().filter(|c| c.0.insert > 0.0).count() as u64;
    let cap = if run_cells { inserting * cfg.ops } else { 0 } + writers.len() as u64 * cfg.writer_ops() + 64;
    let lay = Layout::new(n + cap, cfg.seed);
    let keys = Keys::new(n, cap);
    let oracle = cfg.check.then(OwnedOracle::new);
    let keep_spec = arms::with_param(spec, "keep=on");
    let mut out = serde_json::Map::new();
    out.insert("config".into(), cfg.to_json(n, &src));
    // the main store: load, open, cells, writers (a crash-only run skips it)
    let main = ["load", "open", "run", "writers"].iter().any(|p| cfg.phase(p));
    if main {
        let guard = ServerGuard(spec);
        let mut store: Arc<dyn TraceStore> = Arc::from(arms::open(&keep_spec, work)?);
        rec["store"] = json!(store.name());
        {
            let ctx = Ctx { store: store.as_ref(), lay: &lay, src: &src, keys: &keys, oracle: oracle.as_ref(), scan: cfg.scan };
            out.insert("load".into(), load_phase(&store, &ctx, n, cfg.load_threads)?);
            let (checks, bad) = check_loaded(store.as_ref(), &lay, &src, n, 200, mix(cfg.seed, 0x10AD))?;
            out.insert("load_check".into(), json!({"checks": checks, "bad_keys": bad}));
        }
        if cfg.phase("open") {
            if arms::is_memory(spec) {
                out.insert("open".into(), json!({"skipped": "in-memory arm: nothing to reopen"}));
            } else {
                let (s, v) = open_phase(spec, &keep_spec, work, store, &lay, &src, n, cfg.seed)?;
                store = s;
                out.insert("open".into(), v);
            }
        }
        {
            let ctx = Ctx { store: store.as_ref(), lay: &lay, src: &src, keys: &keys, oracle: oracle.as_ref(), scan: cfg.scan };
            if run_cells {
                let mut vs = Vec::new();
                for (i, &(wl, d, t, b)) in cells.iter().enumerate() {
                    vs.push(run_cell(&store, &ctx, wl, d, t, b, cfg, i)?);
                }
                out.insert("cells".into(), Value::Array(vs));
            }
            if !writers.is_empty() {
                let mut vs = Vec::new();
                for &w in &writers {
                    vs.push(writers_phase(&store, &ctx, w, cfg.writer_ops(), cfg.cell_seconds)?);
                }
                out.insert("writers".into(), Value::Array(vs));
            }
            if let Some(o) = &oracle {
                out.insert("final_check".into(), final_check(store.as_ref(), o, &keys, &lay, cfg.seed)?);
            }
            out.insert("disk_bytes_end".into(), json!(store.disk_bytes()?));
            out.insert("mem_bytes_end".into(), json!(store.mem_bytes()?));
        }
        drop(store);
        drop(guard);
    } else {
        rec["store"] = json!(spec);
    }
    if cfg.phase("crash") {
        out.insert("crash".into(), crash_phase(spec, work, cfg, &src)?);
    }
    // totals of every correctness check in the run
    let mut checks = 0u64;
    let mut fails = 0u64;
    let mut add = |v: &Value| {
        checks += v["checks"].as_u64().unwrap_or(0);
        fails += v["failures"].as_u64().unwrap_or(0);
    };
    if let Some(cs) = out.get("cells").and_then(Value::as_array) {
        for c in cs {
            add(&c["check"]);
        }
    }
    if let Some(f) = out.get("final_check") {
        add(f);
    }
    for k in ["load_check", "open"] {
        if let Some(v) = out.get(k) {
            checks += v["checks"].as_u64().unwrap_or(0);
            fails += v["bad_keys"].as_array().map_or(0, |a| a.len() as u64);
        }
    }
    if let Some(c) = out.get("crash") {
        checks += c["acked"].as_u64().unwrap_or(0);
        fails += c["missing_acked"].as_u64().unwrap_or(0) + c["mismatched_acked"].as_u64().unwrap_or(0);
        // a run that never crashed mid-load proves nothing: a failure
        fails += (c["valid"] == json!(false)) as u64;
    }
    let misses: u64 = out
        .get("cells")
        .and_then(Value::as_array)
        .map_or(0, |cs| cs.iter().map(|c| c["read_misses"].as_u64().unwrap_or(0)).sum());
    out.insert("correctness".into(), json!({"checks": checks, "failures": fails, "read_misses": misses}));
    rec["ycsb"] = Value::Object(out);
    Ok(())
}

// ---------------------------------------------------------------- crash recovery

/// Crash child (`tracebench ycsb-crash-child`): durable single-record
/// appends from `crash_writers` threads into a fresh store in `dir`,
/// printing each record's id on stdout once its append has returned.
pub fn crash_child(spec: &str, dir: &Path, tier: &Tier, cfg: &Config) -> Result<()> {
    let src = if cfg.synthetic { Source::synthetic(cfg.seed) } else { Source::real(tier) };
    let total = cfg.crash_records();
    let lay = Layout::new(total, cfg.seed);
    let store = arms::open(spec, dir)?;
    let next = AtomicU64::new(0);
    let res: Vec<Result<()>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..cfg.crash_writers.max(1))
            .map(|_| {
                let (store, next, lay, src) = (store.as_ref(), &next, &lay, &src);
                s.spawn(move || -> Result<()> {
                    let (mut cur, mut buf) = (Cursor::default(), String::new());
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        if k >= total {
                            return Ok(());
                        }
                        src.fill(k, &mut buf);
                        store.append(&[record(lay, k, k + 1, src.event(k, &mut cur), &buf)])?;
                        let mut o = std::io::stdout().lock();
                        writeln!(o, "{}", k + 1)?;
                        o.flush()?;
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("crash writer")).collect()
    });
    for r in res {
        r?;
    }
    Ok(())
}

/// Start the crash child on a fresh store, SIGKILL it (and a server arm's
/// engine) after `crash_acks` acknowledged appends, then time reopening the
/// store and check every acknowledged record is there, byte for byte.
fn crash_phase(spec: &str, work: &Path, cfg: &Config, src: &Source) -> Result<Value> {
    if arms::is_memory(spec) {
        return Ok(json!({"skipped": "in-memory arm: nothing survives a process kill"}));
    }
    let dir = work.join("crash");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    let keep_spec = arms::with_param(spec, "keep=on");
    let guard = ServerGuard(spec);
    let mut child = ChildGuard(
        Command::new(std::env::current_exe()?)
        .arg("ycsb-crash-child")
        .args(["--arm", &keep_spec, "--work"])
        .arg(&dir)
        .arg("--data")
        .arg(&cfg.data)
        .args(["--tier", &cfg.tier.to_string(), "--seed", &cfg.seed.to_string()])
        .args(["--ycsb-data", if cfg.synthetic { "synthetic" } else { "real" }])
        .args(["--ycsb-crash-writers", &cfg.crash_writers.to_string()])
        .args(["--ycsb-crash-records", &cfg.crash_records().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?,
    );
    let started = Instant::now();
    let stdout = child.0.stdout.take().ok_or("child stdout")?;
    let (tx, rx) = std::sync::mpsc::channel::<u64>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { break };
            if let Ok(id) = l.trim().parse()
                && tx.send(id).is_err()
            {
                break;
            }
        }
    });
    let mut acked: Vec<u64> = Vec::new();
    let deadline = started + Duration::from_secs_f64(cfg.crash_wait_s);
    let mut early_exit = false;
    while (acked.len() as u64) < cfg.crash_acks {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(id) => acked.push(id),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                early_exit = true;
                break;
            }
        }
    }
    let killed_after_s = started.elapsed().as_secs_f64();
    let acked_at_kill = acked.len() as u64;
    let _ = child.0.kill(); // SIGKILL
    arms::kill_engine(spec)?;
    let status = child.0.wait()?;
    reader.join().expect("ack reader");
    acked.extend(rx.try_iter());
    // valid only if the writer was killed mid-load after the full ack count
    let killed = {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(9)
    };
    let mut invalid: Vec<String> = Vec::new();
    if early_exit || !killed {
        invalid.push(format!("the writer was not killed mid-load (exited first: {early_exit}, status {status})"));
    }
    if acked_at_kill < cfg.crash_acks {
        invalid.push(format!(
            "only {acked_at_kill} of {} acknowledgements before the kill (raise --ycsb-crash-wait-s)",
            cfg.crash_acks
        ));
    }
    let load_before = loadavg();
    let t = Instant::now();
    let store = arms::reopen(&keep_spec, &dir)?;
    let open_s = t.elapsed().as_secs_f64();
    let first = acked.first().copied().unwrap_or(1);
    let t1 = Instant::now();
    let _ = store.get(first)?;
    let first_get_ms = t1.elapsed().as_secs_f64() * 1e3;
    let reopen_s = t.elapsed().as_secs_f64();
    // every acknowledged record, byte for byte (the child drew its payloads
    // from the same data and seed)
    let lay = Layout::new(cfg.crash_records(), cfg.seed);
    let mut cur = Cursor::default();
    let (mut missing, mut mismatched, mut bad) = (0u64, 0u64, Vec::new());
    let t2 = Instant::now();
    for &id in &acked {
        let want = expected_row(&lay, src, id - 1, id, &mut cur);
        match store.get(id)? {
            None => {
                missing += 1;
                if bad.len() < 20 {
                    bad.push(id);
                }
            }
            Some(r) if r != want => {
                mismatched += 1;
                if bad.len() < 20 {
                    bad.push(id);
                }
            }
            Some(_) => {}
        }
    }
    let verify_s = t2.elapsed().as_secs_f64();
    // writes that committed but whose acknowledgement died with the process
    let set: HashSet<u64> = acked.iter().copied().collect();
    let top = acked.iter().copied().max().unwrap_or(0) + 4 * cfg.crash_writers as u64 + 64;
    let mut unacked_present = 0u64;
    for id in 1..=top {
        if !set.contains(&id) && store.get(id)?.is_some() {
            unacked_present += 1;
        }
    }
    let disk = store.disk_bytes()?;
    drop(store);
    drop(child);
    drop(guard);
    let engine = if arms::is_server(spec) {
        "server container SIGKILLed too"
    } else {
        "embedded: died with the process"
    };
    eprintln!(
        "ycsb crash: {} acked, reopen {reopen_s:.3} s, missing {missing}, mismatched {mismatched}{}",
        acked.len(),
        if invalid.is_empty() { String::new() } else { format!("  INVALID: {}", invalid.join("; ")) }
    );
    Ok(json!({
        "valid": invalid.is_empty(), "invalid_reasons": invalid,
        "writers": cfg.crash_writers, "kill_after_acks": cfg.crash_acks, "acked_at_kill": acked_at_kill,
        "acked": acked.len(),
        "killed_after_s": killed_after_s, "child_exited_before_kill": early_exit,
        "child_status": format!("{status}"),
        "engine_killed": engine,
        "open_s": open_s, "first_get_ms": first_get_ms, "reopen_s": reopen_s, "verify_s": verify_s,
        "missing_acked": missing, "mismatched_acked": mismatched, "bad_ids": bad,
        "unacked_present": unacked_present, "disk_bytes": disk,
        "loadavg_before": load_before, "loadavg_after": loadavg(),
        "note": "process SIGKILL: the OS page cache survives, so this proves recovery of acknowledged writes, not fsync against power loss",
    }))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 200_000;

    #[test]
    fn zipfian_matches_the_exact_distribution() {
        let items = 1000u64;
        let mut z = Zipfian::new(0, items - 1, ZIPFIAN_CONSTANT);
        let zn = zeta(0, items, ZIPFIAN_CONSTANT, 0.0);
        assert!((z.zetan() - zn).abs() < 1e-12);
        let mut rng = Rng(42);
        let mut counts = vec![0u64; items as usize];
        for _ in 0..N {
            let v = z.next(&mut rng);
            assert!(v < items, "draw {v} out of range");
            counts[v as usize] += 1;
        }
        let p = |i: u64| 1.0 / ((i + 1) as f64).powf(ZIPFIAN_CONSTANT) / zn;
        // ranks 0 and 1 are exact in Gray's method
        for i in 0..2 {
            let got = counts[i as usize] as f64 / N as f64;
            assert!((got / p(i) - 1.0).abs() < 0.05, "rank {i}: {got} vs {}", p(i));
        }
        // mass of the 100 most popular items (approximate for ranks >= 2)
        let want: f64 = (0..100).map(p).sum();
        let got = counts[..100].iter().sum::<u64>() as f64 / N as f64;
        assert!((got - want).abs() < 0.03, "top-100 mass {got} vs {want}");
        // popularity falls with rank
        assert!(counts[0] > counts[1] && counts[1] > counts[5] && counts[5] > counts[50] && counts[50] > counts[500]);
        // the tail is reached
        assert!(counts[500..].iter().sum::<u64>() > 0);
    }

    #[test]
    fn zipfian_grows_zeta_incrementally() {
        let mut z = Zipfian::new(0, 999, ZIPFIAN_CONSTANT);
        let mut rng = Rng(7);
        for _ in 0..1000 {
            assert!(z.next_n(&mut rng, 2500) < 2500);
        }
        assert!((z.zetan() - zeta(0, 2500, ZIPFIAN_CONSTANT, 0.0)).abs() < 1e-9);
        // shrinking recomputes from scratch
        z.next_n(&mut rng, 10);
        assert!((z.zetan() - zeta(0, 10, ZIPFIAN_CONSTANT, 0.0)).abs() < 1e-12);
    }

    #[test]
    fn ycsb_constants_hold() {
        // FNV-1a of 8 zero bytes is 0xA8C7F832281A39C5; as a Java long it is
        // negative, and YCSB's Math.abs gives 2^64 minus it
        assert_eq!(fnvhash64(0), 0x5738_07CD_D7E5_C63B);
        // the precomputed zeta of the 10^10-item scrambled generator is the
        // partial sum it claims to be (sum to 10^6 exactly + integral tail)
        let head = zeta(0, 1_000_000, ZIPFIAN_CONSTANT, 0.0);
        let s = 1.0 - ZIPFIAN_CONSTANT;
        let tail = ((1e10f64 + 1.5).powf(s) - (1e6f64 + 0.5).powf(s)) / s;
        assert!((head + tail - SCRAMBLED_ZETAN).abs() < 1e-6, "{}", head + tail);
    }

    #[test]
    fn scrambled_zipfian_is_skewed_but_spread() {
        let items = 10_000u64;
        let mut z = ScrambledZipfian::new(0, items - 1);
        let mut rng = Rng(3);
        let mut counts = vec![0u64; items as usize];
        for _ in 0..N {
            let v = z.next(&mut rng);
            assert!(v < items);
            counts[v as usize] += 1;
        }
        let mut sorted = counts.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        // the hottest key carries ~1/zeta(10^10) = 3.8% of the draws
        let hot = sorted[0] as f64 / N as f64;
        assert!((0.034..0.07).contains(&hot), "hottest key share {hot}");
        // the 1% hottest keys carry at least the zipfian mass of 100 ranks
        let top = sorted[..100].iter().sum::<u64>() as f64 / N as f64;
        assert!(top > 0.18, "top-1% share {top}");
        // scrambled: the hot keys are not the first keys
        let hottest = counts.iter().enumerate().max_by_key(|x| x.1).unwrap().0;
        assert!(hottest > 0);
        let first100 = counts[..100].iter().sum::<u64>() as f64 / N as f64;
        assert!(first100 < 0.1, "first 100 keys share {first100}");
        // and draws still cover much of the range
        assert!(counts.iter().filter(|c| **c > 0).count() > (items / 3) as usize);
    }

    #[test]
    fn latest_prefers_the_newest_keys() {
        let mut l = Latest::new(1000);
        let mut rng = Rng(11);
        let max = 999;
        let mut newest10 = 0;
        let mut at_max = 0;
        for _ in 0..N {
            let v = l.next(&mut rng, max);
            assert!(v <= max);
            newest10 += (v > max - 10) as u64;
            at_max += (v == max) as u64;
        }
        let zn = zeta(0, max, ZIPFIAN_CONSTANT, 0.0);
        let want: f64 = (0..10).map(|i| 1.0 / ((i + 1) as f64).powf(ZIPFIAN_CONSTANT) / zn).sum();
        let got = newest10 as f64 / N as f64;
        assert!((got - want).abs() < 0.03, "newest-10 share {got} vs {want}");
        assert!(((at_max as f64 / N as f64) * zn - 1.0).abs() < 0.05);
        // the key space grows: draws follow the new maximum
        assert!(l.next(&mut rng, 5000) <= 5000);
    }

    #[test]
    fn uniform_chooser_is_flat() {
        let keys = Keys::new(100, 0);
        let mut c = Chooser::Uniform(100);
        let mut rng = Rng(5);
        let mut counts = [0u64; 100];
        for _ in 0..N {
            counts[c.next(&mut rng, &keys) as usize] += 1;
        }
        let (lo, hi) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
        assert!(*lo > 1700 && *hi < 2300, "{lo}..{hi}");
    }

    #[test]
    fn zipf_chooser_never_returns_unacknowledged_keys() {
        let keys = Keys::new(100, 10);
        let mut c = Chooser::Zipf(ScrambledZipfian::new(0, 110));
        let mut rng = Rng(9);
        for _ in 0..10_000 {
            assert!(c.next(&mut rng, &keys) < 100);
        }
    }

    #[test]
    fn acknowledged_counter_waits_for_gaps() {
        let k = Keys::new(10, 5);
        let a: Vec<(u64, u64)> = (0..3).map(|_| k.alloc_insert()).collect();
        assert_eq!(a.iter().map(|x| x.0).collect::<Vec<_>>(), vec![10, 11, 12]);
        assert_eq!(k.last(), 9);
        k.ack(11);
        k.ack(12);
        assert_eq!(k.last(), 9, "key 10 is still in flight");
        k.ack(10);
        assert_eq!(k.last(), 12);
        assert_eq!(k.id_of(3), 4);
        assert_eq!(k.id_of(11), a[1].1);
        let r = k.alloc_id();
        assert!(r > a[2].1 && k.ids() == r);
    }

    #[test]
    fn layout_sessions_are_1_to_100_keys() {
        let lay = Layout::new(50_000, 1);
        let mut sizes = Vec::new();
        for s in 1..=lay.sessions() as u64 {
            let r = lay.session_keys(s);
            sizes.push(r.end - r.start);
            for k in [r.start, r.end - 1] {
                assert_eq!(lay.session(k), s);
                assert_eq!(lay.agent(k) as u64, (s - 1) % AGENTS);
            }
            assert_eq!(lay.seq(r.start), 0);
        }
        assert!(sizes.iter().all(|s| (1..=MAX_SCAN).contains(s)));
        let mean = sizes.iter().sum::<u64>() as f64 / sizes.len() as f64;
        assert!((mean - 50.5).abs() < 5.0, "mean session {mean}");
        assert_eq!(lay.ts(3) - lay.ts(2), STEP_US);
    }

    #[test]
    fn workload_mixes_are_ycsb_core() {
        let mut rng = Rng(1);
        for (w, read, update, insert, scan, rmw) in [
            ('a', 0.5, 0.5, 0.0, 0.0, 0.0),
            ('b', 0.95, 0.05, 0.0, 0.0, 0.0),
            ('c', 1.0, 0.0, 0.0, 0.0, 0.0),
            ('d', 0.95, 0.0, 0.05, 0.0, 0.0),
            ('e', 0.0, 0.0, 0.05, 0.95, 0.0),
            ('f', 0.5, 0.0, 0.0, 0.0, 0.5),
        ] {
            let wl = Workload::get(w).unwrap();
            let mut c = [0f64; 5];
            for _ in 0..N {
                c[wl.pick(rng.f64()).lat()] += 1.0 / N as f64;
            }
            for (got, want) in c.iter().zip([read, update, insert, scan, rmw]) {
                assert!((got - want).abs() < 0.01, "{w}: {c:?}");
            }
        }
        assert!(Workload::get('d').unwrap().latest);
        assert!(!Workload::get('c').unwrap().writes());
    }

    #[test]
    fn synthetic_records_are_ycsb_sized_json() {
        let mut a = String::new();
        let mut b = String::new();
        synthetic_payload(1, 77, &mut a);
        synthetic_payload(1, 77, &mut b);
        assert_eq!(a, b, "deterministic");
        synthetic_payload(1, 78, &mut b);
        assert_ne!(a, b);
        let v: serde_json::Value = serde_json::from_str(&a).unwrap();
        let o = v.as_object().unwrap();
        assert_eq!(o.len(), FIELDS);
        for f in 0..FIELDS {
            assert_eq!(o[&format!("field{f}")].as_str().unwrap().len(), FIELD_BYTES);
        }
    }

    #[test]
    fn records_and_revisions_follow_the_contract() {
        let lay = Layout::new(1000, 2);
        let e = record(&lay, 5, 6, None, "{}");
        assert_eq!((e.id, e.kind, e.status, e.revises), (6, TOOL_CALL, ST_PENDING, 0));
        let r = revision(&lay, 5, 6, 1234, ST_OK, 3);
        assert_eq!((r.revises, r.agent, r.session, r.duration_us), (6, e.agent, e.session, 1234));
        assert!(r.is_revision() && r.text.is_empty());
        let mut row = Row::from_event(&e);
        row.apply(&r);
        assert_eq!((row.status, row.exit_code, row.duration_us), (ST_OK, Some(3), 1234));
    }
}
