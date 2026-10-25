use super::*;
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "projection-disk-{}",
            hex(&oojuro_store::crypto::random_key())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn binding() -> Binding {
    Binding {
        namespace: [1; 32],
        writer: "generic-writer".into(),
        view: [2; 32],
    }
}

#[test]
fn positional_literals_match_full_oracle_across_unicode_filters_refresh_and_reopen() {
    let samples = [
        "battery electrode",
        "Battery electrode",
        "battery  electrode",
        "battery\telectrode",
        "x_battery electrode",
        "battery electrode_x",
        "λ_電池\r\n終",
        "λ_電池\n終",
        "___ ___",
        "battery electrode battery electrode",
        "e\u{301} exact\0終",
        "終\0exact",
        ".battery electrode.",
    ];
    let mut rows: Vec<_> = (0..300)
        .map(|i| {
            let mut value = row(i);
            value.text = samples[i % samples.len()].into();
            value
        })
        .collect();
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let path = scratch.0.join("positional");
        let limits = Limits {
            compression,
            ..Limits::default()
        };
        let mut builder = Builder::create_positional(&path, &[9; 32], binding(), limits).unwrap();
        for row in &rows {
            builder.push(row.clone()).unwrap();
        }
        let mut index = builder.finish().unwrap();
        assert_eq!(index.manifest.format, POSITIONAL_FORMAT);
        assert!(index.manifest.positions.is_some());
        for round in 0..2 {
            let bound = index.binding().clone();
            let live: Vec<_> = rows
                .iter()
                .filter(|row| round == 0 || row.id != rows[1].id)
                .cloned()
                .collect();
            let reference = super::super::Index::build(bound.clone(), live.clone()).unwrap();
            let originals: BTreeMap<_, _> =
                live.into_iter().map(|row| (row.id.clone(), row)).collect();
            for needle in samples.iter().chain(
                [
                    " battery",
                    "battery ",
                    ".",
                    "absent",
                    "battery electrode missing",
                ]
                .iter(),
            ) {
                let text = format!("\"{needle}\"");
                for filter in [
                    Filter::default(),
                    Filter {
                        agent_id: Some("agent-3".into()),
                        ..Filter::default()
                    },
                ] {
                    let q = query(&text, filter, -8, 7);
                    let ranked = index.rank_uncached(&bound, &q).unwrap();
                    assert_eq!(
                        fingerprint(
                            &index
                                .materialize(&bound, &q, &ranked, |id| Ok(originals[id].clone()))
                                .unwrap()
                        ),
                        fingerprint(&reference.search(&bound, &q).unwrap()),
                        "{text} {compression:?} round {round}"
                    );
                }
            }
            if round == 0 {
                let mut next = bound.clone();
                next.view[0] ^= 1;
                let mut append =
                    Appender::extend_validated_prefix(&index, next.clone(), limits).unwrap();
                append.retire(&rows[1].id).unwrap();
                let mut addition = row(400);
                addition.text = "battery electrode λ_電池\r\n終".into();
                append.push(addition.clone()).unwrap();
                rows.push(addition);
                let updated = append.finish().unwrap();
                drop(updated);
                drop(index);
                index = Index::open(&path, &[9; 32], &next).unwrap();
            }
        }
        rows.pop();
    }
}

#[test]
fn positional_literal_counts_and_block_bounds_preserve_exact_dense_scores() {
    let rows: Vec<_> = (0..5001)
        .map(|i| {
            let mut row = row(i);
            row.text = format!(
                "the battery electrode original {} λ_電池\r\n終 {}",
                "battery electrode ".repeat(i % 5),
                if i.is_multiple_of(3) {
                    "Battery  electrode"
                } else {
                    "battery  electrode"
                }
            );
            row
        })
        .collect();
    let reference = super::super::Index::build(binding(), rows.clone()).unwrap();
    let originals: BTreeMap<_, _> = rows
        .iter()
        .map(|row| (row.id.clone(), row.clone()))
        .collect();
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let mut builder = Builder::create_positional(
            scratch.0.join("index"),
            &[9; 32],
            binding(),
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        for row in &rows {
            builder.push(row.clone()).unwrap();
        }
        let index = builder.finish().unwrap();
        for text in [
            "\"battery electrode\"",
            "\"battery  electrode\"",
            "\"Battery  electrode\"",
            "\"battery electrode original\"",
            "\"the battery\"",
            "\"λ_電池\r\n終\"",
        ] {
            for (from, to) in [(i64::MIN, i64::MAX), (-8, 7)] {
                let q = query(text, Filter::default(), from, to);
                let ranked = index.rank_uncached(&binding(), &q).unwrap();
                assert_eq!(
                    fingerprint(
                        &index
                            .materialize(&binding(), &q, &ranked, |id| Ok(originals[id].clone()))
                            .unwrap()
                    ),
                    fingerprint(&reference.search(&binding(), &q).unwrap()),
                    "{text} {compression:?}"
                );
            }
        }
        let q = query(
            "\"battery electrode\"",
            Filter::default(),
            i64::MIN,
            i64::MAX,
        );
        let plan = scorer::Plan::new(q.text, false, None, &BTreeSet::new(), &BTreeMap::new());
        let mut proof = index
            .literal_candidates("battery electrode")
            .unwrap()
            .unwrap();
        let model = index
            .literal_statistics(&plan, None, &mut proof)
            .unwrap()
            .unwrap()
            .finish(plan);
        assert!(
            index
                .rank_compact_blocks(&model, &Selection::new(&q), q.k, Some(&proof))
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn positional_proof_ranks_without_cached_originals_and_fallback_still_authenticates_text() {
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let path = scratch.0.join("index");
        let mut original = row(1);
        original.text = "battery electrode ".into();
        let mut builder = Builder::create_positional(
            &path,
            &[9; 32],
            binding(),
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        builder.push(original.clone()).unwrap();
        let index = builder.finish().unwrap();
        let metadata = index.metadata(0).unwrap();
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                index
                    .directory
                    .join(&index.manifest.files[metadata.text.file as usize]),
            )
            .unwrap();
        file.seek(SeekFrom::Start(metadata.text.offset)).unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 1;
        file.seek(SeekFrom::Start(metadata.text.offset)).unwrap();
        file.write_all(&byte).unwrap();
        file.sync_all().unwrap();
        let q = query(
            "\"battery electrode\"",
            Filter::default(),
            i64::MIN,
            i64::MAX,
        );
        let ranked = index.rank_uncached(&binding(), &q).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(
            index
                .materialize(&binding(), &q, &ranked, |_| Ok(original.clone()))
                .unwrap()
                .len(),
            1
        );
        let fallback = query(
            "\"battery electrode \"",
            Filter::default(),
            i64::MIN,
            i64::MAX,
        );
        assert!(matches!(
            index.rank_uncached(&binding(), &fallback),
            Err(Error::Authority)
        ));
        let tight = Index::open_with_limits(
            &path,
            &[9; 32],
            &binding(),
            Limits {
                compression,
                original_bytes_per_query: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let mut called = false;
        assert!(matches!(
            tight.search(&binding(), &q, |_| {
                called = true;
                Ok(original.clone())
            }),
            Err(Error::Bounds)
        ));
        assert!(!called);
    }
}

#[test]
fn generic_events_preserve_cursor_timestamp_and_exact_data_contract() {
    fn cursor(i: u64) -> Vec<u8> {
        let mut value = vec![0x83; 16];
        value.extend(i.to_be_bytes());
        value.extend([0xe1; 16]);
        value
    }
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let path = scratch.0.join("events");
        let originals: BTreeMap<_, _> = [
            (
                cursor(1),
                EventRow {
                    timestamp: 2_000_000,
                    data: b"alpha exact\r\nbytes".to_vec(),
                },
            ),
            (
                cursor(2),
                EventRow {
                    timestamp: -1,
                    data: b"alpha exact\r\nbytes".to_vec(),
                },
            ),
            (
                cursor(3),
                EventRow {
                    timestamp: 0,
                    data: vec![0xff, 0, 0x83],
                },
            ),
        ]
        .into();
        let mut builder = EventBuilder::create(
            &path,
            &[9; 32],
            binding(),
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        for (cursor, row) in &originals {
            assert_eq!(
                builder.push(cursor, row.timestamp, &row.data).unwrap(),
                std::str::from_utf8(&row.data).is_ok()
            );
        }
        let index = builder.finish().unwrap();
        assert_eq!(index.row_count(), 2);
        let mut q = EventQuery::new("alpha");
        let hits = index
            .search(&binding(), &q, |cursor| Ok(originals[cursor].clone()))
            .unwrap();
        assert_eq!(hits.len(), 2);
        // Search ties use the stored acceptance time, never a timestamp decoded
        // from the cursor or assumed monotonic from append order.
        assert_eq!(hits[0].cursor, cursor(1));
        assert_eq!(hits[1].cursor, cursor(2));
        assert_eq!(hits[0].timestamp, 2_000_000);
        assert_eq!(hits[1].timestamp, -1);
        assert_eq!(hits[0].excerpt.as_bytes(), originals[&cursor(1)].data);
        assert_eq!(originals[&cursor(3)].data, [0xff, 0, 0x83]);
        q.from = -2;
        q.to = Some(0);
        assert_eq!(
            index
                .search(&binding(), &q, |cursor| Ok(originals[cursor].clone()))
                .unwrap()[0]
                .cursor,
            cursor(2)
        );
        for field in 0..2 {
            assert!(matches!(
                index.search(&binding(), &q, |cursor| {
                    let mut row = originals[cursor].clone();
                    if field == 0 {
                        row.timestamp += 1
                    } else {
                        row.data.push(0)
                    }
                    Ok(row)
                }),
                Err(Error::Authority)
            ));
        }
        q.to = Some(q.from);
        assert!(matches!(index.rank(&binding(), &q), Err(Error::Invalid)));
        q.to = Some(i64::MIN);
        assert!(matches!(index.rank(&binding(), &q), Err(Error::Invalid)));
        q = EventQuery::new("alpha");
        q.max_bytes = 300;
        assert!(matches!(
            index.search(&binding(), &q, |cursor| Ok(originals[cursor].clone())),
            Err(Error::Bounds)
        ));
        drop(index);
        let reopened = EventIndex::open(&path, &[9; 32], &binding()).unwrap();
        let hits = reopened
            .search(&binding(), &EventQuery::new("alpha"), |cursor| {
                Ok(originals[cursor].clone())
            })
            .unwrap();
        assert_eq!(hits[0].cursor, cursor(1));
        fn concurrent<T: Send + Sync>() {}
        concurrent::<EventIndex>();
    }
}

#[test]
fn compact_numeric_mass_is_admitted_before_publication_and_open_allocation() {
    let scratch = Scratch::new();
    let path = scratch.0.join("index");
    let previous = build(&path, &[row(1)], Compression::Raw);
    let descriptor = fs::read(path.join("PROJECTION_CURRENT")).unwrap();
    let mut builder = Builder::create_compact(
        &path,
        &[9; 32],
        binding(),
        Limits {
            metadata_bytes: std::mem::size_of::<Column>(),
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(builder.push(row(1)), Err(Error::Bounds)));
    drop(builder);
    assert_eq!(
        fs::read(path.join("PROJECTION_CURRENT")).unwrap(),
        descriptor
    );
    assert_eq!(previous.row_count(), 1);
    let compact_path = scratch.0.join("compact");
    let rows: Vec<_> = (0..500).map(row).collect();
    let index = build_layout(&compact_path, &rows, Compression::Raw, true);
    let columns = index
        .columns
        .iter()
        .map(|block| std::mem::size_of_val(block.as_ref()) + 2 * std::mem::size_of::<usize>())
        .sum::<usize>()
        + index.columns.capacity() * std::mem::size_of::<Arc<[Column]>>();
    let admitted = columns + index.mass.as_ref().unwrap().bytes();
    assert!(
        Index::open_with_limits(
            &compact_path,
            &[9; 32],
            &binding(),
            Limits {
                metadata_bytes: admitted,
                ..Limits::default()
            }
        )
        .is_ok()
    );
    assert!(matches!(
        Index::open_with_limits(
            &compact_path,
            &[9; 32],
            &binding(),
            Limits {
                metadata_bytes: admitted - 1,
                ..Limits::default()
            }
        ),
        Err(Error::Bounds)
    ));
}

#[test]
fn original_read_plan_checks_every_identity_and_budget_before_provider_prefetch() {
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let rows = vec![row(1), row(2)];
        let originals: BTreeMap<_, _> = rows
            .iter()
            .map(|row| (row.id.clone(), row.clone()))
            .collect();
        let path = scratch.0.join("index");
        let index = build(&path, &rows, compression);
        let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
        let ranked = index.rank(&binding(), &q).unwrap();
        assert_eq!(ranked.len(), 2);
        let expected: Vec<_> = ranked
            .iter()
            .map(|hit| (hit.id.clone(), originals[&hit.id].text.len()))
            .collect();
        assert_eq!(
            index.originals_plan(&binding(), &q, &ranked).unwrap(),
            expected
        );
        let mut foreign = binding();
        foreign.view[0] ^= 1;
        assert!(matches!(
            index.originals_plan(&foreign, &q, &ranked),
            Err(Error::Scope)
        ));
        let mut one = query("alpha", Filter::default(), i64::MIN, i64::MAX);
        one.k = 1;
        assert!(matches!(
            index.originals_plan(&binding(), &one, &ranked),
            Err(Error::Bounds)
        ));
        for change in 0..3 {
            let mut wrong = ranked.clone();
            match change {
                0 => wrong[0].id.push('x'),
                1 => wrong[0].ordinal = u32::MAX,
                _ => wrong[0].bits = f64::NAN,
            }
            assert!(matches!(
                index.originals_plan(&binding(), &q, &wrong),
                Err(Error::Authority | Error::Invalid)
            ));
        }
        let total: usize = expected.iter().map(|(_, bytes)| *bytes).sum();
        let restricted = Index::open_with_limits(
            &path,
            &[9; 32],
            &binding(),
            Limits {
                original_bytes_per_query: total - 1,
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        let mut provider_calls = 0;
        for _ in 0..2 {
            // A reused rank result cannot bypass the stricter handle policy.
            let reused = restricted.rank(&binding(), &q).unwrap();
            let prefetch = (|| -> Result<()> {
                for (id, _) in restricted.originals_plan(&binding(), &q, &reused)? {
                    provider_calls += 1;
                    let _ = &originals[&id];
                }
                Ok(())
            })();
            assert!(matches!(prefetch, Err(Error::Bounds)));
        }
        assert_eq!(provider_calls, 0);
        // Existing materialize remains incremental: its first valid callback
        // occurs before a later row exceeds the cumulative budget.
        let result = restricted.materialize(&binding(), &q, &ranked, |id| {
            provider_calls += 1;
            Ok(originals[id].clone())
        });
        assert!(matches!(result, Err(Error::Bounds)));
        assert_eq!(provider_calls, 1);
        let mut next = binding();
        next.view[1] ^= 1;
        let mut append = Appender::extend_validated_prefix(
            &index,
            next.clone(),
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        append.retire(&ranked[0].id).unwrap();
        let retired = append.finish().unwrap();
        assert!(matches!(
            retired.originals_plan(&next, &q, &ranked),
            Err(Error::Authority)
        ));
        assert!(matches!(
            retired.originals_plan(&binding(), &q, &ranked),
            Err(Error::Scope)
        ));
    }
}

#[test]
fn construction_quota_covers_temporary_files_before_publication() {
    let scratch = Scratch::new();
    let path = scratch.0.join("index");
    let rows = (0..1600).map(row).collect::<Vec<_>>();
    let index = build(&path, &rows, Compression::Raw);
    let metrics = index.build_metrics();
    assert!(metrics.peak_logical_bytes > index.index_bytes());
    assert!(metrics.peak_allocated_bytes_observed > 0);
    assert!(metrics.peak_files >= 5);
    let descriptor = fs::read(path.join("PROJECTION_CURRENT")).unwrap();
    let limits = Limits {
        index_bytes: metrics.peak_logical_bytes / 2,
        ..Limits::default()
    };
    let mut builder = Builder::create(&path, &[9; 32], binding(), limits).unwrap();
    let mut rejected = false;
    for row in rows {
        if matches!(builder.push(row), Err(Error::Bounds)) {
            rejected = true;
            break;
        }
    }
    if !rejected {
        assert!(matches!(builder.finish(), Err(Error::Bounds)));
    } else {
        drop(builder);
    }
    assert_eq!(
        fs::read(path.join("PROJECTION_CURRENT")).unwrap(),
        descriptor
    );
    assert!(!fs::read_dir(&path).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".build-")
    }));
    assert!(BuildMetrics::reservation(u64::MAX, 1, 4096, 4096).is_none());
}

#[test]
fn source_work_budget_refuses_before_fetching_an_oversized_original() {
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let mut original = row(1);
        original.text.push_str(&" long original".repeat(64));
        let mut builder = Builder::create(
            scratch.0.join("index"),
            &[9; 32],
            binding(),
            Limits {
                original_bytes_per_query: 64,
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        builder.push(original).unwrap();
        let index = builder.finish().unwrap();
        let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
        let mut calls = 0;
        assert!(matches!(
            index.search(&binding(), &q, |_| {
                calls += 1;
                Err(Error::Unavailable)
            }),
            Err(Error::Bounds)
        ));
        assert_eq!(calls, 0);
    }
}

#[test]
fn stricter_open_work_caps_apply_to_fresh_and_reused_queries() {
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let path = scratch.0.join("index");
        let mut original = row(1);
        original.text.push_str(&" long original".repeat(64));
        let previous = build(&path, &[original.clone()], compression);
        let both = query("alpha beta", Filter::default(), i64::MIN, i64::MAX);
        assert_eq!(
            previous
                .search(&binding(), &both, |_| Ok(original.clone()))
                .unwrap()
                .len(),
            1
        );
        let index = Index::open_with_limits(
            &path,
            &[9; 32],
            &binding(),
            Limits {
                original_bytes_per_query: 64,
                query_terms: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let mut calls = 0;
        assert!(matches!(
            index.search(&binding(), &both, |_| {
                calls += 1;
                Ok(original.clone())
            }),
            Err(Error::Bounds)
        ));
        let one = query("alpha", Filter::default(), i64::MIN, i64::MAX);
        assert_eq!(index.rank(&binding(), &one).unwrap().len(), 1);
        for _ in 0..2 {
            assert!(matches!(
                index.search(&binding(), &one, |_| {
                    calls += 1;
                    Ok(original.clone())
                }),
                Err(Error::Bounds)
            ));
        }
        assert_eq!(calls, 0);
    }
}

#[test]
fn heterogeneous_term_shapes_and_large_k_fall_back_to_exact_complete_scoring() {
    let mut rows = (0..1100).map(row).collect::<Vec<_>>();
    for (i, row) in rows.iter_mut().enumerate() {
        row.text = "alpha ".repeat(i + 1);
        row.indexed = true;
    }
    let originals: BTreeMap<_, _> = rows.iter().map(|r| (r.id.clone(), r.clone())).collect();
    let reference = super::super::Index::build(binding(), rows.clone()).unwrap();
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let index = build(&scratch.0.join("index"), &rows, compression);
        let mut q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
        q.k = 73;
        q.max_bytes = 1 << 20;
        let ranked = index.rank_uncached(&binding(), &q).unwrap();
        let hits = index
            .materialize(&binding(), &q, &ranked, |id| Ok(originals[id].clone()))
            .unwrap();
        assert_eq!(
            fingerprint(&hits),
            fingerprint(&reference.search(&binding(), &q).unwrap())
        );
        let list = index
            .terms
            .get(&index, *blake3::hash(b"alpha").as_bytes())
            .unwrap()
            .unwrap();
        assert!(list.groups.is_none());
    }
}

#[test]
fn ambiguous_descriptor_publication_keeps_its_complete_generation_readable() {
    let scratch = Scratch::new();
    let path = scratch.0.join("index");
    let first = row(1);
    let second = row(2);
    let mut builder = Builder::create(&path, &[9; 32], binding(), Limits::default()).unwrap();
    builder.push(first.clone()).unwrap();
    storage::FAIL_AFTER_MANIFEST_RENAME.with(|fail| fail.set(true));
    assert!(matches!(builder.finish(), Err(Error::Unavailable)));
    let old = Index::open(&path, &[9; 32], &binding()).unwrap();
    assert_eq!(old.row_count(), 1);
    let mut next = binding();
    next.view[0] ^= 1;
    let mut append =
        Appender::extend_validated_prefix(&old, next.clone(), Limits::default()).unwrap();
    append.push(second.clone()).unwrap();
    storage::FAIL_AFTER_MANIFEST_RENAME.with(|fail| fail.set(true));
    assert!(matches!(append.finish(), Err(Error::Unavailable)));
    let index = Index::open(&path, &[9; 32], &next).unwrap();
    assert_eq!(index.row_count(), 2);
    let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
    assert_eq!(
        index
            .search(&next, &q, |id| Ok(if id == first.id {
                first.clone()
            } else {
                second.clone()
            }))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn incremental_prefix_append_retire_and_reopen_match_complete_global_model() {
    for (compression, compact) in [
        (Compression::Raw, false),
        (Compression::Zstd, false),
        (Compression::Raw, true),
        (Compression::Zstd, true),
    ] {
        let scratch = Scratch::new();
        let path = scratch.0.join("index");
        let initial = (0..2040).step_by(2).rev().map(row).collect::<Vec<_>>();
        let old = if compact {
            let mut b = Builder::create_compact(
                &path,
                &[9; 32],
                binding(),
                Limits {
                    compression,
                    ..Limits::default()
                },
            )
            .unwrap();
            for row in &initial {
                b.push(row.clone()).unwrap();
            }
            let index = b.finish().unwrap();
            assert_eq!(index.manifest.format, COMPACT_FORMAT);
            let sealed = fs::read(path.join("PROJECTION_CURRENT")).unwrap();
            let old_authority = Authority::new(&binding()).unwrap();
            assert!(
                index
                    .cipher
                    .open_frame(
                        &{
                            let mut aad = b"oojuro/projection/manifest/v1\0".to_vec();
                            aad.extend(&old_authority.binding);
                            aad
                        },
                        &sealed
                    )
                    .is_err()
            );
            index
        } else {
            build(&path, &initial, compression)
        };
        old.rank(
            &binding(),
            &query("alpha", Filter::default(), i64::MIN, i64::MAX),
        )
        .unwrap();
        let old_columns = old.columns.clone();
        let mut next = binding();
        next.view[0] ^= 1;
        let mut append = Appender::extend_validated_prefix(
            &old,
            next.clone(),
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        append.retire(&row(6).id).unwrap();
        let additions = (1..120).step_by(2).map(row).collect::<Vec<_>>();
        for row in &additions {
            append.push(row.clone()).unwrap();
        }
        let index = append.finish().unwrap();
        assert_eq!(index.row_count(), 1079);
        assert_eq!(index.indexed_row_count(), 1080);
        assert!(
            old_columns
                .iter()
                .zip(&old.columns)
                .all(|(before, after)| Arc::ptr_eq(before, after))
        );
        let rows = initial
            .iter()
            .filter(|r| r.id != row(6).id)
            .cloned()
            .chain(additions)
            .collect::<Vec<_>>();
        let originals: BTreeMap<_, _> = rows.iter().map(|r| (r.id.clone(), r.clone())).collect();
        let reference = super::super::Index::build(next.clone(), rows).unwrap();
        for text in [
            "alpha",
            "create_config",
            "missing_name",
            "\"exact id\"",
            "key:k-00000006",
            "k-00000006",
        ] {
            for filter in [
                Filter::default(),
                Filter {
                    agent_id: Some("agent-3".into()),
                    ..Filter::default()
                },
            ] {
                let q = query(text, filter, -9, 9);
                assert_eq!(
                    fingerprint(
                        &index
                            .search(&next, &q, |id| Ok(originals[id].clone()))
                            .unwrap()
                    ),
                    fingerprint(&reference.search(&next, &q).unwrap()),
                    "{text} {compression:?}"
                );
                assert!(matches!(index.rank(&binding(), &q), Err(Error::Scope)));
            }
        }
        drop(index);
        let reopened = Index::open(&path, &[9; 32], &next).unwrap();
        let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
        assert_eq!(
            fingerprint(
                &reopened
                    .search(&next, &q, |id| Ok(originals[id].clone()))
                    .unwrap()
            ),
            fingerprint(&reference.search(&next, &q).unwrap())
        );
        assert_eq!(old.row_count(), 1020);
        assert_eq!(
            old.columns
                .iter()
                .map(|b| b.iter().filter(|c| c.live()).count())
                .sum::<usize>(),
            1020
        );
        let current = fs::read(path.join("PROJECTION_CURRENT")).unwrap();
        let mut another = next.clone();
        another.view[1] ^= 1;
        let mut duplicate = Appender::extend_validated_prefix(
            &reopened,
            another,
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        duplicate.push(row(2)).unwrap();
        assert!(matches!(duplicate.finish(), Err(Error::Conflict)));
        assert_eq!(fs::read(path.join("PROJECTION_CURRENT")).unwrap(), current);
    }
}

#[test]
fn compact_block_bounds_match_complete_rank_bits_ties_filters_and_retirement() {
    let mut rows: Vec<_> = (0..20_001)
        .map(|i| {
            let mut row = row(i);
            row.text = "alpha beta gamma".into();
            row.indexed = true;
            row
        })
        .collect();
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let path = scratch.0.join("compact");
        let index = build_layout(&path, &rows, compression, true);
        let q = query("alpha beta", Filter::default(), i64::MIN, i64::MAX);
        let plan = scorer::Plan::new(q.text, false, None, &BTreeSet::new(), &BTreeMap::new());
        let model = index
            .compact_statistics(&plan, None)
            .unwrap()
            .unwrap()
            .finish(plan);
        // More rows than the bounded exhaustive trial: Some proves that this
        // cohort actually prunes blocks and can complete under that work cap.
        assert!(
            index
                .rank_compact_blocks(&model, &Selection::new(&q), q.k, None)
                .unwrap()
                .is_some()
        );
        let mut next = binding();
        next.view[0] ^= 1;
        let mut append = Appender::extend_validated_prefix(
            &index,
            next.clone(),
            Limits {
                compression,
                ..Limits::default()
            },
        )
        .unwrap();
        let retired = rows[58].id.clone();
        append.retire(&retired).unwrap();
        let index = append.finish().unwrap();
        let live: Vec<_> = rows
            .iter()
            .filter(|row| row.id != retired)
            .cloned()
            .collect();
        let originals: BTreeMap<_, _> = live
            .iter()
            .map(|row| (row.id.clone(), row.clone()))
            .collect();
        let reference = super::super::Index::build(next.clone(), live).unwrap();
        for text in [
            "alpha beta",
            "alpha beta gamma",
            "alpha alpha beta gamma",
            "alpha absent",
            "beta",
        ] {
            for filter in [
                Filter::default(),
                Filter {
                    agent_id: Some("agent-3".into()),
                    ..Filter::default()
                },
            ] {
                for (from, to) in [(i64::MIN, i64::MAX), (-5, 7)] {
                    let q = query(text, filter.clone(), from, to);
                    let got = index.rank_uncached(&next, &q).unwrap();
                    assert_eq!(
                        fingerprint(
                            &index
                                .materialize(&next, &q, &got, |id| Ok(originals[id].clone()))
                                .unwrap()
                        ),
                        fingerprint(&reference.search(&next, &q).unwrap()),
                        "{text} {compression:?} {from} {to}"
                    );
                }
            }
        }
    }
    // Deliberately heterogeneous TF/shape correlations exercise a loose bound
    // and the exact bounded-work fallback instead of independent minima.
    for (i, row) in rows.iter_mut().enumerate() {
        row.text = format!("{} beta", "alpha ".repeat(1 + i % 37));
    }
    let scratch = Scratch::new();
    let index = build_layout(
        &scratch.0.join("heterogeneous"),
        &rows,
        Compression::Raw,
        true,
    );
    let reference = super::super::Index::build(binding(), rows.clone()).unwrap();
    let originals: BTreeMap<_, _> = rows
        .iter()
        .map(|row| (row.id.clone(), row.clone()))
        .collect();
    let q = query("alpha beta", Filter::default(), i64::MIN, i64::MAX);
    let ranked = index.rank_uncached(&binding(), &q).unwrap();
    assert_eq!(
        fingerprint(
            &index
                .materialize(&binding(), &q, &ranked, |id| Ok(originals[id].clone()))
                .unwrap()
        ),
        fingerprint(&reference.search(&binding(), &q).unwrap())
    );
}

#[test]
fn sparse_identifier_driver_keeps_exact_floor_and_filtered_view_statistics() {
    let mut rows = (0..1300).map(row).collect::<Vec<_>>();
    for (i, row) in rows.iter_mut().enumerate() {
        if i.is_multiple_of(97) {
            row.text.push_str(" needle_exact");
        }
    }
    let originals: BTreeMap<_, _> = rows
        .iter()
        .map(|row| (row.id.clone(), row.clone()))
        .collect();
    let reference = super::super::Index::build(binding(), rows.clone()).unwrap();
    for compression in [Compression::Raw, Compression::Zstd] {
        let scratch = Scratch::new();
        let index = build(&scratch.0.join("index"), &rows, compression);
        for text in [
            "needle_exact alpha",
            "needle_exact missing",
            "missing_identifier alpha",
            "needle_exact create_config",
        ] {
            for filter in [
                Filter::default(),
                Filter {
                    agent_id: Some("agent-3".into()),
                    ..Filter::default()
                },
            ] {
                let q = query(text, filter, -8, 7);
                assert_eq!(
                    fingerprint(
                        &index
                            .search(&binding(), &q, |id| Ok(originals[id].clone()))
                            .unwrap()
                    ),
                    fingerprint(&reference.search(&binding(), &q).unwrap()),
                    "{text} {compression:?}"
                );
            }
        }
    }
}
fn row(i: usize) -> Row {
    Row {
        id: format!("id/{i:08x}"),
        key: (i.is_multiple_of(3)).then(|| format!("k-{i:08}")),
        text: format!(
            "alpha beta create_config exact id {i} café\r\n ++ alpha {}",
            if i.is_multiple_of(4) {
                "rare literal"
            } else {
                "ordinary result"
            }
        ),
        valid_at: (i % 33) as i64 - 16,
        reported_recorded_at: 1000 + i as i64,
        kind: if i.is_multiple_of(2) {
            Kind::ToolResult
        } else {
            Kind::Output
        },
        tool: (i.is_multiple_of(3)).then(|| "compiler".into()),
        status: Some(if i.is_multiple_of(2) {
            Status::Ok
        } else {
            Status::Error
        }),
        agent_id: format!("agent-{}", i % 5),
        indexed: !i.is_multiple_of(7),
    }
}
fn query<'a>(text: &'a str, filter: Filter, from: i64, to: i64) -> Query<'a> {
    Query {
        text,
        k: 5,
        from,
        to,
        filter,
        excerpt_bytes: 64,
        max_bytes: 16384,
    }
}
fn build_layout(path: &Path, rows: &[Row], compression: Compression, compact: bool) -> Index {
    if !compact {
        return build(path, rows, compression);
    }
    let mut builder = Builder::create_compact(
        path,
        &[9; 32],
        binding(),
        Limits {
            compression,
            ..Limits::default()
        },
    )
    .unwrap();
    for row in rows {
        builder.push(row.clone()).unwrap();
    }
    builder.finish().unwrap()
}
fn build(path: &Path, rows: &[Row], compression: Compression) -> Index {
    let mut b = Builder::create(
        path,
        &[9; 32],
        binding(),
        Limits {
            compression,
            ..Limits::default()
        },
    )
    .unwrap();
    for row in rows {
        b.push(row.clone()).unwrap();
    }
    b.finish().unwrap()
}
type HitFingerprint = (String, u64, bool, i64, i64, usize, usize, String);
fn fingerprint(hits: &[Hit]) -> Vec<HitFingerprint> {
    hits.iter()
        .map(|h| {
            (
                h.id.clone(),
                h.bits.to_bits(),
                h.continued,
                h.valid_at,
                h.reported_recorded_at,
                h.start,
                h.end,
                h.excerpt.clone(),
            )
        })
        .collect()
}

#[test]
fn disk_projection_matches_ram_bits_filters_excerpts_and_reopen() {
    let rows = (0..1200).rev().map(row).collect::<Vec<_>>();
    let originals: BTreeMap<_, _> = rows.iter().map(|r| (r.id.clone(), r.clone())).collect();
    let reference = super::super::Index::build(binding(), rows.clone()).unwrap();
    for (compression, compact) in [
        (Compression::Raw, false),
        (Compression::Zstd, false),
        (Compression::Raw, true),
        (Compression::Zstd, true),
    ] {
        let scratch = Scratch::new();
        let path = scratch.0.join("index");
        let index = build_layout(&path, &rows, compression, compact);
        assert_eq!(index.row_count(), 1200);
        assert_eq!(
            index.text_bytes(),
            rows.iter().map(|r| r.text.len() as u64).sum::<u64>()
        );
        assert!(index.index_bytes() > 0);
        assert!(index.estimated_heap_bytes() < 32 << 20);
        for text in [
            "alpha",
            "create_config",
            "missing_name",
            "\"exact id\"",
            "\"++\"",
            "key:k-00000003",
            "k-00000003",
            "café",
            "alpha alpha",
            "alpha beta",
            "alpha ordinary rare",
            "\"create_config\"",
        ] {
            for filter in [
                Filter::default(),
                Filter {
                    kind: Some(Kind::ToolResult),
                    ..Filter::default()
                },
                Filter {
                    tool: Some("compiler".into()),
                    ..Filter::default()
                },
                Filter {
                    status: Some(Status::Error),
                    ..Filter::default()
                },
                Filter {
                    agent_id: Some("agent-3".into()),
                    ..Filter::default()
                },
            ] {
                for (from, to) in [(i64::MIN, i64::MAX), (-5, 7)] {
                    let q = query(text, filter.clone(), from, to);
                    let expected = reference.search(&binding(), &q).unwrap();
                    let found = index
                        .search(&binding(), &q, |id| {
                            originals.get(id).cloned().ok_or(Error::Authority)
                        })
                        .unwrap();
                    assert_eq!(
                        fingerprint(&found),
                        fingerprint(&expected),
                        "{text}, {compression:?}"
                    );
                }
            }
        }
        drop(index);
        let reopened = Index::open(&path, &[9; 32], &binding()).unwrap();
        let q = query("create_config", Filter::default(), i64::MIN, i64::MAX);
        assert_eq!(
            fingerprint(
                &reopened
                    .search(&binding(), &q, |id| Ok(originals[id].clone()))
                    .unwrap()
            ),
            fingerprint(&reference.search(&binding(), &q).unwrap())
        );
    }
}

#[test]
fn duplicate_raw_keys_are_not_collapsed_across_resolved_rows() {
    let scratch = Scratch::new();
    let mut a = row(1);
    a.key = Some("shared".into());
    let mut b = row(2);
    b.key = a.key.clone();
    let rows = vec![a.clone(), b.clone()];
    let index = build(&scratch.0.join("index"), &rows, Compression::Raw);
    let q = query("key:shared", Filter::default(), i64::MIN, i64::MAX);
    let hits = index
        .search(&binding(), &q, |id| {
            Ok(if id == a.id { a.clone() } else { b.clone() })
        })
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, b.id);
    assert_eq!(hits[1].id, a.id);
    assert!(hits.iter().all(|h| h.bits.to_bits() == 0));
}

#[test]
fn callback_originals_and_snapshot_binding_are_independently_checked() {
    let scratch = Scratch::new();
    let rows = vec![row(1), row(2)];
    let index = build(&scratch.0.join("index"), &rows, Compression::Zstd);
    let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
    let mut wrong = binding();
    wrong.view[0] ^= 1;
    assert!(matches!(index.rank(&wrong, &q), Err(Error::Scope)));
    for field in 0..10 {
        let result = index.search(&binding(), &q, |id| {
            let mut row = rows.iter().find(|r| r.id == id).unwrap().clone();
            match field {
                0 => row.id.push('x'),
                1 => row.text.push('x'),
                2 => row.valid_at += 1,
                3 => row.reported_recorded_at += 1,
                4 => row.key = Some("changed".into()),
                5 => row.kind = Kind::Raw,
                6 => row.tool = Some("other".into()),
                7 => row.status = None,
                8 => row.agent_id.push('x'),
                _ => row.indexed = false,
            }
            Ok(row)
        });
        assert!(matches!(result, Err(Error::Authority)), "field{field}");
    }
    assert!(Index::open(scratch.0.join("index"), &[8; 32], &binding()).is_err());
}

#[test]
fn empty_duplicate_and_poisoned_builds_do_not_publish_partial_indexes() {
    let scratch = Scratch::new();
    let path = scratch.0.join("index");
    let empty = build(&path, &[], Compression::Raw);
    assert_eq!(empty.row_count(), 0);
    assert!(
        empty
            .rank(
                &binding(),
                &query("alpha", Filter::default(), i64::MIN, i64::MAX)
            )
            .unwrap()
            .is_empty()
    );
    drop(empty);
    let mut b = Builder::create(&path, &[9; 32], binding(), Limits::default()).unwrap();
    b.push(row(1)).unwrap();
    b.push(row(1)).unwrap();
    assert!(b.finish().is_err());
    assert_eq!(
        Index::open(&path, &[9; 32], &binding())
            .unwrap()
            .row_count(),
        0
    );
    let path = scratch.0.join("limited");
    let mut b = Builder::create(
        &path,
        &[9; 32],
        binding(),
        Limits {
            rows: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    b.push(row(1)).unwrap();
    assert!(b.push(row(2)).is_err());
    assert!(b.finish().is_err());
    assert!(!path.join("PROJECTION_CURRENT").exists());
}

#[test]
fn encrypted_files_and_corrupt_metadata_fail_closed() {
    let scratch = Scratch::new();
    let rows = vec![row(1), row(2)];
    let index = build(&scratch.0.join("index"), &rows, Compression::Zstd);
    for entry in fs::read_dir(&index.directory).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(b"create_config".len())
                .any(|v| v == b"create_config")
        );
    }
    let r = index.manifest.metadata.node.as_ref().unwrap();
    let path = index
        .directory
        .join(format!("pack-{}-{:016x}.bin", hex(&r.generation), r.file));
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::Start(r.offset + u64::from(r.bytes) - 1))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 1;
    file.seek(SeekFrom::Start(r.offset + u64::from(r.bytes) - 1))
        .unwrap();
    file.write_all(&byte).unwrap();
    let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
    assert!(
        index
            .search(&binding(), &q, |_| panic!("corrupt metadata exposed"))
            .is_err()
    );
}

#[test]
fn ten_concurrent_queries_share_one_immutable_index() {
    let scratch = Scratch::new();
    let rows = (0..1024).map(row).collect::<Vec<_>>();
    let index = Arc::new(build(&scratch.0.join("index"), &rows, Compression::Zstd));
    let source = Arc::new(
        rows.into_iter()
            .map(|r| (r.id.clone(), r))
            .collect::<BTreeMap<_, _>>(),
    );
    let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
    let expected = Arc::new(fingerprint(
        &index
            .search(&binding(), &q, |id| Ok(source[id].clone()))
            .unwrap(),
    ));
    let threads = (0..10)
        .map(|_| {
            let index = index.clone();
            let source = source.clone();
            let expected = expected.clone();
            std::thread::spawn(move || {
                for _ in 0..4 {
                    let q = query("alpha", Filter::default(), i64::MIN, i64::MAX);
                    assert_eq!(
                        fingerprint(
                            &index
                                .search(&binding(), &q, |id| Ok(source[id].clone()))
                                .unwrap()
                        ),
                        *expected
                    );
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Index>();
}

#[test]
fn positional_codec_requirement_refuses_unknown_and_undeclared_codecs_before_original_callbacks() {
    let scratch = Scratch::new();
    let path = scratch.0.join("index");
    let mut original = row(1);
    original.text = "battery electrode".into();
    let mut builder =
        Builder::create_positional(&path, &[9; 32], binding(), Limits::default()).unwrap();
    builder.push(original).unwrap();
    let index = builder.finish().unwrap();
    assert_eq!(index.manifest.position_codec, Some(2));
    for codec in [Some(1), Some(3)] {
        let mut manifest = index.manifest.clone();
        manifest.position_codec = codec;
        publish(&path, &index.cipher, &index.authority, &mut manifest).unwrap();
        assert!(matches!(
            Index::open(&path, &[9; 32], &binding()),
            Err(Error::Bounds)
        ));
    }
    let mut undeclared = index.manifest.clone();
    undeclared.position_codec = None;
    publish(&path, &index.cipher, &index.authority, &mut undeclared).unwrap();
    let undeclared = Index::open(&path, &[9; 32], &binding()).unwrap();
    let q = query(
        "\"battery electrode\"",
        Filter::default(),
        i64::MIN,
        i64::MAX,
    );
    let mut called = false;
    assert!(matches!(
        undeclared.search(&binding(), &q, |_| {
            called = true;
            unreachable!()
        }),
        Err(Error::Authority)
    ));
    assert!(!called);
}

#[test]
fn positional_legacy_codec_generation_reopens_and_extends_with_mixed_codec_pages() {
    let scratch = Scratch::new();
    let path = scratch.0.join("index");
    let mut rows: Vec<_> = (0..2).map(row).collect();
    for row in &mut rows {
        row.text = "battery electrode".into();
    }
    let mut builder =
        Builder::create_positional(&path, &[9; 32], binding(), Limits::default()).unwrap();
    for row in &rows {
        builder.push(row.clone()).unwrap();
    }
    let index = builder.finish().unwrap();
    let mut legacy = tree::Builder::new(index.context(tree::Purpose::ProjectionPostings)).unwrap();
    // Frozen codec-1 grammar: one group, two row bits, one absolute u32 offset.
    for (hash, positions) in literal::analyze("battery electrode") {
        let mut key = hash.to_vec();
        key.extend(0u32.to_be_bytes());
        key.extend(positions[0].to_be_bytes());
        let mut value = vec![1, 1, 0];
        for word in [3u64, 0, 0, 0] {
            value.extend(word.to_le_bytes());
        }
        value.extend(1u16.to_le_bytes());
        value.extend(positions[0].to_le_bytes());
        legacy.push(tree::Entry::new(key, value).unwrap()).unwrap();
    }
    let mut manifest = index.manifest.clone();
    manifest.positions = Some(legacy.finish().unwrap());
    manifest.position_codec = None;
    publish(&path, &index.cipher, &index.authority, &mut manifest).unwrap();
    drop(index);
    let legacy = Index::open(&path, &[9; 32], &binding()).unwrap();
    let mut next = binding();
    next.view[0] ^= 1;
    let mut append =
        Appender::extend_validated_prefix(&legacy, next.clone(), Limits::default()).unwrap();
    let mut addition = row(2);
    addition.text = "extra battery electrode".into();
    append.push(addition.clone()).unwrap();
    rows.push(addition);
    let updated = append.finish().unwrap();
    assert_eq!(updated.manifest.position_codec, Some(2));
    drop(updated);
    drop(legacy);
    let updated = Index::open(&path, &[9; 32], &next).unwrap();
    let reference = super::super::Index::build(next.clone(), rows.clone()).unwrap();
    let originals: BTreeMap<_, _> = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
    let q = query(
        "\"battery electrode\"",
        Filter::default(),
        i64::MIN,
        i64::MAX,
    );
    assert_eq!(
        fingerprint(
            &updated
                .search(&next, &q, |id| Ok(originals[id].clone()))
                .unwrap()
        ),
        fingerprint(&reference.search(&next, &q).unwrap())
    );
}
