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
/// One independent request in a bounded group commit. Order is caller order;
/// retries keep their first committed IDs/clocks even when regrouped.
#[derive(Clone, Copy)]
pub struct Append<'a> {
    pub request: &'a str,
    pub inputs: &'a [Input],
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
        self.append_group(&[Append { request, inputs }])?
            .pop()
            .ok_or(Error::Authority)
    }
    /// Share one synced Store append and one locator publication across requests.
    /// At most 1000 requests, 1000 input rows and 32 MiB of new encoded authority
    /// are accepted. This API does not wait to collect work; callers bound queue/wait.
    /// No request is acknowledged before every new frame and locator is durable.
    pub fn append_group(&mut self, requests: &[Append<'_>]) -> Result<Vec<Commit>> {
        let reader = &mut self.reader;
        reader.healthy()?;
        if requests.is_empty() || requests.len() > 1000 {
            return Err(Error::Bounds);
        }
        let mut input_count = 0usize;
        for part in requests {
            if !name(part.request, 1024) || part.inputs.is_empty() {
                return Err(Error::Bounds);
            }
            input_count = input_count
                .checked_add(part.inputs.len())
                .ok_or(Error::Bounds)?;
            if input_count > 1000 {
                return Err(Error::Bounds);
            }
            for input in part.inputs {
                input.validate()?;
            }
        }
        let first = reader.sequence.checked_add(1).ok_or(Error::Bounds)?;
        if reader.store.len() != first {
            return Err(Error::Authority);
        }
        let mut previous = reader.tip.clone();
        let mut sequence = first;
        let mut batches = Vec::new();
        let mut encoded = Vec::new();
        let mut encoded_bytes = 0usize;
        let mut receipts: BTreeMap<[u8; 32], ([u8; 32], Commit)> = BTreeMap::new();
        let mut output = Vec::with_capacity(requests.len());
        for part in requests {
            let request = reader.tag("request", part.request)?;
            let fingerprint = reader.tag("typed-input-v1", part.inputs)?;
            if let Some((old_fingerprint, old)) = receipts.get(&request) {
                if *old_fingerprint != fingerprint {
                    return Err(Error::Conflict);
                }
                output.push(old.clone());
                continue;
            }
            if let Some(at) = reader
                .disk
                .as_ref()
                .ok_or(Error::Invalid)?
                .request(reader, &request)?
            {
                let batch = reader.batch(usize::try_from(at).map_err(|_| Error::Bounds)?)?;
                if batch.request != request {
                    return Err(Error::Authority);
                }
                if batch.fingerprint != fingerprint {
                    return Err(Error::Conflict);
                }
                let receipt = commit(&batch);
                receipts.insert(request, (fingerprint, receipt.clone()));
                output.push(receipt);
                continue;
            }
            let count = previous
                .records
                .checked_add(part.inputs.len() as u64)
                .ok_or(Error::Bounds)?;
            if count > reader.capacity.records as u64 {
                return Err(Error::Bounds);
            }
            let mut id = reader
                .identity
                .first_id
                .checked_add(previous.records)
                .ok_or(Error::Bounds)?;
            let mut clock = previous.last_at;
            let now = (self.clock)()?;
            let mut assigned = Vec::with_capacity(part.inputs.len());
            for input in part.inputs {
                let recorded_at = match clock {
                    Some(last) => now.max(last.checked_add(1).ok_or(Error::Bounds)?),
                    None => now,
                };
                assigned.push(Assigned {
                    id,
                    recorded_at,
                    input: input.clone(),
                });
                id = id.checked_add(1).ok_or(Error::Bounds)?;
                clock = Some(recorded_at);
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
            let digest = authority::chain(previous.digest, &canonical);
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
            let bytes = json(&batch, MAX_BATCH_BYTES)?;
            encoded_bytes = encoded_bytes
                .checked_add(bytes.len())
                .ok_or(Error::Bounds)?;
            if encoded_bytes > MAX_BATCH_BYTES {
                return Err(Error::Bounds);
            }
            previous = Ref {
                records: count,
                last_at: clock,
                digest,
                request,
            };
            let receipt = commit(&batch);
            receipts.insert(request, (fingerprint, receipt.clone()));
            output.push(receipt);
            batches.push((batch, digest));
            encoded.push(bytes);
            sequence = sequence.checked_add(1).ok_or(Error::Bounds)?;
        }
        if batches.is_empty() {
            return Ok(output);
        }
        let bytes = reader
            .encoded_bytes
            .checked_add(encoded_bytes)
            .ok_or(Error::Bounds)?;
        if bytes > reader.capacity.source_bytes {
            return Err(Error::Bounds);
        }
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(authority::Phase::BeforeStore);
        }
        // All validation/capacity/conflict failures above leave authority unchanged.
        // From here an error is uncertain: reopen and retry each independent request.
        reader.failed = true;
        let carriers: Vec<_> = encoded
            .iter()
            .map(|bytes| authority::carrier(bytes))
            .collect();
        let actual = reader
            .store
            .append(&carriers)
            .map_err(|_| Error::Unavailable)?;
        if actual != first {
            return Err(Error::Authority);
        }
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(authority::Phase::AfterStore);
        }
        reader.publish_group(&batches)?;
        reader.encoded_bytes = bytes;
        reader.failed = false;
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(authority::Phase::BeforeAck);
        }
        Ok(output)
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
    /// Each returned receipt has durable authority and a current searchable index.
    /// An index error may follow committed requests; reopen and retry their identities.
    pub fn append_group_indexed(
        &mut self,
        index: &mut Index,
        requests: &[Append<'_>],
    ) -> Result<Vec<Commit>> {
        let receipts = self.append_group(requests)?;
        index.refresh(self.reader()?)?;
        Ok(receipts)
    }
}
fn commit(batch: &Batch) -> Commit {
    Commit {
        ids: batch.assigned.iter().map(|r| r.id).collect(),
        recorded_at: batch.assigned.iter().map(|r| r.recorded_at).collect(),
        watermark: batch.watermark.clone(),
    }
}
