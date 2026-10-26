//! Generic event adapter: engine UTC timestamp and exact data. Cursor bytes
//! are internal storage identities, not caller-assigned event metadata.
use super::*;

/// Signed Unix microseconds, assigned by the source engine at first acceptance.
/// Retried appends retain the source's original timestamp and data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    pub timestamp: i64,
    pub data: Vec<u8>,
}
pub struct EventQuery<'a> {
    pub text: &'a str,
    pub from: i64,
    /// Exclusive signed Unix-microsecond bound; None includes i64::MAX.
    pub to: Option<i64>,
    pub limit: usize,
    pub excerpt_bytes: usize,
    pub max_bytes: usize,
}
impl<'a> EventQuery<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            from: i64::MIN,
            to: None,
            limit: 20,
            excerpt_bytes: 2048,
            max_bytes: 1 << 20,
        }
    }
    fn query(&self) -> Result<Query<'_>> {
        if self.to.is_some_and(|to| to <= self.from) {
            return Err(Error::Invalid);
        }
        Ok(Query {
            text: self.text,
            k: self.limit,
            from: self.from,
            to: self.to.map_or(i64::MAX, |to| to - 1),
            filter: Filter::default(),
            excerpt_bytes: self.excerpt_bytes,
            max_bytes: self.max_bytes,
        })
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct EventHit {
    pub cursor: Vec<u8>,
    pub timestamp: i64,
    pub bits: f64,
    pub continued: bool,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct EventRanked {
    pub cursor: Vec<u8>,
    pub bits: f64,
    pub continued: bool,
    #[serde(skip)]
    inner: Ranked,
}
/// Generic facade over the same persistent lexical engine. It requires no
/// agent, source, kind, severity or metadata map in the event data model.
pub struct EventBuilder {
    inner: Builder,
}
impl EventBuilder {
    pub fn create(
        directory: impl AsRef<Path>,
        cache_key: &[u8; 32],
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        Ok(Self {
            inner: Builder::create(directory, cache_key, binding, limits)?,
        })
    }
    /// Signed UTC Unix microseconds. Returns false for nonUTF8 data; those
    /// events remain exact/readable in the authoritative event log. Indexing
    /// never edits, converts or removes their original bytes.
    pub fn push(&mut self, cursor: &[u8], timestamp: i64, data: &[u8]) -> Result<bool> {
        cursor_id(cursor)?;
        let Ok(text) = std::str::from_utf8(data) else {
            return Ok(false);
        };
        self.inner.push(neutral(cursor, timestamp, text)?)?;
        Ok(true)
    }
    pub fn finish(self) -> Result<EventIndex> {
        Ok(EventIndex {
            inner: self.inner.finish()?,
        })
    }
}
pub struct EventIndex {
    inner: Index,
}
pub struct EventAppender {
    inner: Appender,
}
impl EventAppender {
    /// Requires the caller to prove that the old source prefix and scope are
    /// unchanged and to revalidate the new Binding before publication/egress.
    pub fn extend_validated_prefix(
        previous: &EventIndex,
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        Ok(Self {
            inner: Appender::extend_validated_prefix(&previous.inner, binding, limits)?,
        })
    }
    /// Extend a validated prefix into a separate root on the same filesystem.
    /// The previous root is unchanged, allowing a caller to publish associated
    /// source-checkpoint metadata and the new index as one cache generation.
    pub fn extend_validated_prefix_to(
        previous: &EventIndex,
        directory: impl AsRef<Path>,
        binding: Binding,
        limits: Limits,
    ) -> Result<Self> {
        Ok(Self {
            inner: Appender::extend_validated_prefix_to(
                &previous.inner,
                directory,
                binding,
                limits,
            )?,
        })
    }
    pub fn push(&mut self, cursor: &[u8], timestamp: i64, data: &[u8]) -> Result<bool> {
        cursor_id(cursor)?;
        let Ok(text) = std::str::from_utf8(data) else {
            return Ok(false);
        };
        self.inner.push(neutral(cursor, timestamp, text)?)?;
        Ok(true)
    }
    pub fn retire(&mut self, cursor: &[u8]) -> Result<()> {
        self.inner.retire(&cursor_id(cursor)?)
    }
    /// Retire a live cursor from the previous projection; absent/already
    /// retired cursors return false. Source originals remain authoritative.
    pub fn retire_if_present(&mut self, cursor: &[u8]) -> Result<bool> {
        self.inner.retire_if_present(&cursor_id(cursor)?)
    }
    pub fn finish(self) -> Result<EventIndex> {
        Ok(EventIndex {
            inner: self.inner.finish()?,
        })
    }
}
impl EventIndex {
    pub fn open(
        directory: impl AsRef<Path>,
        cache_key: &[u8; 32],
        binding: &Binding,
    ) -> Result<Self> {
        Ok(Self {
            inner: Index::open(directory, cache_key, binding)?,
        })
    }
    pub fn open_with_limits(
        directory: impl AsRef<Path>,
        cache_key: &[u8; 32],
        binding: &Binding,
        limits: Limits,
    ) -> Result<Self> {
        Ok(Self {
            inner: Index::open_with_limits(directory, cache_key, binding, limits)?,
        })
    }
    pub fn row_count(&self) -> u64 {
        self.inner.row_count()
    }
    pub fn text_bytes(&self) -> u64 {
        self.inner.text_bytes()
    }
    pub fn index_bytes(&self) -> u64 {
        self.inner.index_bytes()
    }
    pub fn allocated_index_bytes(&self) -> u64 {
        self.inner.allocated_index_bytes()
    }
    pub fn estimated_heap_bytes(&self) -> usize {
        self.inner.estimated_heap_bytes()
    }
    pub fn binding(&self) -> &Binding {
        self.inner.binding()
    }
    /// Stored build limits and effective original-byte query ceiling. A derived
    /// refresh must not raise that ceiling implicitly.
    pub fn limits(&self) -> Limits {
        self.inner.manifest.limits
    }
    pub fn build_metrics(&self) -> BuildMetrics {
        self.inner.build_metrics()
    }
    pub fn rank(&self, binding: &Binding, q: &EventQuery<'_>) -> Result<Vec<EventRanked>> {
        event_ranks(self.inner.rank(binding, &q.query()?)?)
    }
    pub fn rank_uncached(&self, binding: &Binding, q: &EventQuery<'_>) -> Result<Vec<EventRanked>> {
        event_ranks(self.inner.rank_uncached(binding, &q.query()?)?)
    }
    /// Callers revalidate the current source checkpoint/permission before and
    /// after this query, and through any network response completion.
    pub fn search(
        &self,
        binding: &Binding,
        q: &EventQuery<'_>,
        fetch: impl FnMut(&[u8]) -> Result<EventRow>,
    ) -> Result<Vec<EventHit>> {
        let ranked = self.rank(binding, q)?;
        self.materialize(binding, q, &ranked, fetch)
    }
    pub fn materialize(
        &self,
        binding: &Binding,
        q: &EventQuery<'_>,
        ranked: &[EventRanked],
        mut fetch: impl FnMut(&[u8]) -> Result<EventRow>,
    ) -> Result<Vec<EventHit>> {
        for hit in ranked {
            if cursor_id(&hit.cursor)? != hit.inner.id
                || hit.bits.to_bits() != hit.inner.bits.to_bits()
                || hit.continued != hit.inner.continued
            {
                return Err(Error::Invalid);
            }
        }
        let ranked = ranked.iter().map(|r| r.inner.clone()).collect::<Vec<_>>();
        let candidates = self
            .inner
            .materialize(binding, &q.query()?, &ranked, |id| {
                let cursor = decode_cursor(id)?;
                let event = fetch(&cursor)?;
                let text = std::str::from_utf8(&event.data).map_err(|_| Error::Authority)?;
                neutral(&cursor, event.timestamp, text)
            })?;
        let mut bytes = 2usize;
        let mut hits = Vec::with_capacity(candidates.len());
        for hit in candidates {
            let event = EventHit {
                cursor: decode_cursor(&hit.id)?,
                timestamp: hit.valid_at,
                bits: hit.bits,
                continued: hit.continued,
                start: hit.start,
                end: hit.end,
                excerpt: hit.excerpt,
            };
            let size = serde_json::to_vec(&event)
                .map_err(|_| Error::Unavailable)?
                .len();
            let next = bytes
                .checked_add(size)
                .and_then(|v| v.checked_add(usize::from(!hits.is_empty())))
                .ok_or(Error::Bounds)?;
            if next > q.max_bytes {
                return Err(Error::Bounds);
            }
            bytes = next;
            hits.push(event);
        }
        Ok(hits)
    }
}
fn event_ranks(ranked: Vec<Ranked>) -> Result<Vec<EventRanked>> {
    ranked
        .into_iter()
        .map(|inner| {
            Ok(EventRanked {
                cursor: decode_cursor(&inner.id)?,
                bits: inner.bits,
                continued: inner.continued,
                inner,
            })
        })
        .collect()
}
fn neutral(cursor: &[u8], timestamp: i64, text: &str) -> Result<Row> {
    Ok(Row {
        id: cursor_id(cursor)?,
        key: None,
        text: text.to_owned(),
        valid_at: timestamp,
        reported_recorded_at: timestamp,
        kind: Kind::Raw,
        tool: None,
        status: None,
        agent_id: String::new(),
        indexed: true,
    })
}
fn cursor_id(cursor: &[u8]) -> Result<String> {
    if cursor.is_empty() || cursor.len() > 512 {
        return Err(Error::Bounds);
    }
    Ok(hex(cursor))
}
fn decode_cursor(id: &str) -> Result<Vec<u8>> {
    if id.is_empty() || id.len() > 1024 || !id.len().is_multiple_of(2) {
        return Err(Error::Authority);
    }
    id.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            Ok((digit(pair[0]).ok_or(Error::Authority)? << 4)
                | digit(pair[1]).ok_or(Error::Authority)?)
        })
        .collect()
}
