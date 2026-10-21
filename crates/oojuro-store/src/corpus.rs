//! Reader for the normalized benchmark corpus written by
//! `scripts/compress/build_corpus.py` (magic `OJCORP01`, then
//! `kind u8 | session u32 | turn u32 | len u32 | bytes`).

use std::io::{self, Read};
use std::path::Path;

pub const KIND_NAMES: [&str; 7] = [
    "system",
    "user",
    "thinking",
    "tool_call",
    "tool_result",
    "answer",
    "doc",
];

#[derive(Clone, Debug)]
pub struct CorpusRecord {
    pub kind: u8,
    pub session: u32,
    pub turn: u32,
    pub text: Vec<u8>,
}

pub fn read_corpus(path: &Path) -> io::Result<Vec<CorpusRecord>> {
    let mut data = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut data)?;
    if data.len() < 8 || &data[..8] != b"OJCORP01" {
        return Err(io::Error::other("not an OJCORP01 corpus"));
    }
    let mut out = Vec::new();
    let mut at = 8;
    while at < data.len() {
        if at + 13 > data.len() {
            return Err(io::Error::other("truncated corpus header"));
        }
        let kind = data[at];
        let session = u32::from_le_bytes(data[at + 1..at + 5].try_into().unwrap());
        let turn = u32::from_le_bytes(data[at + 5..at + 9].try_into().unwrap());
        let len = u32::from_le_bytes(data[at + 9..at + 13].try_into().unwrap()) as usize;
        at += 13;
        if at + len > data.len() {
            return Err(io::Error::other("truncated corpus record"));
        }
        out.push(CorpusRecord {
            kind,
            session,
            turn,
            text: data[at..at + len].to_vec(),
        });
        at += len;
    }
    Ok(out)
}

/// Group records into per-turn append batches: one batch per (session, turn),
/// in corpus (append) order. Returns index ranges into `records`.
pub fn turn_batches(records: &[CorpusRecord]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=records.len() {
        if i == records.len()
            || records[i].session != records[start].session
            || records[i].turn != records[start].turn
        {
            out.push(start..i);
            start = i;
        }
    }
    out
}

pub fn raw_bytes(records: &[CorpusRecord]) -> usize {
    records.iter().map(|r| r.text.len()).sum()
}
