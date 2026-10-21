//! Tiered segment merges. Flushes create level-0 segments; whenever
//! `fanout` adjacent segments share a level they are merged into one segment
//! of the next level. Merges always cover a contiguous row range, so row
//! numbers, ids, scores and every search result are unchanged by them.
//!
//! A merge streams its inputs (k-way merges of the fingerprint tables, time
//! orders and dictionaries), verifies every merged original byte for byte,
//! and only then swaps the manifest under the writer lock. Commits continue
//! while a merge runs; input files are removed after the swap (on platforms
//! that refuse to remove mapped files, the next open removes them).
use super::*;
use crate::segment::TermCursor;
use std::{cmp::Reverse, collections::{BTreeMap, BinaryHeap}, sync::atomic::AtomicBool};

/// Everything a merge needs, shared with the background merge thread.
#[derive(Clone)]
pub(super) struct MergeCtx {
    pub directory: PathBuf,
    pub options: Options,
    pub state: Arc<RwLock<State>>,
    pub writer: Arc<Mutex<Writer>>,
    pub cache: Arc<Mutex<Cache>>,
    pub stop: Arc<AtomicBool>,
    /// One merge at a time (background thread or `compact`).
    pub merging: Arc<Mutex<()>>,
}

/// Merge `iters` (each sorted ascending) into one ascending sequence.
fn kmerge<'a, T: Ord + Copy + 'a>(
    mut iters: Vec<Box<dyn Iterator<Item = T> + 'a>>,
) -> impl Iterator<Item = T> + 'a {
    let mut heap = BinaryHeap::new();
    for (i, it) in iters.iter_mut().enumerate() {
        if let Some(x) = it.next() {
            heap.push(Reverse((x, i)));
        }
    }
    std::iter::from_fn(move || {
        let Reverse((x, i)) = heap.pop()?;
        if let Some(y) = iters[i].next() {
            heap.push(Reverse((y, i)));
        }
        Some(x)
    })
}

/// The next merge: within the newest maximal run of equal-level segments
/// that has at least `fanout` members, its oldest `fanout` members. Levels
/// then stay non-increasing from old to new (a base-`fanout` counter), so a
/// store of n flushes keeps O(fanout * log n) segments. None when no merge
/// is due.
pub(super) fn plan(manifest: &Manifest, fanout: usize) -> Option<std::ops::Range<usize>> {
    let segs = &manifest.segments;
    if fanout < 2 {
        return None;
    }
    let mut end = segs.len();
    while end > 0 {
        let level = segs[end - 1].level;
        let mut start = end - 1;
        while start > 0 && segs[start - 1].level == level {
            start -= 1;
        }
        if end - start >= fanout {
            return Some(start..start + fanout);
        }
        end = start;
    }
    None
}

/// Write the merge of `inputs` as segment `id`; returns its manifest entry.
pub(super) fn write_merged(
    ctx: &MergeCtx,
    inputs: &[Arc<DiskSegment>],
    id: u64,
    level: u32,
) -> Result<SegmentEntry> {
    let start = inputs[0].start;
    for pair in inputs.windows(2) {
        if pair[0].end() != pair[1].start {
            return err("merge inputs must be contiguous");
        }
    }
    let rows: u64 = inputs.iter().map(|s| u64::from(s.rows)).sum();
    let rows = u32::try_from(rows)?;
    let (idx, src) = seg_names(id);
    let src_path = ctx.directory.join(&src);
    // Originals: stream every input row through the archive codec.
    let failure = std::cell::RefCell::new(None::<String>);
    {
        let mut originals = inputs.iter().flat_map(|s| {
            let failure = &failure;
            (0..u64::from(s.rows)).map_while(move |i| {
                if ctx.stop.load(Ordering::Relaxed) {
                    *failure.borrow_mut() = Some("merge stopped".into());
                    return None;
                }
                match s.archive.get(i) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        *failure.borrow_mut() = Some(e.to_string());
                        None
                    }
                }
            })
        });
        let info = ctx.options.archive.write(&src_path, &mut originals)?;
        if let Some(e) = failure.borrow_mut().take() {
            return Err(e.into());
        }
        if info.rows != u64::from(rows) {
            return err("merged archive has a different row count");
        }
    }
    // Verify every merged original against its input before use.
    let run = ctx.options.archive.open(&src_path)?;
    let mut offset = 0u64;
    for s in inputs {
        let mut mismatch = false;
        run.scan(offset, offset + u64::from(s.rows), &mut |i, bytes| {
            if ctx.stop.load(Ordering::Relaxed) {
                return err("merge stopped");
            }
            mismatch |= s.archive.get(i - offset)? != bytes;
            Ok(())
        })?;
        if mismatch {
            return err("merged archive differs from its inputs");
        }
        offset += u64::from(s.rows);
    }
    drop(run);
    // Index: rows, fingerprints, time order, multi keys, dictionary.
    let mut w = SegmentWriter::create(&ctx.directory.join(&idx), start)?;
    w.rows(
        inputs
            .iter()
            .flat_map(|s| (0..s.rows).map(move |l| s.meta(l))),
    )?;
    let rebased = |keys: bool| {
        kmerge(
            inputs
                .iter()
                .map(|s| {
                    let shift = s.start - start;
                    Box::new(s.fp_entries(keys).map(move |(fp, l)| (fp, l + shift)))
                        as Box<dyn Iterator<Item = (u128, u32)>>
                })
                .collect(),
        )
    };
    w.ids(rebased(false))?;
    w.keys(rebased(true))?;
    w.time(
        kmerge(
            inputs
                .iter()
                .map(|s| {
                    let shift = s.start - start;
                    Box::new((0..s.rows as usize).map(move |i| {
                        let l = s.time_at(i);
                        (s.meta(l).valid_at, l + shift)
                    })) as Box<dyn Iterator<Item = (i64, u32)>>
                })
                .collect(),
        )
        .map(|(_, l)| l),
    )?;
    let mut multi: Vec<u128> = inputs.iter().flat_map(|s| s.multi_keys()).collect();
    multi.sort_unstable();
    multi.dedup();
    w.multi(multi)?;
    let mut cursors: Vec<TermCursor<'_>> = inputs.iter().map(|s| s.terms_cursor()).collect();
    let mut current: Vec<Option<(Vec<u8>, crate::segment::TermInfo)>> = Vec::new();
    for c in &mut cursors {
        current.push(c.next_term()?.map(|(t, i)| (t.to_vec(), i)));
    }
    let mut merged = 0u64;
    loop {
        let Some(smallest) = current.iter().flatten().map(|(t, _)| t).min().cloned() else {
            break;
        };
        merged += 1;
        if merged % 4096 == 0 && ctx.stop.load(Ordering::Relaxed) {
            return err("merge stopped");
        }
        let mut lists = Vec::new();
        for (k, entry) in current.iter().enumerate() {
            if let Some((t, info)) = entry {
                if *t == smallest {
                    let shift = inputs[k].start - start;
                    lists.push(
                        inputs[k]
                            .postings(*info)
                            .map(move |(l, tf)| (l + shift, tf)),
                    );
                }
            }
        }
        w.term(&smallest, lists.into_iter().flatten())?;
        for (k, entry) in current.iter_mut().enumerate() {
            if entry.as_ref().is_some_and(|(t, _)| *t == smallest) {
                *entry = cursors[k].next_term()?.map(|(t, i)| (t.to_vec(), i));
            }
        }
    }
    w.finish()?;
    sync_dir(&ctx.directory)?;
    Ok(SegmentEntry {
        id,
        start,
        rows,
        level,
    })
}

impl MergeCtx {
    /// Run one planned merge if any is due. Returns whether one ran.
    pub fn merge_once(&self) -> Result<bool> {
        self.merge_planned(false)
    }

    /// Merge every segment into one (a major compaction). Returns whether
    /// there was more than one segment.
    pub fn merge_all(&self) -> Result<bool> {
        self.merge_planned(true)
    }

    fn merge_planned(&self, all: bool) -> Result<bool> {
        let _one = lock(&self.merging)?;
        let fanout = self.options.merge_fanout;
        let (inputs, id, level) = {
            let mut w = lock(&self.writer)?;
            if w.poisoned {
                return err("journal I/O previously failed; close and reopen for recovery");
            }
            let planned = if all {
                (!w.manifest.segments.is_empty()).then(|| 0..w.manifest.segments.len())
            } else {
                plan(&w.manifest, fanout)
            };
            let Some(range) = planned else {
                return Ok(false);
            };
            let ids: Vec<u64> = w.manifest.segments[range.clone()]
                .iter()
                .map(|e| e.id)
                .collect();
            let level = if all {
                w.manifest.segments[range.clone()].iter().map(|e| e.level).max().unwrap_or(0) + 1
            } else {
                w.manifest.segments[range.start].level + 1
            };
            // Reserve the id so a concurrent flush cannot reuse it; an
            // abandoned id is simply never used.
            let id = w.manifest.next_segment;
            w.manifest.next_segment += 1;
            let st = self.state.read().map_err(|_| "engine lock poisoned")?;
            let inputs: Vec<Arc<DiskSegment>> = ids
                .iter()
                .map(|i| {
                    st.segments
                        .iter()
                        .find(|s| s.id == *i)
                        .cloned()
                        .ok_or("planned merge input is missing")
                })
                .collect::<std::result::Result<_, _>>()?;
            (inputs, id, level)
        };
        let entry = match write_merged(self, &inputs, id, level) {
            Ok(e) => e,
            Err(e) => {
                let (idx, src) = seg_names(id);
                let _ = fs::remove_file(self.directory.join(idx));
                let _ = fs::remove_file(self.directory.join(src));
                return Err(e);
            }
        };
        let (idx, src) = seg_names(id);
        let run = self.options.archive.open(&self.directory.join(&src))?;
        let segment = Arc::new(DiskSegment::open(id, &self.directory.join(&idx), run)?);
        {
            let mut w = lock(&self.writer)?;
            let pos = w
                .manifest
                .segments
                .iter()
                .position(|e| e.id == inputs[0].id)
                .ok_or("merge inputs left the manifest")?;
            let n = inputs.len();
            if w.manifest.segments.len() < pos + n
                || w.manifest.segments[pos..pos + n]
                    .iter()
                    .zip(&inputs)
                    .any(|(e, s)| e.id != s.id)
            {
                return err("merge inputs changed while merging");
            }
            let mut manifest = w.manifest.clone();
            manifest.segments.splice(pos..pos + n, [entry]);
            write_manifest(&self.directory, &manifest)?;
            {
                let mut st = self.state.write().map_err(|_| "engine lock poisoned")?;
                let at = st
                    .segments
                    .iter()
                    .position(|s| s.id == inputs[0].id)
                    .ok_or("merge inputs left the index")?;
                st.segments.splice(at..at + n, [segment]);
                st.epoch += 1;
            }
            if let Ok(mut c) = self.cache.lock() {
                c.entries.clear();
                c.bytes = 0;
            }
            w.manifest = manifest;
        }
        for s in &inputs {
            let (idx, src) = seg_names(s.id);
            // Mapped files may not be removable on every platform; the next
            // open removes any segment file the manifest does not list.
            let _ = fs::remove_file(self.directory.join(idx));
            let _ = fs::remove_file(self.directory.join(src));
        }
        sync_dir(&self.directory)?;
        Ok(true)
    }

    /// Merge until no merge is due.
    pub fn compact(&self) -> Result<usize> {
        let mut n = 0;
        while self.merge_once()? {
            n += 1;
        }
        Ok(n)
    }
}

/// Background merge loop: waits for a signal after each flush.
pub(super) fn run(ctx: MergeCtx, signal: Arc<(Mutex<bool>, Condvar)>) {
    let (pending, wake) = &*signal;
    loop {
        {
            let Ok(mut due) = pending.lock() else { return };
            while !*due && !ctx.stop.load(Ordering::Relaxed) {
                due = match wake.wait(due) {
                    Ok(d) => d,
                    Err(_) => return,
                };
            }
            if ctx.stop.load(Ordering::Relaxed) {
                return;
            }
            *due = false;
        }
        // A failed merge leaves the store unchanged and is retried after
        // the next flush.
        let _ = ctx.compact();
    }
}

/// Rewrite segment `s` as segment `id` over the same rows, with every row in
/// `dead` (local indexes) replaced by a blank stored-only record (same id,
/// key and clocks; no text, no metadata) and dropped from the index: its
/// postings go, and so does every term only it used. The archive run is
/// rebuilt (with its dictionary) from the surviving originals, so erased
/// text is in no file of the new segment. Row numbers do not change.
pub(super) fn write_erased(
    ctx: &MergeCtx,
    s: &DiskSegment,
    dead: &HashSet<u32>,
    meta: &HashMap<u32, BTreeMap<String, String>>,
    id: u64,
) -> Result<SegmentEntry> {
    let (idx, src) = seg_names(id);
    let src_path = ctx.directory.join(&src);
    // A dead row becomes blank (keeping only the metadata given for it); a
    // live row with given metadata keeps its text, index entry and flags.
    let blank = |l: u32| -> Result<Vec<u8>> {
        let (old, indexed) = decode_record_with(&s.archive.get(u64::from(l))?, u64::from(s.start + l))?;
        let gone = dead.contains(&l);
        let text = if gone { "" } else { old.text.as_str() };
        let mut r = Record::new(&old.id, &old.key, text, old.valid_at);
        r.recorded_at = old.recorded_at;
        r.verified = old.verified;
        if let Some(m) = meta.get(&l) {
            r.metadata = m.clone();
        }
        let mut out = Vec::new();
        encode_record_with(&r, u64::from(s.start + l), indexed && !gone, &mut out);
        Ok(out)
    };
    let edited = |l: u32| dead.contains(&l) || meta.contains_key(&l);
    let failure = std::cell::RefCell::new(None::<String>);
    {
        let mut originals = (0..s.rows).map_while(|l| {
            let got = if edited(l) { blank(l) } else { s.archive.get(u64::from(l)) };
            match got {
                Ok(b) => Some(b),
                Err(e) => {
                    *failure.borrow_mut() = Some(e.to_string());
                    None
                }
            }
        });
        let info = ctx.options.archive.write(&src_path, &mut originals)?;
        if let Some(e) = failure.borrow_mut().take() {
            return Err(e.into());
        }
        if info.rows != u64::from(s.rows) {
            return err("erased archive has a different row count");
        }
    }
    // Verify every original against its input (blanks against a fresh blank).
    let run = ctx.options.archive.open(&src_path)?;
    let mut mismatch = false;
    run.scan(0, u64::from(s.rows), &mut |i, bytes| {
        let l = i as u32;
        let want = if edited(l) { blank(l)? } else { s.archive.get(i)? };
        mismatch |= want != bytes;
        Ok(())
    })?;
    if mismatch {
        return err("erased archive differs from its input");
    }
    drop(run);
    let mut w = SegmentWriter::create(&ctx.directory.join(&idx), s.start)?;
    w.rows((0..s.rows).map(|l| {
        let mut m = s.meta(l);
        if dead.contains(&l) {
            m.length = 0;
            m.scope = 0;
            m.text_len = 0;
            m.flags |= ROW_UNINDEXED;
        }
        m
    }))?;
    w.ids(s.fp_entries(false))?;
    w.keys(s.fp_entries(true))?;
    w.time((0..s.rows as usize).map(|i| s.time_at(i)))?;
    w.multi(s.multi_keys())?;
    let mut cursor = s.terms_cursor();
    while let Some((term, info)) = cursor.next_term()? {
        let kept: Vec<(u32, u32)> = s.postings(info).filter(|(l, _)| !dead.contains(l)).collect();
        if !kept.is_empty() {
            w.term(term, kept.into_iter())?;
        }
    }
    w.finish()?;
    sync_dir(&ctx.directory)?;
    Ok(SegmentEntry { id, start: s.start, rows: s.rows, level: 0 })
}

impl MergeCtx {
    /// Erase `rows` (global) from the immutable segments holding them; see
    /// [`write_erased`]. Returns (segments rewritten, rows erased).
    pub fn erase(&self, rows: &[Row], meta: &HashMap<Row, BTreeMap<String, String>>) -> Result<(usize, usize)> {
        let _one = lock(&self.merging)?;
        let segments: Vec<Arc<DiskSegment>> = self.state.read().map_err(|_| "engine lock poisoned")?.segments.clone();
        let mut done = (0, 0);
        for s in segments {
            let dead: HashSet<u32> =
                rows.iter().filter(|r| **r >= s.start && **r < s.end()).map(|r| *r - s.start).collect();
            let local: HashMap<u32, BTreeMap<String, String>> = meta
                .iter()
                .filter(|(r, _)| **r >= s.start && **r < s.end())
                .map(|(r, m)| (*r - s.start, m.clone()))
                .collect();
            if dead.is_empty() && local.is_empty() {
                continue;
            }
            let (id, level) = {
                let mut w = lock(&self.writer)?;
                let id = w.manifest.next_segment;
                w.manifest.next_segment += 1;
                let level = w.manifest.segments.iter().find(|e| e.id == s.id).map_or(0, |e| e.level);
                (id, level)
            };
            let mut entry = match write_erased(self, &s, &dead, &local, id) {
                Ok(e) => e,
                Err(e) => {
                    let (idx, src) = seg_names(id);
                    let _ = fs::remove_file(self.directory.join(idx));
                    let _ = fs::remove_file(self.directory.join(src));
                    return Err(e);
                }
            };
            entry.level = level;
            let (idx, src) = seg_names(id);
            let run = self.options.archive.open(&self.directory.join(&src))?;
            let segment = Arc::new(DiskSegment::open(id, &self.directory.join(&idx), run)?);
            {
                let mut w = lock(&self.writer)?;
                let pos = w.manifest.segments.iter().position(|e| e.id == s.id).ok_or("erased segment left the manifest")?;
                let mut manifest = w.manifest.clone();
                manifest.segments[pos] = entry;
                write_manifest(&self.directory, &manifest)?;
                {
                    let mut st = self.state.write().map_err(|_| "engine lock poisoned")?;
                    let at = st.segments.iter().position(|x| x.id == s.id).ok_or("erased segment left the index")?;
                    st.segments[at] = segment;
                    st.epoch += 1;
                }
                if let Ok(mut c) = self.cache.lock() {
                    c.entries.clear();
                    c.bytes = 0;
                }
                w.manifest = manifest;
            }
            let (idx, src) = seg_names(s.id);
            drop(s);
            let _ = fs::remove_file(self.directory.join(idx));
            let _ = fs::remove_file(self.directory.join(src));
            sync_dir(&self.directory)?;
            done.0 += 1;
            done.1 += dead.len();
        }
        Ok(done)
    }
}
