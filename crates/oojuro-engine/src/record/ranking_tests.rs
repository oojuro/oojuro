use super::*;
use crate::capture_log::Identity;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
struct Guard {
    dir: PathBuf,
    generation: AtomicU64,
    namespace: [u8; 32],
    epoch: [u8; 32],
}
impl LifecycleGuard for Guard {
    fn current(&self) -> Result<rank_tables::Binding> {
        Ok(rank_tables::Binding {
            namespace: self.namespace,
            generation: self.generation.load(Ordering::SeqCst),
            epoch: self.epoch,
        })
    }
    fn directory(&self) -> &Path {
        &self.dir
    }
}
struct Fixture {
    root: PathBuf,
    log: RecordLog,
    guard: Guard,
}
impl Fixture {
    fn new(rows: &[(&str, bool, Option<&str>, i64)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "oojrank-{}",
            hex(&oojuro_store::crypto::random_key())
        ));
        fs::create_dir(&root).unwrap();
        let mut log = RecordLog::open_clock(
            &root.join("log"),
            &[94; 32],
            Identity {
                writer: "synthetic rank writer".into(),
                agent: 1,
                first_id: 1,
                adapter: "synthetic".into(),
            },
            Arc::new(|| Ok(100)),
        )
        .unwrap();
        let inputs = rows
            .iter()
            .map(|(text, indexed, key, valid_at)| Input {
                agent_id: "reported".into(),
                parent_id: None,
                valid_at: *valid_at,
                kind: Kind::Explicit,
                tool: None,
                status: None,
                exit: None,
                key: key.map(str::to_owned),
                text: (*text).into(),
                attrs: BTreeMap::new(),
                indexed: *indexed,
            })
            .collect::<Vec<_>>();
        log.append("fixture", &inputs).unwrap();
        let tip = log.tip().unwrap();
        let guard = Guard {
            dir: root.join("tables"),
            generation: AtomicU64::new(1),
            namespace: tip.identity,
            epoch: tip.origin,
        };
        Self { root, log, guard }
    }
    fn view(&self) -> View {
        View::open(&self.log, scope(), AsOf::default()).unwrap()
    }
    fn table(&self, view: &View, id: &str) -> Loaded {
        rank_tables::build(
            &self.log,
            view,
            &Filter::default(),
            &self.guard,
            rank_tables::Build {
                id,
                version: "1",
                table_version: "table1",
                work_units: 1000,
            },
            |source, out, work| {
                work.charge(source.len()).map_err(|_| Error::Bounds)?;
                out.write_all(b"synthetic_private_rank_table")
                    .map_err(|_| Error::Unavailable)?;
                Ok(())
            },
        )
        .unwrap();
        rank_tables::load(
            &self.log,
            view,
            &Filter::default(),
            &self.guard,
            id,
            "1",
            "table1",
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn scope() -> Scope<'static> {
    Scope {
        writer: "synthetic rank writer",
        owner: 1,
    }
}
fn query(text: &str, k: usize) -> Query<'_> {
    Query {
        scope: scope(),
        as_of: AsOf::default(),
        text,
        k,
        filter: Filter::default(),
        max_bytes: 1 << 20,
    }
}
enum Mode {
    Empty,
    Delta(f64),
    WrongLength,
    NaN,
    Expand(&'static str),
    Ids(Vec<String>),
    OverBudget,
}
struct Plugin<'a> {
    id: &'static str,
    mode: Mode,
    calls: AtomicUsize,
    dispatches: AtomicUsize,
    advance: Option<&'a Guard>,
}
impl Plugin<'_> {
    fn new(id: &'static str, mode: Mode) -> Self {
        Self {
            id,
            mode,
            calls: AtomicUsize::new(0),
            dispatches: AtomicUsize::new(0),
            advance: None,
        }
    }
}
impl rank::Ranker for Plugin<'_> {
    fn identity(&self) -> rank::Identity<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        rank::Identity {
            id: self.id,
            version: "1",
            table_version: Some("table1"),
        }
    }
    fn work_units(&self, _: &[rank::Term<'_>], n: usize) -> usize {
        if matches!(self.mode, Mode::OverBudget) {
            0
        } else {
            n + 10
        }
    }
    fn expand(
        &self,
        _: &[rank::Term<'_>],
        _: &rank::Context<'_>,
        work: &mut rank::WorkBudget,
    ) -> std::result::Result<Vec<rank::Expansion>, rank::Skip> {
        work.charge(1)?;
        Ok(match self.mode {
            Mode::Expand(s) => vec![rank::Expansion {
                text: s.into(),
                repetitions: NonZeroU32::new(1).unwrap(),
            }],
            _ => vec![],
        })
    }
    fn candidates(
        &self,
        _: &[rank::Term<'_>],
        _: &rank::Context<'_>,
        _: usize,
        work: &mut rank::WorkBudget,
    ) -> std::result::Result<Vec<String>, rank::Skip> {
        work.charge(1)?;
        Ok(match &self.mode {
            Mode::Ids(v) => v.clone(),
            _ => vec![],
        })
    }
    fn rescore(
        &self,
        _: &[rank::Term<'_>],
        c: &[rank::Candidate<'_>],
        work: &mut rank::WorkBudget,
    ) -> std::result::Result<Vec<f64>, rank::Skip> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        if let Some(guard) = self.advance {
            guard.generation.fetch_add(1, Ordering::SeqCst);
        }
        work.charge(c.len())?;
        Ok(match self.mode {
            Mode::WrongLength => vec![],
            Mode::NaN => vec![f64::NAN; c.len()],
            Mode::Delta(delta) => vec![delta; c.len()],
            _ => vec![0.0; c.len()],
        })
    }
}
fn stage<'a>(hook: Hook, plugin: &'a Plugin<'a>, table: Option<&'a Loaded>) -> Stage<'a> {
    Stage {
        hook,
        ranker: plugin,
        table,
        weight: 1.0,
        enabled: true,
    }
}
fn pipeline<'a>(stages: &'a [Stage<'a>]) -> Pipeline<'a> {
    Pipeline {
        enabled: true,
        stages,
        ..Default::default()
    }
}
fn signatures(hits: &[Hit]) -> Vec<(u64, u64, u64, bool)> {
    hits.iter()
        .map(|h| {
            (
                h.record.id,
                h.lexical_bits.to_bits(),
                h.final_bits.to_bits(),
                h.continued,
            )
        })
        .collect()
}
fn baseline(f: &Fixture, index: &Index, view: &View, q: &Query<'_>) -> Vec<(u64, u64, u64, bool)> {
    index
        .search_view(&f.log, view, q)
        .unwrap()
        .hits
        .iter()
        .map(|h| (h.record.id, h.bits.to_bits(), h.bits.to_bits(), h.continued))
        .collect()
}
#[test]
fn off_never_calls_plugin_and_all_skipped_preserves_ids_bits_and_f2() {
    let f = Fixture::new(&[
        ("alpha_beta anchor", true, None, 1),
        ("alpha beta continuation", true, None, 2),
        ("alpha body", true, None, 3),
        ("!!!", false, None, 4),
    ]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let plugin = Plugin::new("rescore", Mode::WrongLength);
    let table = f.table(&view, "rescore");
    let stages = [stage(Hook::Rescore, &plugin, Some(&table))];
    for text in ["alpha_beta", "alpha", r#""!!!""#, "", "key:missing"] {
        let q = query(text, 10);
        let expected = baseline(&f, &index, &view, &q);
        let off = Pipeline {
            enabled: false,
            stages: &stages,
            ..Default::default()
        };
        assert_eq!(
            signatures(
                &search(&index, &f.log, &view, &q, &f.guard, &off)
                    .unwrap()
                    .hits
            ),
            expected
        );
        assert_eq!(plugin.calls.load(Ordering::SeqCst), 0);
    }
    for text in ["alpha_beta", "alpha", r#""!!!""#, "", "key:missing"] {
        let q = query(text, 10);
        let ranked = search(&index, &f.log, &view, &q, &f.guard, &pipeline(&stages)).unwrap();
        assert_eq!(signatures(&ranked.hits), baseline(&f, &index, &view, &q));
        assert!(ranked.receipt.stages[0].skipped.is_some());
    }
}
#[test]
fn empty_expand_missing_table_nonfinite_and_work_fail_transactionally_fallback() {
    let f = Fixture::new(&[("alpha one", true, None, 1), ("alpha two", true, None, 2)]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let q = query("alpha", 2);
    for (hook, mode, with_table) in [
        (Hook::Expand, Mode::Empty, true),
        (Hook::Rescore, Mode::NaN, true),
        (Hook::Rescore, Mode::OverBudget, true),
        (Hook::Rescore, Mode::Delta(10.0), false),
    ] {
        let plugin = Plugin::new("optional", mode);
        let table = f.table(&view, "optional");
        let stages = [stage(
            hook,
            &plugin,
            if with_table { Some(&table) } else { None },
        )];
        let result = search(&index, &f.log, &view, &q, &f.guard, &pipeline(&stages)).unwrap();
        assert_eq!(signatures(&result.hits), baseline(&f, &index, &view, &q));
        assert!(result.receipt.stages[0].skipped.is_some());
    }
}
#[test]
fn negative_window_scores_keep_tail_and_f2_groups_stay_first() {
    let f = Fixture::new(&[
        ("alpha alpha", true, None, 1),
        ("alpha", true, None, 2),
        ("alpha filler", true, None, 3),
    ]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let q = query("alpha", 2);
    let original = index.search_view(&f.log, &view, &q).unwrap();
    let plugin = Plugin::new("negative", Mode::Delta(-1_000_000.0));
    let table = f.table(&view, "negative");
    let stages = [stage(Hook::Rescore, &plugin, Some(&table))];
    let mut config = pipeline(&stages);
    config.window = 1;
    let result = search(&index, &f.log, &view, &q, &f.guard, &config).unwrap();
    assert_eq!(result.hits.len(), 2);
    assert_ne!(result.hits[0].record.id, original.hits[0].record.id);
    assert!(result.hits.iter().all(|h| h.deltas == vec![0.0]));
    let f = Fixture::new(&[
        ("alpha_beta anchor", true, None, 1),
        ("alpha beta later", true, None, 2),
    ]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let table = f.table(&view, "negative");
    let stages = [stage(Hook::Rescore, &plugin, Some(&table))];
    let mut config = pipeline(&stages);
    config.window = 1;
    let result = search(
        &index,
        &f.log,
        &view,
        &query("alpha_beta", 2),
        &f.guard,
        &config,
    )
    .unwrap();
    assert!(!result.hits[0].continued);
    assert!(result.hits[1].continued);
}
#[test]
fn expansion_candidates_use_shared_union_model_and_reject_foreign_duplicates_hidden() {
    let f = Fixture::new(&[
        ("alpha one", true, None, 1),
        ("nebula candidate", true, None, 2),
        ("nebula hidden", false, None, 3),
        ("nebula superseded", true, Some("key"), 4),
        ("alpha winner", true, Some("key"), 5),
    ]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let expand = Plugin::new("expand", Mode::Expand("nebula"));
    let table = f.table(&view, "expand");
    let stages = [stage(Hook::Expand, &expand, Some(&table))];
    let result = search(
        &index,
        &f.log,
        &view,
        &query("alpha", 10),
        &f.guard,
        &pipeline(&stages),
    )
    .unwrap();
    assert_eq!(result.receipt.policy, "expanded-pool-v1/f2-original-v1");
    let equivalent = index
        .search_view(&f.log, &view, &query("alpha nebula", 10))
        .unwrap();
    assert_eq!(
        result
            .hits
            .iter()
            .map(|h| (h.record.id, h.lexical_bits.to_bits(), h.continued))
            .collect::<Vec<_>>(),
        equivalent
            .hits
            .iter()
            .map(|h| (h.record.id, h.bits.to_bits(), h.continued))
            .collect::<Vec<_>>()
    );
    assert!(result.hits.iter().any(|h| h.record.id == 2));
    assert!(
        result
            .hits
            .iter()
            .all(|h| h.record.id != 3 && h.record.id != 4)
    );
    for values in [
        vec!["999".into()],
        vec!["2".into(), "2".into()],
        vec!["3".into()],
        vec!["4".into()],
        vec!["02".into()],
    ] {
        let plugin = Plugin::new("source", Mode::Ids(values));
        let table = f.table(&view, "source");
        let stages = [stage(Hook::Candidates, &plugin, Some(&table))];
        let q = query("alpha", 10);
        let result = search(&index, &f.log, &view, &q, &f.guard, &pipeline(&stages)).unwrap();
        assert_eq!(signatures(&result.hits), baseline(&f, &index, &view, &q));
    }
    let plugin = Plugin::new("source", Mode::Ids(vec!["2".into()]));
    let table = f.table(&view, "source");
    let stages = [stage(Hook::Candidates, &plugin, Some(&table))];
    let result = search(
        &index,
        &f.log,
        &view,
        &query("alpha", 10),
        &f.guard,
        &pipeline(&stages),
    )
    .unwrap();
    assert!(result.hits.iter().any(|h| h.record.id == 2));
}
#[test]
fn tables_are_sealed_bound_to_view_filter_key_and_current_lifecycle_and_load_has_no_writes() {
    let f = Fixture::new(&[("alpha", true, None, 1)]);
    let view = f.view();
    assert!(
        rank_tables::load(
            &f.log,
            &view,
            &Filter::default(),
            &f.guard,
            "rank",
            "1",
            "table1"
        )
        .is_err()
    );
    assert!(!f.guard.dir.exists());
    let first = f.table(&view, "rank");
    let second = f.table(&view, "rank");
    assert_ne!(first.content_version, second.content_version);
    let reopened = rank_tables::load(
        &f.log,
        &view,
        &Filter::default(),
        &f.guard,
        "rank",
        "1",
        "table1",
    )
    .unwrap();
    assert_eq!(second.content_version, reopened.content_version);
    let other = RecordLog::open(
        &f.root.join("different-key"),
        &[95; 32],
        f.log.identity().clone(),
    )
    .unwrap();
    let other_view = View::open(&other, scope(), AsOf::default()).unwrap();
    assert!(
        rank_tables::load(
            &other,
            &other_view,
            &Filter::default(),
            &f.guard,
            "rank",
            "1",
            "table1"
        )
        .is_err()
    );

    for e in fs::read_dir(&f.guard.dir).unwrap() {
        let e = e.unwrap();
        let bytes = fs::read(e.path()).unwrap();
        assert!(
            !bytes
                .windows(b"synthetic_private_rank_table".len())
                .any(|w| w == b"synthetic_private_rank_table")
        );
    }
    let filter = Filter {
        kind: Some(Kind::Input),
        ..Filter::default()
    };
    assert!(rank_tables::load(&f.log, &view, &filter, &f.guard, "rank", "1", "table1").is_err());
    let as_of = AsOf {
        known_at: 100,
        ..AsOf::default()
    };
    let old = View::open(&f.log, scope(), as_of).unwrap();
    assert!(
        rank_tables::load(
            &f.log,
            &old,
            &Filter::default(),
            &f.guard,
            "rank",
            "1",
            "table1"
        )
        .is_err()
    );
    assert!(
        rank_tables::load(
            &f.log,
            &view,
            &Filter::default(),
            &f.guard,
            "rank",
            "2",
            "table1"
        )
        .is_err()
    );
    f.guard.generation.fetch_add(1, Ordering::SeqCst);
    assert!(
        rank_tables::load(
            &f.log,
            &view,
            &Filter::default(),
            &f.guard,
            "rank",
            "1",
            "table1"
        )
        .is_err()
    );
    let table = f.table(&view, "rank");
    let plugin = Plugin {
        advance: Some(&f.guard),
        ..Plugin::new("rank", Mode::Delta(1.0))
    };
    let stages = [stage(Hook::Rescore, &plugin, Some(&table))];
    assert!(matches!(
        search(
            &Index::rebuild(&f.log).unwrap(),
            &f.log,
            &view,
            &query("alpha", 1),
            &f.guard,
            &pipeline(&stages)
        ),
        Err(Error::Authority)
    ));
}
#[test]
fn corrupt_table_rebuild_and_registered_erase_refuse_foreign_files() {
    let f = Fixture::new(&[("alpha", true, None, 1)]);
    let view = f.view();
    let _ = f.table(&view, "rank");
    fs::write(f.guard.dir.join("RANK_TABLE"), b"synthetic corruption").unwrap();
    assert!(
        rank_tables::load(
            &f.log,
            &view,
            &Filter::default(),
            &f.guard,
            "rank",
            "1",
            "table1"
        )
        .is_err()
    );
    let _ = f.table(&view, "rank");
    let unknown = f.guard.dir.join("unregistered");
    fs::write(&unknown, b"synthetic").unwrap();
    assert!(rank_tables::remove_all(&f.log, &f.guard).is_err());
    assert!(unknown.exists());
    fs::remove_file(unknown).unwrap();
    rank_tables::remove_all(&f.log, &f.guard).unwrap();
    assert!(!f.guard.dir.join("RANK_TABLE").exists());
}

#[test]
fn expansion_never_promotes_a_new_identifier_into_the_original_anchor_group() {
    let f = Fixture::new(&[
        ("alpha_beta original anchor", true, None, 1),
        ("other_name expansion only", true, None, 2),
        ("alpha beta continuation", true, None, 3),
    ]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let plugin = Plugin::new("expand", Mode::Expand("other_name"));
    let table = f.table(&view, "expand");
    let stages = [stage(Hook::Expand, &plugin, Some(&table))];
    let result = search(
        &index,
        &f.log,
        &view,
        &query("alpha_beta", 10),
        &f.guard,
        &pipeline(&stages),
    )
    .unwrap();
    assert_eq!(result.hits[0].record.id, 1);
    assert!(!result.hits[0].continued);
    assert!(
        result
            .hits
            .iter()
            .filter(|h| h.record.id != 1)
            .all(|h| h.continued)
    );
}

#[test]
fn rescore_rechecks_lifecycle_before_each_callback() {
    let f = Fixture::new(&[("alpha original", true, None, 1)]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let mut first = Plugin::new("first", Mode::Delta(1.0));
    first.advance = Some(&f.guard);
    let second = Plugin::new("second", Mode::Delta(1.0));
    let first_table = f.table(&view, "first");
    let second_table = f.table(&view, "second");
    let stages = [
        stage(Hook::Rescore, &first, Some(&first_table)),
        stage(Hook::Rescore, &second, Some(&second_table)),
    ];
    let result = search(
        &index,
        &f.log,
        &view,
        &query("alpha", 1),
        &f.guard,
        &pipeline(&stages),
    );
    assert!(matches!(result, Err(Error::Authority)));
    assert_eq!(first.dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(second.dispatches.load(Ordering::SeqCst), 0);
}

#[test]
fn ranked_exact_wire_budget_counts_receipts_deltas_and_escaped_originals() {
    let large = format!("needle {} λ🦀", "\0\"\\\n".repeat(32_768));
    let f = Fixture::new(&[(&large, true, Some("large"), 1), ("needle small", true, None, 2)]);
    let view = f.view();
    let index = Index::rebuild(&f.log).unwrap();
    let plugin = Plugin::new("rescore", Mode::Delta(0.5));
    let table = f.table(&view, "rescore");
    let stages = [stage(Hook::Rescore, &plugin, Some(&table))];
    for enabled in [false, true] {
        let p = Pipeline { enabled, ..pipeline(&stages) };
        let mut q = query("needle", 10);
        q.max_bytes = MAX_OUTPUT_BYTES;
        let full = index.search_ranked(&f.log, &view, &q, &f.guard, &p).unwrap();
        let bytes = serde_json::to_vec(&full).unwrap();
        assert!(bytes.len() > 300_000 && full.hits.len() == 2);
        assert!(!full.receipt.stages.is_empty());
        assert!(full.hits.iter().all(|h| h.deltas.len() == 1));
        q.max_bytes = bytes.len();
        assert_eq!(serde_json::to_vec(&index.search_ranked(&f.log, &view, &q, &f.guard, &p).unwrap()).unwrap(), bytes);
        q.max_bytes -= 1;
        assert!(matches!(index.search_ranked(&f.log, &view, &q, &f.guard, &p), Err(Error::Bounds)));
        q.max_bytes = bytes.len() + 1;
        assert_eq!(serde_json::to_vec(&index.search_ranked(&f.log, &view, &q, &f.guard, &p).unwrap()).unwrap(), bytes);
    }
}
