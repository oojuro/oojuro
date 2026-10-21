//! TBEV0001 event files (see `scripts/tracedb/build_events.py`) and scale
//! tiers made by replicating the base stream with remapped ids.
//!
//! Tier `R` = R replicas of the base. Replica `r` keeps every text byte for
//! byte, shifts its session ids by `r * sessions`, shifts its clock by a
//! hashed offset below one day, and keeps the base's 16 agents (so each
//! agent's store grows with the tier). The replicas are merged by append
//! time; ids are reassigned densely in merged order and parent/revises
//! references are remapped. Replication makes exact repeats that dedupe can
//! collapse: storage results report the 1x tier and dedupe-off arms beside it.
use memmap2::Mmap;
use std::{fs::File, path::Path};

pub const SYSTEM: u8 = 0;
pub const USER: u8 = 1;
pub const THINKING: u8 = 2;
pub const OUTPUT: u8 = 3;
pub const TOOL_CALL: u8 = 4;
pub const TOOL_RESULT: u8 = 5;
pub const KIND_NAMES: [&str; 6] = ["system", "user", "thinking", "output", "tool_call", "tool_result"];

pub const ST_NONE: u8 = 0;
pub const ST_PENDING: u8 = 1;
pub const ST_OK: u8 = 2;
pub const ST_ERROR: u8 = 3;

/// TraceBench v1 indexing rule: thinking and system prompts are stored only
/// (design decision: store-only thinking wins on next-turn retrieval); status
/// revisions carry no text.
pub fn indexed_kind(kind: u8) -> bool {
    matches!(kind, USER | OUTPUT | TOOL_CALL | TOOL_RESULT)
}

const HEAD: usize = 8 + 8 + 4 + 4 + 4 + 8 + 8 + 1 + 1 + 4 + 4 + 4 + 8 + 8 + 2 + 4 + 4;

/// One event. Numeric fields are owned (remapped per replica); text is borrowed
/// from the mapped base file.
#[derive(Clone, Copy, Debug)]
pub struct Event<'a> {
    pub id: u64,
    pub session: u64,
    pub agent: u32,
    pub turn: u32,
    pub seq: u32,
    /// 0 = none.
    pub parent: u64,
    /// Non-zero for a status revision: the tool call it completes.
    pub revises: u64,
    pub kind: u8,
    pub status: u8,
    pub exit_code: Option<i32>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub ts_us: i64,
    pub duration_us: i64,
    pub tool: &'a str,
    pub args: &'a str,
    pub text: &'a str,
}

impl Event<'_> {
    pub fn raw_bytes(&self) -> u64 {
        (self.tool.len() + self.args.len() + self.text.len()) as u64
    }
    pub fn is_revision(&self) -> bool {
        self.revises != 0
    }
}

#[derive(Clone, Copy, Debug)]
struct BaseBatch {
    off: u64,
    n: u32,
    first_id: u64,
    append_ts: i64,
    agent: u32,
    session: u64,
}

pub struct Base {
    map: Mmap,
    batches: Vec<BaseBatch>,
    pub events: u64,
    pub sessions: u64,
    pub raw_bytes: u64,
    pub min_ts: i64,
    pub max_ts: i64,
}

fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
fn i64_at(b: &[u8], p: usize) -> i64 {
    i64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

impl Base {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let f = File::open(path)?;
        let map = unsafe { Mmap::map(&f)? };
        if map.len() < 8 || &map[..8] != b"TBEV0001" {
            return Err(std::io::Error::other("not a TBEV0001 file"));
        }
        let mut batches = Vec::new();
        let (mut p, mut events, mut sessions, mut raw) = (8usize, 0u64, 0u64, 0u64);
        let (mut min_ts, mut max_ts) = (i64::MAX, i64::MIN);
        while p < map.len() {
            let n = u32_at(&map, p);
            let off = p as u64;
            p += 4;
            let mut ats = i64::MIN;
            let (mut first, mut agent, mut session) = (0, 0, 0);
            for k in 0..n {
                let id = u64_at(&map, p);
                if k == 0 {
                    first = id;
                    agent = u32_at(&map, p + 16);
                    session = u64_at(&map, p + 8);
                }
                sessions = sessions.max(u64_at(&map, p + 8));
                let ts = i64_at(&map, p + 58);
                ats = ats.max(ts);
                min_ts = min_ts.min(ts);
                max_ts = max_ts.max(ts);
                let tl = u16::from_le_bytes(map[p + 74..p + 76].try_into().unwrap()) as usize;
                let al = u32_at(&map, p + 76) as usize;
                let xl = u32_at(&map, p + 80) as usize;
                raw += (tl + al + xl) as u64;
                p += HEAD + tl + al + xl;
                events += 1;
            }
            batches.push(BaseBatch { off, n, first_id: first, append_ts: ats, agent, session });
        }
        Ok(Self { map, batches, events, sessions, raw_bytes: raw, min_ts, max_ts })
    }

    pub fn batches(&self) -> usize {
        self.batches.len()
    }

    fn decode<'a>(&'a self, b: usize, out: &mut Vec<Event<'a>>) {
        let m = &self.map[..];
        let bb = self.batches[b];
        let mut p = bb.off as usize + 4;
        for _ in 0..bb.n {
            let tl = u16::from_le_bytes(m[p + 74..p + 76].try_into().unwrap()) as usize;
            let al = u32_at(m, p + 76) as usize;
            let xl = u32_at(m, p + 80) as usize;
            let s = p + HEAD;
            let ex = i32::from_le_bytes(m[p + 46..p + 50].try_into().unwrap());
            out.push(Event {
                id: u64_at(m, p),
                session: u64_at(m, p + 8),
                agent: u32_at(m, p + 16),
                turn: u32_at(m, p + 20),
                seq: u32_at(m, p + 24),
                parent: u64_at(m, p + 28),
                revises: u64_at(m, p + 36),
                kind: m[p + 44],
                status: m[p + 45],
                exit_code: (ex != i32::MIN).then_some(ex),
                tokens_in: u32_at(m, p + 50),
                tokens_out: u32_at(m, p + 54),
                ts_us: i64_at(m, p + 58),
                duration_us: i64_at(m, p + 66),
                tool: std::str::from_utf8(&m[s..s + tl]).expect("utf-8 tool"),
                args: std::str::from_utf8(&m[s + tl..s + tl + al]).expect("utf-8 args"),
                text: std::str::from_utf8(&m[s + tl + al..s + tl + al + xl]).expect("utf-8 text"),
            });
            p = s + tl + al + xl;
        }
    }

    /// Which base batch holds base event `id` (ids are dense and batches are
    /// consecutive id runs).
    fn batch_of(&self, id: u64) -> usize {
        self.batches.partition_point(|b| b.first_id <= id) - 1
    }
}

/// Deterministic per-replica clock offset, below one day.
pub fn replica_offset_us(r: u32) -> i64 {
    if r == 0 {
        return 0;
    }
    let h = blake3::hash(format!("replica|{r}").as_bytes());
    (u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap()) % 86_400_000_000) as i64
}

#[derive(Clone, Copy, Debug)]
pub struct PlanEntry {
    pub replica: u32,
    pub base_batch: u32,
    pub first_id: u64,
    pub append_ts: i64,
    pub agent: u32,
    pub session: u64,
    pub n: u32,
}

/// The full canonical append order of a tier.
pub struct Tier<'b> {
    pub base: &'b Base,
    pub replicas: u32,
    pub plan: Vec<PlanEntry>,
    /// first new id of (replica, base batch): index r * batches + b.
    first_new: Vec<u64>,
    pub events: u64,
}

impl<'b> Tier<'b> {
    pub fn new(base: &'b Base, replicas: u32) -> Self {
        let nb = base.batches.len();
        let mut plan = Vec::with_capacity(nb * replicas as usize);
        let mut first_new = vec![0u64; nb * replicas as usize];
        if replicas == 1 {
            // The base itself, ids unchanged.
            for (b, bb) in base.batches.iter().enumerate() {
                first_new[b] = bb.first_id;
                plan.push(PlanEntry {
                    replica: 0,
                    base_batch: b as u32,
                    first_id: bb.first_id,
                    append_ts: bb.append_ts,
                    agent: bb.agent,
                    session: bb.session,
                    n: bb.n,
                });
            }
            return Self { base, replicas, plan, first_new, events: base.events };
        }
        // k-way merge: each replica's stream keeps base order.
        let offs: Vec<i64> = (0..replicas).map(replica_offset_us).collect();
        let mut heap = std::collections::BinaryHeap::new();
        for r in 0..replicas as usize {
            let b = &base.batches[0];
            heap.push(std::cmp::Reverse((b.append_ts + offs[r], b.agent, r as u32, 0usize)));
        }
        let mut next = 1u64;
        while let Some(std::cmp::Reverse((ats, agent, r, b))) = heap.pop() {
            let bb = base.batches[b];
            first_new[r as usize * nb + b] = next;
            plan.push(PlanEntry {
                replica: r,
                base_batch: b as u32,
                first_id: next,
                append_ts: ats,
                agent,
                session: bb.session + r as u64 * base.sessions,
                n: bb.n,
            });
            next += bb.n as u64;
            if b + 1 < nb {
                let nbb = base.batches[b + 1];
                heap.push(std::cmp::Reverse((nbb.append_ts + offs[r as usize], nbb.agent, r, b + 1)));
            }
        }
        Self { base, replicas, plan, first_new, events: next - 1 }
    }

    fn remap(&self, r: u32, base_id: u64) -> u64 {
        if base_id == 0 {
            return 0;
        }
        let b = self.base.batch_of(base_id);
        let nb = self.base.batches.len();
        self.first_new[r as usize * nb + b] + (base_id - self.base.batches[b].first_id)
    }

    /// Decode plan entry `i` into `out` (cleared first).
    pub fn batch(&self, i: usize, out: &mut Vec<Event<'b>>) {
        out.clear();
        let e = self.plan[i];
        self.base.decode(e.base_batch as usize, out);
        if self.replicas == 1 {
            return;
        }
        let off = replica_offset_us(e.replica);
        let shift = e.replica as u64 * self.base.sessions;
        for (k, ev) in out.iter_mut().enumerate() {
            ev.id = e.first_id + k as u64;
            ev.parent = self.remap(e.replica, ev.parent);
            ev.revises = self.remap(e.replica, ev.revises);
            ev.session += shift;
            ev.ts_us += off;
        }
    }

    pub fn sessions(&self) -> u64 {
        self.base.sessions * self.replicas as u64
    }

    pub fn raw_bytes(&self) -> u64 {
        self.base.raw_bytes * self.replicas as u64
    }

    pub fn min_ts(&self) -> i64 {
        self.base.min_ts
    }

    pub fn max_ts(&self) -> i64 {
        self.base.max_ts + (0..self.replicas).map(replica_offset_us).max().unwrap_or(0)
    }
}
