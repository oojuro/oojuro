use super::*;
use std::os::unix::fs::MetadataExt;

/// Copy-on-write refresh of a caller-validated unchanged source prefix. This
/// is an explicit caller assertion, not proof of source history or authority.
/// New immutable source IDs are appended; callers retire prior resolved winners.
pub struct Appender {
    builder: Builder,
    previous: Manifest,
    previous_mass: Option<compact::Mass>,
    columns: Vec<Arc<[Column]>>,
    changed: BTreeSet<usize>,
    pages: Arc<tree::PageCache>,
    retired: u64,
    totals: Totals,
}
impl Appender {
    pub fn extend_validated_prefix(
        previous: &Index,
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        if binding.namespace != previous.binding.namespace
            || binding.writer != previous.binding.writer
            || binding.view == previous.binding.view
        {
            return Err(Error::Scope);
        }
        limits.validate()?;
        if previous.manifest.rows > limits.rows
            || previous.manifest.rows as usize
                > limits.metadata_bytes / std::mem::size_of::<Column>()
            || previous.manifest.source_bytes > limits.source_bytes
        {
            return Err(Error::Bounds);
        }
        if let Some(mass) = &previous.mass
            && mass.bytes() > mass_limit(&previous.columns, limits.metadata_bytes)?
        {
            return Err(Error::Bounds);
        }
        let mut builder = Builder::create_with_cipher(
            &previous.root_directory,
            previous.cipher.clone(),
            binding,
            limits,
            previous.manifest.format,
        )?;
        // Immutable pack names are cryptographically random and their bytes
        // remain authenticated by roots/blob references. Hard links avoid a
        // population copy while keeping each generation independently owned.
        for entry in fs::read_dir(&previous.directory).map_err(|_| Error::Unavailable)? {
            let entry = entry.map_err(|_| Error::Unavailable)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| Error::Unavailable)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() == 0 {
                return Err(Error::Invalid);
            }
            if entry.file_name() == "LOCK" {
                continue;
            }
            let target = builder.directory.join(entry.file_name());
            builder.authority.reserve(&target, metadata.len())?;
            fs::hard_link(entry.path(), target).map_err(|_| Error::Unavailable)?;
        }
        builder.blobs.files = previous.manifest.files.clone();
        Ok(Self {
            builder,
            previous: previous.manifest.clone(),
            previous_mass: previous.mass.clone(),
            columns: previous.columns.clone(),
            changed: BTreeSet::new(),
            pages: previous.pages.clone(),
            retired: 0,
            totals: previous.totals,
        })
    }
    pub fn push(&mut self, row: Row) -> Result<()> {
        let limits = self.builder.limits;
        if self
            .previous
            .rows
            .checked_add(self.builder.rows)
            .is_none_or(|n| {
                n >= limits.rows
                    || n as usize >= limits.metadata_bytes / std::mem::size_of::<Column>()
            })
            || self
                .previous
                .source_bytes
                .checked_add(self.builder.source_bytes)
                .and_then(|v| v.checked_add(row.text.len() as u64))
                .is_none_or(|v| v > limits.source_bytes)
        {
            self.builder.failed = true;
            return Err(Error::Bounds);
        }
        self.builder.push(row)
    }
    /// Retires one previously supplied live immutable ID. It does not delete or
    /// rewrite any source original. A missing/already retired ID is refused.
    pub fn retire(&mut self, id: &str) -> Result<()> {
        let result = self.retire_inner(id);
        if result.is_err() {
            self.builder.failed = true;
        }
        result
    }
    fn retire_inner(&mut self, id: &str) -> Result<()> {
        if self.builder.failed {
            return Err(Error::Unavailable);
        }
        let ctx = context(
            &self.builder.directory,
            &self.builder.cipher,
            &self.builder.authority,
            tree::Purpose::ProjectionIds,
            Some(&self.pages),
        );
        let value =
            tree::get_ancestor(ctx, &self.previous.ids, id.as_bytes())?.ok_or(Error::Conflict)?;
        let (ordinal, _) = id_value(&value)?;
        let block = ordinal as usize / COLUMN_ROWS;
        let column = Arc::make_mut(self.columns.get_mut(block).ok_or(Error::Authority)?)
            .get_mut(ordinal as usize % COLUMN_ROWS)
            .ok_or(Error::Authority)?;
        if !column.live() {
            return Err(Error::Conflict);
        }
        self.totals.remove(column)?;
        column.flags &= !2;
        self.changed.insert(block);
        self.retired += 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<Index> {
        let b = &mut self.builder;
        if b.failed {
            return Err(Error::Unavailable);
        }
        b.flush_spool()?;
        b.stems.clear();
        b.stems.shrink_to_fit();
        b.authority.budget.as_ref().unwrap().observe()?;
        let authority = b.authority.clone();
        let cipher = b.cipher.clone();
        let directory = b.directory.clone();
        let make = |purpose| context(&directory, &cipher, &authority, purpose, Some(&self.pages));
        let mut sorted_ids = tree::Sorter::new(make(tree::Purpose::ProjectionIds))?;
        let mut sorted_postings = tree::Sorter::new(make(tree::Purpose::ProjectionTerms))?;
        let mut sorted_positions = (b.format == POSITIONAL_FORMAT)
            .then(|| tree::Sorter::new(make(tree::Purpose::ProjectionTerms)))
            .transpose()?;
        let mut keys = tree::Editor::new(
            make(tree::Purpose::ProjectionKeys),
            self.previous.keys.clone(),
        )?;
        let mut metadata = tree::Editor::new(
            make(tree::Purpose::ProjectionMetadata),
            self.previous.metadata.clone(),
        )?;
        let mut ids = tree::Editor::new(
            make(tree::Purpose::ProjectionIds),
            self.previous.ids.clone(),
        )?;
        let mut postings = tree::Editor::new(
            make(tree::Purpose::ProjectionPostings),
            self.previous.postings.clone(),
        )?;
        let mut key_batch = Pending::new();
        let mut reader = RowReader::new(&directory, &cipher, &authority, b.rows)?;
        while let Some((offset, analysis)) = reader.next()? {
            let ordinal =
                u32::try_from(self.previous.rows + u64::from(offset)).map_err(|_| Error::Bounds)?;
            if let Some(positions) = &mut sorted_positions {
                if analysis.metadata.indexed {
                    let bytes = read_blob(
                        &directory,
                        &cipher,
                        &authority,
                        &b.blobs.files,
                        &analysis.metadata.text,
                        MAX_TEXT_BYTES,
                    )?;
                    if *blake3::hash(&bytes).as_bytes() != analysis.metadata.text_digest {
                        return Err(Error::Authority);
                    }
                    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Authority)?;
                    literal::push(positions, ordinal, text)?;
                }
            }
            sorted_ids.push(tree::Entry {
                key: analysis.metadata.id.as_bytes().to_vec(),
                value: ordinal.to_be_bytes().to_vec(),
            })?;
            if let Some(key) = &analysis.metadata.key {
                let mut bytes = blake3::hash(key.as_bytes()).as_bytes().to_vec();
                bytes.extend(ordinal.to_be_bytes());
                key_batch.push(&mut keys, tree::Entry::new(bytes, vec![])?)?;
            }
            for (term, tf) in analysis.terms {
                let mut key = blake3::hash(term.as_bytes()).as_bytes().to_vec();
                key.extend(ordinal.to_be_bytes());
                sorted_postings.push(tree::Entry::new(key, tf.to_le_bytes().to_vec())?)?;
            }
        }
        key_batch.flush(&mut keys)?;
        let temporary_ids = sorted_ids.finish()?;
        let flat_postings = sorted_postings.finish()?;
        let positions = if let Some(rows) = sorted_positions {
            let flat = rows.finish()?;
            let mut output = tree::Editor::new(
                make(tree::Purpose::ProjectionPostings),
                self.previous.positions.clone().ok_or(Error::Authority)?,
            )?;
            let mut pending = Pending::new();
            literal::fold_sorted(make(tree::Purpose::ProjectionTerms), &flat, |entry| {
                pending.push(&mut output, entry)
            })?;
            pending.flush(&mut output)?;
            let root = output.finish()?;
            remove_tree(&directory, &authority, &flat)?;
            Some(root)
        } else {
            None
        };
        if temporary_ids.count != b.rows {
            return Err(Error::Conflict);
        }
        let mut labels = vec![0u128; b.rows as usize];
        let mut id_batch = Pending::new();
        let mut input =
            tree::Cursor::seek(make(tree::Purpose::ProjectionIds), &temporary_ids, b"")?;
        while let Some(entry) = input.next()? {
            let ordinal = parse_u32(&entry.value)?;
            let offset = (u64::from(ordinal))
                .checked_sub(self.previous.rows)
                .ok_or(Error::Authority)? as usize;
            let (before, after) = ids.neighbors(&entry.key)?;
            let tail = after.is_none();
            let mut low = before.map_or(Ok(0), |e| id_value(&e.value).map(|(_, label)| label))?;
            if let Some(previous) = id_batch.entries.last() {
                low = low.max(id_value(&previous.value)?.1);
            }
            let high = after.map_or(Ok(u128::MAX), |e| {
                id_value(&e.value).map(|(_, label)| label)
            })?;
            let gap = high.checked_sub(low).ok_or(Error::Authority)?;
            let step = gap / (u128::from(b.rows) + 1);
            let step = if tail { step.min(1u128 << 64) } else { step };
            let label = low.checked_add(step).ok_or(Error::Bounds)?;
            if label == low || label == high {
                return Err(Error::Bounds);
            } // caller performs full rebuild to relabel
            *labels.get_mut(offset).ok_or(Error::Authority)? = label;
            let mut value = ordinal.to_be_bytes().to_vec();
            value.extend(label.to_le_bytes());
            id_batch.push(
                &mut ids,
                tree::Entry {
                    key: entry.key,
                    value,
                },
            )?;
        }
        id_batch.flush(&mut ids)?;
        let mut input =
            tree::Cursor::seek(make(tree::Purpose::ProjectionTerms), &flat_postings, b"")?;
        let mut term = None::<[u8; 32]>;
        let mut list = Vec::new();
        let mut posting_batch = Pending::new();
        let mut emit = |term: [u8; 32], list: &mut Vec<(u32, u32)>| -> Result<()> {
            if list.is_empty() {
                return Ok(());
            }
            let mut key = term.to_vec();
            key.extend(list[0].0.to_be_bytes());
            posting_batch.push(
                &mut postings,
                tree::Entry::new(
                    key,
                    encode_layout_postings(list, b.limits.compression, b.format)?,
                )?,
            )?;
            list.clear();
            Ok(())
        };
        while let Some(entry) = input.next()? {
            if entry.key.len() != 36 || entry.value.len() != 4 {
                return Err(Error::Authority);
            }
            let next: [u8; 32] = entry.key[..32].try_into().unwrap();
            if term.is_some_and(|term| term != next)
                || list.len() == 256
                || (compact_format(b.format)
                    && list.first().is_some_and(|(row, _)| {
                        row / 256 != parse_u32(&entry.key[32..]).unwrap_or(u32::MAX) / 256
                    }))
            {
                emit(term.unwrap(), &mut list)?;
            }
            term = Some(next);
            let ordinal = parse_u32(&entry.key[32..])?;
            let tf = u32::from_le_bytes(entry.value[..].try_into().unwrap());
            if u64::from(ordinal) < self.previous.rows
                || u64::from(ordinal) >= self.previous.rows + b.rows
                || tf == 0
            {
                return Err(Error::Authority);
            }
            list.push((ordinal, tf));
        }
        if let Some(term) = term {
            emit(term, &mut list)?;
        }
        posting_batch.flush(&mut postings)?;
        let mut reader = RowReader::new(&directory, &cipher, &authority, b.rows)?;
        let mut tail = if !(self.previous.rows as usize).is_multiple_of(COLUMN_ROWS) {
            self.columns.pop().ok_or(Error::Authority)?.to_vec()
        } else {
            Vec::with_capacity(COLUMN_ROWS)
        };
        let mut metadata_batch = Pending::new();
        while let Some((offset, analysis)) = reader.next()? {
            let ordinal =
                u32::try_from(self.previous.rows + u64::from(offset)).map_err(|_| Error::Bounds)?;
            let entry = tree::Entry::new(
                ordinal.to_be_bytes().to_vec(),
                encode_metadata(&analysis.metadata, b.limits.compression, b.format)?,
            )?;
            metadata_batch.push(&mut metadata, entry)?;
            let block = ordinal as usize / COLUMN_ROWS;
            let column = analysis.metadata.column(labels[offset as usize]);
            self.totals.add(&column)?;
            tail.push(column);
            if tail.len() == COLUMN_ROWS {
                self.columns.push(Arc::from(std::mem::take(&mut tail)));
                tail = Vec::with_capacity(COLUMN_ROWS);
            }
            self.changed.insert(block);
        }
        metadata_batch.flush(&mut metadata)?;
        if !tail.is_empty() {
            self.columns.push(Arc::from(tail));
        }
        let mass = if compact_format(b.format) {
            Some(self.previous_mass.take().ok_or(Error::Authority)?.refresh(
                &self.columns,
                &self.changed,
                mass_limit(&self.columns, b.limits.metadata_bytes)?,
            )?)
        } else {
            None
        };
        let mut columns = self.previous.columns.clone();
        for block in self.changed {
            let mut bytes = Vec::with_capacity(self.columns[block].len() * COLUMN_BYTES);
            for col in self.columns[block].iter() {
                col.encode(&mut bytes);
            }
            let reference = b.blobs.put(COLUMNS, &bytes)?;
            if block == columns.len() {
                columns.push(reference);
            } else {
                columns[block] = reference;
            }
        }
        let metadata = metadata.finish()?;
        let ids = ids.finish()?;
        let keys = keys.finish()?;
        let postings = postings.finish()?;
        b.blobs.finish()?;
        b.authority.budget.as_ref().unwrap().observe()?;
        remove_tree(&directory, &authority, &temporary_ids)?;
        remove_tree(&directory, &authority, &flat_postings)?;
        tree::Authority::remove_file(&authority, &directory.join(SPOOL))?;
        let mut manifest = Manifest {
            format: b.format,
            generation: b.generation.clone(),
            binding: authority.binding.clone(),
            rows: self.previous.rows + b.rows,
            live_rows: self.previous.live_rows - self.retired + b.rows,
            source_bytes: self.previous.source_bytes + b.source_bytes,
            limits: b.limits,
            build_metrics: authority.budget.as_ref().unwrap().observe()?,
            files: b.blobs.files.clone(),
            columns,
            metadata,
            ids,
            keys,
            postings,
            positions,
        };
        let previous_directory = b.directory.clone();
        b.directory = finish_directory(&b.root, &b.directory, &b.generation)?;
        authority
            .budget
            .as_ref()
            .unwrap()
            .relocate(&previous_directory, &b.directory)?;
        b.finished = true;
        publish(&b.root, &cipher, &authority, &mut manifest)?;
        let lock = open_file(&b.directory.join("LOCK"))?;
        lock.try_lock_shared().map_err(|_| Error::Unavailable)?;
        let retained_disk_bytes = directory_bytes(&b.directory)?
            .checked_add(
                fs::metadata(b.root.join("PROJECTION_CURRENT"))
                    .map_err(|_| Error::Unavailable)?
                    .len(),
            )
            .ok_or(Error::Bounds)?;
        let retained_allocated_bytes = allocated_directory_bytes(&b.directory)?
            .checked_add(
                fs::metadata(&b.root)
                    .map_err(|_| Error::Unavailable)?
                    .blocks()
                    * 512,
            )
            .and_then(|bytes| {
                fs::metadata(b.root.join("PROJECTION_CURRENT"))
                    .ok()
                    .and_then(|m| bytes.checked_add(m.blocks() * 512))
            })
            .ok_or(Error::Unavailable)?;
        let mut authority = authority;
        authority.budget = None;
        Ok(Index {
            directory: b.directory.clone(),
            root_directory: b.root.clone(),
            binding: b.binding.clone(),
            authority,
            cipher,
            pages: self.pages,
            manifest,
            columns: self.columns,
            mass,
            totals: self.totals,
            queries: query_cache::QueryCache::default(),
            terms: term_cache::TermCache::default(),
            retained_disk_bytes,
            retained_allocated_bytes,
            _lock: lock,
        })
    }
}
fn id_value(bytes: &[u8]) -> Result<(u32, u128)> {
    if bytes.len() != 20 {
        return Err(Error::Authority);
    }
    Ok((
        parse_u32(&bytes[..4])?,
        u128::from_le_bytes(bytes[4..].try_into().unwrap()),
    ))
}

// A constant-size edit buffer; no source population is retained here.
struct Pending {
    entries: Vec<tree::Entry>,
    bytes: usize,
}
impl Pending {
    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(tree::MAX_BATCH_ENTRIES),
            bytes: 0,
        }
    }
    fn push(&mut self, editor: &mut tree::Editor<'_>, entry: tree::Entry) -> Result<()> {
        let bytes = entry.key.len() + entry.value.len() + 8;
        if self.entries.len() == tree::MAX_BATCH_ENTRIES
            || self.bytes + bytes > tree::MAX_BATCH_BYTES
        {
            self.flush(editor)?;
        }
        if bytes > tree::MAX_BATCH_BYTES {
            return Err(Error::Bounds);
        }
        self.bytes += bytes;
        self.entries.push(entry);
        Ok(())
    }
    fn flush(&mut self, editor: &mut tree::Editor<'_>) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        self.entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        editor.insert_batch(std::mem::replace(
            &mut self.entries,
            Vec::with_capacity(tree::MAX_BATCH_ENTRIES),
        ))?;
        self.bytes = 0;
        Ok(())
    }
}
