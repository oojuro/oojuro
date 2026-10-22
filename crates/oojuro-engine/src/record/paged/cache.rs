use super::*;
use crate::lexical::{self, paged, record_cache as projection, segments::codec};
use oojuro_store::crypto::{SEAL_OVERHEAD, random_key};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
const FORMAT: u32 = 1;
const MANIFEST: &str = "PAGED_RECORD_INDEX";
const MANIFEST_BOUND: usize = 1 << 20;
const CACHE_BOUND: usize = 128 << 20;
const SEGMENTS_BOUND: usize = 1024;
const BUILD_ROWS: usize = 1024;
const BUILD_TEXT: usize = 16 << 20;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: String,
    prefix: Watermark,
    start: u32,
    rows: u32,
    bytes: usize,
    decoded: usize,
    digest: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    origin: [u8; 32],
    identity: [u8; 32],
    prefix: Watermark,
    rows: usize,
    segments: Vec<Reference>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opened {
    Loaded,
    Extended,
    Rebuilt,
}
/// Retains only bounded manifest references, not payload rows, terms or a hydrated Index.
pub struct Index {
    manifest: Manifest,
    directory: PathBuf,
    _lock: File,
    opened: Opened,
    failed: bool,
    #[cfg(test)]
    hook: Option<Box<dyn Fn(Phase)>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    SegmentTemp,
    SegmentPublished,
    ManifestTemp,
    ManifestPublished,
    ManifestDurable,
}
impl Index {
    pub fn open(reader: &Reader, directory: &Path) -> Result<Self> {
        let tip = reader.watermark()?;
        let lock = directory_lock(directory)?;
        let loaded = (|| {
            let plain = reader.unseal(
                &manifest_aad(tip.origin, tip.identity),
                &read_bounded(&directory.join(MANIFEST), MANIFEST_BOUND + SEAL_OVERHEAD)?,
            )?;
            let m: Manifest = serde_json::from_slice(&plain).map_err(|_| Error::Invalid)?;
            check_manifest(reader, &m)?;
            for r in &m.segments {
                let segment = load_segment(reader, directory, &m, r)?;
                check_original(reader, &m.prefix, &segment)?;
            }
            Ok::<_, Error>(m)
        })();
        let (manifest, opened) = match loaded {
            Ok(m) => {
                let opened = if same_prefix(&m.prefix, &tip) {
                    Opened::Loaded
                } else {
                    Opened::Extended
                };
                (m, opened)
            }
            Err(_) => (
                Manifest {
                    format: FORMAT,
                    origin: tip.origin,
                    identity: tip.identity,
                    prefix: tip,
                    rows: 0,
                    segments: vec![],
                },
                Opened::Rebuilt,
            ),
        };
        let mut index = Self {
            manifest,
            directory: directory.into(),
            _lock: lock,
            opened,
            failed: false,
            #[cfg(test)]
            hook: None,
        };
        if opened != Opened::Loaded {
            index.append_tail(reader, opened == Opened::Rebuilt)?;
        }
        Ok(index)
    }
    pub fn opened(&self) -> Opened {
        self.opened
    }
    pub fn segment_count(&self) -> usize {
        self.manifest.segments.len()
    }
    fn current(&self, reader: &Reader) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        reader.validate(&self.manifest.prefix)?;
        if !same_prefix(&self.manifest.prefix, &reader.watermark()?) {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    pub fn refresh(&mut self, reader: &Reader) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if self.current(reader).is_ok() {
            return Ok(());
        }
        let result = self.append_tail(reader, false);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn phase(&self, at: Phase) {
        #[cfg(test)]
        if let Some(h) = &self.hook {
            h(at);
        }
        #[cfg(not(test))]
        let _ = at;
    }
    fn append_tail(&mut self, reader: &Reader, replace: bool) -> Result<()> {
        let prefix = reader.watermark()?;
        let count = reader.validate(&prefix)?;
        let start = if replace {
            0
        } else {
            reader.validate(&self.manifest.prefix)?
        };
        let mut next = Manifest {
            format: FORMAT,
            origin: prefix.origin,
            identity: prefix.identity,
            prefix: prefix.clone(),
            rows: count,
            segments: if replace {
                vec![]
            } else {
                self.manifest.segments.clone()
            },
        };
        let mut rows = Vec::new();
        let mut text = 0usize;
        let mut at = start;
        reader.scan(&prefix, start, count, |row| {
            if !rows.is_empty()
                && (rows.len() >= BUILD_ROWS
                    || text
                        .checked_add(row.input.text.len())
                        .is_none_or(|n| n > BUILD_TEXT))
            {
                self.flush_rows(reader, &mut next, &rows, at)?;
                at += rows.len();
                rows.clear();
                text = 0;
            }
            text = text
                .checked_add(row.input.text.len())
                .ok_or(Error::Bounds)?;
            if text > BUILD_TEXT {
                return Err(Error::Bounds);
            }
            rows.push(row.clone());
            Ok(true)
        })?;
        if !rows.is_empty() {
            self.flush_rows(reader, &mut next, &rows, at)?;
        }
        self.commit_manifest(reader, next)
    }
    fn flush_rows(
        &self,
        reader: &Reader,
        m: &mut Manifest,
        rows: &[Assigned],
        start: usize,
    ) -> Result<()> {
        let id = reader.identity();
        let projection = lexical::Index::from_record_rows(
            &id.writer,
            id.agent,
            rows.iter().map(|r| {
                (
                    r.id,
                    r.input.valid_at,
                    r.input.text.as_str(),
                    r.input.indexed,
                )
            }),
        )
        .map_err(map_codec)?;
        if projection::candidate_bytes(&projection).map_err(map_codec)? > CACHE_BOUND {
            return Err(Error::Bounds);
        }
        let mut local = 0;
        while local < rows.len() {
            let mut count = (rows.len() - local).min(codec::MAX_ROWS);
            loop {
                let mut segment =
                    projection::snapshot(&projection, local, count).map_err(map_codec)?;
                segment.start = (start + local) as u32;
                match codec::encode(&segment) {
                    Ok(bytes) => {
                        self.emit(reader, m, &segment, &bytes)?;
                        break;
                    }
                    Err(_) if count > 1 => count = count.div_ceil(2),
                    Err(e) => return Err(map_codec(e)),
                }
            }
            local += count;
        }
        Ok(())
    }
    fn emit(
        &self,
        reader: &Reader,
        m: &mut Manifest,
        segment: &codec::Segment,
        bytes: &[u8],
    ) -> Result<()> {
        let decoded = projection::segment_bytes(segment).map_err(map_codec)?;
        let total = m.segments.iter().try_fold(bytes.len(), |n, r| {
            n.checked_add(r.bytes).ok_or(Error::Bounds)
        })?;
        let expanded = m.segments.iter().try_fold(decoded, |n, r| {
            n.checked_add(r.decoded).ok_or(Error::Bounds)
        })?;
        if total > CACHE_BOUND || expanded > CACHE_BOUND || m.segments.len() >= SEGMENTS_BOUND {
            return Err(Error::Bounds);
        }
        let mut r = Reference {
            id: hex(&random_key()),
            prefix: m.prefix.clone(),
            start: segment.start,
            rows: segment.rows.len() as u32,
            bytes: bytes.len(),
            decoded,
            digest: [0; 32],
        };
        let sealed = reader.seal(&segment_aad(m, &r)?, bytes);
        r.digest = *blake3::hash(&sealed).as_bytes();
        let temp = self.directory.join(format!("tmp-{}.idx", r.id));
        write_new(&temp, &sealed)?;
        self.phase(Phase::SegmentTemp);
        fs::rename(temp, self.directory.join(format!("seg-{}.idx", r.id)))
            .map_err(|_| Error::Unavailable)?;
        directory_sync(&self.directory)?;
        self.phase(Phase::SegmentPublished);
        m.segments.push(r);
        Ok(())
    }
    fn commit_manifest(&mut self, reader: &Reader, next: Manifest) -> Result<()> {
        check_manifest(reader, &next)?;
        let plain = json(&next, MANIFEST_BOUND)?;
        let sealed = reader.seal(&manifest_aad(next.origin, next.identity), &plain);
        let temp = self
            .directory
            .join(format!("tmp-{}.man", hex(&random_key())));
        write_new(&temp, &sealed)?;
        self.phase(Phase::ManifestTemp);
        fs::rename(temp, self.directory.join(MANIFEST)).map_err(|_| Error::Unavailable)?;
        self.phase(Phase::ManifestPublished);
        directory_sync(&self.directory)?;
        self.phase(Phase::ManifestDurable);
        self.manifest = next;
        reap(&self.directory, &self.manifest)
    }
    /// Coalesces contiguous bounded segments; never loads the full population.
    pub fn merge(&mut self, reader: &Reader) -> Result<()> {
        self.current(reader)?;
        let result = (|| {
            let mut next = Manifest {
                format: FORMAT,
                origin: self.manifest.origin,
                identity: self.manifest.identity,
                prefix: reader.watermark()?,
                rows: self.manifest.rows,
                segments: vec![],
            };
            let mut carry: Option<codec::Segment> = None;
            for reference in &self.manifest.segments {
                let segment = load_segment(reader, &self.directory, &self.manifest, reference)?;
                if let Some(old) = &carry {
                    let size = projection::segment_bytes(old)
                        .map_err(map_codec)?
                        .checked_add(projection::segment_bytes(&segment).map_err(map_codec)?)
                        .and_then(|n| n.checked_add(192))
                        .ok_or(Error::Bounds)?;
                    if old.rows.len() + segment.rows.len() > codec::MAX_ROWS
                        || size > codec::MAX_BYTES
                    {
                        let old = carry.take().unwrap();
                        self.emit(
                            reader,
                            &mut next,
                            &old,
                            &codec::encode(&old).map_err(map_codec)?,
                        )?;
                    }
                }
                if let Some(old) = &mut carry {
                    if old.start as usize + old.rows.len() != segment.start as usize {
                        return Err(Error::Authority);
                    }
                    let offset = old.rows.len() as u32;
                    old.rows.extend(segment.rows);
                    for (term, list) in segment.postings {
                        old.postings
                            .entry(term)
                            .or_default()
                            .extend(list.into_iter().map(|(r, tf)| (r + offset, tf)));
                    }
                } else {
                    carry = Some(segment);
                }
            }
            if let Some(old) = carry {
                self.emit(
                    reader,
                    &mut next,
                    &old,
                    &codec::encode(&old).map_err(map_codec)?,
                )?;
            }
            self.commit_manifest(reader, next)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn search(&self, reader: &Reader, query: &Query<'_>) -> Result<Search> {
        let s = reader.view(query.scope, query.as_of)?;
        self.search_view(reader, &s, query)
    }
    pub fn search_view(
        &self,
        reader: &Reader,
        snapshot: &Snapshot,
        query: &Query<'_>,
    ) -> Result<Search> {
        self.current(reader)?;
        reader.snapshot(snapshot)?;
        if query.scope.writer != reader.identity().writer
            || query.scope.owner != reader.identity().agent
        {
            return Err(Error::Scope);
        }
        if query.as_of != snapshot.as_of() {
            return Err(Error::Invalid);
        }
        query.filter.validate()?;
        if query.max_bytes == 0
            || query.max_bytes > MAX_OUTPUT_BYTES
            || query.k > 10000
            || query.text.len() > lexical::MAX_QUERY_BYTES
        {
            return Err(Error::Bounds);
        }
        if query.k == 0 || query.as_of.from > query.as_of.valid_at {
            return Ok(Search {
                watermark: snapshot.watermark().clone(),
                hits: vec![],
            });
        }
        let direct = if query.text.starts_with("key:") {
            Some(reader.winner(snapshot, &query.text[4..])?)
        } else {
            reader.winner(snapshot, query.text)?.map(Some)
        };
        // Quoted keys are literals in the shared scorer, not direct key lookups.
        let direct = if lexical_literal(query.text) {
            None
        } else {
            direct
        };
        let (first, anchored) = self.rank(reader, snapshot, query, false, direct)?;
        let mut hits = first;
        if anchored && hits.len() < query.k {
            let (rest, _) = self.rank(reader, snapshot, query, true, direct)?;
            let listed: BTreeSet<_> = hits.iter().map(|h| h.id).collect();
            for mut h in rest {
                if hits.len() == query.k {
                    break;
                }
                if !listed.contains(&h.id) {
                    h.continued = true;
                    hits.push(h);
                }
            }
        }
        let mut out = Vec::new();
        let mut bytes = 0usize;
        for h in hits {
            let record = reader
                .assigned(snapshot.watermark(), h.id)?
                .ok_or(Error::Authority)?
                .record(&reader.identity().writer, &reader.identity().adapter);
            bytes = bytes
                .checked_add(json(&record, query.max_bytes)?.len())
                .ok_or(Error::Bounds)?;
            if bytes > query.max_bytes {
                return Err(Error::Bounds);
            }
            out.push(Hit {
                record,
                bits: h.bits,
                continued: h.continued,
                row: h.row,
            });
        }
        let result = Search {
            watermark: snapshot.watermark().clone(),
            hits: out,
        };
        json(&result, query.max_bytes)?;
        Ok(result)
    }
    fn rank(
        &self,
        reader: &Reader,
        snapshot: &Snapshot,
        query: &Query<'_>,
        split: bool,
        direct: Option<Option<u64>>,
    ) -> Result<(Vec<lexical::Hit>, bool)> {
        let possible = paged::possible_terms(query.text);
        let mut df: BTreeMap<String, u64> = possible.iter().map(|t| (t.clone(), 0)).collect();
        let mut present = BTreeSet::new();
        let raw: Vec<_> = paged::terms(query.text).into_iter().collect();
        for reference in &self.manifest.segments {
            let segment = load_segment(reader, &self.directory, &self.manifest, reference)?;
            for (term, n) in &mut df {
                *n = n
                    .checked_add(segment.postings.get(term).map_or(0, |p| p.len() as u64))
                    .ok_or(Error::Bounds)?;
            }
        }
        self.walk(
            reader,
            snapshot,
            &query.filter,
            &raw,
            |_, _, tf, visible| {
                if visible {
                    for ((term, _), n) in raw.iter().zip(tf) {
                        if *n > 0 {
                            present.insert(term.clone());
                        }
                    }
                }
                Ok(())
            },
        )?;
        let plan = paged::Plan::new(query.text, split, direct, &present, &df);
        let mut stats = paged::Stats::new(&plan);
        self.walk(
            reader,
            snapshot,
            &query.filter,
            &plan.terms,
            |r, row, tf, visible| {
                stats
                    .visit(
                        &plan,
                        row.length,
                        row.scope,
                        tf,
                        visible,
                        plan.candidate(r.id, r.input.indexed, &r.input.text, visible, tf),
                    )
                    .map_err(map_codec)
            },
        )?;
        let model = stats.finish(plan);
        let anchored = model.plan.continues();
        let mut top = paged::Top::new(query.k);
        self.walk(
            reader,
            snapshot,
            &query.filter,
            &model.plan.terms,
            |r, row, tf, visible| {
                if model
                    .plan
                    .candidate(r.id, r.input.indexed, &r.input.text, visible, tf)
                {
                    top.push(
                        r.id,
                        r.input.valid_at,
                        model.score(row.length, row.scope, tf),
                    );
                }
                Ok(())
            },
        )?;
        Ok((top.finish(), anchored))
    }
    fn walk(
        &self,
        reader: &Reader,
        snapshot: &Snapshot,
        filter: &Filter,
        terms: &[(String, usize)],
        mut visit: impl FnMut(&Assigned, &codec::Row, &[u32], bool) -> Result<()>,
    ) -> Result<()> {
        let bound = reader.snapshot(snapshot)?;
        for reference in &self.manifest.segments {
            let start = reference.start as usize;
            if start >= bound {
                break;
            }
            let segment = load_segment(reader, &self.directory, &self.manifest, reference)?;
            let end = (start + segment.rows.len()).min(bound);
            let mut local = 0usize;
            reader.scan(snapshot.watermark(), start, end, |r| {
                let row = segment.rows.get(local).ok_or(Error::Authority)?;
                if row.id != r.id {
                    return Err(Error::Authority);
                }
                let tf: Vec<_> = terms
                    .iter()
                    .map(|(t, _)| {
                        segment
                            .postings
                            .get(t)
                            .and_then(|p| {
                                p.binary_search_by_key(&(local as u32), |(i, _)| *i)
                                    .ok()
                                    .map(|i| p[i].1)
                            })
                            .unwrap_or(0)
                    })
                    .collect();
                let visible = reader.visible(snapshot, r, filter)?;
                visit(r, row, &tf, visible)?;
                local += 1;
                Ok(true)
            })?;
        }
        Ok(())
    }
}
fn map_codec(e: lexical::Error) -> Error {
    match e {
        lexical::Error::Invalid => Error::Bounds,
        lexical::Error::Authority => Error::Authority,
        lexical::Error::Unavailable => Error::Unavailable,
    }
}
fn lexical_literal(text: &str) -> bool {
    paged::is_literal(text)
}
fn same_prefix(a: &Watermark, b: &Watermark) -> bool {
    a.origin == b.origin
        && a.identity == b.identity
        && a.sequence == b.sequence
        && a.records == b.records
}
fn manifest_aad(origin: [u8; 32], identity: [u8; 32]) -> Vec<u8> {
    let mut out = b"oojuro/native-record-paged/manifest/v1\0".to_vec();
    out.extend(origin);
    out.extend(identity);
    out
}
fn segment_aad(m: &Manifest, r: &Reference) -> Result<Vec<u8>> {
    let mut a = b"oojuro/native-record-paged/segment/v1\0".to_vec();
    a.extend(m.origin);
    a.extend(m.identity);
    a.extend(r.id.as_bytes());
    a.extend(serde_json::to_vec(&r.prefix).map_err(|_| Error::Invalid)?);
    a.extend(r.start.to_le_bytes());
    a.extend(r.rows.to_le_bytes());
    a.extend((r.bytes as u64).to_le_bytes());
    a.extend((r.decoded as u64).to_le_bytes());
    Ok(a)
}
fn check_manifest(reader: &Reader, m: &Manifest) -> Result<()> {
    let tip = reader.watermark()?;
    if m.format != FORMAT
        || m.origin != tip.origin
        || m.identity != tip.identity
        || m.rows > MAX_RECORDS
        || reader.validate(&m.prefix)? != m.rows
        || m.segments.len() > SEGMENTS_BOUND
    {
        return Err(Error::Authority);
    }
    let (mut rows, mut total, mut decoded) = (0usize, 0usize, 0usize);
    let mut ids = HashSet::new();
    for r in &m.segments {
        let authorized = reader.validate(&r.prefix)?;
        if !id_valid(&r.id)
            || !ids.insert(&r.id)
            || r.rows == 0
            || r.rows as usize > codec::MAX_ROWS
            || r.start as usize != rows
            || r.bytes < 192
            || r.bytes > codec::MAX_BYTES
            || r.decoded > codec::MAX_BYTES
            || r.prefix.sequence > m.prefix.sequence
        {
            return Err(Error::Invalid);
        }
        rows = rows.checked_add(r.rows as usize).ok_or(Error::Bounds)?;
        total = total.checked_add(r.bytes).ok_or(Error::Bounds)?;
        decoded = decoded.checked_add(r.decoded).ok_or(Error::Bounds)?;
        if rows > m.rows || rows > authorized || total > CACHE_BOUND || decoded > CACHE_BOUND {
            return Err(Error::Bounds);
        }
    }
    if rows != m.rows {
        return Err(Error::Authority);
    }
    Ok(())
}
fn load_segment(
    reader: &Reader,
    dir: &Path,
    m: &Manifest,
    r: &Reference,
) -> Result<codec::Segment> {
    reader.validate(&r.prefix)?;
    let sealed = read_bounded(
        &dir.join(format!("seg-{}.idx", r.id)),
        codec::MAX_BYTES + SEAL_OVERHEAD,
    )?;
    if sealed.len() != r.bytes + SEAL_OVERHEAD || *blake3::hash(&sealed).as_bytes() != r.digest {
        return Err(Error::Authority);
    }
    let plain = reader.unseal(&segment_aad(m, r)?, &sealed)?;
    let segment = codec::decode(&plain).map_err(map_codec)?;
    if segment.start != r.start
        || segment.rows.len() != r.rows as usize
        || projection::segment_bytes(&segment).map_err(map_codec)? != r.decoded
    {
        return Err(Error::Authority);
    }
    let mut lengths = vec![0u64; segment.rows.len()];
    let mut scopes = vec![0u64; segment.rows.len()];
    for list in segment.postings.values() {
        for (at, tf) in list {
            let at = *at as usize;
            if at >= lengths.len() || *tf == 0 {
                return Err(Error::Authority);
            }
            lengths[at] = lengths[at]
                .checked_add(u64::from(*tf))
                .ok_or(Error::Bounds)?;
            scopes[at] += 1;
        }
    }
    for (i, row) in segment.rows.iter().enumerate() {
        let expected = reader
            .identity()
            .first_id
            .checked_add(r.start as u64)
            .and_then(|n| n.checked_add(i as u64))
            .ok_or(Error::Bounds)?;
        if row.id != expected
            || row.fingerprint != projection::fingerprint(expected)
            || u64::from(row.length) != lengths[i]
            || u64::from(row.scope) != scopes[i]
        {
            return Err(Error::Authority);
        }
    }
    Ok(segment)
}
fn check_original(reader: &Reader, prefix: &Watermark, segment: &codec::Segment) -> Result<()> {
    let mut at = 0;
    reader.scan(
        prefix,
        segment.start as usize,
        segment.start as usize + segment.rows.len(),
        |r| {
            let row = &segment.rows[at];
            if row.id != r.id
                || ((!r.input.indexed || r.input.text.is_empty())
                    && (row.length != 0 || row.scope != 0))
            {
                return Err(Error::Authority);
            }
            at += 1;
            Ok(true)
        },
    )
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn id_valid(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn read_bounded(path: &Path, bound: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)?;
    let metadata = file.metadata().map_err(|_| Error::Unavailable)?;
    if !metadata.is_file() || metadata.len() > bound as u64 {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(bound as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    if bytes.len() > bound {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::Unavailable)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Unavailable)
}
fn directory_sync(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::Unavailable)
}
fn known(name: &str) -> bool {
    name == MANIFEST
        || name == "LOCK"
        || [("seg-", ".idx"), ("tmp-", ".idx"), ("tmp-", ".man")]
            .iter()
            .any(|(a, b)| {
                name.strip_prefix(a)
                    .and_then(|s| s.strip_suffix(b))
                    .is_some_and(id_valid)
            })
}
fn directory_lock(dir: &Path) -> Result<File> {
    match fs::symlink_metadata(dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => return Err(Error::Invalid),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|_| Error::Unavailable)?
        }
        Err(_) => return Err(Error::Unavailable),
    }
    for entry in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if !known(name) || !entry.file_type().map_err(|_| Error::Unavailable)?.is_file() {
            return Err(Error::Invalid);
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Unavailable)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("LOCK"))
        .map_err(|_| Error::Unavailable)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Unavailable);
    }
    Ok(lock)
}
fn reap(dir: &Path, m: &Manifest) -> Result<()> {
    let live: HashSet<_> = m
        .segments
        .iter()
        .map(|r| format!("seg-{}.idx", r.id))
        .collect();
    for entry in fs::read_dir(dir).map_err(|_| Error::Unavailable)? {
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        if known(name) && name != "LOCK" && name != MANIFEST && !live.contains(name) {
            fs::remove_file(entry.path()).map_err(|_| Error::Unavailable)?;
        }
    }
    directory_sync(dir)
}
