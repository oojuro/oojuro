//! Disposable encrypted projections of caller-validated immutable snapshots.
//!
//! This is not a source, WAL or authority. Callers validate current permissions,
//! source checkpoints and Binding before reuse and through response completion.
//! The optional disk representation preserves the existing projection ranker.
mod budget;
mod build;
mod compact;
mod events;
mod joint;
mod literal;
mod query_cache;
mod score_blocks;
mod storage;
mod term_cache;
use super::*;
use crate::lexical::paged as scorer;
use crate::record::paged::tree;
pub use budget::BuildMetrics;
pub use build::{Appender, Builder};
pub use events::{
    EventAppender, EventBuilder, EventHit, EventIndex, EventQuery, EventRanked, EventRow,
};
use oojuro_store::crypto::FrameCipher;
use std::{
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap},
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};
use storage::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compression {
    Raw,
    Zstd,
}

/// Explicit admission/working limits for this representation, separate from the
/// legacy in-memory projection's100k/128MiB bounds. They are not an SLA.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub rows: u64,
    pub source_bytes: u64,
    pub index_bytes: u64,
    pub metadata_bytes: usize,
    pub original_bytes_per_query: usize,
    pub query_terms: usize,
    pub compression: Compression,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            rows: 2_000_000,
            source_bytes: 64 << 30,
            index_bytes: 128 << 30,
            metadata_bytes: 256 << 20,
            original_bytes_per_query: 512 << 20,
            query_terms: 256,
            compression: Compression::Raw,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<()> {
        if self.rows == 0
            || self.rows > u32::MAX as u64
            || self.source_bytes == 0
            || self.index_bytes == 0
            || self.metadata_bytes < std::mem::size_of::<Column>()
            || self.query_terms == 0
            || self.query_terms > 4096
            || self.original_bytes_per_query == 0
        {
            return Err(Error::Bounds);
        }
        Ok(())
    }
}

/// A ranked identity. Original metadata/text is still fetched and compared when
/// materialized. Keeping ranking separate permits bounded async source reads.
#[derive(Clone, Debug, Serialize)]
pub struct Ranked {
    pub id: String,
    pub bits: f64,
    pub continued: bool,
    #[serde(skip)]
    ordinal: u32,
}

/// Immutable positioned-read handle. Numeric metadata is shared across queries;
/// originals and complete Row/String populations are not retained in RAM.
pub struct Index {
    directory: PathBuf,
    root_directory: PathBuf,
    binding: Binding,
    authority: Authority,
    cipher: Arc<FrameCipher>,
    pages: Arc<tree::PageCache>,
    manifest: Manifest,
    columns: Vec<Arc<[Column]>>,
    mass: Option<compact::Mass>,
    totals: Totals,
    queries: query_cache::QueryCache,
    terms: term_cache::TermCache,
    retained_disk_bytes: u64,
    retained_allocated_bytes: u64,
    _lock: File,
}
impl Index {
    pub fn open(directory: impl AsRef<Path>, key: &[u8; 32], binding: &Binding) -> Result<Self> {
        storage::open(directory.as_ref(), key, binding)
    }
    pub fn row_count(&self) -> u64 {
        self.manifest.live_rows
    }
    pub fn indexed_row_count(&self) -> u64 {
        self.manifest.rows
    }
    pub fn text_bytes(&self) -> u64 {
        self.manifest.source_bytes
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// All temporary and retained files from construction. Allocated bytes are
    /// observations at close/removal/phase boundaries, not a filesystem quota.
    pub fn build_metrics(&self) -> BuildMetrics {
        self.manifest.build_metrics
    }
    /// Apparent bytes of encrypted cache files, including metadata and the
    /// descriptor. Allocation blocks and other cache generations are separate.
    pub fn index_bytes(&self) -> u64 {
        self.retained_disk_bytes
    }
    /// Observed Linux allocation blocks for retained encrypted files and their
    /// containing directories. Other generations/builds are additional.
    pub fn allocated_index_bytes(&self) -> u64 {
        self.retained_allocated_bytes
    }
    pub fn open_with_limits(
        directory: impl AsRef<Path>,
        key: &[u8; 32],
        binding: &Binding,
        limits: Limits,
    ) -> Result<Self> {
        storage::open_with_limits(directory.as_ref(), key, binding, limits)
    }
    pub fn estimated_heap_bytes(&self) -> usize {
        fn root_bytes(root: &tree::Root) -> usize {
            root.binding.capacity()
                + root.node.as_ref().map_or(0, |r| {
                    r.birth.capacity() + r.first.capacity() + r.last.capacity()
                })
        }
        self.columns
            .iter()
            .map(|c| std::mem::size_of_val(c.as_ref()) + 2 * std::mem::size_of::<usize>())
            .sum::<usize>()
            .saturating_add(self.columns.capacity() * std::mem::size_of::<Arc<[Column]>>())
            .saturating_add(self.mass.as_ref().map_or(0, compact::Mass::bytes))
            .saturating_add(self.manifest.files.capacity() * std::mem::size_of::<String>())
            .saturating_add(
                self.manifest
                    .files
                    .iter()
                    .map(String::capacity)
                    .sum::<usize>(),
            )
            .saturating_add(self.manifest.columns.capacity() * std::mem::size_of::<Blob>())
            .saturating_add(self.manifest.binding.capacity() + self.manifest.generation.capacity())
            .saturating_add(
                root_bytes(&self.manifest.metadata)
                    + root_bytes(&self.manifest.ids)
                    + root_bytes(&self.manifest.keys)
                    + root_bytes(&self.manifest.postings),
            )
            .saturating_add(self.manifest.positions.as_ref().map_or(0, root_bytes))
            .saturating_add(
                self.directory.capacity()
                    + self.root_directory.capacity()
                    + self.binding.writer.capacity(),
            )
            .saturating_add(self.authority.identity.capacity() + self.authority.binding.capacity())
            // Reserve the complete4MiB decoded-page policy before warming,
            // plus equal headroom for small-allocation bookkeeping.
            .saturating_add(8 << 20)
            .saturating_add(query_cache::RESERVED_BYTES)
            .saturating_add(term_cache::RESERVED_BYTES)
            .saturating_add(std::mem::size_of::<Self>() + std::mem::size_of::<FrameCipher>())
    }
    fn current(&self, binding: &Binding) -> Result<()> {
        if *binding != self.binding {
            return Err(Error::Scope);
        }
        Ok(())
    }
    fn context(&self, purpose: tree::Purpose) -> tree::Context<'_> {
        context(
            &self.directory,
            &self.cipher,
            &self.authority,
            purpose,
            Some(&self.pages),
        )
    }
    fn column(&self, ordinal: u32) -> Result<&Column> {
        self.columns
            .get(ordinal as usize / COLUMN_ROWS)
            .and_then(|v| v.get(ordinal as usize % COLUMN_ROWS))
            .ok_or(Error::Authority)
    }
    fn metadata(&self, ordinal: u32) -> Result<Metadata> {
        let bytes = tree::get(
            self.context(tree::Purpose::ProjectionMetadata),
            &self.manifest.metadata,
            &ordinal.to_be_bytes(),
        )?
        .ok_or(Error::Authority)?;
        decode_metadata(&bytes, &self.manifest.limits, self.manifest.format)
    }
    fn cached_text(&self, ordinal: u32, read: &mut usize) -> Result<String> {
        let meta = self.metadata(ordinal)?;
        *read = read
            .checked_add(meta.text.bytes as usize)
            .ok_or(Error::Bounds)?;
        if *read > self.manifest.limits.original_bytes_per_query {
            return Err(Error::Bounds);
        }
        let bytes = read_blob(
            &self.directory,
            &self.cipher,
            &self.authority,
            &self.manifest.files,
            &meta.text,
            MAX_TEXT_BYTES,
        )?;
        if *blake3::hash(&bytes).as_bytes() != meta.text_digest {
            return Err(Error::Authority);
        }
        String::from_utf8(bytes).map_err(|_| Error::Authority)
    }
    fn query(&self, q: &Query<'_>) -> Result<()> {
        if q.text.trim().is_empty()
            || q.text.len() > lexical::MAX_QUERY_BYTES
            || q.k > 10000
            || q.excerpt_bytes == 0
            || q.excerpt_bytes > 65536
            || q.max_bytes == 0
            || q.max_bytes > MAX_OUTPUT_BYTES
        {
            return Err(Error::Bounds);
        }
        q.filter.validate()
    }
    /// Rank the complete bound projection, never per-shard score fusion. Callers
    /// still validate current source/auth before and after any provider I/O.
    pub fn rank(&self, binding: &Binding, q: &Query<'_>) -> Result<Vec<Ranked>> {
        self.current(binding)?;
        self.query(q)?;
        self.queries
            .rank(query_cache::key(q)?, || self.rank_uncached(binding, q))
    }
    /// Bypass bounded rank-result reuse. Source checks and materialization are
    /// identical; immutable authenticated page reuse can still occur.
    pub fn rank_uncached(&self, binding: &Binding, q: &Query<'_>) -> Result<Vec<Ranked>> {
        self.current(binding)?;
        self.query(q)?;
        if q.k == 0 || q.from > q.to {
            return Ok(vec![]);
        }
        let wanted = q.text.strip_prefix("key:").unwrap_or(q.text);
        if !scorer::is_literal(q.text) {
            let prefix = *blake3::hash(wanted.as_bytes()).as_bytes();
            let mut cursor = tree::Cursor::seek(
                self.context(tree::Purpose::ProjectionKeys),
                &self.manifest.keys,
                &prefix,
            )?;
            let selection = Selection::new(q);
            let mut found = false;
            let mut top = Top::new(q.k);
            while let Some(entry) = cursor.next()? {
                if !entry.key.starts_with(&prefix) {
                    break;
                }
                if entry.key.len() != 36 {
                    return Err(Error::Authority);
                }
                let ordinal = parse_u32(&entry.key[32..])?;
                let col = self.column(ordinal)?;
                if col.live() {
                    found = true;
                }
                if col.visible(&selection) {
                    top.push(Scored {
                        ordinal,
                        label: col.label,
                        time: col.valid_at,
                        bits: 0.0,
                    });
                }
            }
            if found || q.text.starts_with("key:") {
                let mut out = Vec::new();
                for value in top.finish() {
                    let meta = self.metadata(value.ordinal)?;
                    if meta.key.as_deref() != Some(wanted) {
                        return Err(Error::Authority);
                    }
                    out.push(Ranked {
                        id: meta.id,
                        bits: 0.0,
                        continued: false,
                        ordinal: value.ordinal,
                    });
                }
                return Ok(out);
            }
        }
        let direct = None;
        let (mut hits, anchored) = self.rank_pass(q, false, direct)?;
        if anchored && hits.len() < q.k {
            let (rest, _) = self.rank_pass(q, true, direct)?;
            let selected: BTreeSet<_> = hits.iter().map(|h| h.ordinal).collect();
            for mut hit in rest {
                if hits.len() == q.k {
                    break;
                }
                if !selected.contains(&hit.ordinal) {
                    hit.continued = true;
                    hits.push(hit)
                }
            }
        }
        Ok(hits)
    }
    pub fn search(
        &self,
        binding: &Binding,
        q: &Query<'_>,
        fetch: impl FnMut(&str) -> Result<Row>,
    ) -> Result<Vec<Hit>> {
        let ranked = self.rank(binding, q)?;
        self.materialize(binding, q, &ranked, fetch)
    }
    /// Validate selected original identities and the complete input-byte budget
    /// before a caller batches provider reads. Sizes come from authenticated
    /// metadata, in ranked order; they are not source authority or permission.
    /// Callers still revalidate the source/Binding before and after those reads
    /// and pass exact originals to `materialize` for full field/hash validation.
    pub fn originals_plan(
        &self,
        binding: &Binding,
        q: &Query<'_>,
        ranked: &[Ranked],
    ) -> Result<Vec<(String, usize)>> {
        self.originals_request(binding, q, ranked)?;
        let mut read = 0;
        let mut plan = Vec::with_capacity(ranked.len());
        for hit in ranked {
            let meta = self.original_metadata(hit, &mut read)?;
            plan.push((meta.id, meta.text.bytes as usize));
        }
        Ok(plan)
    }
    fn originals_request(&self, binding: &Binding, q: &Query<'_>, ranked: &[Ranked]) -> Result<()> {
        self.current(binding)?;
        self.query(q)?;
        if ranked.len() > q.k {
            return Err(Error::Bounds);
        }
        Ok(())
    }
    fn original_metadata(&self, hit: &Ranked, read: &mut usize) -> Result<Metadata> {
        if !hit.bits.is_finite() {
            return Err(Error::Invalid);
        }
        let meta = self.metadata(hit.ordinal)?;
        if hit.id != meta.id || !self.column(hit.ordinal)?.live() {
            return Err(Error::Authority);
        }
        *read = read
            .checked_add(meta.text.bytes as usize)
            .ok_or(Error::Bounds)?;
        if *read > self.manifest.limits.original_bytes_per_query {
            return Err(Error::Bounds);
        }
        Ok(meta)
    }
    /// Compare complete original bytes and every Row metadata field before
    /// producing excerpts. A derived projection cannot substitute for a source.
    pub fn materialize(
        &self,
        binding: &Binding,
        q: &Query<'_>,
        ranked: &[Ranked],
        mut fetch: impl FnMut(&str) -> Result<Row>,
    ) -> Result<Vec<Hit>> {
        self.originals_request(binding, q, ranked)?;
        let mut out = Vec::new();
        let mut read = 0usize;
        let mut bytes = 2usize;
        for hit in ranked {
            let meta = self.original_metadata(hit, &mut read)?;
            let row = fetch(&hit.id)?;
            if !meta.matches(&row) {
                return Err(Error::Authority);
            }
            let value = super::materialize(&row, &row.text, hit.bits, hit.continued, q)?;
            bytes = bytes
                .checked_add(json_size(&value, q.max_bytes)? + usize::from(!out.is_empty()))
                .ok_or(Error::Bounds)?;
            if bytes > q.max_bytes {
                return Err(Error::Bounds);
            }
            out.push(value);
        }
        if bytes > q.max_bytes {
            return Err(Error::Bounds);
        }
        Ok(out)
    }
    fn rank_pass(
        &self,
        q: &Query<'_>,
        split: bool,
        direct: Option<Option<u64>>,
    ) -> Result<(Vec<Ranked>, bool)> {
        let selection = Selection::new(q);
        let mut present = BTreeSet::new();
        let mut df = BTreeMap::new();
        let raw = scorer::terms(q.text);
        if raw.len() > self.manifest.limits.query_terms {
            return Err(Error::Bounds);
        }
        let selected = if selection.unfiltered() {
            None
        } else {
            Some(match self.terms.selection(self, q, &selection)? {
                Some(view) => view,
                None => Arc::new(self.select(&selection)?),
            })
        };
        let sparse = selected
            .as_ref()
            .filter(|view| view.count <= 65536)
            .map(|view| view.bits.as_slice());
        let mut witness = Vec::new();
        for term in scorer::possible_terms(q.text) {
            if !scorer::is_literal(q.text) && (split || !term.contains('_')) {
                continue;
            }
            if scorer::is_literal(q.text)
                && self.manifest.positions.is_some()
                && let Some(mass) = &self.mass
                && let Some(list) = self
                    .terms
                    .get(self, *blake3::hash(term.as_bytes()).as_bytes())?
            {
                if !split && term.contains('_') {
                    let visible = if selected.is_none() {
                        list.statistics.df > 0
                    } else {
                        witness.resize(mass.live.len(), 0);
                        witness.fill(0);
                        list.pairs.presence(
                            &mut witness,
                            &mass.live,
                            selected.as_ref().map(|view| view.bits.as_slice()),
                        )? > 0
                    };
                    if visible {
                        present.insert(term.clone());
                    }
                }
                df.insert(term, list.statistics.df);
                continue;
            }
            let mut cursor = Postings::new(self, &term)?;
            let mut count = 0u64;
            while let Some((ordinal, _)) = cursor.next()? {
                let column = self.column(ordinal)?;
                if column.live() {
                    count += 1
                }
                if !split && term.contains('_') && column.visible(&selection) {
                    present.insert(term.clone());
                    // Only literal planning consumes global DF. Identifier
                    // presence needs one eligible witness, not the full list.
                    if !scorer::is_literal(q.text) {
                        break;
                    }
                }
            }
            df.insert(term, count);
        }
        let plan = scorer::Plan::new(q.text, split, direct, &present, &df);
        if plan.terms.len() > self.manifest.limits.query_terms {
            return Err(Error::Bounds);
        }
        if selected.as_ref().is_some_and(|view| view.count == 0) {
            return Ok((vec![], false));
        }
        if plan.terms.is_empty() && plan.literal.is_none() {
            return Ok((vec![], false));
        }
        if selection.unfiltered() && plan.scope_model() && plan.terms.len() == 1 {
            let term = *blake3::hash(plan.terms[0].0.as_bytes()).as_bytes();
            if let Some(list) = self.terms.get(self, term)? {
                let model = scorer::Stats::single_scope(
                    &plan,
                    list.statistics.df,
                    list.statistics.scope,
                    self.totals.scope,
                )
                .map_err(|_| Error::Bounds)?
                .finish(plan);
                let mut top = Top::new(q.k);
                if let Some(groups) = list
                    .groups
                    .as_ref()
                    .filter(|_| q.k <= term_cache::GROUP_TOP)
                {
                    for group in groups {
                        let (length, scope, tf) = group.shape;
                        let bits = model.score(length, scope, &[tf]);
                        for row in &group.top {
                            top.push(Scored {
                                ordinal: row.ordinal,
                                label: row.label,
                                time: row.time,
                                bits,
                            });
                        }
                    }
                } else {
                    if let Some(top) = self.rank_joint(&model, &selection, q.k)? {
                        return Ok((self.ranked(top)?, false));
                    }
                    for (ordinal, tf) in list.pairs.iter() {
                        let column = self.column(ordinal)?;
                        if column.live() {
                            top.push(Scored {
                                ordinal,
                                label: column.label,
                                time: column.valid_at,
                                bits: model.score(column.length, column.scope, &[tf]),
                            });
                        }
                    }
                }
                return Ok((self.ranked(top)?, false));
            }
        }
        let proof = plan
            .literal
            .as_ref()
            .map(|needle| self.literal_candidates(needle))
            .transpose()?
            .flatten();
        let proven = proof.is_some();
        let mut literal = proof.or_else(|| {
            plan.literal
                .as_ref()
                .map(|_| vec![0u64; (self.manifest.rows as usize).div_ceil(64)])
        });
        let compact_stats = if proven {
            self.literal_statistics(&plan, selected.as_deref(), literal.as_mut().unwrap())?
        } else {
            self.compact_statistics(&plan, selected.as_deref())?
        };
        let stats = if let Some(stats) = compact_stats {
            stats
        } else {
            let mut stats = scorer::Stats::new(&plan);
            let mut read = 0usize;
            self.walk(&plan, direct, true, sparse, |ordinal, tf| {
                let column = self.column(ordinal)?;
                let visible = column.visible(&selection);
                let mut candidate =
                    plan.candidate_terms(u64::from(ordinal), column.indexed(), visible, tf);
                if candidate && plan.literal.is_some() {
                    if proven {
                        candidate = literal.as_ref().unwrap()[ordinal as usize / 64]
                            & (1 << (ordinal % 64))
                            != 0;
                    } else {
                        let text = self.cached_text(ordinal, &mut read)?;
                        candidate = plan.candidate(
                            u64::from(ordinal),
                            column.indexed(),
                            &text,
                            visible,
                            tf,
                        );
                        if candidate {
                            literal.as_mut().unwrap()[ordinal as usize / 64] |= 1 << (ordinal % 64);
                        }
                    }
                }
                stats
                    .visit(&plan, column.length, column.scope, tf, visible, candidate)
                    .map_err(|_| Error::Bounds)
            })?;
            if stats.needs_view_total() {
                if plan.anchored_driver() {
                    stats.reset_view();
                    self.walk(&plan, direct, false, sparse, |ordinal, tf| {
                        let col = self.column(ordinal)?;
                        stats
                            .visit(
                                &plan,
                                col.length,
                                col.scope,
                                tf,
                                col.visible(&selection),
                                false,
                            )
                            .map_err(|_| Error::Bounds)
                    })?;
                }
                let total = if let Some(selected) = &selected {
                    if plan.scope_model() {
                        selected.totals.scope
                    } else {
                        selected.totals.length
                    }
                } else {
                    if plan.scope_model() {
                        self.totals.scope
                    } else {
                        self.totals.length
                    }
                };
                stats.set_view_total(total);
            }
            stats
        };
        let anchored = plan.continues();
        let model = stats.finish(plan);
        if let Some(top) = self.rank_joint(&model, &selection, q.k)? {
            return Ok((self.ranked(top)?, anchored));
        }
        if let Some(top) = self.rank_compact_blocks(&model, &selection, q.k, literal.as_deref())? {
            return Ok((self.ranked(top)?, anchored));
        }
        let mut scores = self
            .mass
            .as_ref()
            .and_then(|_| compact::Scores::new(model.plan.terms.len()));
        let mut top = Top::new(q.k);
        self.walk(&model.plan, direct, true, sparse, |ordinal, tf| {
            let col = self.column(ordinal)?;
            let candidate = model.plan.candidate_terms(
                u64::from(ordinal),
                col.indexed(),
                col.visible(&selection),
                tf,
            ) && literal
                .as_ref()
                .is_none_or(|b| b[ordinal as usize / 64] & (1 << (ordinal % 64)) != 0);
            if candidate {
                top.push(Scored {
                    ordinal,
                    label: col.label,
                    time: col.valid_at,
                    bits: scores.as_mut().map_or_else(
                        || model.score(col.length, col.scope, tf),
                        |scores| scores.score(&model, col.length, col.scope, tf),
                    ),
                });
            }
            Ok(())
        })?;
        Ok((self.ranked(top)?, anchored))
    }
    fn ranked(&self, top: Top) -> Result<Vec<Ranked>> {
        let mut output = Vec::new();
        for hit in top.finish() {
            output.push(Ranked {
                id: self.metadata(hit.ordinal)?.id,
                bits: hit.bits,
                continued: false,
                ordinal: hit.ordinal,
            });
        }
        Ok(output)
    }
    fn compact_statistics(
        &self,
        plan: &scorer::Plan,
        selected: Option<&Selected>,
    ) -> Result<Option<scorer::Stats>> {
        let Some(mass) = self.mass.as_ref().filter(|_| plan.scope_model()) else {
            return Ok(None);
        };
        let mut lists = Vec::with_capacity(plan.terms.len());
        for (term, _) in &plan.terms {
            let Some(list) = self
                .terms
                .get(self, *blake3::hash(term.as_bytes()).as_bytes())?
            else {
                return Ok(None);
            };
            lists.push(list);
        }
        let mut union = vec![0u64; mass.live.len()];
        let mut counts = Vec::with_capacity(lists.len());
        for list in lists {
            counts.push(list.pairs.presence(
                &mut union,
                &mass.live,
                selected.map(|selected| selected.bits.as_slice()),
            )?);
        }
        let total = mass.sum(&union, true)?;
        let view = selected.map_or(self.totals.scope, |selected| selected.totals.scope);
        Ok(Some(
            scorer::Stats::scope_counts(plan, counts, total, view).map_err(|_| Error::Bounds)?,
        ))
    }
    fn literal_statistics(
        &self,
        plan: &scorer::Plan,
        selected: Option<&Selected>,
        literal: &mut [u64],
    ) -> Result<Option<scorer::Stats>> {
        let Some(mass) = &self.mass else {
            return Ok(None);
        };
        if plan.literal.is_none() || literal.len() != mass.live.len() {
            return Err(Error::Authority);
        }
        let mut lists = Vec::with_capacity(plan.terms.len());
        for (term, _) in &plan.terms {
            let Some(list) = self
                .terms
                .get(self, *blake3::hash(term.as_bytes()).as_bytes())?
            else {
                return Ok(None);
            };
            lists.push(list);
        }
        let mut view = mass.live.clone();
        if let Some(selected) = selected {
            if selected.bits.len() != view.len() {
                return Err(Error::Authority);
            }
            for (word, selected) in view.iter_mut().zip(&selected.bits) {
                *word &= selected;
            }
        }
        for (word, visible) in literal.iter_mut().zip(&view) {
            *word &= visible;
        }
        if let Some(driver) = plan.literal_driver() {
            let mut possible = vec![0u64; literal.len()];
            lists.get(driver).ok_or(Error::Authority)?.pairs.presence(
                &mut possible,
                &mass.live,
                selected.map(|selected| selected.bits.as_slice()),
            )?;
            for (word, possible) in literal.iter_mut().zip(possible) {
                *word &= possible;
            }
        }
        let collection = lists
            .iter()
            .map(|list| list.pairs.frequency(literal))
            .collect::<Result<Vec<_>>>()?;
        let frequencies = lists
            .iter()
            .map(|list| list.pairs.frequency(&view))
            .collect::<Result<Vec<_>>>()?;
        let total = mass.sum(literal, false)?;
        let view_total = selected.map_or(self.totals.length, |selected| selected.totals.length);
        Ok(Some(
            scorer::Stats::literal_counts(plan, collection, frequencies, total, view_total)
                .map_err(|_| Error::Bounds)?,
        ))
    }
    fn select(&self, selection: &Selection) -> Result<Selected> {
        let mut selected = Selected {
            bits: vec![0; (self.manifest.rows as usize).div_ceil(64)],
            count: 0,
            totals: Totals::default(),
        };
        for (block, rows) in self.columns.iter().enumerate() {
            for (offset, column) in rows.iter().enumerate() {
                if column.visible(selection) {
                    let ordinal = block * COLUMN_ROWS + offset;
                    selected.bits[ordinal / 64] |= 1 << (ordinal % 64);
                    selected.count += 1;
                    selected.totals.add(column)?;
                }
            }
        }
        Ok(selected)
    }
    fn walk(
        &self,
        plan: &scorer::Plan,
        direct: Option<Option<u64>>,
        candidates_only: bool,
        selected: Option<&[u64]>,
        mut visit: impl FnMut(u32, &[u32]) -> Result<()>,
    ) -> Result<()> {
        let mut tf = vec![0; plan.terms.len()];
        if let Some(row) = direct {
            if let Some(row) = row {
                visit(u32::try_from(row).map_err(|_| Error::Bounds)?, &tf)?
            }
            return Ok(());
        }
        if plan.terms.is_empty() && selected.is_none() {
            for row in 0..self.manifest.rows {
                visit(row as u32, &tf)?;
            }
            return Ok(());
        }
        let mut cursors = plan
            .terms
            .iter()
            .map(|(term, _)| Postings::new(self, term))
            .collect::<Result<Vec<_>>>()?;
        let mut heads = cursors
            .iter_mut()
            .map(Postings::next)
            .collect::<Result<Vec<_>>>()?;
        if let Some(selected) = selected {
            for (word, &bits) in selected.iter().enumerate() {
                let mut bits = bits;
                while bits != 0 {
                    let bit = bits.trailing_zeros();
                    bits &= bits - 1;
                    let ordinal =
                        u32::try_from(word * 64 + bit as usize).map_err(|_| Error::Bounds)?;
                    tf.fill(0);
                    for (i, head) in heads.iter_mut().enumerate() {
                        if head.is_some_and(|(row, _)| row < ordinal) {
                            *head = cursors[i].at_least(ordinal)?;
                        }
                        if head.is_some_and(|(row, _)| row == ordinal) {
                            tf[i] = head.unwrap().1;
                            *head = cursors[i].next()?;
                        }
                    }
                    visit(ordinal, &tf)?;
                }
            }
            return Ok(());
        }
        let anchored = candidates_only && plan.anchored_driver();
        while let Some(row) = heads
            .iter()
            .enumerate()
            .filter(|(i, _)| !anchored || plan.anchor_driver(*i))
            .filter_map(|(_, head)| *head)
            .map(|(row, _)| row)
            .min()
        {
            tf.fill(0);
            for (i, head) in heads.iter_mut().enumerate() {
                if anchored && !plan.anchor_driver(i) && head.is_some_and(|(at, _)| at < row) {
                    *head = cursors[i].at_least(row)?;
                }
                if head.is_some_and(|(r, _)| r == row) {
                    tf[i] = head.unwrap().1;
                    *head = cursors[i].next()?;
                }
            }
            visit(row, &tf)?;
        }
        Ok(())
    }
}

struct DiskPostings<'a> {
    cursor: tree::Cursor<'a>,
    rows: u64,
    format: u32,
    term: [u8; 32],
    buffer: Vec<(u32, u32)>,
    at: usize,
    last: Option<u32>,
    done: bool,
}
impl<'a> DiskPostings<'a> {
    fn new_hash(index: &'a Index, term: [u8; 32]) -> Result<Self> {
        Ok(Self {
            cursor: tree::Cursor::seek(
                index.context(tree::Purpose::ProjectionPostings),
                &index.manifest.postings,
                &term,
            )?,
            term,
            rows: index.manifest.rows,
            format: index.manifest.format,
            buffer: vec![],
            at: 0,
            last: None,
            done: false,
        })
    }
    fn next(&mut self) -> Result<Option<(u32, u32)>> {
        loop {
            if let Some(&(row, tf)) = self.buffer.get(self.at) {
                if self.last.is_some_and(|last| last >= row) {
                    return Err(Error::Authority);
                }
                self.last = Some(row);
                self.at += 1;
                return Ok(Some((row, tf)));
            }
            if self.done {
                return Ok(None);
            }
            let Some(entry) = self.cursor.next()? else {
                self.done = true;
                return Ok(None);
            };
            if !entry.key.starts_with(&self.term) {
                self.done = true;
                return Ok(None);
            }
            if entry.key.len() != 36 {
                return Err(Error::Authority);
            }
            self.buffer = decode_layout_postings(
                &entry.value,
                parse_u32(&entry.key[32..])?,
                self.rows,
                self.format,
            )?;
            self.at = 0;
            if self.buffer.first().map(|(r, _)| r.to_be_bytes().to_vec())
                != Some(entry.key[32..].to_vec())
            {
                return Err(Error::Authority);
            }
        }
    }
    fn at_least(&mut self, wanted: u32) -> Result<Option<(u32, u32)>> {
        loop {
            let skip = self.buffer[self.at..].partition_point(|(row, _)| *row < wanted);
            if skip != 0 {
                self.last = Some(self.buffer[self.at + skip - 1].0);
                self.at += skip;
            }
            let found = self.next()?;
            if found.is_none_or(|(row, _)| row >= wanted) {
                return Ok(found);
            }
        }
    }
}
enum Postings<'a> {
    Cached {
        list: Arc<term_cache::TermList>,
        block: usize,
        at: usize,
    },
    Disk(Box<DiskPostings<'a>>),
}
impl<'a> Postings<'a> {
    fn new(index: &'a Index, term: &str) -> Result<Self> {
        let term = *blake3::hash(term.as_bytes()).as_bytes();
        if let Some(list) = index.terms.get(index, term)? {
            Ok(Self::Cached {
                list,
                block: 0,
                at: 0,
            })
        } else {
            Ok(Self::Disk(Box::new(DiskPostings::new_hash(index, term)?)))
        }
    }
    fn next(&mut self) -> Result<Option<(u32, u32)>> {
        match self {
            Self::Disk(disk) => disk.next(),
            Self::Cached { list, block, at } => Ok(list.pairs.next(block, at)),
        }
    }
    fn at_least(&mut self, wanted: u32) -> Result<Option<(u32, u32)>> {
        match self {
            Self::Disk(disk) => disk.at_least(wanted),
            Self::Cached { list, block, at } => Ok(list.pairs.at_least(block, at, wanted)),
        }
    }
}
#[derive(Clone)]
struct Scored {
    ordinal: u32,
    label: u128,
    time: i64,
    bits: f64,
}
fn compare(a: &Scored, b: &Scored) -> std::cmp::Ordering {
    b.bits
        .total_cmp(&a.bits)
        .then_with(|| b.time.cmp(&a.time))
        .then_with(|| a.label.cmp(&b.label))
}
impl PartialEq for Scored {
    fn eq(&self, b: &Self) -> bool {
        compare(self, b).is_eq()
    }
}
impl Eq for Scored {}
impl PartialOrd for Scored {
    fn partial_cmp(&self, b: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(b))
    }
}
impl Ord for Scored {
    fn cmp(&self, b: &Self) -> std::cmp::Ordering {
        compare(self, b)
    }
}
struct Top {
    heap: BinaryHeap<Scored>,
    limit: usize,
}
impl Top {
    fn new(limit: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            limit,
        }
    }
    fn admits_bound(&self, bits: f64, time: i64, label: u128) -> bool {
        self.heap.len() < self.limit
            || self.heap.peek().is_none_or(|worst| {
                compare(
                    &Scored {
                        ordinal: 0,
                        bits,
                        time,
                        label,
                    },
                    worst,
                )
                .is_le()
            })
    }
    fn push(&mut self, value: Scored) {
        if self.limit == 0 {
            return;
        }
        if self.heap.len() < self.limit {
            self.heap.push(value)
        } else if self
            .heap
            .peek()
            .is_some_and(|worst| compare(&value, worst).is_lt())
        {
            self.heap.pop();
            self.heap.push(value)
        }
    }
    fn finish(self) -> Vec<Scored> {
        let mut v = self.heap.into_vec();
        v.sort_by(compare);
        v
    }
}

#[cfg(test)]
mod tests;
