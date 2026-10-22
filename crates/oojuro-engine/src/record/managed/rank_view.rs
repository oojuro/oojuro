//! Actual managed row/key authority and data-DEK capability, held through the callback.
use super::*;
const DERIVED_AAD: &[u8] = b"oojuro/managed/derived-cache/v1\0";
const MAX_DERIVED_BYTES: usize = 16 << 20;

/// Constructor is private: callers cannot provide an arbitrary claimed authority binding.
/// Records and plaintext returned earlier cannot be recalled by later logical denial.
pub struct ManagedRankView<'a> {
    binding: Binding,
    as_of: AsOf,
    filter: Filter,
    records: Vec<Record>,
    directory: PathBuf,
    cipher: &'a FrameCipher,
    stamp: [u8; 32],
}
impl ManagedLog {
    /// No query directory creation, training, writes, or fake RecordLog/Watermark.
    /// The retained control guard remains held through projection, callback and recheck.
    pub fn with_rank_view<T>(
        &self,
        scope: Scope<'_>,
        as_of: AsOf,
        filter: Filter,
        run: impl FnOnce(&ManagedRankView<'_>) -> Result<T>,
    ) -> Result<T> {
        if scope.writer != self.snapshot.identity.writer
            || scope.owner != self.snapshot.identity.agent
        {
            return Err(Error::Scope);
        }
        filter.validate()?;
        let owner = self.control.lock()?;
        self.current(&owner.state)?;
        let binding = binding(
            &owner.state,
            owner.checkpoint.floor().map_err(|_| Error::Authority)?,
        );
        let mut rows = crate::record::view::latest_rows(self.snapshot.rows.iter(), as_of);
        rows.retain(|row| {
            row.input.indexed && row.input.valid_at >= as_of.from && filter.keep(row)
        });
        rows.sort_by_key(|row| row.id);
        let records = rows
            .into_iter()
            .map(|row| {
                row.record(
                    &self.snapshot.identity.writer,
                    &self.snapshot.identity.adapter,
                )
            })
            .collect();
        // A keyed opaque stamp cannot become an offline guessing oracle for a private filter.
        let stamp = *blake3::keyed_hash(
            &self.snapshot.intent,
            &json(&("managed-rank-stamp-v1", &binding, as_of, &filter), 8192)?,
        )
        .as_bytes();
        let view = ManagedRankView {
            binding,
            as_of,
            filter,
            records,
            directory: self
                .directory
                .join(format!("d-{}", self.snapshot.generation)),
            cipher: &self.cipher,
            stamp,
        };
        let result = run(&view);
        owner.recheck()?;
        self.current(&owner.state)?;
        result
    }
}
impl ManagedRankView<'_> {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn as_of(&self) -> AsOf {
        self.as_of
    }
    pub fn filter(&self) -> &Filter {
        &self.filter
    }
    pub fn stamp(&self) -> [u8; 32] {
        self.stamp
    }
    /// Read/load/query path. It never creates a directory or file.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    /// Explicit table-build path only, while this genuine current guard is held.
    pub fn create_derived_directory(&self) -> Result<&Path> {
        directory_create(&self.directory)?;
        sync_dir(self.directory.parent().ok_or(Error::Invalid)?)?;
        Ok(&self.directory)
    }
    pub fn seal_derived(&self, purpose: &str, plain: &[u8]) -> Result<Vec<u8>> {
        if plain.len() > MAX_DERIVED_BYTES {
            return Err(Error::Bounds);
        }
        Ok(self.cipher.seal_dict(&self.aad(purpose)?, plain))
    }
    pub fn open_derived(&self, purpose: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() > MAX_DERIVED_BYTES + oojuro_store::crypto::SEAL_OVERHEAD {
            return Err(Error::Bounds);
        }
        self.cipher
            .open_dict(&self.aad(purpose)?, sealed)
            .map_err(|_| Error::Authority)
    }
    fn aad(&self, purpose: &str) -> Result<Vec<u8>> {
        if purpose.is_empty()
            || purpose.len() > 64
            || !purpose
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_/".contains(&b))
        {
            return Err(Error::Invalid);
        }
        let mut bytes = DERIVED_AAD.to_vec();
        bytes.extend(json(
            &(purpose, &self.binding, self.as_of, &self.filter),
            8192,
        )?);
        Ok(bytes)
    }
}
#[cfg(test)]
mod tests;
