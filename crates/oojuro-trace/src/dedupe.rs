//! Record-level dedupe ("store each message once").
//!
//! An event whose (indexing, tool, args, text) equals an earlier event of the
//! same agent store gets no engine record of its own: its typed fields ride
//! as one entry of the batch's `B` metadata (on the batch's first stored
//! record, or on a text-less bundle record when every event of the batch is
//! a duplicate) and its column slot points at the engine row that holds the
//! text (`src`). It is not indexed again: search finds the text through the
//! first occurrence and reports the earliest live occurrence.
//!
//! Reference counting: the text row stays while its own event or any
//! duplicate pointing at it is live. When its own event is deleted but
//! duplicates live, reclaim turns the record into a holder (text, tool and
//! args kept; the deleted event's fields removed); when the last reference
//! dies, reclaim blanks the row and the engine rewrites the segment, so the
//! bytes leave every file.
use std::collections::HashMap;

/// Where a duplicate's text lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Src {
    /// An engine row (absolute).
    Row(u32),
    /// The `j`-th record stored by the same batch, i.e. host row + `j`.
    Local(u32),
}

/// One duplicate event as carried in a `B` entry list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DupEntry {
    pub id: u64,
    pub src: Src,
    pub session: u64,
    pub turn: u32,
    pub seq: u32,
    pub parent: u64,
    pub kind: u8,
    pub status: u8,
    pub exit: i32,
    pub tin: u32,
    pub tout: u32,
    pub ts: i64,
    pub duration: i64,
    pub text_len: u32,
    pub tool: String,
}

fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn unzz(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn escape(t: &str, out: &mut String) {
    if t.is_empty() {
        out.push('%');
        return;
    }
    for ch in t.chars() {
        match ch {
            '%' => out.push_str("%25"),
            ',' => out.push_str("%2C"),
            ';' => out.push_str("%3B"),
            c => out.push(c),
        }
    }
}

fn unescape(t: &str) -> Option<String> {
    if t == "%" {
        return Some(String::new());
    }
    let mut out = String::with_capacity(t.len());
    let mut it = t.split('%');
    out.push_str(it.next()?);
    for part in it {
        let (code, rest) = (part.get(..2)?, &part[2..]);
        out.push(match code {
            "25" => '%',
            "2C" => ',',
            "3B" => ';',
            _ => return None,
        });
        out.push_str(rest);
    }
    Some(out)
}

/// ASCII entry list: entries joined by `;`, fields by `,`; ids, sessions,
/// sequence numbers, clocks and absolute source rows are deltas against the
/// previous entry (zigzag), a tool is written only when it changes.
pub(crate) fn encode(entries: &[DupEntry]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(entries.len() * 24);
    let (mut id, mut session, mut seq, mut ts, mut row) = (0i64, 0i64, 0i64, 0i64, 0i64);
    let mut tool: Option<&str> = None;
    for (k, e) in entries.iter().enumerate() {
        if k > 0 {
            s.push(';');
        }
        let src = match e.src {
            Src::Row(r) => {
                let d = zz(i64::from(r) - row);
                row = i64::from(r);
                format!("r{d}")
            }
            Src::Local(j) => format!("l{j}"),
        };
        let parent = if e.parent == 0 { String::new() } else { zz(e.id as i64 - e.parent as i64).to_string() };
        let exit = if e.exit == crate::columns::NO_EXIT { String::new() } else { e.exit.to_string() };
        let _ = write!(
            s,
            "{},{src},{},{},{},{parent},{},{},{exit},{},{},{},{},{},",
            zz(e.id as i64 - id),
            zz(e.session as i64 - session),
            e.turn,
            zz(i64::from(e.seq) - seq),
            e.kind,
            e.status,
            e.tin,
            e.tout,
            zz(e.ts - ts),
            zz(e.duration),
            e.text_len,
        );
        if tool != Some(e.tool.as_str()) {
            escape(&e.tool, &mut s);
            tool = Some(&e.tool);
        }
        id = e.id as i64;
        session = e.session as i64;
        seq = i64::from(e.seq);
        ts = e.ts;
    }
    s
}

pub(crate) fn decode(s: &str) -> Option<Vec<DupEntry>> {
    let (mut id, mut session, mut seq, mut ts, mut row) = (0i64, 0i64, 0i64, 0i64, 0i64);
    let mut tool = String::new();
    let mut out = Vec::new();
    if s.is_empty() {
        return Some(out);
    }
    for (k, part) in s.split(';').enumerate() {
        let f: Vec<&str> = part.split(',').collect();
        if f.len() != 15 {
            return None;
        }
        let n = |i: usize| f[i].parse::<u64>().ok();
        id += unzz(n(0)?);
        let src = match f[1].as_bytes().first()? {
            b'r' => {
                row += unzz(f[1][1..].parse().ok()?);
                Src::Row(u32::try_from(row).ok()?)
            }
            b'l' => Src::Local(f[1][1..].parse().ok()?),
            _ => return None,
        };
        session += unzz(n(2)?);
        seq += unzz(n(4)?);
        ts += unzz(n(11)?);
        if !f[14].is_empty() {
            tool = unescape(f[14])?;
        } else if k == 0 {
            return None;
        }
        out.push(DupEntry {
            id: id as u64,
            src,
            session: session as u64,
            turn: n(3)? as u32,
            seq: seq as u32,
            parent: if f[5].is_empty() { 0 } else { (id - unzz(n(5)?)) as u64 },
            kind: n(6)? as u8,
            status: n(7)? as u8,
            exit: if f[8].is_empty() { crate::columns::NO_EXIT } else { f[8].parse().ok()? },
            tin: n(9)? as u32,
            tout: n(10)? as u32,
            ts,
            duration: unzz(n(12)?),
            text_len: n(13)? as u32,
            tool: tool.clone(),
        });
    }
    Some(out)
}

/// Dedupe key: 128 bits of BLAKE3 (byte array: 20-byte map entries).
pub(crate) type Key = [u8; 16];

/// Keys are already uniform hashes: a map hashes them by their first 8 bytes.
#[derive(Default)]
pub(crate) struct PassHasher(u64);

impl std::hash::Hasher for PassHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for (i, x) in b.iter().take(8).enumerate() {
            self.0 ^= u64::from(*x) << (8 * i);
        }
    }
    fn write_usize(&mut self, _: usize) {}
}

pub(crate) type KeyMap<V> = HashMap<Key, V, std::hash::BuildHasherDefault<PassHasher>>;

/// Dedupe key over (indexed, tool, args, text).
pub(crate) fn key(indexed: bool, tool: &str, args: &str, text: &str) -> Key {
    let mut h = blake3::Hasher::new();
    h.update(&[indexed as u8]);
    h.update(&(tool.len() as u64).to_le_bytes());
    h.update(tool.as_bytes());
    h.update(&(args.len() as u64).to_le_bytes());
    h.update(args.as_bytes());
    h.update(text.as_bytes());
    h.finalize().as_bytes()[..16].try_into().unwrap()
}

/// key -> engine row holding that text, bounded: two generations (lookups
/// see both, a hit in the old one is promoted; when the current one holds
/// half the window it becomes the old one), a cheap LRU approximation.
#[derive(Default)]
pub(crate) struct Window {
    cur: KeyMap<u32>,
    old: KeyMap<u32>,
}

impl Window {
    pub fn get(&mut self, k: Key, cap: usize) -> Option<u32> {
        if let Some(r) = self.cur.get(&k) {
            return Some(*r);
        }
        let r = self.old.remove(&k)?;
        self.insert(k, r, cap);
        Some(r)
    }
    pub fn insert(&mut self, k: Key, row: u32, cap: usize) {
        self.cur.insert(k, row);
        if self.cur.len() >= (cap / 2).max(1) {
            self.old = std::mem::take(&mut self.cur);
        }
    }
    /// Drop every key whose row lost its text.
    pub fn forget(&mut self, rows: &std::collections::HashSet<u32>) {
        self.cur.retain(|_, r| !rows.contains(r));
        self.old.retain(|_, r| !rows.contains(r));
    }
    pub fn len(&self) -> usize {
        self.cur.len() + self.old.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_roundtrip() {
        let mut v = Vec::new();
        for i in 0..50u64 {
            v.push(DupEntry {
                id: 1000 + i * 3,
                src: if i % 4 == 0 { Src::Local(i as u32 % 3) } else { Src::Row((i * 7 % 40) as u32) },
                session: 5 + i / 10,
                turn: (i % 9) as u32,
                seq: (i * 2) as u32,
                parent: if i % 3 == 0 { 0 } else { 900 + i },
                kind: (i % 6) as u8,
                status: (i % 4) as u8,
                exit: if i % 5 == 0 { crate::columns::NO_EXIT } else { i as i32 - 20 },
                tin: i as u32 * 11,
                tout: i as u32,
                ts: 1_788_220_800_000_000 + (i as i64 * 7919) % 1000 - 500,
                duration: -(i as i64) * 3,
                text_len: (i * 1013) as u32,
                tool: ["", "bash", "a,b;c%d", "bash", "str_replace_editor"][(i % 5) as usize].to_string(),
            });
        }
        let s = encode(&v);
        assert!(s.is_ascii());
        assert_eq!(decode(&s).unwrap(), v);
        assert_eq!(decode("").unwrap(), Vec::new());
        assert!(decode("1,2,3").is_none());
    }
}
