//! Indexed JSONL baseline: one immutable frame per acknowledged batch, raw or
//! Zstd level 3. SQLite holds offsets/metadata and a contentless FTS5 index.
use super::{
    COMMIT,
    sqlite::{SEARCH, match_expression},
};
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL, sys};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS format(codec INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS docs(
  seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, key TEXT NOT NULL,
  valid_at INTEGER NOT NULL, recorded_at INTEGER NOT NULL,
  frame TEXT NOT NULL, offset INTEGER NOT NULL, length INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS docs_key ON docs(key, valid_at, recorded_at, seq);
CREATE INDEX IF NOT EXISTS docs_time ON docs(valid_at, seq);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
  text, content='', contentless_delete=1, tokenize='porter unicode61');
";

pub struct JsonlArm {
    compressed: bool,
    db: Option<Connection>,
    directory: PathBuf,
}

impl JsonlArm {
    pub fn new(compressed: bool) -> Self {
        Self {
            compressed,
            db: None,
            directory: PathBuf::new(),
        }
    }
    fn db(&self) -> Result<&Connection> {
        self.db.as_ref().ok_or_else(|| "store not open".into())
    }
    fn read_docs(&self, ids: &[String]) -> Result<Vec<Option<Doc>>> {
        let mut frames = BTreeMap::new();
        let mut stmt = self
            .db()?
            .prepare_cached("SELECT frame, offset, length FROM docs WHERE id=?1")?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let found = stmt
                .query_row([id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .optional()?;
            let Some((frame, offset, length)) = found else {
                out.push(None);
                continue;
            };
            let offset = usize::try_from(offset)?;
            let length = usize::try_from(length)?;
            if frame.len() != 64
                || !frame
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err("invalid JSONL frame identity".into());
            }
            if !frames.contains_key(&frame) {
                let encoded = std::fs::read(self.directory.join("payloads").join(&frame))?;
                if blake3::hash(&encoded).to_hex().as_str() != frame {
                    return Err("JSONL frame checksum mismatch".into());
                }
                let plain = if self.compressed {
                    zstd::stream::decode_all(encoded.as_slice())?
                } else {
                    encoded
                };
                frames.insert(frame.clone(), plain);
            }
            let plain = &frames[&frame];
            let end = offset.checked_add(length).ok_or("invalid JSONL offset")?;
            let doc: Doc =
                serde_json::from_slice(plain.get(offset..end).ok_or("invalid JSONL range")?)?;
            if &doc.id != id {
                return Err("JSONL index identity mismatch".into());
            }
            out.push(Some(doc));
        }
        Ok(out)
    }
}

impl Arm for JsonlArm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: if self.compressed { "jsonl-zstd-fts5" } else { "jsonl-fts5" }.into(),
            version: format!("Zstd {}; SQLite {} contentless FTS5 porter unicode61 bm25", zstd::zstd_safe::version_string(), rusqlite::version()),
            commit: COMMIT.into(),
            durability: "immutable batch file fsync + directory fsync, then SQLite WAL synchronous=FULL commit; one host, no replication/backups/encryption".into(),
            protocol: PROTOCOL.into(),
            notes: vec![
                "one JSONL frame per batch; no dictionary or deduplication; level 3 when compressed".into(),
                "same OR-token query/ranking and two-clock SQL visibility as sqlite arm; FTS5 stores no original text".into(),
                "all payloads, index, WAL/SHM and uncommitted orphan frames count toward footprint; no automatic orphan reclamation".into(),
                "get decodes each selected batch frame once per request; no retained payload cache".into(),
            ],
        }
    }
    fn open(&mut self, dir: &Path, _config: &BTreeMap<String, String>) -> Result<()> {
        if self.db.is_some() {
            self.close()?;
        }
        std::fs::create_dir_all(dir.join("payloads"))?;
        File::open(dir)?.sync_all()?;
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        let mut db = Connection::open(dir.join("index.sqlite"))?;
        db.execute_batch(SCHEMA)?;
        let tx = db.transaction()?;
        let codecs = tx
            .prepare("SELECT codec FROM format")?
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let codec = if self.compressed { 3 } else { 0 };
        match codecs.as_slice() {
            [] => {
                tx.execute("INSERT INTO format(codec) VALUES (?1)", [codec])?;
            }
            [value] if *value == codec => {}
            _ => return Err("JSONL codec differs from the retained store".into()),
        }
        tx.commit()?;
        File::open(dir)?.sync_all()?;
        self.directory = dir.into();
        self.db = Some(db);
        Ok(())
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        self.db()?;
        let mut plain = Vec::new();
        let mut offsets = Vec::with_capacity(docs.len());
        for doc in docs {
            let offset = plain.len();
            serde_json::to_writer(&mut plain, doc)?;
            offsets.push((offset, plain.len() - offset));
            plain.push(b'\n');
        }
        let encoded = if self.compressed {
            zstd::stream::encode_all(plain.as_slice(), 3)?
        } else {
            plain
        };
        let frame = blake3::hash(&encoded).to_hex().to_string();
        let payloads = self.directory.join("payloads");
        let path = payloads.join(&frame);
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&encoded)?;
                file
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path)? != encoded {
                    return Err("existing JSONL frame differs".into());
                }
                OpenOptions::new().write(true).open(&path)?
            }
            Err(e) => return Err(e.into()),
        };
        file.flush()?;
        file.sync_all()?;
        File::open(payloads)?.sync_all()?;
        // Publishing offsets only after durable payloads makes interrupted frames
        // harmless orphans; a committed index never points at an unflushed frame.
        let tx = self.db.as_mut().ok_or("store not open")?.transaction()?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO docs(id,key,valid_at,recorded_at,frame,offset,length) VALUES (?1,?2,?3,?4,?5,?6,?7)")?;
            let mut index = tx.prepare_cached("INSERT INTO fts(rowid,text) VALUES (?1,?2)")?;
            for (doc, (offset, length)) in docs.iter().zip(offsets) {
                let seq = insert.insert(params![
                    doc.id,
                    doc.key(),
                    doc.valid_at,
                    doc.recorded_at,
                    frame,
                    offset as i64,
                    length as i64
                ])?;
                index.execute(params![seq, doc.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        let Some(expression) = match_expression(&query.text) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.db()?.prepare_cached(SEARCH)?;
        let hits = stmt.query_map(
            params![expression, query.valid_at, query.known_at, query.k as i64],
            |r| {
                Ok(Hit {
                    id: r.get(0)?,
                    score: -r.get::<_, f64>(1)?,
                })
            },
        )?;
        Ok(hits.collect::<std::result::Result<_, _>>()?)
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        Ok(self
            .read_docs(ids)?
            .into_iter()
            .map(|doc| doc.map(|d| d.text))
            .collect())
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        let ids = self.db()?.prepare_cached("SELECT id FROM docs WHERE valid_at BETWEEN ?1 AND ?2 ORDER BY valid_at,seq LIMIT ?3")?
            .query_map(params![from, to, limit as i64], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?;
        self.read_docs(&ids)?
            .into_iter()
            .map(|d| d.ok_or_else(|| "missing indexed JSONL document".into()))
            .collect()
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        let records: i64 = self
            .db()?
            .query_row("SELECT count(*) FROM docs", [], |r| r.get(0))?;
        let referenced: i64 =
            self.db()?
                .query_row("SELECT count(DISTINCT frame) FROM docs", [], |r| r.get(0))?;
        let frames = std::fs::read_dir(self.directory.join("payloads"))?.count() as u64;
        Ok(
            serde_json::json!({"records":records,"codec_level":if self.compressed {3} else {0},
            "payload_files":frames,"referenced_frames":referenced,"orphan_frames":frames.saturating_sub(u64::try_from(referenced)?),
            "payload_bytes":sys::dir_bytes(&self.directory.join("payloads")).0,
            "whole_store_bytes":sys::dir_bytes(&self.directory).0}),
        )
    }
    fn close(&mut self) -> Result<()> {
        if let Some(db) = self.db.take() {
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            db.close().map_err(|(_, e)| e)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_and_raw_frames_keep_exact_docs_atomic_index_and_count_orphans() {
        for compressed in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "oojuro-jsonl-{}-{}",
                std::process::id(),
                sys::unique_suffix()
            ));
            let mut arm = JsonlArm::new(compressed);
            arm.open(&dir, &BTreeMap::new()).unwrap();
            let original = Doc {
                id: "original".into(),
                key: "revision".into(),
                text: "\u{feff}héllo\0\r\n".repeat(100),
                valid_at: 5,
                recorded_at: 10,
                meta: BTreeMap::from([("kind".into(), "output".into())]),
            };
            arm.append_batch(std::slice::from_ref(&original)).unwrap();
            let mut collision = original.clone();
            collision.text = "different duplicate".into();
            assert!(arm.append_batch(&[collision]).is_err());
            assert_eq!(arm.get_range(0, 100, 10).unwrap(), vec![original.clone()]);
            assert_eq!(arm.stats().unwrap()["orphan_frames"], 1);
            assert_eq!(
                arm.stats().unwrap()["whole_store_bytes"],
                sys::dir_bytes(&dir).0
            );
            arm.close().unwrap();
            let mut wrong = JsonlArm::new(!compressed);
            assert!(wrong.open(&dir, &BTreeMap::new()).is_err());
            arm.open(&dir, &BTreeMap::new()).unwrap();
            assert_eq!(
                arm.get(&[original.id.clone(), "missing".into()]).unwrap(),
                vec![Some(original.text), None]
            );
            let referenced: String = arm
                .db()
                .unwrap()
                .query_row("SELECT frame FROM docs LIMIT 1", [], |r| r.get(0))
                .unwrap();
            std::fs::write(dir.join("payloads").join(referenced), b"corrupt").unwrap();
            assert!(arm.get(&[original.id]).is_err());
            arm.close().unwrap();
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
