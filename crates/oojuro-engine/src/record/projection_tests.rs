use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::current_dir()
            .unwrap()
            .join(".scratch")
            .join(format!(
                "projection-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn binding() -> Binding {
    Binding {
        namespace: [7; 32],
        writer: "opaque-writer".into(),
        view: [8; 32],
    }
}
fn row(id: &str, text: &str, at: i64) -> Row {
    Row {
        id: id.into(),
        key: Some(id.into()),
        text: text.into(),
        valid_at: at,
        reported_recorded_at: -99,
        kind: Kind::Output,
        tool: None,
        status: None,
        agent_id: String::new(),
        indexed: true,
    }
}
fn query(text: &str, k: usize) -> Query<'_> {
    Query {
        text,
        k,
        from: i64::MIN,
        to: i64::MAX,
        filter: Filter::default(),
        excerpt_bytes: 320,
        max_bytes: 1 << 20,
    }
}
fn compare(rows: Vec<Row>, texts: &[&str]) {
    let f = Fixture::new();
    let oracle = oojuro_nine::Nine::open(&f.0.join("oracle")).unwrap();
    oracle
        .insert_batch_indexed(
            rows.iter()
                .map(|r| {
                    let mut v = oojuro_core::Record::new(
                        &r.id,
                        r.key.as_deref().unwrap(),
                        &r.text,
                        r.valid_at,
                    );
                    v.recorded_at = r.reported_recorded_at;
                    (v, r.indexed)
                })
                .collect(),
        )
        .unwrap();
    let index = Index::build(binding(), rows).unwrap();
    for &text in texts {
        for k in [1, 5, 100] {
            for (from, to) in [(i64::MIN, i64::MAX), (0, 2), (4, 7)] {
                let mut q = query(text, k);
                q.from = from;
                q.to = to;
                let got = index.search(&binding(), &q).unwrap();
                let mut oq = oojuro_core::Query::new(text, to);
                oq.known_at = i64::MAX;
                oq.k = k;
                let want = oracle.search_refs(&oq, from).unwrap();
                assert_eq!(
                    got.iter()
                        .map(|h| (&h.id, h.bits.to_bits(), h.continued, h.valid_at))
                        .collect::<Vec<_>>(),
                    want.matches
                        .iter()
                        .map(|h| (&h.id, h.score_bits.to_bits(), h.continued, h.time))
                        .collect::<Vec<_>>(),
                    "q={text},k={k},{from}..{to}"
                );
                for h in got {
                    assert_eq!(h.reported_recorded_at, -99);
                    assert!(h.excerpt.len() <= 320);
                }
            }
        }
    }
}
#[test]
fn projected_query_preserves_opaque_ids_bits_time_continuation_and_pre_topk_ties() {
    let mut rows = vec![];
    for i in (0..1200).rev() {
        let text = if i < 4 {
            "lookup_needle battery electrode".into()
        } else if i % 3 == 0 {
            "battery electrode ".repeat(20)
        } else {
            format!("battery material{i} substrate ").repeat(15)
        };
        rows.push(row(&format!("{i:064x}"), &text, (i % 7) as i64));
    }
    compare(
        rows,
        &[
            "battery",
            "battery battery electrode",
            "lookup_needle battery",
            "absent_name battery",
            "\"battery electrode\"",
            "key:0000000000000000000000000000000000000000000000000000000000000001",
        ],
    );
    let rows = (0..100)
        .rev()
        .map(|i| row(&format!("{i:064x}"), "same battery", 1))
        .collect();
    let index = Index::build(binding(), rows).unwrap();
    let hits = index.search(&binding(), &query("battery", 1)).unwrap();
    assert_eq!(hits[0].id, format!("{:064x}", 0));
}
#[test]
fn projected_query_filter_precedes_floor_and_stored_only_cannot_reappear() {
    let mut a = row("a", "the !!! lookup_needle battery", 1);
    a.indexed = false;
    let mut b = row("b", "lookup needle battery", 2);
    b.kind = Kind::ToolResult;
    let c = row("c", &"battery other ".repeat(100), 3);
    let index = Index::build(binding(), vec![a, b.clone(), c]).unwrap();
    assert!(
        index
            .search(&binding(), &query("\"!!!\"", 10))
            .unwrap()
            .is_empty()
    );
    assert!(
        index
            .search(&binding(), &query("key:a", 10))
            .unwrap()
            .is_empty()
    );
    let hits = index
        .search(&binding(), &query("lookup_needle battery", 10))
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| !h.continued));
    let single = Index::build(binding(), vec![b]).unwrap();
    let mut filtered = query("battery", 10);
    filtered.filter.kind = Some(Kind::ToolResult);
    assert_eq!(
        index
            .search(&binding(), &filtered)
            .unwrap()
            .iter()
            .map(|h| (&h.id, h.bits.to_bits()))
            .collect::<Vec<_>>(),
        single
            .search(&binding(), &query("battery", 10))
            .unwrap()
            .iter()
            .map(|h| (&h.id, h.bits.to_bits()))
            .collect::<Vec<_>>()
    );
}
#[test]
fn projected_query_binding_duplicate_and_budget_fail_closed() {
    let a = row("original", "battery", 7);
    assert!(matches!(
        Index::build(binding(), vec![a.clone(), a.clone()]),
        Err(Error::Conflict)
    ));
    let mut b = a.clone();
    b.id = "other".into();
    assert!(matches!(
        Index::build(binding(), vec![a.clone(), b]),
        Err(Error::Conflict)
    ));
    let index = Index::build(binding(), vec![a]).unwrap();
    let mut foreign = binding();
    foreign.view[0] ^= 1;
    assert!(matches!(
        index.search(&foreign, &query("battery", 5)),
        Err(Error::Scope)
    ));
    foreign = binding();
    foreign.namespace[0] ^= 1;
    assert!(index.search(&foreign, &query("battery", 5)).is_err());
    let mut q = query("battery", 5);
    q.max_bytes = 2;
    assert!(matches!(index.search(&binding(), &q), Err(Error::Bounds)));
    assert!(index.search(&binding(), &query("", 5)).is_err());
}
#[test]
fn projected_query_exact_utf8_passages_match_dev_oracle() {
    let text = format!(
        "{}\nThe garnet job uses resolved_epoch=91; workers were running efficiently. café.\n{}",
        "λ🦀 noise\n".repeat(300),
        "unrelated tail ".repeat(100)
    );
    for q in [
        "garnet resolved_epoch",
        "worker runs",
        "\"resolved_epoch=91\"",
        "absent",
        "repair_shadow_page",
        "the",
    ] {
        for bytes in [1, 3, 30, 128, 320] {
            let got = passage::extract_passage(&text, q, bytes).unwrap();
            let want = oojuro_nine::extract_passage(&text, q, bytes).unwrap();
            assert_eq!(
                got.as_ref().map(|p| (p.start, p.end, p.text)),
                want.as_ref().map(|p| (p.start, p.end, p.text))
            );
            if let Some(p) = got {
                assert_eq!(p.text, &text[p.start..p.end]);
                assert!(p.text.len() <= bytes);
            }
        }
    }
}
