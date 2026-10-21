//! Record-level dedupe gates: repeated texts are stored once (proxy-style
//! re-sends and cross-session repeats), every read is byte-exact, reopen
//! (checkpoint + replay, and a full rebuild) answers the same, a deleted
//! first occurrence keeps its text for live duplicates (search still finds
//! them), and the bytes leave every file when the last reference dies.
use oojuro_trace::{EventIn, Filter, Options, TraceDb};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("oojuro-dedupe-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Ev {
    id: u64,
    session: u64,
    agent: u32,
    turn: u32,
    seq: u32,
    kind: u8,
    status: u8,
    revises: u64,
    ts: i64,
    tool: String,
    args: String,
    text: String,
}

const SHARED: &str = "sharedfile77x";

/// Sessions 1..=n over two agents. Each turn re-sends every earlier event of
/// its session (proxy capture), then appends a tool result (status
/// revision of the previous call), thinking and a call. Sessions 3 and 5
/// (agent 1) both read the same file (text with `SHARED`), session 3 first.
fn batches(n: u64, turns: u32) -> Vec<Vec<Ev>> {
    let mut out = Vec::new();
    let mut id = 0u64;
    let mut prior: HashMap<u64, Vec<Ev>> = HashMap::new();
    let mut seq: HashMap<u64, u32> = HashMap::new();
    let mut calls: HashMap<u64, u64> = HashMap::new();
    for t in 0..turns {
        for s in 1..=n {
            let agent = (s % 2) as u32;
            let ts = 1_000_000 * (t as i64 * 100 + s as i64);
            let mut b = Vec::new();
            let mut push = |b: &mut Vec<Ev>, kind: u8, status: u8, revises: u64, tool: &str, args: &str, text: String| {
                id += 1;
                let q = seq.entry(s).or_default();
                b.push(Ev { id, session: s, agent, turn: t, seq: *q, kind, status, revises, ts, tool: tool.into(), args: args.into(), text });
                *q += 1;
                id
            };
            for p in prior.get(&s).cloned().unwrap_or_default() {
                push(&mut b, p.kind, 0, 0, &p.tool, &p.args, p.text.clone());
            }
            let mut fresh = Vec::new();
            if t == 0 {
                fresh.push((1u8, 0u8, 0u64, "", "", format!("task {s}: fix the parser, onlyinsession{s}x")));
            } else {
                let body = if (s == 3 || s == 5) && t == 1 {
                    format!("[file src/parse.rs]\n{SHARED} {}", "fn parse(input: &str) -> Tree { todo!() }\n".repeat(40))
                } else {
                    format!("output of step {t} for session {s}: {} onlyinsession{s}x", "lorem ipsum ".repeat(1 + s as usize % 4))
                };
                fresh.push((5, 2, 0, "", "", body));
                fresh.push((4, if s % 4 == 0 { 3 } else { 2 }, calls[&s], "", "", String::new()));
            }
            fresh.push((2, 0, 0, "", "", format!("thinking about session {s} turn {t}")));
            fresh.push((4, 1, 0, "bash", "{\"cmd\":\"ls\"}", if t % 3 == 0 { "ls".to_string() } else { format!("run --session {s} --turn {t}") }));
            for (kind, status, revises, tool, args, text) in fresh {
                let i = push(&mut b, kind, status, revises, tool, args, text);
                if kind == 4 && revises == 0 {
                    calls.insert(s, i);
                }
                if revises == 0 {
                    prior.entry(s).or_default().push(b.last().unwrap().clone());
                }
            }
            out.push(b);
        }
    }
    out
}

fn events(b: &[Ev]) -> Vec<EventIn<'_>> {
    b.iter()
        .map(|e| EventIn {
            id: e.id,
            session: e.session,
            agent: e.agent,
            turn: e.turn,
            seq: e.seq,
            parent: 0,
            revises: e.revises,
            kind: e.kind,
            status: e.status,
            exit_code: if e.revises != 0 { Some((e.status == 3) as i32) } else { None },
            tokens_in: 7,
            tokens_out: e.text.len() as u32 / 4,
            ts_us: e.ts,
            duration_us: if e.revises != 0 { 9 } else { 0 },
            tool: &e.tool,
            args: &e.args,
            text: &e.text,
            indexed: matches!(e.kind, 1 | 3 | 4 | 5),
        })
        .collect()
}

fn snapshot(db: &TraceDb, sessions: u64, max_id: u64) -> String {
    let mut out = String::new();
    for id in 1..=max_id {
        out += &format!("{:?}\n", db.get(id).unwrap());
        out += &format!("{:?}\n", db.text_range(id, 3, 40).unwrap());
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
    for a in 0..2 {
        for q in [SHARED, "parser onlyinsession3x", "lorem ipsum output"] {
            out += &format!("{:?}\n", db.search(a, q, i64::MAX / 2, 5).unwrap());
        }
    }
    out
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

fn check_exact(db: &TraceDb, data: &[Vec<Ev>], dead: &dyn Fn(&Ev) -> bool) {
    for e in data.iter().flatten().filter(|e| e.revises == 0) {
        let got = db.get(e.id).unwrap();
        if dead(e) {
            assert!(got.is_none(), "deleted event {} readable", e.id);
            continue;
        }
        let got = got.unwrap_or_else(|| panic!("event {} missing", e.id));
        assert_eq!(blake3::hash(got.text.as_bytes()), blake3::hash(e.text.as_bytes()), "text of {}", e.id);
        assert_eq!((got.tool.as_str(), got.args.as_str(), got.kind, got.session, got.seq), (e.tool.as_str(), e.args.as_str(), e.kind, e.session, e.seq));
        let r = db.text_range(e.id, 1, 9).unwrap().unwrap();
        let t = e.text.as_bytes();
        assert_eq!(r, t[1.min(t.len())..9.min(t.len())].to_vec());
    }
}

fn opts(erase: bool) -> Options {
    // The shared log (the default) supports erase-mode reclaim only; rewrite mode uses private journals.
    Options { flush_text_bytes: 64 << 10, merge_fanout: 4, erase_segments: erase, shared_log: erase, ..Options::default() }
}

fn run(erase: bool) {
    let tmp = Scratch::new(if erase { "erase" } else { "rewrite" });
    let dir = tmp.0.join("db");
    let sessions = 12;
    let data = batches(sessions, 14);
    let max_id = data.iter().flatten().map(|e| e.id).max().unwrap();
    let events_n = data.iter().flatten().filter(|e| e.revises == 0).count();
    let half = data.len() / 2;
    let before;
    {
        let db = TraceDb::open(&dir, opts(erase)).unwrap();
        for b in &data[..half] {
            db.append(&events(b)).unwrap();
        }
        db.flush().unwrap();
        for b in &data[half..] {
            db.append(&events(b)).unwrap();
        }
        check_exact(&db, &data, &|_| false);
        let stored = db.stored_records();
        assert!(stored * 4 < events_n, "dedupe stored {stored} records for {events_n} events");
        before = snapshot(&db, sessions, max_id);
        // the first occurrence of the shared file (session 3) is the search hit
        let hit = db.search(1, SHARED, i64::MAX / 2, 5).unwrap();
        let first = data.iter().flatten().find(|e| e.text.contains(SHARED)).unwrap();
        assert_eq!((hit.len(), hit[0]), (1, first.id), "repeats collapse to the first occurrence");
    }
    // reopen: checkpoint + replay
    {
        let db = TraceDb::open(&dir, opts(erase)).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), before);
    }
    // delete session 3 (owner of the shared text) and 7; session 5 keeps a copy
    let gone3 = |e: &Ev| e.session == 3 || e.session == 7;
    let after_delete;
    {
        let db = TraceDb::open(&dir, opts(erase)).unwrap();
        db.delete_session(3).unwrap();
        db.delete_session(7).unwrap();
        check_exact(&db, &data, &gone3);
        after_delete = snapshot(&db, sessions, max_id);
        let hit = db.search(1, SHARED, i64::MAX / 2, 5).unwrap();
        let five = data.iter().flatten().find(|e| e.session == 5 && e.text.contains(SHARED)).unwrap();
        assert_eq!(hit, vec![five.id], "a deleted first occurrence hands search to its earliest live copy");
        db.reclaim().unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
        check_exact(&db, &data, &gone3);
        assert_eq!(db.scan_decoded(&[b"onlyinsession3x".to_vec(), b"onlyinsession7x".to_vec()]).unwrap(), 0, "deleted text decodes from a row");
        assert!(db.scan_decoded(&[SHARED.as_bytes().to_vec()]).unwrap() > 0, "text still referenced by session 5 is kept");
    }
    assert_eq!(grep_files(&dir, b"onlyinsession3x"), 0);
    assert_eq!(grep_files(&dir, b"onlyinsession7x"), 0);
    // reopen after reclaim (holders survive the checkpoint)
    {
        let db = TraceDb::open(&dir, opts(erase)).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), after_delete);
        // the last reference dies: the shared bytes go
        db.delete_session(5).unwrap();
        db.reclaim().unwrap();
        assert_eq!(db.scan_decoded(&[SHARED.as_bytes().to_vec()]).unwrap(), 0, "unreferenced shared text survived");
        assert!(db.search(1, SHARED, i64::MAX / 2, 5).unwrap().is_empty());
    }
    assert_eq!(grep_files(&dir, SHARED.as_bytes()), 0, "unreferenced shared text left in a file");
    let gone = |e: &Ev| e.session == 3 || e.session == 5 || e.session == 7;
    let last;
    {
        let db = TraceDb::open(&dir, opts(erase)).unwrap();
        check_exact(&db, &data, &gone);
        last = snapshot(&db, sessions, max_id);
    }
    // missing checkpoint: full rebuild from records, holders and bundles
    std::fs::remove_file(dir.join("cols.ckpt")).unwrap();
    {
        let db = TraceDb::open(&dir, opts(erase)).unwrap();
        assert_eq!(snapshot(&db, sessions, max_id), last);
        // dedupe keeps working after a rebuild; new appends stay exact
        let more = batches(sessions, 15);
        let extra: Vec<Vec<Ev>> = more[data.len()..].iter().filter(|b| !gone(&b[0])).cloned().collect();
        for b in &extra {
            db.append(&events(b)).unwrap();
        }
        check_exact(&db, &extra, &|_| false);
    }
}

#[test]
fn dedupe_erase_mode() {
    run(true);
}

#[test]
fn dedupe_rewrite_mode() {
    run(false);
}
