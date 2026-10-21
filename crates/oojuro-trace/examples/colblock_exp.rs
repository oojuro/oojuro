//! How small can the typed columns get? Encodes tier-1 typed fields in
//! 4,096-event blocks (canonical id order): per-field LEB128 streams of
//! zigzag deltas, then zstd over the block; reports bytes/event per variant.
//! Usage: colblock_exp <base.tbe>
use memmap2::Mmap;
use std::fs::File;

#[derive(Clone, Copy, Default)]
struct C {
    session: u64,
    agent: u32,
    turn: u32,
    seq: u32,
    parent: u64,
    revises: u64,
    kind: u8,
    status: u8,
    exit: i32,
    tin: u32,
    tout: u32,
    ts: i64,
    dur: i64,
    tool: u16,
    text_len: u32,
    id: u64,
}

fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn main() {
    let f = File::open(std::env::args().nth(1).unwrap()).unwrap();
    let m = unsafe { Mmap::map(&f).unwrap() };
    let mut tools: std::collections::HashMap<Vec<u8>, u16> = Default::default();
    let mut cols = Vec::new();
    let mut p = 8;
    while p < m.len() {
        let n = u32::from_le_bytes(m[p..p + 4].try_into().unwrap());
        p += 4;
        for _ in 0..n {
            let g8 = |o: usize| u64::from_le_bytes(m[p + o..p + o + 8].try_into().unwrap());
            let g4 = |o: usize| u32::from_le_bytes(m[p + o..p + o + 4].try_into().unwrap());
            let tl = u16::from_le_bytes(m[p + 74..p + 76].try_into().unwrap()) as usize;
            let al = g4(76) as usize;
            let xl = g4(80) as usize;
            let tool = m[p + 84..p + 84 + tl].to_vec();
            let nt = tools.len() as u16;
            let t = *tools.entry(tool).or_insert(nt);
            cols.push(C {
                id: g8(0),
                session: g8(8),
                agent: g4(16),
                turn: g4(20),
                seq: g4(24),
                parent: g8(28),
                revises: g8(36),
                kind: m[p + 44],
                status: m[p + 45],
                exit: g4(46) as i32,
                tin: g4(50),
                tout: g4(54),
                ts: g8(58) as i64,
                dur: g8(66) as i64,
                tool: t,
                text_len: xl as u32,
            });
            p += 84 + tl + al + xl;
        }
    }
    let n = cols.len();
    for (name, level, block) in [("zstd1-4k", 1, 4096usize), ("zstd3-4k", 3, 4096), ("zstd19-4k", 19, 4096), ("zstd3-64k", 3, 65536), ("raw-varint-4k", 0, 4096)] {
        let mut total = 0usize;
        for b in cols.chunks(block) {
            let mut streams: Vec<Vec<u8>> = vec![Vec::new(); 12];
            let (mut pts, mut pses) = (b[0].ts, b[0].session as i64);
            let mut last_by_session: std::collections::HashMap<u64, i64> = Default::default();
            for c in b {
                streams[0].push(c.kind | (c.status << 3) | ((c.revises != 0) as u8) << 5);
                streams[1].push(c.agent as u8);
                put(&mut streams[2], zz(c.session as i64 - pses));
                pses = c.session as i64;
                // ts: delta vs the previous event of the same session (else block order)
                let base = last_by_session.get(&c.session).copied().unwrap_or(pts);
                put(&mut streams[3], zz(c.ts - base));
                last_by_session.insert(c.session, c.ts);
                pts = c.ts;
                put(&mut streams[4], if c.parent == 0 { 0 } else { c.id - c.parent + 1 });
                put(&mut streams[5], if c.revises == 0 { 0 } else { c.id - c.revises });
                put(&mut streams[6], c.turn as u64);
                put(&mut streams[7], c.seq as u64);
                put(&mut streams[8], if c.exit == i32::MIN { 0 } else { zz(c.exit as i64) + 1 });
                put(&mut streams[9], c.tool as u64);
                put(&mut streams[10], c.dur as u64);
                // tokens out are derived from the text length here; store both honestly
                put(&mut streams[11], c.tin as u64);
                put(&mut streams[11], c.tout as u64);
                put(&mut streams[11], c.text_len as u64);
            }
            let cat: Vec<u8> = streams.concat();
            total += if level == 0 { cat.len() } else { zstd::bulk::compress(&cat, level).unwrap().len() } + 16 + 8 * 12;
        }
        println!(
            "{{\"variant\":\"{name}\",\"events\":{n},\"bytes\":{total},\"bytes_per_event\":{:.3},\"bits_per_event\":{:.2}}}",
            total as f64 / n as f64,
            total as f64 * 8.0 / n as f64
        );
    }
}
