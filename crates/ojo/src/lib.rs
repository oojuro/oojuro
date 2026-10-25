//! A small synchronous transaction log. See `spec/0.1/FORMAT.md` for its experimental wire format.
//! All successful commits include a file sync. Readers share a lock; a writer excludes all readers.
//! Values are opaque bytes: UTF-8 text and tool-call JSON require no special storage path.
#![doc = include_str!("../README.md")]
mod wire;
use chacha20poly1305::XChaCha20Poly1305;
use std::{
    collections::{BTreeSet, HashMap},
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};
use wire::{Body, FILE, FOOT, HEAD, hash, u32_at, u64_at};
const READ_BUFFER: usize = 64 << 10;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Locked,
    Invalid(&'static str),
    Corrupt(&'static str),
    Unsupported,
    KeyRequired,
    WrongKey,
    Downgrade,
    Conflict,
    ReadOnly,
    Poisoned,
    Limit,
    Rollback,
    Random,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<std::fs::TryLockError> for Error {
    fn from(error: std::fs::TryLockError) -> Self {
        match error {
            std::fs::TryLockError::WouldBlock => Self::Locked,
            std::fs::TryLockError::Error(error) => Self::Io(error),
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug)]
pub struct Change<'a> {
    pub key: &'a [u8],
    pub value: Option<&'a [u8]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub file_id: [u8; 16],
    pub sequence: u64,
    pub digest: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    pub checkpoint: Checkpoint,
    pub operations: u32,
}
#[derive(Clone, Copy)]
enum Location {
    Raw {
        offset: u64,
        length: usize,
        digest: [u8; 32],
    },
    Encoded {
        transaction: usize,
        value: Option<(usize, usize)>,
    },
}
struct Transaction {
    offset: u64,
    head: [u8; HEAD],
    digest: [u8; 32],
    // Derived retry bookkeeping. Open already validates the committed body/digest;
    // compute this second digest only if a caller actually retries that transaction.
    fingerprint: Option<[u8; 32]>,
}
#[derive(Clone, Copy)]
enum CommitMode {
    Durable,
    // Only for an unpublished archive temporary; archive_to syncs before publication.
    #[cfg(feature = "archive")]
    Archive,
}

/// Memory admission limits, independent of wire bounds. Exceeding them refuses an open/write.
/// The current implementation rebuilds its indexes on open; it has no persisted index or cache.
const MAX_OPERATIONS: usize = 10_000_000;
const MAX_KEY_BYTES: usize = 256 << 20;

pub struct Database {
    file: File,
    header: [u8; FILE],
    cipher: Option<XChaCha20Poly1305>,
    codecs: wire::Codecs,
    writable: bool,
    poisoned: bool,
    end: u64,
    tail: u64,
    transactions: Vec<Transaction>,
    latest: HashMap<Vec<u8>, Location>,
    // Built only for prefix traversal. The ordinary point-read path retains its
    // hash lookup and does not allocate this additional ordered key index.
    ordered_keys: Option<BTreeSet<Vec<u8>>>,
    requests: HashMap<[u8; 16], usize>,
    operations: usize,
    key_bytes: usize,
}

impl Drop for Database {
    fn drop(&mut self) {
        // Release explicitly: a concurrent fork/exec can briefly inherit the open file description.
        let _ = self.file.unlock();
    }
}

pub fn generate_key() -> Result<[u8; 32]> {
    wire::random()
}
pub fn request_id() -> Result<[u8; 16]> {
    loop {
        let id = wire::random()?;
        if id != [0; 16] {
            return Ok(id);
        }
    }
}

impl Database {
    /// Creates a private file exclusively and syncs both its header and parent directory.
    pub fn create(path: impl AsRef<Path>, key: Option<&[u8; 32]>) -> Result<Self> {
        let header = wire::new_header(key)?;
        let cipher = wire::cipher(&header, key)?;
        Self::create_header(path.as_ref(), header, cipher)
    }
    fn create_header(
        path: &Path,
        header: [u8; FILE],
        cipher: Option<XChaCha20Poly1305>,
    ) -> Result<Self> {
        let directory = File::open(parent(path))?;
        Self::initialize(new_file(path)?, directory, header, cipher)
    }
    fn initialize(
        mut file: File,
        directory: File,
        header: [u8; FILE],
        cipher: Option<XChaCha20Poly1305>,
    ) -> Result<Self> {
        file.try_lock().map_err(Error::from)?;
        file.write_all(&header)?;
        file.sync_all()?;
        directory.sync_all()?;
        Ok(Self::empty(file, header, cipher, true))
    }
    fn empty(
        file: File,
        header: [u8; FILE],
        cipher: Option<XChaCha20Poly1305>,
        writable: bool,
    ) -> Self {
        Self {
            file,
            header,
            cipher,
            codecs: wire::Codecs::default(),
            writable,
            poisoned: false,
            end: FILE as u64,
            tail: 0,
            transactions: Vec::new(),
            latest: HashMap::new(),
            ordered_keys: None,
            requests: HashMap::new(),
            operations: 0,
            key_bytes: 0,
        }
    }
    /// Locks the inode for this handle's lifetime. Read opens never modify the file.
    /// A writable open removes only a verified incomplete tail, after validating every complete transaction.
    pub fn open(path: impl AsRef<Path>, key: Option<&[u8; 32]>, writable: bool) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).write(writable).open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("regular file required"));
        }
        if writable {
            file.try_lock()
        } else {
            file.try_lock_shared()
        }
        .map_err(Error::from)?;
        let mut header = [0; FILE];
        read_exact(&mut file, &mut header)?;
        let cipher = wire::cipher(&header, key)?;
        let mut db = Self::empty(file, header, cipher, writable);
        let length = db.file.metadata()?.len();
        let mut reader = BufReader::with_capacity(0, &db.file);
        while db.end < length {
            if length - db.end < HEAD as u64 {
                break;
            }
            let mut head = [0; HEAD];
            read_exact(&mut reader, &mut head)?;
            wire::validate_head(&head, db.next_sequence()?, &db.checkpoint().digest)?;
            if db.transactions.is_empty() && u64_at(&head, 16) < (READ_BUFFER / 4) as u64 {
                reader = BufReader::with_capacity(READ_BUFFER, reader.into_inner());
            }
            let request = head[72..88].try_into().unwrap();
            if db.requests.contains_key(&request) {
                return Err(Error::Corrupt("duplicate request id"));
            }
            let size = HEAD as u64 + u64_at(&head, 16) + FOOT as u64;
            if size > length - db.end {
                break;
            }
            let (body, digest) = read_body(
                &mut reader,
                &db.header,
                db.cipher.as_ref(),
                &mut db.codecs,
                &head,
            )?;
            let mut key_bytes = 0;
            wire::walk(&body, u32_at(&head, 32), |_, key, _| key_bytes += key.len())?;
            db.admit(u32_at(&head, 32) as usize, key_bytes)?;
            let transaction = db.transactions.len();
            wire::walk(&body, u32_at(&head, 32), |operation, key, value| {
                let location = location(
                    transaction,
                    operation,
                    &head,
                    db.cipher.is_none(),
                    db.end,
                    &body,
                    value,
                );
                db.latest.insert(key.to_vec(), location);
            })?;
            db.requests.insert(request, transaction);
            db.transactions.push(Transaction {
                offset: db.end,
                head,
                digest,
                fingerprint: None,
            });
            db.operations += u32_at(&head, 32) as usize;
            db.key_bytes += key_bytes;
            db.end += size;
        }
        drop(reader);
        db.tail = length - db.end;
        if writable && db.tail > 0 {
            db.file.set_len(db.end)?;
            db.file.sync_all()?;
        }
        Ok(db)
    }
    /// Bytes in the incomplete tail found on open (including bytes removed by writable recovery).
    pub fn recovered_tail_bytes(&self) -> u64 {
        self.tail
    }
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            file_id: self.header[16..32].try_into().unwrap(),
            sequence: self.transactions.len() as u64,
            digest: self.transactions.last().map_or([0; 32], |tx| tx.digest),
        }
    }
    /// Refuses a different file identity, missing prefix, or changed committed prefix.
    pub fn require_checkpoint(&self, checkpoint: &Checkpoint) -> Result<()> {
        self.healthy()?;
        let digest = if checkpoint.sequence > self.transactions.len() as u64 {
            None
        } else if checkpoint.sequence == 0 {
            Some([0; 32])
        } else {
            self.transactions
                .get((checkpoint.sequence - 1) as usize)
                .map(|tx| tx.digest)
        };
        if checkpoint.file_id != self.checkpoint().file_id || digest != Some(checkpoint.digest) {
            return Err(Error::Rollback);
        }
        Ok(())
    }
    /// Atomically appends all changes, uncompressed. Reuse the SAME request id after an ambiguous error.
    /// An exact retry returns its original commit; a changed request under that id is a conflict.
    pub fn commit(&mut self, request: [u8; 16], changes: &[Change<'_>]) -> Result<Commit> {
        self.commit_encoded(request, changes, CommitMode::Durable)
    }
    fn commit_encoded(
        &mut self,
        request: [u8; 16],
        changes: &[Change<'_>],
        mode: CommitMode,
    ) -> Result<Commit> {
        let durable = matches!(mode, CommitMode::Durable);
        self.healthy()?;
        if !self.writable {
            return Err(Error::ReadOnly);
        }
        if request == [0; 16] || changes.is_empty() || changes.len() > wire::MAX_OPS {
            return Err(Error::Invalid("request id or operation count"));
        }
        let mut length = 0usize;
        let mut key_bytes = 0;
        for change in changes {
            let value = change.value.map_or(0, <[u8]>::len);
            if change.key.is_empty() || change.key.len() > wire::MAX_KEY || value > wire::MAX_VALUE
            {
                return Err(Error::Invalid("key or value length"));
            }
            length = length
                .checked_add(16 + change.key.len() + value)
                .ok_or(Error::Limit)?;
            key_bytes += change.key.len();
        }
        if length > wire::MAX_BODY {
            return Err(Error::Invalid("transaction length"));
        }
        let prefix = HEAD + if self.cipher.is_some() { 24 } else { 0 };
        let mut frame =
            Vec::with_capacity(HEAD + length + FOOT + if self.cipher.is_some() { 40 } else { 0 });
        frame.resize(prefix, 0);
        for change in changes {
            frame.extend_from_slice(&[u8::from(change.value.is_none()), 0, 0, 0]);
            frame.extend_from_slice(&(change.key.len() as u32).to_le_bytes());
            frame.extend_from_slice(&(change.value.map_or(0, <[u8]>::len) as u64).to_le_bytes());
            frame.extend_from_slice(change.key);
            frame.extend_from_slice(change.value.unwrap_or_default());
        }
        let fingerprint = hash(&[&frame[prefix..]]);
        if let Some(&transaction) = self.requests.get(&request) {
            let previous = match self.transactions[transaction].fingerprint {
                Some(previous) => previous,
                None => {
                    let previous = hash(&[&self.transaction_body(transaction)?]);
                    self.transactions[transaction].fingerprint = Some(previous);
                    previous
                }
            };
            if previous != fingerprint {
                return Err(Error::Conflict);
            }
            return Ok(self.receipt(transaction));
        }
        self.admit(changes.len(), key_bytes)?;
        let codec = if !durable {
            #[cfg(not(feature = "archive"))]
            return Err(Error::Unsupported);
            #[cfg(feature = "archive")]
            {
                let packed = self.codecs.compress(&frame[prefix..])?;
                if packed.len() < length {
                    frame.truncate(prefix);
                    frame.extend_from_slice(&packed);
                    1
                } else {
                    0
                }
            }
        } else {
            0
        };
        let mut head = [0; HEAD];
        head[..8].copy_from_slice(b"OJOTXN1\0");
        head[8..16].copy_from_slice(&self.next_sequence()?.to_le_bytes());
        let stored = frame.len() - HEAD + if self.cipher.is_some() { 16 } else { 0 };
        head[16..24].copy_from_slice(&(stored as u64).to_le_bytes());
        head[24..32].copy_from_slice(&(length as u64).to_le_bytes());
        head[32..36].copy_from_slice(&(changes.len() as u32).to_le_bytes());
        head[36] = codec;
        head[40..72].copy_from_slice(&self.checkpoint().digest);
        head[72..88].copy_from_slice(&request);
        wire::seal_crc(&mut head);
        frame[..HEAD].copy_from_slice(&head);
        if let Some(cipher) = &self.cipher {
            let tag = wire::encrypt(cipher, &self.header, &head, &mut frame[HEAD..])?;
            frame.extend_from_slice(&tag);
        }
        debug_assert_eq!(frame.len(), HEAD + stored);
        let digest = hash(&[&self.header, &frame]);
        frame.extend_from_slice(b"OJOCMT1\0");
        frame.extend_from_slice(&head[8..16]);
        frame.extend_from_slice(&digest);
        let end = self
            .end
            .checked_add(frame.len() as u64)
            .ok_or(Error::Limit)?;
        // Reads may move the cursor. Always seek to the committed end before appending.
        let append = (|| -> std::io::Result<()> {
            self.file.seek(SeekFrom::Start(self.end))?;
            self.file.write_all(&frame)?;
            if durable {
                self.file.sync_all()?;
            }
            Ok(())
        })();
        if let Err(error) = append {
            self.poisoned = true;
            return Err(error.into());
        }
        let transaction = self.transactions.len();
        let mut value_offset = self.end + HEAD as u64;
        for change in changes {
            value_offset += 16 + change.key.len() as u64;
            let location = if self.cipher.is_none() && codec == 0 {
                change.value.map(|value| Location::Raw {
                    offset: value_offset,
                    length: value.len(),
                    digest: hash(&[value]),
                })
            } else {
                None
            }
            .unwrap_or(Location::Encoded {
                transaction,
                value: change.value.map(|value| {
                    (
                        (value_offset - self.end - HEAD as u64) as usize,
                        value.len(),
                    )
                }),
            });
            self.latest.insert(change.key.to_vec(), location);
            if let Some(keys) = &mut self.ordered_keys {
                if change.value.is_none() {
                    keys.remove(change.key);
                } else if !keys.contains(change.key) {
                    keys.insert(change.key.to_vec());
                }
            }
            value_offset += change.value.map_or(0, <[u8]>::len) as u64;
        }
        self.requests.insert(request, transaction);
        self.transactions.push(Transaction {
            offset: self.end,
            head,
            digest,
            fingerprint: Some(fingerprint),
        });
        self.end = end;
        self.operations += changes.len();
        self.key_bytes += key_bytes;
        Ok(self.receipt(transaction))
    }
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.healthy()?;
        let Some(location) = self.latest.get(key).copied() else {
            return Ok(None);
        };
        if let Location::Raw {
            offset,
            length,
            digest,
        } = location
        {
            let mut value = vec![0; length];
            read_raw(&mut self.file, offset, length, digest, &mut value)?;
            return Ok(Some(value));
        }
        let Location::Encoded { transaction, value } = location else {
            unreachable!()
        };
        // Revalidate and decode the WHOLE containing transaction, including tombstone reads.
        // Its trusted digest binds the operation grammar and offsets checked at open/commit.
        let body = self.transaction_body(transaction)?;
        let Some((start, length)) = value else {
            return Ok(None);
        };
        let end = start
            .checked_add(length)
            .ok_or(Error::Corrupt("value range"))?;
        let value = body.get(start..end).ok_or(Error::Corrupt("value range"))?;
        Ok(Some(value.to_vec()))
    }

    /// Visits a bounded page of latest live values in bytewise key order.
    /// `after` is an exclusive key cursor; `limit` must be 1..=1,024. The returned
    /// cursor is the last visited key, only when another matching key exists.
    /// Empty values are visited; deleted keys and older versions are omitted.
    ///
    /// The first call builds an optional ordered index of live keys in memory;
    /// subsequent pages use a range lookup and decode only selected values.
    /// This copies key metadata, not the complete journal or its values. Raw
    /// selected values retain their SHA-256 check. Encoded values verify and
    /// decode their containing transaction. At most one decoded transaction is
    /// retained within the page, and it is dropped before returning.
    ///
    /// A callback error stops immediately and is returned unchanged. The file
    /// lock remains attached to this handle; drop it before network operations
    /// when a writer must proceed. Reopening still validates the whole history.
    pub fn scan_prefix(
        &mut self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<()>,
    ) -> Result<Option<Vec<u8>>> {
        use std::ops::Bound::{Excluded, Included, Unbounded};
        self.healthy()?;
        if !(1..=wire::MAX_OPS).contains(&limit)
            || prefix.len() > wire::MAX_KEY
            || after.is_some_and(|key| key.len() > wire::MAX_KEY)
        {
            return Err(Error::Invalid("prefix page bounds"));
        }
        let mut upper = prefix.to_vec();
        while upper.last() == Some(&255) {
            upper.pop();
        }
        let upper = if let Some(last) = upper.last_mut() {
            *last += 1;
            Some(upper)
        } else {
            None
        };
        if upper
            .as_deref()
            .is_some_and(|end| after.is_some_and(|key| key >= end))
        {
            return Ok(None);
        }
        let keys = self.ordered_keys.get_or_insert_with(|| {
            self.latest
                .iter()
                .filter(|(_, location)| !matches!(location, Location::Encoded { value: None, .. }))
                .map(|(key, _)| key.clone())
                .collect()
        });
        let start = match after {
            Some(key) if key >= prefix => Excluded(key),
            _ => Included(prefix),
        };
        let end = upper.as_deref().map_or(Unbounded, Excluded);
        let mut selected = keys
            .range::<[u8], _>((start, end))
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let continuation = if selected.len() > limit {
            selected.pop();
            selected.last().cloned()
        } else {
            None
        };
        let mut raw = Vec::new();
        let mut decoded: Option<(usize, Body)> = None;
        for key in selected {
            match self.latest[&key] {
                Location::Raw {
                    offset,
                    length,
                    digest,
                } => {
                    read_raw(&mut self.file, offset, length, digest, &mut raw)?;
                    visit(&key, &raw)?;
                }
                Location::Encoded {
                    transaction,
                    value: Some((start, length)),
                } => {
                    if decoded
                        .as_ref()
                        .is_none_or(|(index, _)| *index != transaction)
                    {
                        decoded = Some((transaction, self.transaction_body(transaction)?));
                    }
                    let end = start
                        .checked_add(length)
                        .ok_or(Error::Corrupt("value range"))?;
                    let value = decoded
                        .as_ref()
                        .unwrap()
                        .1
                        .get(start..end)
                        .ok_or(Error::Corrupt("value range"))?;
                    visit(&key, value)?;
                }
                Location::Encoded { value: None, .. } => unreachable!("ordered live keys"),
            }
        }
        Ok(continuation)
    }

    /// Streams every committed version in order. Values are borrowed for the duration of the callback.
    /// A tombstone is `None`; an empty value is `Some(&[])`. This is a logical delete, not physical erasure.
    pub fn scan(&mut self, mut visit: impl FnMut(u64, u32, &[u8], Option<&[u8]>)) -> Result<()> {
        self.healthy()?;
        self.file.seek(SeekFrom::Start(FILE as u64))?;
        let mean_frame = (self.end - FILE as u64) / self.transactions.len().max(1) as u64;
        let capacity = if mean_frame < (READ_BUFFER / 4) as u64 {
            READ_BUFFER
        } else {
            0
        };
        let mut reader = BufReader::with_capacity(capacity, &self.file);
        for (index, tx) in self.transactions.iter().enumerate() {
            let body = read_transaction(
                &mut reader,
                &self.header,
                self.cipher.as_ref(),
                &mut self.codecs,
                tx,
            )?;
            wire::walk(
                &body,
                u32_at(&self.transactions[index].head, 32),
                |operation, key, value| {
                    visit(index as u64 + 1, operation, key, value);
                },
            )?;
        }
        Ok(())
    }
    fn healthy(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
    fn next_sequence(&self) -> Result<u64> {
        (self.transactions.len() as u64)
            .checked_add(1)
            .ok_or(Error::Limit)
    }
    fn admit(&self, operations: usize, key_bytes: usize) -> Result<()> {
        if operations > MAX_OPERATIONS - self.operations
            || key_bytes > MAX_KEY_BYTES - self.key_bytes
        {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }
    fn receipt(&self, index: usize) -> Commit {
        let tx = &self.transactions[index];
        Commit {
            checkpoint: Checkpoint {
                file_id: self.checkpoint().file_id,
                sequence: index as u64 + 1,
                digest: tx.digest,
            },
            operations: u32_at(&tx.head, 32),
        }
    }
    fn transaction_body(&mut self, index: usize) -> Result<Body> {
        let tx = &self.transactions[index];
        self.file.seek(SeekFrom::Start(tx.offset))?;
        read_transaction(
            &mut &self.file,
            &self.header,
            self.cipher.as_ref(),
            &mut self.codecs,
            tx,
        )
    }
}
fn read_transaction(
    reader: &mut impl Read,
    header: &[u8; FILE],
    cipher: Option<&XChaCha20Poly1305>,
    codecs: &mut wire::Codecs,
    tx: &Transaction,
) -> Result<Body> {
    let mut head = [0; HEAD];
    read_exact(reader, &mut head)?;
    if head != tx.head {
        return Err(Error::Corrupt("changed transaction header"));
    }
    let (body, digest) = read_body(reader, header, cipher, codecs, &head)?;
    if digest != tx.digest {
        return Err(Error::Corrupt("changed transaction"));
    }
    Ok(body)
}
fn read_body(
    reader: &mut impl Read,
    header: &[u8; FILE],
    cipher: Option<&XChaCha20Poly1305>,
    codecs: &mut wire::Codecs,
    head: &[u8; HEAD],
) -> Result<(Body, [u8; 32])> {
    let length = u64_at(head, 16) as usize;
    let mut stored = vec![0; length + FOOT];
    read_exact(reader, &mut stored)?;
    let footer = &stored[length..];
    let digest = hash(&[header, head, &stored[..length]]);
    if &footer[..8] != b"OJOCMT1\0" || footer[8..16] != head[8..16] || footer[16..] != digest {
        return Err(Error::Corrupt("commit footer or digest"));
    }
    stored.truncate(length);
    Ok((wire::decode(codecs, cipher, header, head, stored)?, digest))
}

fn location(
    transaction: usize,
    _operation: u32,
    head: &[u8; HEAD],
    plaintext: bool,
    offset: u64,
    body: &[u8],
    value: Option<&[u8]>,
) -> Location {
    if plaintext
        && head[36] == 0
        && let Some(value) = value
    {
        // Both slices come from the same validated body; no wire pointer or native offset is persisted.
        return Location::Raw {
            offset: offset
                + HEAD as u64
                + (value.as_ptr() as usize - body.as_ptr() as usize) as u64,
            length: value.len(),
            digest: hash(&[value]),
        };
    }
    Location::Encoded {
        transaction,
        value: value.map(|value| {
            (
                value.as_ptr() as usize - body.as_ptr() as usize,
                value.len(),
            )
        }),
    }
}
fn read_error(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::Corrupt("incomplete committed bytes")
    } else {
        Error::Io(error)
    }
}
fn read_raw(
    file: &mut File,
    offset: u64,
    length: usize,
    digest: [u8; 32],
    value: &mut Vec<u8>,
) -> Result<()> {
    value.resize(length, 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(value, offset).map_err(read_error)?;
    }
    #[cfg(not(unix))]
    {
        file.seek(SeekFrom::Start(offset))?;
        read_exact(file, value)?;
    }
    if hash(&[value]) != digest {
        return Err(Error::Corrupt("changed value"));
    }
    Ok(())
}
fn read_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<()> {
    reader.read_exact(bytes).map_err(read_error)
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
fn new_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

#[cfg(feature = "archive")]
impl Database {
    /// Creates a separate compressed .ojo file, preserving all transactions, request IDs and encryption.
    /// The source is untouched. The archive has a fresh identity, so its physical checkpoints differ.
    /// Publication is atomic and never replaces an existing destination; a failed build removes its temporary file.
    pub fn archive_to(&mut self, destination: impl AsRef<Path>) -> Result<Checkpoint> {
        self.healthy()?;
        if self.file.metadata()?.len() != self.end {
            return Err(Error::Invalid("recover incomplete source before archiving"));
        }
        let destination = destination.as_ref();
        let directory = File::open(parent(destination))?;
        let temporary = parent(destination).join(format!(
            ".ojo-{:032x}.tmp",
            u128::from_le_bytes(wire::random()?)
        ));
        let mut header = self.header;
        header[16..32].copy_from_slice(&wire::random::<16>()?);
        wire::seal_crc(&mut header);
        // Claim ownership before entering the cleanup path. A pre-existing name is never removed.
        let file = new_file(&temporary)?;
        let result = (|| {
            let mut archive = Self::initialize(
                file,
                File::open(parent(&temporary))?,
                header,
                self.cipher.clone(),
            )?;
            for index in 0..self.transactions.len() {
                let body = self.transaction_body(index)?;
                let head = self.transactions[index].head;
                let mut changes = Vec::with_capacity(u32_at(&head, 32) as usize);
                wire::walk(&body, u32_at(&head, 32), |_, key, value| {
                    changes.push(Change { key, value })
                })?;
                archive.commit_encoded(
                    head[72..88].try_into().unwrap(),
                    &changes,
                    CommitMode::Archive,
                )?;
            }
            // No archive transaction was acknowledged externally. Sync every byte before
            // publishing the separate file; ordinary public commits still sync individually.
            archive.file.sync_all()?;
            // Link creates the final name without a replacement race.
            std::fs::hard_link(&temporary, destination)?;
            directory.sync_all()?;
            Ok(archive.checkpoint())
        })();
        // Only our randomly named temporary is ever removed; originals and existing destinations survive errors.
        std::fs::remove_file(&temporary)?;
        directory.sync_all()?;
        result
    }
}
