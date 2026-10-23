//! Pure disposable query projection of a caller-resolved immutable snapshot.
//! Binding is a caller assertion, not proof of remote commit/auth/freshness.
//! No files, clock, Store append, local receipt, DEK or independent authority.
use super::*;
use crate::lexical::{self, RecordVisibility, passage};
use std::collections::BTreeSet;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub namespace: [u8; 32],
    pub writer: String,
    pub view: [u8; 32],
}
#[derive(Clone, PartialEq, Serialize)]
pub struct Row {
    pub id: String,
    pub key: Option<String>,
    pub text: String,
    pub valid_at: i64,
    /// Opaque imported metadata; never promoted to an engine-assigned clock.
    pub reported_recorded_at: i64,
    pub kind: Kind,
    pub tool: Option<String>,
    pub status: Option<Status>,
    pub agent_id: String,
    pub indexed: bool,
}
pub struct Query<'a> {
    pub text: &'a str,
    pub k: usize,
    pub from: i64,
    pub to: i64,
    pub filter: Filter,
    pub excerpt_bytes: usize,
    pub max_bytes: usize,
}
#[derive(Serialize)]
pub struct Hit {
    pub id: String,
    pub bits: f64,
    pub continued: bool,
    pub valid_at: i64,
    pub reported_recorded_at: i64,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
}
/// Immutable in-memory view. Caller revalidates authority/keys/grants before
/// every reuse and before delivering each response frame; this API cannot do so.
pub struct Index {
    binding: Binding,
    // Text is owned once by inner; these rows retain the other source fields.
    rows: Vec<Row>,
    text_bytes: usize,
    ids: Vec<String>,
    inner: lexical::Index,
}
impl Index {
    /// Source rows retained by this disposable projection, including unindexed rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
    /// Retained source text bytes; posting/allocation overhead is additional.
    pub fn text_bytes(&self) -> usize {
        self.text_bytes
    }
    fn validate(binding: &Binding, rows: &[Row]) -> Result<()> {
        if !name(&binding.writer, 256) || rows.len() > MAX_RECORDS {
            return Err(Error::Bounds);
        }
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut bytes = 0usize;
        for r in rows {
            if !name(&r.id, 1024)
                || r.key.as_ref().is_some_and(|k| !name(k, 1024))
                || r.text.len() > MAX_TEXT_BYTES
            {
                return Err(Error::Bounds);
            }
            if !ids.insert(&r.id) || r.key.as_ref().is_some_and(|k| !keys.insert(k)) {
                return Err(Error::Conflict);
            }
            bytes = bytes
                .checked_add(json_size(r, MAX_BATCH_BYTES)?)
                .ok_or(Error::Bounds)?;
            if bytes > MAX_STATE_BYTES {
                return Err(Error::Bounds);
            }
        }
        Ok(())
    }
    pub fn build(binding: Binding, mut rows: Vec<Row>) -> Result<Self> {
        Self::validate(&binding, &rows)?;
        let text_bytes = rows.iter().map(|row| row.text.len()).sum();
        let ids = rows.iter().map(|r| r.id.clone()).collect();
        // Ordinals are private posting handles, never public source identities.
        let inner = lexical::Index::from_record_rows(
            &binding.writer,
            1,
            rows.iter()
                .enumerate()
                .map(|(i, r)| ((i + 1) as u64, r.valid_at, r.text.as_str(), r.indexed)),
        )
        .map_err(|_| Error::Unavailable)?;
        // Lexical carriers own the exact originals; retain metadata here once.
        for row in &mut rows {
            row.text = String::new();
        }
        Ok(Self {
            binding,
            rows,
            text_bytes,
            ids,
            inner,
        })
    }
    /// Consume the old projection and resolve the complete caller-supplied live
    /// snapshot again. Only byte/metadata-identical retained rows reuse postings.
    /// Replacements, deletions and filter changes rebuild; errors consume the old
    /// value. This is derived work reuse, never a current-authority substitute.
    pub fn refresh(mut self, binding: Binding, mut rows: Vec<Row>) -> Result<Self> {
        if binding.namespace != self.binding.namespace || binding.writer != self.binding.writer {
            return Err(Error::Scope);
        }
        Self::validate(&binding, &rows)?;
        let texts: Vec<_> = rows
            .iter_mut()
            .map(|row| std::mem::take(&mut row.text))
            .collect();
        let unchanged = {
            let incoming: std::collections::BTreeMap<_, _> = rows
                .iter()
                .enumerate()
                .map(|(i, row)| (row.id.as_str(), (i, row)))
                .collect();
            self.rows.iter().enumerate().all(|(old_i, old)| {
                incoming.get(old.id.as_str()).is_some_and(|(i, row)| {
                    *row == old && self.inner.record_text(old_i) == Some(texts[*i].as_str())
                })
            })
        };
        if binding.view == self.binding.view && (!unchanged || rows.len() != self.rows.len()) {
            return Err(Error::Conflict);
        }
        if !unchanged {
            drop(self);
            for (row, text) in rows.iter_mut().zip(texts) {
                row.text = text;
            }
            return Self::build(binding, rows);
        }
        let old_ids: BTreeSet<_> = self.ids.iter().map(String::as_str).collect();
        let mut added: Vec<_> = rows
            .into_iter()
            .zip(texts)
            .filter(|(row, _)| !old_ids.contains(row.id.as_str()))
            .map(|(mut row, text)| {
                row.text = text;
                row
            })
            .collect();
        drop(old_ids);
        let start = self.rows.len();
        self.inner
            .append_record_rows(
                added.iter().enumerate().map(|(i, row)| {
                    (
                        (start + i + 1) as u64,
                        row.valid_at,
                        row.text.as_str(),
                        row.indexed,
                    )
                }),
                true,
            )
            .map_err(|_| Error::Unavailable)?;
        self.ids.extend(added.iter().map(|row| row.id.clone()));
        self.text_bytes += added.iter().map(|row| row.text.len()).sum::<usize>();
        for row in &mut added {
            row.text = String::new();
        }
        self.rows.extend(added);
        self.binding = binding;
        Ok(self)
    }
    pub fn search(&self, binding: &Binding, q: &Query<'_>) -> Result<Vec<Hit>> {
        if binding != &self.binding {
            return Err(Error::Scope);
        }
        if q.text.trim().is_empty()
            || q.text.len() > lexical::MAX_QUERY_BYTES
            || q.k > 10000
            || q.excerpt_bytes == 0
            || q.excerpt_bytes > 65536
            || q.max_bytes == 0
            || q.max_bytes > MAX_OUTPUT_BYTES
        {
            return Err(Error::Bounds);
        }
        q.filter.validate()?;
        let eligible = |r: &Row| {
            r.indexed
                && q.filter.kind.is_none_or(|k| r.kind == k)
                && q.filter
                    .tool
                    .as_ref()
                    .is_none_or(|t| r.tool.as_ref() == Some(t))
                && q.filter.status.is_none_or(|s| r.status == Some(s))
                && q.filter.agent_id.as_ref().is_none_or(|a| r.agent_id == *a)
        };
        let ids = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| eligible(r))
            .map(|(i, _)| (i + 1) as u64)
            .collect();
        // Keys are already resolved by the caller's committed snapshot. Duplicate
        // keys are refused; do not reinterpret its memory winner using event time.
        let keys = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.key.as_ref().map(|k| (k.clone(), (i + 1) as u64)))
            .collect();
        let mask = RecordVisibility {
            ids: &ids,
            keys: &keys,
        };
        let hits = self
            .inner
            .search_projected(
                &lexical::Query {
                    writer: &binding.writer,
                    agent: 1,
                    text: q.text,
                    from: q.from,
                    to: q.to,
                    k: q.k,
                },
                &mask,
                &self.ids,
            )
            .map_err(|_| Error::Unavailable)?;
        let mut output = vec![];
        for h in hits {
            let at = usize::try_from(h.id.checked_sub(1).ok_or(Error::Authority)?)
                .map_err(|_| Error::Authority)?;
            let row = self.rows.get(at).ok_or(Error::Authority)?;
            let text = self.inner.record_text(at).ok_or(Error::Authority)?;
            let (start, end) = match passage::extract_passage(text, q.text, q.excerpt_bytes)? {
                Some(p) => (p.start, p.end),
                None => {
                    let mut end = text.len().min(q.excerpt_bytes);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    (0, end)
                }
            };
            output.push(Hit {
                id: row.id.clone(),
                bits: h.bits,
                continued: h.continued,
                valid_at: row.valid_at,
                reported_recorded_at: row.reported_recorded_at,
                start,
                end,
                excerpt: text[start..end].into(),
            });
        }
        json_size(&output, q.max_bytes)?;
        Ok(output)
    }
}
#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
