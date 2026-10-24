//! Correctness gates every arm must pass before its quality or speed counts:
//! byte-exact originals, two-clock as-of visibility (no future leakage),
//! read-your-writes freshness, crash recovery under SIGKILL, determinism.
use crate::{
    ArmHandle, ArmSpec, Doc, Request, Result, SearchReq,
    scorecard::{Ctx, Runs, Scorecard},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

pub struct Options {
    pub arms: Vec<ArmSpec>,
    pub out: std::path::PathBuf,
}

fn doc(id: &str, key: &str, text: &str, valid_at: i64, recorded_at: i64) -> Doc {
    Doc { id: id.into(), key: key.into(), text: text.into(), valid_at, recorded_at, meta: BTreeMap::new() }
}

fn query(text: &str, valid_at: i64, known_at: i64, k: usize) -> SearchReq {
    SearchReq { text: text.into(), valid_at, known_at, k, context: BTreeMap::new() }
}

pub fn tricky_texts() -> Vec<(String, String)> {
    let mut long = String::new();
    while long.len() < 300_000 {
        long.push_str("lorem ipsum dolor sit amet ");
    }
    long.push_str("longtailmarker");
    [
        ("plain", "plain ascii text".to_string()),
        ("unicode", "héllo wörld — 你好 🌍 e\u{301} ﬁ".to_string()),
        ("crlf", "line one\r\nline two\r\n".to_string()),
        ("cr", "old mac\rline\n\r".to_string()),
        ("spaces", "  leading and trailing  ".to_string()),
        ("bom", "\u{feff}bom first".to_string()),
        ("nul", "nul\u{0}inside".to_string()),
        ("tabs", "a\tb\tc".to_string()),
        ("json", "{\"tool\": \"grep\", \"args\": [1, 2.50, null], \"s\": \"\\\"q\\\"\"}".to_string()),
        ("empty", String::new()),
        ("long", long),
        ("unbroken", "x".repeat(70_000)),
    ]
    .into_iter()
    .map(|(k, v)| (format!("exact-{k}"), v))
    .collect()
}

/// Deterministic synthetic corpus (xorshift over a fixed vocabulary).
pub fn synthetic(n: usize, prefix: &str) -> Vec<Doc> {
    const WORDS: &[&str] = &[
        "alpha", "bravo", "cache", "delta", "error", "fetch", "graph", "hash", "index", "json", "kernel",
        "lock", "merge", "node", "offset", "parse", "query", "retry", "shard", "token", "update", "vector",
        "write", "yield", "zone", "socket", "buffer", "thread", "timeout", "commit",
    ];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    (0..n)
        .map(|i| {
            let len = 8 + (next() % 24) as usize;
            let text: Vec<&str> = (0..len).map(|_| WORDS[(next() % WORDS.len() as u64) as usize]).collect();
            doc(&format!("{prefix}{i}"), "", &format!("{} item{i}", text.join(" ")), 100 + i as i64, 100 + i as i64)
        })
        .collect()
}

fn reopen(ctx: &Ctx, spec: &ArmSpec, store: &Path, log: &Path) -> Result<ArmHandle> {
    let mut arm = spec.start(false, ctx.isolation.as_ref(), log)?;
    arm.open(store, &BTreeMap::new())?;
    Ok(arm)
}

fn mismatches(ids: &[String], got: &[Option<String>], want: &[(String, String)]) -> Vec<String> {
    ids.iter()
        .zip(got)
        .zip(want)
        .filter(|((_, g), (_, t))| g.as_deref() != Some(t.as_str()))
        .map(|((id, _), _)| id.clone())
        .collect()
}

fn exact(runs: &mut Runs, ctx: &Ctx, spec: &ArmSpec, run: usize) -> Result<()> {
    let mut s = ctx.session(spec, run, "exact")?;
    let texts = tricky_texts();
    let docs: Vec<Doc> = texts.iter().enumerate().map(|(i, (id, t))| doc(id, "", t, 1 + i as i64, 1 + i as i64)).collect();
    s.arm.append_batch(docs)?;
    let ids: Vec<String> = texts.iter().map(|(id, _)| id.clone()).collect();
    let bad = mismatches(&ids, &s.arm.get(ids.clone())?, &texts);
    runs.gate("byte_exact", bad.is_empty(), json!({"cases": ids.len(), "mismatched": bad}));
    // The same bytes after a clean close and a fresh process.
    let store = s.store.clone();
    s.arm.close()?;
    s.arm.finish()?;
    let mut arm = reopen(ctx, spec, &store, &store.with_file_name("reopen.stderr"))?;
    let bad = mismatches(&ids, &arm.get(ids.clone())?, &texts);
    runs.gate("byte_exact_after_reopen", bad.is_empty(), json!({"cases": ids.len(), "mismatched": bad}));
    let order: Vec<String> = arm.get_range(1, texts.len() as i64, 100)?.into_iter().map(|d| d.id).collect();
    runs.gate("get_range_order", order == ids, json!({"expected": ids.len(), "got": order.len()}));
    arm.close()?;
    arm.finish()?;
    if !ctx.keep {
        let _ = std::fs::remove_dir_all(store.parent().unwrap_or(&store));
    }
    Ok(())
}

fn as_of(runs: &mut Runs, ctx: &Ctx, spec: &ArmSpec, run: usize) -> Result<()> {
    let mut s = ctx.session(spec, run, "asof")?;
    s.arm.append_batch(vec![
        doc("rev-1", "rev:k", "zebraquartz version one", 10, 10),
        doc("rev-2", "rev:k", "zebraquartz version two", 20, 30),
        doc("rev-3", "rev:k", "zebraquartz version three backfill", 15, 40),
        doc("fut-1", "", "zebraquartz future note", 1000, 1000),
        doc("late-1", "", "zebraquartz recorded late", 5, 900),
    ])?;
    let cases: &[((i64, i64), &[&str])] = &[
        ((12, 100), &["rev-1"]),
        ((25, 25), &["rev-1"]),
        ((25, 35), &["rev-2"]),
        ((25, 50), &["rev-2"]),
        ((16, 50), &["rev-3"]),
        ((5, 50), &[]),
        ((5, 899), &[]),
        ((5, 900), &["late-1"]),
        ((2000, 2000), &["rev-2", "fut-1", "late-1"]),
    ];
    let mut failures = Vec::new();
    for ((valid, known), want) in cases {
        let (hits, _) = s.arm.search(query("zebraquartz", *valid, *known, 10))?;
        let got: BTreeSet<String> = hits.into_iter().map(|h| h.id).collect();
        let want: BTreeSet<String> = want.iter().map(|s| s.to_string()).collect();
        if got != want {
            failures.push(json!({"valid_at": valid, "known_at": known, "want": want, "got": got}));
        }
    }
    runs.gate("as_of_visibility", failures.is_empty(), json!({"cases": cases.len(), "failures": failures}));
    // Read-your-writes: a fresh record is searchable by the very next query.
    let mut misses = 0;
    for n in 0..20 {
        let token = format!("freshmarker{n}q{}", n * 7919);
        s.arm.append_batch(vec![doc(&format!("fresh-{n}"), "", &format!("note {token} end"), 50 + n, 50 + n)])?;
        let (hits, _) = s.arm.search(query(&token, 50 + n, 50 + n, 5))?;
        misses += usize::from(!hits.iter().any(|h| h.id == format!("fresh-{n}")));
    }
    runs.gate("freshness", misses == 0, json!({"trials": 20, "misses": misses}));
    ctx.finish(s)?;
    Ok(())
}

fn determinism(runs: &mut Runs, ctx: &Ctx, spec: &ArmSpec, run: usize) -> Result<()> {
    let corpus = synthetic(3000, "det-");
    let queries: Vec<String> = synthetic(60, "q-").into_iter().map(|d| d.text).collect();
    let mut lists = Vec::new();
    for copy in 0..2 {
        let mut s = ctx.session(spec, run, &format!("det{copy}"))?;
        for chunk in corpus.chunks(500) {
            s.arm.append_batch(chunk.to_vec())?;
        }
        let mut out = Vec::new();
        for q in &queries {
            let (hits, _) = s.arm.search(query(q, 10_000, 10_000, 20))?;
            out.push(hits.into_iter().map(|h| h.id).collect::<Vec<_>>());
        }
        lists.push(out);
        ctx.finish(s)?;
    }
    let same = lists[0].iter().zip(&lists[1]).filter(|(a, b)| a == b).count();
    runs.gate("determinism", same == queries.len(), json!({"queries": queries.len(), "identical": same}));
    Ok(())
}

/// SIGKILL the arm with a large batch in flight, at three delays; every batch
/// acknowledged before the kill must come back byte-exact and searchable.
fn crash(runs: &mut Runs, ctx: &Ctx, spec: &ArmSpec, run: usize) -> Result<()> {
    let dir = ctx.work.join(&spec.label).join(format!("run{run}-crash"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let store = dir.join("store");
    let mut acked: Vec<Doc> = Vec::new();
    let mut generated = BTreeMap::new();
    let mut previously_recovered: BTreeMap<String, Doc> = BTreeMap::new();
    let mut inflight_report = Vec::new();
    let mut failures = Vec::new();
    for (trial, delay_ms) in [0u64, 5, 30].into_iter().enumerate() {
        let mut arm = reopen(ctx, spec, &store, &dir.join(format!("trial{trial}.stderr")))?;
        // PostgreSQL promises one transaction per batch. Other arms may durably
        // commit smaller chunks before their outer batch acknowledgement.
        let atomic_batch_required = matches!(arm.info()?.name.as_str(), "postgres" | "postgres-frequency");
        for b in 0..10 {
            let batch: Vec<Doc> = (0..20)
                .map(|i| {
                    let t = (trial * 1000 + b * 20 + i) as i64;
                    doc(&format!("crash-{trial}-{b}-{i}"), "", &format!("crashtok{trial}x{b}x{i} payload {t}"), t, t)
                })
                .collect();
            arm.append_batch(batch.clone())?;
            generated.extend(batch.iter().cloned().map(|d| (d.id.clone(), d)));
            acked.extend(batch);
        }
        let inflight: Vec<Doc> = (0..2000)
            .map(|i| {
                let t = 50_000 + trial as i64 * 10_000 + i;
                doc(&format!("crash-{trial}-inflight-{i}"), "", &format!("inflight{trial}x{i} {}", "pad ".repeat(50)), t, t)
            })
            .collect();
        generated.extend(inflight.iter().cloned().map(|d| (d.id.clone(), d)));
        let ArmHandle::Process(p) = &mut arm else { return Err("crash gate needs a process arm".into()) };
        p.send_only(&Request::AppendBatch { docs: inflight.clone() })?;
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        p.kill()?;
        let mut arm = reopen(ctx, spec, &store, &dir.join(format!("trial{trial}-recover.stderr")))?;
        let mut changed = 0;
        let mut acked_reply_cardinality_invalid = false;
        for docs in acked.chunks(500) {
            let got = arm.get(docs.iter().map(|d| d.id.clone()).collect())?;
            acked_reply_cardinality_invalid |= got.len() != docs.len();
            changed += got.iter().zip(docs).filter(|(g, d)| g.as_deref() != Some(d.text.as_str())).count();
        }
        let inflight_got = arm.get(inflight.iter().map(|d| d.id.clone()).collect())?;
        let inflight_reply_cardinality_invalid = inflight_got.len() != inflight.len();
        let present = inflight_got
            .iter()
            .zip(&inflight)
            .filter(|(g, d)| g.as_deref() == Some(d.text.as_str()))
            .count();
        // Text-only reads cannot prove the original keys, clocks or metadata.
        let recovered = arm.get_range(i64::MIN, i64::MAX, 10_000)?;
        let recovered_len = recovered.len();
        let recovered: BTreeMap<_, _> = recovered.into_iter().map(|d| (d.id.clone(), d)).collect();
        let inflight_get_matches_range = inflight_got.iter().zip(&inflight)
            .all(|(got, d)| got.as_deref() == recovered.get(&d.id).map(|row| row.text.as_str()));
        let full_acked_changed = acked.iter().filter(|d| recovered.get(&d.id) != Some(*d)).count();
        let inflight_rows = inflight.iter().filter(|d| recovered.contains_key(&d.id)).count();
        let inflight_changed = inflight.iter().filter(|d| recovered.get(&d.id).is_some_and(|got| got != *d)).count();
        let atomic = (inflight_rows == 0 || inflight_rows == inflight.len())
            && present == inflight_rows && inflight_changed == 0;
        let complete_unique = recovered_len < 10_000 && recovered.len() == recovered_len;
        let all_returned_originals = recovered.iter().all(|(id, got)| generated.get(id) == Some(got));
        let previous_survive = previously_recovered.iter().all(|(id, d)| recovered.get(id) == Some(d));
        inflight_report.push(json!({"delay_ms": delay_ms, "inflight_present": present, "of": inflight.len(),
            "inflight_rows": inflight_rows, "inflight_changed": inflight_changed, "atomic": atomic,
            "atomic_batch_required": atomic_batch_required, "full_acked_changed": full_acked_changed,
            "complete_unique": complete_unique, "all_returned_originals": all_returned_originals,
            "previous_survive": previous_survive, "acked_reply_cardinality_invalid": acked_reply_cardinality_invalid,
            "inflight_reply_cardinality_invalid": inflight_reply_cardinality_invalid,
            "inflight_get_matches_range": inflight_get_matches_range}));
        let last = acked.last().cloned().ok_or("no acked docs")?;
        let token = last.text.split(' ').next().unwrap_or_default().to_string();
        let (hits, _) = arm.search(query(&token, 1_000_000, 1_000_000, 5))?;
        let searchable = hits.iter().any(|h| h.id == last.id);
        if changed > 0 || full_acked_changed > 0 || (atomic_batch_required && !atomic)
            || !complete_unique || !all_returned_originals || !previous_survive
            || !inflight_get_matches_range || acked_reply_cardinality_invalid
            || inflight_reply_cardinality_invalid || !searchable {
            failures.push(json!({"trial": trial, "acked_missing_or_changed": changed,
                "full_acked_changed": full_acked_changed, "atomic": atomic,
                "atomic_batch_required": atomic_batch_required, "complete_unique": complete_unique,
                "all_returned_originals": all_returned_originals, "previous_survive": previous_survive,
                "acked_reply_cardinality_invalid": acked_reply_cardinality_invalid,
                "inflight_reply_cardinality_invalid": inflight_reply_cardinality_invalid,
                "inflight_get_matches_range": inflight_get_matches_range,
                "last_acked_searchable": searchable}));
        }
        previously_recovered = recovered;
        arm.close()?;
        arm.finish()?;
    }
    runs.gate(
        "crash_recovery",
        failures.is_empty(),
        json!({"trials": 3, "acked_records": acked.len(), "failures": failures, "inflight": inflight_report,
               "note": "all acknowledged and previously recovered full Docs survive; PostgreSQL additionally promises all-or-nothing for each batch"}),
    );
    if !ctx.keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

pub fn run(ctx: &Ctx, o: &Options) -> Result<Scorecard> {
    let mut card = Scorecard::new("gates", "synthetic", "all", ctx);
    let _lock = ctx.bench_lock()?;
    card.settings.insert(
        "cases".into(),
        json!("byte_exact(+after reopen), get_range_order, as_of_visibility, freshness, determinism, crash_recovery (SIGKILL x3)"),
    );
    for spec in &o.arms {
        let mut runs = Runs::default();
        let mut info = Default::default();
        let mut error = None;
        for run in 0..ctx.runs {
            let outcome = (|| -> Result<()> {
                let probe = ctx.session(spec, run, "probe")?;
                info = probe.info.clone();
                ctx.finish(probe)?;
                exact(&mut runs, ctx, spec, run)?;
                as_of(&mut runs, ctx, spec, run)?;
                determinism(&mut runs, ctx, spec, run)?;
                crash(&mut runs, ctx, spec, run)?;
                Ok(())
            })();
            if let Err(e) = outcome {
                eprintln!("{} gates run {run}: {e}", spec.label);
                error = Some(e.to_string());
                break;
            }
            eprintln!("gates {} run {run} done", spec.label);
        }
        card.arms.push(runs.into_result(spec, info, error));
    }
    card.save(&o.out)?;
    Ok(card)
}
