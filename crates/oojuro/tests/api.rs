//! Library semantics on a synthetic journal (direct, in-process owner):
//! explicit writes, versioning by key (newest wins), as-of reads, filters,
//! exact excerpts and byte ranges, long queries, redaction, range/export,
//! and forget with an erasure proof checked again byte for byte.
mod common;
use common::{Scratch, grep_files};
use oojuro::{Filters, Journal, SearchArgs, Target};

const BIG: usize = 16 << 20;

fn args(q: &str, k: usize) -> SearchArgs {
    SearchArgs { query: q.into(), k, filters: Filters::default(), excerpt_bytes: 64, max_query_bytes: 65_536 }
}

#[test]
fn versioning_as_of_filters_and_excerpts() {
    let s = Scratch::new("api");
    let store = s.path("journal");
    let mut j = Journal::open(&store).unwrap();
    let t0 = 1_700_000_000_000_000i64; // valid times in µs
    let v1 = j
        .remember("human:ann", "the deploy gate is the orange button in the ops panel", Some("deploy-gate"), Some(t0))
        .unwrap();
    let v2 = j
        .remember(
            "human:ann",
            "the deploy gate moved: use the release train dashboard",
            Some("deploy-gate"),
            Some(t0 + 10),
        )
        .unwrap();
    let other = j
        .remember("agent:bob", "bob notes: the deploy gate flakes on Fridays", Some("deploy-gate"), Some(t0 + 5))
        .unwrap();
    let plain = j
        .remember("human:ann", "lunch is at noon; the zebra parser review is after lunch", None, Some(t0 + 20))
        .unwrap();
    assert_eq!((v1.version, v2.version, other.version, plain.version), (1, 2, 1, 0));
    assert_eq!(v1.writer, "human:ann");
    assert!(v1.id < v2.id && v2.id < other.id);
    let r = j.reader();

    // newest version wins in search and get; history keeps all
    let hits = r.search(&args("deploy gate", 10)).unwrap();
    let ids: Vec<u64> = hits.iter().map(|h| h.id).collect();
    assert!(ids.contains(&v2.id) && ids.contains(&other.id), "{ids:?}");
    assert!(!ids.contains(&v1.id), "a superseded version was returned");
    assert_eq!(r.get_key("human:ann", "deploy-gate", None).unwrap().unwrap().id, v2.id);
    let h = r.history("human:ann", "deploy-gate").unwrap();
    assert_eq!(h.iter().map(|x| (x.id, x.seq)).collect::<Vec<_>>(), vec![(v1.id, 1), (v2.id, 2)]);
    assert_eq!(h[0].key.as_deref(), Some("deploy-gate"));
    assert!(h[0].recorded_us.is_some());
    // keys are per writer
    assert_eq!(r.get_key("agent:bob", "deploy-gate", None).unwrap().unwrap().id, other.id);
    assert!(r.get_key("agent:nobody", "deploy-gate", None).unwrap().is_none());

    // as-of: the view at t0+7 has only v1 of ann's key, and bob's note
    let mut a = args("deploy gate", 10);
    a.filters.as_of_us = Some(t0 + 7);
    let at: Vec<u64> = r.search(&a).unwrap().iter().map(|h| h.id).collect();
    assert_eq!(at.iter().copied().collect::<std::collections::BTreeSet<_>>(), [v1.id, other.id].into());
    assert_eq!(r.get_key("human:ann", "deploy-gate", Some(t0 + 7)).unwrap().unwrap().id, v1.id);
    assert!(r.get_key("human:ann", "deploy-gate", Some(t0 - 1)).unwrap().is_none());
    a.filters.as_of_us = Some(t0 - 1);
    assert!(r.search(&a).unwrap().is_empty(), "nothing existed yet");

    // filters: writer, key, kind, time window
    let mut a = args("deploy gate", 10);
    a.filters.writers = vec!["agent:bob".into()];
    assert_eq!(r.search(&a).unwrap().iter().map(|h| h.id).collect::<Vec<_>>(), vec![other.id]);
    let mut a = args("deploy gate", 10);
    a.filters.key = Some("deploy-gate".into());
    a.filters.writers = vec!["human:ann".into()];
    assert_eq!(r.search(&a).unwrap().iter().map(|h| h.id).collect::<Vec<_>>(), vec![v2.id]);
    let mut a = args("deploy gate", 10);
    a.filters.kinds = vec!["tool_call".into()];
    assert!(r.search(&a).unwrap().is_empty());
    a.filters.kinds = vec!["nonsense".into()];
    assert!(r.search(&a).is_err());
    let mut a = args("zebra deploy", 10);
    a.filters.from_us = Some(t0 + 15);
    assert_eq!(r.search(&a).unwrap().iter().map(|h| h.id).collect::<Vec<_>>(), vec![plain.id]);

    // exact excerpts: text[start..end] == excerpt, the match inside
    let a = SearchArgs {
        query: "zebra parser review".into(),
        k: 1,
        filters: Filters::default(),
        excerpt_bytes: 24,
        max_query_bytes: 65_536,
    };
    let hit = &r.search(&a).unwrap()[0];
    let full = r.get_id(hit.id).unwrap().unwrap().text;
    assert_eq!(hit.len, full.len());
    assert_eq!(&full[hit.start..hit.end], hit.excerpt);
    assert!(hit.excerpt.len() <= 24 && hit.excerpt.contains("zebra"), "{hit:?}");
    assert!(hit.score_bits.is_finite());

    // long queries: whole pasted context, up to max_query_bytes
    let long = format!("{} zebra parser review {}", "filler words about nothing ".repeat(4000), "x".repeat(10));
    assert!(long.len() > 65_536);
    assert!(r.search(&args(&long, 3)).is_err(), "default max_query_bytes is 64 KiB");
    let mut a = args(&long, 3);
    a.max_query_bytes = 1 << 20;
    assert_eq!(r.search(&a).unwrap().first().map(|h| h.id), Some(plain.id));
    a.max_query_bytes = BIG * 2;
    a.query = "y".repeat(BIG + 1);
    assert!(r.search(&a).is_err(), "the hard cap holds whatever the setting says");

    // range (inclusive, every version) and count
    let f = Filters { from_us: Some(t0), to_us: Some(t0 + 10), ..Filters::default() };
    assert_eq!(r.range(&f, 100).unwrap().iter().map(|x| x.id).collect::<Vec<_>>(), vec![v1.id, other.id, v2.id]);
    assert_eq!(r.range(&f, 2).unwrap().len(), 2);
    assert_eq!(r.count(&Filters { writers: vec!["human:ann".into()], ..Filters::default() }).unwrap(), 3);
    let fk = Filters { key: Some("deploy-gate".into()), writers: vec!["human:ann".into()], ..Filters::default() };
    assert_eq!(r.range(&fk, 10).unwrap().len(), 2);
    let mut exported = Vec::new();
    let n = r
        .export(&Filters::default(), &mut |rec| {
            exported.push(rec.id);
            Ok(())
        })
        .unwrap();
    assert_eq!(n, 4);
    assert_eq!(exported, vec![v1.id, other.id, v2.id, plain.id]);

    // stats
    let st = r.stats().unwrap();
    assert_eq!(st.records, 4);
    assert_eq!(st.kinds["explicit"], 4);
    assert_eq!(
        st.writers.iter().map(|w| (w.writer.as_str(), w.records)).collect::<Vec<_>>(),
        vec![("agent:bob", 1), ("human:ann", 3)]
    );

    // invalid writes
    assert!(j.remember("human:ann", "   ", None, None).is_err());
    assert!(j.remember("", "x", None, None).is_err());
    assert!(j.remember("human:ann", "x", Some(""), None).is_err());
    assert!(j.remember("human:ann", &"x".repeat(BIG + 1), None, None).is_err());

    // forgetting the newest version makes the previous one current again
    let rep = j.forget(&Target::Id { id: v2.id }, true).unwrap();
    assert_eq!(rep.forgotten, 1);
    assert!(rep.proof.as_ref().unwrap().proven);
    assert_eq!(r.get_key("human:ann", "deploy-gate", None).unwrap().unwrap().id, v1.id);
    let ids: Vec<u64> = r.search(&args("deploy gate", 10)).unwrap().iter().map(|h| h.id).collect();
    assert!(ids.contains(&v1.id) && !ids.contains(&v2.id), "{ids:?}");
    drop((j, r)); // the reader shares the open store
    assert_eq!(grep_files(&store, b"release train dashboard"), 0);

    // reopen: everything (versions, deletions) as it was
    let j = Journal::open(&store).unwrap();
    let r = j.reader();
    assert_eq!(r.get_key("human:ann", "deploy-gate", None).unwrap().unwrap().id, v1.id);
    assert_eq!(r.history("human:ann", "deploy-gate").unwrap().len(), 1);
    assert!(r.get_id(v2.id).unwrap().is_none());
}

#[test]
fn redaction_and_forget_with_byte_scan_proof() {
    let s = Scratch::new("forget");
    let store = s.path("journal");
    let mut j = Journal::open(&store).unwrap();
    // secrets are masked before anything is stored
    // built at run time so no token-shaped literal sits in the source
    let key_owned = ["ghp", "_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef1234"].concat();
    let key = key_owned.as_str();
    let r = j.remember("human:ann", &format!("my token is {key} keep it safe"), None, None).unwrap();
    assert_eq!(r.redacted, 1);
    let rec = j.reader().get_id(r.id).unwrap().unwrap();
    assert!(rec.text.contains("[REDACTED:github:") && !rec.text.contains(key));
    drop(j);
    assert_eq!(grep_files(&store, key.as_bytes()), 0);

    let mut j = Journal::open(&store).unwrap();
    let t0 = 1_700_000_000_000_000i64;
    let mut ids = Vec::new();
    for i in 0..30 {
        let w = if i % 3 == 0 { "agent:bob" } else { "human:ann" };
        ids.push(
            j.remember(
                w,
                &format!("note {i}: unique-marker-{i:03}-qwertyuiop about the zebra parser"),
                None,
                Some(t0 + i),
            )
            .unwrap()
            .id,
        );
    }
    j.remember("human:ann", "versioned A unique-key-marker-alpha", Some("k1"), Some(t0 + 100)).unwrap();
    j.remember("human:ann", "versioned B unique-key-marker-bravo", Some("k1"), Some(t0 + 101)).unwrap();
    // the same text twice: forgetting one copy keeps the other
    let twin_a = j.remember("human:ann", "twin text unique-twin-marker-zzz", None, Some(t0 + 200)).unwrap();
    let twin_b = j.remember("agent:bob", "twin text unique-twin-marker-zzz", None, Some(t0 + 201)).unwrap();
    let r = j.reader();

    // by id
    let rep = j.forget(&Target::Id { id: ids[4] }, true).unwrap();
    assert_eq!((rep.forgotten, rep.writers.clone()), (1, vec!["human:ann".to_string()]));
    let p = rep.proof.unwrap();
    assert!(
        p.proven && p.needles == 1 && p.dead_rows_with_text == 0 && p.files_with_text == 0 && p.files_scanned > 3,
        "{p:?}"
    );
    assert!(r.get_id(ids[4]).unwrap().is_none());
    assert!(j.forget(&Target::Id { id: ids[4] }, true).is_err(), "already gone");

    // by key: every version
    let rep = j.forget(&Target::Key { key: "k1".into(), writer: "human:ann".into() }, true).unwrap();
    assert_eq!(rep.forgotten, 2);
    assert!(rep.proof.unwrap().proven);
    assert!(r.history("human:ann", "k1").unwrap().is_empty());

    // older than, one writer
    let rep = j.forget(&Target::OlderThan { before_us: t0 + 10, writer: Some("agent:bob".into()) }, true).unwrap();
    assert_eq!(rep.forgotten, 4); // bob's notes 0, 3, 6, 9
    assert!(rep.proof.unwrap().proven);
    assert!(r.get_id(ids[3]).unwrap().is_none() && r.get_id(ids[1]).unwrap().is_some());

    // a window, every writer
    let rep = j.forget(&Target::Window { from_us: t0 + 20, to_us: t0 + 25, writer: None }, true).unwrap();
    assert_eq!(rep.forgotten, 6);
    assert_eq!(rep.writers, vec!["agent:bob".to_string(), "human:ann".to_string()]);
    assert!(rep.proof.unwrap().proven);

    // one copy of a shared text: the other copy keeps it, and the proof says why
    let rep = j.forget(&Target::Id { id: twin_a.id }, true).unwrap();
    let p = rep.proof.unwrap();
    assert!(p.proven, "{p:?}");
    assert_eq!(p.dead_rows_with_text, 0);
    assert_eq!(r.get_id(twin_b.id).unwrap().unwrap().text, "twin text unique-twin-marker-zzz");

    // older than, everyone
    let rep = j.forget(&Target::OlderThan { before_us: t0 + 1_000, writer: None }, false).unwrap();
    assert_eq!(rep.forgotten, 30 - 1 - 4 - 6 + 1);
    assert!(rep.proof.is_none());
    assert!(j.reader().range(&Filters::default(), 100).unwrap().iter().all(|x| x.ts_us >= t0 + 1_000));
    drop((j, r));

    // independent check: no forgotten text in any file
    for i in [3usize, 4, 9, 20, 25, 29] {
        assert_eq!(grep_files(&store, format!("unique-marker-{i:03}-qwertyuiop").as_bytes()), 0, "note {i}");
    }
    assert_eq!(grep_files(&store, b"unique-key-marker-alpha"), 0);
    assert_eq!(grep_files(&store, b"unique-key-marker-bravo"), 0);
    assert_eq!(grep_files(&store, b"unique-twin-marker-zzz"), 0);
}

#[test]
fn shared_log_stores_work_the_same_and_are_detected() {
    let s = Scratch::new("shared");
    let store = s.path("journal");
    let mut opts = oojuro::journal::store_options();
    opts.trace.shared_log = true;
    {
        let mut j = Journal::open_with(&store, opts).unwrap();
        let a = j.remember("human:ann", "first version shared-log-marker-one", Some("k"), None).unwrap();
        let b = j.remember("human:ann", "second version shared-log-marker-two", Some("k"), None).unwrap();
        j.remember("agent:bob", "bob writes shared-log-marker-bob", None, None).unwrap();
        let r = j.reader();
        assert_eq!(r.search(&args("version", 5)).unwrap().iter().map(|h| h.id).collect::<Vec<_>>(), vec![b.id]);
        let rep = j.forget(&Target::Id { id: a.id }, true).unwrap();
        assert!(rep.proof.unwrap().proven);
        drop((j, r));
    }
    assert!(store.join("trace/shared-log").is_dir());
    assert!(oojuro::journal::store_options_for(&store).trace.shared_log);
    assert_eq!(grep_files(&store, b"shared-log-marker-one"), 0);
    // reopened by detection (whatever the engine default is)
    let mut j = Journal::open(&store).unwrap();
    assert_eq!(
        j.reader().get_key("human:ann", "k", None).unwrap().unwrap().text,
        "second version shared-log-marker-two"
    );
    let rep = j.forget(&Target::Window { from_us: 0, to_us: i64::MAX / 2, writer: None }, true).unwrap();
    assert_eq!(rep.forgotten, 2);
    assert!(rep.proof.unwrap().proven);
    drop(j);
    assert_eq!(grep_files(&store, b"shared-log-marker-two"), 0);
    assert_eq!(grep_files(&store, b"shared-log-marker-bob"), 0);
    // a private-journal store is detected as such
    let other = s.path("private");
    let mut j = Journal::open_with(&other, {
        let mut o = oojuro::journal::store_options();
        o.trace.shared_log = false;
        o
    })
    .unwrap();
    j.remember("human:ann", "private journal", None, None).unwrap();
    drop(j);
    assert!(!oojuro::journal::store_options_for(&other).trace.shared_log);
    assert_eq!(Journal::open(&other).unwrap().reader().stats().unwrap().records, 1);
}

/// F1 and F2 of the outcome eval (`docs/SEARCH_FIXES.md`). F1: `to` is part
/// of the engine's view, so a window ending before the records that name the
/// query's identifier searches what that window holds, exactly as the view
/// `as_of` at the same time does (it used to return nothing). F2: a record
/// nearly identical to the query ranks first and the list is still filled.
#[test]
fn search_windows_are_engine_views_and_lists_are_never_cut_short() {
    let s = Scratch::new("windows");
    let mut j = Journal::open(&s.path("journal")).unwrap();
    let t0 = 1_700_000_000_000_000i64;
    let w = "public:sympy";
    let mut older = Vec::new();
    for (i, t) in [
        "lambdify with the mpmath module loses precision on rationals",
        "nsolve precision drops when a rational is evaluated as a float",
        "printing rationals for mpmath: wrap them in mpf so precision is kept",
        "RisingFactorial is not supported by the mpmath printer",
        "the mpmath printer and lambdify modules argument",
        "Eq with rf and a rational right hand side",
    ]
    .iter()
    .enumerate()
    {
        older.push(j.remember(w, t, None, Some(t0 + i as i64)).unwrap().id);
    }
    let issue = "lambdify(modules='mpmath') doesn't wrap rationals: def _lambdifygenerated(x): \
                 return RisingFactorial(18, x) - 232/3, so nsolve loses precision on the rational";
    let at = t0 + 100;
    let first = j.remember(w, &format!("issue | {issue}"), None, Some(at)).unwrap().id;
    for i in 0..3 {
        // no exact score ties (the engine and the journal order ties differently)
        let text = format!("step {i}: _lambdifygenerated source printed for mpmath{}", " again".repeat(i as usize));
        j.remember(w, &text, None, Some(at + 10 + i)).unwrap();
    }
    j.remember("human:ann", "an unrelated note about lunch", None, Some(t0)).unwrap();
    let r = j.reader();
    let run = |to: Option<i64>, as_of: Option<i64>, k: usize| {
        let mut a = args(issue, k);
        a.filters.to_us = to;
        a.filters.as_of_us = as_of;
        a.filters.writers = vec![w.into()];
        r.search(&a).unwrap().iter().map(|h| (h.id, h.score_bits.to_bits())).collect::<Vec<_>>()
    };
    // F1: before the issue, no record names `_lambdifygenerated`
    let window = run(Some(at - 1), None, 10);
    assert_eq!(window.len(), older.len(), "{window:?}");
    assert_eq!(window, run(None, Some(at - 1), 10), "to and as_of at the same time see the same records");
    assert_eq!(run(Some(at - 1), None, 3), window[..3]);
    // F1 + F2: up to the issue, the issue first, then every older record
    let upto = run(Some(at), None, 10);
    assert_eq!(upto.len(), older.len() + 1, "{upto:?}");
    assert_eq!(upto[0].0, first);
    assert_eq!(upto, run(None, Some(at), 10));
    // F2: today, the near-verbatim issue first and the list filled
    let now = run(None, None, 10);
    assert_eq!(now.len(), older.len() + 4, "{now:?}");
    assert_eq!(now[0].0, first);
    assert_eq!(run(None, None, 2), now[..2]);
}

/// One tie rule (`docs/SEARCH_FIXES.md`): equal scores rank newer first,
/// then by id as text, in the engine and in the journal's merge of writers,
/// so a smaller k is always a prefix of a larger one.
#[test]
fn tied_scores_rank_the_same_for_every_k_and_across_writers() {
    let s = Scratch::new("ties");
    let mut j = Journal::open(&s.path("journal")).unwrap();
    let t0 = 1_700_000_000_000_000i64;
    for i in 0..24i64 {
        // identical scores; times repeat so that ids also break ties, and
        // ids cross from one to two digits
        let w = if i % 3 == 0 { "agent:b" } else { "agent:a" };
        j.remember(w, &format!("zebra parser note {}", 100 + i), None, Some(t0 + i / 4)).unwrap();
    }
    let r = j.reader();
    let ranked = |k: usize, writers: &[&str]| {
        let mut a = args("zebra parser", k);
        a.filters.writers = writers.iter().map(|w| w.to_string()).collect();
        r.search(&a).unwrap().iter().map(|h| (h.id, h.score_bits.to_bits(), h.ts_us)).collect::<Vec<_>>()
    };
    for writers in [&["agent:a"][..], &["agent:a", "agent:b"][..]] {
        let all = ranked(24, writers);
        assert!(all.windows(2).all(|w| w[0].1 == w[1].1), "every score ties: {all:?}");
        assert!(all.windows(2).all(|w| w[0].2 > w[1].2 || (w[0].2 == w[1].2 && w[0].0.to_string() < w[1].0.to_string())));
        for k in 1..all.len() {
            assert_eq!(ranked(k, writers), all[..k], "k={k} {writers:?}");
        }
    }
}
