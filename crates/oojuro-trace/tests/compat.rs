//! Stores written by an older build stay readable (docs/PERF_ROUND2.md, item 2).
//!
//! `tests/fixtures/main-store/` was written by `main` at `df8fa85` (records
//! with captured-style `src` / `rt` args that never deduplicated) together
//! with `expected.txt`, the answers `main` gave over it. The tests open it with
//! this build and check the same answers, then append, flush and merge on top
//! of it (repeats of text main stored now deduplicate) and compare with a store
//! this build wrote from scratch. Regenerate the fixture with a build of `main`:
//!
//! ```text
//! OOJURO_FIXTURE_OUT=/some/dir cargo test -p oojuro-trace --test compat -- --ignored write_fixture
//! ```
use oojuro_trace::{EventIn, Filter, Options, TraceDb};
use std::path::{Path, PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("oojuro-compat-{name}-{}", std::process::id()));
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
    seq: u32,
    kind: u8,
    status: u8,
    revises: u64,
    ts: i64,
    tool: String,
    args: String,
    text: String,
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const WORDS: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "parser", "tokens", "shadow", "page", "repair_shadow_page", "workers", "build", "cargo",
    "lockfile", "segment", "journal", "merge", "flush", "index", "postings", "röntgen", "東京", "café",
];

fn sentence(seed: u64, n: usize) -> String {
    (0..n).map(|i| WORDS[(mix(seed.wrapping_mul(31).wrapping_add(i as u64)) % WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
}

/// Captured-style args: the record's own source position and receive time.
fn args(id: u64, session: u64) -> String {
    format!("{{\"src\":\"{}:{}:{}\",\"rt\":{},\"from\":\"human\"}}", session % 4, id * 37, id % 2, 1_790_000_000_000_000i64 + id as i64 * 11)
}

/// Events `from..to` (ids from 1). Texts of the first `base` events are all
/// different; with `dups`, later events with odd ids repeat the text of an
/// earlier event of the same agent store (same session and kind: the ids
/// agree modulo 85).
fn events(from: u64, to: u64, base: u64, dups: bool) -> Vec<Ev> {
    let mut out = Vec::new();
    for id in from..to {
        let i = id - 1;
        let session = 1 + i % 17;
        let agent = (session % 3) as u32;
        let kind = [1u8, 3, 4, 5, 5][(i % 5) as usize];
        let source = if dups && id > base && id % 2 == 1 {
            let s0 = (id - 1) % 85;
            1 + s0 + 85 * (mix(id) % ((base - 1 - s0) / 85 + 1))
        } else {
            id
        };
        let text = format!("event {source} {} unique-token-{source}", sentence(source, 12 + (mix(source) % 30) as usize));
        out.push(Ev {
            id,
            session,
            agent,
            seq: ((id - 1) / 17) as u32,
            kind,
            status: if kind == 4 { 1 } else { 0 },
            revises: 0,
            ts: 1_790_000_000_000_000 + id as i64 * 1_000_000,
            tool: if kind >= 4 { "bash".into() } else { String::new() },
            args: args(id, session),
            text,
        });
    }
    out
}

fn as_in(e: &Ev) -> EventIn<'_> {
    EventIn {
        id: e.id,
        session: e.session,
        parent_session: 0,
        agent: e.agent,
        turn: 0,
        seq: e.seq,
        parent: 0,
        revises: e.revises,
        kind: e.kind,
        status: e.status,
        exit_code: None,
        tokens_in: 3,
        tokens_out: 4,
        ts_us: e.ts,
        duration_us: 0,
        tool: &e.tool,
        args: &e.args,
        text: &e.text,
        indexed: matches!(e.kind, 1 | 3 | 4 | 5),
    }
}

/// A status revision for every tool call: its own record, key `r<call id>`.
fn revisions(calls: &[Ev], first_id: u64) -> Vec<Ev> {
    calls
        .iter()
        .filter(|e| e.kind == 4)
        .enumerate()
        .map(|(k, c)| Ev {
            id: first_id + k as u64,
            revises: c.id,
            kind: 4,
            status: 2,
            tool: c.tool.clone(),
            args: String::new(),
            text: String::new(),
            ..c.clone()
        })
        .collect()
}

fn opts(fanout: usize) -> Options {
    Options { flush_text_bytes: 20 << 10, merge_fanout: fanout, shared_log: false, erase_segments: true, ..Options::default() }
}

const BASE: u64 = 700;

/// Append `evs` per agent in small batches, flushing every `flush_every` events.
fn load(db: &TraceDb, evs: &[Ev], revs: &[Ev], flush_every: usize) {
    let mut n = 0;
    for chunk in evs.chunks(5) {
        for a in 0..3u32 {
            let b: Vec<EventIn> = chunk.iter().filter(|e| e.agent == a).map(as_in).collect();
            if !b.is_empty() {
                db.append(&b).unwrap();
            }
        }
        n += chunk.len();
        if flush_every > 0 && n % flush_every < 5 {
            db.flush().unwrap();
        }
    }
    for chunk in revs.chunks(5) {
        for a in 0..3u32 {
            let b: Vec<EventIn> = chunk.iter().filter(|e| e.agent == a).map(as_in).collect();
            if !b.is_empty() {
                db.append(&b).unwrap();
            }
        }
    }
}

fn base_set() -> (Vec<Ev>, Vec<Ev>) {
    let evs = events(1, BASE + 1, BASE, false);
    let revs = revisions(&evs, BASE + 1);
    (evs, revs)
}

/// Layout-independent answers, one line per part: a BLAKE3 digest of every
/// event, the text ranges, each session, the counts and aggregates and each
/// search (the expected file stays small; a mismatch names the part).
fn snapshot(db: &TraceDb, max_id: u64) -> String {
    let mut out = String::new();
    let mut part = |name: String, body: String| {
        out += &format!("{name} {} {}\n", body.len(), blake3::hash(body.as_bytes()).to_hex());
    };
    for from in (1..=max_id).step_by(100) {
        let body: String = (from..(from + 100).min(max_id + 1)).map(|id| format!("{:?}\n", db.get(id).unwrap())).collect();
        part(format!("events {from}"), body);
    }
    let body: String = (1..=max_id).step_by(7).map(|id| format!("{:?}\n", db.text_range(id, 2, 33).unwrap())).collect();
    part("ranges".into(), body);
    for s in 1..=18 {
        part(format!("session {s}"), format!("{:?}\n{:?}\n", db.session(s).unwrap(), db.agg_session(s)));
    }
    let all = Filter { from_us: i64::MIN / 2, to_us: i64::MAX / 2, ..Default::default() };
    for st in [None, Some(1), Some(2)] {
        let f = Filter { status: st, ..all.clone() };
        part(format!("filter {st:?}"), format!("{} {:?}\n", db.count(&f).unwrap(), db.filter(&f, 40).unwrap()));
    }
    part("aggregates".into(), format!("{:?}\n{:?}\n", db.agg_tools(i64::MIN / 2, i64::MAX / 2), db.top_sessions(i64::MIN / 2, i64::MAX / 2, 5)));
    part("time range".into(), format!("{:?}\n", db.time_range(1_790_000_100_000_000, 1_790_000_300_000_000, 50).unwrap()));
    for a in 0..3 {
        for q in ["parser shadow page", "unique-token-5 unique-token-9", "repair_shadow_page lockfile journal", "röntgen 東京 café", "unique-token-650"] {
            part(format!("search {a} {q}"), format!("{:?}\n", db.search(a, q, i64::MAX / 2, 8).unwrap()));
        }
    }
    out
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/main-store")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let t = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &t);
        } else {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}

/// Run once with a build of `main` (see the module docs): writes the store
/// and the answers it gives.
#[test]
#[ignore]
fn write_fixture() {
    let out = PathBuf::from(std::env::var("OOJURO_FIXTURE_OUT").expect("OOJURO_FIXTURE_OUT"));
    let (evs, revs) = base_set();
    let store = out.join("main-store");
    let _ = std::fs::remove_dir_all(&store);
    {
        let db = TraceDb::open(&store.join("db"), opts(64)).unwrap();
        load(&db, &evs, &revs, 150);
        std::fs::write(out.join("expected.txt"), snapshot(&db, BASE + revs.len() as u64)).unwrap();
        // the last appends stay in the journal: no flush, no clean shutdown
        std::mem::forget(db);
    }
}

fn max_id() -> u64 {
    BASE + base_set().1.len() as u64
}

#[test]
fn a_store_written_by_main_answers_as_it_did() {
    let s = Scratch::new("open");
    copy_dir(&fixture_dir(), &s.0);
    let want = std::fs::read_to_string(fixture_dir().join("../expected.txt")).unwrap();
    let db = TraceDb::open(&s.0.join("db"), opts(64)).unwrap();
    assert_eq!(snapshot(&db, max_id()), want);
    // and again from a rebuilt column table
    drop(db);
    let _ = std::fs::remove_file(s.0.join("db/cols.ckpt"));
    let db = TraceDb::open(&s.0.join("db"), opts(64)).unwrap();
    assert_eq!(snapshot(&db, max_id()), want);
}

#[test]
fn a_store_this_build_writes_gives_the_same_answers() {
    let s = Scratch::new("fresh");
    let want = std::fs::read_to_string(fixture_dir().join("../expected.txt")).unwrap();
    let (evs, revs) = base_set();
    let db = TraceDb::open(&s.0.join("db"), opts(64)).unwrap();
    load(&db, &evs, &revs, 150);
    assert_eq!(snapshot(&db, max_id()), want);
    db.flush().unwrap();
    assert_eq!(snapshot(&db, max_id()), want, "after a flush");
    db.settle().unwrap();
    assert_eq!(snapshot(&db, max_id()), want, "after merges");
    drop(db);
    let db = TraceDb::open(&s.0.join("db"), opts(64)).unwrap();
    assert_eq!(snapshot(&db, max_id()), want, "after a reopen");
}

#[test]
fn new_segments_and_duplicates_join_a_store_main_wrote() {
    let s = Scratch::new("mixed");
    copy_dir(&fixture_dir(), &s.0);
    let extra = events(max_id() + 1, max_id() + 401, BASE, true);
    // the same events appended to a store written entirely by this build
    let fresh = Scratch::new("mixed-fresh");
    let (evs, revs) = base_set();
    let reference = TraceDb::open(&fresh.0.join("db"), opts(4)).unwrap();
    load(&reference, &evs, &revs, 150);
    load(&reference, &extra, &[], 100);
    reference.flush().unwrap();
    reference.settle().unwrap();
    let last = max_id() + 400;
    let want = snapshot(&reference, last);
    let (_, dups_ref) = reference.dedupe_stats();
    assert!(dups_ref > 100, "{dups_ref} duplicates");

    // no column checkpoint: the open replays every record, so the dedupe
    // window learns the texts in main's segments (relaxed keys over their
    // args, which carry their own src and rt)
    std::fs::remove_file(s.0.join("db/cols.ckpt")).unwrap();
    let db = TraceDb::open(&s.0.join("db"), opts(4)).unwrap();
    load(&db, &extra, &[], 100);
    let (_, dups) = db.dedupe_stats();
    assert_eq!(dups, dups_ref, "repeats of text main stored in its segments are found");
    assert_eq!(snapshot(&db, last), want, "appended");
    db.flush().unwrap();
    db.settle().unwrap();
    assert_eq!(snapshot(&db, last), want, "after the old and new segments merged");
    // every event reads back with the args it was appended with
    for e in evs.iter().chain(&extra) {
        let got = db.get(e.id).unwrap().unwrap();
        assert_eq!((got.args.as_str(), got.text.as_str()), (e.args.as_str(), e.text.as_str()), "event {}", e.id);
    }
    drop(db);
    let db = TraceDb::open(&s.0.join("db"), opts(4)).unwrap();
    assert_eq!(snapshot(&db, last), want, "after a reopen");
    // erase an event whose text a later repeat shares, then reclaim
    let victim = extra.iter().find(|e| e.id % 2 == 1).unwrap();
    db.delete_session(victim.session).unwrap();
    db.reclaim().unwrap();
    for e in evs.iter().chain(&extra) {
        let got = db.get(e.id).unwrap();
        if e.session == victim.session {
            assert!(got.is_none());
        } else {
            let got = got.unwrap_or_else(|| panic!("event {} missing", e.id));
            assert_eq!((got.args.as_str(), got.text.as_str()), (e.args.as_str(), e.text.as_str()), "event {}", e.id);
        }
    }
}
