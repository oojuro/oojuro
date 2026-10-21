//! `periods`: find stretches of agent activity by how they looked
//! (`docs/ACTIVITY_SEARCH.md`). A pure query-time function over what every
//! record already carries in the column table: time, kind, tool, status,
//! exit code, text length, the agent instance (session) and its parent.
//! No text is read, nothing is computed or stored at ingest, and removing
//! this file (plus its request, CLI command and MCP tool) removes the
//! feature.
//!
//! Signals, each detected inside one agent instance's clock-aligned
//! windows (5 min for `errors` and `repeat`, 1 h otherwise):
//! - `volume`: activity records (everything but harness context and
//!   Oojuro's own tool calls).
//! - `errors`: failed tool results (the harness status, or a nonzero exit
//!   code), as the binomial upper-tail surprisal in bits against the
//!   writer's trailing 7-day failure rate (coordinator and subagent
//!   instances have separate baselines).
//! - `repeat`: stuck loops. A call is a candidate repeat when the same tool
//!   with input of the same length was among the instance's last 32 calls;
//!   it is stuck when its result fails again, waiting otherwise (polling,
//!   sleeping, re-reading). Bits as for errors, against the trailing rate
//!   of candidates; `include_waiting` counts both. Ranked by `repeat`, the
//!   candidates of the windows that can still reach the top are checked
//!   against the two calls' texts, and only exact repeats count (the one
//!   place periods reads text: those calls only).
//! - `unusual`: five components (rate, tool mix, error rate, tools not used
//!   in the trailing 7 days, bytes per record), each `-log2` of its
//!   empirical tail among the writer's own windows of the trailing 7 days.
//! - `text`: with a query and no signal, the bits its search hits save.
//!
//! Windows that make the top `k` merge into periods: adjacent windows of
//! one task tree (an instance and every instance it spawned, recursively;
//! the default), one instance (`per session`), or one writer (`per writer`,
//! all its instances summed per window).
use crate::{
    Reader, Result, SearchArgs,
    types::Filters,
    when::{self, When},
};
use oojuro_capture::{kind, status, time::format_rfc3339_us};
use oojuro_trace::ActivityRow;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

const MIN: i64 = 60_000_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// Trailing baseline of every signal.
pub const BASELINE_US: i64 = 7 * DAY;
/// Calls of an instance a repeat is looked for among.
pub const REPEAT_RING: usize = 32;
/// Exposure cap of the unusual components (shape, not size).
const N_REF: f64 = 50.0;
/// Fewest records of a window ranked by `unusual`.
pub const MIN_EVENTS_UNUSUAL: u32 = 20;
/// Fewest repeated calls of a window ranked by `repeat`.
pub const MIN_REPEATS: u32 = 2;
/// Baseline windows needed before a component is calibrated.
const MIN_BASELINE: usize = 20;
/// Default range when neither `when` nor `from` is given.
pub const DEFAULT_SPAN_US: i64 = 7 * DAY;
pub const MAX_K: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Volume,
    Errors,
    Repeat,
    Unusual,
    Text,
}

impl By {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "volume" | "activity" => By::Volume,
            "errors" | "error" => By::Errors,
            "repeat" | "loops" | "stuck" => By::Repeat,
            "unusual" => By::Unusual,
            "text" => By::Text,
            "novelty" => {
                return Err("`novelty` is not available: it needs the records' text, which periods never reads \
                    (docs/ACTIVITY_SEARCH.md, \"Implemented\")"
                    .into());
            }
            _ => return Err(format!("--by volume|errors|repeat|unusual|text, not {s:?}").into()),
        })
    }
    fn default_window(self) -> i64 {
        match self {
            By::Errors | By::Repeat => 5 * MIN,
            _ => HOUR,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Per {
    /// An instance and every instance it spawned (recursively).
    #[default]
    Task,
    Session,
    Writer,
}

impl Per {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "task" | "tree" => Per::Task,
            "session" | "agent" | "instance" => Per::Session,
            "writer" => Per::Writer,
            _ => return Err(format!("--per task|session|writer, not {s:?}").into()),
        })
    }
}

/// A `periods` request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PeriodsArgs {
    /// Only windows holding a search hit of this text (at least `min_bits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// The ranking (default: `text` with a query, else `volume`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<By>,
    #[serde(default)]
    pub per: Per,
    /// Window length in µs (default per signal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_us: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_us: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_us: Option<i64>,
    /// A date phrase or anchor (`yesterday`, `after the first "failed test"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<String>,
    #[serde(default)]
    pub k: usize,
    /// Sample record ids per period (0-5).
    #[serde(default)]
    pub samples: usize,
    /// `repeat` counts waiting/polling loops too.
    #[serde(default)]
    pub include_waiting: bool,
    /// The caller's session (anchors and the text query leave out what is
    /// still in its context window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_context_of: Option<String>,
    /// IANA time zone for `when` and day windows (default: the machine's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    #[serde(default)]
    pub min_bits: f64,
    #[serde(default)]
    pub max_query_bytes: usize,
}

/// The unusual components of a window, in bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Unusual {
    pub rate: f64,
    pub mix: f64,
    pub errors: f64,
    pub new: f64,
    pub size: f64,
}

impl Unusual {
    fn is_zero(&self) -> bool {
        *self == Unusual::default()
    }
}

/// One agent instance contributing to a period.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentPart {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub writer: String,
    pub events: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub id: u64,
    pub kind: String,
    pub ts: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// Why it was picked: `error`, `stuck`, `waiting`, `new_tool`, `first`, `hit`.
    pub why: String,
}

/// A ranked period.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Period {
    pub rank: usize,
    /// The task tree's root instance (`per task`), the instance (`per
    /// session`), or empty (`per writer`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_id: String,
    pub writers: Vec<String>,
    pub from: String,
    pub to: String,
    pub from_us: i64,
    pub to_us: i64,
    pub minutes: f64,
    /// Windows merged into the period.
    pub windows: usize,
    /// The ranking score: bits, or records for `volume`.
    pub score: f64,
    pub score_unit: String,
    pub events: u64,
    pub calls: u64,
    pub results: u64,
    pub errors: u64,
    /// Computed against a baseline (ranking by errors, repeat or unusual).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub error_bits: f64,
    pub repeats_stuck: u64,
    pub repeats_waiting: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeat_bits: f64,
    /// The repeat counts are exact (checked against the texts: `by repeat`),
    /// else candidates (same tool, same input length).
    pub repeats_exact: bool,
    /// Computed when ranking by unusual.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unusual_bits: f64,
    #[serde(default, skip_serializing_if = "Unusual::is_zero")]
    pub unusual: Unusual,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text_bits: f64,
    pub bytes: u64,
    /// Most used tools (at most 5).
    pub tools: BTreeMap<String, u64>,
    /// Contributing agent instances, busiest first (at most 20).
    pub agents: Vec<AgentPart>,
    pub agent_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<Sample>,
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Periods {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<When>,
    pub by: By,
    pub per: Per,
    pub window: String,
    pub baseline: String,
    pub from: String,
    pub to: String,
    pub from_us: i64,
    pub to_us: i64,
    /// Records read from the columns (the baseline span included) and the
    /// time spent, for the record.
    pub scanned: u64,
    pub elapsed_ms: f64,
    pub periods: Vec<Period>,
}

const SAMPLES: usize = 5;

#[derive(Clone, Copy, Debug)]
struct Pick {
    id: u64,
    ts: i64,
    kind: u8,
    tool: u16,
    why: u8,
}

const WHY: [&str; 6] = ["error", "stuck", "waiting", "new_tool", "first", "hit"];

/// One instance's (or one writer's) activity in one window.
#[derive(Clone, Debug, Default)]
struct Cell {
    agent: u32,
    session: u64,
    start: i64,
    events: u32,
    calls: u32,
    results: u32,
    errors: u32,
    stuck: u32,
    waiting: u32,
    /// Candidate repeats: (call, the earlier call it matched, its time, tool, stuck)
    reps: Vec<(u64, u64, i64, u16, bool)>,
    /// Exact repeats only (checked against the texts).
    exact: bool,
    rep_q0: f64,
    bytes: u64,
    /// (tool, calls, first call id, its time)
    tools: Vec<(u16, u32, u64, i64)>,
    first: Option<Pick>,
    picks: Vec<Pick>,
    text_bits: f64,
    // scores
    sub: bool,
    error_bits: f64,
    repeat_bits: f64,
    unusual: Unusual,
    unusual_bits: f64,
    new_tools: u32,
}

impl Cell {
    fn pick(&mut self, p: Pick) {
        if self.picks.iter().filter(|x| x.why == p.why).count() < SAMPLES {
            self.picks.push(p);
        }
    }
}

/// A cheap hasher for the scan's integer keys (ids and times, not
/// attacker-chosen in a way that matters for a local query).
#[derive(Default, Clone, Copy)]
struct Fast(u64);

impl std::hash::Hasher for Fast {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(*b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_i64(&mut self, v: i64) {
        self.write_u64(v as u64);
    }
}

type FastMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fast>>;

/// `-log2 P(X >= k)`, `X ~ Binomial(n, p)`; 0 when `k <= n p`.
pub fn binom_upper_bits(k: u64, n: u64, p: f64) -> f64 {
    if k == 0 || k > n || (k as f64) <= n as f64 * p || p <= 0.0 || p >= 1.0 {
        return 0.0;
    }
    let (lp, lq) = (p.ln(), (1.0 - p).ln());
    let first = ln_choose(n, k) + k as f64 * lp + (n - k) as f64 * lq;
    // terms after k relative to the first: t(i+1)/t(i) = (n-i)/(i+1) * p/q
    let (mut t, mut total) = (1.0f64, 1.0f64);
    let r = p / (1.0 - p);
    for i in k..n {
        t *= (n - i) as f64 / (i + 1) as f64 * r;
        total += t;
        if t < 1e-12 * total {
            break;
        }
    }
    (-(first + total.ln()) / std::f64::consts::LN_2).max(0.0)
}

fn ln_fact(n: u64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    if n < 64 {
        return (2..=n).map(|i| (i as f64).ln()).sum();
    }
    let x = n as f64;
    x * x.ln() - x + 0.5 * (2.0 * std::f64::consts::PI * x).ln() + 1.0 / (12.0 * x) - 1.0 / (360.0 * x * x * x)
}

fn ln_choose(n: u64, k: u64) -> f64 {
    ln_fact(n) - ln_fact(k) - ln_fact(n - k)
}

/// Excess code length (bits) of a window's tool mix under the baseline
/// mix (Dirichlet ½) over its own mix, minus the cost of describing its own
/// mix, exposure capped at `N_REF` calls (MDL; `docs/ACTIVITY_SEARCH.md` §2.5).
fn mix_bits(tools: &[(u16, u32, u64, i64)], base: &[u64], base_total: u64, alphabet: usize) -> f64 {
    let n: u32 = tools.iter().map(|t| t.1).sum();
    if n < 2 {
        return 0.0;
    }
    let n = f64::from(n);
    let denom = base_total as f64 + 0.5 * alphabet.max(1) as f64;
    let mut kl = 0.0;
    for &(t, c, _, _) in tools {
        let q = (base.get(t as usize).copied().unwrap_or(0) as f64 + 0.5) / denom;
        let p = f64::from(c) / n;
        kl += p * (p / q).log2();
    }
    let ne = n.min(N_REF);
    (ne * kl - (tools.len() as f64 - 1.0) / 2.0 * ne.log2()).max(0.0)
}

/// Evidence (bits) that a rate `k/n` exceeds `p0`, exposure capped.
fn bern_kl_bits(k: u32, n: u32, p0: f64) -> f64 {
    if n == 0 || f64::from(k) / f64::from(n) <= p0 {
        return 0.0;
    }
    let q = (f64::from(k) / f64::from(n)).min(1.0 - 1e-9);
    let kl = q * (q / p0).log2() + (1.0 - q) * ((1.0 - q) / (1.0 - p0)).log2();
    f64::from(n).min(N_REF) * kl
}

/// Counts of quantized values (a Fenwick tree): tails among a sliding set.
struct Tails {
    tree: Vec<u32>,
    n: usize,
}

const QBINS: usize = 2048;

fn quant(x: f64) -> usize {
    ((x.max(0.0) + 1.0).log2() * 64.0).min((QBINS - 1) as f64) as usize
}

impl Tails {
    fn new() -> Self {
        Self { tree: vec![0; QBINS + 1], n: 0 }
    }
    fn add(&mut self, x: f64, d: i32) {
        let mut i = quant(x) + 1;
        while i <= QBINS {
            self.tree[i] = (self.tree[i] as i64 + d as i64) as u32;
            i += 1 << i.trailing_zeros(); // the lowest set bit (i >= 1)
        }
        self.n = (self.n as i64 + d as i64) as usize;
    }
    /// Values in bins `<= quant(x)`.
    fn at_most(&self, x: f64) -> usize {
        let mut i = quant(x) + 1;
        let mut s = 0usize;
        while i > 0 {
            s += self.tree[i] as usize;
            i &= i - 1; // clear the lowest set bit
        }
        s
    }
    /// Values in bins `>= quant(x)`.
    fn at_least(&self, x: f64) -> usize {
        self.n - if quant(x) == 0 { 0 } else { self.at_most_bin(quant(x) - 1) }
    }
    fn at_most_bin(&self, b: usize) -> usize {
        let mut i = b + 1;
        let mut s = 0usize;
        while i > 0 {
            s += self.tree[i] as usize;
            i &= i - 1; // clear the lowest set bit
        }
        s
    }
}

/// A writer's trailing baseline of one role (coordinator or subagent
/// instances; all of the writer's windows `per writer`).
struct Baseline {
    q: std::collections::VecDeque<usize>,
    events: u64,
    calls: u64,
    results: u64,
    errors: u64,
    repeats: u64,
    tools: Vec<u64>,
    tools_total: u64,
    distinct: usize,
    tails: [Tails; 5],
    /// Raw unusual statistics of the windows in `q` that have them.
    raw: HashMap<usize, [f64; 5]>,
}

impl Baseline {
    fn new(ntools: usize) -> Self {
        Self {
            q: Default::default(),
            events: 0,
            calls: 0,
            results: 0,
            errors: 0,
            repeats: 0,
            tools: vec![0; ntools],
            tools_total: 0,
            distinct: 0,
            tails: [Tails::new(), Tails::new(), Tails::new(), Tails::new(), Tails::new()],
            raw: HashMap::new(),
        }
    }
    fn change(&mut self, c: &Cell, rep: u32, d: i64) {
        self.events = (self.events as i64 + d * i64::from(c.events)) as u64;
        self.calls = (self.calls as i64 + d * i64::from(c.calls)) as u64;
        self.results = (self.results as i64 + d * i64::from(c.results)) as u64;
        self.errors = (self.errors as i64 + d * i64::from(c.errors)) as u64;
        self.repeats = (self.repeats as i64 + d * i64::from(rep)) as u64;
        for &(t, n, _, _) in &c.tools {
            let slot = &mut self.tools[t as usize];
            let was = *slot;
            *slot = (*slot as i64 + d * i64::from(n)) as u64;
            self.tools_total = (self.tools_total as i64 + d * i64::from(n)) as u64;
            if was == 0 && *slot > 0 {
                self.distinct += 1;
            } else if was > 0 && *slot == 0 {
                self.distinct -= 1;
            }
        }
    }
}

/// An instance's last calls as (tool, input length) keys.
struct Ring {
    /// `tool << 32 | len + 1` (0 = empty slot)
    calls: [u64; REPEAT_RING],
    ids: [u64; REPEAT_RING],
    at: usize,
}

impl Ring {
    fn new() -> Self {
        Self { calls: [0; REPEAT_RING], ids: [0; REPEAT_RING], at: 0 }
    }
    /// The latest of the last calls this call may repeat; then remember it.
    fn push(&mut self, c: (u16, u32), id: u64) -> Option<u64> {
        let key = u64::from(c.0) << 32 | (u64::from(c.1) + 1);
        let hit = self.calls.iter().fold(false, |h, &x| h | (x == key));
        let matched = hit.then(|| {
            (1..=REPEAT_RING)
                .map(|d| (self.at + REPEAT_RING - d) % REPEAT_RING)
                .find(|&i| self.calls[i] == key)
                .map(|i| self.ids[i])
        });
        self.calls[self.at] = key;
        self.ids[self.at] = id;
        self.at = (self.at + 1) % REPEAT_RING;
        matched.flatten()
    }
}

/// An Oojuro tool (its calls and results are the journal's own echo, never activity).
fn oojuro_tool(name: &str) -> bool {
    let parts: Vec<&str> = name.split("__").collect();
    parts.len() >= 2 && parts[..parts.len() - 1].iter().any(|p| p.to_ascii_lowercase().contains("oojuro"))
}

fn window_name(us: i64) -> String {
    if us % DAY == 0 {
        format!("{}d", us / DAY)
    } else if us % HOUR == 0 {
        format!("{}h", us / HOUR)
    } else if us % MIN == 0 {
        format!("{}m", us / MIN)
    } else {
        format!("{}s", us / 1_000_000)
    }
}

/// Rank periods (see the module docs).
pub fn periods(r: &Reader, a: &PeriodsArgs) -> Result<Periods> {
    let started = std::time::Instant::now();
    let now = oojuro_capture::time::now_us();
    let by = match (a.by, &a.query) {
        (Some(b), _) => b,
        (None, Some(_)) => By::Text,
        (None, None) => By::Volume,
    };
    if by == By::Text && a.query.is_none() {
        return Err("--by text needs a query".into());
    }
    let k = if a.k == 0 { 10 } else { a.k.min(MAX_K) };
    let window = a.window_us.unwrap_or(by.default_window());
    if !(MIN..=DAY).contains(&window) {
        return Err("window must be 1m to 1d".into());
    }
    let min_bits = if a.min_bits > 0.0 { a.min_bits } else { 8.0 };
    // the period: when, else from/to, else the last 7 days
    let when = match &a.when {
        Some(p) => {
            let scope = when::Scope {
                writers: a.writers.clone(),
                outside_context_of: a.outside_context_of.clone(),
                span_us: None,
                tz: a.tz.clone(),
            };
            Some(when::resolve_when(r, p, &scope, now, min_bits)?)
        }
        None => None,
    };
    let mut from = a.from_us.unwrap_or(i64::MIN);
    let mut to = a.to_us.map_or(now, |t| t.saturating_add(1)).min(now);
    if let Some(w) = &when {
        from = from.max(w.from_us.unwrap_or(i64::MIN));
        to = to.min(w.to_us);
    }
    if from == i64::MIN {
        from = to - DEFAULT_SPAN_US;
    }
    // clock alignment of windows in the person's time zone (whole days start at local midnight)
    let tz = when::time_zone(a.tz.as_deref())?;
    let off = jiff::Timestamp::from_microsecond(to.clamp(0, 253_402_207_200_000_000))
        .map(|t| i64::from(tz.to_offset(t).seconds()) * 1_000_000)
        .unwrap_or(0);
    let bucket = |ts: i64| (ts + off).div_euclid(window) * window - off;
    // baselines need the trailing week (unusual: the week before that too,
    // for its baseline windows' own statistics); volume and text need none
    let lead = match by {
        By::Unusual => 2 * BASELINE_US,
        By::Errors | By::Repeat => BASELINE_US,
        By::Volume | By::Text => 0,
    };
    let scan_from = bucket(from.saturating_sub(lead));

    let db = r.db();
    let tools = db.tool_names();
    let echo: Vec<bool> = tools.iter().map(|t| oojuro_tool(t)).collect();
    let parents = db.session_parents();
    let mut cells: Vec<Cell> = Vec::new();
    let mut scanned = 0u64;
    let per_writer = a.per == Per::Writer;
    let agents = r.agents(&a.writers);
    if !a.writers.is_empty() && agents.is_empty() {
        return Err(format!("no writer {:?} in this journal", a.writers).into());
    }
    for (agent, _) in &agents {
        let first_cell = cells.len();
        let mut index: FastMap<(u64, i64), usize> = FastMap::default();
        let mut ring_of: FastMap<u64, usize> = FastMap::default();
        let mut rings: Vec<Ring> = Vec::new();
        let mut last_ring: (u64, usize) = (u64::MAX, 0);
        // candidate repeats waiting for their result: call id -> (cell, pick, matched call)
        let mut pending: FastMap<u64, (usize, Pick, u64)> = FastMap::default();
        let mut last: Option<((u64, i64), usize)> = None;
        db.scan_agent(*agent, scan_from, to, |row: &ActivityRow| {
            scanned += 1;
            if row.kind == kind::SYSTEM || echo.get(row.tool as usize).copied().unwrap_or(false) {
                return;
            }
            let key = (if per_writer { 0 } else { row.session }, bucket(row.ts_us));
            let ci = match last {
                Some((k, i)) if k == key => i,
                _ => {
                    let i = *index.entry(key).or_insert_with(|| {
                        cells.push(Cell {
                            agent: *agent,
                            session: key.0,
                            start: key.1,
                            sub: !per_writer && parents.contains_key(&key.0),
                            ..Cell::default()
                        });
                        cells.len() - 1
                    });
                    last = Some((key, i));
                    i
                }
            };
            let p = Pick { id: row.id, ts: row.ts_us, kind: row.kind, tool: row.tool, why: 0 };
            let c = &mut cells[ci];
            c.events += 1;
            c.bytes += u64::from(row.text_len);
            if c.first.is_none() {
                c.first = Some(Pick { why: 4, ..p });
            }
            match row.kind {
                kind::TOOL_CALL => {
                    c.calls += 1;
                    match c.tools.iter_mut().find(|t| t.0 == row.tool) {
                        Some(t) => t.1 += 1,
                        None => c.tools.push((row.tool, 1, row.id, row.ts_us)),
                    }
                    if last_ring.0 != row.session {
                        let n = rings.len();
                        let i = *ring_of.entry(row.session).or_insert(n);
                        if i == n {
                            rings.push(Ring::new());
                        }
                        last_ring = (row.session, i);
                    }
                    if let Some(m) = rings[last_ring.1].push((row.tool, row.text_len), row.id) {
                        pending.insert(row.id, (ci, p, m));
                    }
                }
                kind::TOOL_RESULT => {
                    c.results += 1;
                    let failed = row.status == status::ERROR || row.exit_code.is_some_and(|x| x != 0);
                    if failed {
                        c.errors += 1;
                        c.pick(Pick { why: 0, ..p });
                    }
                    if row.call != 0
                        && !pending.is_empty()
                        && let Some((cj, call, m)) = pending.remove(&row.call)
                    {
                        let cc = &mut cells[cj];
                        if failed {
                            cc.stuck += 1;
                        } else {
                            cc.waiting += 1;
                        }
                        cc.reps.push((call.id, m, call.ts, call.tool, failed));
                    }
                }
                _ => {}
            }
        });
        // baselines per writer and role, in time order
        let mine = &mut cells[first_cell..];
        mine.sort_by_key(|c| (c.start, c.session));
        score_writer(mine, by, a.include_waiting, window, from, tools.len());
        if lead == 0 {
            // no baseline was read: no surprise to report
            for c in mine.iter_mut() {
                c.error_bits = 0.0;
                c.repeat_bits = 0.0;
            }
        }
    }

    // a text query: its hits mark (and with `text`, rank) windows
    let mut hit_cells: HashSet<usize> = HashSet::new();
    if let Some(q) = &a.query {
        let args = SearchArgs {
            query: q.clone(),
            k: crate::journal::MAX_K,
            filters: Filters {
                from_us: Some(from),
                to_us: Some(to - 1),
                writers: a.writers.clone(),
                outside_context_of: a.outside_context_of.clone(),
                ..Filters::default()
            },
            excerpt_bytes: 1,
            max_query_bytes: if a.max_query_bytes == 0 { 65_536 } else { a.max_query_bytes },
        };
        let mut at: HashMap<(u32, u64, i64), usize> = HashMap::new();
        for (i, c) in cells.iter().enumerate() {
            at.insert((c.agent, c.session, c.start), i);
        }
        let hits = r.search_hits(&args)?;
        let floor = when::relevance_floor(hits.iter().map(|h| h.0).fold(0.0, f64::max), min_bits);
        for (bits, id) in hits {
            if bits < floor {
                continue;
            }
            let Some(x) = db.info(id) else { continue };
            let key = (x.agent, if per_writer { 0 } else { x.session }, bucket(x.ts_us));
            if let Some(&i) = at.get(&key) {
                cells[i].text_bits += bits;
                cells[i].pick(Pick { id, ts: x.ts_us, kind: x.kind, tool: 0, why: 5 });
                hit_cells.insert(i);
            }
        }
    }

    // candidates in [from, to), best first
    let score = |c: &Cell| -> f64 {
        match by {
            By::Volume => f64::from(c.events),
            By::Errors => c.error_bits,
            By::Repeat => c.repeat_bits,
            By::Unusual => c.unusual_bits,
            By::Text => c.text_bits,
        }
    };
    let qualifies = |c: &Cell| -> bool {
        let reps = c.stuck + if a.include_waiting { c.waiting } else { 0 };
        match by {
            By::Volume => c.events > 0,
            By::Errors => c.errors > 0 && c.error_bits > 0.0,
            By::Repeat => reps >= MIN_REPEATS && c.repeat_bits > 0.0,
            By::Unusual => c.events >= MIN_EVENTS_UNUSUAL && c.unusual_bits > 0.0,
            By::Text => c.text_bits > 0.0,
        }
    };
    let mut cand: Vec<usize> = (0..cells.len())
        .filter(|&i| {
            let c = &cells[i];
            c.start + window > from && c.start < to && qualifies(c) && (a.query.is_none() || hit_cells.contains(&i))
        })
        .collect();
    cand.sort_by(|&x, &y| score(&cells[y]).total_cmp(&score(&cells[x])).then(cells[y].start.cmp(&cells[x].start)));

    if by == By::Repeat {
        // exact repeats only: check candidates against the texts, lazily.
        // A window's exact score never exceeds its candidate score, so once
        // the 3k-th best checked score beats the next unchecked candidate,
        // no unchecked window can reach the top.
        let want = 3 * k;
        let mut checked: Vec<usize> = Vec::new();
        let mut at = 0;
        while at < cand.len() {
            let mut best: Vec<f64> = checked.iter().map(|&i| score(&cells[i])).filter(|x| *x > 0.0).collect();
            best.sort_by(|x, y| y.total_cmp(x));
            if best.len() >= want && best[want - 1] >= score(&cells[cand[at]]) {
                break;
            }
            let batch: Vec<usize> = cand[at..(at + 8).min(cand.len())].to_vec();
            at += batch.len();
            verify(db, &mut cells, &batch, a.include_waiting)?;
            checked.extend(batch);
        }
        cand = checked.into_iter().filter(|&i| qualifies(&cells[i])).collect();
        cand.sort_by(|&x, &y| score(&cells[y]).total_cmp(&score(&cells[x])).then(cells[y].start.cmp(&cells[x].start)));
    }

    // the task tree of an instance: its root
    let root = |s: u64| -> u64 {
        let mut s = s;
        for _ in 0..64 {
            match parents.get(&s) {
                Some(&p) if p != s => s = p,
                _ => break,
            }
        }
        s
    };
    let group = |c: &Cell| -> u64 {
        match a.per {
            Per::Task => root(c.session),
            Per::Session => c.session,
            Per::Writer => u64::from(c.agent),
        }
    };
    // merge: a window joins a period of its group it touches; a new group
    // period opens while fewer than k exist
    struct Acc {
        group: u64,
        start: i64,
        end: i64,
        cells: Vec<usize>,
        score: f64,
    }
    // A window joins a period of its group that it overlaps (a sibling
    // instance in the same clock window: a fan-out folds into one period),
    // or one it only touches when it scores at least half the period's best
    // window (a weaker neighbour in time is left out, so continuous work does
    // not chain into one long period). Otherwise it opens a period while
    // fewer than `k` exist.
    let mut acc: Vec<Acc> = Vec::new();
    for &i in &cand {
        let c = &cells[i];
        let sc = score(c);
        let g = group(c);
        let (s, e) = (c.start, c.start + window);
        if let Some(p) = acc.iter_mut().find(|p| p.group == g && s <= p.end && e >= p.start) {
            if (s < p.end && e > p.start) || sc >= p.score / 2.0 {
                p.start = p.start.min(s);
                p.end = p.end.max(e);
                p.cells.push(i);
            }
            continue;
        }
        if acc.len() < k {
            acc.push(Acc { group: g, start: s, end: e, cells: vec![i], score: sc });
        }
    }

    let samples = a.samples.min(SAMPLES);
    let mut out = Vec::with_capacity(acc.len());
    for (rank, p) in acc.iter().enumerate() {
        let mut o = Period {
            rank: rank + 1,
            agent_id: if per_writer { String::new() } else { format!("{:016x}", p.group) },
            writers: Vec::new(),
            from: format_rfc3339_us(p.start),
            to: format_rfc3339_us(p.end),
            from_us: p.start,
            to_us: p.end,
            minutes: (p.end - p.start) as f64 / MIN as f64,
            windows: p.cells.len(),
            score: p.score,
            score_unit: if by == By::Volume { "records" } else { "bits" }.into(),
            events: 0,
            calls: 0,
            results: 0,
            errors: 0,
            error_bits: 0.0,
            repeats_stuck: 0,
            repeats_waiting: 0,
            repeats_exact: by == By::Repeat,
            repeat_bits: 0.0,
            unusual_bits: 0.0,
            unusual: Unusual::default(),
            text_bits: 0.0,
            bytes: 0,
            tools: BTreeMap::new(),
            agents: Vec::new(),
            agent_count: 0,
            samples: Vec::new(),
        };
        let mut tool_n: HashMap<u16, u64> = HashMap::new();
        let mut parts: HashMap<(u32, u64), u64> = HashMap::new();
        let mut picks: Vec<Pick> = Vec::new();
        for &i in &p.cells {
            let c = &cells[i];
            o.events += u64::from(c.events);
            o.calls += u64::from(c.calls);
            o.results += u64::from(c.results);
            o.errors += u64::from(c.errors);
            o.repeats_stuck += u64::from(c.stuck);
            o.repeats_waiting += u64::from(c.waiting);
            o.bytes += c.bytes;
            o.text_bits += c.text_bits;
            o.error_bits = o.error_bits.max(c.error_bits);
            o.repeat_bits = o.repeat_bits.max(c.repeat_bits);
            if c.unusual_bits > o.unusual_bits {
                o.unusual_bits = c.unusual_bits;
                o.unusual = c.unusual;
            }
            for &(t, n, _, _) in &c.tools {
                *tool_n.entry(t).or_default() += u64::from(n);
            }
            *parts.entry((c.agent, c.session)).or_default() += u64::from(c.events);
        }
        // samples: the signal's own records from the best windows first
        let want: &[u8] = match by {
            By::Errors => &[0],
            By::Repeat if a.include_waiting => &[1, 2],
            By::Repeat => &[1],
            By::Text => &[5],
            By::Unusual => &[3, 0, 1],
            By::Volume => &[],
        };
        let mut order = p.cells.clone();
        order.sort_by(|&x, &y| score(&cells[y]).total_cmp(&score(&cells[x])));
        for why in want {
            for &i in &order {
                let c = &cells[i];
                let reps = c.reps.iter().map(|r| Pick {
                    id: r.0,
                    ts: r.2,
                    kind: kind::TOOL_CALL,
                    tool: r.3,
                    why: if r.4 { 1 } else { 2 },
                });
                for x in c.picks.iter().copied().chain(reps).filter(|x| x.why == *why).take(SAMPLES) {
                    picks.push(x);
                }
            }
        }
        picks.extend(order.iter().filter_map(|&i| cells[i].first));
        let mut seen = HashSet::new();
        picks.retain(|x| seen.insert(x.id));
        o.samples = picks
            .iter()
            .take(samples)
            .map(|x| Sample {
                id: x.id,
                kind: kind::name(x.kind).to_string(),
                ts: format_rfc3339_us(x.ts),
                tool: if x.why == 5 {
                    db.get(x.id).ok().flatten().map(|e| e.tool).unwrap_or_default()
                } else {
                    tools.get(x.tool as usize).cloned().unwrap_or_default()
                },
                why: WHY[x.why as usize].to_string(),
            })
            .collect();
        let mut tv: Vec<(u16, u64)> = tool_n.into_iter().collect();
        tv.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        o.tools =
            tv.into_iter().take(5).map(|(t, n)| (tools.get(t as usize).cloned().unwrap_or_default(), n)).collect();
        let mut pv: Vec<((u32, u64), u64)> = parts.into_iter().collect();
        pv.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        o.agent_count = if per_writer { 0 } else { pv.len() };
        let mut ws: Vec<String> = pv.iter().map(|((ag, _), _)| r.writer_name(*ag)).collect();
        ws.sort();
        ws.dedup();
        o.writers = ws;
        if !per_writer {
            o.agents = pv
                .iter()
                .take(20)
                .map(|((ag, s), n)| AgentPart {
                    agent_id: format!("{s:016x}"),
                    parent_id: parents.get(s).map(|p| format!("{p:016x}")),
                    writer: r.writer_name(*ag),
                    events: *n,
                })
                .collect();
        }
        out.push(o);
    }
    let role = if per_writer {
        "the writer's own windows"
    } else {
        "the writer's windows of the same role (top-level or subagent instances)"
    };
    Ok(Periods {
        when,
        by,
        per: a.per,
        window: window_name(window),
        baseline: format!("{role}, trailing 7 days"),
        from: format_rfc3339_us(from),
        to: format_rfc3339_us(to),
        from_us: from,
        to_us: to,
        scanned,
        elapsed_ms: started.elapsed().as_secs_f64() * 1e3,
        periods: out,
    })
}

/// Keep only the exact repeats of `which` cells (the two calls' texts are
/// equal) and score them again.
fn verify(db: &oojuro_trace::TraceDb, cells: &mut [Cell], which: &[usize], include_waiting: bool) -> Result<()> {
    let mut ids: Vec<u64> = which.iter().flat_map(|&i| cells[i].reps.iter().flat_map(|r| [r.0, r.1])).collect();
    ids.sort_unstable();
    ids.dedup();
    let texts: HashMap<u64, String> = db.get_many(&ids)?.into_iter().flatten().map(|e| (e.id, e.text)).collect();
    for &i in which {
        let c = &mut cells[i];
        if c.exact {
            continue;
        }
        c.reps.retain(|r| matches!((texts.get(&r.0), texts.get(&r.1)), (Some(a), Some(b)) if a == b));
        c.stuck = c.reps.iter().filter(|r| r.4).count() as u32;
        c.waiting = c.reps.len() as u32 - c.stuck;
        let rep = c.stuck + if include_waiting { c.waiting } else { 0 };
        c.repeat_bits = binom_upper_bits(u64::from(rep), u64::from(c.calls), c.rep_q0);
        c.exact = true;
    }
    Ok(())
}

/// Score one writer's cells (sorted by start) against trailing baselines of
/// their role. Windows before `from` only feed baselines (`unusual` needs
/// their own statistics, so it reads 14 days before `from`).
fn score_writer(cells: &mut [Cell], by: By, include_waiting: bool, window: i64, from: i64, ntools: usize) {
    let mut base = [Baseline::new(ntools), Baseline::new(ntools)];
    let rep = |c: &Cell| c.stuck + if include_waiting { c.waiting } else { 0 };
    let unusual = by == By::Unusual;
    let slots = (BASELINE_US / window) as usize;
    let per_writer = cells.first().is_some_and(|c| c.session == 0);
    let raw_from = from - BASELINE_US;
    let mut i = 0;
    while i < cells.len() {
        // windows starting together are scored before any joins a baseline
        let start = cells[i].start;
        let mut j = i;
        while j < cells.len() && cells[j].start == start {
            j += 1;
        }
        for b in base.iter_mut() {
            while let Some(&x) = b.q.front() {
                if cells[x].start >= start - BASELINE_US {
                    break;
                }
                b.q.pop_front();
                let c = cells[x].clone();
                b.change(&c, rep(&c), -1);
                if let Some(raw) = b.raw.remove(&x) {
                    for (t, v) in b.tails.iter_mut().zip(raw) {
                        t.add(v, -1);
                    }
                }
            }
        }
        for x in i..j {
            let b = &base[cells[x].sub as usize];
            let c = &cells[x];
            let p0 = (b.errors as f64 + 1.0) / (b.results as f64 + 2.0);
            let error_bits = binom_upper_bits(u64::from(c.errors), u64::from(c.results), p0);
            let q0 = (b.repeats as f64 + 1.0) / (b.calls as f64 + 2.0);
            let repeat_bits = binom_upper_bits(u64::from(rep(c)), u64::from(c.calls), q0);
            let mut raw = None;
            if unusual && start >= raw_from {
                let new_tools = c.tools.iter().filter(|t| b.tools.get(t.0 as usize).copied().unwrap_or(0) == 0).count();
                let r = [
                    f64::from(c.events),
                    mix_bits(&c.tools, &b.tools, b.tools_total, b.distinct + new_tools),
                    bern_kl_bits(c.errors, c.results, p0),
                    new_tools as f64,
                    if c.events > 0 { c.bytes as f64 / f64::from(c.events) } else { 0.0 },
                ];
                raw = Some(r);
                if start >= from - window && b.q.len() >= MIN_BASELINE {
                    // empty slots count as zero for time windows of a writer
                    let n = if per_writer { slots.max(b.q.len()) } else { b.q.len() };
                    let mut u = [0.0f64; 5];
                    for d in 0..4 {
                        if r[d] > 0.0 {
                            let hi = b.tails[d].at_least(r[d]);
                            u[d] = -(((hi + 1) as f64 / (n + 1) as f64).min(1.0)).log2();
                        }
                    }
                    if c.events >= 5 {
                        let t = &b.tails[4];
                        let lo = t.at_most(r[4]);
                        let hi = t.at_least(r[4]);
                        u[4] = -((2.0 * (lo.min(hi) + 1) as f64 / (t.n + 1) as f64).min(1.0)).log2();
                    }
                    let fresh: Vec<Pick> = c
                        .tools
                        .iter()
                        .filter(|t| b.tools.get(t.0 as usize).copied().unwrap_or(0) == 0)
                        .map(|t| Pick { id: t.2, ts: t.3, kind: kind::TOOL_CALL, tool: t.0, why: 3 })
                        .collect();
                    let c = &mut cells[x];
                    c.unusual = Unusual { rate: u[0], mix: u[1], errors: u[2], new: u[3], size: u[4] };
                    c.unusual_bits = u.iter().sum();
                    c.new_tools = new_tools as u32;
                    for f in fresh {
                        c.pick(f);
                    }
                }
            }
            let c = &mut cells[x];
            c.error_bits = error_bits;
            c.repeat_bits = repeat_bits;
            c.rep_q0 = q0;
            if let Some(r) = raw {
                base[c.sub as usize].raw.insert(x, r);
            }
        }
        for (x, c) in cells.iter().enumerate().take(j).skip(i) {
            let c = c.clone();
            let b = &mut base[c.sub as usize];
            b.change(&c, rep(&c), 1);
            if let Some(raw) = b.raw.get(&x).copied() {
                for (t, v) in b.tails.iter_mut().zip(raw) {
                    t.add(v, 1);
                }
            }
            b.q.push_back(x);
        }
        i = j;
    }
    let _ = by;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binomial_tails_in_bits() {
        // P(X >= 1), n = 1, p = 1/4 -> 2 bits
        assert!((binom_upper_bits(1, 1, 0.25) - 2.0).abs() < 1e-9);
        // P(X >= 3), n = 3, p = 1/2 -> 3 bits
        assert!((binom_upper_bits(3, 3, 0.5) - 3.0).abs() < 1e-9);
        // at or below the mean: 0
        assert_eq!(binom_upper_bits(5, 100, 0.1), 0.0);
        // P(X >= 2), n = 4, p = 0.1: 1 - 0.9^4 - 4*0.1*0.9^3 = 0.0523
        let want = -(1.0 - 0.9f64.powi(4) - 4.0 * 0.1 * 0.9f64.powi(3)).log2();
        assert!((binom_upper_bits(2, 4, 0.1) - want).abs() < 1e-9);
        // large n uses Stirling: monotone in k
        assert!(binom_upper_bits(60, 500, 0.05) > binom_upper_bits(50, 500, 0.05));
        assert!((ln_fact(100) - (1..=100).map(|i| (i as f64).ln()).sum::<f64>()).abs() < 1e-9);
    }

    #[test]
    fn tails_count_like_a_sorted_list() {
        let mut t = Tails::new();
        for v in [0.0, 1.0, 1.0, 3.0, 10.0, 100.0] {
            t.add(v, 1);
        }
        assert_eq!(t.at_least(1.0), 5);
        assert_eq!(t.at_least(0.0), 6);
        assert_eq!(t.at_most(1.0), 3);
        assert_eq!(t.at_least(1000.0), 0);
        t.add(100.0, -1);
        assert_eq!(t.at_least(50.0), 0);
    }

    #[test]
    fn repeats_are_found_among_the_last_calls() {
        let mut r = Ring::new();
        assert_eq!(r.push((1, 10), 100), None);
        assert_eq!(r.push((1, 10), 101), Some(100));
        assert_eq!(r.push((1, 11), 102), None);
        assert_eq!(r.push((1, 10), 103), Some(101), "the latest match");
        for i in 0..REPEAT_RING as u32 {
            r.push((2, i), 200 + u64::from(i));
        }
        assert_eq!(r.push((1, 10), 300), None, "pushed out of the ring");
        assert!(oojuro_tool("mcp__oojuro__search") && !oojuro_tool("Bash") && !oojuro_tool("mcp__other__get"));
    }
}
