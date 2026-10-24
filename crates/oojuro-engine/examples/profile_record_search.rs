//! Phase/work probe for the fixed synthetic profile_disk fixture only.
//! Source and cache verification on open remain outside each warm query timing.
use oojuro_engine::{
    capture_log::Identity,
    record::{
        self, AsOf, Filter, Query, Scope,
        paged::{DiskLimits, Index, Reader},
    },
};
use serde_json::json;
use std::path::Path;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn checked<T>(value: record::Result<T>) -> Result<T> {
    value.map_err(|e| format!("record {e:?}").into())
}
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("profile_record_search EXISTING_PROFILE_DIRECTORY BASE_RECORDS SAMPLES".into());
    }
    let root = Path::new(&args[1]);
    let count: usize = args[2].parse()?;
    let samples: usize = args[3].parse()?;
    if !(1000..=1_000_000).contains(&count) || !(1..=20).contains(&samples) {
        return Err("1k..1M base records and 1..20 samples".into());
    }
    let reader = checked(Reader::open_disk_with_limits(
        &root.join("source"),
        &[42; 32],
        Identity {
            writer: "disk-scale-profile-v1".into(),
            agent: 1,
            first_id: 1,
            adapter: "fixed-synthetic-v1".into(),
        },
        &root.join("directory"),
        None,
        DiskLimits::new(2_000_000, 4usize << 30, 4usize << 30, 10000)
            .map_err(|e| format!("{e:?}"))?,
    ))?;
    let index = checked(Index::open_disk(&reader, &root.join("lexical")))?;
    let cases = [
        ("unique", "needle000000123", i64::MAX, None),
        ("common_all", "synthetic", i64::MAX, None),
        ("compound_common", "database row", i64::MAX, None),
        ("missing", "absenttermzzzz", i64::MAX, None),
        ("time_half", "synthetic", count as i64 / 2, None),
        ("agent_filter", "synthetic", i64::MAX, Some("source-7")),
        ("literal_rare", "\"needle000000123\"", i64::MAX, None),
    ];
    let mut runs = Vec::new();
    for sample in 0..samples {
        for (kind, text, valid, agent) in cases {
            let query = Query {
                scope: Scope {
                    writer: "disk-scale-profile-v1",
                    owner: 1,
                },
                as_of: AsOf {
                    valid_at: valid,
                    ..AsOf::default()
                },
                text,
                k: 20,
                filter: Filter {
                    agent_id: agent.map(str::to_owned),
                    ..Filter::default()
                },
                max_bytes: record::MAX_OUTPUT_BYTES,
            };
            let (found, profile) = checked(index.search_profiled(&reader, &query))?;
            let originals=found.hits.iter().map(|h|json!({"id":h.record.id,"fixture_number":h.record.attrs["number"],"bits":h.bits.to_bits()})).collect::<Vec<_>>();
            runs.push(json!({"sample":sample,"kind":kind,"hits":originals,"profile":profile}));
        }
    }
    println!(
        "{}",
        json!({"scope":"fixed synthetic warm query phase/work probe, one client; not an SLA or independent scale acceptance", "records":checked(reader.watermark())?.records,"segments":index.segment_count(),"runs":runs})
    );
    Ok(())
}
