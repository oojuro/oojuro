//! Diagnostic timing of the actual native arm and identical pure projection.
//! Run only on copied fixture stores; no hosted/authority acceptance claim.
use oojuro_bench::{Arm, Doc, Result, SearchReq, arms::native_record::NativeRecordArm};
use oojuro_engine::{
    capture_log::Identity,
    record::{self, AsOf, Filter, Kind, RecordLog, Scope, View, projection},
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Instant};

#[derive(Deserialize)]
struct Query {
    id: String,
    text: String,
}
fn native<T>(value: record::Result<T>) -> Result<T> {
    value.map_err(|e| format!("native: {e:?}").into())
}
fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e6
}
fn rows(docs: Vec<Doc>) -> Vec<projection::Row> {
    docs.into_iter()
        .map(|d| projection::Row {
            key: Some(if d.key.is_empty() {
                d.id.clone()
            } else {
                d.key
            }),
            id: d.id,
            text: d.text,
            valid_at: d.valid_at * 1_000_000,
            reported_recorded_at: d.recorded_at * 1_000_000,
            kind: Kind::Raw,
            tool: None,
            status: None,
            agent_id: "benchmark".into(),
            indexed: true,
        })
        .collect()
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("profile_native COPIED_STORE QUERIES_JSON".into());
    }
    let store = Path::new(&args[1]);
    let queries: Vec<Query> = serde_json::from_slice(&std::fs::read(&args[2])?)?;
    let binding = {
        let log = native(RecordLog::open(
            &store.join("records"),
            &[42; 32],
            Identity {
                writer: "oojuro-bench-import".into(),
                agent: 1,
                first_id: 1,
                adapter: "benchmark-import-v2".into(),
            },
        ))?;
        let view = native(View::open(
            &log,
            Scope {
                writer: "oojuro-bench-import",
                owner: 1,
            },
            AsOf::default(),
        ))?;
        projection::Binding {
            namespace: view.watermark().origin,
            writer: "oojuro-bench-import".into(),
            view: *blake3::hash(&serde_json::to_vec(view.watermark())?).as_bytes(),
        }
    };
    let mut arm = NativeRecordArm::default();
    arm.open(store, &BTreeMap::new())?;
    for run in 0..3 {
        for query in &queries {
            let request = SearchReq {
                text: query.text.clone(),
                valid_at: i64::MAX,
                known_at: i64::MAX,
                k: 101,
                context: BTreeMap::new(),
            };
            let start = Instant::now();
            let actual = arm.search(&request)?;
            let native_search_us = elapsed(start);
            let ids: Vec<_> = actual
                .iter()
                .filter(|h| h.id != query.id)
                .take(100)
                .map(|h| h.id.clone())
                .collect();
            let start = Instant::now();
            let originals = arm.get(&ids)?;
            let native_get_us = elapsed(start);
            let start = Instant::now();
            let mut docs = arm.get_range(i64::MIN, i64::MAX, 10000)?;
            let materialize_us = elapsed(start);
            // ArguAna has no revisions and every source clock is zero. Refuse
            // another corpus rather than silently changing winner semantics.
            assert!(
                docs.iter()
                    .all(|d| d.valid_at == 0 && d.recorded_at == 0 && d.key.is_empty())
            );
            let original_by_id: BTreeMap<_, _> = docs.iter().map(|d| (&d.id, &d.text)).collect();
            for (id, text) in ids.iter().zip(&originals) {
                assert_eq!(text.as_ref(), original_by_id.get(id).copied());
            }
            let count = docs.len();
            let bytes: usize = docs.iter().map(|d| d.text.len()).sum();
            let start = Instant::now();
            docs.sort_by(|a, b| a.id.cmp(&b.id));
            let rows = rows(docs);
            let rows_us = elapsed(start);
            let start = Instant::now();
            let index = native(projection::Index::build(binding.clone(), rows))?;
            let build_us = elapsed(start);
            let start = Instant::now();
            let hits = native(index.search(
                &binding,
                &projection::Query {
                    text: &query.text,
                    k: 101,
                    from: i64::MIN,
                    to: i64::MAX,
                    filter: Filter::default(),
                    excerpt_bytes: 320,
                    max_bytes: record::MAX_OUTPUT_BYTES,
                },
            ))?;
            let query_us = elapsed(start);
            let signature = blake3::hash(&serde_json::to_vec(
                &hits
                    .iter()
                    .map(|h| {
                        (
                            &h.id,
                            h.bits.to_bits(),
                            h.continued,
                            h.valid_at,
                            h.reported_recorded_at,
                            h.start,
                            h.end,
                            &h.excerpt,
                        )
                    })
                    .collect::<Vec<_>>(),
            )?)
            .to_hex()
            .to_string();
            assert_eq!(
                actual
                    .iter()
                    .map(|h| (&h.id, h.score.to_bits()))
                    .collect::<Vec<_>>(),
                hits.iter()
                    .map(|h| (&h.id, h.bits.to_bits()))
                    .collect::<Vec<_>>()
            );
            println!(
                "{}",
                json!({"run":run,"query_blake3":blake3::hash(query.text.as_bytes()).to_hex().to_string(),
                "records":count,"text_bytes":bytes,"native_search_us":native_search_us,"native_get_us":native_get_us,
                "public_materialize_us":materialize_us,"row_prepare_us":rows_us,"projection_build_us":build_us,
                "projection_query_us":query_us,"exact_ranked_ids_and_score_bits":true,"originals_exact":true,
                "ranked_ids_bits_metadata_excerpt_blake3":signature,
                "scope":"local diagnostic on copied AWS fixture; not hosted or comparative acceptance"})
            );
        }
    }
    arm.close()
}
