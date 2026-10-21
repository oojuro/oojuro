//! Event-level helpers used by the front door (`crates/oojuro`): delete by
//! id with tombstone replay (a torn last line is ignored), filtered and
//! scored search, typed-field reads without text, batch reads, and the
//! erasure scan (deleted text gone from every decoded row after reclaim).
use oojuro_trace::{EventIn, Options, TraceDb};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("oojuro-trace-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ev<'a>(id: u64, agent: u32, session: u64, seq: u32, kind: u8, ts: i64, text: &'a str) -> EventIn<'a> {
    EventIn {
        id,
        session,
        agent,
        turn: 0,
        seq,
        parent: 0,
        revises: 0,
        kind,
        status: 0,
        exit_code: None,
        tokens_in: 0,
        tokens_out: 0,
        ts_us: ts,
        duration_us: 0,
        tool: "",
        args: "",
        text,
        indexed: true,
    }
}

fn grep_files(dir: &std::path::Path, needle: &[u8]) -> usize {
    let mut hits = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let data = std::fs::read(&p).unwrap();
                hits += data.windows(needle.len()).filter(|w| *w == needle).count();
            }
        }
    }
    hits
}

#[test]
fn delete_by_id_search_where_and_erasure_scan() {
    let s = Scratch::new("delete");
    let texts: Vec<String> = (1..=40u64).map(|i| format!("note {i} about the zebra parser alpha{i}omega")).collect();
    {
        let db = TraceDb::open(&s.0, Options::default()).unwrap();
        for (i, t) in texts.iter().enumerate() {
            let id = i as u64 + 1;
            let agent = 1 + (id % 2) as u32;
            db.append(&[ev(id, agent, 100 + id % 4, i as u32, if id % 5 == 0 { 6 } else { 3 }, 1_000 + id as i64, t)]).unwrap();
        }
        // typed fields without text
        let info = db.info(5).unwrap();
        assert_eq!((info.agent, info.session, info.kind, info.ts_us), (2, 101, 6, 1_005));
        assert_eq!(db.session_info(101).iter().map(|x| x.id).collect::<Vec<_>>(), vec![1, 5, 9, 13, 17, 21, 25, 29, 33, 37]);
        // batch reads keep the caller's order; unknown ids are None
        let got = db.get_many(&[7, 999, 2]).unwrap();
        assert_eq!(got[0].as_ref().unwrap().text, texts[6]);
        assert!(got[1].is_none());
        assert_eq!(got[2].as_ref().unwrap().text, texts[1]);
        // search_where: scores, a kind filter, a time window
        let all = db.search_where(1, "zebra parser", 0, i64::MAX / 2, 100, &mut |_| true).unwrap();
        assert_eq!(all.len(), 20);
        assert!(all.iter().all(|h| h.bits.is_finite() && !h.continued));
        let only6 = db.search_where(1, "zebra parser", 0, i64::MAX / 2, 100, &mut |x| x.kind == 6).unwrap();
        assert_eq!(only6.iter().map(|x| x.id).collect::<std::collections::BTreeSet<_>>(), [10u64, 20, 30, 40].into());
        let window = db.search_where(1, "zebra", 1_010, 1_020, 100, &mut |_| true).unwrap();
        assert_eq!(window.iter().map(|x| x.id).collect::<std::collections::BTreeSet<_>>(), [10u64, 12, 14, 16, 18, 20].into());
        assert_eq!(db.search(2, "alpha13omega", i64::MAX / 2, 5).unwrap(), vec![13]);
        // delete by id: gone from every read at once
        assert_eq!(db.delete_events(&[13, 13, 14, 999]).unwrap(), 2);
        assert_eq!(db.delete_events(&[13]).unwrap(), 0, "already deleted");
        assert!(db.get(13).unwrap().is_none() && db.info(13).is_none());
        assert!(db.search(2, "alpha13omega", i64::MAX / 2, 5).unwrap().is_empty());
        assert!(!db.session_info(101).iter().any(|x| x.id == 13));
    }
    // tombstones replay on open; a torn last line is ignored
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(s.0.join("tombstones.log")).unwrap();
        f.write_all(b"E 2").unwrap(); // torn: "E 21\n" was never acknowledged
    }
    {
        let db = TraceDb::open(&s.0, Options::default()).unwrap();
        assert!(db.get(13).unwrap().is_none() && db.get(14).unwrap().is_none());
        assert!(db.get(2).unwrap().is_some(), "a torn tombstone line deleted an event");
        assert_eq!(db.len(), 38);
        // before reclaim the text is still stored, in a row nothing live needs
        let needle = vec![b"alpha13omega".to_vec()];
        let before = db.erasure_scan(None, &needle).unwrap();
        assert_eq!((before.dead_rows_with_needle, before.live_rows_with_needle), (1, 0));
        assert_eq!((before.dead_by_needle.clone(), before.live_by_needle.clone()), (vec![1], vec![0]));
        db.reclaim().unwrap();
        let after = db.erasure_scan(None, &needle).unwrap();
        assert_eq!((after.dead_rows_with_needle, after.live_rows_with_needle), (0, 0));
        assert_eq!(after.rows, before.rows, "erase keeps row numbers");
        let live = db.erasure_scan(Some(&[2]), &[b"alpha15omega".to_vec()]).unwrap();
        assert_eq!((live.dead_rows_with_needle, live.live_rows_with_needle), (0, 1));
        // per needle: one pass over the rows, counts in the order given
        let multi = db
            .erasure_scan(None, &[b"alpha15omega".to_vec(), Vec::new(), b"alpha13omega".to_vec(), b"zebra parser".to_vec()])
            .unwrap();
        assert_eq!(multi.live_by_needle, vec![1, 0, 0, 38]);
        assert_eq!(multi.dead_by_needle, vec![0, 0, 0, 0]);
    }
    assert_eq!(grep_files(&s.0, b"alpha13omega"), 0);
    assert_eq!(grep_files(&s.0, b"alpha14omega"), 0);
    let db = TraceDb::open(&s.0, Options::default()).unwrap();
    assert_eq!(db.len(), 38);
    assert_eq!(db.get(15).unwrap().unwrap().text, texts[14]);
}

/// F1 of the outcome eval (`docs/SEARCH_FIXES.md`): a filter the engine
/// cannot evaluate (`keep`) drops candidates, and the search asks the engine
/// for more until it has k or the engine has no more. Here the only events
/// the filter accepts rank below 10,045 better matches; the search used to
/// stop asking at 10,000 candidates and return nothing.
#[test]
fn filters_outside_the_engine_never_end_the_search_early() {
    let s = Scratch::new("deep");
    let db = TraceDb::open(&s.0, Options::default()).unwrap();
    let texts: Vec<String> = (1..=10_050u64)
        .map(|i| if i <= 5 { format!("zebra note {i} with padding") } else { format!("zebra parser note {i}") })
        .collect();
    for chunk in (1..=10_050u64).collect::<Vec<_>>().chunks(1_000) {
        let batch: Vec<EventIn> = chunk
            .iter()
            .map(|&id| ev(id, 1, 7, id as u32, if id <= 5 { 6 } else { 3 }, 1_000 + id as i64, &texts[id as usize - 1]))
            .collect();
        db.append(&batch).unwrap();
    }
    let top = db.search_where(1, "zebra parser", 0, i64::MAX / 2, 3, &mut |_| true).unwrap();
    assert!(top.iter().all(|h| h.id > 5), "the filtered-in events must rank last: {top:?}");
    let got = db.search_where(1, "zebra parser", 0, i64::MAX / 2, 3, &mut |x| x.kind == 6).unwrap();
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.iter().all(|h| h.id <= 5), "{got:?}");
    let all = db.search_where(1, "zebra parser", 0, i64::MAX / 2, 100, &mut |x| x.kind == 6).unwrap();
    assert_eq!(all.iter().map(|h| h.id).collect::<std::collections::BTreeSet<_>>(), (1..=5).collect());
}
