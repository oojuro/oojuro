//! Agent instances and their parents: each session is one agent instance
//! and may name the instance that spawned it. The link is a 12th field of
//! the event metadata only when there is one (so other records keep the
//! 11-field format byte for byte), a per-session column in memory, and a
//! checkpoint section; it survives reopen with or without a checkpoint
//! (also one written before the section existed) and is dropped with the
//! session's last record. `scan_agent` gives the typed fields of one
//! store's live events for query-time analysis.
use oojuro_trace::{ActivityRow, EventIn, Options, TraceDb};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("oojuro-trace-agents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const T0: i64 = 1_790_000_000_000_000;

/// (id, session, parent session, kind, status, ts, text): session 1 is a
/// top-level session, 2 a subagent of 1, 3 a subagent of 2, 4 another top
/// level; all in one store (agent 0).
fn rows() -> Vec<(u64, u64, u64, u8, u8, i64, String)> {
    let mut v = Vec::new();
    let mut id = 0;
    for turn in 0..20i64 {
        for (s, parent) in [(1u64, 0u64), (2, 1), (3, 2), (4, 0)] {
            id += 1;
            v.push((id, s, parent, 4, 0, T0 + turn * 60_000_000 + s as i64, format!("call {s} {turn}")));
            id += 1;
            let st = if turn % 5 == 0 { 3 } else { 2 };
            v.push((
                id,
                s,
                parent,
                5,
                st,
                T0 + turn * 60_000_000 + s as i64 + 1,
                format!("result {s} {turn} x{}", "y".repeat(turn as usize)),
            ));
        }
    }
    v
}

fn append(db: &TraceDb, rows: &[(u64, u64, u64, u8, u8, i64, String)]) {
    for r in rows {
        let e = EventIn {
            id: r.0,
            session: r.1,
            parent_session: r.2,
            agent: 0,
            turn: 0,
            seq: r.0 as u32,
            parent: if r.3 == 5 { r.0 - 1 } else { 0 },
            revises: 0,
            kind: r.3,
            status: r.4,
            exit_code: None,
            tokens_in: 0,
            tokens_out: 0,
            ts_us: r.5,
            duration_us: 0,
            tool: if r.3 == 4 { "Bash" } else { "" },
            args: "",
            text: &r.6,
            indexed: true,
        };
        db.append(&[e]).unwrap();
    }
}

fn scan(db: &TraceDb) -> Vec<ActivityRow> {
    let mut v = Vec::new();
    db.scan_agent(0, i64::MIN, i64::MAX, |r| v.push(*r));
    v
}

fn parents(db: &TraceDb) -> Vec<(u64, Option<u64>)> {
    (1..=4).map(|s| (s, db.parent_of(s))).collect()
}

fn opts() -> Options {
    // small blocks of rows flush to segments and seal column blocks often
    Options { flush_text_bytes: 4 << 10, ..Options::default() }
}

#[test]
fn parents_are_stored_only_when_set_and_survive_reopen_rebuild_and_old_checkpoints() {
    let s = Scratch::new("reopen");
    let rows = rows();
    let want = vec![(1, None), (2, Some(1)), (3, Some(2)), (4, None)];
    let before;
    {
        let db = TraceDb::open(&s.0, opts()).unwrap();
        append(&db, &rows);
        assert_eq!(parents(&db), want);
        // the metadata line: 11 fields without a parent, 12 with one
        let nine = db.engine(0).unwrap().unwrap();
        let fields = |row: usize| nine.record_at(row as u32).unwrap().metadata["e"].split(',').count();
        assert_eq!(fields(0), 11, "session 1 has no parent");
        assert_eq!(fields(2), 12, "session 2 names its parent");
        assert!(nine.record_at(2).unwrap().metadata["e"].ends_with(",1"));
        before = scan(&db);
        assert_eq!(before.len(), rows.len());
        assert!(before.iter().zip(&rows).all(|(a, r)| a.id == r.0
            && a.session == r.1
            && a.ts_us == r.5
            && a.kind == r.3
            && a.status == r.4
            && a.text_len as usize == r.6.len()));
        assert!(before.iter().filter(|a| a.kind == 5).all(|a| a.call == a.id - 1));
        let tools = db.tool_names();
        assert!(before.iter().filter(|a| a.kind == 4).all(|a| tools[a.tool as usize] == "Bash"));
        // time bounds are half-open
        let mut n = 0;
        db.scan_agent(0, T0 + 60_000_000, T0 + 120_000_000, |_| n += 1);
        assert_eq!(n, 8);
        db.flush().unwrap(); // checkpoint
        // a crash after more appends: the checkpoint is behind, replay catches up
        append(&db, &[(1000, 9, 4, 4, 0, T0 + 9_000_000_000, "late call".into())]);
    }
    let check = |db: &TraceDb| {
        assert_eq!(parents(db), want);
        assert_eq!(db.parent_of(9), Some(4));
        assert_eq!(scan(db)[..before.len()], before[..]);
    };
    check(&TraceDb::open(&s.0, opts()).unwrap());
    // no checkpoint at all: rebuilt from the records
    std::fs::remove_file(s.0.join("cols.ckpt")).unwrap();
    check(&TraceDb::open(&s.0, opts()).unwrap());
    // a checkpoint written before the parents section existed: it loads;
    // records past it are replayed, so a fresh flush writes the section again
    {
        let db = TraceDb::open(&s.0, opts()).unwrap();
        db.flush().unwrap();
    }
    let path = s.0.join("cols.ckpt");
    let buf = std::fs::read(&path).unwrap();
    let body = &buf[..buf.len() - 4];
    let at = body.windows(4).rposition(|w| w == b"PRNT").expect("parents section");
    let mut old = body[..at].to_vec();
    let crc = crc32fast::hash(&old);
    old.extend_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, &old).unwrap();
    let db = TraceDb::open(&s.0, opts()).unwrap();
    assert_eq!(scan(&db)[..before.len()], before[..], "an old checkpoint still loads every column");
    assert_eq!(db.parent_of(2), None, "an old checkpoint carries no parents (no record before them had one)");
}

#[test]
fn erasing_a_session_drops_its_events_and_its_parent_link() {
    let s = Scratch::new("erase");
    let db = TraceDb::open(&s.0, opts()).unwrap();
    let rows = rows();
    append(&db, &rows);
    db.delete_session(3).unwrap();
    assert!(scan(&db).iter().all(|a| a.session != 3), "deleted events leave the scan at once");
    db.reclaim().unwrap();
    assert_eq!(db.parent_of(3), None, "the link goes with the session's last record");
    assert_eq!(db.parent_of(2), Some(1));
    assert_eq!(scan(&db).len(), rows.iter().filter(|r| r.1 != 3).count());
    drop(db);
    let db = TraceDb::open(&s.0, opts()).unwrap();
    assert_eq!(parents(&db), vec![(1, None), (2, Some(1)), (3, None), (4, None)]);
}
