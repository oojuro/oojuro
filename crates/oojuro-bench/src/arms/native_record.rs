//! Current sealed RecordLog + the disposable projection used by hosted search.
//! Benchmark source IDs/clocks are imported attributes, never engine-assigned clocks.
use super::COMMIT;
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL};
use oojuro_engine::{
    capture_log::Identity,
    record::{
        self, AsOf, Filter, Input, Kind, Limits, Record, RecordLog, Scope, Versions, View,
        projection,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const WRITER: &str = "oojuro-bench-import";
// Public benchmark fixture key; this is not production key management or confidentiality.
const KEY: [u8; 32] = [42; 32];

#[derive(Default)]
pub struct NativeRecordArm {
    log: Option<RecordLog>,
    ids: BTreeMap<String, u64>,
}

fn native<T>(result: record::Result<T>) -> Result<T> {
    result.map_err(|e| format!("native record: {e:?}").into())
}
fn scope() -> Scope<'static> {
    Scope {
        writer: WRITER,
        owner: 1,
    }
}
fn micros(seconds: i64) -> Result<i64> {
    seconds
        .checked_mul(1_000_000)
        .ok_or_else(|| "benchmark timestamp exceeds native microsecond range".into())
}
fn document(row: &Record) -> Result<Doc> {
    if row.valid_at % 1_000_000 != 0 {
        return Err("invalid imported timestamp units".into());
    }
    Ok(Doc {
        id: row
            .attrs
            .get("benchmark_id")
            .and_then(|v| v.as_str())
            .ok_or("missing imported ID")?
            .into(),
        key: row
            .attrs
            .get("benchmark_key")
            .and_then(|v| v.as_str())
            .ok_or("missing imported key")?
            .into(),
        text: row.text.clone(),
        valid_at: row.valid_at / 1_000_000,
        recorded_at: row
            .attrs
            .get("benchmark_known_at")
            .and_then(|v| v.as_i64())
            .ok_or("missing imported clock")?,
        meta: serde_json::from_value(
            row.attrs
                .get("benchmark_meta")
                .ok_or("missing imported metadata")?
                .clone(),
        )?,
    })
}

impl NativeRecordArm {
    fn log(&self) -> Result<&RecordLog> {
        self.log.as_ref().ok_or_else(|| "store not open".into())
    }
    fn records(&self) -> Result<Vec<Record>> {
        let log = self.log()?;
        let view = native(View::open(log, scope(), AsOf::default()))?;
        let limits = Limits {
            entries: 1000,
            bytes: record::MAX_OUTPUT_BYTES,
        };
        let mut page = native(view.range(log, Versions::All, Filter::default(), limits))?;
        let mut rows = Vec::new();
        loop {
            rows.extend(page.records);
            let Some(cursor) = page.next else { break };
            page = native(View::continue_page(log, scope(), &cursor, limits))?;
        }
        Ok(rows)
    }
}

impl Arm for NativeRecordArm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: "native-record".into(), version: "RecordLog sealed record-2 + current record::projection".into(),
            commit: COMMIT.into(), protocol: PROTOCOL.into(),
            durability: "native sealed Store sync=true; one durable append per <=1000 records; one host, no replication/backups".into(),
            notes: vec![
                "actual native RecordLog and production projection; snapshot resolution, original reads, projection rebuild and excerpts are inside each search".into(),
                "source IDs/known-at clocks and metadata persist as explicit benchmark import attributes; seconds convert to native microseconds; native assigned IDs/ingest clocks remain genuine and unchanged".into(),
                "native local Store compression/encryption differs from hosted per-object Zstd/S3/KMS; public fixture key, no hosted footprint/cost or failure-domain claim".into(),
                "RecordLog Store defaults: hot Zstd 3, dictionary cap 512KiB trained after 4MiB, hot-file target 64MiB, decode cache 4MiB; no explicit cold compaction in this arm".into(),
                "no persistent/warm search index; native 100k-record/128MiB state bounds apply; >1000 input batches split into native commits and can recover partially before ACK".into(),
            ],
        }
    }
    fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<()> {
        if !config.is_empty() {
            return Err("native-record accepts no tuning configuration".into());
        }
        self.close()?;
        std::fs::create_dir_all(dir)?;
        self.log = Some(native(RecordLog::open(
            &dir.join("records"),
            &KEY,
            Identity {
                writer: WRITER.into(),
                agent: 1,
                first_id: 1,
                adapter: "benchmark-import-v2".into(),
            },
        ))?);
        for row in self.records()? {
            let doc = document(&row)?;
            if self.ids.insert(doc.id, row.id).is_some() {
                return Err("duplicate imported ID".into());
            }
        }
        Ok(())
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        self.log()?;
        let mut seen = BTreeSet::new();
        let inputs: Vec<_> = docs
            .iter()
            .map(|d| {
                if self.ids.contains_key(&d.id) || !seen.insert(d.id.as_str()) {
                    return Err("duplicate imported ID".into());
                }
                micros(d.recorded_at)?;
                let input = Input {
                    agent_id: "benchmark".into(),
                    parent_id: None,
                    valid_at: micros(d.valid_at)?,
                    kind: Kind::Raw,
                    tool: None,
                    status: None,
                    exit: None,
                    key: Some(d.key().into()),
                    text: d.text.clone(),
                    indexed: true,
                    attrs: BTreeMap::from([
                        ("benchmark_id".into(), serde_json::json!(d.id)),
                        ("benchmark_key".into(), serde_json::json!(d.key)),
                        (
                            "benchmark_known_at".into(),
                            serde_json::json!(d.recorded_at),
                        ),
                        ("benchmark_meta".into(), serde_json::to_value(&d.meta)?),
                    ]),
                };
                native(input.validate())?;
                Ok(input)
            })
            .collect::<Result<_>>()?;
        let request = blake3::hash(&serde_json::to_vec(docs)?)
            .to_hex()
            .to_string();
        for (part, chunk) in inputs.chunks(1000).enumerate() {
            let commit = native(
                self.log
                    .as_mut()
                    .ok_or("store not open")?
                    .append(&format!("{request}:{part}"), chunk),
            )?;
            for (doc, id) in docs[part * 1000..].iter().zip(commit.ids) {
                self.ids.insert(doc.id.clone(), id);
            }
        }
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        if query.text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut winners: BTreeMap<String, (u64, Doc)> = BTreeMap::new();
        for row in self.records()? {
            let doc = document(&row)?;
            if doc.valid_at > query.valid_at || doc.recorded_at > query.known_at {
                continue;
            }
            let order = (doc.valid_at, doc.recorded_at, row.id);
            if winners
                .get(doc.key())
                .is_none_or(|(seq, old)| order > (old.valid_at, old.recorded_at, *seq))
            {
                winners.insert(doc.key().into(), (row.id, doc));
            }
        }
        let view = native(View::open(self.log()?, scope(), AsOf::default()))?;
        let binding = projection::Binding {
            namespace: view.watermark().origin,
            writer: WRITER.into(),
            view: *blake3::hash(&serde_json::to_vec(view.watermark())?).as_bytes(),
        };
        let rows = winners
            .into_values()
            .map(|(_, d)| {
                Ok(projection::Row {
                    key: Some(d.key().into()),
                    id: d.id,
                    text: d.text,
                    valid_at: micros(d.valid_at)?,
                    reported_recorded_at: micros(d.recorded_at)?,
                    kind: Kind::Raw,
                    tool: None,
                    status: None,
                    agent_id: "benchmark".into(),
                    indexed: true,
                })
            })
            .collect::<Result<_>>()?;
        let index = native(projection::Index::build(binding.clone(), rows))?;
        let hits = native(index.search(
            &binding,
            &projection::Query {
                text: &query.text,
                k: query.k,
                from: i64::MIN,
                to: query.valid_at.saturating_mul(1_000_000),
                filter: Filter::default(),
                excerpt_bytes: 320,
                max_bytes: record::MAX_OUTPUT_BYTES,
            },
        ))?;
        Ok(hits
            .into_iter()
            .map(|h| Hit {
                id: h.id,
                score: h.bits,
            })
            .collect())
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        let log = self.log()?;
        let view = native(View::open(log, scope(), AsOf::default()))?;
        ids.iter()
            .map(|id| match self.ids.get(id) {
                None => Ok(None),
                Some(id) => {
                    Ok(native(view.get_id(log, *id, record::MAX_OUTPUT_BYTES))?.map(|r| r.text))
                }
            })
            .collect()
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        let mut rows = self.records()?;
        rows.retain(|r| {
            r.valid_at >= from.saturating_mul(1_000_000)
                && r.valid_at <= to.saturating_mul(1_000_000)
        });
        rows.sort_by_key(|r| (r.valid_at, r.id));
        rows.iter().take(limit).map(document).collect()
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        let view = native(View::open(self.log()?, scope(), AsOf::default()))?;
        Ok(
            serde_json::json!({"records":view.watermark().records,"projection_rebuilt_per_search":true}),
        )
    }
    fn close(&mut self) -> Result<()> {
        self.log = None;
        self.ids.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_preserve_originals_without_replacing_native_ingest_clock() {
        let dir = std::env::temp_dir().join(format!(
            "oojuro-native-arm-{}-{}",
            std::process::id(),
            crate::sys::unique_suffix()
        ));
        let mut arm = NativeRecordArm::default();
        arm.open(&dir, &BTreeMap::new()).unwrap();
        let doc = Doc {
            id: "original".into(),
            key: String::new(),
            text: "\u{feff}héllo\0\r\n exact original".into(),
            valid_at: -100,
            recorded_at: -50,
            meta: BTreeMap::from([("kind".into(), "tool_result".into())]),
        };
        arm.append_batch(std::slice::from_ref(&doc)).unwrap();
        let rows = arm.records().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].recorded_at > 0);
        assert_ne!(rows[0].recorded_at, doc.recorded_at);
        assert_eq!(rows[0].valid_at, doc.valid_at * 1_000_000);
        assert_eq!(document(&rows[0]).unwrap(), doc);
        assert!(arm.append_batch(std::slice::from_ref(&doc)).is_err());
        assert_eq!(arm.records().unwrap().len(), 1);
        arm.close().unwrap();
        arm.open(&dir, &BTreeMap::new()).unwrap();
        assert_eq!(
            arm.get_range(i64::MIN, i64::MAX, 10).unwrap(),
            vec![doc.clone()]
        );
        assert_eq!(
            arm.get(&[doc.id.clone(), "missing".into()]).unwrap(),
            vec![Some(doc.text), None]
        );
        let query = SearchReq {
            text: "original".into(),
            valid_at: -100,
            known_at: -51,
            k: 10,
            context: BTreeMap::new(),
        };
        assert!(arm.search(&query).unwrap().is_empty());
        let mut query = query;
        query.known_at = -50;
        assert_eq!(arm.search(&query).unwrap()[0].id, "original");
        arm.close().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
