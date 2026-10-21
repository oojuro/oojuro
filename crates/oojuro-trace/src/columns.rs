//! Typed columns of every event, in per-agent blocks of slots (block b of
//! agent a = slots [b·BLOCK, (b+1)·BLOCK) of a's event log, in append order),
//! plus a global id → (agent, slot) location array. A slot names the engine
//! row holding its text (`src`) and the row holding its durable fields
//! (`host`): both are the slot's own row for a plain record; a deduplicated
//! event (`dup`) shares another record's text and is described inside the
//! batch's host record (see `oojuro_trace` docs).
//!
//! Agents run their sessions back to back, so a session's events sit in one
//! or two blocks of its agent: session reads decode little. A block starts
//! hot (plain `Col` values); once the agent's rows are `SEAL_LAG` blocks past
//! it (most status revisions of its calls have landed) it is sealed: each
//! field becomes a LEB128 stream of zigzag deltas, the streams are
//! zstd-compressed together, and per-block aggregates (per tool, per
//! session) are kept beside it. Liveness stays outside the compressed bytes
//! (a bitset), so deletes never thaw a block; a revision landing on a sealed
//! block goes to a small override map. A checkpoint file stores every block;
//! the location array and indexes are rebuilt from the blocks on load.
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex},
};

pub(crate) const BLOCK: usize = 1024;
const SEAL_LAG: usize = 2;

pub(crate) const NO_EXIT: i32 = i32::MIN;
pub(crate) const EMPTY: u8 = 255;

/// One event's typed fields, or a status revision's slot (`parent` = the call).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Col {
    pub id: u64,
    pub ts: i64,
    pub duration: i64,
    pub session: u64,
    pub parent: u64,
    /// Slot in the agent's event log (position in its block list).
    pub row: u32,
    /// Engine row of the record holding the text, tool and args.
    pub src: u32,
    /// Engine row of the record that carries this slot's fields.
    pub host: u32,
    pub turn: u32,
    pub seq: u32,
    pub tin: u32,
    pub tout: u32,
    pub exit: i32,
    pub text_len: u32,
    pub agent: u32,
    pub tool: u16,
    pub kind: u8,
    pub status: u8,
    pub live: bool,
    pub revision: bool,
    /// The text lives in another event's record (`src`).
    pub dup: bool,
    /// Not an event: the record at `src` keeps a deleted event's text for
    /// live duplicates of it (`id` = the record's id, `kind` = its kind).
    pub holder: bool,
}

impl Col {
    pub const VACANT: Col = Col {
        id: 0,
        ts: 0,
        duration: 0,
        session: 0,
        parent: 0,
        row: 0,
        src: 0,
        host: 0,
        turn: 0,
        seq: 0,
        tin: 0,
        tout: 0,
        exit: NO_EXIT,
        text_len: 0,
        agent: 0,
        tool: 0,
        kind: EMPTY,
        status: 0,
        live: false,
        revision: false,
        dup: false,
        holder: false,
    };
    pub fn is_event(&self) -> bool {
        self.kind != EMPTY && self.live && !self.revision && !self.holder
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Zone {
    pub min_ts: i64,
    pub max_ts: i64,
}

const NO_ZONE: Zone = Zone { min_ts: i64::MAX, max_ts: i64::MIN };

/// A typed predicate over (kind, status, tool); None = any.
pub(crate) struct Pred {
    pub kind: Option<u8>,
    pub status: Option<u8>,
    pub tool: Option<u16>,
}

impl Pred {
    fn keys(&self, kind: u8, status: u8, tool: u16) -> bool {
        self.kind.is_none_or(|k| k == kind) && self.status.is_none_or(|s| s == status) && self.tool.is_none_or(|t| t == tool)
    }
}

/// Aggregates of a sealed block's live events.
#[derive(Default)]
pub(crate) struct Summary {
    /// tool -> (calls, errors, total duration) over live tool calls
    pub tools: Vec<(u16, u64, u64, i64)>,
    /// session -> tokens in + out over live events
    pub sessions: Vec<(u64, u64)>,
    /// (kind, status, tool) -> live events
    pub counts: Vec<((u8, u8, u16), u64)>,
}

enum Data {
    Hot(Vec<Col>),
    Sealed(Box<[u8]>),
}

struct Block {
    data: Data,
    zone: Zone,
    present: u32,
    /// Sealed blocks only: bit i set = slot i live.
    live: Vec<u64>,
    /// Sealed and unchanged since sealing (no deletes or overrides since).
    summary: Option<Arc<Summary>>,
}

impl Block {
    fn new() -> Self {
        Self { data: Data::Hot(vec![Col::VACANT; BLOCK]), zone: NO_ZONE, present: 0, live: Vec::new(), summary: None }
    }
}

#[derive(Default)]
struct AgentCols {
    blocks: Vec<Block>,
    /// One past the highest row stored.
    rows: u32,
}

type Cache = (HashMap<(u32, usize), Arc<Vec<Col>>>, std::collections::VecDeque<(u32, usize)>);

pub(crate) struct ColStore {
    agents: HashMap<u32, AgentCols>,
    /// id - 1 -> (agent << 32 | row) + 1; 0 = no such id.
    loc: Vec<u64>,
    pub tools: Vec<String>,
    tool_arc: Arc<Vec<String>>,
    pub tool_ids: HashMap<String, u16>,
    pub sessions: HashMap<u64, Vec<u64>>,
    /// The agent instance (session) that spawned each session that has one.
    pub parents: HashMap<u64, u64>,
    /// Latest (status, exit, duration) of calls in sealed blocks revised after sealing.
    overrides: HashMap<u64, (u8, i32, i64)>,
    /// Per agent, built on first use (a search hit whose own event is gone)
    /// and kept current by `put` until the agent's next reclaim: src row ->
    /// ids of the duplicate slots whose text is there.
    dups: Mutex<HashMap<u32, HashMap<u32, Vec<u64>>>>,
    /// Duplicate slots (all agents), for statistics.
    pub dup_slots: usize,
    cache: Mutex<Cache>,
    /// Decoded sealed blocks kept (bounded by the column cache budget).
    cache_blocks: usize,
    seal: bool,
}

fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
#[inline]
fn get(b: &[u8], p: &mut usize) -> u64 {
    let (mut v, mut s) = (0u64, 0);
    loop {
        let x = b[*p];
        *p += 1;
        v |= u64::from(x & 0x7f) << s;
        if x < 0x80 {
            return v;
        }
        s += 7;
    }
}
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn unzz(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

const STREAMS: usize = 16;

/// Checkpoint section of session parents (after the blocks; absent in
/// checkpoints written before agent parents existed, which then load with
/// none: no record written before them carries one).
const PARENTS_TAG: &[u8] = b"PRNT";

/// Block layout: slots in row order; deltas are against the previous slot.
fn encode(cols: &[Col]) -> Box<[u8]> {
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); STREAMS];
    let (mut pts, mut pses, mut pid, mut psrc, mut phost) = (0i64, 0i64, 0i64, 0i64, 0i64);
    for c in cols {
        s[0].push(c.kind);
        s[0].push(c.status | (c.revision as u8) << 4 | (c.dup as u8) << 5 | (c.holder as u8) << 6);
        put(&mut s[1], zz((c.id as i64).wrapping_sub(pid)));
        pid = c.id as i64;
        put(&mut s[2], zz((c.session as i64).wrapping_sub(pses)));
        pses = c.session as i64;
        put(&mut s[3], zz(c.ts.wrapping_sub(pts)));
        pts = c.ts;
        // parent as a distance back from the id (0 = none)
        put(&mut s[4], if c.parent == 0 { 0 } else { zz((c.id as i64).wrapping_sub(c.parent as i64)) + 1 });
        put(&mut s[5], c.turn as u64);
        put(&mut s[6], c.seq as u64);
        put(&mut s[7], if c.exit == NO_EXIT { 0 } else { zz(c.exit as i64) + 1 });
        put(&mut s[8], c.tool as u64);
        put(&mut s[9], zz(c.duration));
        put(&mut s[10], c.tin as u64);
        put(&mut s[11], c.tout as u64);
        put(&mut s[12], c.text_len as u64);
        put(&mut s[13], c.agent as u64);
        put(&mut s[14], zz(i64::from(c.src).wrapping_sub(psrc)));
        psrc = i64::from(c.src);
        put(&mut s[15], zz(i64::from(c.host).wrapping_sub(phost)));
        phost = i64::from(c.host);
    }
    let mut cat = Vec::new();
    put(&mut cat, cols.len() as u64);
    for x in &s {
        put(&mut cat, x.len() as u64);
    }
    for x in &s {
        cat.extend_from_slice(x);
    }
    zstd::bulk::compress(&cat, 3).expect("zstd").into_boxed_slice()
}

/// Decode a block; `first_row` is the row of slot 0. Field by field, one
/// stream at a time (sequential reads beat interleaving 14 cursors).
fn decode(bytes: &[u8], first_row: u32) -> Vec<Col> {
    let cat = zstd::bulk::decompress(bytes, 16 << 20).expect("column block");
    let mut p = 0;
    let n = get(&cat, &mut p) as usize;
    let mut lens = [0usize; STREAMS];
    for l in &mut lens {
        *l = get(&cat, &mut p) as usize;
    }
    let mut starts = [0usize; STREAMS];
    let mut at = p;
    for i in 0..STREAMS {
        starts[i] = at;
        at += lens[i];
    }
    let mut out: Vec<Col> = (0..n).map(|k| Col { row: first_row + k as u32, ..Col::VACANT }).collect();
    let k0 = starts[0];
    for (k, c) in out.iter_mut().enumerate() {
        c.kind = cat[k0 + 2 * k];
        let st = cat[k0 + 2 * k + 1];
        c.status = st & 0x0f;
        c.revision = st & 0x10 != 0;
        c.dup = st & 0x20 != 0;
        c.holder = st & 0x40 != 0;
        c.live = true;
    }
    let (mut q, mut v) = (starts[1], 0i64);
    for c in out.iter_mut() {
        v = v.wrapping_add(unzz(get(&cat, &mut q)));
        c.id = v as u64;
    }
    let (mut q, mut v) = (starts[2], 0i64);
    for c in out.iter_mut() {
        v = v.wrapping_add(unzz(get(&cat, &mut q)));
        c.session = v as u64;
    }
    let (mut q, mut v) = (starts[3], 0i64);
    for c in out.iter_mut() {
        v = v.wrapping_add(unzz(get(&cat, &mut q)));
        c.ts = v;
    }
    let mut q = starts[4];
    for c in out.iter_mut() {
        let x = get(&cat, &mut q);
        c.parent = if x == 0 { 0 } else { (c.id as i64).wrapping_sub(unzz(x - 1)) as u64 };
    }
    macro_rules! field {
        ($i:expr, $f:ident, $t:ty) => {{
            let mut q = starts[$i];
            for c in out.iter_mut() {
                c.$f = get(&cat, &mut q) as $t;
            }
        }};
    }
    field!(5, turn, u32);
    field!(6, seq, u32);
    let mut q = starts[7];
    for c in out.iter_mut() {
        let x = get(&cat, &mut q);
        c.exit = if x == 0 { NO_EXIT } else { unzz(x - 1) as i32 };
    }
    field!(8, tool, u16);
    let mut q = starts[9];
    for c in out.iter_mut() {
        c.duration = unzz(get(&cat, &mut q));
    }
    field!(10, tin, u32);
    field!(11, tout, u32);
    field!(12, text_len, u32);
    field!(13, agent, u32);
    for (i, f) in [(14usize, 0usize), (15, 1)] {
        let (mut q, mut v) = (starts[i], 0i64);
        for c in out.iter_mut() {
            v = v.wrapping_add(unzz(get(&cat, &mut q)));
            if f == 0 {
                c.src = v as u32;
            } else {
                c.host = v as u32;
            }
        }
    }
    out
}

/// One slot's fields for query-time analysis (see [`ColStore::scan_agent`]).
#[derive(Clone, Copy, Default)]
struct Proj {
    id: u64,
    session: u64,
    call: u64,
    ts_us: i64,
    exit: i32,
    text_len: u32,
    tool: u16,
    kind: u8,
    /// status | revision << 4 | dup << 5 | holder << 6
    flags: u8,
}

/// Decodes the streams [`Proj`] needs from sealed blocks, reusing its
/// buffers from block to block.
#[derive(Default)]
struct Projector {
    dec: Option<zstd::bulk::Decompressor<'static>>,
    cat: Vec<u8>,
    rows: Vec<Proj>,
}

impl Projector {
    fn decode(&mut self, bytes: &[u8]) -> &[Proj] {
        let size = zstd::zstd_safe::get_frame_content_size(bytes).ok().flatten().unwrap_or(1 << 20) as usize;
        self.cat.clear();
        self.cat.reserve(size);
        let dec = self.dec.get_or_insert_with(|| zstd::bulk::Decompressor::new().expect("zstd context"));
        dec.decompress_to_buffer(bytes, &mut self.cat).expect("column block");
        let cat = &self.cat;
        let mut p = 0;
        let n = get(cat, &mut p) as usize;
        let mut lens = [0usize; STREAMS];
        for l in &mut lens {
            *l = get(cat, &mut p) as usize;
        }
        let mut starts = [0usize; STREAMS];
        for i in 0..STREAMS {
            starts[i] = p;
            p += lens[i];
        }
        self.rows.clear();
        self.rows.resize(n, Proj::default());
        let rows = &mut self.rows;
        let k0 = starts[0];
        for (k, r) in rows.iter_mut().enumerate() {
            r.kind = cat[k0 + 2 * k];
            r.flags = cat[k0 + 2 * k + 1];
        }
        let (mut q, mut v) = (starts[1], 0i64);
        for r in rows.iter_mut() {
            v = v.wrapping_add(unzz(get(cat, &mut q)));
            r.id = v as u64;
        }
        let (mut q, mut v) = (starts[2], 0i64);
        for r in rows.iter_mut() {
            v = v.wrapping_add(unzz(get(cat, &mut q)));
            r.session = v as u64;
        }
        let (mut q, mut v) = (starts[3], 0i64);
        for r in rows.iter_mut() {
            v = v.wrapping_add(unzz(get(cat, &mut q)));
            r.ts_us = v;
        }
        let mut q = starts[4];
        for r in rows.iter_mut() {
            let x = get(cat, &mut q);
            r.call = if x == 0 { 0 } else { (r.id as i64).wrapping_sub(unzz(x - 1)) as u64 };
        }
        let mut q = starts[7];
        for r in rows.iter_mut() {
            let x = get(cat, &mut q);
            r.exit = if x == 0 { NO_EXIT } else { unzz(x - 1) as i32 };
        }
        let mut q = starts[8];
        for r in rows.iter_mut() {
            r.tool = get(cat, &mut q) as u16;
        }
        let mut q = starts[12];
        for r in rows.iter_mut() {
            r.text_len = get(cat, &mut q) as u32;
        }
        &self.rows
    }
}

fn summarize(cols: &[Col], live: &[u64]) -> Summary {
    let mut tools: HashMap<u16, (u64, u64, i64)> = HashMap::new();
    let mut sessions: HashMap<u64, u64> = HashMap::new();
    let mut counts: HashMap<(u8, u8, u16), u64> = HashMap::new();
    for (i, c) in cols.iter().enumerate() {
        if c.kind == EMPTY || c.revision || c.holder || live[i / 64] >> (i % 64) & 1 == 0 {
            continue;
        }
        *counts.entry((c.kind, c.status, c.tool)).or_default() += 1;
        *sessions.entry(c.session).or_default() += c.tin as u64 + c.tout as u64;
        if c.kind == crate::KIND_TOOL_CALL {
            let t = tools.entry(c.tool).or_default();
            t.0 += 1;
            t.1 += (c.status == crate::ST_ERROR) as u64;
            t.2 += c.duration;
        }
    }
    Summary {
        tools: tools.into_iter().map(|(k, v)| (k, v.0, v.1, v.2)).collect(),
        sessions: sessions.into_iter().collect(),
        counts: counts.into_iter().collect(),
    }
}

fn live_bits(v: &[Col]) -> Vec<u64> {
    let mut live = vec![0u64; BLOCK / 64];
    for (k, c) in v.iter().enumerate() {
        if c.live {
            live[k / 64] |= 1 << (k % 64);
        }
    }
    live
}

fn zone_of(v: &[Col]) -> Zone {
    let mut z = NO_ZONE;
    for c in v {
        if c.kind != EMPTY && !c.revision && !c.holder {
            z.min_ts = z.min_ts.min(c.ts);
            z.max_ts = z.max_ts.max(c.ts);
        }
    }
    z
}

impl ColStore {
    pub fn new(seal: bool) -> Self {
        Self::with_cache(seal, 128 << 20)
    }

    /// `cache_bytes` bounds the decoded copies of sealed blocks.
    pub fn with_cache(seal: bool, cache_bytes: usize) -> Self {
        let mut c = Self {
            cache_blocks: (cache_bytes / (BLOCK * std::mem::size_of::<Col>())).max(16),
            agents: HashMap::new(),
            loc: Vec::new(),
            tools: Vec::new(),
            tool_arc: Arc::new(Vec::new()),
            tool_ids: HashMap::new(),
            sessions: HashMap::new(),
            parents: HashMap::new(),
            overrides: HashMap::new(),
            dups: Mutex::new(HashMap::new()),
            dup_slots: 0,
            cache: Mutex::new((HashMap::new(), std::collections::VecDeque::new())),
            seal,
        };
        c.tool("");
        c
    }

    pub fn tool(&mut self, t: &str) -> u16 {
        if let Some(&i) = self.tool_ids.get(t) {
            return i;
        }
        let i = self.tools.len() as u16;
        self.tools.push(t.to_string());
        self.tool_ids.insert(t.to_string(), i);
        self.tool_arc = Arc::new(self.tools.clone());
        i
    }

    /// Record that `session` was spawned by `parent` (first one wins).
    pub fn set_parent(&mut self, session: u64, parent: u64) {
        if parent != 0 && parent != session {
            self.parents.entry(session).or_insert(parent);
        }
    }

    /// Ids of the duplicates whose text is at `agent`'s engine row `src`.
    pub fn dups_of(&self, agent: u32, src: u32) -> Vec<u64> {
        if let Some(m) = self.dups.lock().unwrap().get(&agent) {
            return m.get(&src).cloned().unwrap_or_default();
        }
        let mut m: HashMap<u32, Vec<u64>> = HashMap::new();
        if let Some(a) = self.agents.get(&agent) {
            for b in 0..a.blocks.len() {
                self.visit_block(agent, b, |c| {
                    if c.dup {
                        m.entry(c.src).or_default().push(c.id);
                    }
                });
            }
        }
        let out = m.get(&src).cloned().unwrap_or_default();
        self.dups.lock().unwrap().insert(agent, m);
        out
    }

    /// The next free slot of `agent` (one past its highest slot).
    pub fn next_slot(&self, agent: u32) -> u32 {
        self.agents.get(&agent).map_or(0, |a| a.rows)
    }

    /// Tool names by id (shared snapshot; cheap to take per read).
    pub fn tool_names(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.tool_arc)
    }

    fn locate(&self, id: u64) -> Option<(u32, u32)> {
        let x = *self.loc.get(id.checked_sub(1)? as usize)?;
        (x != 0).then(|| (((x - 1) >> 32) as u32, (x - 1) as u32))
    }

    fn set_loc(&mut self, id: u64, agent: u32, row: u32) {
        let i = (id - 1) as usize;
        if i >= self.loc.len() {
            self.loc.resize(i + 1, 0);
        }
        self.loc[i] = ((u64::from(agent) << 32) | u64::from(row)) + 1;
    }

    /// Plain columns of a sealed block (decoded through a small cache).
    fn sealed_cols(&self, agent: u32, b: usize, bytes: &[u8]) -> Arc<Vec<Col>> {
        if let Some(v) = self.cache.lock().unwrap().0.get(&(agent, b)) {
            return Arc::clone(v);
        }
        let v = Arc::new(decode(bytes, (b * BLOCK) as u32));
        let mut g = self.cache.lock().unwrap();
        if g.0.insert((agent, b), Arc::clone(&v)).is_none() {
            g.1.push_back((agent, b));
            if g.1.len() > self.cache_blocks {
                if let Some(old) = g.1.pop_front() {
                    g.0.remove(&old);
                }
            }
        }
        v
    }

    fn drop_cached(&self, agent: u32, b: usize) {
        let mut g = self.cache.lock().unwrap();
        if g.0.remove(&(agent, b)).is_some() {
            g.1.retain(|x| *x != (agent, b));
        }
    }

    /// Apply live bits and overrides of a sealed block to a slot.
    fn view(&self, blk: &Block, k: usize, mut c: Col) -> Col {
        if let Data::Sealed(_) = blk.data {
            c.live = blk.live[k / 64] >> (k % 64) & 1 == 1;
            if let Some(&(s, e, d)) = self.overrides.get(&c.id) {
                c.status = s;
                c.exit = e;
                c.duration = d;
            }
        }
        c
    }

    /// Visit an agent block's occupied slots as they read.
    fn visit_block(&self, agent: u32, b: usize, mut f: impl FnMut(Col)) {
        let blk = &self.agents[&agent].blocks[b];
        match &blk.data {
            Data::Hot(v) => {
                for c in v {
                    if c.kind != EMPTY {
                        f(*c);
                    }
                }
            }
            Data::Sealed(bytes) => {
                let cols = self.sealed_cols(agent, b, bytes);
                for (k, c) in cols.iter().enumerate() {
                    if c.kind != EMPTY {
                        f(self.view(blk, k, *c));
                    }
                }
            }
        }
    }

    pub fn get(&self, id: u64) -> Option<Col> {
        let (agent, row) = self.locate(id)?;
        let a = self.agents.get(&agent)?;
        let (b, k) = (row as usize / BLOCK, row as usize % BLOCK);
        let blk = a.blocks.get(b)?;
        let c = match &blk.data {
            Data::Hot(v) => v[k],
            Data::Sealed(bytes) => self.sealed_cols(agent, b, bytes)[k],
        };
        (c.kind != EMPTY && c.id == id).then(|| self.view(blk, k, c))
    }

    /// Store a new slot (an event or a revision record) at its agent row.
    pub fn put(&mut self, col: Col) {
        let (agent, row) = (col.agent, col.row);
        let (b, k) = (row as usize / BLOCK, row as usize % BLOCK);
        let a = self.agents.entry(agent).or_default();
        while a.blocks.len() <= b {
            a.blocks.push(Block::new());
        }
        a.rows = a.rows.max(row + 1);
        if let Data::Sealed(_) = a.blocks[b].data {
            self.thaw(agent, b);
        }
        let blk = &mut self.agents.get_mut(&agent).unwrap().blocks[b];
        let Data::Hot(v) = &mut blk.data else { unreachable!() };
        if v[k].kind == EMPTY {
            blk.present += 1;
        }
        v[k] = col;
        if !col.revision && !col.holder && col.kind != EMPTY {
            blk.zone.min_ts = blk.zone.min_ts.min(col.ts);
            blk.zone.max_ts = blk.zone.max_ts.max(col.ts);
            self.sessions.entry(col.session).or_default().push(col.id);
        }
        if col.dup {
            self.dup_slots += 1;
            if let Some(m) = self.dups.get_mut().unwrap().get_mut(&agent) {
                m.entry(col.src).or_default().push(col.id);
            }
        }
        self.set_loc(col.id, agent, row);
        self.maybe_seal(agent);
    }

    /// The latest status of a tool call.
    pub fn revise(&mut self, call: u64, status: u8, exit: i32, duration: i64) {
        let Some((agent, row)) = self.locate(call) else { return };
        let (b, k) = (row as usize / BLOCK, row as usize % BLOCK);
        let blk = &mut self.agents.get_mut(&agent).unwrap().blocks[b];
        match &mut blk.data {
            Data::Hot(v) => {
                let c = &mut v[k];
                if c.kind != EMPTY && c.id == call {
                    c.status = status;
                    c.exit = exit;
                    c.duration = duration;
                }
            }
            Data::Sealed(_) => {
                self.overrides.insert(call, (status, exit, duration));
                blk.summary = None;
            }
        }
    }

    pub fn kill(&mut self, id: u64) {
        let Some((agent, row)) = self.locate(id) else { return };
        let (b, k) = (row as usize / BLOCK, row as usize % BLOCK);
        let blk = &mut self.agents.get_mut(&agent).unwrap().blocks[b];
        match &mut blk.data {
            Data::Hot(v) => {
                if v[k].id == id {
                    v[k].live = false;
                }
            }
            Data::Sealed(_) => {
                blk.live[k / 64] &= !(1u64 << (k % 64));
                blk.summary = None;
            }
        }
    }

    pub fn kill_before(&mut self, ts: i64) {
        let mut dead = Vec::new();
        for (&agent, a) in &self.agents {
            for (b, blk) in a.blocks.iter().enumerate() {
                if blk.zone.min_ts >= ts {
                    continue;
                }
                self.visit_block(agent, b, |c| {
                    if !c.revision && !c.holder && c.ts < ts && c.live {
                        dead.push(c.id);
                    }
                });
            }
        }
        for id in dead {
            self.kill(id);
        }
    }

    fn maybe_seal(&mut self, agent: u32) {
        if !self.seal {
            return;
        }
        let a = &self.agents[&agent];
        let top = a.rows.saturating_sub(1) as usize / BLOCK;
        let due: Vec<usize> = (0..a.blocks.len())
            .take_while(|b| b + SEAL_LAG <= top)
            .filter(|&b| a.blocks[b].present as usize == BLOCK && matches!(a.blocks[b].data, Data::Hot(_)))
            .collect();
        for b in due {
            self.seal_block(agent, b);
        }
    }

    fn seal_block(&mut self, agent: u32, b: usize) {
        let blk = &mut self.agents.get_mut(&agent).unwrap().blocks[b];
        let Data::Hot(v) = &blk.data else { return };
        let live = live_bits(v);
        let summary = summarize(v, &live);
        blk.data = Data::Sealed(encode(v));
        blk.live = live;
        blk.summary = Some(Arc::new(summary));
    }

    /// Turn a sealed block back into plain columns (overrides folded in).
    fn thaw(&mut self, agent: u32, b: usize) {
        let mut v = vec![Col::VACANT; BLOCK];
        {
            let blk = &self.agents[&agent].blocks[b];
            let Data::Sealed(bytes) = &blk.data else { return };
            let cols = self.sealed_cols(agent, b, bytes);
            for (k, c) in cols.iter().enumerate() {
                v[k] = if c.kind == EMPTY { *c } else { self.view(blk, k, *c) };
            }
        }
        for c in &v {
            if c.kind != EMPTY {
                self.overrides.remove(&c.id);
            }
        }
        let blk = &mut self.agents.get_mut(&agent).unwrap().blocks[b];
        blk.data = Data::Hot(v);
        blk.live = Vec::new();
        blk.summary = None;
        self.drop_cached(agent, b);
    }

    /// Every slot (events, revisions, dead ones).
    pub fn for_each(&self, mut f: impl FnMut(&Col)) {
        for (&agent, a) in &self.agents {
            for b in 0..a.blocks.len() {
                self.visit_block(agent, b, |c| f(&c));
            }
        }
    }

    /// Live events with `from <= ts < to`, pruning by zone.
    pub fn scan_time(&self, from: i64, to: i64, mut f: impl FnMut(&Col)) {
        for (&agent, a) in &self.agents {
            for (b, blk) in a.blocks.iter().enumerate() {
                if blk.zone.max_ts < from || blk.zone.min_ts >= to {
                    continue;
                }
                self.visit_block(agent, b, |c| {
                    if c.is_event() && c.ts >= from && c.ts < to {
                        f(&c);
                    }
                });
            }
        }
    }

    /// Live events of one agent with `from <= ts < to`, in slot order, for
    /// query-time analysis (typed fields only). A sealed block comes from
    /// the cache when it is there; otherwise only the streams these fields
    /// need are decoded, into buffers reused across the scan, and nothing
    /// is added to the cache (a long scan never evicts the blocks other
    /// reads keep warm).
    pub fn scan_agent(&self, agent: u32, from: i64, to: i64, mut f: impl FnMut(&crate::ActivityRow)) {
        let Some(a) = self.agents.get(&agent) else { return };
        let plain = self.overrides.is_empty();
        let row = |c: &Col| crate::ActivityRow {
            id: c.id,
            session: c.session,
            call: c.parent,
            ts_us: c.ts,
            text_len: c.text_len,
            exit_code: (c.exit != NO_EXIT).then_some(c.exit),
            tool: c.tool,
            kind: c.kind,
            status: c.status,
        };
        let mut proj = Projector::default();
        for (b, blk) in a.blocks.iter().enumerate() {
            if blk.zone.max_ts < from || blk.zone.min_ts >= to {
                continue;
            }
            match &blk.data {
                Data::Hot(v) => {
                    for c in v {
                        if c.is_event() && c.ts >= from && c.ts < to {
                            f(&row(c));
                        }
                    }
                }
                Data::Sealed(bytes) => {
                    let cached = self.cache.lock().unwrap().0.get(&(agent, b)).cloned();
                    if let Some(cols) = cached {
                        for (k, c) in cols.iter().enumerate() {
                            if c.kind == EMPTY || c.revision || c.holder || c.ts < from || c.ts >= to {
                                continue;
                            }
                            if blk.live[k / 64] >> (k % 64) & 1 == 0 {
                                continue;
                            }
                            f(&row(&if plain { *c } else { self.view(blk, k, *c) }));
                        }
                        continue;
                    }
                    for (k, r) in proj.decode(bytes).iter().enumerate() {
                        if r.kind == EMPTY || r.flags & 0x50 != 0 || r.ts_us < from || r.ts_us >= to {
                            continue; // empty, revision or holder slots, outside the window
                        }
                        if blk.live[k / 64] >> (k % 64) & 1 == 0 {
                            continue;
                        }
                        let mut out = crate::ActivityRow {
                            id: r.id,
                            session: r.session,
                            call: r.call,
                            ts_us: r.ts_us,
                            text_len: r.text_len,
                            exit_code: (r.exit != NO_EXIT).then_some(r.exit),
                            tool: r.tool,
                            kind: r.kind,
                            status: r.flags & 0x0f,
                        };
                        if !plain && let Some(&(st, ex, _)) = self.overrides.get(&r.id) {
                            out.status = st;
                            out.exit_code = (ex != NO_EXIT).then_some(ex);
                        }
                        f(&out);
                    }
                }
            }
        }
    }

    /// Count live events matching a typed filter: blocks of other agents are
    /// skipped, whole sealed blocks inside the window answer from their
    /// (kind, status, tool) counts, the rest are decoded.
    pub fn count_where(&self, from: i64, to: i64, agent: Option<u32>, pred: &Pred) -> u64 {
        let mut n = 0;
        for (&a, ac) in &self.agents {
            if agent.is_some_and(|x| x != a) {
                continue;
            }
            for (b, blk) in ac.blocks.iter().enumerate() {
                let z = blk.zone;
                if z.max_ts < from || z.min_ts >= to {
                    continue;
                }
                if z.min_ts >= from && z.max_ts < to {
                    if let Some(s) = &blk.summary {
                        n += s.counts.iter().filter(|((k, st, t), _)| pred.keys(*k, *st, *t)).map(|x| x.1).sum::<u64>();
                        continue;
                    }
                }
                self.visit_block(a, b, |c| {
                    if c.is_event() && c.ts >= from && c.ts < to && pred.keys(c.kind, c.status, c.tool) {
                        n += 1;
                    }
                });
            }
        }
        n
    }

    /// The first `limit` matching live events in (ts, id) order. Blocks are
    /// visited by their earliest time and the scan stops once no remaining
    /// block can hold an earlier match.
    pub fn first_where(&self, from: i64, to: i64, agent: Option<u32>, pred: &Pred, limit: usize) -> Vec<(i64, u64)> {
        let mut blocks: Vec<(i64, u32, usize)> = Vec::new();
        for (&a, ac) in &self.agents {
            if agent.is_some_and(|x| x != a) {
                continue;
            }
            for (b, blk) in ac.blocks.iter().enumerate() {
                let z = blk.zone;
                if z.max_ts >= from && z.min_ts < to {
                    blocks.push((z.min_ts.max(from), a, b));
                }
            }
        }
        blocks.sort_unstable();
        let mut best: std::collections::BinaryHeap<(i64, u64)> = std::collections::BinaryHeap::new();
        for (start, a, b) in blocks {
            if best.len() >= limit && best.peek().is_some_and(|w| start > w.0) {
                break;
            }
            self.visit_block(a, b, |c| {
                if c.is_event() && c.ts >= from && c.ts < to && pred.keys(c.kind, c.status, c.tool) {
                    best.push((c.ts, c.id));
                    if best.len() > limit {
                        best.pop();
                    }
                }
            });
        }
        let mut v = best.into_vec();
        v.sort_unstable();
        v
    }

    /// Visit, per block, either its summary (block entirely inside the
    /// window, sealed and unchanged) or its live events in the window.
    pub fn scan_agg(&self, from: i64, to: i64, mut whole: impl FnMut(&Summary), mut each: impl FnMut(&Col)) {
        for (&agent, a) in &self.agents {
            for (b, blk) in a.blocks.iter().enumerate() {
                let z = blk.zone;
                if z.max_ts < from || z.min_ts >= to {
                    continue;
                }
                if z.min_ts >= from && z.max_ts < to {
                    if let Some(s) = &blk.summary {
                        whole(s);
                        continue;
                    }
                }
                self.visit_block(agent, b, |c| {
                    if c.is_event() && c.ts >= from && c.ts < to {
                        each(&c);
                    }
                });
            }
        }
    }

    /// After a reclaim of `agent`: kept slots move to their new (slot, src,
    /// host) (a rewrite) or stay (an erase); every other slot of the agent goes.
    /// Slots named in `holders` become text holders.
    pub fn rewrite_agent(&mut self, agent: u32, kept: &HashMap<u64, (u32, u32, u32)>, holders: &std::collections::HashSet<u64>) {
        let mut slots: Vec<Col> = Vec::new();
        if self.agents.contains_key(&agent) {
            let n = self.agents[&agent].blocks.len();
            for b in 0..n {
                self.visit_block(agent, b, |c| slots.push(c));
                self.drop_cached(agent, b);
            }
        }
        let gone: Vec<u64> = slots.iter().filter(|c| !kept.contains_key(&c.id)).map(|c| c.id).collect();
        for id in &gone {
            if let Some(x) = self.loc.get_mut((id - 1) as usize) {
                *x = 0;
            }
            self.overrides.remove(id);
        }
        let mut a = AgentCols::default();
        self.dups.get_mut().unwrap().remove(&agent);
        self.dup_slots -= slots.iter().filter(|c| c.dup).count();
        for mut c in slots.into_iter().filter(|c| kept.contains_key(&c.id)) {
            (c.row, c.src, c.host) = kept[&c.id];
            if holders.contains(&c.id) {
                c.holder = true;
                c.dup = false;
                c.live = true;
            }
            self.dup_slots += c.dup as usize;

            let (b, k) = (c.row as usize / BLOCK, c.row as usize % BLOCK);
            while a.blocks.len() <= b {
                a.blocks.push(Block::new());
            }
            a.rows = a.rows.max(c.row + 1);
            let blk = &mut a.blocks[b];
            let Data::Hot(v) = &mut blk.data else { unreachable!() };
            v[k] = c;
            blk.present += 1;
            self.overrides.remove(&c.id);
            self.loc[(c.id - 1) as usize] = ((u64::from(agent) << 32) | u64::from(c.row)) + 1;
        }
        for blk in &mut a.blocks {
            if let Data::Hot(v) = &blk.data {
                blk.zone = zone_of(v);
            }
        }
        self.agents.insert(agent, a);
        let gone: std::collections::HashSet<u64> = gone.into_iter().chain(holders.iter().copied()).collect();
        for ids in self.sessions.values_mut() {
            ids.retain(|id| !gone.contains(id));
        }
        self.sessions.retain(|_, ids| !ids.is_empty());
        let sessions = &self.sessions;
        self.parents.retain(|s, _| sessions.contains_key(s));
        self.reseal(agent);
    }

    /// Seal every block of `agent` below its last one (holes count as done).
    fn reseal(&mut self, agent: u32) {
        if !self.seal {
            return;
        }
        let n = self.agents[&agent].blocks.len();
        for b in 0..n.saturating_sub(1) {
            if matches!(self.agents[&agent].blocks[b].data, Data::Hot(_)) {
                self.seal_block(agent, b);
            }
        }
    }

    /// Memory held by the columns: (sealed bytes incl. live bits, hot bytes, location bytes).
    pub fn bytes(&self) -> (usize, usize, usize) {
        let (mut sealed, mut hot) = (0, 0);
        for a in self.agents.values() {
            for b in &a.blocks {
                match &b.data {
                    Data::Hot(v) => hot += v.len() * std::mem::size_of::<Col>(),
                    Data::Sealed(x) => sealed += x.len() + b.live.len() * 8,
                }
            }
        }
        (sealed, hot, self.loc.len() * 8)
    }

    // ------------------------------------------------------- persistence

    /// Write a checkpoint: every block (hot ones encoded too), live bits,
    /// overrides, tools, and the per-agent row watermarks up to which the
    /// columns reflect the engine. Atomic (tmp + rename + directory sync).
    pub fn save(&self, path: &Path, watermarks: &HashMap<u32, u32>) -> std::io::Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(b"OJCOLS03");
        put(&mut out, self.tools.len() as u64);
        for t in &self.tools {
            put(&mut out, t.len() as u64);
            out.extend_from_slice(t.as_bytes());
        }
        let mut wm: Vec<(&u32, &u32)> = watermarks.iter().collect();
        wm.sort();
        put(&mut out, wm.len() as u64);
        for (a, r) in wm {
            put(&mut out, *a as u64);
            put(&mut out, *r as u64);
        }
        put(&mut out, self.overrides.len() as u64);
        for (id, (s, e, d)) in &self.overrides {
            put(&mut out, *id);
            put(&mut out, *s as u64);
            put(&mut out, zz(*e as i64));
            put(&mut out, zz(*d));
        }
        let mut agents: Vec<&u32> = self.agents.keys().collect();
        agents.sort();
        put(&mut out, agents.len() as u64);
        for agent in agents {
            let a = &self.agents[agent];
            put(&mut out, *agent as u64);
            put(&mut out, a.rows as u64);
            put(&mut out, a.blocks.len() as u64);
            for blk in &a.blocks {
                let (bytes, live) = match &blk.data {
                    Data::Sealed(x) => (x.clone(), blk.live.clone()),
                    Data::Hot(v) => (encode(v), live_bits(v)),
                };
                put(&mut out, matches!(blk.data, Data::Sealed(_)) as u64);
                put(&mut out, blk.present as u64);
                put(&mut out, zz(blk.zone.min_ts));
                put(&mut out, zz(blk.zone.max_ts));
                put(&mut out, bytes.len() as u64);
                out.extend_from_slice(&bytes);
                for w in live {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            }
        }
        // optional trailing section (older checkpoints end before it)
        out.extend_from_slice(PARENTS_TAG);
        let mut pairs: Vec<(&u64, &u64)> = self.parents.iter().collect();
        pairs.sort_unstable();
        put(&mut out, pairs.len() as u64);
        for (s, p) in pairs {
            out.extend_from_slice(&s.to_le_bytes());
            out.extend_from_slice(&p.to_le_bytes());
        }
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        let tmp = path.with_extension("tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(&out)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        if let Some(d) = path.parent() {
            File::open(d)?.sync_all()?;
        }
        Ok(())
    }

    /// Load a checkpoint: the store and its per-agent row watermarks. A
    /// missing, older-format or damaged checkpoint returns `None` (rebuild
    /// from the records).
    pub fn load(path: &Path, seal: bool, cache_bytes: usize) -> std::io::Result<Option<(Self, HashMap<u32, u32>)>> {
        let mut buf = Vec::new();
        match File::open(path) {
            Ok(mut f) => f.read_to_end(&mut buf)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if buf.len() < 12 || &buf[..8] != b"OJCOLS03" {
            return Ok(None);
        }
        let (b, crc) = buf.split_at(buf.len() - 4);
        if crc32fast::hash(b) != u32::from_le_bytes(crc.try_into().unwrap()) {
            return Ok(None);
        }
        let mut p = 8;
        let mut c = Self::with_cache(seal, cache_bytes);
        c.tools.clear();
        c.tool_ids.clear();
        for _ in 0..get(b, &mut p) {
            let l = get(b, &mut p) as usize;
            let t = String::from_utf8_lossy(&b[p..p + l]).into_owned();
            p += l;
            c.tool(&t);
        }
        let mut wm = HashMap::new();
        for _ in 0..get(b, &mut p) {
            let a = get(b, &mut p) as u32;
            wm.insert(a, get(b, &mut p) as u32);
        }
        for _ in 0..get(b, &mut p) {
            let id = get(b, &mut p);
            let s = get(b, &mut p) as u8;
            let e = unzz(get(b, &mut p)) as i32;
            let d = unzz(get(b, &mut p));
            c.overrides.insert(id, (s, e, d));
        }
        for _ in 0..get(b, &mut p) {
            let agent = get(b, &mut p) as u32;
            let rows = get(b, &mut p) as u32;
            let nb = get(b, &mut p) as usize;
            let mut a = AgentCols { blocks: Vec::with_capacity(nb), rows };
            for bi in 0..nb {
                let sealed = get(b, &mut p) == 1;
                let present = get(b, &mut p) as u32;
                let zone = Zone { min_ts: unzz(get(b, &mut p)), max_ts: unzz(get(b, &mut p)) };
                let l = get(b, &mut p) as usize;
                let bytes: Box<[u8]> = b[p..p + l].into();
                p += l;
                let mut live = vec![0u64; BLOCK / 64];
                for w in &mut live {
                    *w = u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
                    p += 8;
                }
                let mut cols = decode(&bytes, (bi * BLOCK) as u32);
                for (k, col) in cols.iter_mut().enumerate() {
                    if col.kind == EMPTY {
                        continue;
                    }
                    col.live = live[k / 64] >> (k % 64) & 1 == 1;
                    c.set_loc(col.id, agent, col.row);
                    if !col.revision && !col.holder {
                        c.sessions.entry(col.session).or_default().push(col.id);
                    }
                    c.dup_slots += col.dup as usize;
                }
                if sealed {
                    let dirty = cols.iter().any(|x| x.kind != EMPTY && c.overrides.contains_key(&x.id));
                    let summary = (!dirty).then(|| Arc::new(summarize(&cols, &live)));
                    a.blocks.push(Block { data: Data::Sealed(bytes), zone, present, live, summary });
                } else {
                    a.blocks.push(Block { data: Data::Hot(cols), zone, present, live: Vec::new(), summary: None });
                }
            }
            c.agents.insert(agent, a);
        }
        if b.len() >= p + PARENTS_TAG.len() && &b[p..p + PARENTS_TAG.len()] == PARENTS_TAG {
            p += PARENTS_TAG.len();
            for _ in 0..get(b, &mut p) {
                let s = u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
                let par = u64::from_le_bytes(b[p + 8..p + 16].try_into().unwrap());
                p += 16;
                c.parents.insert(s, par);
            }
        }
        for ids in c.sessions.values_mut() {
            ids.sort_unstable();
        }
        Ok(Some((c, wm)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four agents, ids dealt round-robin, rows dense per agent.
    fn col(i: u64) -> Col {
        Col {
            id: i,
            ts: 1_788_220_800_000_000 + (i as i64 * 7919) % 100_000 - 50_000,
            duration: if i % 5 == 0 { -3 } else { (i as i64) << 20 },
            session: 1 + (i * 31) % 97,
            parent: if i % 3 == 0 { 0 } else { i / 2 },
            row: ((i - 1) / 4) as u32,
            src: if i % 7 == 0 { ((i - 1) / 9) as u32 } else { ((i - 1) / 4) as u32 },
            host: if i % 7 == 0 { ((i - 1) / 5) as u32 } else { ((i - 1) / 4) as u32 },
            turn: (i % 50) as u32,
            seq: (i % 400) as u32,
            tin: (i * 977 % 100_000) as u32,
            tout: (i % 3000) as u32,
            exit: match i % 4 {
                0 => NO_EXIT,
                1 => -1,
                2 => i32::MAX,
                _ => (i % 256) as i32,
            },
            text_len: (i * 1013 % 70_000) as u32,
            agent: ((i - 1) % 4) as u32,
            tool: (i % 40) as u16,
            kind: (i % 6) as u8,
            status: (i % 4) as u8,
            live: true,
            revision: i % 11 == 0,
            dup: i % 7 == 0 && i % 11 != 0,
            holder: i % 13 == 0 && i % 7 != 0 && i % 11 != 0,
        }
    }

    #[test]
    fn block_codec_roundtrips_edge_values() {
        let v: Vec<Col> = (1..=BLOCK as u64)
            .map(|i| {
                if i % 97 == 0 {
                    return Col { row: (i - 1) as u32, ..Col::VACANT };
                }
                Col { row: (i - 1) as u32, ..col(i) }
            })
            .collect();
        let back = decode(&encode(&v), 0);
        for (a, b) in v.iter().zip(&back) {
            if a.kind == EMPTY {
                assert_eq!(b.kind, EMPTY);
                continue;
            }
            assert_eq!(a, b);
        }
    }

    #[test]
    #[ignore]
    fn block_decode_speed() {
        let v: Vec<Col> = (1..=BLOCK as u64).map(|i| Col { row: (i - 1) as u32, ..col(i) }).collect();
        let e = encode(&v);
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(decode(&e, 0));
        }
        eprintln!("decode: {:.1} us/block of {BLOCK} ({} B)", t.elapsed().as_secs_f64() * 1e6 / 1000.0, e.len());
    }

    #[test]
    fn sealed_reads_equal_hot_reads_and_checkpoint_roundtrips() {
        let mut hot = ColStore::new(false);
        let mut sealed = ColStore::new(true);
        let n = (BLOCK * 4 * 5) as u64;
        for id in 1..=n {
            hot.put(col(id));
            sealed.put(col(id));
        }
        for id in (1..(BLOCK * 4) as u64).step_by(37) {
            hot.revise(id, 3, -7, 99);
            sealed.revise(id, 3, -7, 99);
        }
        for id in (1..n).step_by(101) {
            hot.kill(id);
            sealed.kill(id);
        }
        hot.kill_before(1_788_220_800_000_000 - 40_000);
        sealed.kill_before(1_788_220_800_000_000 - 40_000);
        assert!(sealed.bytes().0 > 0, "some blocks sealed");
        for id in 1..=n {
            assert_eq!(hot.get(id), sealed.get(id), "id {id}");
        }
        let mut a = Vec::new();
        let mut b = Vec::new();
        hot.scan_time(i64::MIN, i64::MAX, |c| a.push(*c));
        sealed.scan_time(i64::MIN, i64::MAX, |c| b.push(*c));
        a.sort_by_key(|c| c.id);
        b.sort_by_key(|c| c.id);
        assert_eq!(a, b);
        let dir = std::env::temp_dir().join(format!("oojuro-cols-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cols.ckpt");
        let marks: HashMap<u32, u32> = [(3, 77)].into_iter().collect();
        sealed.save(&path, &marks).unwrap();
        let (loaded, m) = ColStore::load(&path, true, 128 << 20).unwrap().unwrap();
        assert_eq!(m, marks);
        for id in 1..=n {
            assert_eq!(loaded.get(id), sealed.get(id), "loaded id {id}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
