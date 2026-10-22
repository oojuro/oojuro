//! Bounded local residue counting; no permanent erasure or completeness authority.
//! The managed caller must supply every future-live row, DATA metadata and all
//! owned current/orphan/staged files under exclusive independently current fencing.
use crate::record::{Assigned, Error, Result};
use aho_corasick::AhoCorasick;
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
const CHUNK: usize = 64 << 10;
const MAX_DEPTH: usize = 40;
const MAX_NODES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::record) struct Counts {
    pub decoded: u64,
    pub encoded: u64,
}
/// Conservative work budget, shared across ALL selected rows in one operation.
/// Bytes include complete serialization/decoded/file inputs, rescanned overlap
/// and one unit per reported match. Counters are work accounting, not RSS.
pub(in crate::record) struct Work {
    max_bytes: u64,
    max_rows: u64,
    bytes: u64,
    rows: u64,
    failed: bool,
}
impl Work {
    pub fn new(max_bytes: u64, max_rows: u64) -> Result<Self> {
        if max_bytes == 0 || max_rows == 0 {
            return Err(Error::Bounds);
        }
        Ok(Self {
            max_bytes,
            max_rows,
            bytes: 0,
            rows: 0,
            failed: false,
        })
    }
    pub fn charged_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn charged_rows(&self) -> u64 {
        self.rows
    }
    pub fn charge_bytes(&mut self, n: u64) -> Result<()> {
        if self.failed {
            return Err(Error::Bounds);
        }
        let next = self.bytes.checked_add(n).filter(|v| *v <= self.max_bytes);
        match next {
            Some(n) => {
                self.bytes = n;
                Ok(())
            }
            None => {
                self.failed = true;
                Err(Error::Bounds)
            }
        }
    }
    pub fn charge_rows(&mut self, n: u64) -> Result<()> {
        if self.failed {
            return Err(Error::Bounds);
        }
        match self.rows.checked_add(n).filter(|v| *v <= self.max_rows) {
            Some(n) => {
                self.rows = n;
                Ok(())
            }
            None => {
                self.failed = true;
                Err(Error::Bounds)
            }
        }
    }
}
/// A successful return means only that the supplied complete callbacks finished.
/// Budget, IO, callback or validation failure poisons Work; no partial Counts escape.
pub(in crate::record) fn scan_one(
    selected: &Assigned,
    metadata: &Value,
    live_rows: &mut dyn FnMut(&mut dyn FnMut(&Assigned) -> Result<()>) -> Result<()>,
    files: &mut dyn FnMut(&mut dyn FnMut(&Path) -> Result<()>) -> Result<()>,
    work: &mut Work,
) -> Result<Counts> {
    let out = scan_inner(selected, metadata, live_rows, files, work);
    if out.is_err() {
        work.failed = true;
    }
    out
}
fn scan_inner(
    selected: &Assigned,
    metadata: &Value,
    live_rows: &mut dyn FnMut(&mut dyn FnMut(&Assigned) -> Result<()>) -> Result<()>,
    files: &mut dyn FnMut(&mut dyn FnMut(&Path) -> Result<()>) -> Result<()>,
    work: &mut Work,
) -> Result<Counts> {
    work.charge_rows(1)?;
    selected.input.validate()?;
    // Charge full selected source before allocating its one-row matcher.
    charge_serialized(selected, work)?;
    let patterns = crate::record::managed::row_strings(selected);
    let longest = patterns.iter().map(Vec::len).max().unwrap_or(0);
    for p in &patterns {
        work.charge_bytes(p.len() as u64)?;
    }
    let matcher = if patterns.is_empty() {
        None
    } else {
        Some(AhoCorasick::new(&patterns).map_err(|_| Error::Bounds)?)
    };
    drop(patterns);
    let mut counts = Counts {
        decoded: 0,
        encoded: 0,
    };
    validate_value(metadata)?;
    scan_value(
        metadata,
        matcher.as_ref(),
        longest,
        work,
        &mut counts.decoded,
    )?;
    live_rows(&mut |row| {
        work.charge_rows(1)?;
        row.input.validate()?;
        charge_serialized(row, work)?;
        // Only one live row is materialized; no row population or ID set is kept.
        let value = serde_json::to_value(row).map_err(|_| Error::Invalid)?;
        validate_value(&value)?;
        scan_value(&value, matcher.as_ref(), longest, work, &mut counts.decoded)
    })?;
    files(&mut |path| scan_file(path, matcher.as_ref(), longest, work, &mut counts.encoded))?;
    if work.failed {
        return Err(Error::Bounds);
    }
    Ok(counts)
}
struct Charge<'a>(&'a mut Work);
impl Write for Charge<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .charge_bytes(b.len() as u64)
            .map_err(|_| std::io::Error::other("proof work bound"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn charge_serialized<T: Serialize>(v: &T, work: &mut Work) -> Result<()> {
    serde_json::to_writer(Charge(work), v).map_err(|_| {
        if work.failed {
            Error::Bounds
        } else {
            Error::Invalid
        }
    })
}
/// Validate depth/frontier before recursive serde walks of caller metadata.
fn validate_value(value: &Value) -> Result<()> {
    fn check(v: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
        *nodes = nodes.checked_add(1).ok_or(Error::Bounds)?;
        if depth > MAX_DEPTH || *nodes > MAX_NODES {
            return Err(Error::Bounds);
        }
        match v {
            Value::Array(v) => {
                if v.len() > 1024 {
                    return Err(Error::Bounds);
                }
                for x in v {
                    check(x, depth + 1, nodes)?
                }
            }
            Value::Object(v) => {
                if v.len() > 1024 {
                    return Err(Error::Bounds);
                }
                for x in v.values() {
                    check(x, depth + 1, nodes)?
                }
            }
            _ => {}
        }
        Ok(())
    }
    check(value, 0, &mut 0)
}
fn add(count: &mut u64, n: u64) -> Result<()> {
    *count = count.checked_add(n).ok_or(Error::Bounds)?;
    Ok(())
}
fn matches(
    bytes: &[u8],
    matcher: Option<&AhoCorasick>,
    work: &mut Work,
    count: &mut u64,
) -> Result<()> {
    work.charge_bytes(bytes.len() as u64)?;
    if let Some(m) = matcher {
        for _ in m.find_overlapping_iter(bytes) {
            work.charge_bytes(1)?;
            add(count, 1)?;
        }
    }
    Ok(())
}
/// One bounded window plus at most longest-pattern minus one bytes of overlap.
/// Repeated overlap is charged as work and matches ending there aren't recounted.
struct Sink<'a> {
    matcher: Option<&'a AhoCorasick>,
    longest: usize,
    work: &'a mut Work,
    count: &'a mut u64,
    window: Vec<u8>,
    carry: usize,
    new_bytes: usize,
}
impl<'a> Sink<'a> {
    fn new(
        matcher: Option<&'a AhoCorasick>,
        longest: usize,
        work: &'a mut Work,
        count: &'a mut u64,
    ) -> Self {
        Self {
            matcher,
            longest,
            work,
            count,
            window: Vec::new(),
            carry: 0,
            new_bytes: 0,
        }
    }
    fn feed(&mut self, mut bytes: &[u8]) -> Result<()> {
        self.work.charge_bytes(bytes.len() as u64)?;
        while !bytes.is_empty() {
            let n = bytes.len().min(CHUNK - self.new_bytes);
            self.window.extend(&bytes[..n]);
            self.new_bytes += n;
            bytes = &bytes[n..];
            if self.new_bytes == CHUNK {
                self.scan()?;
            }
        }
        Ok(())
    }
    fn scan(&mut self) -> Result<()> {
        if self.new_bytes == 0 {
            return Ok(());
        }
        self.work.charge_bytes(self.window.len() as u64)?;
        if let Some(m) = self.matcher {
            for hit in m.find_overlapping_iter(&self.window) {
                if hit.end() > self.carry {
                    self.work.charge_bytes(1)?;
                    add(self.count, 1)?;
                }
            }
        }
        let keep = self.longest.saturating_sub(1).min(self.window.len());
        let from = self.window.len() - keep;
        self.window.copy_within(from.., 0);
        self.window.truncate(keep);
        self.carry = keep;
        self.new_bytes = 0;
        Ok(())
    }
    fn finish(mut self) -> Result<()> {
        self.scan()
    }
}
impl Write for Sink<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.feed(b)
            .map_err(|_| std::io::Error::other("proof scan bound"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn canonical(
    value: &Value,
    matcher: Option<&AhoCorasick>,
    longest: usize,
    work: &mut Work,
    count: &mut u64,
) -> Result<()> {
    let mut sink = Sink::new(matcher, longest, work, count);
    if serde_json::to_writer(&mut sink, value).is_err() {
        return Err(if sink.work.failed {
            Error::Bounds
        } else {
            Error::Invalid
        });
    }
    sink.finish()
}
fn scan_value(
    value: &Value,
    matcher: Option<&AhoCorasick>,
    longest: usize,
    work: &mut Work,
    count: &mut u64,
) -> Result<()> {
    match value {
        Value::Array(items) => {
            canonical(value, matcher, longest, work, count)?;
            for v in items {
                scan_value(v, matcher, longest, work, count)?
            }
        }
        Value::Object(items) => {
            canonical(value, matcher, longest, work, count)?;
            for (k, v) in items {
                matches(k.as_bytes(), matcher, work, count)?;
                scan_value(v, matcher, longest, work, count)?
            }
        }
        Value::String(s) => matches(s.as_bytes(), matcher, work, count)?,
        _ => canonical(value, matcher, longest, work, count)?,
    }
    Ok(())
}
fn scan_file(
    path: &Path,
    matcher: Option<&AhoCorasick>,
    longest: usize,
    work: &mut Work,
    count: &mut u64,
) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|_| Error::Unavailable)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::Scope);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)?;
    let initial = file.metadata().map_err(|_| Error::Unavailable)?;
    if initial.len() != meta.len() {
        return Err(Error::Conflict);
    }
    let mut bytes = 0u64;
    let mut buf = [0u8; CHUNK];
    let mut sink = Sink::new(matcher, longest, work, count);
    loop {
        let n = file.read(&mut buf).map_err(|_| Error::Unavailable)?;
        if n == 0 {
            break;
        }
        bytes = bytes.checked_add(n as u64).ok_or(Error::Bounds)?;
        if bytes > initial.len() {
            return Err(Error::Conflict);
        }
        sink.feed(&buf[..n])?;
    }
    if bytes != initial.len()
        || file.metadata().map_err(|_| Error::Unavailable)?.len() != initial.len()
    {
        return Err(Error::Conflict);
    }
    sink.finish()
}
#[cfg(test)]
#[path = "proof/tests.rs"]
mod tests;
