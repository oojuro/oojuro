//! Tantivy 0.27 BM25 (en_stem), commit per batch, as-of range filters and
//! latest-revision post-filtering.
use super::{COMMIT, Revisions};
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
    path::Path,
};
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, Searcher, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, BoostQuery, Occur, Query, RangeQuery, TermQuery},
    schema::{
        FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
        Value,
    },
};

struct Fields {
    id: Field,
    key: Field,
    text: Field,
    valid_at: Field,
    recorded_at: Field,
    seq: Field,
    meta: Field,
}

struct State {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    f: Fields,
    revisions: Revisions,
}

#[derive(Default)]
pub struct TantivyArm {
    state: Option<State>,
}

fn schema() -> Schema {
    let mut s = Schema::builder();
    s.add_text_field("id", STRING | STORED);
    s.add_text_field("key", STRING | STORED);
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("en_stem")
                .set_index_option(IndexRecordOption::WithFreqs),
        )
        .set_stored();
    s.add_text_field("text", text);
    s.add_i64_field("valid_at", INDEXED | FAST | STORED);
    s.add_i64_field("recorded_at", INDEXED | FAST | STORED);
    s.add_u64_field("seq", FAST | STORED);
    s.add_text_field("meta", STORED);
    s.build()
}

fn fields(schema: &Schema) -> Result<Fields> {
    Ok(Fields {
        id: schema.get_field("id")?,
        key: schema.get_field("key")?,
        text: schema.get_field("text")?,
        valid_at: schema.get_field("valid_at")?,
        recorded_at: schema.get_field("recorded_at")?,
        seq: schema.get_field("seq")?,
        meta: schema.get_field("meta")?,
    })
}

fn text_of(d: &TantivyDocument, f: Field) -> String {
    d.get_first(f).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}
fn i64_of(d: &TantivyDocument, f: Field) -> i64 {
    d.get_first(f).and_then(|v| v.as_i64()).unwrap_or_default()
}

impl State {
    fn doc(&self, d: &TantivyDocument) -> Doc {
        let id = text_of(d, self.f.id);
        let key = text_of(d, self.f.key);
        Doc {
            key: if key == id { String::new() } else { key },
            id,
            text: text_of(d, self.f.text),
            valid_at: i64_of(d, self.f.valid_at),
            recorded_at: i64_of(d, self.f.recorded_at),
            meta: serde_json::from_str(&text_of(d, self.f.meta)).unwrap_or_default(),
        }
    }
    fn lookup(&self, searcher: &Searcher, id: &str) -> Result<Option<TantivyDocument>> {
        let q = TermQuery::new(Term::from_field_text(self.f.id, id), IndexRecordOption::Basic);
        let top = searcher.search(&q, &TopDocs::with_limit(1).order_by_score())?;
        Ok(match top.first() {
            Some((_, address)) => Some(searcher.doc(*address)?),
            None => None,
        })
    }
    fn query(&self, text: &str, valid_at: i64, known_at: i64) -> Result<Option<BooleanQuery>> {
        let mut analyzer = self.index.tokenizer_for_field(self.f.text)?;
        let mut terms = BTreeSet::new();
        let mut stream = analyzer.token_stream(text);
        while stream.advance() {
            terms.insert(stream.token().text.clone());
        }
        if terms.is_empty() {
            return Ok(None);
        }
        let should: Vec<(Occur, Box<dyn Query>)> = terms
            .into_iter()
            .map(|t| {
                let q: Box<dyn Query> =
                    Box::new(TermQuery::new(Term::from_field_text(self.f.text, &t), IndexRecordOption::WithFreqs));
                (Occur::Should, q)
            })
            .collect();
        let upto = |field: Field, at: i64| -> Box<dyn Query> {
            Box::new(BoostQuery::new(
                Box::new(RangeQuery::new(Bound::Unbounded, Bound::Included(Term::from_field_i64(field, at)))),
                0.0,
            ))
        };
        Ok(Some(BooleanQuery::new(vec![
            (Occur::Must, Box::new(BooleanQuery::new(should))),
            (Occur::Must, upto(self.f.valid_at, valid_at)),
            (Occur::Must, upto(self.f.recorded_at, known_at)),
        ])))
    }
}

impl TantivyArm {
    fn state(&mut self) -> Result<&mut State> {
        self.state.as_mut().ok_or_else(|| "store not open".into())
    }
}

impl Arm for TantivyArm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: "tantivy".into(),
            version: "tantivy 0.27.0 bm25 en_stem".into(),
            commit: COMMIT.into(),
            durability: "IndexWriter::commit per batch (fsync of segments + meta.json), then reader reload".into(),
            protocol: PROTOCOL.into(),
            notes: vec![
                "query = OR of distinct en_stem tokens; as-of range filters scored 0".into(),
                "latest revision per key enforced by an in-memory revision map rebuilt at open".into(),
                "one indexing thread, 64 MB writer arena, default merge policy".into(),
            ],
        }
    }
    fn open(&mut self, dir: &Path, _config: &BTreeMap<String, String>) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let index = if dir.join("meta.json").exists() {
            Index::open_in_dir(dir)?
        } else {
            Index::create_in_dir(dir, schema())?
        };
        let f = fields(&index.schema())?;
        let writer = index.writer_with_num_threads(1, 64_000_000)?;
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        // Rebuild the revision map in append order from committed documents.
        let searcher = reader.searcher();
        let mut rows = Vec::new();
        for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
            for doc in segment.doc_ids_alive() {
                let d: TantivyDocument = searcher.doc(DocAddress::new(ordinal as u32, doc))?;
                let seq = d.get_first(f.seq).and_then(|v| v.as_u64()).unwrap_or_default();
                rows.push((seq, text_of(&d, f.id), text_of(&d, f.key), i64_of(&d, f.valid_at), i64_of(&d, f.recorded_at)));
            }
        }
        rows.sort();
        let mut revisions = Revisions::default();
        for (_, id, key, v, r) in rows {
            revisions.insert(&id, &key, v, r);
        }
        self.state = Some(State { index, writer, reader, f, revisions });
        Ok(())
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        let s = self.state()?;
        for d in docs {
            if s.revisions.contains(&d.id) {
                return Err(format!("duplicate id {}", d.id).into());
            }
            let mut doc = TantivyDocument::default();
            doc.add_text(s.f.id, &d.id);
            doc.add_text(s.f.key, d.key());
            doc.add_text(s.f.text, &d.text);
            doc.add_i64(s.f.valid_at, d.valid_at);
            doc.add_i64(s.f.recorded_at, d.recorded_at);
            doc.add_u64(s.f.seq, s.revisions.len() as u64);
            doc.add_text(s.f.meta, serde_json::to_string(&d.meta)?);
            s.writer.add_document(doc)?;
            s.revisions.insert(&d.id, d.key(), d.valid_at, d.recorded_at);
        }
        s.writer.commit()?;
        s.reader.reload()?;
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        let s = self.state()?;
        let Some(q) = s.query(&query.text, query.valid_at, query.known_at)? else { return Ok(Vec::new()) };
        let searcher = s.reader.searcher();
        let total = searcher.num_docs() as usize;
        let mut limit = (query.k * 2 + 16).min(total.max(1));
        loop {
            let top = searcher.search(&q, &TopDocs::with_limit(limit).order_by_score())?;
            let exhausted = top.len() < limit;
            let mut hits = Vec::new();
            for (score, address) in &top {
                let d: TantivyDocument = searcher.doc(*address)?;
                let id = text_of(&d, s.f.id);
                if s.revisions.visible(&id, query.valid_at, query.known_at) {
                    hits.push(Hit { id, score: *score as f64 });
                }
            }
            if hits.len() >= query.k || exhausted || limit >= total {
                hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
                hits.truncate(query.k);
                return Ok(hits);
            }
            limit = (limit * 4).min(total);
        }
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        let s = self.state()?;
        let searcher = s.reader.searcher();
        ids.iter()
            .map(|id| Ok(s.lookup(&searcher, id)?.map(|d| text_of(&d, s.f.text))))
            .collect()
    }
    fn get_range(&mut self, from: i64, to: i64, limit: usize) -> Result<Vec<Doc>> {
        let s = self.state()?;
        let searcher = s.reader.searcher();
        let q = RangeQuery::new(
            Bound::Included(Term::from_field_i64(s.f.valid_at, from)),
            Bound::Included(Term::from_field_i64(s.f.valid_at, to)),
        );
        let top = searcher.search(&q, &TopDocs::with_limit(limit.max(1)).order_by_fast_field::<i64>("valid_at", Order::Asc))?;
        let mut docs = Vec::new();
        for (_, address) in top {
            let d: TantivyDocument = searcher.doc(address)?;
            let seq = d.get_first(s.f.seq).and_then(|v| v.as_u64()).unwrap_or_default();
            let doc = s.doc(&d);
            docs.push((doc.valid_at, seq, doc));
        }
        docs.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        Ok(docs.into_iter().take(limit).map(|(_, _, d)| d).collect())
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        let s = self.state()?;
        let searcher = s.reader.searcher();
        Ok(serde_json::json!({ "records": searcher.num_docs(), "segments": searcher.segment_readers().len() }))
    }
    fn close(&mut self) -> Result<()> {
        if let Some(s) = self.state.take() {
            s.writer.wait_merging_threads()?;
        }
        Ok(())
    }
}
