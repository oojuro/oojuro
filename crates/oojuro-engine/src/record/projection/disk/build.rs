use super::*;
use oojuro_store::crypto::SEAL_OVERHEAD;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufReader, BufWriter, Read, Write},
};
mod extend;
pub use extend::Appender;

const SPOOL: &str = "BUILD_ROWS";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Analysis {
    metadata: Metadata,
    terms: Vec<(String, u32)>,
}

/// Streaming construction of a disposable projection. `push` retains neither
/// complete rows nor source text. A failed push poisons this builder; no new
/// descriptor is published until every page and original fingerprint is ready.
pub struct Builder {
    root: PathBuf,
    directory: PathBuf,
    generation: String,
    binding: Binding,
    authority: Authority,
    cipher: Arc<FrameCipher>,
    limits: Limits,
    format: u32,
    blobs: Blobs,
    spool: BufWriter<File>,
    spool_bytes: u64,
    rows: u64,
    source_bytes: u64,
    length_bits: u32,
    scope_bits: u32,
    stems: HashMap<String, String>,
    failed: bool,
    finished: bool,
    _lock: File,
}
impl Drop for Builder {
    fn drop(&mut self) {
        let _ = self._lock.unlock();
        if !self.finished {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}
impl Builder {
    pub fn create(
        directory: impl AsRef<Path>,
        cache_key: &[u8; 32],
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        Self::create_with_cipher(
            directory.as_ref(),
            Arc::new(FrameCipher::new(cache_key)),
            binding,
            limits,
            1,
        )
    }
    /// Opt-in version 2 derived cache. Existing caches are not migrated.
    pub fn create_compact(
        directory: impl AsRef<Path>,
        cache_key: &[u8; 32],
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        Self::create_with_cipher(
            directory.as_ref(),
            Arc::new(FrameCipher::new(cache_key)),
            binding,
            limits,
            COMPACT_FORMAT,
        )
    }
    /// Opt-in version 3 disposable cache with exact raw-run positions. Existing
    /// caches and all source wire/profile formats are unchanged.
    pub fn create_positional(
        directory: impl AsRef<Path>,
        cache_key: &[u8; 32],
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        Self::create_with_cipher(
            directory.as_ref(),
            Arc::new(FrameCipher::new(cache_key)),
            binding,
            limits,
            POSITIONAL_FORMAT,
        )
    }
    fn create_with_cipher(
        directory: &Path,
        cipher: Arc<FrameCipher>,
        binding: Binding,
        limits: Limits,
        format: u32,
    ) -> Result<Self> {
        limits.validate()?;
        let mut authority = Authority::for_layout(&binding, format)?;
        let root = directory.to_path_buf();
        let (lock, work, generation) = prepare(&root)?;
        authority.budget = Some(Arc::new(budget::Budget::new(
            &root,
            &work,
            limits.index_bytes,
        )));
        authority.reserve(&work.join("LOCK"), 0)?;
        authority.reserve(&root.join("BUILD_LOCK"), 0)?;
        authority.reserve(&work.join(SPOOL), 0)?;
        let spool = BufWriter::with_capacity(64 << 10, new_file(&work.join(SPOOL))?);
        let blobs = Blobs::new(
            work.clone(),
            cipher.clone(),
            authority.clone(),
            limits.compression,
            vec![],
        );
        Ok(Self {
            root,
            directory: work,
            generation,
            binding,
            authority,
            cipher,
            limits,
            format,
            blobs,
            spool,
            spool_bytes: 0,
            rows: 0,
            source_bytes: 0,
            length_bits: 0,
            scope_bits: 0,
            stems: HashMap::new(),
            failed: false,
            finished: false,
            _lock: lock,
        })
    }
    pub fn push(&mut self, row: Row) -> Result<()> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        let result = self.push_inner(row);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_inner(&mut self, row: Row) -> Result<()> {
        if !name(&row.id, 1024)
            || row.key.as_ref().is_some_and(|k| !name(k, 1024))
            || row.text.len() > MAX_TEXT_BYTES
            || self.rows >= self.limits.rows
            || self.rows as usize >= self.limits.metadata_bytes / std::mem::size_of::<Column>()
        {
            return Err(Error::Bounds);
        }
        let bytes = self
            .source_bytes
            .checked_add(row.text.len() as u64)
            .ok_or(Error::Bounds)?;
        if bytes > self.limits.source_bytes {
            return Err(Error::Bounds);
        }
        let terms = if row.indexed && !row.text.is_empty() {
            lexical::analyze_with_stems(&row.text, &mut self.stems).map_err(|_| Error::Bounds)?
        } else {
            vec![]
        };
        let length = u32::try_from(terms.iter().map(|(_, n)| u64::from(*n)).sum::<u64>())
            .map_err(|_| Error::Bounds)?;
        let scope = u32::try_from(terms.len()).map_err(|_| Error::Bounds)?;
        if compact_format(self.format) {
            let rows = self.rows as usize + 1;
            let blocks = rows.div_ceil(COLUMN_ROWS);
            let columns = rows
                .checked_mul(std::mem::size_of::<Column>())
                .and_then(|bytes| {
                    bytes.checked_add(
                        blocks
                            * (std::mem::size_of::<Arc<[Column]>>()
                                + 2 * std::mem::size_of::<usize>()),
                    )
                })
                .ok_or(Error::Bounds)?;
            let mass =
                compact::Mass::required(rows, self.length_bits | length, self.scope_bits | scope)?;
            if columns
                .checked_add(mass)
                .is_none_or(|bytes| bytes > self.limits.metadata_bytes)
            {
                return Err(Error::Bounds);
            }
        }
        let text = self.blobs.put(TEXT, row.text.as_bytes())?;
        let metadata = Metadata::from_row(row, text, length, scope);
        // Refuse oversized individual metadata before it reaches a tree page.
        encode_metadata(&metadata, self.limits.compression, self.format)?;
        let plain = json(&Analysis { metadata, terms }, MAX_BATCH_BYTES)?;
        let sealed = self
            .cipher
            .seal_frame(&spool_aad(&self.authority, self.rows), &plain);
        self.spool_bytes = self
            .spool_bytes
            .checked_add(4 + sealed.len() as u64)
            .ok_or(Error::Bounds)?;
        self.authority
            .reserve(&self.directory.join(SPOOL), self.spool_bytes)?;
        self.spool
            .write_all(&(sealed.len() as u32).to_le_bytes())
            .and_then(|_| self.spool.write_all(&sealed))
            .map_err(|_| Error::Unavailable)?;
        self.rows += 1;
        self.source_bytes = bytes;
        self.length_bits |= length;
        self.scope_bits |= scope;
        Ok(())
    }
    pub fn finish(mut self) -> Result<Index> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        self.flush_spool()?;
        self.stems.clear();
        self.stems.shrink_to_fit();
        self.authority.budget.as_ref().unwrap().observe()?;
        let authority = self.authority.clone();
        let cipher = self.cipher.clone();
        let directory = self.directory.clone();
        let make = |purpose| context(&directory, &cipher, &authority, purpose, None);
        let mut ids = tree::Sorter::new(make(tree::Purpose::ProjectionIds))?;
        let mut keys = tree::Sorter::new(make(tree::Purpose::ProjectionKeys))?;
        let mut posting_rows = tree::Sorter::new(make(tree::Purpose::ProjectionTerms))?;
        let mut position_rows = (self.format == POSITIONAL_FORMAT)
            .then(|| tree::Sorter::new(make(tree::Purpose::ProjectionTerms)))
            .transpose()?;
        self.read_rows(|ordinal, analysis| {
            if let Some(positions) = &mut position_rows
                && analysis.metadata.indexed
            {
                let bytes = read_blob(
                    &directory,
                    &cipher,
                    &authority,
                    &self.blobs.files,
                    &analysis.metadata.text,
                    MAX_TEXT_BYTES,
                )?;
                if *blake3::hash(&bytes).as_bytes() != analysis.metadata.text_digest {
                    return Err(Error::Authority);
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| Error::Authority)?;
                literal::push(positions, ordinal, text)?;
            }
            ids.push(tree::Entry {
                key: analysis.metadata.id.as_bytes().to_vec(),
                value: ordinal.to_be_bytes().to_vec(),
            })?;
            if let Some(key) = &analysis.metadata.key {
                let mut bytes = blake3::hash(key.as_bytes()).as_bytes().to_vec();
                bytes.extend(ordinal.to_be_bytes());
                keys.push(tree::Entry::new(bytes, vec![])?)?;
            }
            for (term, tf) in analysis.terms {
                let mut key = blake3::hash(term.as_bytes()).as_bytes().to_vec();
                key.extend(ordinal.to_be_bytes());
                posting_rows.push(tree::Entry::new(key, tf.to_le_bytes().to_vec())?)?;
            }
            Ok(())
        })?;
        let temporary_ids = ids.finish()?;
        let keys = keys.finish()?;
        let flat_postings = posting_rows.finish()?;
        let positions = if let Some(rows) = position_rows {
            let flat = rows.finish()?;
            let mut output = tree::Builder::new(make(tree::Purpose::ProjectionPostings))?;
            literal::fold_sorted(make(tree::Purpose::ProjectionTerms), &flat, |entry| {
                output.push(entry)
            })?;
            let root = output.finish()?;
            remove_tree(&directory, &authority, &flat)?;
            Some(root)
        } else {
            None
        };
        if temporary_ids.count != self.rows {
            return Err(Error::Conflict);
        }
        let mut labels = vec![0u128; self.rows as usize];
        let step = u128::MAX / (u128::from(self.rows) + 1);
        let mut input =
            tree::Cursor::seek(make(tree::Purpose::ProjectionIds), &temporary_ids, b"")?;
        let mut ids = tree::Builder::new(make(tree::Purpose::ProjectionIds))?;
        let mut at = 0u128;
        while let Some(entry) = input.next()? {
            let ordinal = parse_u32(&entry.value)?;
            if ordinal as u64 >= self.rows || labels[ordinal as usize] != 0 {
                return Err(Error::Authority);
            }
            at += 1;
            let label = step.checked_mul(at).ok_or(Error::Bounds)?;
            labels[ordinal as usize] = label;
            let mut value = ordinal.to_be_bytes().to_vec();
            value.extend(label.to_le_bytes());
            ids.push(tree::Entry {
                key: entry.key,
                value,
            })?;
        }
        if at != u128::from(self.rows) {
            return Err(Error::Authority);
        }
        let ids = ids.finish()?;
        let mut input =
            tree::Cursor::seek(make(tree::Purpose::ProjectionTerms), &flat_postings, b"")?;
        let mut postings = tree::Builder::new(make(tree::Purpose::ProjectionPostings))?;
        let mut term = None::<[u8; 32]>;
        let mut list = Vec::new();
        let emit = |postings: &mut tree::Builder<'_>,
                    term: [u8; 32],
                    list: &mut Vec<(u32, u32)>|
         -> Result<()> {
            if list.is_empty() {
                return Ok(());
            }
            let mut key = term.to_vec();
            key.extend(list[0].0.to_be_bytes());
            postings.push(tree::Entry::new(
                key,
                encode_layout_postings(list, self.limits.compression, self.format)?,
            )?)?;
            list.clear();
            Ok(())
        };
        while let Some(entry) = input.next()? {
            if entry.key.len() != 36 || entry.value.len() != 4 {
                return Err(Error::Authority);
            }
            let next: [u8; 32] = entry.key[..32].try_into().unwrap();
            if term.is_some_and(|t| t != next)
                || list.len() == 256
                || (compact_format(self.format)
                    && list.first().is_some_and(|(row, _)| {
                        row / 256 != parse_u32(&entry.key[32..]).unwrap_or(u32::MAX) / 256
                    }))
            {
                emit(&mut postings, term.unwrap(), &mut list)?;
            }
            term = Some(next);
            let row = parse_u32(&entry.key[32..])?;
            let tf = u32::from_le_bytes(entry.value[..].try_into().unwrap());
            if row as u64 >= self.rows || tf == 0 {
                return Err(Error::Authority);
            }
            list.push((row, tf));
        }
        if let Some(term) = term {
            emit(&mut postings, term, &mut list)?;
        }
        let postings = postings.finish()?;
        let mut metadata = tree::Builder::new(make(tree::Purpose::ProjectionMetadata))?;
        let mut columns = Vec::new();
        let mut block = Vec::with_capacity(COLUMN_ROWS);
        let mut reader = RowReader::new(&directory, &cipher, &authority, self.rows)?;
        while let Some((ordinal, analysis)) = reader.next()? {
            metadata.push(tree::Entry::new(
                ordinal.to_be_bytes().to_vec(),
                encode_metadata(&analysis.metadata, self.limits.compression, self.format)?,
            )?)?;
            block.push(analysis.metadata.column(labels[ordinal as usize]));
            if block.len() == COLUMN_ROWS {
                flush_columns(&mut self.blobs, &mut columns, &mut block)?;
            }
        }
        flush_columns(&mut self.blobs, &mut columns, &mut block)?;
        let metadata = metadata.finish()?;
        self.blobs.finish()?;
        self.authority.budget.as_ref().unwrap().observe()?;
        remove_tree(&directory, &authority, &temporary_ids)?;
        remove_tree(&directory, &authority, &flat_postings)?;
        tree::Authority::remove_file(&authority, &directory.join(SPOOL))?;
        if directory_bytes(&directory)? > self.limits.index_bytes {
            return Err(Error::Bounds);
        }
        let mut manifest = Manifest {
            format: self.format,
            generation: self.generation.clone(),
            binding: self.authority.binding.clone(),
            rows: self.rows,
            live_rows: self.rows,
            source_bytes: self.source_bytes,
            limits: self.limits,
            build_metrics: self.authority.budget.as_ref().unwrap().observe()?,
            files: self.blobs.files.clone(),
            columns,
            metadata,
            ids,
            keys,
            postings,
            positions,
        };
        // Publish while the budget paths still name the owned build directory;
        // only metadata publication follows its atomic generation rename.
        let previous = self.directory.clone();
        self.directory = finish_directory(&self.root, &self.directory, &self.generation)?;
        self.authority
            .budget
            .as_ref()
            .unwrap()
            .relocate(&previous, &self.directory)?;
        // Once publication can name this complete generation, an ambiguous
        // descriptor sync failure must not make Drop unlink its files.
        self.finished = true;
        publish(&self.root, &cipher, &self.authority, &mut manifest)?;
        storage::open_with_cipher(&self.root, cipher, &self.binding, self.limits)
    }
    fn read_rows(&self, mut visit: impl FnMut(u32, Analysis) -> Result<()>) -> Result<()> {
        let mut reader = RowReader::new(&self.directory, &self.cipher, &self.authority, self.rows)?;
        while let Some((ordinal, row)) = reader.next()? {
            visit(ordinal, row)?;
        }
        Ok(())
    }
    fn flush_spool(&mut self) -> Result<()> {
        self.spool.flush().map_err(|_| Error::Unavailable)?;
        // BUILD_ROWS is temporary and removed before publication. Drain large
        // cold-build spools; tiny refreshes need only become readable locally.
        if self.spool_bytes > 1 << 20 {
            self.spool
                .get_ref()
                .sync_all()
                .map_err(|_| Error::Unavailable)?;
        }
        Ok(())
    }
}
fn flush_columns(
    blobs: &mut Blobs,
    columns: &mut Vec<Blob>,
    block: &mut Vec<Column>,
) -> Result<()> {
    if !block.is_empty() {
        let mut bytes = Vec::with_capacity(block.len() * COLUMN_BYTES);
        for row in block.iter() {
            row.encode(&mut bytes);
        }
        columns.push(blobs.put(COLUMNS, &bytes)?);
        block.clear();
    }
    Ok(())
}
fn spool_aad(authority: &Authority, ordinal: u64) -> Vec<u8> {
    let mut bytes = b"oojuro/projection/build-rows/v1\0".to_vec();
    bytes.extend(&authority.binding);
    bytes.extend(ordinal.to_le_bytes());
    bytes
}
struct RowReader<'a> {
    reader: BufReader<File>,
    cipher: &'a FrameCipher,
    authority: &'a Authority,
    rows: u64,
    at: u64,
}
impl<'a> RowReader<'a> {
    fn new(
        directory: &Path,
        cipher: &'a FrameCipher,
        authority: &'a Authority,
        rows: u64,
    ) -> Result<Self> {
        Ok(Self {
            reader: BufReader::with_capacity(64 << 10, open_file(&directory.join(SPOOL))?),
            cipher,
            authority,
            rows,
            at: 0,
        })
    }
    fn next(&mut self) -> Result<Option<(u32, Analysis)>> {
        if self.at == self.rows {
            let mut extra = [0];
            if self
                .reader
                .read(&mut extra)
                .map_err(|_| Error::Unavailable)?
                != 0
            {
                return Err(Error::Authority);
            }
            return Ok(None);
        }
        let mut length = [0; 4];
        self.reader
            .read_exact(&mut length)
            .map_err(|_| Error::Unavailable)?;
        let length = u32::from_le_bytes(length) as usize;
        if !(SEAL_OVERHEAD..=MAX_BATCH_BYTES + SEAL_OVERHEAD).contains(&length) {
            return Err(Error::Bounds);
        }
        let mut sealed = vec![0; length];
        self.reader
            .read_exact(&mut sealed)
            .map_err(|_| Error::Unavailable)?;
        let plain = self
            .cipher
            .open_frame(&spool_aad(self.authority, self.at), &sealed)
            .map_err(|_| Error::Authority)?;
        let row = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
        let at = self.at as u32;
        self.at += 1;
        Ok(Some((at, row)))
    }
}
fn remove_tree(directory: &Path, authority: &Authority, root: &tree::Root) -> Result<()> {
    if let Some(node) = &root.node {
        let generation = root.fresh.ok_or(Error::Authority)?;
        if generation != node.generation {
            return Err(Error::Authority);
        }
        for file in 0..=node.file {
            tree::Authority::remove_file(
                authority,
                &directory.join(format!("pack-{}-{file:016x}.bin", hex(&generation))),
            )?;
        }
    }
    Ok(())
}
