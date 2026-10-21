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
//!
//! Provenance. A captured record's args carry its own source position
//! (`src`) and receive time (`rt`), which differ for every record, so they
//! are left out of the comparison: two records dedupe when their tool, text
//! and the rest of their args are equal. Each duplicate then keeps its own
//! `src` and `rt` as the last two fields of its entry ([`Own`]), and a read
//! puts them back into the args of the record that holds the text
//! ([`apply`]), so every event reads back byte for byte as it was appended.
//! Args without `src` or `rt` (or not in the exact form capture writes them)
//! compare whole, as before.
use std::borrow::Cow;
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
    /// The duplicate's own `src` / `rt` (None: its args equal the text row's).
    pub own: Option<Own>,
}

/// The values of the `src` and `rt` members of an args object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Own {
    /// Content of the `src` string member (digits and colons only).
    pub src: Option<String>,
    /// Value of the `rt` integer member.
    pub rt: Option<i64>,
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
/// previous entry (zigzag), a tool is written only when it changes. An entry
/// with provenance ([`Own`]) has two more fields: the `src` content (empty:
/// none; `%`: an empty string) and the `rt` as a delta against the previous
/// entry that has one (empty: none).
pub(crate) fn encode(entries: &[DupEntry]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(entries.len() * 24);
    let (mut id, mut session, mut seq, mut ts, mut row, mut rt) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
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
        if let Some(own) = &e.own {
            s.push(',');
            if let Some(src) = &own.src {
                escape(src, &mut s);
            }
            s.push(',');
            if let Some(v) = own.rt {
                let _ = write!(s, "{}", zz(v.wrapping_sub(rt)));
                rt = v;
            }
        }
        id = e.id as i64;
        session = e.session as i64;
        seq = i64::from(e.seq);
        ts = e.ts;
    }
    s
}

pub(crate) fn decode(s: &str) -> Option<Vec<DupEntry>> {
    let (mut id, mut session, mut seq, mut ts, mut row, mut rt) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let mut tool = String::new();
    let mut out = Vec::new();
    if s.is_empty() {
        return Some(out);
    }
    for (k, part) in s.split(';').enumerate() {
        let f: Vec<&str> = part.split(',').collect();
        if f.len() != 15 && f.len() != 17 {
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
        let own = if f.len() == 17 {
            let src = if f[15].is_empty() { None } else { Some(unescape(f[15])?) };
            let rt_v = if f[16].is_empty() {
                None
            } else {
                rt = rt.wrapping_add(unzz(n(16)?));
                Some(rt)
            };
            Some(Own { src, rt: rt_v })
        } else {
            None
        };
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
            own,
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

/// Byte extents of the top-level `src` and `rt` values of a compact JSON object.
struct Members {
    src: Option<(usize, usize)>,
    rt: Option<(usize, usize)>,
}

/// Find the `src` and `rt` members of `args` (a JSON object); None when it
/// is not a well-formed object or names either member twice.
fn members(args: &str) -> Option<Members> {
    let b = args.as_bytes();
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    // end of the string starting at b[i] == '"'
    let string = |i: usize| -> Option<usize> {
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'"' => return Some(j + 1),
                _ => j += 1,
            }
        }
        None
    };
    let mut m = Members { src: None, rt: None };
    let mut i = 0;
    ws(&mut i);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    ws(&mut i);
    if b.get(i) == Some(&b'}') {
        i += 1;
    } else {
        loop {
            ws(&mut i);
            if b.get(i) != Some(&b'"') {
                return None;
            }
            let ke = string(i)?;
            let key = &b[i + 1..ke - 1];
            i = ke;
            ws(&mut i);
            if b.get(i) != Some(&b':') {
                return None;
            }
            i += 1;
            ws(&mut i);
            let vs = i;
            match b.get(i)? {
                b'"' => i = string(i)?,
                b'{' | b'[' => {
                    let mut depth = 0usize;
                    while i < b.len() {
                        match b[i] {
                            b'"' => {
                                i = string(i)?;
                                continue;
                            }
                            b'{' | b'[' => depth += 1,
                            b'}' | b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    if depth != 0 {
                        return None;
                    }
                }
                _ => {
                    while i < b.len() && !matches!(b[i], b',' | b'}' | b' ' | b'\t' | b'\n' | b'\r') {
                        i += 1;
                    }
                }
            }
            if i == vs {
                return None;
            }
            let slot = match key {
                b"src" => Some(&mut m.src),
                b"rt" => Some(&mut m.rt),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return None;
                }
                *slot = Some((vs, i));
            }
            ws(&mut i);
            match b.get(i)? {
                b',' => i += 1,
                b'}' => {
                    i += 1;
                    break;
                }
                _ => return None,
            }
        }
    }
    ws(&mut i);
    (i == b.len()).then_some(m)
}

/// `src` as capture writes it: a string of digits and colons, no escapes.
fn src_token(args: &str, (a, b): (usize, usize)) -> Option<&str> {
    let t = args[a..b].strip_prefix('"')?.strip_suffix('"')?;
    t.bytes().all(|c| c.is_ascii_digit() || c == b':').then_some(t)
}

/// `rt` as capture writes it: a plain integer.
fn rt_token(args: &str, (a, b): (usize, usize)) -> Option<i64> {
    let t = &args[a..b];
    let v: i64 = t.parse().ok()?;
    (v.to_string() == t).then_some(v)
}

/// The args with the values of `src` and `rt` replaced by `0`, and those
/// values; None when `args` has neither member or they are not in the exact
/// form capture writes (then the args compare whole).
pub(crate) fn split(args: &str) -> Option<(String, Own)> {
    let m = members(args)?;
    if m.src.is_none() && m.rt.is_none() {
        return None;
    }
    let src = m.src.map(|r| src_token(args, r).map(str::to_string)).map_or(Some(None), |x| x.map(Some))?;
    let rt = m.rt.map(|r| rt_token(args, r)).map_or(Some(None), |x| x.map(Some))?;
    let mut spans: Vec<(usize, usize)> = m.src.into_iter().chain(m.rt).collect();
    spans.sort_unstable();
    let mut norm = String::with_capacity(args.len());
    let mut at = 0;
    for (a, b) in spans {
        norm.push_str(&args[at..a]);
        norm.push('0');
        at = b;
    }
    norm.push_str(&args[at..]);
    Some((norm, Own { src, rt }))
}

/// `args` (the text row's, or a holder's) with its `src` and `rt` values
/// replaced by `own`'s. None when the structure differs (a member present in
/// one and not in the other).
pub(crate) fn apply(args: &str, own: &Own) -> Option<String> {
    let m = members(args)?;
    if m.src.is_some() != own.src.is_some() || m.rt.is_some() != own.rt.is_some() {
        return None;
    }
    let mut parts: Vec<((usize, usize), String)> = Vec::new();
    if let (Some(r), Some(v)) = (m.src, &own.src) {
        parts.push((r, format!("\"{v}\"")));
    }
    if let (Some(r), Some(v)) = (m.rt, own.rt) {
        parts.push((r, v.to_string()));
    }
    parts.sort_unstable_by_key(|p| p.0);
    let mut out = String::with_capacity(args.len() + 16);
    let mut at = 0;
    for ((a, b), v) in parts {
        out.push_str(&args[at..a]);
        out.push_str(&v);
        at = b;
    }
    out.push_str(&args[at..]);
    Some(out)
}

/// The args a holder keeps: the deleted event's own `src` and `rt` are gone.
pub(crate) fn anonymous(args: &str) -> String {
    split(args).map_or_else(|| args.to_string(), |(norm, _)| norm)
}

/// Dedupe key over (indexed, tool, args without its `src` / `rt`, text), and
/// the duplicate's own `src` / `rt`.
pub(crate) fn key_own(indexed: bool, tool: &str, args: &str, text: &str) -> (Key, Option<Own>) {
    let (norm, own) = match split(args) {
        Some((n, o)) => (Cow::Owned(n), Some(o)),
        None => (Cow::Borrowed(args), None),
    };
    let mut h = blake3::Hasher::new();
    h.update(&[indexed as u8]);
    h.update(&(tool.len() as u64).to_le_bytes());
    h.update(tool.as_bytes());
    h.update(&(norm.len() as u64).to_le_bytes());
    h.update(norm.as_bytes());
    h.update(text.as_bytes());
    (h.finalize().as_bytes()[..16].try_into().unwrap(), own)
}

/// Dedupe key over (indexed, tool, args, text); see [`key_own`].
pub(crate) fn key(indexed: bool, tool: &str, args: &str, text: &str) -> Key {
    key_own(indexed, tool, args, text).0
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
                own: match i % 4 {
                    0 => None,
                    1 => Some(Own { src: Some(format!("{}:{}:{}", i % 3, i * 977, i % 2)), rt: Some(1_800_000_000_000_000 + (i as i64 % 5) * 3) }),
                    2 => Some(Own { src: None, rt: Some(-5 + i as i64) }),
                    _ => Some(Own { src: Some(String::new()), rt: None }),
                },
            });
        }
        let s = encode(&v);
        assert!(s.is_ascii());
        assert_eq!(decode(&s).unwrap(), v);
        assert_eq!(decode("").unwrap(), Vec::new());
        assert!(decode("1,2,3").is_none());
    }

    #[test]
    fn provenance_splits_and_applies_exactly() {
        let a = r#"{"src":"3:120:0","rt":1792612725000000,"from":"human","model":"m"}"#;
        let b = r#"{"src":"9:7:2","rt":1792612799123456,"from":"human","model":"m"}"#;
        let (na, oa) = split(a).unwrap();
        let (nb, ob) = split(b).unwrap();
        assert_eq!(na, nb);
        assert_eq!(na, r#"{"src":0,"rt":0,"from":"human","model":"m"}"#);
        assert_eq!(oa, Own { src: Some("3:120:0".into()), rt: Some(1792612725000000) });
        // each copy's args come back from the other's text row
        assert_eq!(apply(a, &ob).unwrap(), b);
        assert_eq!(apply(b, &oa).unwrap(), a);
        // a holder keeps neither
        assert_eq!(anonymous(a), na);
        assert_eq!(apply(&anonymous(a), &ob).unwrap(), b);
        assert_eq!(key(true, "t", a, "x"), key(true, "t", b, "x"));
        assert_ne!(key(true, "t", a, "x"), key(true, "t", b, "y"));
        assert_ne!(key(true, "t", a, "x"), key(true, "u", a, "x"));
        // only rt (a hook record), only src, and whitespace
        let (n, o) = split(r#"{ "via" : "hook", "rt" : 5 }"#).unwrap();
        assert_eq!((n.as_str(), o.rt, o.src.is_some()), (r#"{ "via" : "hook", "rt" : 0 }"#, Some(5), false));
        assert_eq!(apply(r#"{ "via" : "hook", "rt" : 5 }"#, &Own { src: None, rt: Some(-7) }).unwrap(), r#"{ "via" : "hook", "rt" : -7 }"#);
        // structure must match
        assert!(apply(a, &Own { src: Some("1:1:1".into()), rt: None }).is_none());
        assert!(apply(r#"{"rt":1}"#, &ob).is_none());
        // no provenance, or not in capture form: compared whole
        for no in [
            r#"{"cmd":"ls"}"#,
            "",
            "[1]",
            "null",
            r#"{"a":{"src":"1:1:1","rt":4}}"#,
            r#"{"rt":1.0}"#,
            r#"{"rt":"5"}"#,
            r#"{"rt":+5}"#,
            r#"{"src":"x"}"#,
            r#"{"src":"1:2:0","src":"1:3:0"}"#,
            r#"{"src":"1:2:0"} trailing"#,
            r#"{"src":"1:2:0""#,
            r#"{"rt":99999999999999999999}"#,
        ] {
            assert!(split(no).is_none(), "{no}");
            assert_eq!(key(false, "", no, "x"), key_own(false, "", no, "x").0);
            assert_eq!(anonymous(no), no);
        }
        // a string with escapes and brackets elsewhere is skipped correctly
        let tricky = r#"{"a":"}{\"src\":1","b":[1,{"rt":2}],"src":"4:5:6","rt":7}"#;
        let (n, o) = split(tricky).unwrap();
        assert_eq!(o, Own { src: Some("4:5:6".into()), rt: Some(7) });
        assert_eq!(apply(tricky, &o).unwrap(), tricky);
        assert!(n.ends_with(r#""src":0,"rt":0}"#), "{n}");
    }
}
