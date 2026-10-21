use super::*;
use super::V3 as Nine;
use crate::expand_range;
use crate::text::{literal_probability, normalize_word, phrase_start, query_terms, retrieval_terms};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicUsize, Ordering},
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(std::env::temp_dir().join(format!(
                "nine-local-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let (Ok(target), Ok(parent)) =
            (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        {
            if target.parent() == Some(parent.as_path()) {
                let _ = fs::remove_dir_all(target);
            }
        }
    }
}
fn r(id: &str, key: &str, text: &str, valid: i64, known: i64) -> Record {
    let mut r = Record::new(id, key, text, valid);
    r.recorded_at = known;
    r.verified = false;
    r
}

#[test]
fn revision_view_matches_independent_oracle_at_both_clock_boundaries() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let mut originals = Vec::new();
    let mut seed = 63u64;
    for n in 0..257 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let valid = ((seed >> 16) % 17) as i64;
        let known = ((seed >> 32) % 17) as i64;
        let record = r(
            &format!("v{n}"),
            &format!("key{}", n % 13),
            "needle evidence",
            valid,
            known,
        );
        db.insert(record.clone())?;
        originals.push(record);
    }
    for valid in -1..=17 {
        for known in -1..=17 {
            let mut q = Query::new("needle", valid);
            q.known_at = known;
            let expected = oojuro_core::visible_latest(&originals, &q);
            let mut view = HashMap::new();
            for i in (0..originals.len()).rev() {
                assert_eq!(db.latest(i, &q, &mut view), expected.contains(&i));
            }
            assert!(view.len() <= 13);
            if valid == 17 && known == 17 {
                assert!(view.is_empty());
            }
        }
    }
    Ok(())
}

#[test]
fn late_known_revision_cache_is_shared_across_addressing_paths_and_queries_do_not_leak()
-> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    for n in 0..128 {
        db.insert(r(
            &format!("v{n}"),
            "timeline",
            "repair_cache needle evidence",
            n,
            if n < 64 { 1 } else { 1000 },
        ))?;
    }
    for text in [
        "needle",
        "repair_cache",
        "\"repair_cache needle\"",
        "key:timeline",
        "",
    ] {
        let mut q = Query::new(text, 127);
        q.known_at = 1;
        let old = db.search_refs(&q, i64::MIN)?;
        assert_eq!(old.matches.len(), 1);
        assert_eq!(old.matches[0].id, "v63");
        q.known_at = 1000;
        assert_eq!(db.search_refs(&q, i64::MIN)?.matches[0].id, "v127");
        q.known_at = 1;
        db.clear_query_cache();
        assert_eq!(
            serde_json::to_vec(&db.search_refs(&q, i64::MIN)?)?,
            serde_json::to_vec(&old)?
        );
    }
    let mut q = Query::new("needle", 127);
    q.known_at = 1;
    let mut view = HashMap::new();
    for n in 0..128 {
        db.latest(n, &q, &mut view);
    }
    assert_eq!(view.len(), 1);
    assert_eq!(view["timeline"], Some(63));
    drop(db);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(db.search_refs(&q, i64::MIN)?.matches[0].id, "v63");
    assert!(db.search_refs(&q, 64)?.matches.is_empty());
    Ok(())
}

#[test]
fn lossless_delta_archive_recovers_unicode_and_bounds_chains() -> Result<()> {
    let tmp = Scratch::new();
    let mut seed = 761u64;
    let mut payload = String::new();
    for _ in 0..12_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        payload.push((33 + ((seed >> 32) % 90) as u8) as char);
    }
    let originals: Vec<_> = (0..12)
        .map(|n| format!("東京🦀 tool output {payload}\ncompletion={n}\n終わり"))
        .collect();
    {
        let mut db = Nine::open(&tmp.0)?;
        for (i, text) in originals.iter().enumerate() {
            db.insert(r(&format!("r{i}"), "tool", text, i as i64, i as i64))?;
        }
        let stats = db.stats()?;
        assert!(stats.delta_records > 0);
        assert!(stats.archive_bytes < (stats.raw_text_bytes / 2) as u64);
        assert!(db.rows.iter().all(|r| r.depth <= MAX_DELTA_DEPTH));
        assert!(Nine::open(&tmp.0).is_err());
    }
    let db = Nine::open(&tmp.0)?;
    for (i, text) in originals.iter().enumerate() {
        assert_eq!(db.read(&format!("r{i}")), Some(text.as_str()));
    }
    Ok(())
}

#[test]
fn historical_source_visibility_and_future_statistics_do_not_leak() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.insert(r("old", "runtime", "compiler runtime Node 22", 10, 10))?;
    let q = Query::new("compiler runtime", 30);
    let before = db.search(&q)?;
    db.insert(r("new", "runtime", "compiler runtime Node 24", 20, 50))?;
    db.insert(r(
        "future",
        "different",
        "compiler compiler runtime runtime future",
        99,
        99,
    ))?;
    let after = db.search(&q)?;
    assert_eq!(before.hits[0].record.id, after.hits[0].record.id);
    assert_eq!(before.hits[0].score, after.hits[0].score);
    let mut known = q.clone();
    known.known_at = 60;
    assert_eq!(db.search(&known)?.hits[0].record.id, "new");
    assert_eq!(
        db.range(10, 20, 100)?
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["old", "new"]
    );
    Ok(())
}

#[test]
fn query_cache_covers_identity_and_invalidates_on_source_changes() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.insert(r("one", "key", "exact cache needle", 1, 1))?;
    let q = Query::new("cache needle", 10);
    let first = db.search(&q)?;
    assert!(!db.metrics().cache_hit);
    assert_eq!(
        first.hits[0].record.text,
        db.search(&q)?.hits[0].record.text
    );
    assert!(db.metrics().cache_hit);
    let mut short = q.clone();
    short.max_context_bytes = 0;
    assert!(db.search(&short)?.hits.is_empty());
    assert!(!db.metrics().cache_hit);
    db.insert(r("two", "key", "exact cache needle revised", 2, 2))?;
    assert_eq!(db.search(&q)?.hits[0].record.id, "two");
    assert!(!db.metrics().cache_hit);
    assert_eq!(db.stats()?.persisted_queries, 4);
    drop(db);
    assert_eq!(Nine::open(&tmp.0)?.stats()?.persisted_queries, 4);
    Ok(())
}

#[test]
fn verified_feedback_routes_a_repeated_query_without_rewriting_sources() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.insert(r(
        "failed",
        "failed",
        &format!("E_TEMP {}", "unrelated output ".repeat(500)),
        1,
        1,
    ))?;
    db.insert(r(
        "fix",
        "fix",
        "Use the validated recipe alpha and fresh configuration.",
        2,
        2,
    ))?;
    let mut q = Query::new("E_TEMP", 30);
    q.k = 1;
    assert_eq!(db.search(&q)?.hits[0].record.id, "failed");
    db.record_outcome("E_TEMP", "failed", false, 25, "public-fixture-executor")?;
    db.record_outcome("E_TEMP", "fix", true, 25, "public-fixture-executor")?;
    assert!(
        db.record_outcome("E_TEMP", "fix", true, 25, "public-fixture-executor")
            .is_err()
    );
    assert_eq!(db.search(&q)?.hits[0].record.id, "fix");
    assert!(!db.metrics().cache_hit);
    assert!(!db.record("fix").unwrap().verified);
    q.known_at = 20;
    assert_eq!(db.search(&q)?.hits[0].record.id, "failed");
    drop(db);
    let mut db = Nine::open(&tmp.0)?;
    q.known_at = 30;
    assert_eq!(db.search(&q)?.hits[0].record.id, "fix");
    Ok(())
}

#[test]
fn local_search_accepts_no_vector_and_uses_no_empty_query_fallback_for_unknown_text() -> Result<()>
{
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let mut record = r("a", "a", "migration failed schema v1; use schema v2", 1, 1);
    record.embedding = Some(vec![1.; 3072]);
    assert!(db.insert(record).is_err());
    db.put(1, "migration failed schema v1; use schema v2")?;
    assert!(
        db.search(&Query::new("totally_unknown_needle", 10))?
            .hits
            .is_empty()
    );
    let mut q = Query::new("migration", 10);
    q.embedding = Some(vec![1.; 3072]);
    assert!(db.search(&q).is_err());
    assert_eq!(db.find("schema", 1, 10, 50)?.matches.len(), 1);
    let found = db.search(&Query::new("schema", 10))?;
    assert_eq!(found.hits.len(), 1);
    assert!(found.hits[0].uncertainty_bits.is_none());
    assert_eq!(serde_json::to_vec(&found.context_packet(2)?)?.len(), 2);
    Ok(())
}

#[test]
fn archive_partial_tail_recovers_but_checksum_corruption_fails() -> Result<()> {
    let tmp = Scratch::new();
    {
        let mut db = Nine::open(&tmp.0)?;
        db.put(1, "preserve this exact original")?;
    }
    let path = tmp.0.join("timeline.nine");
    let size = fs::metadata(&path)?.len();
    {
        let mut f = OpenOptions::new().append(true).open(&path)?;
        f.write_all(&[1, 2, 3, 4, 5])?;
        f.sync_all()?;
    }
    {
        let db = Nine::open(&tmp.0)?;
        assert_eq!(db.len(), 1);
        assert_eq!(fs::metadata(&path)?.len(), size);
    }
    {
        let mut f = OpenOptions::new().read(true).write(true).open(&path)?;
        f.seek(SeekFrom::End(-1))?;
        let mut b = [0];
        f.read_exact(&mut b)?;
        f.seek(SeekFrom::End(-1))?;
        f.write_all(&[b[0] ^ 1])?;
        f.sync_all()?;
    }
    assert!(Nine::open(&tmp.0).is_err());
    Ok(())
}

#[test]
fn long_sources_are_discoverable_without_spending_the_context_budget() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let original = format!(
        "unique_failure_identifier\n{}終わり",
        "tool output\n".repeat(30_000)
    );
    let id = db.put(1, &original)?;
    let mut q = Query::new("unique_failure_identifier", 2);
    q.max_context_bytes = 2;
    let refs = db.search_refs(&q, 0)?;
    assert_eq!(refs.matches.len(), 1);
    assert_eq!(refs.matches[0].id, id);
    assert_eq!(refs.matches[0].bytes, original.len());
    assert_eq!(db.read(&id), Some(original.as_str()));
    assert!(db.search(&q)?.hits.is_empty());
    drop(db);
    assert_eq!(Nine::open(&tmp.0)?.read(&id), Some(original.as_str()));
    Ok(())
}

#[test]
fn query_cache_is_bounded_and_new_feedback_invalidates_it() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let id = db.put(1, "the shared needle")?;
    for at in 2..50 {
        db.find("needle", 0, at, 50)?;
    }
    assert_eq!(db.stats()?.cache_entries, CACHE_ENTRIES);
    assert!(db.stats()?.cache_bytes_estimate <= CACHE_BYTES);
    db.find("needle", 0, 2, 50)?;
    assert!(!db.metrics().cache_hit);
    db.find("needle", 0, 2, 50)?;
    assert!(db.metrics().cache_hit);
    db.record_outcome("needle", &id, true, 2, "fixture-executor")?;
    assert_eq!(db.stats()?.cache_entries, 0);
    Ok(())
}

#[test]
fn repeated_record_envelopes_compress_without_losing_metadata() -> Result<()> {
    let tmp = Scratch::new();
    let mut seed = 191u64;
    let large: String = (0..8000)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (33 + ((seed >> 32) % 90) as u8) as char
        })
        .collect();
    let mut originals = Vec::new();
    {
        let mut db = Nine::open(&tmp.0)?;
        for i in 0..16 {
            let mut record = r(
                &format!("source-{i}"),
                &format!("unique-{i}"),
                &format!("Short unrelated payload {i}"),
                i,
                i,
            );
            record
                .metadata
                .insert("long-repeated-provenance".into(), large.clone());
            db.insert(record.clone())?;
            originals.push(record);
        }
        let serialized_bytes = originals
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap().len())
            .sum::<usize>();
        assert!(db.stats()?.archive_bytes < (serialized_bytes / 2) as u64);
    }
    let db = Nine::open(&tmp.0)?;
    for original in originals {
        assert_eq!(
            serde_json::to_vec(db.record(&original.id).unwrap())?,
            serde_json::to_vec(&original)?
        );
    }
    Ok(())
}

#[test]
fn sparse_scope_scores_match_dense_query_likelihood_on_historical_views() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let words = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "running", "runs", "file",
        "files",
    ];
    for i in 0..160 {
        let text = (0..(11 + i % 57))
            .map(|j| words[(i * 3 + j * (1 + i % 3)) % words.len()])
            .collect::<Vec<_>>()
            .join(" ");
        db.insert(r(
            &format!("row-{i}"),
            &format!("key-{}", i % 80),
            &text,
            (i / 10) as i64,
            (i / 10 + i % 4) as i64,
        ))?;
    }
    for at in [3, 9, 20] {
        for text in [
            "alpha beta",
            "beta beta gamma",
            "epsilon unknownword",
            "running files",
        ] {
            let mut q = Query::new(text, at);
            q.known_at = at - 1;
            q.k = 240;
            let mut query = query_terms(text);
            let originals: Vec<_> = db.rows.iter().map(|row| row.record.clone()).collect();
            let visible = oojuro_core::visible_latest(&originals, &q);
            let eligible: Vec<_> = db
                .rows
                .iter()
                .enumerate()
                .filter(|(i, _)| visible.contains(i))
                .map(|(i, row)| {
                    let mut counts = HashMap::<String, usize>::new();
                    for term in retrieval_terms(&row.record.text) {
                        *counts
                            .entry(normalize_word(&term).into_owned())
                            .or_default() += 1;
                    }
                    (i, counts)
                })
                .filter(|(_, counts)| query.keys().any(|term| counts.contains_key(term)))
                .collect();
            query.retain(|term, _| eligible.iter().any(|(_, counts)| counts.contains_key(term)));
            let total: usize = eligible.iter().map(|(_, c)| c.len()).sum();
            // Below the floor the background is padded with the view's own
            // statistics (every visible record) up to the floor's mass.
            let view: Vec<HashMap<String, usize>> = db
                .rows
                .iter()
                .enumerate()
                .filter(|(i, _)| visible.contains(i))
                .map(|(_, row)| {
                    let mut counts = HashMap::<String, usize>::new();
                    for term in retrieval_terms(&row.record.text) {
                        *counts.entry(normalize_word(&term).into_owned()).or_default() += 1;
                    }
                    counts
                })
                .collect();
            let view_total = view.iter().map(HashMap::len).sum::<usize>() as f64;
            let floor = crate::BACKGROUND_FLOOR_RECORDS * MU;
            let pad = (!eligible.is_empty() && (total as f64) < floor).then(|| floor - total as f64);
            let background: BTreeMap<_, _> = query
                .keys()
                .map(|term| {
                    let count = eligible
                        .iter()
                        .map(|(_, c)| usize::from(c.contains_key(term)))
                        .sum::<usize>();
                    let in_view = view.iter().filter(|c| c.contains_key(term)).count() as f64;
                    let p = match pad {
                        None => (count as f64 + literal_probability(term)) / (total as f64 + 1.),
                        Some(r) => {
                            (count as f64 + r * in_view / view_total + literal_probability(term))
                                / (total as f64 + r + 1.)
                        }
                    };
                    (term.clone(), p)
                })
                .collect();
            let dense: HashMap<_, _> = eligible
                .iter()
                .map(|(i, counts)| {
                    let length = counts.values().sum::<usize>() as f64;
                    let score = query
                        .iter()
                        .map(|(term, n)| {
                            let p = (counts.len() as f64
                                * counts.get(term).copied().unwrap_or(0) as f64
                                / length
                                + MU * background[term])
                                / (counts.len() as f64 + MU);
                            *n as f64 * (p.log2() - background[term].log2())
                        })
                        .sum::<f64>();
                    (db.rows[*i].record.id.clone(), score)
                })
                .collect();
            let found = db.search_refs(&q, i64::MIN)?;
            assert_eq!(found.matches.len(), dense.len());
            for m in &found.matches {
                assert!((m.score_bits - dense[m.id.as_str()]).abs() < 1e-10);
            }
            assert!(
                found
                    .matches
                    .windows(2)
                    .all(|m| m[0].score_bits + 1e-10 >= m[1].score_bits)
            );
            q.k = 3;
            let short = db.search_refs(&q, i64::MIN)?;
            assert_eq!(
                short.matches.iter().map(|m| &m.id).collect::<Vec<_>>(),
                found
                    .matches
                    .iter()
                    .take(3)
                    .map(|m| &m.id)
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

#[test]
fn scope_mass_matches_hand_probabilities_escape_mass_and_addressed_fallback() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.insert(r("left", "left-key", "alpha alpha beta", 1, 1))?;
    db.insert(r("right", "right-key", "alpha gamma gamma gamma", 1, 1))?;
    let words = ["alpha", "beta", "gamma"];
    // Scope mass 4 (2 + 2) is below the floor: the background is padded with
    // the view's statistics (here the same two records) by floor - 4.
    let pad = crate::BACKGROUND_FLOOR_RECORDS * MU - 4.;
    let background = [
        (2. + pad * 2. / 4. + literal_probability("alpha")) / (4. + pad + 1.),
        (1. + pad * 1. / 4. + literal_probability("beta")) / (4. + pad + 1.),
        (1. + pad * 1. / 4. + literal_probability("gamma")) / (4. + pad + 1.),
    ];
    let escape = (1. - words.iter().map(|w| literal_probability(w)).sum::<f64>()) / (4. + pad + 1.);
    assert!((background.iter().sum::<f64>() + escape - 1.).abs() < 1e-12);
    let found = db.find("alpha beta", i64::MIN, 2, 10)?;
    for (id, counts, n) in [("left", [2., 1., 0.], 3.), ("right", [1., 0., 3.], 4.)] {
        let probabilities: Vec<_> = counts
            .iter()
            .zip(background)
            .map(|(tf, b)| (2. * tf / n + 128. * b) / 130.)
            .collect();
        assert!((probabilities.iter().sum::<f64>() + 128. * escape / 130. - 1.).abs() < 1e-12);
        let score =
            (probabilities[0] / background[0]).log2() + (probabilities[1] / background[1]).log2();
        let hit = found.matches.iter().find(|m| m.id == id).unwrap();
        assert!((hit.score_bits - score).abs() < 1e-10);
    }
    let literal = db.find("\"alpha\"", i64::MIN, 2, 10)?;
    // Length mass 7 (3 + 4), padded the same way: alpha is 3 of 7 tokens.
    let pad = crate::BACKGROUND_FLOOR_RECORDS * MU - 7.;
    let old_background = (3. + pad * 3. / 7. + literal_probability("alpha")) / (7. + pad + 1.);
    for (id, tf, n) in [("left", 2., 3.), ("right", 1., 4.)] {
        let expected = ((tf + 128. * old_background) / ((n + 128.) * old_background)).log2();
        assert!(
            (literal
                .matches
                .iter()
                .find(|m| m.id == id)
                .unwrap()
                .score_bits
                - expected)
                .abs()
                < 1e-10
        );
    }
    assert!(db.find("alpha", 2, 2, 10)?.matches.is_empty());
    db.insert(r("empty", "empty-key", "", 1, 1))?;
    db.record_outcome("alpha beta", "empty", true, 1, "scope-fixture-executor")?;
    let learned = db.find("alpha beta", i64::MIN, 2, 10)?;
    assert!(learned.matches.iter().all(|m| m.score_bits.is_finite()));
    assert!(
        (learned
            .matches
            .iter()
            .find(|m| m.id == "empty")
            .unwrap()
            .score_bits
            - 1.)
            .abs()
            < 1e-12
    );
    drop(db);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(
        serde_json::to_vec(&db.find("alpha beta", i64::MIN, 2, 10)?)?,
        serde_json::to_vec(&learned)?
    );
    assert_eq!(db.read("left"), Some("alpha alpha beta"));
    Ok(())
}

#[test]
fn whole_source_repetition_preserves_scope_background_and_all_ordinary_scores() -> Result<()> {
    let first = Scratch::new();
    let second = Scratch::new();
    let mut a = Nine::open(&first.0)?;
    let mut b = Nine::open(&second.0)?;
    let records = [
        r("one", "one-key", "alpha alpha beta beta", 1, 1),
        r("two", "two-key", "alpha gamma gamma gamma", 1, 1),
        r(
            "three",
            "three-key",
            "gamma delta running runners workers café",
            1,
            1,
        ),
    ];
    for row in &records {
        a.insert(row.clone())?;
        let mut repeated = row.clone();
        if row.id == "two" {
            repeated.text = std::iter::repeat_n(row.text.as_str(), 13)
                .collect::<Vec<_>>()
                .join(" ");
        }
        b.insert(repeated)?;
    }
    assert_eq!(a.rows[1].scope, b.rows[1].scope);
    assert_eq!(a.rows[1].length * 13, b.rows[1].length);
    let mut expected = Vec::new();
    for text in [
        "alpha beta",
        "gamma delta",
        "running worker",
        "unknown alpha",
        "alpha",
    ] {
        let x = a.find(text, i64::MIN, 2, 20)?;
        let y = b.find(text, i64::MIN, 2, 20)?;
        assert_eq!(
            x.matches.iter().map(|m| &m.id).collect::<Vec<_>>(),
            y.matches.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
        for (x, y) in x.matches.iter().zip(&y.matches) {
            assert!((x.score_bits - y.score_bits).abs() < 1e-10);
        }
        assert!(
            (a.metrics().background_query_bits - b.metrics().background_query_bits).abs() < 1e-10
        );
        expected.push((text, y));
    }
    drop(b);
    let mut b = Nine::open(&second.0)?;
    for (text, y) in expected {
        assert_eq!(
            serde_json::to_vec(&b.find(text, i64::MIN, 2, 20)?)?,
            serde_json::to_vec(&y)?
        );
    }
    for row in records {
        assert_eq!(a.read(&row.id), Some(row.text.as_str()));
    }
    Ok(())
}

#[test]
fn exact_identifiers_keep_components_and_respect_historical_revisions() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.insert(r(
        "exact",
        "routine",
        &format!(
            "fn repair_shadow_page() {{}}\n{}",
            "implementation detail ".repeat(500)
        ),
        1,
        1,
    ))?;
    for i in 0..40 {
        db.insert(r(
            &format!("fragments-{i}"),
            &format!("fragments-{i}"),
            "repair repair shadow shadow page page",
            1,
            1,
        ))?;
    }
    assert_eq!(
        db.find("repair_shadow_page", 0, 2, 1)?.matches[0].id,
        "exact"
    );
    assert_eq!(
        db.find(
            "Find the implementation of the Rust function repair_shadow_page",
            0,
            2,
            1
        )?
        .matches[0]
            .id,
        "exact"
    );
    assert!(
        db.find("shadow", 0, 2, 100)?
            .matches
            .iter()
            .any(|m| m.id == "exact")
    );
    db.insert(r(
        "replacement",
        "routine",
        "fn unrelated_method() {}",
        3,
        4,
    ))?;
    assert_eq!(
        db.find("REPAIR_SHADOW_PAGE", 0, 2, 1)?.matches[0].id,
        "exact"
    );
    assert!(
        !db.find("repair_shadow_page", 0, 5, 100)?
            .matches
            .iter()
            .any(|m| m.id == "exact")
    );
    drop(db);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(
        db.find("repair_shadow_page", 0, 2, 1)?.matches[0].id,
        "exact"
    );
    Ok(())
}

#[test]
fn quoted_phrases_require_exact_bytes_before_ranking_and_feedback() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.insert(r(
        "definition",
        "routine",
        "fn repair_shadow_page() { /* done */ }",
        1,
        1,
    ))?;
    db.insert(r(
        "caller",
        "caller",
        "repair_shadow_page(); unrelated fn elsewhere",
        1,
        1,
    ))?;
    db.insert(r(
        "prefix",
        "prefix",
        "fn repair_shadow_page_extra() {}",
        1,
        1,
    ))?;
    let phrase = "\"fn repair_shadow_page\"";
    assert_eq!(
        db.find(phrase, 0, 2, 50)?
            .matches
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["definition"]
    );
    assert!(!db.metrics().cache_hit);
    db.find(phrase, 0, 2, 50)?;
    assert!(db.metrics().cache_hit);
    db.find("fn repair_shadow_page", 0, 2, 50)?;
    assert!(!db.metrics().cache_hit);
    db.insert(r("new", "routine", "fn different_method() {}", 3, 4))?;
    db.record_outcome(phrase, "caller", true, 4, "fixture-verifier")?;
    assert!(db.find(phrase, 0, 5, 50)?.matches.is_empty());
    assert_eq!(db.find(phrase, 0, 2, 50)?.matches[0].id, "definition");
    db.put(6, "Unicode 🦀 exact punctuation")?;
    assert_eq!(db.find("\"🦀\"", 0, 7, 50)?.matches.len(), 1);
    let range = db.passage("definition", phrase, 30)?.unwrap();
    assert!(range.text.contains("fn repair_shadow_page"));
    assert_eq!(
        range.text,
        &db.read("definition").unwrap()[range.start..range.end]
    );
    assert!(db.passage("caller", phrase, 30)?.is_none());
    assert!(db.passage("definition", phrase, 4)?.is_none());
    db.put(7, "xa a a")?;
    assert_eq!(db.find("\"a a\"", 0, 8, 50)?.matches.len(), 1);
    assert_eq!(phrase_start("xaaaaaaaaaa aaaa", "aaaa"), Some(12));
    assert!(expand_range("café", 3, 4, 16).is_err());
    drop(db);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(db.find(phrase, 0, 2, 50)?.matches[0].id, "definition");
    Ok(())
}

#[test]
fn future_statistics_can_change_phrase_scan_plan_without_changing_scores() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    db.put(1, "alpha beta stable evidence")?;
    let before = db.find("\"alpha beta\"", 0, 2, 50)?;
    let bits = db.metrics().background_query_bits;
    for _ in 0..8 {
        db.put(50, "alpha future unrelated evidence")?;
    }
    let after = db.find("\"alpha beta\"", 0, 2, 50)?;
    assert_eq!(before.matches[0].id, after.matches[0].id);
    assert_eq!(before.matches[0].score_bits, after.matches[0].score_bits);
    assert_eq!(bits, db.metrics().background_query_bits);
    Ok(())
}

#[test]
fn normalized_words_preserve_literal_identifiers_originals_and_time() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let original = "Workers are running in café.";
    db.insert(r("old", "workers", original, 1, 1))?;
    db.insert(r("other", "other", "A runner runs nearby.", 1, 1))?;
    db.insert(r("exact", "exact", "fn running_workers() {}", 1, 1))?;
    db.insert(r("similar", "similar", "fn runs_workers() {}", 1, 1))?;
    assert!(
        db.find("worker runs", 0, 2, 50)?
            .matches
            .iter()
            .any(|m| m.id == "old")
    );
    assert_eq!(
        db.find("\"running\"", 0, 2, 50)?
            .matches
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["old"]
    );
    // The identifier's own record is its anchored view; past it the list
    // goes on with the query read as words (docs/SEARCH_FIXES.md, F2).
    assert_eq!(
        db.find("running_workers", 0, 2, 1)?
            .matches
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["exact"]
    );
    let found = db.find("running_workers", 0, 2, 50)?.matches;
    assert_eq!(found[0].id, "exact");
    assert!(!found[0].continued && found[1..].iter().all(|m| m.continued));
    let words: Vec<String> = db
        .find("running workers", 0, 2, 50)?
        .matches
        .into_iter()
        .map(|m| m.id)
        .filter(|id| id != "exact")
        .collect();
    assert_eq!(found[1..].iter().map(|m| m.id.clone()).collect::<Vec<_>>(), words);
    assert!(words.contains(&"old".to_string()) && words.contains(&"similar".to_string()));
    let part = db.passage("old", "worker runs café", 64)?.unwrap();
    assert!(part.text.contains("running"));
    assert_eq!(part.text, &original[part.start..part.end]);
    db.insert(r("new", "workers", "Workers stopped.", 3, 4))?;
    assert!(db.find("\"running\"", 0, 5, 50)?.matches.is_empty());
    assert_eq!(db.find("\"running\"", 0, 2, 50)?.matches[0].id, "old");
    assert_eq!(db.read("old"), Some(original));
    drop(db);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(db.find("\"running\"", 0, 2, 50)?.matches[0].id, "old");
    assert_eq!(db.read("old"), Some(original));
    Ok(())
}

#[test]
fn query_receipts_identify_the_model_and_keep_legacy_receipts_readable() -> Result<()> {
    let tmp = Scratch::new();
    let mut db = Nine::open(&tmp.0)?;
    let id = db.put(1, "A worker is running.")?;
    drop(db);
    let path = tmp.0.join("queries.timeline");
    let (mut log, _) = Journal::open::<serde_json::Value>(&path, QUERY_MAGIC, true)?;
    let query = [0u8; 32];
    log.append(&serde_json::json!({"query":query,"from":0,"at":1,"known_at":1,"ids":[id],"entropy_bits":0.}))?;
    drop(log);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(db.stats()?.persisted_queries, 1);
    db.find("worker", 0, 2, 5)?;
    drop(db);
    let (_, receipts) = Journal::open::<serde_json::Value>(&path, QUERY_MAGIC, true)?;
    assert_eq!(receipts.len(), 2);
    assert!(receipts[0].get("model").is_none());
    let model = receipts[1]["model"].as_str().unwrap();
    assert!(model.contains(RETRIEVAL_COUNT_MODEL));
    assert!(model.contains(RETRIEVAL_NORMALIZATION));
    Ok(())
}

#[test]
fn stores_with_null_entropy_receipts_open_again() -> Result<()> {
    let tmp = Scratch::new();
    let id = Nine::open(&tmp.0)?.put(1, "A worker is running.")?;
    let path = tmp.0.join("queries.timeline");
    let (mut log, _) = Journal::open::<serde_json::Value>(&path, QUERY_MAGIC, true)?;
    let query = [0u8; 32];
    log.append(&serde_json::json!({"query":query,"from":0,"at":1,"known_at":1,
        "ids":[id],"entropy_bits":null,"model":"dirichlet-scope-128-v1"}))?;
    drop(log);
    let mut db = Nine::open(&tmp.0)?;
    assert_eq!(db.stats()?.persisted_queries, 1);
    assert_eq!(db.find("worker", 0, 2, 5)?.matches.len(), 1);
    drop(db);
    assert_eq!(Nine::open(&tmp.0)?.stats()?.persisted_queries, 2);
    Ok(())
}
