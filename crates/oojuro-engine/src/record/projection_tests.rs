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

fn assert_same_projection(a: &Index, b: &Index, binding: &Binding) {
    for text in [
        "battery",
        "battery battery electrode",
        "lookup_needle battery",
        "\"battery electrode\"",
        "key:0003",
    ] {
        for k in [1, 5, 100] {
            for (from, to) in [(i64::MIN, i64::MAX), (0, 2), (4, 7)] {
                for kind in [None, Some(Kind::ToolResult)] {
                    let mut q = query(text, k);
                    q.from = from;
                    q.to = to;
                    q.filter.kind = kind;
                    let signature = |hits: Vec<Hit>| {
                        hits.into_iter()
                            .map(|h| {
                                (
                                    h.id,
                                    h.bits.to_bits(),
                                    h.continued,
                                    h.valid_at,
                                    h.reported_recorded_at,
                                    h.start,
                                    h.end,
                                    h.excerpt,
                                )
                            })
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(
                        signature(a.search(binding, &q).unwrap()),
                        signature(b.search(binding, &q).unwrap()),
                        "{text}: {k}, {from}..{to}, {kind:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn refreshed_projection_matches_fresh_build_for_growth_reorder_replacement_and_removal() {
    let make = |i: usize| {
        let mut r = row(
            &format!("{i:04}"),
            if i % 3 == 0 {
                "lookup_needle battery electrode"
            } else {
                "battery electrode"
            },
            (i % 7) as i64,
        );
        if i % 5 == 0 {
            r.kind = Kind::ToolResult;
        }
        r
    };
    let mut rows: Vec<_> = (1..1000).rev().filter(|i| i % 2 == 0).map(make).collect();
    let mut current = binding();
    let mut index = Index::build(current.clone(), rows.clone()).unwrap();
    assert_eq!(index.row_count(), rows.len());
    assert_eq!(
        index.text_bytes(),
        rows.iter().map(|r| r.text.len()).sum::<usize>()
    );
    assert!(
        index
            .rows
            .iter()
            .all(|r| r.text.is_empty() && r.text.capacity() == 0)
    );
    // New IDs interleave old IDs; private posting order must not break ties.
    rows.extend((0..1000).filter(|i| i % 2 == 1 || *i == 0).map(make));
    rows.reverse();
    for phase in 0..5 {
        let previous = current.clone();
        current.view[0] = phase + 1;
        match phase {
            1 => rows.reverse(),
            2 => {
                rows[3].text = "changed original battery".into();
                rows[4].indexed = false;
                rows[5].kind = Kind::ToolResult;
                rows[6].reported_recorded_at += 1;
                rows[8].tool = Some("changed-tool".into());
                rows[9].status = Some(Status::Error);
                rows[10].key = Some("changed-key".into());
            }
            3 => {
                rows.remove(4);
                rows[2].valid_at = 99;
                rows[2].agent_id = "another-agent".into();
                rows[7].id = "replacement-record".into();
            }
            4 => rows.clear(),
            _ => {}
        }
        index = index.refresh(current.clone(), rows.clone()).unwrap();
        assert_eq!(index.row_count(), rows.len());
        assert_eq!(
            index.text_bytes(),
            rows.iter().map(|r| r.text.len()).sum::<usize>()
        );
        assert!(
            index
                .rows
                .iter()
                .all(|r| r.text.is_empty() && r.text.capacity() == 0)
        );
        assert!(matches!(
            index.search(&previous, &query("battery", 1)),
            Err(Error::Scope)
        ));
        let fresh = Index::build(current.clone(), rows.clone()).unwrap();
        assert_same_projection(&index, &fresh, &current);
    }
}

#[test]
fn refreshed_projection_refuses_scope_duplicates_and_invalid_rows() {
    let rows = vec![row("a", "battery", 1)];
    for change in [0, 1] {
        let mut foreign = binding();
        if change == 0 {
            foreign.namespace[0] ^= 1;
        } else {
            foreign.writer = "another-writer".into();
        }
        assert!(matches!(
            Index::build(binding(), rows.clone())
                .unwrap()
                .refresh(foreign, rows.clone()),
            Err(Error::Scope)
        ));
    }
    assert!(matches!(
        Index::build(binding(), rows.clone())
            .unwrap()
            .refresh(binding(), vec![rows[0].clone(), rows[0].clone()]),
        Err(Error::Conflict)
    ));
    let mut invalid = rows.clone();
    invalid[0].id.clear();
    assert!(matches!(
        Index::build(binding(), rows)
            .unwrap()
            .refresh(binding(), invalid),
        Err(Error::Bounds)
    ));
    let original = vec![row("a", "battery", 1)];
    let changed = vec![row("a", "different original", 1)];
    assert!(matches!(
        Index::build(binding(), original)
            .unwrap()
            .refresh(binding(), changed),
        Err(Error::Conflict)
    ));
}
#[test]
fn every_source_field_change_requires_a_new_view() {
    let original = row("a", "battery", 1);
    for field in 0..10 {
        let mut changed = original.clone();
        match field {
            0 => changed.id = "b".into(),
            1 => changed.key = Some("b".into()),
            2 => changed.text = "another original".into(),
            3 => changed.valid_at += 1,
            4 => changed.reported_recorded_at += 1,
            5 => changed.kind = Kind::Raw,
            6 => changed.tool = Some("tool".into()),
            7 => changed.status = Some(Status::Error),
            8 => changed.agent_id = "agent".into(),
            9 => changed.indexed = false,
            _ => unreachable!(),
        }
        assert!(
            matches!(
                Index::build(binding(), vec![original.clone()])
                    .unwrap()
                    .refresh(binding(), vec![changed]),
                Err(Error::Conflict)
            ),
            "field {field}"
        );
    }
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

#[test]
fn streamed_rows_preserve_owned_serialization_and_exact_byte_bounds() {
    for optional in [false, true] {
        let mut owned = row("escaped-λ", "quoted \"line\"\0\r\n🦀", -7);
        owned.key = optional.then(|| "key\\quoted".into());
        owned.tool = optional.then(|| "tool\nλ".into());
        owned.status = optional.then_some(Status::Error);
        owned.agent_id = "agent\"🦀".into();
        owned.indexed = optional;
        let borrowed = owned.as_borrowed();
        let bytes = serde_json::to_vec(&owned).unwrap();
        assert_eq!(serde_json::to_vec(&borrowed).unwrap(), bytes);
        assert_eq!(json_size(&borrowed, bytes.len()).unwrap(), bytes.len());
        assert!(matches!(json_size(&borrowed, bytes.len() - 1), Err(Error::Bounds)));
        assert!(matches!(json_size(&owned, bytes.len() - 1), Err(Error::Bounds)));
    }
}

#[test]
fn streamed_refresh_preserves_fresh_scores_originals_and_every_metadata_field() {
    let original = vec![
        row("a", "battery electrode café\0", -2),
        row("b", "battery worker resolved_epoch=91", 2),
        row("c", "electrode tool", 4),
    ];
    for phase in 0..14 {
        let mut rows = original.clone();
        match phase {
            0 => rows.reverse(),
            1 => rows.insert(1, row("added", "battery worker", 3)),
            2 => { rows.remove(1); }
            3 => rows.clear(),
            4 => rows[1].id = "replacement".into(),
            5 => rows[1].key = Some("other-key".into()),
            6 => rows[1].text = "changed original electrode".into(),
            7 => rows[1].valid_at = -3,
            8 => rows[1].reported_recorded_at = 99,
            9 => rows[1].kind = Kind::ToolResult,
            10 => rows[1].tool = Some("changed-tool".into()),
            11 => rows[1].status = Some(Status::Error),
            12 => rows[1].agent_id = "changed-agent".into(),
            13 => rows[1].indexed = false,
            _ => unreachable!(),
        }
        let mut next = binding();
        next.view[0] += 1;
        let old = Index::build(binding(), original.clone()).unwrap();
        let mut stream = old.begin_refresh(next.clone(), rows.len()).unwrap();
        for fresh in &rows {
            stream.push(fresh.as_borrowed()).unwrap();
        }
        let got = stream.finish().unwrap();
        let want = Index::build(next.clone(), rows).unwrap();
        assert_same_projection(&got, &want, &next);
        assert_eq!(got.row_count(), want.row_count());
        assert_eq!(got.text_bytes(), want.text_bytes());
        assert!(matches!(got.search(&binding(), &query("battery", 10)), Err(Error::Scope)));
    }
}

#[test]
fn streamed_refresh_refuses_partial_excess_duplicate_invalid_and_changed_same_view() {
    let original = row("a", "battery", 1);
    let make = || Index::build(binding(), vec![original.clone()]).unwrap();
    let mut next = binding();
    next.view[0] += 1;
    assert!(matches!(make().begin_refresh(next.clone(), MAX_RECORDS + 1), Err(Error::Bounds)));
    let mut foreign = next.clone();
    foreign.namespace[0] ^= 1;
    assert!(matches!(make().begin_refresh(foreign, 1), Err(Error::Scope)));
    assert!(matches!(make().begin_refresh(next.clone(), 1).unwrap().finish(), Err(Error::Bounds)));
    let mut excess = make().begin_refresh(next.clone(), 0).unwrap();
    assert!(matches!(excess.push(original.as_borrowed()), Err(Error::Bounds)));
    assert!(matches!(excess.finish(), Err(Error::Bounds)));
    for key_duplicate in [false, true] {
        let mut duplicate = make().begin_refresh(next.clone(), 2).unwrap();
        duplicate.push(original.as_borrowed()).unwrap();
        let mut other = original.clone();
        if key_duplicate { other.id = "b".into(); }
        assert!(matches!(duplicate.push(other.as_borrowed()), Err(Error::Conflict)));
        assert!(matches!(duplicate.finish(), Err(Error::Conflict)));
    }
    let mut invalid = make().begin_refresh(next, 1).unwrap();
    let mut bad = original.clone();
    bad.id = String::new();
    assert!(matches!(invalid.push(bad.as_borrowed()), Err(Error::Bounds)));
    assert!(matches!(invalid.push(original.as_borrowed()), Err(Error::Bounds)));
    assert!(matches!(invalid.finish(), Err(Error::Bounds)));
    let mut changed = make().begin_refresh(binding(), 1).unwrap();
    bad.id = original.id.clone();
    bad.text = "changed bytes".into();
    changed.push(bad.as_borrowed()).unwrap();
    assert!(matches!(changed.finish(), Err(Error::Conflict)));
    // Complete validation precedes the same-view conflict, even when an
    // earlier fresh row changed. A late malformed row must still win.
    let mut late_invalid = make().begin_refresh(binding(), 2).unwrap();
    late_invalid.push(bad.as_borrowed()).unwrap();
    bad.id.clear();
    assert!(matches!(late_invalid.push(bad.as_borrowed()), Err(Error::Bounds)));
    assert!(matches!(late_invalid.finish(), Err(Error::Bounds)));
    let mut unchanged = make().begin_refresh(binding(), 1).unwrap();
    unchanged.push(original.as_borrowed()).unwrap();
    assert_same_projection(&unchanged.finish().unwrap(), &make(), &binding());
}
