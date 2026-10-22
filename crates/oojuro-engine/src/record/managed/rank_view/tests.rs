use super::*;
use crate::record::managed::tests::{Fixture, identity, input};
fn scope() -> Scope<'static> {
    Scope {
        writer: "synthetic-managed-owner",
        owner: 7,
    }
}
#[test]
fn genuine_managed_projection_selects_key_winner_before_eligibility_or_filter() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    let mut older = input("synthetic-old-agent", "old winning content");
    older.key = Some("state".into());
    older.valid_at = 10;
    older.tool = Some("calendar".into());
    let mut newer = input("synthetic-new-agent", "stored-only shadow");
    newer.key = Some("state".into());
    newer.valid_at = 20;
    newer.tool = Some("other".into());
    newer.indexed = false;
    let mut plain = input("synthetic-plain-agent", "unkeyed content");
    plain.valid_at = 0;
    let ack = log.append("versions", &[older, newer, plain]).unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        assert_eq!(
            view.records().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![11]
        );
        assert_eq!(
            view.lexical_records()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert!(!view.lexical_records()[0].indexed);
        assert_eq!(view.records()[0].recorded_at, ack.commit.recorded_at[2]);
        assert_eq!(view.records()[0].adapter, identity().adapter);
        assert!(!view.directory().exists());
        Ok(())
    })
    .unwrap();
    let at_first_ingest = AsOf {
        known_at: ack.commit.recorded_at[0],
        ..AsOf::default()
    };
    log.with_rank_view(scope(), at_first_ingest, Filter::default(), |view| {
        assert_eq!(
            view.records().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![9]
        );
        assert_eq!(view.records()[0].key.as_deref(), Some("state"));
        Ok(())
    })
    .unwrap();
    let calendar = Filter {
        tool: Some("calendar".into()),
        ..Filter::default()
    };
    log.with_rank_view(scope(), AsOf::default(), calendar, |view| {
        assert!(view.records().is_empty()); // no fallback to the old matching version
        Ok(())
    })
    .unwrap();
}
#[test]
fn derived_sealing_is_current_generation_scope_time_filter_and_purpose_bound() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    log.append(
        "one",
        &[input("synthetic-source", "real authorized managed row")],
    )
    .unwrap();
    let (sealed, before, old_stamp) = log
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            assert!(!view.directory().exists());
            let sealed = view.seal_derived("rank-table-v1", b"synthetic learned table")?;
            assert_eq!(
                view.open_derived("rank-table-v1", &sealed)?,
                b"synthetic learned table"
            );
            assert!(view.open_derived("different-purpose", &sealed).is_err());
            assert!(view.seal_derived("invalid purpose", b"table").is_err());
            assert!(!view.directory().exists()); // query and crypto do not create any cache directory
            Ok((sealed, view.binding().clone(), view.stamp()))
        })
        .unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        assert_eq!(view.stamp(), old_stamp);
        assert_eq!(
            view.open_derived("rank-table-v1", &sealed)?,
            b"synthetic learned table"
        );
        Ok(())
    })
    .unwrap();
    let cutoff = AsOf {
        known_at: 0,
        ..AsOf::default()
    };
    log.with_rank_view(scope(), cutoff, Filter::default(), |view| {
        assert!(view.open_derived("rank-table-v1", &sealed).is_err());
        Ok(())
    })
    .unwrap();
    let filtered = Filter {
        agent_id: Some("unmatched-private-agent".into()),
        ..Filter::default()
    };
    log.with_rank_view(scope(), AsOf::default(), filtered, |view| {
        assert!(view.open_derived("rank-table-v1", &sealed).is_err());
        assert_ne!(view.stamp(), old_stamp);
        Ok(())
    })
    .unwrap();
    log.prune("remove", &Selection::Ids(vec![9])).unwrap();
    log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        assert_ne!(view.binding(), &before);
        assert!(view.records().is_empty());
        assert_eq!(view.scope().writer, scope().writer);
        assert_eq!(view.scope().owner, scope().owner);
        assert!(view.open_derived("rank-table-v1", &sealed).is_err());
        assert!(!view.directory().exists());
        Ok(())
    })
    .unwrap();
}
#[test]
fn explicit_builder_directory_is_retired_and_denial_prevents_new_callbacks() {
    use std::cell::Cell;
    let f = Fixture::new();
    let control = f.control();
    let mut log = f.log(control.clone());
    log.append(
        "one",
        &[input("synthetic-cache-source", "authorized cache source")],
    )
    .unwrap();
    let directory = log
        .with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
            let directory = view.create_derived_directory()?.to_path_buf();
            let bytes = view.seal_derived("rank-tables-v1", b"synthetic table")?;
            write_new(&directory.join("RANK_TABLE"), &bytes)?;
            Ok(directory)
        })
        .unwrap();
    assert!(directory.join("RANK_TABLE").exists());
    log.prune("remove", &Selection::Ids(vec![9])).unwrap();
    assert!(!directory.exists());
    let called = Cell::new(false);
    let wrong = Scope {
        owner: 8,
        ..scope()
    };
    assert!(
        log.with_rank_view(wrong, AsOf::default(), Filter::default(), |_| {
            called.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(!called.get());
    control.deny().unwrap();
    assert!(
        log.with_rank_view(scope(), AsOf::default(), Filter::default(), |_| {
            called.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(!called.get());
}

#[test]
fn final_retained_witness_check_refuses_callback_result_after_external_change() {
    let f = Fixture::new();
    let mut log = f.log(f.control());
    log.append(
        "one",
        &[input("synthetic-current-source", "authorized row")],
    )
    .unwrap();
    let result = log.with_rank_view(scope(), AsOf::default(), Filter::default(), |view| {
        assert_eq!(view.records().len(), 1);
        let sealed = view.seal_derived("rank-table-v1", b"synthetic prior table")?;
        fs::write(f.root.join("witness/CURRENT"), b"invalid retained witness").map_err(io)?;
        // The ranking owner calls this before callback2: callback1's change fences it.
        assert!(view.validate_current().is_err());
        assert!(view.open_derived("rank-table-v1", &sealed).is_err());
        assert!(view.seal_derived("rank-table-v1", b"new table").is_err());
        assert!(view.create_derived_directory().is_err());
        assert!(!view.directory().exists());
        Ok("callback result must be refused")
    });
    assert!(result.is_err());
}
