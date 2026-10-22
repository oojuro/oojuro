//! Bounded local residue counting; no permanent erasure or completeness authority.
//! The managed caller must supply every future-live row, DATA metadata and all
//! owned current/orphan/staged files under exclusive independently current fencing.
use crate::record::{Assigned, Error, Result};
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
/// Bytes include complete serialization/decoded/file inputs, prefix tables/construction
/// and extra fallback comparisons and one unit per reported match. Counters are work accounting, not RSS.
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
pub(in crate::record) type Rows<'a> =
    dyn FnMut(&mut dyn FnMut(&Assigned) -> Result<()>) -> Result<()> + 'a;
pub(in crate::record) type Files<'a> =
    dyn FnMut(&mut dyn FnMut(&Path) -> Result<()>) -> Result<()> + 'a;

/// A successful return means only that the supplied complete callbacks finished.
/// Budget, IO, callback or validation failure poisons Work; no partial Counts escape.
pub(in crate::record) fn scan_one(
    selected: &Assigned,
    metadata: &Value,
    live_rows: &mut Rows<'_>,
    files: &mut Files<'_>,
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
    live_rows: &mut Rows<'_>,
    files: &mut Files<'_>,
    work: &mut Work,
) -> Result<Counts> {
    work.charge_rows(1)?;
    selected.input.validate()?;
    // Charge full selected source before allocating its one-row matcher.
    charge_serialized(selected, work)?;
    let patterns = crate::record::managed::row_strings(selected);
    for pattern in &patterns {
        work.charge_bytes(pattern.len() as u64)?;
    }
    validate_value(metadata)?;
    let mut counts = Counts {
        decoded: 0,
        encoded: 0,
    };
    // One exact-sized u32 KMP table at a time: <=64MiB for a legal16MiB text.
    // Every callback must reconstruct its complete immutable iterator each pass.
    for pattern in &patterns {
        let matcher = Matcher::new(pattern, work)?;
        scan_value(metadata, &matcher, work, &mut counts.decoded)?;
        live_rows(&mut |row| {
            work.charge_rows(1)?;
            row.input.validate()?;
            charge_serialized(row, work)?;
            let value = serde_json::to_value(row).map_err(|_| Error::Invalid)?;
            validate_value(&value)?;
            scan_value(&value, &matcher, work, &mut counts.decoded)
        })?;
        files(&mut |path| scan_file(path, &matcher, work, &mut counts.encoded))?;
    }
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
/// Standard KMP has linear construction/search work and one u32 per pattern byte.
/// Unlike a default automaton builder, it has no uncontrolled dense-state peak.
struct Matcher<'a> {
    pattern: &'a [u8],
    prefix: Vec<u32>,
}
impl<'a> Matcher<'a> {
    fn new(pattern: &'a [u8], work: &mut Work) -> Result<Self> {
        if pattern.is_empty() || pattern.len() > crate::record::MAX_TEXT_BYTES {
            return Err(Error::Bounds);
        }
        let bytes = pattern
            .len()
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(Error::Bounds)?;
        work.charge_bytes(bytes as u64)?;
        let mut prefix = vec![0u32; pattern.len()];
        for i in 1..pattern.len() {
            let mut j = prefix[i - 1] as usize;
            while j > 0 && pattern[i] != pattern[j] {
                work.charge_bytes(1)?;
                j = prefix[j - 1] as usize;
            }
            work.charge_bytes(1)?;
            if pattern[i] == pattern[j] {
                j += 1;
            }
            prefix[i] = j as u32;
        }
        Ok(Self { pattern, prefix })
    }
}
struct Sink<'a, 'm> {
    matcher: &'a Matcher<'m>,
    work: &'a mut Work,
    count: &'a mut u64,
    state: usize,
}
impl<'a, 'm> Sink<'a, 'm> {
    fn new(matcher: &'a Matcher<'m>, work: &'a mut Work, count: &'a mut u64) -> Self {
        Self {
            matcher,
            work,
            count,
            state: 0,
        }
    }
    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        self.work.charge_bytes(bytes.len() as u64)?;
        for &b in bytes {
            while self.state > 0 && b != self.matcher.pattern[self.state] {
                self.work.charge_bytes(1)?;
                self.state = self.matcher.prefix[self.state - 1] as usize;
            }
            if b == self.matcher.pattern[self.state] {
                self.state += 1;
            }
            if self.state == self.matcher.pattern.len() {
                self.work.charge_bytes(1)?;
                add(self.count, 1)?;
                self.state = self.matcher.prefix[self.state - 1] as usize;
            }
        }
        Ok(())
    }
}
impl Write for Sink<'_, '_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.feed(b)
            .map_err(|_| std::io::Error::other("proof scan bound"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn canonical(value: &Value, matcher: &Matcher<'_>, work: &mut Work, count: &mut u64) -> Result<()> {
    let mut sink = Sink::new(matcher, work, count);
    serde_json::to_writer(&mut sink, value).map_err(|_| {
        if sink.work.failed {
            Error::Bounds
        } else {
            Error::Invalid
        }
    })
}
fn raw(bytes: &[u8], matcher: &Matcher<'_>, work: &mut Work, count: &mut u64) -> Result<()> {
    Sink::new(matcher, work, count).feed(bytes)
}
fn scan_value(
    value: &Value,
    matcher: &Matcher<'_>,
    work: &mut Work,
    count: &mut u64,
) -> Result<()> {
    match value {
        Value::Array(items) => {
            canonical(value, matcher, work, count)?;
            for v in items {
                scan_value(v, matcher, work, count)?
            }
        }
        Value::Object(items) => {
            canonical(value, matcher, work, count)?;
            for (k, v) in items {
                raw(k.as_bytes(), matcher, work, count)?;
                scan_value(v, matcher, work, count)?
            }
        }
        Value::String(s) => raw(s.as_bytes(), matcher, work, count)?,
        _ => canonical(value, matcher, work, count)?,
    }
    Ok(())
}
fn scan_file(path: &Path, matcher: &Matcher<'_>, work: &mut Work, count: &mut u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
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
    if initial.len() != meta.len() || initial.dev() != meta.dev() || initial.ino() != meta.ino() {
        return Err(Error::Conflict);
    }
    let mut bytes = 0u64;
    let mut buf = [0u8; CHUNK];
    let mut sink = Sink::new(matcher, work, count);
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
    let final_meta = file.metadata().map_err(|_| Error::Unavailable)?;
    if bytes != initial.len()
        || final_meta.len() != initial.len()
        || final_meta.mtime() != initial.mtime()
        || final_meta.mtime_nsec() != initial.mtime_nsec()
        || final_meta.ctime() != initial.ctime()
        || final_meta.ctime_nsec() != initial.ctime_nsec()
    {
        return Err(Error::Conflict);
    }
    Ok(())
}
#[cfg(test)]
#[path = "proof/tests.rs"]
mod tests;
