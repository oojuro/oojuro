//! Durable Record2 appends with disk-backed receipts and locators.
use super::*;

/// Explicit out-of-core Record2 writer. Existing `RecordLog` admission limits stay unchanged.
/// A successful append includes a synced authority frame and published locator directory.
/// On an uncertain write error, drop this writer, reopen, and retry the same request and inputs.
pub struct Writer {
    reader: Reader,
    clock: Arc<dyn Fn() -> Result<i64> + Send + Sync>,
    #[cfg(test)]
    pub(in crate::record::paged) hook: Option<Box<dyn Fn(authority::Phase)>>,
}
impl Writer {
    pub fn open_disk(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        directory: &Path,
        minimum: Option<&Watermark>,
        limits: DiskLimits,
    ) -> Result<Self> {
        Self::open_clock(
            dir,
            key,
            identity,
            directory,
            minimum,
            limits,
            Arc::new(authority::now),
        )
    }
    pub(in crate::record::paged) fn open_clock(
        dir: &Path,
        key: &[u8; 32],
        identity: Identity,
        directory: &Path,
        minimum: Option<&Watermark>,
        limits: DiskLimits,
        clock: Arc<dyn Fn() -> Result<i64> + Send + Sync>,
    ) -> Result<Self> {
        let reader = Reader::open_disk_inner(dir, key, identity, directory, minimum, limits, true)?;
        let parent = dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            reader,
            clock,
            #[cfg(test)]
            hook: None,
        })
    }
    /// Borrowing prevents concurrent mutation; failed append state is never exposed for reads.
    pub fn reader(&self) -> Result<&Reader> {
        self.reader.healthy()?;
        Ok(&self.reader)
    }
    /// Rebuild locator trees and retire their obsolete COW packs. This is
    /// explicit maintenance, separately timed from append acknowledgements.
    pub fn compact_directory(&mut self) -> Result<()> {
        self.reader.healthy()?;
        self.reader.compact_directory()
    }
    /// Durable authority append. Call `Index::refresh` before using an older search index.
    pub fn append(&mut self, request: &str, inputs: &[Input]) -> Result<Commit> {
        let reader = &mut self.reader;
        reader.healthy()?;
        if !name(request, 1024) || inputs.is_empty() || inputs.len() > 1000 {
            return Err(Error::Bounds);
        }
        for input in inputs {
            input.validate()?;
        }
        let request = reader.tag("request", request)?;
        let fingerprint = reader.tag("typed-input-v1", inputs)?;
        if let Some(sequence) = reader
            .disk
            .as_ref()
            .ok_or(Error::Invalid)?
            .request(reader, &request)?
        {
            let batch = reader.batch(usize::try_from(sequence).map_err(|_| Error::Bounds)?)?;
            if batch.request != request {
                return Err(Error::Authority);
            }
            return if batch.fingerprint == fingerprint {
                Ok(commit(&batch))
            } else {
                Err(Error::Conflict)
            };
        }
        let count = reader
            .tip
            .records
            .checked_add(inputs.len() as u64)
            .ok_or(Error::Bounds)?;
        if count > reader.capacity.records as u64 {
            return Err(Error::Bounds);
        }
        let mut id = reader
            .identity
            .first_id
            .checked_add(reader.tip.records)
            .ok_or(Error::Bounds)?;
        let mut previous = reader.tip.last_at;
        let now = (self.clock)()?;
        let mut assigned = Vec::with_capacity(inputs.len());
        for input in inputs {
            let recorded_at = match previous {
                Some(last) => now.max(last.checked_add(1).ok_or(Error::Bounds)?),
                None => now,
            };
            assigned.push(Assigned {
                id,
                recorded_at,
                input: input.clone(),
            });
            id = id.checked_add(1).ok_or(Error::Bounds)?;
            previous = Some(recorded_at);
        }
        let sequence = reader.sequence.checked_add(1).ok_or(Error::Bounds)?;
        if reader.store.len() != sequence {
            return Err(Error::Authority);
        }
        let canonical = json(
            &Unsigned {
                schema: 2,
                request,
                fingerprint,
                assigned: &assigned,
            },
            MAX_BATCH_BYTES,
        )?;
        let digest = authority::chain(reader.tip.digest, &canonical);
        let mut watermark = Watermark {
            schema: 1,
            origin: reader.origin,
            identity: reader.identity_tag,
            sequence,
            records: count,
            commitment: vec![],
        };
        watermark.commitment = reader
            .cipher
            .seal_frame(&authority::aad(&watermark)?, &digest);
        let batch = Batch {
            schema: 2,
            request,
            fingerprint,
            assigned,
            watermark,
        };
        let encoded = json(&batch, MAX_BATCH_BYTES)?;
        let bytes = reader
            .encoded_bytes
            .checked_add(encoded.len())
            .ok_or(Error::Bounds)?;
        if bytes > reader.capacity.source_bytes {
            return Err(Error::Bounds);
        }
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(authority::Phase::BeforeStore);
        }
        // From here onward an error is uncertain; only reopen/retry may recover a receipt.
        reader.failed = true;
        let actual = reader
            .store
            .append(&[authority::carrier(&encoded)])
            .map_err(|_| Error::Unavailable)?;
        if actual != sequence {
            return Err(Error::Authority);
        }
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(authority::Phase::AfterStore);
        }
        reader.publish_batch(&batch, digest)?;
        reader.encoded_bytes = bytes;
        reader.failed = false;
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(authority::Phase::BeforeAck);
        }
        Ok(commit(&batch))
    }
    /// Success means both a durable receipt and a current lexical index. Cache failure
    /// can follow a committed append: reopen the cache and retry the same request.
    pub fn append_indexed(
        &mut self,
        index: &mut Index,
        request: &str,
        inputs: &[Input],
    ) -> Result<Commit> {
        let commit = self.append(request, inputs)?;
        index.refresh(self.reader()?)?;
        Ok(commit)
    }
}
fn commit(batch: &Batch) -> Commit {
    Commit {
        ids: batch.assigned.iter().map(|r| r.id).collect(),
        recorded_at: batch.assigned.iter().map(|r| r.recorded_at).collect(),
        watermark: batch.watermark.clone(),
    }
}
