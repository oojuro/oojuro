//! General timestamp-and-bytes journals, using the separately versioned Event profile 1.
//!
//! The logical record is an engine acceptance time (signed Unix microseconds,
//! UTC) and exact bytes. Cursors and retry intents are engine bookkeeping. A
//! journal has no required application keys or provider/session/agent fields.
//! This profile is additive: ordinary wire 0.1 key/value files are refused.

use crate::{Change, Database, Error, Replay, Result, request_id, wire};
use std::{
    collections::BTreeSet,
    fs::File,
    ops::Bound::{Excluded, Included, Unbounded},
    path::Path,
    sync::{Condvar, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PROFILE_KEY: &[u8] = b"\0ojo.events/1";
const PROFILE_MAGIC: &[u8; 8] = b"OJOEVT1\0";
const EVENT_PREFIX: &[u8; 4] = b"\0evt";
const INTENT_MAGIC: &[u8; 8] = b"OJOINT1\0";
const MAX_PAGE: usize = 1024;
const MAX_COLLECTED_BYTES: usize = 64 << 20;

/// Opaque append-order position. Stable across exact export and compression;
/// unrelated journals and divergent appends do not share positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cursor([u8; 40]);

impl Cursor {
    pub fn to_bytes(self) -> [u8; 40] {
        self.0
    }
    pub fn from_bytes(bytes: [u8; 40]) -> Result<Self> {
        if bytes[..16] == [0; 16] || bytes[16..24] == [0; 8] || bytes[24..] == [0; 16] {
            return Err(Error::Invalid("event cursor"));
        }
        Ok(Self(bytes))
    }
    fn ordinal(self) -> u64 {
        u64::from_be_bytes(self.0[16..24].try_into().unwrap())
    }
    fn request(self) -> [u8; 16] {
        self.0[24..].try_into().unwrap()
    }
}

/// The durable acknowledgement. Time is independent of append order: clocks
/// can repeat or move backward. Identical retries return this original receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub cursor: Cursor,
    pub accepted_at: i64,
}

/// Exact original plus retrieval position. Only time and data are logical fields.
#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub accepted_at: i64,
    pub data: Vec<u8>,
    pub cursor: Cursor,
}

/// Persist this engine-issued intent alongside the exact data before submitting
/// a write when an acknowledgement might be lost. It binds the accepted bytes,
/// original engine time and request nonce, without requiring a caller key.
#[derive(Clone, Copy)]
pub struct AppendIntent {
    journal: [u8; 16],
    request: [u8; 16],
    accepted_at: i64,
    data_hash: [u8; 32],
}

impl AppendIntent {
    pub fn to_bytes(self) -> [u8; 80] {
        let mut bytes = [0; 80];
        bytes[..8].copy_from_slice(INTENT_MAGIC);
        bytes[8..24].copy_from_slice(&self.journal);
        bytes[24..40].copy_from_slice(&self.request);
        bytes[40..48].copy_from_slice(&self.accepted_at.to_be_bytes());
        bytes[48..].copy_from_slice(&self.data_hash);
        bytes
    }
    /// Restores a retained intent; this is not a trusted timestamp attestation.
    /// Keep intent bytes private with the original payload and source identity.
    pub fn from_bytes(bytes: [u8; 80]) -> Result<Self> {
        if &bytes[..8] != INTENT_MAGIC || bytes[8..24] == [0; 16] || bytes[24..40] == [0; 16] {
            return Err(Error::Invalid("event append intent"));
        }
        Ok(Self {
            journal: bytes[8..24].try_into().unwrap(),
            request: bytes[24..40].try_into().unwrap(),
            accepted_at: i64::from_be_bytes(bytes[40..48].try_into().unwrap()),
            data_hash: bytes[48..].try_into().unwrap(),
        })
    }
}

/// Resume after `cursor`, including when caught up; `has_more` describes only
/// the snapshot read by this call. A later append can make another page visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub cursor: Option<Cursor>,
    pub has_more: bool,
}

#[derive(Clone, Copy)]
struct Metadata {
    accepted_at: i64,
    request: [u8; 16],
    length: usize,
}

pub struct Journal {
    db: Database,
    id: [u8; 16],
    events: Vec<Metadata>,
    time_order: Option<BTreeSet<(i64, u64)>>,
}

impl Journal {
    /// Publishes a complete empty Event journal without replacing a destination.
    /// The file header, profile transaction and containing directory are synced.
    pub fn create(path: impl AsRef<Path>, key: Option<&[u8; 32]>) -> Result<Self> {
        let destination = path.as_ref();
        let directory = File::open(crate::parent(destination))?;
        let temporary = crate::parent(destination).join(format!(
            ".ojo-event-{:032x}.tmp",
            u128::from_le_bytes(request_id()?)
        ));
        let header = wire::new_header(key)?;
        let cipher = wire::cipher(&header, key)?;
        let id = request_id()?;
        let mut marker = [0; 24];
        marker[..8].copy_from_slice(PROFILE_MAGIC);
        marker[8..].copy_from_slice(&id);
        // Claim the temporary before entering its cleanup path.
        let file = crate::new_file(&temporary)?;
        let result = (|| {
            let mut db = Database::initialize(
                file,
                File::open(crate::parent(destination))?,
                header,
                cipher,
            )?;
            db.commit(
                id,
                &[Change {
                    key: PROFILE_KEY,
                    value: Some(&marker),
                }],
            )?;
            std::fs::hard_link(&temporary, destination)?;
            directory.sync_all()?;
            Ok(Self {
                db,
                id,
                events: Vec::new(),
                time_order: None,
            })
        })();
        std::fs::remove_file(&temporary)?;
        directory.sync_all()?;
        result
    }

    /// Validates the Event grammar during the existing wire replay, without a
    /// second scan. A profile mismatch refuses before writable tail recovery.
    pub fn open(path: impl AsRef<Path>, key: Option<&[u8; 32]>, writable: bool) -> Result<Self> {
        let mut id = None;
        let mut events = Vec::new();
        let db = Database::open_observed(path.as_ref(), key, writable, |item| {
            match item {
                Replay::Finish => {
                    if id.is_none() {
                        return Err(Error::Invalid("Event profile required"));
                    }
                }
                Replay::Transaction {
                    sequence,
                    request,
                    operations,
                    body,
                } => {
                    if operations != 1 {
                        return Err(Error::Invalid("Event transaction shape"));
                    }
                    let mut parsed = Err(Error::Invalid("Event transaction"));
                    wire::walk(body, operations, |_, key, value| {
                        parsed = if sequence == 1 {
                            match value {
                                Some(value)
                                    if key == PROFILE_KEY
                                        && value.len() == 24
                                        && &value[..8] == PROFILE_MAGIC
                                        && value[8..] != [0; 16] =>
                                {
                                    id = Some(value[8..].try_into().unwrap());
                                    Ok(None)
                                }
                                _ => Err(Error::Invalid("Event profile required")),
                            }
                        } else {
                            parse_key(key, sequence - 1, request).and_then(|mut meta| {
                                let data = value.ok_or(Error::Invalid("Event deletion"))?;
                                meta.length = data.len();
                                Ok(Some(meta))
                            })
                        };
                    })?;
                    if let Some(metadata) = parsed? {
                        events.push(metadata);
                    }
                }
            }
            Ok(())
        })?;
        Ok(Self {
            db,
            id: id.unwrap(),
            events,
            time_order: None,
        })
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn recovered_tail_bytes(&self) -> u64 {
        self.db.recovered_tail_bytes()
    }
    pub fn checkpoint(&self) -> crate::Checkpoint {
        self.db.checkpoint()
    }
    pub fn require_checkpoint(&self, checkpoint: &crate::Checkpoint) -> Result<()> {
        self.db.require_checkpoint(checkpoint)
    }

    /// The simplest write: engine time, nonce and cursor; synced before success.
    /// Distinct calls append distinct events even when their bytes are identical.
    /// Use prepare/append_prepared when a retained retry identity is needed.
    pub fn append(&mut self, data: &[u8]) -> Result<Receipt> {
        self.writable(data)?;
        self.accept(self.fresh_request()?, now()?, data)
    }

    /// Accepts an immutable write intent using the engine clock. Preparation is
    /// not a durable acknowledgement. Retain this intent before submitting it.
    pub fn prepare(&self, data: &[u8]) -> Result<AppendIntent> {
        self.writable(data)?;
        Ok(AppendIntent {
            journal: self.id,
            request: self.fresh_request()?,
            accepted_at: now()?,
            data_hash: wire::hash(&[data]),
        })
    }

    pub fn append_prepared(&mut self, intent: &AppendIntent, data: &[u8]) -> Result<Receipt> {
        self.writable(data)?;
        if intent.journal != self.id || wire::hash(&[data]) != intent.data_hash {
            return Err(Error::Conflict);
        }
        self.accept(intent.request, intent.accepted_at, data)
    }

    fn writable(&self, data: &[u8]) -> Result<()> {
        self.db.healthy()?;
        if !self.db.writable {
            return Err(Error::ReadOnly);
        }
        if data.len() > wire::MAX_VALUE {
            return Err(Error::Invalid("event data length"));
        }
        Ok(())
    }

    fn fresh_request(&self) -> Result<[u8; 16]> {
        loop {
            let nonce = request_id()?;
            if !self.db.requests.contains_key(&nonce) {
                return Ok(nonce);
            }
        }
    }

    fn accept(&mut self, request: [u8; 16], accepted_at: i64, data: &[u8]) -> Result<Receipt> {
        let previous = self.db.requests.get(&request).copied();
        let ordinal = match previous {
            Some(0) => return Err(Error::Conflict),
            Some(index) => {
                if self.events[index - 1].accepted_at != accepted_at {
                    return Err(Error::Conflict);
                }
                index as u64
            }
            None => self.events.len() as u64 + 1,
        };
        let metadata = Metadata {
            accepted_at,
            request,
            length: data.len(),
        };
        let key = event_key(ordinal, metadata);
        self.db.commit(
            request,
            &[Change {
                key: &key,
                value: Some(data),
            }],
        )?;
        if previous.is_none() {
            self.events.push(metadata);
            if let Some(order) = &mut self.time_order {
                order.insert((accepted_at, ordinal));
            }
        }
        Ok(Receipt {
            cursor: self.cursor(ordinal, metadata),
            accepted_at,
        })
    }

    fn cursor(&self, ordinal: u64, metadata: Metadata) -> Cursor {
        let mut cursor = [0; 40];
        cursor[..16].copy_from_slice(&self.id);
        cursor[16..24].copy_from_slice(&ordinal.to_be_bytes());
        cursor[24..].copy_from_slice(&metadata.request);
        Cursor(cursor)
    }

    fn position(&self, cursor: Cursor) -> Result<Option<usize>> {
        if cursor.0[..16] != self.id {
            return Err(Error::Invalid("foreign event cursor"));
        }
        let index = usize::try_from(cursor.ordinal() - 1).map_err(|_| Error::Limit)?;
        Ok(self
            .events
            .get(index)
            .filter(|m| m.request == cursor.request())
            .map(|_| index))
    }

    pub fn get(&mut self, cursor: Cursor) -> Result<Option<Event>> {
        self.db.healthy()?;
        let Some(index) = self.position(cursor)? else {
            return Ok(None);
        };
        let metadata = self.events[index];
        let data = self
            .db
            .get(&event_key(index as u64 + 1, metadata))?
            .ok_or(Error::Corrupt("missing event original"))?;
        Ok(Some(Event {
            cursor,
            accepted_at: metadata.accepted_at,
            data,
        }))
    }

    /// Reads append order, independent of the wall clock. An unknown/future/
    /// divergent resume cursor refuses instead of silently skipping records.
    pub fn scan(
        &mut self,
        after: Option<Cursor>,
        limit: usize,
        mut visit: impl FnMut(Event) -> Result<()>,
    ) -> Result<Page> {
        self.db.healthy()?;
        page_limit(limit)?;
        let start = match after {
            Some(cursor) => self.position(cursor)?.ok_or(Error::Rollback)? + 1,
            None => 0,
        };
        let end = start.saturating_add(limit).min(self.events.len());
        let mut cursor = after;
        for index in start..end {
            let selected = self.cursor(index as u64 + 1, self.events[index]);
            visit(
                self.get(selected)?
                    .ok_or(Error::Corrupt("missing selected event"))?,
            )?;
            cursor = Some(selected);
        }
        Ok(Page {
            cursor,
            has_more: end < self.events.len(),
        })
    }

    /// Time range [from,to), ordered by (acceptance time, append position).
    /// Building the optional time index copies metadata only; originals stay on disk.
    pub fn scan_time(
        &mut self,
        from: i64,
        to: Option<i64>,
        after: Option<Cursor>,
        limit: usize,
        mut visit: impl FnMut(Event) -> Result<()>,
    ) -> Result<Page> {
        self.db.healthy()?;
        page_limit(limit)?;
        if to.is_some_and(|end| end <= from) {
            return Err(Error::Invalid("event time range"));
        }
        let mut lower = Included((from, 0));
        if let Some(cursor) = after {
            let index = self.position(cursor)?.ok_or(Error::Rollback)?;
            let at = self.events[index].accepted_at;
            if to.is_some_and(|end| at >= end) {
                return Ok(Page {
                    cursor: after,
                    has_more: false,
                });
            }
            if at >= from {
                lower = Excluded((at, index as u64 + 1));
            }
        }
        let order = self.time_order.get_or_insert_with(|| {
            self.events
                .iter()
                .enumerate()
                .map(|(index, event)| (event.accepted_at, index as u64 + 1))
                .collect()
        });
        let upper = to.map_or(Unbounded, |end| Excluded((end, 0)));
        let mut ordinals = order
            .range((lower, upper))
            .take(limit + 1)
            .map(|(_, ordinal)| *ordinal)
            .collect::<Vec<_>>();
        let has_more = ordinals.len() > limit;
        if has_more {
            ordinals.pop();
        }
        let mut cursor = after;
        for ordinal in ordinals {
            let selected = self.cursor(ordinal, self.events[ordinal as usize - 1]);
            visit(
                self.get(selected)?
                    .ok_or(Error::Corrupt("missing time event"))?,
            )?;
            cursor = Some(selected);
        }
        Ok(Page { cursor, has_more })
    }

    #[cfg(feature = "archive")]
    pub fn archive_to(&mut self, destination: impl AsRef<Path>) -> Result<crate::Checkpoint> {
        self.db.archive_to(destination)
    }
}

/// One healthy owner for embedded concurrent readers, appenders and followers.
/// Waiting releases the mutex. Results leave the lock before caller processing.
/// This does not bypass wire 0.1's lifetime inode lock for external processes;
/// a cross-process application must route operations through its owning service.
pub struct Collector {
    journal: Mutex<Journal>,
    changed: Condvar,
}

impl Collector {
    pub fn new(journal: Journal) -> Self {
        Self {
            journal: Mutex::new(journal),
            changed: Condvar::new(),
        }
    }
    pub fn append(&self, data: &[u8]) -> Result<Receipt> {
        let receipt = self
            .journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .append(data);
        self.changed.notify_all();
        receipt
    }
    pub fn prepare(&self, data: &[u8]) -> Result<AppendIntent> {
        self.journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .prepare(data)
    }
    pub fn append_prepared(&self, intent: &AppendIntent, data: &[u8]) -> Result<Receipt> {
        let receipt = self
            .journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .append_prepared(intent, data);
        self.changed.notify_all();
        receipt
    }
    pub fn get(&self, cursor: Cursor) -> Result<Option<Event>> {
        self.journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .get(cursor)
    }
    /// A bounded live page. None starts at the beginning; resume with Page.cursor.
    /// Timeout returns an empty page retaining the supplied cursor.
    pub fn follow(
        &self,
        after: Option<Cursor>,
        limit: usize,
        wait: Duration,
    ) -> Result<(Vec<Event>, Page)> {
        page_limit(limit)?;
        if wait > Duration::from_secs(60) {
            return Err(Error::Invalid("follow wait bound"));
        }
        let journal = self.journal.lock().map_err(|_| Error::Poisoned)?;
        let seen = match after {
            Some(cursor) => journal.position(cursor)?.ok_or(Error::Rollback)? + 1,
            None => 0,
        };
        let (mut journal, _) = self
            .changed
            .wait_timeout_while(journal, wait, |j| !j.db.poisoned && j.events.len() <= seen)
            .map_err(|_| Error::Poisoned)?;
        journal.db.healthy()?;
        let mut events = Vec::new();
        let mut next = seen;
        let mut remaining = MAX_COLLECTED_BYTES;
        let mut cursor = after;
        while next < journal.events.len() && events.len() < limit {
            let metadata = journal.events[next];
            if metadata.length > remaining {
                break;
            }
            remaining -= metadata.length;
            let selected = journal.cursor(next as u64 + 1, metadata);
            events.push(
                journal
                    .get(selected)?
                    .ok_or(Error::Corrupt("missing followed event"))?,
            );
            cursor = Some(selected);
            next += 1;
        }
        let page = Page {
            cursor,
            has_more: next < journal.events.len(),
        };
        Ok((events, page))
    }
}

fn page_limit(limit: usize) -> Result<()> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(Error::Invalid("event page bound"));
    }
    Ok(())
}
fn event_key(ordinal: u64, metadata: Metadata) -> [u8; 36] {
    let mut key = [0; 36];
    key[..4].copy_from_slice(EVENT_PREFIX);
    key[4..12].copy_from_slice(&ordinal.to_be_bytes());
    key[12..20].copy_from_slice(&metadata.accepted_at.to_be_bytes());
    key[20..].copy_from_slice(&metadata.request);
    key
}
fn parse_key(key: &[u8], ordinal: u64, request: [u8; 16]) -> Result<Metadata> {
    if key.len() != 36
        || &key[..4] != EVENT_PREFIX
        || key[4..12] != ordinal.to_be_bytes()
        || key[20..] != request
    {
        return Err(Error::Invalid("Event key grammar"));
    }
    Ok(Metadata {
        accepted_at: i64::from_be_bytes(key[12..20].try_into().unwrap()),
        request,
        length: 0,
    })
}
fn now() -> Result<i64> {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(time) => i64::try_from(time.as_micros()).map_err(|_| Error::Limit),
        Err(error) => i64::try_from(error.duration().as_micros())
            .map(|time| -time)
            .map_err(|_| Error::Limit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, path::PathBuf, sync::Arc, thread};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ojo-event-{:032x}",
                u128::from_le_bytes(request_id().unwrap())
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exact_bytes_and_lost_ack_retry_survive_reopen() {
        let root = Scratch::new();
        let key = [71; 32];
        let path = root.file("events.ojo");
        let mut journal = Journal::create(&path, Some(&key)).unwrap();
        let data = b"\xff\0exact\r\n";
        let intent = journal.prepare(data).unwrap();
        let receipt = journal.append_prepared(&intent, data).unwrap();
        let checkpoint = journal.checkpoint();
        drop(journal); // simulate a lost response; retain only engine intent and original bytes
        let intent = AppendIntent::from_bytes(intent.to_bytes()).unwrap();
        let mut journal = Journal::open(&path, Some(&key), true).unwrap();
        assert_eq!(journal.append_prepared(&intent, data).unwrap(), receipt);
        assert_eq!(journal.checkpoint(), checkpoint);
        assert!(matches!(
            journal.append_prepared(&intent, b"different"),
            Err(Error::Conflict)
        ));
        assert_eq!(journal.get(receipt.cursor).unwrap().unwrap().data, data);
        assert_eq!(journal.len(), 1);
        let empty = journal.append(b"").unwrap();
        assert_eq!(journal.get(empty.cursor).unwrap().unwrap().data, b"");
        let duplicate = journal.append(data).unwrap();
        assert_ne!(duplicate.cursor, receipt.cursor);
        assert_eq!(journal.len(), 3);
    }

    #[test]
    fn event_recovery_preserves_receipt_at_every_torn_boundary() {
        for key in [None, Some([29; 32])] {
            let root = Scratch::new();
            let path = root.file("recovery.ojo");
            let mut journal = Journal::create(&path, key.as_ref()).unwrap();
            journal.append(b"committed prefix").unwrap();
            let checkpoint = journal.checkpoint();
            let prefix = std::fs::metadata(&path).unwrap().len() as usize;
            let intent = journal.prepare(b"retry original").unwrap();
            let receipt = journal.append_prepared(&intent, b"retry original").unwrap();
            drop(journal);
            let all = std::fs::read(&path).unwrap();
            for end in prefix..=all.len() {
                std::fs::write(&path, &all[..end]).unwrap();
                let reader = Journal::open(&path, key.as_ref(), false).unwrap();
                assert_eq!(reader.len(), if end == all.len() { 2 } else { 1 });
                assert_eq!(std::fs::read(&path).unwrap(), all[..end]);
                drop(reader);
                let mut recovered = Journal::open(&path, key.as_ref(), true).unwrap();
                recovered.require_checkpoint(&checkpoint).unwrap();
                assert_eq!(
                    recovered
                        .append_prepared(&intent, b"retry original")
                        .unwrap(),
                    receipt
                );
                assert_eq!(recovered.len(), 2);
            }
        }
    }

    #[test]
    fn full_value_limit_is_available_without_timestamp_payload_overhead() {
        let root = Scratch::new();
        let mut journal = Journal::create(root.file("maximum.ojo"), None).unwrap();
        let data = vec![0xa5; wire::MAX_VALUE];
        let receipt = journal.append(&data).unwrap();
        assert_eq!(journal.get(receipt.cursor).unwrap().unwrap().data, data);
        let checkpoint = journal.checkpoint();
        assert!(journal.append(&vec![0; wire::MAX_VALUE + 1]).is_err());
        assert_eq!(journal.checkpoint(), checkpoint);
    }

    #[test]
    fn clock_backtracking_does_not_change_append_order_or_time_pagination() {
        let root = Scratch::new();
        let mut journal = Journal::create(root.file("clock.ojo"), None).unwrap();
        let mut receipts = Vec::new();
        for (time, data) in [(20, b"a"), (10, b"b"), (20, b"c"), (-3, b"d")] {
            receipts.push(journal.accept(request_id().unwrap(), time, data).unwrap());
        }
        let mut append = Vec::new();
        let page = journal
            .scan(None, 2, |event| {
                append.push(event.data);
                Ok(())
            })
            .unwrap();
        assert!(page.has_more);
        let page = journal
            .scan(page.cursor, 2, |event| {
                append.push(event.data);
                Ok(())
            })
            .unwrap();
        assert!(!page.has_more);
        assert_eq!(append, [b"a", b"b", b"c", b"d"]);
        let mut chronological = Vec::new();
        let first = journal
            .scan_time(i64::MIN, None, None, 2, |e| {
                chronological.push(e.data);
                Ok(())
            })
            .unwrap();
        assert!(first.has_more);
        journal
            .accept(request_id().unwrap(), 19, b"between")
            .unwrap(); // updates already built time index
        let last = journal
            .scan_time(i64::MIN, None, first.cursor, 10, |e| {
                chronological.push(e.data);
                Ok(())
            })
            .unwrap();
        assert!(!last.has_more);
        assert_eq!(
            chronological,
            [
                b"d".to_vec(),
                b"b".to_vec(),
                b"between".to_vec(),
                b"a".to_vec(),
                b"c".to_vec()
            ]
        );
        let mut equal = Vec::new();
        journal
            .scan_time(20, Some(21), None, 10, |e| {
                equal.push(e.cursor);
                Ok(())
            })
            .unwrap();
        assert_eq!(equal, [receipts[0].cursor, receipts[2].cursor]);
    }

    #[test]
    fn wrong_profile_and_invalid_complete_transaction_never_recover_tail() {
        let root = Scratch::new();
        let path = root.file("kv.ojo");
        let mut db = Database::create(&path, None).unwrap();
        db.commit(
            request_id().unwrap(),
            &[Change {
                key: b"existing",
                value: Some(b"original"),
            }],
        )
        .unwrap();
        drop(db);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"incomplete tail")
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(Journal::open(&path, None, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let path = root.file("invalid-event.ojo");
        drop(Journal::create(&path, None).unwrap());
        let mut db = Database::open(&path, None, true).unwrap();
        db.commit(
            request_id().unwrap(),
            &[Change {
                key: b"user-key",
                value: Some(b"payload"),
            }],
        )
        .unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        assert!(Journal::open(&path, None, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let empty = root.file("unmarked.ojo");
        drop(Database::create(&empty, None).unwrap());
        assert!(Journal::open(&empty, None, true).is_err());
    }

    #[test]
    fn cursors_reject_foreign_and_divergent_sources_and_callbacks_stop() {
        let root = Scratch::new();
        let first = root.file("first.ojo");
        let second = root.file("second.ojo");
        let mut journal = Journal::create(&first, None).unwrap();
        let a = journal.append(b"a").unwrap();
        assert_eq!(Cursor::from_bytes(a.cursor.to_bytes()).unwrap(), a.cursor);
        let b = Journal::create(&second, None)
            .unwrap()
            .append(b"b")
            .unwrap();
        assert!(journal.get(b.cursor).is_err());
        let mut forged = a.cursor.to_bytes();
        forged[39] ^= 1;
        let forged = Cursor::from_bytes(forged).unwrap();
        assert!(journal.get(forged).unwrap().is_none());
        assert!(matches!(
            journal.scan(Some(forged), 1, |_| Ok(())),
            Err(Error::Rollback)
        ));
        journal.append(b"c").unwrap();
        let mut visits = 0;
        assert!(matches!(
            journal.scan(None, 10, |_| {
                visits += 1;
                Err(Error::Limit)
            }),
            Err(Error::Limit)
        ));
        assert_eq!(visits, 1);
        assert!(Journal::create(&first, None).is_err());
        assert_eq!(journal.get(a.cursor).unwrap().unwrap().data, b"a");
    }

    #[test]
    fn follow_releases_owner_lock_and_reuses_original_position() {
        let root = Scratch::new();
        let collector = Arc::new(Collector::new(
            Journal::create(root.file("follow.ojo"), None).unwrap(),
        ));
        let first = collector.append(b"before").unwrap();
        let follower = collector.clone();
        let thread = thread::spawn(move || {
            follower
                .follow(Some(first.cursor), 10, Duration::from_secs(5))
                .unwrap()
        });
        let second = collector.append(b"after").unwrap();
        let (events, page) = thread.join().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cursor, second.cursor);
        assert_eq!(events[0].data, b"after");
        assert_eq!(page.cursor, Some(second.cursor));
        let (events, caught_up) = collector.follow(page.cursor, 10, Duration::ZERO).unwrap();
        assert!(events.is_empty());
        assert_eq!(caught_up, page);
        assert_eq!(
            collector.get(first.cursor).unwrap().unwrap().data,
            b"before"
        );
    }

    #[cfg(feature = "archive")]
    #[test]
    fn compressed_archive_preserves_logical_cursors_times_and_retry_receipts() {
        let root = Scratch::new();
        let path = root.file("source.ojo");
        let archive = root.file("archive.ojo");
        let key = [83; 32];
        let mut journal = Journal::create(&path, Some(&key)).unwrap();
        let data = vec![b'x'; 128 << 10];
        let intent = journal.prepare(&data).unwrap();
        let receipt = journal.append_prepared(&intent, &data).unwrap();
        let before = std::fs::read(&path).unwrap();
        let physical = journal.archive_to(&archive).unwrap();
        assert_ne!(physical.file_id, journal.checkpoint().file_id);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut archived = Journal::open(&archive, Some(&key), true).unwrap();
        assert_eq!(archived.append_prepared(&intent, &data).unwrap(), receipt);
        let event = archived.get(receipt.cursor).unwrap().unwrap();
        assert_eq!(event.data, data);
        assert_eq!(event.accepted_at, receipt.accepted_at);
        assert_eq!(archived.len(), 1);
    }
}
