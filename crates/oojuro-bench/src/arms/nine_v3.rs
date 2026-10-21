//! The frozen v3 engine (main@7b98903, `oojuro_nine::legacy::V3`), built from
//! this tree: the baseline arm for the lexical track's v4 engine.
use super::COMMIT;
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL};
use oojuro_core::{Engine, Query, Record};
use oojuro_nine::legacy::V3 as Nine;
use std::{collections::BTreeMap, path::Path};

#[derive(Default)]
pub struct NineV3Arm {
    db: Option<Nine>,
}

impl NineV3Arm {
    fn db(&mut self) -> Result<&mut Nine> {
        self.db.as_mut().ok_or_else(|| "store not open".into())
    }
}

fn doc(r: &Record) -> Doc {
    Doc {
        id: r.id.clone(),
        key: if r.key == r.id { String::new() } else { r.key.clone() },
        text: r.text.clone(),
        valid_at: r.valid_at,
        recorded_at: r.recorded_at,
        meta: r.metadata.clone(),
    }
}

impl Arm for NineV3Arm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: "nine-v3".into(),
            version: format!(
                "{}:{}:{}",
                oojuro_nine::RETRIEVAL_MODEL,
                oojuro_nine::RETRIEVAL_COUNT_MODEL,
                oojuro_nine::RETRIEVAL_NORMALIZATION
            ),
            commit: COMMIT.into(),
            durability: "journal append + sync_data per record; each search also fsyncs a query receipt".into(),
            protocol: PROTOCOL.into(),
            notes: vec!["search_refs(from=i64::MIN); max_context_bytes unbounded; in-engine query cache on".into()],
        }
    }
    fn open(&mut self, dir: &Path, _config: &BTreeMap<String, String>) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        self.db = Some(Nine::open(dir)?);
        Ok(())
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        let db = self.db()?;
        for d in docs {
            let mut r = Record::new(&d.id, d.key(), &d.text, d.valid_at);
            r.recorded_at = d.recorded_at;
            r.verified = false;
            r.metadata = d.meta.clone();
            db.insert(r)?;
        }
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        let mut q = Query::new(&query.text, query.valid_at);
        q.known_at = query.known_at;
        q.k = query.k;
        q.max_context_bytes = usize::MAX;
        let found = self.db()?.search_refs(&q, i64::MIN)?;
        Ok(found.matches.into_iter().map(|m| Hit { id: m.id, score: m.score_bits }).collect())
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        let db = self.db()?;
        Ok(ids.iter().map(|id| db.read(id).map(str::to_string)).collect())
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        Ok(self.db()?.range(from, to, limit)?.into_iter().map(doc).collect())
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(self.db()?.stats()?)?)
    }
    fn close(&mut self) -> Result<()> {
        self.db = None;
        Ok(())
    }
}
