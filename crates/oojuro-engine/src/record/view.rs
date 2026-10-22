//! Prefix-bound time/key views. No clock or authority write occurs on this path.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
const CURSOR_AAD: &[u8] = b"oojuro/record-cursor/v1\0";
pub const MAX_OUTPUT_BYTES: usize = 64 << 20;
const MAX_CURSOR: usize = 16 << 10;
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub writer: &'a str,
    pub owner: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsOf {
    pub from: i64,
    pub valid_at: i64,
    pub known_at: i64,
}
impl Default for AsOf {
    fn default() -> Self {
        Self {
            from: i64::MIN,
            valid_at: i64::MAX,
            known_at: i64::MAX,
        }
    }
}
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub kind: Option<Kind>,
    pub tool: Option<String>,
    pub status: Option<Status>,
    pub agent_id: Option<String>,
}
impl Filter {
    /// Pure filter bounds; selection and authorization remain caller duties.
    pub fn validate(&self) -> Result<()> {
        if self.tool.as_ref().is_some_and(|v| !name(v, 1024))
            || self.agent_id.as_ref().is_some_and(|v| !name(v, 256))
        {
            Err(Error::Bounds)
        } else {
            Ok(())
        }
    }
    pub(super) fn keep(&self, row: &Assigned) -> bool {
        let v = &row.input;
        self.kind.is_none_or(|x| x == v.kind)
            && self
                .tool
                .as_ref()
                .is_none_or(|x| Some(x) == v.tool.as_ref())
            && self.status.is_none_or(|x| Some(x) == v.status)
            && self.agent_id.as_ref().is_none_or(|x| x == &v.agent_id)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Versions {
    Current,
    All,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 100,
            bytes: 1 << 20,
        }
    }
}
#[derive(Serialize)]
pub struct Page {
    pub watermark: Watermark,
    pub records: Vec<Record>,
    pub next: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    watermark: Watermark,
    writer: String,
    owner: u32,
    as_of: AsOf,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "read", deny_unknown_fields)]
enum Mode {
    History { key: String },
    Range { versions: Versions, filter: Filter },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    schema: u32,
    context: Context,
    mode: Mode,
    next: usize,
}
pub struct View {
    context: Context,
}
impl View {
    pub fn open(log: &RecordLog, scope: Scope<'_>, as_of: AsOf) -> Result<Self> {
        check_scope(log, scope)?;
        Ok(Self {
            context: Context {
                watermark: log.tip()?,
                writer: scope.writer.into(),
                owner: scope.owner,
                as_of,
            },
        })
    }
    pub fn watermark(&self) -> &Watermark {
        &self.context.watermark
    }
    pub(super) fn as_of(&self) -> AsOf {
        self.context.as_of
    }
    fn validate(&self, log: &RecordLog) -> Result<usize> {
        check_scope(
            log,
            Scope {
                writer: &self.context.writer,
                owner: self.context.owner,
            },
        )?;
        log.validate(&self.context.watermark)
    }
    fn time(&self, row: &Assigned) -> bool {
        row.input.valid_at <= self.context.as_of.valid_at
            && row.recorded_at <= self.context.as_of.known_at
    }
    fn lower(&self, row: &Assigned) -> bool {
        row.input.valid_at >= self.context.as_of.from
    }
    pub fn get_id(&self, log: &RecordLog, id: u64, bytes: usize) -> Result<Option<Record>> {
        let end = self.validate(log)?;
        check_bytes(bytes)?;
        let row = log.records[..end]
            .iter()
            .find(|r| r.id == id && self.time(r) && self.lower(r))
            .map(|r| r.record(&self.context.writer, &log.identity().adapter));
        sized(&row, bytes)?;
        Ok(row)
    }
    pub fn get_key(&self, log: &RecordLog, key: &str, bytes: usize) -> Result<Option<Record>> {
        check_key(key)?;
        let rows = self.latest(log)?;
        check_bytes(bytes)?;
        let row = rows
            .into_iter()
            .find(|r| r.input.key.as_deref() == Some(key) && self.lower(r))
            .map(|r| r.record(&self.context.writer, &log.identity().adapter));
        sized(&row, bytes)?;
        Ok(row)
    }
    /// All original versions within the snapshot/time bounds, oldest first.
    pub fn history(&self, log: &RecordLog, key: &str, limits: Limits) -> Result<Page> {
        check_key(key)?;
        self.page(log, Mode::History { key: key.into() }, 0, limits)
    }
    pub fn range(
        &self,
        log: &RecordLog,
        versions: Versions,
        filter: Filter,
        limits: Limits,
    ) -> Result<Page> {
        filter.validate()?;
        self.page(log, Mode::Range { versions, filter }, 0, limits)
    }
    pub fn export(
        &self,
        log: &RecordLog,
        versions: Versions,
        filter: Filter,
        limits: Limits,
    ) -> Result<Vec<u8>> {
        let page = self.range(log, versions, filter, limits)?;
        json(&page, limits.bytes)
    }
    /// Resume exactly the protected prefix, cutoffs, filters and read mode.
    /// Replay intentionally returns the same rows; opaque next bytes may vary.
    pub fn continue_page(
        log: &RecordLog,
        scope: Scope<'_>,
        cursor: &str,
        limits: Limits,
    ) -> Result<Page> {
        check_scope(log, scope)?;
        let encoded = unhex(cursor)?;
        let plain = log.unseal(CURSOR_AAD, &encoded)?;
        let cursor: Cursor = serde_json::from_slice(&plain).map_err(|_| Error::Invalid)?;
        if cursor.schema != 1
            || cursor.next == 0
            || cursor.next > MAX_RECORDS
            || cursor.context.writer != scope.writer
            || cursor.context.owner != scope.owner
        {
            return Err(Error::Invalid);
        }
        let view = Self {
            context: cursor.context,
        };
        view.page(log, cursor.mode, cursor.next, limits)
    }
    pub(super) fn latest<'a>(&self, log: &'a RecordLog) -> Result<Vec<&'a Assigned>> {
        let end = self.validate(log)?;
        Ok(latest_rows(log.records[..end].iter(), self.context.as_of))
    }
    pub(super) fn visible(&self, log: &RecordLog, filter: &Filter) -> Result<BTreeSet<u64>> {
        filter.validate()?;
        Ok(self
            .latest(log)?
            .into_iter()
            .filter(|r| self.lower(r) && filter.keep(r))
            .map(|r| r.id)
            .collect())
    }
    pub(super) fn keys(&self, log: &RecordLog) -> Result<BTreeMap<String, u64>> {
        Ok(self
            .latest(log)?
            .into_iter()
            .filter_map(|r| r.input.key.as_ref().map(|key| (key.clone(), r.id)))
            .collect())
    }
    fn selection<'a>(&self, log: &'a RecordLog, mode: &Mode) -> Result<Vec<&'a Assigned>> {
        let end = self.validate(log)?;
        let mut rows: Vec<&Assigned> = match mode {
            Mode::History { key } => {
                check_key(key)?;
                log.records[..end]
                    .iter()
                    .filter(|r| {
                        r.input.key.as_deref() == Some(key) && self.time(r) && self.lower(r)
                    })
                    .collect()
            }
            Mode::Range { versions, filter } => {
                filter.validate()?;
                let rows = match versions {
                    Versions::All => log.records[..end].iter().filter(|r| self.time(r)).collect(),
                    Versions::Current => self.latest(log)?,
                };
                rows.into_iter()
                    .filter(|r| self.lower(r) && filter.keep(r))
                    .collect()
            }
        };
        rows.sort_by_key(|r| (r.input.valid_at, r.recorded_at, r.id));
        Ok(rows)
    }
    fn page(&self, log: &RecordLog, mode: Mode, start: usize, limits: Limits) -> Result<Page> {
        if limits.entries == 0 || limits.entries > 1000 {
            return Err(Error::Bounds);
        }
        check_bytes(limits.bytes)?;
        let rows = self.selection(log, &mode)?;
        if start > rows.len() || (start != 0 && start == rows.len()) {
            return Err(Error::Invalid);
        }
        // Compute candidate lengths once. Cursor overhead must be checked for
        // the FINAL page: an intermediate cursor may exceed a budget that the
        // complete cursor-free page fits. Never skip an oversized first row.
        let stop = rows.len().min(start + limits.entries);
        let mut sizes = vec![0usize];
        for row in &rows[start..stop] {
            let length = json(
                &row.record(&self.context.writer, &log.identity().adapter),
                MAX_OUTPUT_BYTES,
            )?
            .len();
            sizes.push(
                sizes
                    .last()
                    .copied()
                    .unwrap_or(0)
                    .checked_add(length)
                    .ok_or(Error::Bounds)?,
            );
        }
        let mut end = stop;
        let next = loop {
            let next = if end < rows.len() {
                Some(self.cursor(log, mode.clone(), end)?)
            } else {
                None
            };
            let empty = Page {
                watermark: self.context.watermark.clone(),
                records: vec![],
                next: next.clone(),
            };
            let count = end - start;
            let length = json(&empty, limits.bytes)?
                .len()
                .checked_add(sizes[count])
                .and_then(|n| n.checked_add(count.saturating_sub(1)))
                .ok_or(Error::Bounds)?;
            if length <= limits.bytes {
                break next;
            }
            if end == start {
                return Err(Error::Bounds);
            }
            end -= 1;
            if end == start && start < rows.len() {
                return Err(Error::Bounds);
            }
        };
        let page = Page {
            watermark: self.context.watermark.clone(),
            records: rows[start..end]
                .iter()
                .map(|r| r.record(&self.context.writer, &log.identity().adapter))
                .collect(),
            next,
        };
        sized(&page, limits.bytes)?;
        Ok(page)
    }
    fn cursor(&self, log: &RecordLog, mode: Mode, next: usize) -> Result<String> {
        let bytes = serde_json::to_vec(&Cursor {
            schema: 1,
            context: self.context.clone(),
            mode,
            next,
        })
        .map_err(|_| Error::Unavailable)?;
        let token = hex(&log.seal(CURSOR_AAD, &bytes));
        if token.len() > MAX_CURSOR {
            return Err(Error::Bounds);
        }
        Ok(token)
    }
}
fn check_scope(log: &RecordLog, scope: Scope<'_>) -> Result<()> {
    log.healthy()?;
    let id = log.identity();
    if scope.writer != id.writer || scope.owner != id.agent {
        Err(Error::Scope)
    } else {
        Ok(())
    }
}
fn check_key(key: &str) -> Result<()> {
    if name(key, 1024) {
        Ok(())
    } else {
        Err(Error::Bounds)
    }
}
fn check_bytes(bytes: usize) -> Result<()> {
    if bytes == 0 || bytes > MAX_OUTPUT_BYTES {
        Err(Error::Bounds)
    } else {
        Ok(())
    }
}
fn sized<T: Serialize>(value: &T, bytes: usize) -> Result<()> {
    json(value, bytes).map(|_| ())
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}
fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.is_empty() || s.len() > MAX_CURSOR || !s.len().is_multiple_of(2) {
        return Err(Error::Invalid);
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(Error::Invalid),
    };
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Ok(digit(p[0])? * 16 + digit(p[1])?))
        .collect()
}

/// Shared current-key winner kernel. Eligibility/filter/lower bound apply AFTER this selection.
pub(super) fn latest_rows<'a>(
    rows: impl IntoIterator<Item = &'a Assigned>,
    as_of: AsOf,
) -> Vec<&'a Assigned> {
    let mut keyed: BTreeMap<&str, &Assigned> = BTreeMap::new();
    let mut unkeyed = Vec::new();
    for row in rows
        .into_iter()
        .filter(|row| row.input.valid_at <= as_of.valid_at && row.recorded_at <= as_of.known_at)
    {
        if let Some(key) = &row.input.key {
            if keyed.get(key.as_str()).is_none_or(|old| {
                (row.input.valid_at, row.recorded_at, row.id)
                    > (old.input.valid_at, old.recorded_at, old.id)
            }) {
                keyed.insert(key, row);
            }
        } else {
            unkeyed.push(row);
        }
    }
    unkeyed.extend(keyed.into_values());
    unkeyed
}
