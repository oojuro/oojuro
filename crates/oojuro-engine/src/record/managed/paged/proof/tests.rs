use super::*;
use crate::record::{Input, Kind};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
fn row(agent: &str, text: &str, id: u64) -> Assigned {
    Assigned {
        id,
        recorded_at: 31,
        input: Input {
            agent_id: agent.into(),
            parent_id: None,
            valid_at: 17,
            kind: Kind::Explicit,
            tool: None,
            status: None,
            exit: None,
            key: None,
            text: text.into(),
            attrs: BTreeMap::new(),
            indexed: true,
        },
    }
}
struct F {
    root: PathBuf,
}
impl F {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "oojuro-managed-proof-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self { root }
    }
}
impl Drop for F {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn scan(
    selected: &Assigned,
    metadata: &Value,
    rows: &[Assigned],
    paths: &[PathBuf],
    work: &mut Work,
) -> Result<Counts> {
    scan_one(
        selected,
        metadata,
        &mut |visit| {
            for r in rows {
                visit(r)?;
            }
            Ok(())
        },
        &mut |visit| {
            for p in paths {
                visit(p)?;
            }
            Ok(())
        },
        work,
    )
}
#[test]
fn typed_scalar_container_string_and_attribute_key_residue_is_never_reported_absent() {
    for payload in [
        json!(123456789),
        json!([false, [true, null, 987654321]]),
        json!(null),
        json!([]),
        json!({}),
        json!([""]),
        json!({"":""}),
        json!({"nested-key":[{"inner":false}]}),
    ] {
        let mut selected = row("removed-only-agent", "", 1);
        selected
            .input
            .attrs
            .insert("removed-only-field".into(), payload.clone());
        let mut survivor = row("survivor-agent", "safe", 2);
        survivor
            .input
            .attrs
            .insert("kept-only-field".into(), payload);
        let c = scan(
            &selected,
            &json!({"writer":"independent"}),
            &[survivor],
            &[],
            &mut Work::new(4 << 20, 4).unwrap(),
        )
        .unwrap();
        assert!(c.decoded > 0, "typed residue was skipped");
        assert_eq!(c.encoded, 0);
    }
    let mut selected = row("removed-agent", "removed-text", 1);
    selected
        .input
        .attrs
        .insert("shared-attr-key".into(), json!("removed-value"));
    let mut survivor = row("kept-agent", "kept-text", 2);
    survivor
        .input
        .attrs
        .insert("shared-attr-key".into(), json!("different-value"));
    assert!(
        scan(
            &selected,
            &json!({}),
            &[survivor],
            &[],
            &mut Work::new(1 << 20, 3).unwrap()
        )
        .unwrap()
        .decoded
            > 0
    );
}
#[test]
fn retained_identity_and_layout_metadata_are_scanned_even_when_no_live_rows() {
    let selected = row("identity-only-marker", "removed-content-marker", 1);
    let metadata =
        json!({"identity":{"writer":"identity-only-marker","adapter":"native"},"layout":["DATA"]});
    let c = scan(
        &selected,
        &metadata,
        &[],
        &[],
        &mut Work::new(1 << 20, 1).unwrap(),
    )
    .unwrap();
    assert!(c.decoded > 0);
    assert_eq!(c.encoded, 0);
    let selected = row("gone-agent-marker", "gone-text-marker", 1);
    let c = scan(
        &selected,
        &json!({"writer":"other","adapter":"different"}),
        &[],
        &[],
        &mut Work::new(1 << 20, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(
        c,
        Counts {
            decoded: 0,
            encoded: 0
        }
    );
}
#[test]
fn file_stream_matches_across_fixed_buffer_boundaries_and_counts_overlaps_exactly() {
    let f = F::new();
    let p = f.root.join("owned.bin");
    let mut bytes = vec![b'x'; CHUNK - 3];
    bytes.extend(b"needle");
    bytes.extend(vec![b'x'; CHUNK]);
    bytes.extend(b"needleneedle");
    std::fs::write(&p, &bytes).unwrap();
    let selected = row("unique-agent-not-in-file", "needle", 1);
    let c = scan(
        &selected,
        &json!({}),
        &[],
        &[p.clone()],
        &mut Work::new(2 << 20, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(c.encoded, 3);
    std::fs::write(&p, b"aaaaa").unwrap();
    let selected = row("unique-agent-not-in-file", "aaa", 1);
    let c = scan(
        &selected,
        &json!({}),
        &[],
        &[p],
        &mut Work::new(1 << 20, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(c.encoded, 3);
}
#[test]
fn long_pattern_overlap_and_large_escaped_canonical_nodes_are_streamed() {
    let f = F::new();
    let p = f.root.join("large.bin");
    let text = format!("{}END", "q".repeat(CHUNK + 31));
    std::fs::write(&p, format!("prefix{text}suffix")).unwrap();
    let selected = row("long-pattern-agent", &text, 1);
    let c = scan(
        &selected,
        &json!({}),
        &[],
        &[p],
        &mut Work::new(8 << 20, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(c.encoded, 1);
    let selected = row("gone-only-agent", "not-present-marker", 1);
    let huge = row("kept-only-agent", &"\0".repeat(CHUNK * 2), 2);
    let mut w = Work::new(16 << 20, 3).unwrap();
    let c = scan(&selected, &json!({}), &[huge], &[], &mut w).unwrap();
    assert_eq!(c.decoded, 0);
    assert!(
        w.charged_bytes() > CHUNK as u64 * 12,
        "full escaped canonical work was not charged"
    );
    assert_eq!(w.charged_rows(), 2);
}
#[test]
fn work_limits_callback_io_and_invalid_metadata_poison_without_partial_completion() {
    let selected = row("unique-removed-agent", "removed-payload", 1);
    let live = row("kept-agent", "retained", 2);
    let mut w = Work::new(1, 4).unwrap();
    assert!(scan(&selected, &json!({}), &[], &[], &mut w).is_err());
    assert!(w.charge_bytes(0).is_err());
    let mut w = Work::new(1 << 20, 1).unwrap();
    assert!(scan(&selected, &json!({}), &[live], &[], &mut w).is_err());
    assert!(w.charge_rows(0).is_err());
    let f = F::new();
    let p = f.root.join("many.bin");
    std::fs::write(&p, vec![b'x'; CHUNK * 3]).unwrap();
    let mut w = Work::new(4 * CHUNK as u64, 4).unwrap();
    assert!(scan(&selected, &json!({}), &[], &[p], &mut w).is_err());
    let mut w = Work::new(1 << 20, 4).unwrap();
    assert!(
        scan_one(
            &selected,
            &json!({}),
            &mut |_| Err(Error::Unavailable),
            &mut |_| Ok(()),
            &mut w
        )
        .is_err()
    );
    assert!(w.charge_bytes(0).is_err());
    let mut w = Work::new(1 << 20, 4).unwrap();
    assert!(
        scan(
            &selected,
            &json!({}),
            &[],
            &[f.root.join("missing")],
            &mut w
        )
        .is_err()
    );
    assert!(w.charge_rows(0).is_err());
    let mut metadata = json!(null);
    for _ in 0..42 {
        metadata = json!([metadata]);
    }
    assert!(
        scan(
            &selected,
            &metadata,
            &[],
            &[],
            &mut Work::new(1 << 20, 4).unwrap()
        )
        .is_err()
    );
}
#[test]
fn empty_leaf_has_no_absence_needle_and_files_must_be_regular_not_symlink() {
    let f = F::new();
    let p = f.root.join("file");
    std::fs::write(&p, b"unrelated exact bytes").unwrap();
    let selected = row("gone-only-agent", "", 1);
    assert_eq!(
        scan(
            &selected,
            &json!({"empty":""}),
            &[],
            &[p.clone()],
            &mut Work::new(1 << 20, 1).unwrap()
        )
        .unwrap(),
        Counts {
            decoded: 0,
            encoded: 0
        }
    );
    let link = f.root.join("link");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert!(
        scan(
            &selected,
            &json!({}),
            &[],
            &[link],
            &mut Work::new(1 << 20, 1).unwrap()
        )
        .is_err()
    );
    assert!(
        scan(
            &selected,
            &json!({}),
            &[],
            &[f.root.clone()],
            &mut Work::new(1 << 20, 1).unwrap()
        )
        .is_err()
    );
}
#[test]
fn driver_external_hash_and_retirement_charges_share_the_same_checked_budget() {
    let mut w = Work::new(10, 3).unwrap();
    w.charge_bytes(4).unwrap();
    w.charge_rows(2).unwrap();
    assert_eq!((w.charged_bytes(), w.charged_rows()), (4, 2));
    assert!(w.charge_bytes(7).is_err());
    assert!(w.charge_rows(0).is_err());
    let mut w = Work::new(10, 3).unwrap();
    assert!(w.charge_rows(4).is_err());
    assert!(Work::new(0, 1).is_err());
}
