use super::*;
use crate::{
    audit::{Domain, Signer},
    capture_log::Identity,
    record::managed::{self, FileWitness, ManagedLog, RetainedControl, Selection},
};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use std::{
    fs,
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
const DATA: [u8; 32] = [71; 32];
struct Fixture {
    root: PathBuf,
    control: Arc<RetainedControl>,
    key: [u8; 32],
}
impl Fixture {
    fn new() -> Self {
        Self::with_key(DATA)
    }
    fn with_key(key: [u8; 32]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ooj-managed-rank-{}",
            oojuro_store::crypto::random_key()
                .iter()
                .map(|c| format!("{c:02x}"))
                .collect::<String>()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let signer = Signer::from_pkcs8([73; 32], Domain::NativeLifecycle, pkcs8.as_ref()).unwrap();
        let control = Arc::new(
            RetainedControl::open(
                &root.join("control"),
                &[72; 32],
                signer,
                [73; 32],
                FileWitness::open(&root.join("witness")).unwrap(),
                managed::identity_binding(&key, &identity()).unwrap(),
            )
            .unwrap(),
        );
        Self { root, control, key }
    }
    fn log(&self) -> ManagedLog {
        ManagedLog::create(
            &self.root.join("data"),
            &self.key,
            identity(),
            self.control.clone(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn identity() -> Identity {
    Identity {
        writer: "synthetic-managed-rank".into(),
        agent: 7,
        first_id: 9,
        adapter: "synthetic".into(),
    }
}
fn scope() -> Scope<'static> {
    Scope {
        writer: "synthetic-managed-rank",
        owner: 7,
    }
}
fn input(text: &str, time: i64, key: Option<&str>, indexed: bool) -> Input {
    Input {
        agent_id: "reported-source".into(),
        parent_id: None,
        valid_at: time,
        kind: Kind::Explicit,
        tool: None,
        status: None,
        exit: None,
        key: key.map(str::to_owned),
        text: text.into(),
        attrs: BTreeMap::new(),
        indexed,
    }
}
struct Plugin {
    calls: AtomicUsize,
    delta: Option<f64>,
    change: Option<PathBuf>,
}
impl Plugin {
    fn new(delta: Option<f64>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delta,
            change: None,
        }
    }
}
impl rank::Ranker for Plugin {
    fn identity(&self) -> rank::Identity<'_> {
        rank::Identity {
            id: "synthetic",
            version: "1",
            table_version: Some("table1"),
        }
    }
    fn work_units(&self, _: &[rank::Term<'_>], n: usize) -> usize {
        n
    }
    fn rescore(
        &self,
        _: &[rank::Term<'_>],
        c: &[rank::Candidate<'_>],
        w: &mut rank::WorkBudget,
    ) -> std::result::Result<Vec<f64>, rank::Skip> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        w.charge(c.len())?;
        if let Some(path) = &self.change {
            fs::write(path, b"invalid witness").map_err(|_| rank::Skip::Failed)?;
        }
        Ok(self.delta.map(|d| vec![d; c.len()]).unwrap_or_default())
    }
}
fn stages<'a>(plugin: &'a Plugin, table: Option<&'a Loaded>) -> [Stage<'a>; 1] {
    [Stage {
        hook: Hook::Rescore,
        ranker: plugin,
        table,
        weight: 1.0,
        enabled: true,
    }]
}
fn pipeline<'a>(stages: &'a [Stage<'a>]) -> Pipeline<'a> {
    Pipeline {
        enabled: true,
        stages,
        ..Default::default()
    }
}
fn build(view: &ManagedRankView<'_>) {
    rank_tables::managed::build(
        view,
        rank_tables::Build {
            id: "synthetic",
            version: "1",
            table_version: "table1",
            work_units: 1000,
        },
        |rows, out, work| {
            work.charge(rows.len()).map_err(|_| Error::Bounds)?;
            assert!(rows.iter().all(|r| !r.text.contains("stored-only")));
            out.write_all(b"synthetic-private-derived-table")
                .map_err(|_| Error::Unavailable)
        },
    )
    .unwrap();
}
fn load(view: &ManagedRankView<'_>) -> Loaded {
    rank_tables::managed::load(view, "synthetic", "1", "table1").unwrap()
}
fn signature(hits: &[Hit]) -> Vec<(u64, u64, u64, bool)> {
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
#[test]
fn managed_off_and_allskip_match_direct_lexical_including_empty_and_stored_key() {
    let f = Fixture::new();
    let mut log = f.log();
    let rows = vec![
        input("alpha_beta original anchor", 1, None, true),
        input("alpha beta continuation", 2, None, true),
        input("alpha older key", 3, Some("state"), true),
        input("stored-only current winner", 4, Some("state"), false),
        input("alpha unkeyed", 5, None, true),
    ];
    let ack = log.append("seed", &rows).unwrap();
    let mut direct = RecordLog::open_clock(
        &f.root.join("direct"),
        &[74; 32],
        identity(),
        Arc::new(move || Ok(ack.commit.recorded_at[0])),
    )
    .unwrap();
    direct.append("seed", &rows).unwrap();
    let index = Index::rebuild(&direct).unwrap();
    let plugin = Plugin::new(None);
    for cuts in [
        AsOf::default(),
        AsOf {
            valid_at: 3,
            ..Default::default()
        },
    ] {
        log.with_rank_view(scope(), cuts, Filter::default(), |view| {
            assert_eq!(view.scope().writer, scope().writer);
            build(view);
            let table = load(view);
            let enabled = stages(&plugin, Some(&table));
            for text in ["alpha_beta", "alpha", "", "key:state", r#""stored-only""#] {
                let query = Query {
                    scope: scope(),
                    as_of: cuts,
                    text,
                    k: 10,
                    filter: Filter::default(),
                    max_bytes: 1 << 20,
                };
                let expected = index.search(&direct, &query)?;
                let off = view.search_ranked(text, 10, 1 << 20, &Pipeline::default())?;
                assert_eq!(
                    off.hits
                        .iter()
                        .map(|h| (h.record.id, h.lexical_bits.to_bits(), h.continued))
                        .collect::<Vec<_>>(),
                    expected
                        .hits
                        .iter()
                        .map(|h| (h.record.id, h.bits.to_bits(), h.continued))
                        .collect::<Vec<_>>()
                );
                let skipped = view.search_ranked(text, 10, 1 << 20, &pipeline(&enabled))?;
                assert_eq!(signature(&off.hits), signature(&skipped.hits));
                assert_eq!(off.binding, *view.binding());
            }
            Ok(())
        })
        .unwrap();
    }
}
#[test]
fn managed_real_tables_reopen_encrypted_without_query_writes_and_pin_scope_filter_key() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("seed", &[input("alpha", 1, None, true)])
        .unwrap();
    let (directory, version) = log
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            assert!(rank_tables::managed::load(view, "synthetic", "1", "table1").is_err());
            assert!(!view.directory().exists());
            build(view);
            let loaded = load(view);
            let disk = fs::read(view.directory().join("RANK_TABLE")).unwrap();
            assert!(
                !disk
                    .windows(b"synthetic-private-derived-table".len())
                    .any(|w| w == b"synthetic-private-derived-table")
            );
            Ok((view.directory().to_path_buf(), loaded.content_version))
        })
        .unwrap();
    drop(log);
    let log = ManagedLog::open(&f.root.join("data"), &DATA, identity(), f.control.clone()).unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        let before = fs::read(directory.join("RANK_TABLE")).unwrap();
        assert_eq!(load(view).content_version, version);
        assert_eq!(fs::read(directory.join("RANK_TABLE")).unwrap(), before);
        assert!(rank_tables::managed::load(view, "synthetic", "wrong", "table1").is_err());
        Ok(())
    })
    .unwrap();
    log.with_rank_view(
        scope(),
        AsOf {
            known_at: 0,
            ..Default::default()
        },
        Filter::default(),
        |view| {
            assert!(rank_tables::managed::load(view, "synthetic", "1", "table1").is_err());
            Ok(())
        },
    )
    .unwrap();
    log.with_rank_view(
        scope(),
        AsOf::default(),
        Filter {
            agent_id: Some("other".into()),
            ..Default::default()
        },
        |view| {
            assert!(rank_tables::managed::load(view, "synthetic", "1", "table1").is_err());
            Ok(())
        },
    )
    .unwrap();
    let other = Fixture::with_key([75; 32]);
    let mut otherlog = other.log();
    otherlog
        .append("seed", &[input("alpha", 1, None, true)])
        .unwrap();
    otherlog
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            view.create_derived_directory()?;
            fs::write(view.directory().join("LOCK"), b"").unwrap();
            fs::copy(
                directory.join("RANK_TABLE"),
                view.directory().join("RANK_TABLE"),
            )
            .unwrap();
            assert!(rank_tables::managed::load(view, "synthetic", "1", "table1").is_err());
            Ok(())
        })
        .unwrap();
}
#[test]
fn retained_loaded_table_never_reaches_plugin_after_append_prune_or_writer_denial() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append(
        "seed",
        &[
            input("alpha first", 1, None, true),
            input("alpha second", 2, None, true),
        ],
    )
    .unwrap();
    let (loaded, old_dir) = log
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            build(view);
            Ok((load(view), view.directory().to_path_buf()))
        })
        .unwrap();
    log.append("later", &[input("alpha third", 3, None, true)])
        .unwrap();
    let plugin = Plugin::new(Some(1.0));
    let stages = stages(&plugin, Some(&loaded));
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        let result = view.search_ranked("alpha", 10, 1 << 20, &pipeline(&stages))?;
        assert_eq!(
            result.receipt.stages[0].skipped.as_deref(),
            Some("stale_table")
        );
        assert_eq!(plugin.calls.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
    let report = log.prune("remove", &Selection::Ids(vec![9])).unwrap();
    assert!(report.all_owned_prior_generations_removed);
    assert!(!old_dir.exists());
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        let result = view.search_ranked("alpha", 10, 1 << 20, &pipeline(&stages))?;
        assert!(result.hits.iter().all(|h| h.record.id != 9));
        assert_eq!(plugin.calls.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
    log.deny_writer().unwrap();
    assert!(
        log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| view
            .search_ranked("alpha", 10, 1 << 20, &pipeline(&stages)))
            .is_err()
    );
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn managed_each_actual_rescore_callback_checks_retained_witness() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("seed", &[input("alpha", 1, None, true)])
        .unwrap();
    let mut first = Plugin::new(Some(1.0));
    first.change = Some(f.root.join("witness/CURRENT"));
    let second = Plugin::new(Some(1.0));
    let result = log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        build(view);
        let table = load(view);
        let stages = [
            Stage {
                hook: Hook::Rescore,
                ranker: &first,
                table: Some(&table),
                weight: 1.0,
                enabled: true,
            },
            Stage {
                hook: Hook::Rescore,
                ranker: &second,
                table: Some(&table),
                weight: 1.0,
                enabled: true,
            },
        ];
        view.search_ranked("alpha", 10, 1 << 20, &pipeline(&stages))
    });
    assert!(result.is_err());
    assert_eq!(first.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn managed_explicit_rebuild_and_registered_remove_handle_corruption_without_authority_write() {
    let f = Fixture::new();
    let mut log = f.log();
    log.append("seed", &[input("alpha", 1, None, true)])
        .unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        build(view);
        let version = load(view).content_version;
        fs::write(
            view.directory().join("RANK_TABLE"),
            b"corrupt disposable cache",
        )
        .unwrap();
        assert!(rank_tables::managed::load(view, "synthetic", "1", "table1").is_err());
        build(view);
        assert_ne!(load(view).content_version, version);
        fs::write(view.directory().join("unregistered"), b"foreign").unwrap();
        assert!(rank_tables::managed::remove_all(view).is_err());
        fs::remove_file(view.directory().join("unregistered")).unwrap();
        rank_tables::managed::remove_all(view)?;
        assert!(!view.directory().join("RANK_TABLE").exists());
        assert_eq!(view.records().len(), 1);
        Ok(())
    })
    .unwrap();
}
#[test]
fn managed_successful_ranker_preserves_originals_and_binding_without_claiming_watermark() {
    let f = Fixture::new();
    let mut log = f.log();
    let ack = log
        .append(
            "seed",
            &[
                input("alpha_beta", 1, None, true),
                input("alpha beta", 2, None, true),
            ],
        )
        .unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        build(view);
        let table = load(view);
        let plugin = Plugin::new(Some(2.0));
        let stages = stages(&plugin, Some(&table));
        let result = view.search_ranked("alpha_beta", 10, 1 << 20, &pipeline(&stages))?;
        assert_eq!(plugin.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.hits[0].record.id, 9);
        assert!(!result.hits[0].continued);
        assert!(result.hits[1].continued);
        assert_eq!(result.hits[0].record.recorded_at, ack.commit.recorded_at[0]);
        assert_eq!(result.hits[0].deltas, vec![2.0]);
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("binding").is_some());
        assert!(json.get("watermark").is_none());
        assert!(
            view.search_ranked("alpha", 10, 8, &Pipeline::default())
                .is_err()
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn managed_expansion_and_candidates_receive_only_current_indexed_originals_and_share_model() {
    struct Expander {
        calls: AtomicUsize,
    }
    impl rank::Ranker for Expander {
        fn identity(&self) -> rank::Identity<'_> {
            rank::Identity {
                id: "synthetic",
                version: "1",
                table_version: Some("table1"),
            }
        }
        fn work_units(&self, _: &[rank::Term<'_>], _: usize) -> usize {
            4
        }
        fn expand(
            &self,
            _: &[rank::Term<'_>],
            cx: &rank::Context<'_>,
            budget: &mut rank::WorkBudget,
        ) -> std::result::Result<Vec<rank::Expansion>, rank::Skip> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            budget.charge(1)?;
            assert_eq!(
                cx.visible.iter().map(|r| r.id).collect::<Vec<_>>(),
                vec!["9", "10"]
            );
            assert!(cx.visible.iter().all(|r| r.writer == scope().writer));
            Ok(vec![rank::Expansion {
                text: "nebula".into(),
                repetitions: NonZeroU32::new(1).unwrap(),
            }])
        }
        fn candidates(
            &self,
            _: &[rank::Term<'_>],
            cx: &rank::Context<'_>,
            _: usize,
            budget: &mut rank::WorkBudget,
        ) -> std::result::Result<Vec<String>, rank::Skip> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            budget.charge(1)?;
            assert_eq!(
                cx.visible.iter().map(|r| r.id).collect::<Vec<_>>(),
                vec!["9", "10"]
            );
            Ok(vec!["10".into()])
        }
        fn rescore(
            &self,
            _: &[rank::Term<'_>],
            _: &[rank::Candidate<'_>],
            _: &mut rank::WorkBudget,
        ) -> std::result::Result<Vec<f64>, rank::Skip> {
            panic!("not a rescore stage")
        }
    }
    let f = Fixture::new();
    let mut log = f.log();
    log.append(
        "seed",
        &[
            input("alpha source", 1, None, true),
            input("nebula candidate", 2, None, true),
            input("alpha hidden older", 3, Some("state"), true),
            input("stored-only shadow", 4, Some("state"), false),
        ],
    )
    .unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        build(view);
        let table = load(view);
        let plugin = Expander {
            calls: AtomicUsize::new(0),
        };
        let stages = [
            Stage {
                hook: Hook::Expand,
                ranker: &plugin,
                table: Some(&table),
                weight: 1.0,
                enabled: true,
            },
            Stage {
                hook: Hook::Candidates,
                ranker: &plugin,
                table: Some(&table),
                weight: 1.0,
                enabled: true,
            },
        ];
        let expanded = view.search_ranked("alpha", 10, 1 << 20, &pipeline(&stages))?;
        let equivalent = view.search_ranked("alpha nebula", 10, 1 << 20, &Pipeline::default())?;
        assert_eq!(expanded.receipt.policy, "expanded-pool-v1/f2-original-v1");
        assert_eq!(signature(&expanded.hits), signature(&equivalent.hits));
        assert_eq!(plugin.calls.load(Ordering::SeqCst), 2);
        Ok(())
    })
    .unwrap();
}

#[test]
fn managed_implicit_key_never_falls_back_to_visible_content_when_key_winner_is_filtered() {
    let f = Fixture::new();
    let mut log = f.log();
    let mut hidden = input("nonmatching key payload", 1, Some("hiddenkey"), true);
    hidden.agent_id = "hidden-agent".into();
    let rows = vec![hidden, input("hiddenkey visible content", 2, None, true)];
    let ack = log.append("seed", &rows).unwrap();
    let mut direct = RecordLog::open_clock(
        &f.root.join("direct"),
        &[74; 32],
        identity(),
        Arc::new(move || Ok(ack.commit.recorded_at[0])),
    )
    .unwrap();
    direct.append("seed", &rows).unwrap();
    let index = Index::rebuild(&direct).unwrap();
    for (cuts, filter) in [
        (
            AsOf::default(),
            Filter {
                agent_id: Some("reported-source".into()),
                ..Filter::default()
            },
        ),
        (
            AsOf {
                from: 2,
                ..AsOf::default()
            },
            Filter::default(),
        ),
    ] {
        let expected = index
            .search(
                &direct,
                &Query {
                    scope: scope(),
                    as_of: cuts,
                    text: "hiddenkey",
                    k: 10,
                    filter: filter.clone(),
                    max_bytes: 1 << 20,
                },
            )
            .unwrap();
        assert!(expected.hits.is_empty());
        log.with_rank_view(scope(), cuts, filter, |view| {
            let actual = view.search_ranked("hiddenkey", 10, 1 << 20, &Pipeline::default())?;
            assert_eq!(
                actual.hits.iter().map(|h| h.record.id).collect::<Vec<_>>(),
                expected
                    .hits
                    .iter()
                    .map(|h| h.record.id)
                    .collect::<Vec<_>>()
            );
            assert_eq!(view.records().len(), 1);
            Ok(())
        })
        .unwrap();
    }
}
