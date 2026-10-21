//! Copies of one recorded text (a tool result seen again, a file read twice)
//! are stored once although each carries its own receive time; every copy
//! keeps its own metadata, erasure is reference counted, and the proof holds
//! (docs/PERF_ROUND2.md, item 2).
mod common;
use common::{Scratch, grep_files};
use oojuro::{
    Filters, Journal, SearchArgs, Target,
    append::{Appended, NewRecord},
};

const Q: &str = "build log compiled crates without warning";
const TEXT: &str = "the build log, line 1: unique-dup-marker-qq1 compiled 12 crates without a warning";

fn result(text: &str, ts_us: i64) -> NewRecord {
    NewRecord {
        writer: "agent:dup".into(),
        kind: "tool_result".into(),
        ts_us,
        text: text.into(),
        tool: "Bash".into(),
        via: "hook".into(),
        ..NewRecord::default()
    }
}

fn append(j: &mut Journal, r: NewRecord) -> Appended {
    j.owner().append(&[r]).unwrap()
}

fn find(j: &Journal, q: &str) -> Vec<u64> {
    let a = SearchArgs { query: q.into(), k: 10, filters: Filters::default(), excerpt_bytes: 32, max_query_bytes: 65_536 };
    j.reader().search(&a).unwrap().into_iter().map(|r| r.id).collect()
}

#[test]
fn copies_dedupe_keep_their_own_metadata_and_erase_by_reference_count() {
    let s = Scratch::new("dups");
    let store = s.path("journal");
    let t0 = 1_700_000_000_000_000i64;
    let mut j = Journal::open(&store).unwrap();
    let ids: Vec<u64> = (0..4).map(|i| append(&mut j, result(TEXT, t0 + i * 1_000)).ids[0]).collect();
    append(&mut j, result("something else entirely, unique-other-marker-zz9", t0 + 9_000));
    assert_eq!(ids, vec![1, 2, 3, 4]);
    let r = j.reader();
    let (texts, dups) = r.db().dedupe_stats();
    assert_eq!((texts, dups), (2, 3), "four copies of one text and one other: two texts, three repeats");
    assert_eq!(r.db().scan_decoded(&[TEXT.as_bytes().to_vec()]).unwrap(), 1, "the text is stored once");
    // each copy reads back with its own receive time and its exact text
    let mut seen = Vec::new();
    for id in &ids {
        let rec = r.get_id(*id).unwrap().unwrap();
        assert_eq!(rec.text, TEXT);
        seen.push(rec.recorded_us.unwrap());
        assert_eq!(rec.ts_us, t0 + (*id as i64 - 1) * 1_000);
    }
    let mut uniq = seen.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), 4, "own receive times: {seen:?}");
    assert!(seen.windows(2).all(|w| w[0] < w[1]));
    // search finds the text once, through its first copy
    assert_eq!(find(&j, Q), vec![1]);

    // forgetting the first copy hands the text to the next one
    let rep = j.forget(&Target::Id { id: 1 }, true).unwrap();
    let p = rep.proof.unwrap();
    assert!(p.proven && p.dead_rows_with_text == 0, "{p:?}");
    assert!(r.get_id(1).unwrap().is_none());
    assert_eq!(find(&j, Q), vec![2]);
    for id in [2u64, 3, 4] {
        let rec = r.get_id(id).unwrap().unwrap();
        assert_eq!(rec.text, TEXT);
        assert_eq!(rec.recorded_us.unwrap(), seen[id as usize - 1], "copy {id} keeps its own receive time");
    }
    // a middle copy: the others stay
    let rep = j.forget(&Target::Id { id: 3 }, true).unwrap();
    assert!(rep.proof.unwrap().proven);
    assert_eq!(r.get_id(4).unwrap().unwrap().recorded_us.unwrap(), seen[3]);
    // the live journal reopens (holder, entry lists) with the same answers
    drop((j, r));
    let mut j = Journal::open(&store).unwrap();
    let r = j.reader();
    assert_eq!(find(&j, Q), vec![2]);
    assert_eq!(r.get_id(2).unwrap().unwrap().recorded_us.unwrap(), seen[1]);
    assert_eq!(r.get_id(4).unwrap().unwrap().recorded_us.unwrap(), seen[3]);
    assert_eq!(r.db().scan_decoded(&[TEXT.as_bytes().to_vec()]).unwrap(), 1, "live copies keep the text, once");
    // the last references go: the bytes leave every file
    let rep = j.forget(&Target::Window { from_us: t0, to_us: t0 + 5_000, writer: None }, true).unwrap();
    assert_eq!(rep.forgotten, 2);
    let p = rep.proof.unwrap();
    assert!(p.proven && p.files_with_text == 0, "{p:?}");
    assert!(find(&j, Q).is_empty());
    assert_eq!(find(&j, "something else entirely").len(), 1);
    drop((j, r));
    assert_eq!(grep_files(&store, b"unique-dup-marker-qq1"), 0);
}
