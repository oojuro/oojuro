//! Journals of different stores in one process are independent: each has its
//! own "store once" cache (docs/PERF_ROUND2.md, item 1).
mod common;

use common::Scratch;
use oojuro::{
    append::{Appended, NewRecord},
    journal::Journal,
};
use std::path::Path;

fn rec(writer: &str, text: String, key: String) -> NewRecord {
    NewRecord {
        writer: writer.into(),
        kind: "tool_result".into(),
        ts_us: 1_800_000_000_000_000,
        text,
        via: "hook".into(),
        dedupe: key,
        ..NewRecord::default()
    }
}

/// Store `s`'s work: 40 appends of 3 records; a third of the records repeat
/// the key of an earlier one, a pure function of `s`.
fn work(s: usize) -> Vec<Vec<NewRecord>> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ s as u64;
    let mut next = move |m: u64| {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (x >> 33) % m
    };
    (0..40u64)
        .map(|a| {
            (0..3u64)
                .map(|b| {
                    let n = a * 3 + b;
                    let source = if n > 0 && next(3) == 0 { next(n) } else { n };
                    rec(&format!("agent{}", source % 2), format!("result {s} {source} body"), format!("key-{source}"))
                })
                .collect()
        })
        .collect()
}

fn drive(dir: &Path, s: usize) -> Vec<Appended> {
    let mut j = Journal::open(dir).unwrap();
    let out = work(s).iter().map(|batch| j.owner().append(batch).unwrap()).collect();
    j.close().unwrap();
    out
}

#[test]
fn stores_appended_from_threads_match_the_serial_run() {
    const N: usize = 6;
    let (a, b) = (Scratch::new("stores-serial"), Scratch::new("stores-threads"));
    let serial: Vec<Vec<Appended>> = (0..N).map(|s| drive(&a.path(&format!("s{s}")), s)).collect();
    let mut threaded: Vec<Vec<Appended>> = Vec::new();
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..N)
            .map(|s| {
                let dir = b.path(&format!("s{s}"));
                sc.spawn(move || drive(&dir, s))
            })
            .collect();
        threaded = hs.into_iter().map(|h| h.join().unwrap()).collect();
    });
    assert_eq!(serial, threaded);
    let deduped: usize = serial.iter().flatten().map(|o| o.deduped).sum();
    let stored: usize = serial.iter().flatten().map(|o| o.ids.len()).sum();
    assert!(deduped > 0 && stored > 0, "the workload must exercise both paths ({stored} stored, {deduped} deduped)");
    assert_eq!(deduped + stored, N * 40 * 3);
}

#[test]
fn a_key_seen_in_one_store_is_new_in_another() {
    let d = Scratch::new("stores-keys");
    let mut a = Journal::open(&d.path("a")).unwrap();
    let mut b = Journal::open(&d.path("b")).unwrap();
    let r = || vec![rec("w", "the same result".into(), "call:1".into())];
    assert_eq!(a.owner().append(&r()).unwrap().ids.len(), 1);
    assert_eq!(a.owner().append(&r()).unwrap().deduped, 1);
    // interleaved with the other store's appends, each keeps its own answers
    assert_eq!(b.owner().append(&r()).unwrap().ids.len(), 1, "store b has not seen the key");
    assert_eq!(a.owner().append(&r()).unwrap().deduped, 1);
    assert_eq!(b.owner().append(&r()).unwrap().deduped, 1);
}

#[test]
fn the_cache_survives_a_reopen_and_is_per_open() {
    let d = Scratch::new("stores-reopen");
    let r = || vec![rec("w", "tool output".into(), "result:9".into())];
    {
        let mut j = Journal::open(&d.path("s")).unwrap();
        assert_eq!(j.owner().append(&r()).unwrap().ids.len(), 1);
        j.close().unwrap();
    }
    let mut j = Journal::open(&d.path("s")).unwrap();
    assert_eq!(j.owner().append(&r()).unwrap().deduped, 1, "a restarted owner still knows the key");
}

/// A copy of a store as it lies on disk (without the lock file).
fn copy_store(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (p, t) = (e.path(), to.join(e.file_name()));
        if e.file_name() == "LOCK" {
            continue;
        }
        if p.is_dir() {
            copy_store(&p, &t);
        } else {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}

/// Each save of the capture state is two flushes (the file and its directory);
/// an append call already pays one flush per writer store for its records. A
/// restarted owner finds records past the saved state by probing ids, so the
/// state is saved when `save_every` has passed, on close and for a new writer,
/// not on every call.
#[test]
fn an_append_call_saves_the_capture_state_only_when_it_is_due() {
    use std::time::Duration;
    let next_id_on_disk = |store: &Path| -> u64 {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(store.join("capture-state.json")).unwrap()).unwrap();
        v["next_id"].as_u64().unwrap()
    };
    let d = Scratch::new("savestate");
    // never due within the test: the state follows only the new writer and the close
    let mut o = oojuro::journal::store_options();
    o.save_every = Duration::from_secs(3600);
    let mut j = Journal::open_with(&d.path("lazy"), o).unwrap();
    for i in 0..5 {
        let out = j.owner().append(&[rec("w", format!("result {i}"), format!("k{i}"))]).unwrap();
        assert_eq!(out.ids, vec![i + 1]);
    }
    assert_eq!(next_id_on_disk(&d.path("lazy")), 1, "no save per call (the writer was registered at id 1)");
    // a crash now: the files as they are (no close, no drop) open with every record
    copy_store(&d.path("lazy"), &d.path("crashed"));
    let mut c = Journal::open(&d.path("crashed")).unwrap();
    for id in 1..=5u64 {
        assert_eq!(c.reader().get_id(id).unwrap().unwrap().text, format!("result {}", id - 1));
    }
    assert_eq!(c.owner().append(&[rec("w", "next".into(), "k7".into())]).unwrap().ids, vec![6], "ids continue");
    assert_eq!(c.owner().append(&[rec("w", "result 3".into(), "k3".into())]).unwrap().deduped, 1, "so do the store-once keys");
    drop(c);
    j.close().unwrap();
    assert_eq!(next_id_on_disk(&d.path("lazy")), 6, "close saves");
    let mut j = Journal::open(&d.path("lazy")).unwrap();
    assert_eq!(j.owner().append(&[rec("w", "after".into(), "k9".into())]).unwrap().ids, vec![6]);
    drop(j);

    // always due: every call saves, as before
    let mut o = oojuro::journal::store_options();
    o.save_every = Duration::ZERO;
    let mut j = Journal::open_with(&d.path("eager"), o).unwrap();
    for i in 0..3 {
        j.owner().append(&[rec("w", format!("result {i}"), format!("k{i}"))]).unwrap();
        assert_eq!(next_id_on_disk(&d.path("eager")), i + 2);
    }
}
