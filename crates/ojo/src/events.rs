//! General timestamp-and-bytes journals, using the separately versioned Event profile 1.
//!
//! The logical record is an engine acceptance time (signed Unix microseconds,
//! UTC) and exact bytes. Cursors and retry intents are engine bookkeeping. A
//! journal has no required application keys or provider/session/agent fields.
//! This profile is additive: ordinary wire 0.1 key/value files are refused.

use crate::{Change, CommitMode, Database, Error, Replay, Result, request_id, wire};
use std::{
    collections::{BTreeMap, BTreeSet},
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
            ".juro-event-{:032x}.tmp",
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
    /// Confirms a replayed prefix before an application acknowledges its own
    /// reconstructed retry, without generating a new timestamp or event.
    pub fn confirm(&mut self) -> Result<crate::Checkpoint> {
        self.db.confirm()
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
        self.check_intent_data(intent, data)?;
        self.accept(intent.request, intent.accepted_at, data)
    }

    /// Appends up to 1,024 retained intents and 64 MiB of data, sharing one final
    /// durability barrier. Each event keeps its own unchanged profile frame.
    /// All input is checked before writing, and success confirms every receipt.
    ///
    /// This is group durability, not an atomic visibility or rollback boundary.
    /// An interrupted/erroring group may leave any complete prefix visible on
    /// reopen. Retain every original intent and retry the entire group. Already
    /// committed events retain their original cursor/time without duplication.
    /// A failure after writing starts poisons the handle; drop and reopen it.
    pub fn append_prepared_batch(
        &mut self,
        items: &[(&AppendIntent, &[u8])],
    ) -> Result<Vec<Receipt>> {
        self.writable(&[])?;
        if items.is_empty() || items.len() > MAX_PAGE {
            return Err(Error::Invalid("event batch count"));
        }
        let mut bytes = 0usize;
        let mut requests: BTreeMap<[u8; 16], (&AppendIntent, &[u8])> = BTreeMap::new();
        for &(intent, data) in items {
            bytes = bytes.checked_add(data.len()).ok_or(Error::Limit)?;
            if bytes > MAX_COLLECTED_BYTES {
                return Err(Error::Limit);
            }
            self.check_intent(intent, data)?;
            if let Some((previous, original)) = requests.insert(intent.request, (intent, data))
                && (previous.accepted_at != intent.accepted_at || original != data)
            {
                return Err(Error::Conflict);
            }
        }
        let new_events = requests
            .keys()
            .filter(|request| !self.db.requests.contains_key(*request))
            .count();
        self.db.admit(new_events, new_events * 36)?;
        let result = (|| {
            let mut receipts = Vec::with_capacity(items.len());
            for &(intent, data) in items {
                receipts.push(self.accept_mode(
                    intent.request,
                    intent.accepted_at,
                    data,
                    CommitMode::Buffered,
                )?);
            }
            self.db.confirm()?;
            Ok(receipts)
        })();
        if result.is_err() {
            self.db.poisoned = true;
        }
        result
    }

    /// Validates retained input without requiring a writable handle or changing
    /// a torn tail. A previously committed request also checks its exact original.
    /// This is validation, not a durable acknowledgement or writer reservation.
    pub fn check_intent(&mut self, intent: &AppendIntent, data: &[u8]) -> Result<()> {
        self.check_intent_data(intent, data)?;
        if let Some(&index) = self.db.requests.get(&intent.request) {
            if index == 0 || self.events[index - 1].accepted_at != intent.accepted_at {
                return Err(Error::Conflict);
            }
            let cursor = self.cursor(index as u64, self.events[index - 1]);
            if self
                .get(cursor)?
                .ok_or(Error::Corrupt("missing retry original"))?
                .data
                != data
            {
                return Err(Error::Conflict);
            }
        }
        Ok(())
    }

    /// Resolves a retained engine intent to its exact accepted event, if present.
    /// Validates the journal, original engine time and original data hash. Works
    /// through read-only handles and never confirms durability or acknowledges a
    /// write; use confirm or an exact append retry before returning a write ACK.
    pub fn get_prepared(&mut self, intent: &AppendIntent) -> Result<Option<Event>> {
        self.db.healthy()?;
        if intent.journal != self.id {
            return Err(Error::Conflict);
        }
        let Some(&index) = self.db.requests.get(&intent.request) else {
            return Ok(None);
        };
        if index == 0 || self.events[index - 1].accepted_at != intent.accepted_at {
            return Err(Error::Conflict);
        }
        let event = self
            .get(self.cursor(index as u64, self.events[index - 1]))?
            .ok_or(Error::Corrupt("missing retry original"))?;
        if wire::hash(&[&event.data]) != intent.data_hash {
            return Err(Error::Conflict);
        }
        Ok(Some(event))
    }

    fn check_intent_data(&self, intent: &AppendIntent, data: &[u8]) -> Result<()> {
        self.db.healthy()?;
        if data.len() > wire::MAX_VALUE {
            return Err(Error::Invalid("event data length"));
        }
        if intent.journal != self.id || wire::hash(&[data]) != intent.data_hash {
            return Err(Error::Conflict);
        }
        Ok(())
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
        self.accept_mode(request, accepted_at, data, CommitMode::Durable)
    }

    fn accept_mode(
        &mut self,
        request: [u8; 16],
        accepted_at: i64,
        data: &[u8],
        mode: CommitMode,
    ) -> Result<Receipt> {
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
        self.db.commit_encoded(
            request,
            &[Change {
                key: &key,
                value: Some(data),
            }],
            mode,
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
    /// Shares the journal's bounded group durability barrier under its owner lock.
    pub fn append_prepared_batch(&self, items: &[(&AppendIntent, &[u8])]) -> Result<Vec<Receipt>> {
        let receipts = self
            .journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .append_prepared_batch(items);
        self.changed.notify_all();
        receipts
    }
    pub fn get(&self, cursor: Cursor) -> Result<Option<Event>> {
        self.journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .get(cursor)
    }
    pub fn get_prepared(&self, intent: &AppendIntent) -> Result<Option<Event>> {
        self.journal
            .lock()
            .map_err(|_| Error::Poisoned)?
            .get_prepared(intent)
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
        let path = root.file("events.juro");
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
    fn batch_validates_every_intent_before_writing_and_preserves_exact_retries() {
        for key in [None, Some([81; 32])] {
            let root = Scratch::new();
            let path = root.file("group.juro");
            let mut journal = Journal::create(&path, key.as_ref()).unwrap();
            let a = journal.prepare(b"first\0\xff").unwrap();
            let b = journal.prepare(b"").unwrap();
            let items = [(&a, b"first\0\xff".as_slice()), (&b, b"".as_slice())];
            let original = std::fs::read(&path).unwrap();
            assert!(matches!(
                journal.append_prepared_batch(&[items[0], (&b, b"changed")]),
                Err(Error::Conflict)
            ));
            let mut changed_time = a;
            changed_time.accepted_at += 1;
            assert!(matches!(
                journal.append_prepared_batch(&[items[0], (&changed_time, items[0].1)]),
                Err(Error::Conflict)
            ));
            assert!(journal.append_prepared_batch(&[]).is_err());
            assert!(
                journal
                    .append_prepared_batch(&vec![items[0]; 1025])
                    .is_err()
            );
            let operations = journal.db.operations;
            journal.db.operations = crate::MAX_OPERATIONS - 1;
            assert!(matches!(
                journal.append_prepared_batch(&items),
                Err(Error::Limit)
            ));
            journal.db.operations = operations;
            let key_bytes = journal.db.key_bytes;
            journal.db.key_bytes = crate::MAX_KEY_BYTES - 36;
            assert!(matches!(
                journal.append_prepared_batch(&items),
                Err(Error::Limit)
            ));
            journal.db.key_bytes = key_bytes;
            journal.db.healthy().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(journal.len(), 0);
            let receipts = journal.append_prepared_batch(&items).unwrap();
            assert_eq!(journal.len(), 2);
            let checkpoint = journal.checkpoint();
            assert_eq!(journal.append_prepared_batch(&items).unwrap(), receipts);
            assert_eq!(journal.checkpoint(), checkpoint);
            assert_eq!(
                journal
                    .append_prepared_batch(&[items[0], items[0]])
                    .unwrap(),
                vec![receipts[0], receipts[0]]
            );
            drop(journal);
            let mut replay = Journal::open(&path, key.as_ref(), true).unwrap();
            assert_eq!(replay.append_prepared_batch(&items).unwrap(), receipts);
            for (receipt, (_, data)) in receipts.iter().zip(items) {
                let event = replay.get(receipt.cursor).unwrap().unwrap();
                assert_eq!(event.accepted_at, receipt.accepted_at);
                assert_eq!(event.data, data);
            }
            assert_eq!(replay.checkpoint(), checkpoint);
            drop(replay);
            let mut reader = Journal::open(&path, key.as_ref(), false).unwrap();
            assert!(matches!(
                reader.append_prepared_batch(&items),
                Err(Error::ReadOnly)
            ));
        }
    }

    #[test]
    fn partial_group_retry_preserves_surviving_prefix_receipts() {
        for key in [None, Some([82; 32])] {
            let root = Scratch::new();
            let path = root.file("partial-group.juro");
            let mut journal = Journal::create(&path, key.as_ref()).unwrap();
            journal.append(b"older original").unwrap();
            let a = journal.prepare(b"one").unwrap();
            let b = journal.prepare(b"two\0\xff").unwrap();
            let c = journal.prepare(b"").unwrap();
            let items = [
                (&a, b"one".as_slice()),
                (&b, b"two\0\xff".as_slice()),
                (&c, b"".as_slice()),
            ];
            let start = std::fs::metadata(&path).unwrap().len() as usize;
            let receipts = journal.append_prepared_batch(&items).unwrap();
            let checkpoint = journal.checkpoint();
            let ends = journal
                .db
                .transactions
                .iter()
                .skip(2)
                .map(|tx| tx.offset as usize)
                .collect::<Vec<_>>();
            drop(journal);
            let all = std::fs::read(&path).unwrap();
            let mut cuts = vec![start, all.len()];
            for end in ends.into_iter().skip(1) {
                cuts.extend([end - 1, end, end + 1]);
            }
            cuts.push(all.len() - 1);
            cuts.sort_unstable();
            cuts.dedup();
            for end in cuts {
                std::fs::write(&path, &all[..end]).unwrap();
                let mut replay = Journal::open(&path, key.as_ref(), true).unwrap();
                assert_eq!(replay.append_prepared_batch(&items).unwrap(), receipts);
                assert_eq!(replay.len(), 4);
                // Encrypted recovery may reseal the missing suffix with fresh nonces.
                assert_eq!(replay.checkpoint().sequence, checkpoint.sequence);
                for (receipt, (_, data)) in receipts.iter().zip(items) {
                    assert_eq!(replay.get(receipt.cursor).unwrap().unwrap().data, data);
                }
            }
        }
    }

    #[test]
    fn batch_full_original_limit_and_aggregate_budget_are_independent() {
        let root = Scratch::new();
        let path = root.file("group-budget.juro");
        let mut journal = Journal::create(&path, None).unwrap();
        let data = vec![0xa5; wire::MAX_VALUE];
        let intent = journal.prepare(&data).unwrap();
        let extra = journal.prepare(b"x").unwrap();
        let checkpoint = journal.checkpoint();
        let item = (&intent, data.as_slice());
        assert!(matches!(
            journal.append_prepared_batch(&[item, item, item, item, (&extra, b"x")]),
            Err(Error::Limit)
        ));
        assert_eq!(journal.checkpoint(), checkpoint);
        let receipts = journal.append_prepared_batch(&[item]).unwrap();
        assert_eq!(journal.get(receipts[0].cursor).unwrap().unwrap().data, data);
        assert_eq!(journal.db.transactions.last().unwrap().head[36], 0);
    }

    #[test]
    fn retained_intent_reads_bind_exact_original_time_and_receipt_without_writing() {
        for key in [None, Some([85; 32])] {
            let root = Scratch::new();
            let path = root.file("intent-read.juro");
            let mut journal = Journal::create(&path, key.as_ref()).unwrap();
            let intent = journal.prepare(b"original\0\xff").unwrap();
            assert!(journal.get_prepared(&intent).unwrap().is_none());
            let receipt = journal.append_prepared(&intent, b"original\0\xff").unwrap();
            let expected = journal.get(receipt.cursor).unwrap().unwrap();
            assert_eq!(journal.get_prepared(&intent).unwrap(), Some(expected));
            drop(journal);
            let bytes = std::fs::read(&path).unwrap();
            let mut reader = Journal::open(&path, key.as_ref(), false).unwrap();
            let event = reader.get_prepared(&intent).unwrap().unwrap();
            assert_eq!(event.cursor, receipt.cursor);
            assert_eq!(event.accepted_at, receipt.accepted_at);
            assert_eq!(event.data, b"original\0\xff");
            let mut wrong = intent;
            wrong.accepted_at += 1;
            assert!(matches!(reader.get_prepared(&wrong), Err(Error::Conflict)));
            wrong = intent;
            wrong.data_hash[0] ^= 1;
            assert!(matches!(reader.get_prepared(&wrong), Err(Error::Conflict)));
            wrong = intent;
            wrong.journal[0] ^= 1;
            assert!(matches!(reader.get_prepared(&wrong), Err(Error::Conflict)));
            let collector = Collector::new(reader);
            assert_eq!(
                collector.get_prepared(&intent).unwrap().unwrap().cursor,
                receipt.cursor
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn event_recovery_preserves_receipt_at_every_torn_boundary() {
        for key in [None, Some([29; 32])] {
            let root = Scratch::new();
            let path = root.file("recovery.juro");
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
        let mut journal = Journal::create(root.file("maximum.juro"), None).unwrap();
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
        let mut journal = Journal::create(root.file("clock.juro"), None).unwrap();
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
        let path = root.file("kv.juro");
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

        let path = root.file("invalid-event.juro");
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
        let empty = root.file("unmarked.juro");
        drop(Database::create(&empty, None).unwrap());
        assert!(Journal::open(&empty, None, true).is_err());
    }

    #[test]
    fn cursors_reject_foreign_and_divergent_sources_and_callbacks_stop() {
        let root = Scratch::new();
        let first = root.file("first.juro");
        let second = root.file("second.juro");
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
            Journal::create(root.file("follow.juro"), None).unwrap(),
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
        let path = root.file("source.juro");
        let archive = root.file("archive.juro");
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
