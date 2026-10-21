//! The engine as built from this source tree (oojuro-nine). On another
//! branch this arm measures that branch's engine.
//!
//! track/lexical (v4 engine): one `append_batch` is one atomic, durable
//! group commit (one sync). `--config` keys map to engine options:
//! `thinking=store` (thinking records stored exactly but not indexed),
//! `background=candidates|view`, `mu=F`, `receipts=buffered|durable|off`,
//! `threads=N` (query threads), `merge_fanout=N`, `flush_mb=N`.
use super::COMMIT;
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL};
use oojuro_core::{Query, Record};
use oojuro_nine::{Background, Nine, Options, Receipts};
use std::{collections::BTreeMap, path::Path};

#[derive(Default)]
pub struct NineArm {
    db: Option<Nine>,
    store_thinking: bool,
}

impl NineArm {
    fn db(&mut self) -> Result<&mut Nine> {
        self.db.as_mut().ok_or_else(|| "store not open".into())
    }
}

fn doc(r: &Record) -> Doc {
    Doc {
        id: r.id.clone(),
        key: if r.key == r.id {
            String::new()
        } else {
            r.key.clone()
        },
        text: r.text.clone(),
        valid_at: r.valid_at,
        recorded_at: r.recorded_at,
        meta: r.metadata.clone(),
    }
}

fn options(config: &BTreeMap<String, String>) -> Result<Options> {
    let mut o = Options::default();
    for (k, v) in config {
        match k.as_str() {
            "thinking" => {}
            "background" => {
                o.background = match v.as_str() {
                    "candidates" => Background::Candidates,
                    "view" => Background::View,
                    _ => return Err(format!("background={v}: use candidates or view").into()),
                }
            }
            "mu" => o.mu = v.parse()?,
            "receipts" => {
                o.receipts = match v.as_str() {
                    "buffered" => Receipts::Buffered,
                    "durable" => Receipts::Durable,
                    "off" => Receipts::Off,
                    _ => return Err(format!("receipts={v}: use buffered, durable or off").into()),
                }
            }
            "threads" => o.query_threads = v.parse()?,
            "merge_fanout" => o.merge_fanout = v.parse()?,
            "flush_mb" => o.flush_text_bytes = v.parse::<usize>()? << 20,
            _ => {}
        }
    }
    Ok(o)
}

impl Arm for NineArm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: "nine".into(),
            version: format!(
                "{}:{}:{}",
                oojuro_nine::RETRIEVAL_MODEL,
                oojuro_nine::RETRIEVAL_COUNT_MODEL,
                oojuro_nine::RETRIEVAL_NORMALIZATION
            ),
            commit: COMMIT.into(),
            durability: "v4: one journal write + sync_data per append_batch (group commit); \
                query receipts buffered (synced on close)"
                .into(),
            protocol: PROTOCOL.into(),
            notes: vec![
                "search_refs(from=i64::MIN); max_context_bytes unbounded; in-engine query cache on"
                    .into(),
            ],
        }
    }
    fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        self.store_thinking = config.get("thinking").is_some_and(|v| v == "store");
        self.db = None;
        self.db = Some(Nine::open_with(dir, options(config)?)?);
        Ok(())
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        let store_thinking = self.store_thinking;
        let records = docs
            .iter()
            .map(|d| {
                let mut r = Record::new(&d.id, d.key(), &d.text, d.valid_at);
                r.recorded_at = d.recorded_at;
                r.verified = false;
                r.metadata = d.meta.clone();
                let indexed =
                    !(store_thinking && d.meta.get("kind").is_some_and(|k| k == "thinking"));
                (r, indexed)
            })
            .collect();
        self.db()?.insert_batch_indexed(records)?;
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        let mut q = Query::new(&query.text, query.valid_at);
        q.known_at = query.known_at;
        q.k = query.k;
        q.max_context_bytes = usize::MAX;
        let found = self.db()?.search_refs(&q, i64::MIN)?;
        Ok(found
            .matches
            .into_iter()
            .map(|m| Hit {
                id: m.id,
                score: m.score_bits,
            })
            .collect())
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        let db = self.db()?;
        ids.iter().map(|id| db.read(id)).collect()
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        Ok(self
            .db()?
            .range(from, to, limit)?
            .iter()
            .map(doc)
            .collect())
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(self.db()?.stats()?)?)
    }
    fn close(&mut self) -> Result<()> {
        self.db = None;
        Ok(())
    }
}
