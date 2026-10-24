//! SQLite FTS5 (porter unicode61, bm25) with SQL as-of visibility.
use super::COMMIT;
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL};
use rusqlite::{Connection, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Default)]
pub struct SqliteArm {
    db: Option<Connection>,
}

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS docs(
  seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, key TEXT NOT NULL, text TEXT NOT NULL,
  valid_at INTEGER NOT NULL, recorded_at INTEGER NOT NULL, meta TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS docs_key ON docs(key, valid_at, recorded_at, seq);
CREATE INDEX IF NOT EXISTS docs_time ON docs(valid_at, seq);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, content='docs', content_rowid='seq', tokenize='porter unicode61');
";

pub(super) const SEARCH: &str = "
SELECT d.id, bm25(fts) AS s FROM fts JOIN docs d ON d.seq = fts.rowid
WHERE fts MATCH ?1 AND d.valid_at <= ?2 AND d.recorded_at <= ?3
  AND NOT EXISTS (SELECT 1 FROM docs n WHERE n.key = d.key AND n.seq != d.seq
    AND n.valid_at <= ?2 AND n.recorded_at <= ?3
    AND (n.valid_at, n.recorded_at, n.seq) > (d.valid_at, d.recorded_at, d.seq))
ORDER BY s, d.id LIMIT ?4";

/// Every distinct alphanumeric token, quoted, OR-ed: a bag-of-words BM25 query
/// with no FTS5 operator syntax reachable from the query text.
pub fn match_expression(text: &str) -> Option<String> {
    let terms: BTreeSet<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    (!terms.is_empty()).then(|| {
        terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}

impl SqliteArm {
    fn db(&mut self) -> Result<&mut Connection> {
        self.db.as_mut().ok_or_else(|| "store not open".into())
    }
}

impl Arm for SqliteArm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: "sqlite-fts5".into(),
            version: format!("sqlite {} fts5 porter unicode61 bm25", rusqlite::version()),
            commit: COMMIT.into(),
            durability: "one transaction per batch; WAL + synchronous=FULL (fsync at commit)".into(),
            protocol: PROTOCOL.into(),
            notes: vec!["query = OR of distinct tokens; latest-revision filter in SQL".into()],
        }
    }
    fn open(&mut self, dir: &Path, _config: &BTreeMap<String, String>) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let db = Connection::open(dir.join("store.sqlite"))?;
        db.execute_batch(SCHEMA)?;
        self.db = Some(db);
        Ok(())
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        let db = self.db()?;
        let tx = db.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO docs(id, key, text, valid_at, recorded_at, meta) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut index = tx.prepare_cached("INSERT INTO fts(rowid, text) VALUES (?1, ?2)")?;
            for d in docs {
                let seq = insert.insert(params![
                    d.id,
                    d.key(),
                    d.text,
                    d.valid_at,
                    d.recorded_at,
                    serde_json::to_string(&d.meta)?
                ])?;
                index.execute(params![seq, d.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        let Some(expression) = match_expression(&query.text) else { return Ok(Vec::new()) };
        let db = self.db()?;
        let mut statement = db.prepare_cached(SEARCH)?;
        let rows = statement.query_map(
            params![expression, query.valid_at, query.known_at, query.k as i64],
            |row| Ok(Hit { id: row.get(0)?, score: -row.get::<_, f64>(1)? }),
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        let db = self.db()?;
        let mut statement = db.prepare_cached("SELECT text FROM docs WHERE id = ?1")?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let mut rows = statement.query(params![id])?;
            out.push(match rows.next()? {
                Some(row) => Some(row.get(0)?),
                None => None,
            });
        }
        Ok(out)
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        let db = self.db()?;
        let mut statement = db.prepare_cached(
            "SELECT id, key, text, valid_at, recorded_at, meta FROM docs
             WHERE valid_at BETWEEN ?1 AND ?2 ORDER BY valid_at, seq LIMIT ?3",
        )?;
        let rows = statement.query_map(params![from, to, limit as i64], |row| {
            let id: String = row.get(0)?;
            let key: String = row.get(1)?;
            let meta: String = row.get(5)?;
            Ok(Doc {
                key: if key == id { String::new() } else { key },
                id,
                text: row.get(2)?,
                valid_at: row.get(3)?,
                recorded_at: row.get(4)?,
                meta: serde_json::from_str(&meta).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        let db = self.db()?;
        let records: i64 = db.query_row("SELECT count(*) FROM docs", [], |r| r.get(0))?;
        Ok(serde_json::json!({ "records": records }))
    }
    fn close(&mut self) -> Result<()> {
        if let Some(db) = self.db.take() {
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            db.close().map_err(|(_, e)| e)?;
        }
        Ok(())
    }
}
