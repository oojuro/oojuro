//! Capture statistics: what was read, what was stored, what was redacted, and
//! how much hash dedupe could save.
use crate::{ParseCounts, Rec, kind};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// Duplicate measurement (the lead for hash dedupe). Two levels:
/// - exact records: the same text again (a file read twice, an identical
///   tool result, a prompt re-sent across turns or sessions), counted
///   globally and within the writer's own store (dedupe never crosses stores);
/// - content-defined chunks (FastCDC, ~1 KiB average) within a writer: the
///   repeated parts of records that are not identical as a whole.
#[derive(Default, Serialize)]
pub struct Dedupe {
    #[serde(skip)]
    global: HashSet<u64>,
    #[serde(skip)]
    writer: HashSet<u64>,
    #[serde(skip)]
    chunks: HashSet<u64>,
    pub records: u64,
    pub bytes: u64,
    /// (records, bytes) that repeat an earlier record anywhere.
    pub dup_global: (u64, u64),
    /// (records, bytes) that repeat an earlier record of the same writer.
    pub dup_writer: (u64, u64),
    /// Same-writer repeats of records of at least 1 KiB.
    pub dup_writer_1k: (u64, u64),
    pub dup_writer_by_kind: BTreeMap<String, (u64, u64)>,
    /// Same-writer repeats of tool calls / results by tool.
    pub dup_writer_by_tool: BTreeMap<String, (u64, u64)>,
    /// Same-writer repeats of system context by subtype.
    pub dup_writer_by_sub: BTreeMap<String, (u64, u64)>,
    pub cdc_chunks: u64,
    pub cdc_unique_chunks: u64,
    pub cdc_bytes: u64,
    pub cdc_unique_bytes: u64,
}

const CDC_MIN: u32 = 256;
const CDC_AVG: u32 = 1024;
const CDC_MAX: u32 = 8192;

fn mix(agent: u32) -> u64 {
    (agent as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn add(m: &mut BTreeMap<String, (u64, u64)>, k: &str, bytes: u64) {
    let e = m.entry(k.to_string()).or_default();
    e.0 += 1;
    e.1 += bytes;
}

impl Dedupe {
    pub fn observe(&mut self, agent: u32, rec: &Rec) {
        let t = rec.text.as_bytes();
        let n = t.len() as u64;
        self.records += 1;
        self.bytes += n;
        let h = blake3::hash(t);
        let hb = h.as_bytes();
        let g = u64::from_le_bytes(hb[..8].try_into().unwrap());
        let w = u64::from_le_bytes(hb[8..16].try_into().unwrap()) ^ mix(agent);
        if !self.global.insert(g) {
            self.dup_global.0 += 1;
            self.dup_global.1 += n;
        }
        if !self.writer.insert(w) {
            self.dup_writer.0 += 1;
            self.dup_writer.1 += n;
            if n >= 1024 {
                self.dup_writer_1k.0 += 1;
                self.dup_writer_1k.1 += n;
            }
            add(&mut self.dup_writer_by_kind, kind::NAMES[rec.kind as usize], n);
            if !rec.tool.is_empty() {
                add(&mut self.dup_writer_by_tool, &format!("{}:{}", kind::NAMES[rec.kind as usize], rec.tool), n);
            }
            if rec.kind == kind::SYSTEM {
                let sub = rec.meta.iter().find(|(k, _)| *k == "sub").map(|x| x.1.as_str()).unwrap_or("-");
                add(&mut self.dup_writer_by_sub, sub, n);
            }
        }
        // content-defined chunks within the writer
        let mut chunk = |c: &[u8]| {
            let ch = blake3::hash(c);
            let k = u64::from_le_bytes(ch.as_bytes()[..8].try_into().unwrap()) ^ mix(agent);
            self.cdc_chunks += 1;
            self.cdc_bytes += c.len() as u64;
            if self.chunks.insert(k) {
                self.cdc_unique_chunks += 1;
                self.cdc_unique_bytes += c.len() as u64;
            }
        };
        if t.len() < CDC_MIN as usize * 2 {
            chunk(t);
        } else {
            for c in fastcdc::v2020::FastCDC::new(t, CDC_MIN, CDC_AVG, CDC_MAX) {
                chunk(&t[c.offset..c.offset + c.length]);
            }
        }
    }
}

#[derive(Default, Serialize)]
pub struct Stats {
    pub files: u64,
    pub files_new: u64,
    pub files_rotated: u64,
    pub lines: u64,
    pub raw_bytes: u64,
    /// Bytes of incomplete last lines left for the next read.
    pub partial_tail_bytes: u64,
    pub commits: u64,
    /// Records found in the store beyond the saved state on open (restart).
    pub recovered: u64,
    /// Records skipped on re-read because the store already had them.
    pub skipped_existing: u64,
    pub records: u64,
    pub text_bytes: u64,
    pub kinds: BTreeMap<String, (u64, u64)>,
    pub writers: BTreeMap<String, (u64, u64)>,
    pub tools: BTreeMap<String, (u64, u64)>,
    pub senders: BTreeMap<String, u64>,
    pub system_subs: BTreeMap<String, (u64, u64)>,
    pub redactions: BTreeMap<String, u64>,
    /// Records the recording policy did not store, by reason: (count, bytes).
    pub withheld: BTreeMap<String, (u64, u64)>,
    pub parse: ParseCounts,
    pub dedupe: Dedupe,
    pub wall_secs: f64,
    pub store_bytes: u64,
    pub store_breakdown: BTreeMap<String, u64>,
}

impl Stats {
    pub fn withhold(&mut self, why: &str, bytes: usize) {
        add(&mut self.withheld, why, bytes as u64);
    }

    pub fn record(&mut self, rec: &Rec) {
        let n = rec.text.len() as u64;
        self.records += 1;
        self.text_bytes += n;
        add(&mut self.kinds, kind::NAMES[rec.kind as usize], n);
        add(&mut self.writers, &rec.writer, n);
        if rec.kind == kind::TOOL_CALL {
            add(&mut self.tools, &rec.tool, n);
        }
        for (k, v) in &rec.meta {
            match *k {
                "from" if rec.kind == kind::USER_INPUT => {
                    // agent paths vary per run; keep the sender class
                    let class = if v.starts_with("agent:") { "agent" } else { v.as_str() };
                    *self.senders.entry(class.to_string()).or_default() += 1;
                }
                "sub" if rec.kind == kind::SYSTEM => add(&mut self.system_subs, v, n),
                _ => {}
            }
        }
    }

    pub fn merge_parse(&mut self, p: ParseCounts) {
        let q = &mut self.parse;
        for (k, v) in p.entries {
            *q.entries.entry(k).or_default() += v;
        }
        for (k, v) in p.unknown {
            *q.unknown.entry(k).or_default() += v;
        }
        for (k, (n, b)) in p.noise {
            let e = q.noise.entry(k).or_default();
            e.0 += n;
            e.1 += b;
        }
        for (k, (n, b)) in p.resent {
            let e = q.resent.entry(k).or_default();
            e.0 += n;
            e.1 += b;
        }
        q.images.0 += p.images.0;
        q.images.1 += p.images.1;
        q.persisted.0 += p.persisted.0;
        q.persisted.1 += p.persisted.1;
        q.persisted.2 += p.persisted.2;
        q.bad_lines += p.bad_lines;
    }
}
