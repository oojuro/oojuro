//! Real managed binding output; no RecordLog or legacy Watermark is synthesized.
use super::*;
use crate::record::managed::{Binding, ManagedRankView};
#[derive(Serialize)]
pub struct ManagedSearch {
    pub binding: Binding,
    pub receipt: Receipt,
    pub hits: Vec<Hit>,
}
impl ManagedRankView<'_> {
    /// RAM query over genuine held current rows. Optional hooks remain OFF by default.
    pub fn search_ranked(
        &self,
        text: &str,
        k: usize,
        max_bytes: usize,
        pipeline: &Pipeline<'_>,
    ) -> Result<ManagedSearch> {
        self.validate_current()?;
        if max_bytes == 0
            || max_bytes > MAX_OUTPUT_BYTES
            || text.len() > lexical::MAX_QUERY_BYTES
            || k > 10_000
        {
            return Err(Error::Bounds);
        }
        let scope = self.scope();
        let q = Query {
            scope,
            as_of: self.as_of(),
            text,
            k,
            filter: self.filter().clone(),
            max_bytes,
        };
        let rows = self.lexical_records();
        let index = lexical::Index::from_record_rows(
            scope.writer,
            scope.owner,
            self.lexical_documents()
                .iter()
                .map(|r| (r.id, r.valid_at, r.text.as_str(), r.indexed)),
        )
        .map_err(|_| Error::Unavailable)?;
        let ids = rows.iter().map(|r| r.id).collect::<BTreeSet<_>>();
        let keys = self
            .latest_records()
            .filter_map(|r| r.key.as_ref().map(|key| (key.clone(), r.id)))
            .collect::<BTreeMap<_, _>>();
        let mask = lexical::RecordVisibility {
            ids: &ids,
            keys: &keys,
        };
        let baseline = || {
            let hits = index
                .search_record_view(
                    &lexical::Query {
                        writer: scope.writer,
                        agent: scope.owner,
                        text,
                        from: q.as_of.from,
                        to: q.as_of.valid_at,
                        k,
                    },
                    &mask,
                )
                .map_err(|_| Error::Bounds)?;
            let lookup = rows.iter().map(|r| (r.id, r)).collect::<BTreeMap<_, _>>();
            hits.into_iter()
                .map(|h| {
                    Ok(super::super::Hit {
                        record: (*lookup.get(&h.id).ok_or(Error::Authority)?).clone(),
                        bits: h.bits,
                        continued: h.continued,
                        row: h.row,
                    })
                })
                .collect::<Result<Vec<_>>>()
        };
        let validate = |table: &Loaded| rank_tables::managed::validate_loaded(self, table);
        let ranked = ram::run(
            &ram::Source {
                index: &index,
                rows,
                mask: &mask,
                query: &q,
                validate: &validate,
                baseline: &baseline,
            },
            pipeline,
        )?;
        self.validate_current()?;
        let result = ManagedSearch {
            binding: self.binding().clone(),
            receipt: ranked.receipt,
            hits: ranked.hits,
        };
        json(&result, max_bytes)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
