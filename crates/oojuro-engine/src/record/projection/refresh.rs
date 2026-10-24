//! Request-local equality checks; incoming references never survive push().
use super::*;
use std::collections::BTreeMap;

/// Borrowed source bytes, with exactly the owned Row's serialized fields/order.
#[derive(Clone, Copy, Serialize)]
pub struct RowRef<'a> {
    pub id: &'a str,
    pub key: Option<&'a str>,
    pub text: &'a str,
    pub valid_at: i64,
    pub reported_recorded_at: i64,
    pub kind: Kind,
    pub tool: Option<&'a str>,
    pub status: Option<Status>,
    pub agent_id: &'a str,
    pub indexed: bool,
}
impl Row {
    pub fn as_borrowed(&self) -> RowRef<'_> {
        let Self {
            id, key, text, valid_at, reported_recorded_at, kind, tool, status,
            agent_id, indexed,
        } = self;
        RowRef {
            id, key: key.as_deref(), text, valid_at: *valid_at,
            reported_recorded_at: *reported_recorded_at, kind: *kind,
            tool: tool.as_deref(), status: *status, agent_id, indexed: *indexed,
        }
    }
}
impl RowRef<'_> {
    fn same_metadata(&self, other: &Row) -> bool {
        let Self {
            id, key, text: _, valid_at, reported_recorded_at, kind, tool, status,
            agent_id, indexed,
        } = *self;
        id == other.id && key == other.key.as_deref()
            && valid_at == other.valid_at
            && reported_recorded_at == other.reported_recorded_at
            && kind == other.kind && tool == other.tool.as_deref()
            && status == other.status && agent_id == other.agent_id
            && indexed == other.indexed
    }
    pub fn into_owned(self) -> Row {
        let Self {
            id, key, text, valid_at, reported_recorded_at, kind, tool, status,
            agent_id, indexed,
        } = self;
        Row {
            id: id.into(), key: key.map(str::to_owned), text: text.into(),
            valid_at, reported_recorded_at, kind, tool: tool.map(str::to_owned),
            status, agent_id: agent_id.into(), indexed,
        }
    }
}
enum Pending {
    Matched(usize),
    Fresh,
}
/// Consumes the old projection. Only rows compared in this invocation can reuse
/// its bytes/postings; no partial refresh can be searched or returned on error.
pub struct Refresh {
    old: Option<Index>,
    binding: Binding,
    expected: usize,
    order: Vec<Pending>,
    fresh: Vec<Row>,
    old_ids: BTreeMap<String, usize>,
    seen_old: Vec<bool>,
    ids: BTreeSet<String>,
    keys: BTreeSet<String>,
    bytes: usize,
    failure: Option<Error>,
}
impl Refresh {
    pub(super) fn new(old: Index, binding: Binding, expected: usize) -> Result<Self> {
        if binding.namespace != old.binding.namespace || binding.writer != old.binding.writer {
            return Err(Error::Scope);
        }
        if !name(&binding.writer, 256) || expected > MAX_RECORDS {
            return Err(Error::Bounds);
        }
        let old_ids = old.rows.iter().enumerate().map(|(i, row)| (row.id.clone(), i)).collect();
        let seen_old = vec![false; old.rows.len()];
        Ok(Self {
            old: Some(old), binding, expected, order: Vec::with_capacity(expected),
            fresh: Vec::new(), old_ids, seen_old, ids: BTreeSet::new(),
            keys: BTreeSet::new(), bytes: 0, failure: None,
        })
    }
    /// Validate all fields and fresh original bytes now, retaining no input borrow.
    /// An error poisons this builder and immediately drops its old/fresh text.
    pub fn push(&mut self, row: RowRef<'_>) -> Result<()> {
        match self.check(row)? {
            Some(at) => self.order.push(Pending::Matched(at)),
            None => {
                self.fresh.push(row.into_owned());
                self.order.push(Pending::Fresh);
            }
        }
        Ok(())
    }
    /// As push(), but move new/changed owned originals instead of copying them.
    pub fn push_owned(&mut self, row: Row) -> Result<()> {
        match self.check(row.as_borrowed())? {
            Some(at) => self.order.push(Pending::Matched(at)),
            None => {
                self.fresh.push(row);
                self.order.push(Pending::Fresh);
            }
        }
        Ok(())
    }
    fn check(&mut self, row: RowRef<'_>) -> Result<Option<usize>> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.check_inner(row);
        if let Err(error) = result {
            self.failure = Some(error);
            self.old = None;
            self.fresh = Vec::new();
            self.order = Vec::new();
            self.old_ids.clear();
            self.ids.clear();
            self.keys.clear();
        }
        result
    }
    fn check_inner(&mut self, row: RowRef<'_>) -> Result<Option<usize>> {
        if self.order.len() >= self.expected || !name(row.id, 1024)
            || row.key.is_some_and(|key| !name(key, 1024)) || row.text.len() > MAX_TEXT_BYTES
        {
            return Err(Error::Bounds);
        }
        if !self.ids.insert(row.id.into()) || row.key.is_some_and(|key| !self.keys.insert(key.into())) {
            return Err(Error::Conflict);
        }
        self.bytes = self.bytes.checked_add(json_size(&row, MAX_BATCH_BYTES)?).ok_or(Error::Bounds)?;
        if self.bytes > MAX_STATE_BYTES {
            return Err(Error::Bounds);
        }
        let old = self.old.as_ref().ok_or(Error::Unavailable)?;
        if let Some(&at) = self.old_ids.get(row.id) {
            if row.same_metadata(&old.rows[at]) && old.inner.record_text(at) == Some(row.text) {
                self.seen_old[at] = true;
                return Ok(Some(at));
            }
        }
        Ok(None)
    }
    /// Publish only a complete validated snapshot, preserving growth ordinals or
    /// rebuilding from the exact incoming order on replacement/deletion.
    pub fn finish(mut self) -> Result<Index> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.order.len() != self.expected {
            return Err(Error::Bounds);
        }
        let mut old = self.old.take().ok_or(Error::Unavailable)?;
        let unchanged = self.seen_old.iter().all(|seen| *seen);
        if self.binding.view == old.binding.view && (!unchanged || self.expected != old.rows.len()) {
            return Err(Error::Conflict);
        }
        if !unchanged {
            let mut fresh = self.fresh.into_iter();
            let mut rows = Vec::with_capacity(self.expected);
            for pending in self.order {
                rows.push(match pending {
                    Pending::Matched(at) => {
                        // This exact text and ALL fields matched a fresh push in
                        // this invocation. This is not proof from a prior request.
                        let mut row = old.rows.get(at).ok_or(Error::Authority)?.clone();
                        row.text = old.inner.record_text(at).ok_or(Error::Authority)?.into();
                        row
                    }
                    Pending::Fresh => fresh.next().ok_or(Error::Authority)?,
                });
            }
            drop(old);
            return Index::build(self.binding, rows);
        }
        let start = old.rows.len();
        old.inner.append_record_rows(
            self.fresh.iter().enumerate().map(|(i, row)| {
                ((start + i + 1) as u64, row.valid_at, row.text.as_str(), row.indexed)
            }), true,
        ).map_err(|_| Error::Unavailable)?;
        old.ids.extend(self.fresh.iter().map(|row| row.id.clone()));
        old.text_bytes += self.fresh.iter().map(|row| row.text.len()).sum::<usize>();
        for row in &mut self.fresh {
            row.text = String::new();
        }
        old.rows.extend(self.fresh);
        old.binding = self.binding;
        Ok(old)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matched_stream_retains_only_new_text_and_poison_drops_all_projection_text() {
        let binding = Binding { namespace: [7; 32], writer: "writer".into(), view: [8; 32] };
        let rows: Vec<_> = (0..64).map(|id| Row {
            id: id.to_string(), key: Some(id.to_string()), text: "battery quartz ".repeat(2048),
            valid_at: id, reported_recorded_at: -1, kind: Kind::Raw, tool: None,
            status: None, agent_id: String::new(), indexed: true,
        }).collect();
        let mut next = binding.clone();
        next.view[0] += 1;
        let old = Index::build(binding, rows.clone()).unwrap();
        let mut stream = old.begin_refresh(next, rows.len() + 3).unwrap();
        for row in rows.iter().rev() { stream.push(row.as_borrowed()).unwrap(); }
        assert!(stream.fresh.is_empty());
        assert_eq!(stream.order.len(), rows.len());
        assert!(stream.seen_old.iter().all(|seen| *seen));
        // Only ordinal markers and bounded duplicate metadata accompany old
        // originals: no complete incoming plaintext set is retained here.
        assert!(stream.order.iter().all(|row| matches!(row, Pending::Matched(_))));
        let mut fresh = rows[0].clone();
        fresh.id = "new".into();
        fresh.key = Some("new".into());
        stream.push(fresh.as_borrowed()).unwrap();
        assert_eq!(stream.fresh.len(), 1);
        assert_eq!(stream.fresh[0].text, fresh.text);
        fresh.text.clear();
        assert!(!stream.fresh[0].text.is_empty());
        let mut owned = rows[0].clone();
        owned.id = "owned-new".into();
        owned.key = Some("owned-new".into());
        let pointer = owned.text.as_ptr();
        stream.push_owned(owned).unwrap();
        assert_eq!(stream.fresh.len(), 2);
        assert_eq!(stream.fresh[1].text.as_ptr(), pointer);
        assert!(matches!(stream.push(fresh.as_borrowed()), Err(Error::Conflict)));
        assert!(stream.old.is_none());
        assert_eq!(stream.fresh.capacity(), 0);
        assert_eq!(stream.order.capacity(), 0);
        assert!(matches!(stream.finish(), Err(Error::Conflict)));
    }
}
