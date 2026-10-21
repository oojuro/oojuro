//! Reopen and recovery gates for oojuro-trace: every read is identical
//! after drop + reopen (checkpoint + replay), after deletes (tombstone
//! replay), after reclaim (rewritten stores; deleted text gone from every
//! file), and with a missing or damaged column checkpoint (full rebuild).
//! The same gates run with private journals and with the shared log.
use oojuro_trace::{EventIn, Filter, Options, TraceDb};
use std::path::{Path, PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("oojuro-trace-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Synthetic sessions: per turn [result of prev call, revision, thinking, call].
fn texts(n_sessions: u64, turns: u32) -> Vec<Vec<(u64, u64, u32, u32, u8, u8, u64, i64, String)>> {
    // (id, session, agent, turn, kind, status, revises, ts, text)
    let mut batches = Vec::new();
    let mut id = 0u64;
    let mut calls = std::collections::HashMap::new();
    for t in 0..turns {
        for s in 1..=n_sessions {
            let agent = (s % 3) as u32;
            let ts = 1_000_000 * (t as i64 * 100 + s as i64);
            let mut b = Vec::new();
            if t == 0 {
                id += 1;
                b.push((id, s, agent, t, 1u8, 0u8, 0u64, ts, format!("task {s}: fix the parser bug number {s} uniquemark{s}x")));
            } else {
                id += 1;
                b.push((id, s, agent, t, 5, if s % 4 == 0 { 3 } else { 2 }, 0, ts, format!("output of step {t} for session {s}: {} uniquemark{s}x", "lorem ipsum ".repeat((s as usize % 5) + 1))));
                id += 1;
                let call = calls[&s];
                b.push((id, s, agent, t, 4, if s % 4 == 0 { 3 } else { 2 }, call, ts, String::new()));
            }
            id += 1;
            b.push((id, s, agent, t, 2, 0, 0, ts + 1, format!("thinking about session {s} turn {t}")));
            id += 1;
            calls.insert(s, id);
            b.push((id, s, agent, t, 4, 1, 0, ts + 2, format!("run tests --session {s} --turn {t}")));
            batches.push(b);
        }
    }
    batches
}

fn events(b: &[(u64, u64, u32, u32, u8, u8, u64, i64, String)]) -> Vec<EventIn<'_>> {
    b.iter()
        .enumerate()
        .map(|(k, e)| EventIn {
            id: e.0,
            session: e.1,
            parent_session: 0,
            agent: e.2,
            turn: e.3,
            seq: k as u32,
            parent: 0,
            revises: e.6,
            kind: e.4,
            status: e.5,
            exit_code: if e.4 == 4 && e.6 != 0 { Some((e.5 == 3) as i32) } else { None },
            tokens_in: 10,
            tokens_out: e.8.len() as u32 / 4,
            ts_us: e.7,
            duration_us: if e.6 != 0 { 5 } else { 0 },
            tool: if e.4 == 4 { "run" } else { "" },
            args: if e.4 == 4 && e.6 == 0 { "{\"x\":1}" } else { "" },
            text: &e.8,
            indexed: matches!(e.4, 1 | 3 | 4 | 5),
        })
        .collect()
}

/// Everything readable, as a comparable string.
fn snapshot(db: &TraceDb, sessions: u64, max_id: u64) -> String {
    let mut out = String::new();
    for id in 1..=max_id {
        out += &format!("{:?}\n", db.get(id).unwrap());
    }
    for s in 1..=sessions + 1 {
        out += &format!("{:?}\n{:?}\n", db.session(s).unwrap(), db.agg_session(s));
    }
    let all = Filter { from_us: i64::MIN / 2, to_us: i64::MAX / 2, ..Default::default() };
    for st in [None, Some(1), Some(2), Some(3)] {
        let f = Filter { status: st, ..all.clone() };
        out += &format!("{} {:?}\n", db.count(&f).unwrap(), db.filter(&f, 50).unwrap());
    }
    out += &format!("{:?}\n{:?}\n", db.agg_tools(i64::MIN / 2, i64::MAX / 2), db.top_sessions(i64::MIN / 2, i64::MAX / 2, 5));
    out += &format!("{:?}\n", db.time_range(0, 2_000_000_000, 100).unwrap());
    for a in 0..3 {
        out += &format!("{:?}\n", db.search(a, "uniquemark7x parser bug", i64::MAX / 2, 5).unwrap());
    }
    out
}

fn opts() -> Options {
    // tiny blocks of work: force flushes and many sealed column blocks
    Options { flush_text_bytes: 64 << 10, merge_fanout: 4, ..Options::default() }
}

fn db_files(dir: &Path, keep: impl Fn(&str) -> bool) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if keep(&e.file_name().to_string_lossy()) {
                n += 1;
            }
        }
    }
    n
}

fn grep_files(dir: &Path, needle: &[u8]) -> usize {
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

fn shared_opts() -> Options {
    // small log files: rotations and retirements happen during the test
    Options { shared_log: true, log_segment_bytes: 32 << 10, ..opts() }
}

#[test]
fn reopen_checkpoint_deletes_reclaim_and_damaged_checkpoint() {
    scenario("reopen", opts);
}

#[test]
fn shared_log_reopen_checkpoint_deletes_reclaim_and_damaged_checkpoint() {
    scenario("shared", shared_opts);
}

fn scenario(name: &str, opts: fn() -> Options) {
    let shared = opts().shared_log;
    let tmp = Scratch::new(name);
    let dir = tmp.0.join("db");
    let sessions = 40;
    let data = texts(sessions, 120); // ~20k events: several sealed column blocks
    let max_id = data.iter().flatten().map(|e| e.0).max().unwrap();
    let half = data.len() / 2;
    let before;
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        for b in &data[..half] {
            db.append(&events(b)).unwrap();
        }
        db.flush().unwrap(); // checkpoint at the half
        for b in &data[half..] {
            db.append(&events(b)).unwrap();
        }
        before = snapshot(&db, sessions, max_id);
        let (sealed, _, _) = db.column_bytes();
        assert!(sealed > 0, "some column blocks are sealed");
        if let Some(st) = db.shared_log_stats() {
            assert!(st.batches >= data.len() as u64, "{st:?}");
            assert!(st.rounds <= st.batches);
        }
    }
    let wal_files = db_files(&dir, |n| n.starts_with("wal-"));
    if shared {
        assert_eq!(wal_files, 0, "agent stores have no private journals");
        assert!(db_files(&dir.join("shared-log"), |n| n.starts_with("log-")) > 0);
    } else {
        assert!(wal_files > 0);
    }
    // 1. reopen: checkpoint + replay past the watermarks
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), before);
    }
    // 2. deletes survive reopen via the tombstone log
    let after_delete;
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        db.delete_session(7).unwrap();
        db.purge_before(1_000_000 * 300).unwrap();
        after_delete = snapshot(&db, sessions, max_id);
        assert_ne!(after_delete, before);
        assert!(db.session(7).unwrap().is_empty());
    }
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
        assert!(grep_files(&dir, b"uniquemark7x") > 0, "before reclaim the text is still in the files");
        // 3. reclaim: same answers, deleted text gone from every file
        db.reclaim().unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
    }
    assert_eq!(grep_files(&dir, b"uniquemark7x"), 0, "deleted session text left in a store file");
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
    }
    // 4. missing checkpoint: full rebuild from the records
    std::fs::remove_file(dir.join("cols.ckpt")).unwrap();
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
        db.flush().unwrap();
    }
    // 5. damaged checkpoint: ignored, full rebuild
    let ck = dir.join("cols.ckpt");
    let mut b = std::fs::read(&ck).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0x55;
    std::fs::write(&ck, b).unwrap();
    {
        let db = TraceDb::open(&dir, opts()).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
    }
}

#[test]
fn shared_log_rejects_rewrite_reclaim_and_deferred_sync() {
    let tmp = Scratch::new("shared-opts");
    let rewrite = Options { erase_segments: false, ..shared_opts() };
    assert!(TraceDb::open(&tmp.0.join("a"), rewrite).is_err());
    let deferred = Options { deferred_sync: true, ..shared_opts() };
    assert!(TraceDb::open(&tmp.0.join("b"), deferred).is_err());
}

/// Erased rows that already sit in an agent's segments are still in the
/// shared log while other agents' unabsorbed rows keep its files; reclaim
/// must make every agent absorb and scrub the log.
#[test]
fn shared_log_reclaim_erases_text_kept_in_the_log_by_other_agents() {
    let tmp = Scratch::new("shared-erase");
    let dir = tmp.0.join("db");
    let opts = Options { flush_text_bytes: 16 << 20, ..shared_opts() };
    let data = texts(9, 6);
    let max_id = data.iter().flatten().map(|e| e.0).max().unwrap();
    let db = TraceDb::open(&dir, opts.clone()).unwrap();
    for b in &data {
        db.append(&events(b)).unwrap();
    }
    db.delete_session(7).unwrap();
    // nothing is flushed yet: the whole history is in the log
    assert!(grep_files(&dir.join("shared-log"), b"uniquemark7x") > 0);
    let after_delete = snapshot(&db, 9, max_id);
    db.reclaim().unwrap();
    assert_eq!(snapshot(&db, 9, max_id), after_delete);
    assert_eq!(grep_files(&dir, b"uniquemark7x"), 0, "deleted text left in a file");
    // appends continue after the scrub and survive reopen
    let more = texts(9, 7);
    for b in &more[data.len()..] {
        db.append(&events(b)).unwrap();
    }
    let max_id = more.iter().flatten().map(|e| e.0).max().unwrap();
    let live = snapshot(&db, 9, max_id);
    drop(db);
    let db = TraceDb::open(&dir, opts).unwrap();
    assert_eq!(snapshot(&db, 9, max_id), live);
}
