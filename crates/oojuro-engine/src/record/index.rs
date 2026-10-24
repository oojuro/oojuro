//! Version visibility precedes every scorer stage; legacy arithmetic remains shared.
use super::*;
use crate::lexical::{self, RecordVisibility};
use std::collections::BTreeMap;
pub struct Query<'a> {
    pub scope: Scope<'a>,
    pub as_of: AsOf,
    pub text: &'a str,
    pub k: usize,
    pub filter: Filter,
    pub max_bytes: usize,
}
#[derive(Serialize)]
pub struct Hit {
    pub record: Record,
    pub bits: f64,
    pub continued: bool,
    pub row: u64,
}
#[derive(Serialize)]
pub struct Search {
    pub watermark: Watermark,
    pub hits: Vec<Hit>,
}
/// Disposable memory projection; each read independently validates its authority.
pub struct Index {
    pub(super) inner: lexical::Index,
    pub(super) mark: Watermark,
    pub(super) failed: bool,
}
impl Index {
    pub fn rebuild(log: &RecordLog) -> Result<Self> {
        let mark = log.tip()?;
        let identity = log.identity();
        let inner = lexical::Index::borrowed_record_rows(
            &identity.writer,
            identity.agent,
            log.records.iter().map(|r| {
                (
                    r.id,
                    r.input.valid_at,
                    r.input.text.as_str(),
                    r.input.indexed,
                )
            }),
            true,
        )
        .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            inner,
            mark,
            failed: false,
        })
    }
    pub fn refresh(&mut self, log: &RecordLog) -> Result<()> {
        match log.validate(&self.mark).and_then(|_| Self::rebuild(log)) {
            Ok(next) => {
                *self = next;
                Ok(())
            }
            Err(e) => {
                self.failed = true;
                Err(e)
            }
        }
    }
    pub fn search(&self, log: &RecordLog, query: &Query<'_>) -> Result<Search> {
        let view = View::open(log, query.scope, query.as_of)?;
        self.search_view(log, &view, query)
    }
    pub(super) fn original_text(log: &RecordLog, row: usize, id: u64) -> lexical::Result<&str> {
        log.records
            .get(row)
            .filter(|record| record.id == id)
            .map(|record| record.input.text.as_str())
            .ok_or(lexical::Error::Authority)
    }
    pub fn search_view(&self, log: &RecordLog, view: &View, query: &Query<'_>) -> Result<Search> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        if query.max_bytes == 0 || query.max_bytes > view::MAX_OUTPUT_BYTES {
            return Err(Error::Bounds);
        }
        let current = log.tip()?;
        log.validate(&self.mark)?;
        if current.sequence != self.mark.sequence || current.records != self.mark.records {
            return Err(Error::Unavailable);
        }
        let identity = log.identity();
        if query.scope.writer != identity.writer || query.scope.owner != identity.agent {
            return Err(Error::Scope);
        }
        if view.as_of() != query.as_of {
            return Err(Error::Invalid);
        }
        let visible = view.visible(log, &query.filter)?;
        let keys = view.keys(log)?;
        let original_text = |row, id| Self::original_text(log, row, id);
        let mask = RecordVisibility {
            ids: &visible,
            keys: &keys,
            original_text: Some(&original_text),
        };
        let hits = self
            .inner
            .search_record_view(
                &lexical::Query {
                    writer: query.scope.writer,
                    agent: query.scope.owner,
                    text: query.text,
                    from: query.as_of.from,
                    to: query.as_of.valid_at,
                    k: query.k,
                },
                &mask,
            )
            .map_err(|e| match e {
                lexical::Error::Invalid => Error::Bounds,
                _ => Error::Unavailable,
            })?;
        let rows: BTreeMap<_, _> = log.records.iter().map(|r| (r.id, r)).collect();
        let hits = hits
            .into_iter()
            .map(|h| {
                Ok(Hit {
                    record: rows
                        .get(&h.id)
                        .ok_or(Error::Authority)?
                        .record(query.scope.writer, &identity.adapter),
                    bits: h.bits,
                    continued: h.continued,
                    row: h.row,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let result = Search {
            watermark: view.watermark().clone(),
            hits,
        };
        json_size(&result, query.max_bytes)?;
        Ok(result)
    }
}
